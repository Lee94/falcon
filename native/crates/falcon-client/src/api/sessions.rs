//! 终端会话的 REST 部分（连接走 [`crate::SessionSocket`]）。
//!
//! Detach / Terminate 的区分照 CONTEXT.md：关掉会话 socket 只是 Detach，会话照跑；
//! [`FalconClient::terminate_session`] 才是 Terminate。

use std::future::Future;

use bytes::Bytes;
use falcon_proto::{
    CreateSessionRequest, OkResponse, PasteImageResult, Session, SessionForeground,
    SessionWithProject,
};
use reqwest::Method;
use serde::Serialize;

use super::seg;
use crate::client::{FalconClient, Req, ReqBody, json_body};
use crate::error::ApiResult;

impl FalconClient {
    /// `GET /api/sessions`：全部会话（含已丢失的），带所属项目名。web 每 5s 轮询一次。
    pub fn list_sessions(&self) -> impl Future<Output = ApiResult<Vec<SessionWithProject>>> + Send + 'static {
        self.get("/api/sessions".to_owned())
    }

    /// `POST /api/projects/:id/sessions`。项目已存档是 409，建不起来是 502。
    /// 请求体缺省就是 `CreateSessionRequest::default()`（web 发 `{}`）。
    pub fn create_session(
        &self,
        project_id: &str,
        request: &CreateSessionRequest,
    ) -> impl Future<Output = ApiResult<Session>> + Send + 'static {
        self.json(
            Method::POST,
            format!("/api/projects/{}/sessions", seg(project_id)),
            json_body(request),
        )
    }

    /// `POST /api/sessions/:id/reattach`：手动接回。确认已死时照样 200，返回 dead 的行。
    pub fn reattach_session(&self, id: &str) -> impl Future<Output = ApiResult<Session>> + Send + 'static {
        self.bare(Method::POST, format!("/api/sessions/{}/reattach", seg(id)))
    }

    /// `GET /api/sessions/:id/foreground`：关窗口（Terminate）前问一嘴前台有没有程序在跑。
    /// 侦测不到的场景 busy 恒为 false；会话不存在也答空闲。设计文档 §4.3 要求 app
    /// 给它套 2s 超时——它是道保险，自己卡住不能把关窗口拦下来。
    pub fn session_foreground(&self, id: &str) -> impl Future<Output = ApiResult<SessionForeground>> + Send + 'static {
        self.get(format!("/api/sessions/{}/foreground", seg(id)))
    }

    /// `POST /api/sessions/:id/terminate`：Terminate，销毁会话及其 Zellij 会话。
    pub fn terminate_session(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::POST, format!("/api/sessions/{}/terminate", seg(id)))
    }

    /// `DELETE /api/sessions/:id`：清除一条已丢失（dead）的会话记录；不是 dead 是 409。
    pub fn clear_session(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::DELETE, format!("/api/sessions/{}", seg(id)))
    }

    /// `PATCH /api/sessions/:id`：改名。空串是合法的——清掉名字就回到自动标题。
    pub fn rename_session(&self, id: &str, name: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
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
    /// 大小上限是 [`falcon_proto::PASTE_IMAGE_MAX_BYTES`]：app 应当像 web 一样先检查、
    /// 超限直接提示，不发请求；服务端的 bodyLimit 只是兜底。不支持的类型是 415。
    pub fn paste_image(
        &self,
        session_id: &str,
        bytes: impl Into<Bytes>,
        content_type: &str,
    ) -> impl Future<Output = ApiResult<String>> + Send + 'static {
        let path = format!("/api/sessions/{}/paste-image", seg(session_id));
        let body = ReqBody::Raw { data: bytes.into(), content_type: content_type.to_owned() };
        self.call(move |inner| async move {
            let res: PasteImageResult = inner.fetch_json(&Req::new(Method::POST, path, body)).await?;
            Ok(res.path)
        })
    }
}
