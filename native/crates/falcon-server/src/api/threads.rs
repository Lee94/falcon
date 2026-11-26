//! 环境和线程都是宿主机节点上的项目资源，因此文件、会话和 Git 面板沿用节点路由。
use std::collections::HashSet;
use std::path::Path;

use axum::{
    Json, Router,
    extract::{Path as Param, State},
    routing::post,
};
use falcon_proto::{MultiRepoMember, Project, ThreadInput, WorktreeMode};

use super::{
    AppState,
    error::{ApiError, ApiResult},
    worktrees::derived_row,
};
use crate::db::{Db, ProjectRow};
use crate::git::{
    command::{self, GIT_ENV_RO},
    host::GitHost,
    lock::{repo_lock_key, with_repo_lock},
    path::canon_key,
    remove::{CreatedWorktree, rollback_created_worktrees},
    repo::{
        AddWorktreeOptions, RunOpts, add_worktree, list_worktrees, main_worktree_of, probe_git,
        repo_root,
    },
};
use crate::virtualdir::CLAUDE_MD_BODY;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/projects/{id}/threads", post(create))
}

struct Repository {
    root: String,
    main: String,
}

async fn repositories(host: &GitHost, dirs: Vec<String>) -> ApiResult<Vec<Repository>> {
    let opts = RunOpts::default();
    let mut seen = HashSet::new();
    let mut repos = Vec::new();
    for dir in dirs {
        let root = repo_root(host, &dir, &opts)
            .await
            .map_err(|e| ApiError::bad_request(format!("{dir}：{e}")))?;
        let trees = list_worktrees(host, &root, &opts)
            .await
            .map_err(ApiError::internal)?;
        let main = main_worktree_of(&trees, &root);
        if !seen.insert(canon_key(host.kind, &main)) {
            return Err(ApiError::bad_request(format!(
                "环境中重复引用了仓库 {main}"
            )));
        }
        repos.push(Repository { root, main });
    }
    Ok(repos)
}

pub(super) async fn environment_repos(
    state: &AppState,
    dirs: Option<Vec<String>>,
) -> ApiResult<Vec<String>> {
    let dirs = dirs
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::bad_request("环境至少需要一个 Git 项目"))?;
    state
        .engine
        .call(move |e| async move {
            let local = ProjectRow {
                project_type: "local".into(),
                ..Default::default()
            };
            let host = e.git_host(&local).await.map_err(ApiError::internal)?;
            Ok(repositories(&host, dirs)
                .await?
                .into_iter()
                .map(|r| r.root)
                .collect())
        })
        .await?
}

async fn create(
    State(state): State<AppState>,
    Param(id): Param<String>,
    Json(input): Json<ThreadInput>,
) -> ApiResult<Json<Project>> {
    let lock = state.nodes.lock(&format!("environment:{id}"));
    let guard = lock.lock_owned().await;
    let env = state
        .db
        .get_project(&id)
        .ok_or_else(|| ApiError::not_found("环境不存在"))?;
    let Some(context) = env
        .environment_context
        .clone()
        .filter(|_| env.project_type == "local" && env.source_project_id.is_none())
    else {
        return Err(ApiError::bad_request("创建线程需要选择环境"));
    };
    let members = Db::parse_multi_repos(env.multi_repos.as_deref())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::conflict("环境缺少 Git 项目"))?;
    let name = input.name.trim().to_owned();
    if name.is_empty() {
        return Err(ApiError::bad_request("线程名称不能为空"));
    }
    state
        .engine
        .call(move |engine| async move {
            // 客户端断线后引擎仍会完成请求，环境锁也必须跟随引擎任务。
            let _guard = guard;
            let host = engine.git_host(&env).await.map_err(ApiError::internal)?;
            let repos = repositories(&host, members.into_iter().map(|m| m.dir).collect()).await?;
            let threads = engine.sessions.data_dir().join("threads");
            std::fs::create_dir_all(&threads).map_err(ApiError::internal)?;
            let threads = dunce::canonicalize(&threads).map_err(ApiError::internal)?;
            let id = crate::askpass::hub::uuid_v4();
            let branch = format!("falcon/thread-{id}");
            let root = threads.join(&id);
            let projects = root.join("projects");
            let mut plan = Vec::new();
            let mut names = HashSet::new();
            let mut manifest = format!(
                "# {name}\n\n## 环境 · {}\n\n{context}\n\n## 任务\n\n{}\n\n## 项目\n",
                env.name, input.task
            );
            for repo in repos {
                let inside = |source: &str| {
                    crate::git::path::is_ancestor(host.kind, source, &root.to_string_lossy())
                };
                if inside(&repo.root) || inside(&repo.main) {
                    return Err(ApiError::conflict(format!(
                        "线程目录位于源仓库中：{}",
                        root.display()
                    )));
                }
                let base = Path::new(&repo.root)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| ApiError::bad_request("仓库根目录缺少名称"))?;
                let mut alias = base.to_string();
                let mut suffix = 2;
                while !names.insert(alias.to_lowercase()) {
                    alias = format!("{base}-{suffix}");
                    suffix += 1;
                }
                let head = probe_git(
                    &host,
                    &command::head_commit_args(&host.git, &repo.root),
                    GIT_ENV_RO,
                    &RunOpts::default(),
                )
                .await
                .map_err(ApiError::internal)?;
                if head.code != Some(0) {
                    return Err(ApiError::conflict(format!(
                        "{} 缺少可检出的提交：{}",
                        repo.root, head.stderr
                    )));
                }
                let sha = head.stdout.trim().to_owned();
                manifest.push_str(&format!(
                    "\n- `projects/{alias}` ← `{}` @ `{sha}`（`{branch}`）",
                    repo.root
                ));
                plan.push((repo, projects.join(alias), sha));
            }
            manifest.push('\n');
            std::fs::create_dir(&root).map_err(ApiError::internal)?;
            let mut created = Vec::new();
            let result: anyhow::Result<Json<Project>> = async {
                std::fs::create_dir(&projects)?;
                write_context_files(&root, &manifest)?;
                for (repo, target, sha) in plan {
                    let opts = AddWorktreeOptions {
                        mode: WorktreeMode::NewBranch,
                        branch: branch.clone(),
                        dir: target.to_string_lossy().into_owned(),
                        start_point: Some(sha),
                    };
                    let dir = with_repo_lock(
                        repo_lock_key(&host, &repo.main),
                        add_worktree(&host, &repo.main, &opts),
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "{}：{}",
                            repo.root,
                            e.detail.as_deref().unwrap_or(&e.message)
                        )
                    })?;
                    created.push(CreatedWorktree {
                        dir,
                        repo_dir: repo.main,
                    });
                }
                let members: Vec<_> = created
                    .iter()
                    .map(|m| MultiRepoMember {
                        dir: m.dir.clone(),
                        repo_dir: Some(m.repo_dir.clone()),
                    })
                    .collect();
                let mut thread =
                    derived_row(&env, name, root.to_string_lossy().into_owned(), &branch);
                thread.id = id.clone();
                thread.multi_repos =
                    Some(serde_json::to_string(&members).expect("serialize members"));
                thread.environment_context = Some(manifest.clone());
                engine
                    .db
                    .try_insert_project(&thread, &id, thread.created_at)?;
                Ok(Json(Db::to_project(&thread)))
            }
            .await;
            match result {
                Ok(thread) => Ok(thread),
                Err(e) => {
                    let leftovers = rollback_created_worktrees(
                        &host,
                        &created,
                        &projects.to_string_lossy(),
                        true,
                    )
                    .await;
                    remove_context_files(&root, &manifest);
                    if root.exists() && std::fs::remove_dir(&root).is_err() {
                        return Err(ApiError::conflict(format!(
                            "{e}；请检查残留目录 {} {leftovers:?}",
                            root.display()
                        )));
                    }
                    Err(ApiError::conflict(e.to_string()))
                }
            }
        })
        .await?
}

fn write_context_files(root: &Path, manifest: &str) -> std::io::Result<()> {
    use std::io::Write;
    for (name, text) in [("AGENTS.md", manifest), ("CLAUDE.md", CLAUDE_MD_BODY)] {
        let file = root.join(name);
        let mut output = std::fs::File::create_new(&file)?;
        if let Err(e) = output.write_all(text.as_bytes()) {
            drop(output);
            let _ = std::fs::remove_file(file);
            return Err(e);
        }
    }
    Ok(())
}

/// 用户编辑过的线程上下文必须保留。只清理全文仍与初始快照相同的文件。
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
    use super::*;
    use crate::api::test_support::{TestApp, json_of};
    use crate::git::repo::tests::init_repo;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;

    #[test]
    fn thread_manifest_failure_preserves_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("CLAUDE.md")).unwrap();
        assert!(write_context_files(dir.path(), "notes").is_err());
        remove_context_files(dir.path(), "notes");
        assert!(!dir.path().join("AGENTS.md").exists());
        assert!(dir.path().join("CLAUDE.md").is_dir());
    }

    #[tokio::test]
    async fn thread_creation_and_cleanup() {
        let app = TestApp::new();
        let dirs = [app.root.join("web"), app.root.join("second/web")];
        for dir in &dirs {
            init_repo(dir);
        }
        let payload = json!({"type":"local","name":"E","environmentContext":"before","repos":dirs});
        let (code, env) = json_of(app.post_json("/api/environments", payload.clone()).await).await;
        assert_eq!(code, StatusCode::OK, "{env}");
        let env_id = env["id"].as_str().unwrap();
        let path = format!("/api/projects/{env_id}");
        let url = format!("{path}/threads");
        let req = || {
            Request::post(&url)
                .header("content-type", "application/json")
                .header("idempotency-key", "thread-op")
                .body(Body::from(r#"{"name":"same","task":"implement"}"#))
                .unwrap()
        };
        let (code, first) = json_of(app.send(req()).await).await;
        assert_eq!(code, StatusCode::OK, "{first}");
        assert_eq!(json_of(app.send(req()).await).await.1, first);
        let dir = Path::new(first["workingDir"].as_str().unwrap());
        for alias in ["web", "web-2"] {
            assert!(dir.join("projects").join(alias).join(".git").is_file());
        }
        let initial = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(
            initial.contains("before")
                && initial.contains("implement")
                && initial.contains("projects/web-2")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap(),
            CLAUDE_MD_BODY
        );
        let mut updated = payload;
        updated["environmentContext"] = json!("after");
        assert_eq!(app.put_json(&path, updated).await.status(), StatusCode::OK);
        let (_, second) = json_of(app.post_json(&url, json!({"name":"same"})).await).await;
        assert_ne!(first["workingDir"], second["workingDir"]);
        assert!(
            second["environmentContext"]
                .as_str()
                .unwrap()
                .contains("after")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            initial
        );
        let second_dir = Path::new(second["workingDir"].as_str().unwrap());
        let del = |id: &str| {
            Request::delete(format!("/api/projects/{id}"))
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(app.send(del(env_id)).await.status(), StatusCode::CONFLICT);
        assert_eq!(
            app.send(del(second["id"].as_str().unwrap())).await.status(),
            StatusCode::OK
        );
        assert!(!second_dir.exists());
        std::fs::write(dir.join("AGENTS.md"), "keep notes").unwrap();
        let first_id = first["id"].as_str().unwrap();
        assert_eq!(app.send(del(first_id)).await.status(), StatusCode::CONFLICT);
        let force = Request::delete(format!("/api/projects/{first_id}?force=true"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.send(force).await.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            "keep notes"
        );
        assert!(!dir.join("projects").exists());
        for d in dirs {
            assert!(d.join(".git").is_dir());
        }
    }

    #[tokio::test]
    async fn invalid_env_and_partial_thread_failure() {
        let app = TestApp::new();
        let plain = app.local_project("plain");
        let env = |dirs: serde_json::Value| json!({"name":"env","type":"local","repos":dirs});
        assert_eq!(
            app.post_json("/api/environments", env(json!([plain])))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.post_json("/api/projects/plain/threads", json!({"name":"x"}))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        let dirs = [app.root.join("one"), app.root.join("two")];
        for dir in &dirs {
            init_repo(dir);
        }
        // 第二个仓库有同名分支前缀，Git 在第一棵树建完后才拒绝创建第二棵树。
        let status = std::process::Command::new("git")
            .current_dir(&dirs[1])
            .args(["branch", "falcon"])
            .status()
            .unwrap();
        assert!(status.success());
        let (_, e) = json_of(app.post_json("/api/environments", env(json!(dirs))).await).await;
        let id = e["id"].as_str().unwrap();
        let (code, error) = json_of(
            app.post_json(&format!("/api/projects/{id}/threads"), json!({"name":"x"}))
                .await,
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(app.state.db.list_worktree_children(id).is_empty());
        assert_eq!(
            std::fs::read_dir(app.data_dir.join("threads"))
                .unwrap()
                .count(),
            0
        );
    }
}
