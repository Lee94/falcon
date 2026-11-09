//! 右侧「修改」面板、工作区差异与侧栏的改动计数。
//!
//! 这一组端点的共同规矩：**环境事实与 git 命令失败都不是错误**，写在 `available` /
//! `reason` / `detail`（写操作是 `ok` / `detail`）里——没装 git、不是仓库、凭据不对、
//! 有冲突都是仓库的正常状态。只有项目不存在（404）、参数造错了（400）才是 `ApiError`。
//!
//! 多仓库项目的端点统一带 `?repo=<成员 dir>`（取自 `project.multi.repos`，原样带回）：
//! 读端点缺省回退第一个成员，写端点（commit / op）缺省是 400——写操作服务端不猜。

use std::collections::HashMap;
use std::future::Future;

use falcon_proto::{
    GitChangeCounts, GitCommitInput, GitFileDiff, GitOpInput, GitSyncResult, GitWorkingChanges,
};
use reqwest::Method;
use serde::Serialize;

use super::{Query, seg};
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

/// 要看 diff 的那个文件：从工作区改动里原样带回的仓库根相对路径。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitFileRef {
    pub path: String,
    /// 重命名 / 复制前的路径
    pub orig_path: Option<String>,
    /// 未跟踪文件：服务端走 `--no-index` 与空文件比
    pub untracked: bool,
}

impl GitFileRef {
    pub fn new(path: impl Into<String>) -> Self {
        GitFileRef { path: path.into(), ..Default::default() }
    }
}

fn repo_query(repo: Option<&str>) -> String {
    Query::new().nonempty("repo", repo).finish()
}

impl FalconClient {
    /// `GET /api/projects/:id/git/changes`：侧栏最后一层的 +N −M。读不到时
    /// `available: false`。
    pub fn git_changes(&self, project_id: &str) -> impl Future<Output = ApiResult<GitChangeCounts>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/git/changes", seg(project_id)))
    }

    /// `POST /api/git/changes`：侧栏轮询的批量版，服务端按宿主机分组、同主机一次 exec
    /// 拿全。已被删掉的项目 id 在结果里直接缺席。最多 500 个。
    pub fn git_changes_batch(
        &self,
        project_ids: &[String],
    ) -> impl Future<Output = ApiResult<HashMap<String, GitChangeCounts>>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            ids: &'a [String],
        }
        self.json(Method::POST, "/api/git/changes".to_owned(), json_body(&Body { ids: project_ids }))
    }

    /// `GET /api/projects/:id/git/diff`：工作区里单个文件相对 HEAD 的 diff。
    pub fn git_file_diff(
        &self,
        project_id: &str,
        file: &GitFileRef,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitFileDiff>> + MaybeSend + 'static {
        let q = Query::new()
            .push("path", &file.path)
            .nonempty("origPath", file.orig_path.as_deref())
            .flag("untracked", file.untracked)
            .nonempty("repo", repo)
            .finish();
        self.get(format!("/api/projects/{}/git/diff{q}", seg(project_id)))
    }

    /// `GET /api/projects/:id/git/working`：「修改」面板，全部未提交改动带 +N −M。
    /// 比 `git_changes` 重（多一条 numstat），只在面板打开时问。
    pub fn git_working(
        &self,
        project_id: &str,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitWorkingChanges>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/git/working{}", seg(project_id), repo_query(repo)))
    }

    /// `POST /api/projects/:id/git/commit`：提交工作区改动。没配 user.name、钩子拒绝、
    /// 没有可提交的改动都写在 `ok` / `detail` 里；缺提交信息（且不是 amend）是 400。
    pub fn git_commit_changes(
        &self,
        project_id: &str,
        input: &GitCommitInput,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitSyncResult>> + MaybeSend + 'static {
        self.json(
            Method::POST,
            format!("/api/projects/{}/git/commit{}", seg(project_id), repo_query(repo)),
            json_body(input),
        )
    }

    /// `POST /api/projects/:id/git/op`：「修改」面板的丢弃 / 继续 / 中止 / 取边。
    pub fn git_op(
        &self,
        project_id: &str,
        input: &GitOpInput,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitSyncResult>> + MaybeSend + 'static {
        self.json(
            Method::POST,
            format!("/api/projects/{}/git/op{}", seg(project_id), repo_query(repo)),
            json_body(input),
        )
    }
}
