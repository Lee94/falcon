//! AI 助手的 REST 部分（docs/design/ai-assistant.md 第 8 节）。

use std::future::Future;

use falcon_proto::assistant::{AssistantSettings, AssistantSettingsPatch, CreateThreadRequest, ThreadPatch, ThreadSummary};
use falcon_proto::{CreateSessionRequest, OkResponse, Session};
use reqwest::Method;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

impl FalconClient {
    /// `GET /api/assistant/threads`：全部对话（跨机器），新的在前
    pub fn assistant_threads(&self) -> impl Future<Output = ApiResult<Vec<ThreadSummary>>> + MaybeSend + 'static {
        self.get("/api/assistant/threads".to_owned())
    }

    /// `POST /api/assistant/threads/:id/shell`：对话的终端，活着就给、丢了另开一个。请求体只用得上
    /// 配色的深浅（另开时写 COLORFGBG 等）。建出来就是普通会话，连接走 [`crate::SessionSocket`]
    pub fn assistant_thread_shell(
        &self,
        id: &str,
        request: &CreateSessionRequest,
    ) -> impl Future<Output = ApiResult<Session>> + MaybeSend + 'static {
        self.json(Method::POST, format!("/api/assistant/threads/{}/shell", seg(id)), json_body(request))
    }

    /// `PATCH /api/assistant/threads/:id`：换 agent
    pub fn update_assistant_thread(
        &self,
        id: &str,
        patch: &ThreadPatch,
    ) -> impl Future<Output = ApiResult<ThreadSummary>> + MaybeSend + 'static {
        self.json(Method::PATCH, format!("/api/assistant/threads/{}", seg(id)), json_body(patch))
    }

    /// `POST /api/assistant/machines/:machine/threads`：新对话 = 在这台机器上开一个终端 + 一段新的
    /// AI 上下文（缺省内置 agent）
    pub fn create_assistant_thread(
        &self,
        machine: &str,
        request: &CreateThreadRequest,
    ) -> impl Future<Output = ApiResult<ThreadSummary>> + MaybeSend + 'static {
        self.json(Method::POST, format!("/api/assistant/machines/{}/threads", seg(machine)), json_body(request))
    }

    /// `DELETE /api/assistant/threads/:id`：在跑的先停、它的终端终止
    pub fn delete_assistant_thread(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        self.bare(Method::DELETE, format!("/api/assistant/threads/{}", seg(id)))
    }

    /// `GET /api/assistant/settings`：API key 只给有没有与末四位
    pub fn assistant_settings(&self) -> impl Future<Output = ApiResult<AssistantSettings>> + MaybeSend + 'static {
        self.get("/api/assistant/settings".to_owned())
    }

    /// `PUT /api/assistant/settings`：只改带了的字段，回改完的全貌
    pub fn update_assistant_settings(
        &self,
        patch: &AssistantSettingsPatch,
    ) -> impl Future<Output = ApiResult<AssistantSettings>> + MaybeSend + 'static {
        self.json(Method::PUT, "/api/assistant/settings".to_owned(), json_body(patch))
    }
}
