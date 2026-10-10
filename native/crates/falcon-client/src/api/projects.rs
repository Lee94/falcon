//! 项目、附属项目（派生 / 存档 / 删除前预检）与宿主机 Zellij 授权。

use std::future::Future;

use falcon_proto::{
    DeleteProjectResult, HostZellijStatus, MultiRepoProbe, MultiWorktreeInput, OkResponse, Project,
    ProjectInput, RepoInfo, WorktreeInput, WorktreeStatus,
};
use reqwest::Method;
use serde::Serialize;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

/// 派生请求体：单仓库项目吃 [`WorktreeInput`]，多仓库容器吃 [`MultiWorktreeInput`]
/// （服务端按项目分流，同一个端点）。TS 里是 `WorktreeInput | MultiWorktreeInput`。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum DeriveInput {
    Single(WorktreeInput),
    Multi(MultiWorktreeInput),
}

impl From<WorktreeInput> for DeriveInput {
    fn from(v: WorktreeInput) -> Self {
        DeriveInput::Single(v)
    }
}

impl From<MultiWorktreeInput> for DeriveInput {
    fn from(v: MultiWorktreeInput) -> Self {
        DeriveInput::Multi(v)
    }
}

/// `POST /api/projects/:id/host` 的请求体（TS 里是内联的 `{ authorized?, baseUrl? }`）。
///
/// 只带要改的字段。`base_url` 给了（哪怕是空串 = 恢复官方源）就会作废该主机之前的
/// 安装记录，下次重新走一遍安装流程。
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HostAuthorizationPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl FalconClient {
    pub fn list_projects(&self) -> impl Future<Output = ApiResult<Vec<Project>>> + MaybeSend + 'static {
        self.get("/api/projects".to_owned())
    }

    /// 校验失败（路径不存在、SSH 字段不全）是 400，`message` 就是服务端的原话。
    pub fn create_project(&self, input: &ProjectInput) -> impl Future<Output = ApiResult<Project>> + MaybeSend + 'static {
        self.json(Method::POST, "/api/projects".to_owned(), json_body(input))
    }

    pub fn update_project(
        &self,
        id: &str,
        input: &ProjectInput,
    ) -> impl Future<Output = ApiResult<Project>> + MaybeSend + 'static {
        self.json(Method::PUT, format!("/api/projects/{}", seg(id)), json_body(input))
    }

    /// `DELETE /api/projects/:id?force=<bool>`，连坐它的附属项目。
    ///
    /// 还有未终止的会话且没带 `force` 时是 409（确认框问过再带 force 重发）。
    /// 文件系统清理 best-effort：删不掉的目录写在 `warnings` 里，仍是 200。
    pub fn delete_project(
        &self,
        id: &str,
        force: bool,
    ) -> impl Future<Output = ApiResult<DeleteProjectResult>> + MaybeSend + 'static {
        self.bare(Method::DELETE, format!("/api/projects/{}?force={force}", seg(id)))
    }

    /// `GET /api/projects/:id/repo`：源项目的仓库信息（派生用的分支清单）。
    /// 环境事实写在 `derivable` / `reason` 里，不会是错误。
    pub fn repo_info(&self, project_id: &str) -> impl Future<Output = ApiResult<RepoInfo>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/repo", seg(project_id)))
    }

    /// `GET /api/projects/:id/repos`：多仓库容器的派生前探测，逐成员 RepoInfo。
    pub fn repo_info_multi(&self, project_id: &str) -> impl Future<Output = ApiResult<MultiRepoProbe>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/repos", seg(project_id)))
    }

    /// `POST /api/projects/:id/worktrees`：派生附属项目。
    ///
    /// 与探测端点相反，环境事实在这里一律是状态冲突（409）。批量派生失败时错误体是
    /// [`falcon_proto::MultiDeriveError`]（`err.body_as()`），回滚有残留是 502。
    pub fn create_worktree(
        &self,
        project_id: &str,
        input: impl Into<DeriveInput>,
    ) -> impl Future<Output = ApiResult<Project>> + MaybeSend + 'static {
        let input = input.into();
        self.json(
            Method::POST,
            format!("/api/projects/{}/worktrees", seg(project_id)),
            json_body(&input),
        )
    }

    /// `GET /api/projects/:id/worktree`：附属项目删除前的预检（脏文件、ignored 文件数、
    /// 未推送提交）。读不到状态时写在 `error` 里，不是错误。
    pub fn worktree_status(&self, project_id: &str) -> impl Future<Output = ApiResult<WorktreeStatus>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/worktree", seg(project_id)))
    }

    /// 存档附属项目：隐藏并终止其会话，目录保留，到期由后端自动删除。幂等。
    pub fn archive_project(&self, id: &str) -> impl Future<Output = ApiResult<Project>> + MaybeSend + 'static {
        self.bare(Method::POST, format!("/api/projects/{}/archive", seg(id)))
    }

    /// 恢复已存档的附属项目。幂等。
    pub fn restore_project(&self, id: &str) -> impl Future<Output = ApiResult<Project>> + MaybeSend + 'static {
        self.bare(Method::POST, format!("/api/projects/{}/restore", seg(id)))
    }

    /// `GET /api/projects/:id/host`：SSH 项目宿主机上的 Zellij 状态（授权按主机记）。
    /// 本地项目是 400。
    pub fn host_status(&self, project_id: &str) -> impl Future<Output = ApiResult<HostZellijStatus>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/host", seg(project_id)))
    }

    /// `POST /api/projects/:id/host`：授权 / 拒绝在该主机安装 Zellij、改下载源。
    pub fn set_host_authorization(
        &self,
        project_id: &str,
        patch: &HostAuthorizationPatch,
    ) -> impl Future<Output = ApiResult<OkResponse>> + MaybeSend + 'static {
        self.json(Method::POST, format!("/api/projects/{}/host", seg(project_id)), json_body(patch))
    }
}
