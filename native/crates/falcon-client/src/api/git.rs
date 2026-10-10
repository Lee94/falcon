//! 右侧 Git 面板（修改 / 差异 / 历史）与侧栏的改动计数。
//!
//! 这一组端点的共同规矩：**环境事实与 git 命令失败都不是错误**，写在 `available` /
//! `reason` / `detail`（写操作是 `ok` / `detail`）里——没装 git、不是仓库、凭据不对、
//! 有冲突都是仓库的正常状态。只有项目不存在（404）、参数造错了（400）才是 `ApiError`。
//!
//! 多仓库项目的端点统一带 `?repo=<成员 dir>`（取自 `project.multi.repos`，原样带回）：
//! 读端点缺省回退第一个成员，写端点（commit / pull / push / op）缺省是 400——写操作
//! 服务端不猜。

use std::collections::HashMap;
use std::future::Future;

use falcon_proto::{
    GitChangeCounts, GitCommitDetail, GitCommitInput, GitFileDiff, GitLogPage, GitOpInput,
    GitRefsInfo, GitSnapshot, GitSyncResult, GitWorkingChanges,
};
use reqwest::Method;
use serde::Serialize;

use super::{Query, seg};
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

/// 要看 diff 的那个文件：从快照 / 提交详情里原样带回的仓库根相对路径。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitFileRef {
    pub path: String,
    /// 重命名 / 复制前的路径
    pub orig_path: Option<String>,
    /// 未跟踪文件：服务端走 `--no-index` 与空文件比。提交里的 diff 忽略这一项
    pub untracked: bool,
}

impl GitFileRef {
    pub fn new(path: impl Into<String>) -> Self {
        GitFileRef { path: path.into(), ..Default::default() }
    }
}

/// History 列表的筛选与分页（`GET …/git/log` 的 query）。空串与 0 等于不带。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitLogQuery {
    pub branch: Option<String>,
    pub author: Option<String>,
    /// 按提交信息搜（git log --grep）
    pub q: Option<String>,
    pub skip: u32,
    pub repo: Option<String>,
}

/// Pull / Push（`POST …/git/pull|push`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitSyncAction {
    Pull,
    Push,
}

impl GitSyncAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            GitSyncAction::Pull => "pull",
            GitSyncAction::Push => "push",
        }
    }
}

fn repo_query(repo: Option<&str>) -> String {
    Query::new().nonempty("repo", repo).finish()
}

impl FalconClient {
    /// `GET /api/projects/:id/git`：Git 面板的仓库快照。源项目和附属项目都能问。
    pub fn git_snapshot(
        &self,
        project_id: &str,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitSnapshot>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/git{}", seg(project_id), repo_query(repo)))
    }

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

    /// `GET /api/projects/:id/git/log`：History 列表的一页。
    pub fn git_log(
        &self,
        project_id: &str,
        query: &GitLogQuery,
    ) -> impl Future<Output = ApiResult<GitLogPage>> + MaybeSend + 'static {
        let skip = (query.skip > 0).then(|| query.skip.to_string());
        let q = Query::new()
            .nonempty("branch", query.branch.as_deref())
            .nonempty("author", query.author.as_deref())
            .nonempty("q", query.q.as_deref())
            .nonempty("skip", skip.as_deref())
            .nonempty("repo", query.repo.as_deref())
            .finish();
        self.get(format!("/api/projects/{}/git/log{q}", seg(project_id)))
    }

    /// `GET /api/projects/:id/git/refs`：Branch / User 两个筛选下拉的候选值。
    pub fn git_refs(
        &self,
        project_id: &str,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitRefsInfo>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/git/refs{}", seg(project_id), repo_query(repo)))
    }

    /// `GET /api/projects/:id/git/commit?sha=`：选中提交的详情。sha 只认 4–40 位十六进制，
    /// 否则 400。
    pub fn git_commit(
        &self,
        project_id: &str,
        sha: &str,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitCommitDetail>> + MaybeSend + 'static {
        let q = Query::new().push("sha", sha).nonempty("repo", repo).finish();
        self.get(format!("/api/projects/{}/git/commit{q}", seg(project_id)))
    }

    /// `GET /api/projects/:id/git/commit/diff`：某条提交里单个文件的 diff。
    pub fn git_commit_diff(
        &self,
        project_id: &str,
        sha: &str,
        file: &GitFileRef,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitFileDiff>> + MaybeSend + 'static {
        let q = Query::new()
            .push("sha", sha)
            .push("path", &file.path)
            .nonempty("origPath", file.orig_path.as_deref())
            .nonempty("repo", repo)
            .finish();
        self.get(format!("/api/projects/{}/git/commit/diff{q}", seg(project_id)))
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

    /// `POST /api/projects/:id/git/pull|push`。凭据不对、非快进、远端拒绝都写在
    /// `ok` / `detail` 里，面板要把 git 的原话给用户看。
    pub fn git_sync(
        &self,
        project_id: &str,
        action: GitSyncAction,
        repo: Option<&str>,
    ) -> impl Future<Output = ApiResult<GitSyncResult>> + MaybeSend + 'static {
        self.bare(
            Method::POST,
            format!("/api/projects/{}/git/{}{}", seg(project_id), action.as_str(), repo_query(repo)),
        )
    }

    /// `POST /api/projects/:id/git/op`：历史面板上的 17 种写操作。
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
