use std::{collections::HashSet, path::Path};

use axum::{Json, Router, extract::{Path as Param, State}, routing::post};
use falcon_proto::{Project, ThreadInput};

use super::{AppState, error::{ApiError, ApiResult}};
use crate::db::{Db, ProjectRow};
use crate::git::path::canon_key;
use crate::sessions::local::local_kind;
use crate::virtualdir::CLAUDE_MD_BODY;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/projects/{id}/threads", post(create))
}

/// 校验并规范化多文件夹环境的目录列表：允许普通目录与 Git 仓库混选，按宿主机路径口径去重。
pub(super) fn environment_directories(dirs: Vec<String>) -> ApiResult<Vec<String>> {
    let kind = local_kind();
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let canonical = super::projects::environment_directory(&dir)?;
        if !seen.insert(canon_key(kind, &canonical)) {
            return Err(ApiError::bad_request(format!("环境中重复选择了目录 {canonical}")));
        }
        resolved.push(canonical);
    }
    Ok(resolved)
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
    let has_dirs = env.working_dir.as_deref().is_some_and(|d| !d.is_empty())
        || Db::parse_multi_repos(env.multi_repos.as_deref()).is_some_and(|m| !m.is_empty());
    if !has_dirs {
        return Err(ApiError::conflict("环境缺少工作目录"));
    }
    let thread = ProjectRow {
        id: crate::askpass::hub::uuid_v4(),
        name: name.into(),
        project_type: "local".into(),
        working_dir: env.working_dir,
        multi_repos: env.multi_repos,
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

    #[tokio::test]
    async fn thread_multi_directory_environment_allows_plain_folders_and_inherits_members() {
        let app = TestApp::new();
        let dir_a = app.root.join("alpha");
        let dir_b = app.root.join("beta");
        let dir_c = app.root.join("gamma");
        for d in [&dir_a, &dir_b, &dir_c] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(dir_a.join("keep.txt"), "a").unwrap();

        // 重复目录与带基础分支的多文件夹环境都会被拒绝
        let (code, _) = json_of(app.post_json("/api/environments", json!({
            "type": "local", "name": "dup", "repos": [dir_a, dir_a],
        })).await).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let (code, _) = json_of(app.post_json("/api/environments", json!({
            "type": "local", "name": "branch", "repos": [dir_a, dir_b], "defaultWorktreeBranch": "main",
        })).await).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        // 允许多个普通（非 Git）文件夹创建环境
        let (code, env) = json_of(app.post_json("/api/environments", json!({
            "type": "local", "name": "multi-env", "environmentContext": "shared", "repos": [dir_a, dir_b],
        })).await).await;
        assert_eq!(code, StatusCode::OK, "{env}");
        let env_id = env["id"].as_str().unwrap();
        assert!(env.get("workingDir").is_none());
        let expected_a = dunce::canonicalize(&dir_a).unwrap().to_string_lossy().into_owned();
        let expected_b = dunce::canonicalize(&dir_b).unwrap().to_string_lossy().into_owned();
        let expected_c = dunce::canonicalize(&dir_c).unwrap().to_string_lossy().into_owned();
        assert_eq!(env["multi"]["repos"], json!([{"dir": expected_a}, {"dir": expected_b}]));

        // 还没有线程时可以增删环境的文件夹
        let (code, updated) = json_of(app.put_json(&format!("/api/projects/{env_id}"), json!({
            "type": "local", "name": "multi-env", "environmentContext": "shared", "repos": [dir_a, dir_b, dir_c],
        })).await).await;
        assert_eq!(code, StatusCode::OK, "{updated}");
        assert_eq!(updated["multi"]["repos"], json!([{"dir": expected_a}, {"dir": expected_b}, {"dir": expected_c}]));

        // 创建线程：继承环境的全部目录
        let (code, thread) = json_of(app.post_json(&format!("/api/projects/{env_id}/threads"), json!({
            "name": "task-1",
        })).await).await;
        assert_eq!(code, StatusCode::OK, "{thread}");
        let thread_id = thread["id"].as_str().unwrap();
        assert_eq!(thread["threadEnvironmentId"], env_id);
        assert!(thread.get("workingDir").is_none());
        assert_eq!(thread["multi"]["repos"], json!([{"dir": expected_a}, {"dir": expected_b}, {"dir": expected_c}]));

        // 文件接口首次打开会为多目录线程建出虚拟目录（含 AGENTS.md 与目录授权）
        assert_eq!(app.get(&format!("/api/projects/{thread_id}/files")).await.status(), StatusCode::OK);
        let vdir = app.data_dir.join("projects").join(thread_id);
        assert!(vdir.join("AGENTS.md").is_file());
        assert!(vdir.join(".claude/settings.json").is_file());

        // 已有线程后：可改名/改上下文，不可改环境的目录列表，也不可改线程的目录列表
        let (code, renamed) = json_of(app.put_json(&format!("/api/projects/{env_id}"), json!({
            "type": "local", "name": "renamed-env", "environmentContext": "updated",
        })).await).await;
        assert_eq!(code, StatusCode::OK, "{renamed}");
        assert_eq!(renamed["name"], "renamed-env");
        let (code, _) = json_of(app.put_json(&format!("/api/projects/{env_id}"), json!({
            "type": "local", "name": "renamed-env", "repos": [dir_a],
        })).await).await;
        assert_eq!(code, StatusCode::CONFLICT);
        let (code, _) = json_of(app.put_json(&format!("/api/projects/{thread_id}"), json!({
            "type": "local", "name": "task-1", "repos": [dir_a],
        })).await).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        // 删除线程时清掉线程自己的虚拟目录、保留源文件夹，之后可删除环境
        let del = |id: &str| Request::delete(format!("/api/projects/{id}")).body(Body::empty()).unwrap();
        assert_eq!(app.send(del(thread_id)).await.status(), StatusCode::OK);
        assert!(!vdir.exists());
        assert_eq!(std::fs::read_to_string(dir_a.join("keep.txt")).unwrap(), "a");
        assert_eq!(app.send(del(env_id)).await.status(), StatusCode::OK);
    }
}
