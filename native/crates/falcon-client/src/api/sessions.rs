//! 终端会话的 REST 部分（连接走 [`crate::SessionSocket`]）。
//!
//! Detach / Terminate 的区分照 CONTEXT.md：关掉会话 socket 只是 Detach，会话照跑；
//! [`FalconClient::terminate_session`] 才是 Terminate。

use std::future::Future;

use bytes::Bytes;
use falcon_proto::{
    CreateSessionRequest, OkResponse, PasteImageResult, Session, SessionForeground,
    SessionWithProject, SessionAgent,
};
use reqwest::Method;
use serde::Serialize;

use super::{Query, seg};
use super::system::ProbeTarget;
use crate::client::{FalconClient, Req, ReqBody, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

impl FalconClient {
    /// 后端本机实际可用的开场 CLI。
    pub fn local_agents(&self) -> impl Future<Output = ApiResult<Vec<SessionAgent>>> + MaybeSend + 'static {
        self.agents(&ProbeTarget::Backend)
    }

    /// 指定宿主机实际可用的 CLI；SSH 主机与项目沿用其它探测接口的目标参数。
    pub fn agents(&self, target: &ProbeTarget) -> impl Future<Output = ApiResult<Vec<SessionAgent>>> + MaybeSend + 'static {
        let query = target.query(Query::new()).finish();
        self.get(format!("/api/agents{query}"))
    }

    /// `GET /api/sessions`：全部会话（含已丢失的），带所属项目名。界面每 5s 轮询一次。
    pub fn list_sessions(&self) -> impl Future<Output = ApiResult<Vec<SessionWithProject>>> + MaybeSend + 'static {
        self.get("/api/sessions".to_owned())
    }

    /// `POST /api/projects/:id/sessions`。项目已存档是 409，建不起来是 502。
    /// 请求体缺省就是 `CreateSessionRequest::default()`（React 版当初发的就是 `{}`）。
    pub fn create_session(
        &self,
        project_id: &str,
        request: &CreateSessionRequest,
    ) -> impl Future<Output = ApiResult<Session>> + MaybeSend + 'static {
        self.json(
            Method::POST,
            format!("/api/projects/{}/sessions", seg(project_id)),
            json_body(request),
        )
    }

    /// `POST /api/sessions/:id/reattach`：手动接回。确认已死时照样 200，返回 dead 的行。
    pub fn reattach_session(&self, id: &str) -> impl Future<Output = ApiResult<Session>> + MaybeSend + 'static {
        self.bare(Method::POST, format!("/api/sessions/{}/reattach", seg(id)))
    }

    /// 探测当前会话的服务端 Agent 模式能力（404 / 405 视为旧服务端，退回全 false）。
    pub fn agent_mode_capabilities(&self, id: &str) -> impl Future<Output = ApiResult<falcon_proto::AgentModeCapabilities>> + MaybeSend + 'static {
        let client = self.clone();
        let path = format!("/api/sessions/{}/agent-mode", seg(id));
        async move {
            match client.get(path).await {
                Ok(cap) => Ok(cap),
                Err(error) if matches!(error.status, Some(404 | 405)) => {
                    Ok(falcon_proto::AgentModeCapabilities::default())
                }
                Err(error) => Err(error),
            }
        }
    }

    /// 开启/关闭真实终端的只读图形镜像，始终保留原进程。
    /// 先探测新能力，防止旧服务端把同一路径的 POST 解释成 terminate + resume。
    pub fn switch_agent_mode(&self, id: &str, graphical: bool) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body { mode: &'static str }
        let client = self.clone();
        let path = format!("/api/sessions/{}/agent-mode", seg(id));
        async move {
            let capability: serde_json::Value = match client.get(path.clone()).await {
                Ok(capability) => capability,
                Err(error) if matches!(error.status, Some(404 | 405)) => {
                    return Err(crate::error::ApiError::internal("agent_mirror_unsupported"));
                }
                Err(error) => return Err(error),
            };
            if capability["preservesProcess"].as_bool() != Some(true) {
                return Err(crate::error::ApiError::internal("agent_mirror_unsupported"));
            }
            client.json(Method::POST, path, json_body(&Body {
                mode: if graphical { "gui" } else { "terminal" },
            })).await
        }
    }

    /// 启用可交互 GUI。与旧只读镜像使用不同的模式值，旧服务端不能把它解释成镜像或重启。
    /// 首次转换会续接对话，调用方须先明确确认；后端负责检查当前轮次和进程状态。
    pub fn enable_agent_gui(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body { mode: &'static str }
        let client = self.clone();
        let path = format!("/api/sessions/{}/agent-mode", seg(id));
        async move {
            let capability: serde_json::Value = match client.get(path.clone()).await {
                Ok(capability) => capability,
                Err(error) if matches!(error.status, Some(404 | 405)) => {
                    return Err(crate::error::ApiError::internal("agent_gui_unsupported"));
                }
                Err(error) => return Err(error),
            };
            if capability["interactiveGui"].as_bool() != Some(true) {
                return Err(crate::error::ApiError::internal("agent_gui_unsupported"));
            }
            client.json(Method::POST, path, json_body(&Body { mode: "gui-control" })).await
        }
    }

    /// 从可交互 GUI 切回真实 PTY 终端（`"terminal-control"`），须先确认服务端声明 `interactiveTerminal`。
    pub fn enable_agent_terminal(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body { mode: &'static str }
        let client = self.clone();
        let path = format!("/api/sessions/{}/agent-mode", seg(id));
        async move {
            let capability: serde_json::Value = match client.get(path.clone()).await {
                Ok(capability) => capability,
                Err(error) if matches!(error.status, Some(404 | 405)) => {
                    return Err(crate::error::ApiError::internal("agent_terminal_unsupported"));
                }
                Err(error) => return Err(error),
            };
            if capability["interactiveTerminal"].as_bool() != Some(true) {
                return Err(crate::error::ApiError::internal("agent_terminal_unsupported"));
            }
            client.json(Method::POST, path, json_body(&Body { mode: "terminal-control" })).await
        }
    }

    /// `GET /api/sessions/:id/foreground`：关窗口（Terminate）前问一嘴前台有没有程序在跑。
    /// 侦测不到的场景 busy 恒为 false；会话不存在也答空闲。设计文档 §4.3 要求 app
    /// 给它套 2s 超时——它是道保险，自己卡住不能把关窗口拦下来。
    pub fn session_foreground(&self, id: &str) -> impl Future<Output = ApiResult<SessionForeground>> + MaybeSend + 'static {
        self.get(format!("/api/sessions/{}/foreground", seg(id)))
    }

    /// `POST /api/sessions/:id/terminate`：Terminate，销毁会话（持久会话连同宿主机上的守护进程）。
    pub fn terminate_session(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        self.bare(Method::POST, format!("/api/sessions/{}/terminate", seg(id)))
    }

    /// `DELETE /api/sessions/:id`：清除一条已丢失（dead）的会话记录；不是 dead 是 409。
    pub fn clear_session(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        self.bare(Method::DELETE, format!("/api/sessions/{}", seg(id)))
    }

    /// `PATCH /api/sessions/:id`：改名。空串是合法的——清掉名字就回到自动标题。
    pub fn rename_session(&self, id: &str, name: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            name: &'a str,
        }
        self.json(Method::PATCH, format!("/api/sessions/{}", seg(id)), json_body(&Body { name }))
    }

    /// `POST /api/sessions/:id/paste-image`：图片按原始字节直传（Content-Type 就是图片
    /// 类型，不走 JSON），写到会话宿主机的 `<falcon 根>/paste/`，返回落盘的绝对路径——
    /// app 把它粘进终端输入（CONTEXT.md「Image Paste」）。
    ///
    /// 大小上限是 [`falcon_proto::PASTE_IMAGE_MAX_BYTES`]：app 应当像当初 React 版那样先检查、
    /// 超限直接提示，不发请求；服务端的 bodyLimit 只是兜底。不支持的类型是 415。
    pub fn paste_image(
        &self,
        session_id: &str,
        bytes: impl Into<Bytes>,
        content_type: &str,
    ) -> impl Future<Output = ApiResult<String>> + MaybeSend + 'static {
        let path = format!("/api/sessions/{}/paste-image", seg(session_id));
        let body = ReqBody::Raw { data: bytes.into(), content_type: content_type.to_owned() };
        self.call(move |inner| async move {
            let res: PasteImageResult = inner.fetch_json(&Req::new(Method::POST, path, body)).await?;
            Ok(res.path)
        })
    }
}
