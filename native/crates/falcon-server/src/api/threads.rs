use std::{collections::HashSet, path::Path};

use axum::{Json, Router, extract::{Path as Param, State}, routing::post};
use falcon_proto::{Project, ThreadInput};

use super::{AppState, error::{ApiError, ApiResult}};
use crate::db::{Db, ProjectRow};
use crate::git::{path::canon_key, repo::{RunOpts, list_worktrees, main_worktree_of, repo_root}};
use crate::virtualdir::CLAUDE_MD_BODY;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/projects/{id}/threads", post(create))
}

pub(super) async fn environment_repos(state: &AppState, dirs: Vec<String>) -> ApiResult<Vec<String>> {
    state.engine.call(move |engine| async move {
        let local = ProjectRow { project_type: "local".into(), ..Default::default() };
        let host = engine.git_host(&local).await.map_err(ApiError::internal)?;
        let opts = RunOpts::default();
        let mut seen = HashSet::new();
        let mut roots = Vec::new();
        for dir in dirs {
            let root = repo_root(&host, &dir, &opts).await
                .map_err(|e| ApiError::bad_request(format!("{dir}：{e}")))?;
            let trees = list_worktrees(&host, &root, &opts).await.map_err(ApiError::internal)?;
            let main = main_worktree_of(&trees, &root);
            if !seen.insert(canon_key(host.kind, &main)) {
                return Err(ApiError::bad_request(format!("环境中重复引用了仓库 {main}")));
            }
            roots.push(root);
        }
        Ok(roots)
    }).await?
}

async fn create(
    State(state): State<AppState>,
    Param(id): Param<String>,
    Json(input): Json<ThreadInput>,
) -> ApiResult<Json<Project>> {
    let lock = state.nodes.lock(&format!("environment:{id}"));
    let _guard = lock.lock().await;
    let env = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("环境不存在"))?;
    if env.project_type != "local" || env.environment_context.is_none() || env.source_project_id.is_some() {
        return Err(ApiError::bad_request("创建线程需要选择环境"));
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("线程名称不能为空"));
    }
    let cwd = env.working_dir.or_else(|| {
        Db::parse_multi_repos(env.multi_repos.as_deref())?.into_iter().next().map(|repo| repo.dir)
    }).ok_or_else(|| ApiError::conflict("环境缺少工作目录"))?;
    let thread = ProjectRow {
        id: crate::askpass::hub::uuid_v4(),
        name: name.into(),
        project_type: "local".into(),
        working_dir: Some(cwd),
        shell: env.shell,
        created_at: crate::auth::now_ms(),
        thread_environment_id: Some(id),
        ..Default::default()
    };
    state.db.try_insert_project(&thread, &thread.id, thread.created_at).map_err(ApiError::internal)?;
    Ok(Json(Db::to_project(&thread)))
}

/// 用户编辑过的旧线程上下文必须保留。只清理全文仍与初始快照相同的文件。
pub(crate) fn matches_context(path: &Path, expected: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == expected.len() as u64)
        && std::fs::read(path).is_ok_and(|bytes| bytes == expected.as_bytes())
}

pub(crate) fn remove_context_files(root: &Path, initial: &str) {
    for (name, expected) in [("AGENTS.md", initial), ("CLAUDE.md", CLAUDE_MD_BODY)] {
        let path = root.join(name);
        if matches_context(&path, expected) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::api::test_support::{TestApp, json_of};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;

    #[tokio::test]
    async fn threads_share_environment_directory_and_have_distinct_identity() {
        let app = TestApp::new();
        let dir = app.root.join("plain");
        std::fs::create_dir(&dir).unwrap();
        let (code, env) = json_of(app.post_json("/api/environments", json!({
            "type":"local", "name":"E", "environmentContext":"notes", "workingDir":dir,
        })).await).await;
        assert_eq!(code, StatusCode::OK, "{env}");
        let env_id = env["id"].as_str().unwrap();
        let url = format!("/api/projects/{env_id}/threads");
        let req = || Request::post(&url).header("content-type", "application/json")
            .header("idempotency-key", "thread-op")
            .body(Body::from(r#"{"name":"same","task":"implement"}"#)).unwrap();
        let (code, first) = json_of(app.send(req()).await).await;
        assert_eq!(code, StatusCode::OK, "{first}");
        assert_eq!(json_of(app.send(req()).await).await.1, first);
        let (code, second) = json_of(app.post_json(&url, json!({"name":"same"})).await).await;
        assert_eq!(code, StatusCode::OK, "{second}");
        assert_ne!(first["id"], second["id"]);
        for thread in [&first, &second] {
            assert_eq!(thread["workingDir"], env["workingDir"]);
            assert_eq!(thread["threadEnvironmentId"], env_id);
            assert!(thread.get("worktree").is_none());
        }
        let del = |id: &str| Request::delete(format!("/api/projects/{id}")).body(Body::empty()).unwrap();
        assert_eq!(app.send(del(env_id)).await.status(), StatusCode::CONFLICT);
        for thread in [first, second] {
            assert_eq!(app.send(del(thread["id"].as_str().unwrap())).await.status(), StatusCode::OK);
        }
        assert_eq!(app.send(del(env_id)).await.status(), StatusCode::OK);
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }
}
