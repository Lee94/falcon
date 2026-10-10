//! 附属项目（git worktree，ADR 0002 / 0003）：派生、删除前预检、存档 / 恢复。
//! routes.ts 的 `POST /api/projects/:id/worktrees`（含多仓库批量派生 handleMultiDerive）、
//! `GET /api/projects/:id/worktree`、`POST /archive`、`POST /restore`。
//!
//! 与 GET /repo 的分工：那边是探测，环境事实如实报告；这边是操作，同样的事实一律当
//! 状态冲突（409）。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use falcon_proto::{
    MultiDeriveError, MultiDeriveFailedMember, MultiRepoMember, MultiWorktreeInput, MultiWorktreeMode, Project,
    WorktreeFailure, WorktreeMode, WorktreeStatus,
};
use serde_json::{Value, json};

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult};
use super::git_routes::detail_of;
use super::input::{field, present, str_field, trimmed, trimmed_non_empty};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::db::{Db, ProjectRow};
use crate::engine::Engine;
use crate::git::error::{WorktreeError, git_error_line, worktree_failure_text};
use crate::git::lock::{repo_lock_key, with_repo_lock};
use crate::git::multi::{DeriveMultiError, derive_multi_worktrees};
use crate::git::path::{basename_of, is_absolute, is_ancestor, is_unc, path_depth, sibling_worktree_path, veto_target_dir};
use crate::git::repo::{AddWorktreeOptions, RunOpts, add_worktree, path_exists, repo_root, worktree_status};
use crate::virtualdir::{
    CentralMember, central_manifest_files, posix_write_manifest_command, windows_write_manifest_commands,
    write_local_manifest,
};
use crate::zellij::host::HostKind;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/worktrees", post(derive))
        .route("/api/projects/{id}/worktree", get(status))
        .route("/api/projects/{id}/archive", post(archive))
        .route("/api/projects/{id}/restore", post(restore))
}

/// WorktreeFailure → HTTP 码。
///
/// 环境事实与状态冲突一律 409（用户去改环境 / 换个分支就能过），
/// 只有"命令没跑起来"和"git 自己失败了"才是 502。
pub fn worktree_status_code(reason: WorktreeFailure) -> StatusCode {
    match reason {
        WorktreeFailure::WorktreeAddFailed | WorktreeFailure::LinkFailed | WorktreeFailure::Unknown => {
            StatusCode::BAD_GATEWAY
        }
        _ => StatusCode::CONFLICT,
    }
}

/// `err.detail ? `${err.message}：${gitErrorLine(err.detail)}` : err.message`
fn worktree_error_text(message: &str, detail: Option<&str>) -> String {
    match detail.filter(|d| !d.is_empty()) {
        Some(d) => format!("{message}：{}", git_error_line(d)),
        None => message.to_string(),
    }
}

fn worktree_api_error(e: &WorktreeError) -> ApiError {
    ApiError::new(worktree_status_code(e.reason), worktree_error_text(&e.message, e.detail.as_deref()))
}

/// 附属项目行：ssh_* 与 host_id 从源项目整行复制（含密文，同一个 SecretBox 能解，全程不碰明文）。
///
/// SshLink 由 ProjectRow 构造、按 project.id 缓存，get_link / prepare_zellij / GET host 全部
/// 直接读 project.ssh_host。复制让这些点一处都不用改，也让删除清理不依赖源项目行还在不在。
/// 代价是源项目改配置时要手动传播（见 PUT 里的 update_children_ssh）；已保存主机改配置走
/// update_projects_from_host。
///
/// 顺带一个白捡的正确行为：Zellij 授权按 host+port+username 记，所以附属项目第一次开
/// 会话不会再弹一次安装授权。
fn derived_row(src: &ProjectRow, name: String, working_dir: String, branch: &str) -> ProjectRow {
    ProjectRow {
        id: uuid_v4(),
        name,
        project_type: src.project_type.clone(),
        working_dir: Some(working_dir),
        shell: src.shell.clone(),
        ssh_host: src.ssh_host.clone(),
        ssh_port: src.ssh_port,
        ssh_username: src.ssh_username.clone(),
        ssh_auth_method: src.ssh_auth_method.clone(),
        ssh_key_path: src.ssh_key_path.clone(),
        ssh_secret_enc: src.ssh_secret_enc.clone(),
        host_id: src.host_id.clone(),
        created_at: now_ms(),
        source_project_id: Some(src.id.clone()),
        worktree_branch: Some(branch.to_string()),
        worktree_repo_dir: None,
        worktree_created_by_mojito: Some(1),
        worktree_archived_at: None,
        multi_repos: None,
        default_worktree_branch: None,
    }
}

/// 派生一个附属项目。
async fn derive(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Project>> {
    let src = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;

    // 禁止二级派生。git 本身允许（从 linked worktree 建的 worktree 仍属同一仓库），
    // 但侧栏的两级树会变成任意深度、删除级联要递归，而收益是零——从源项目派生
    // 完全等价。将来若要放开：把 source 取成 `row.source_project_id ?? row.id`
    // 重新挂到根即可，树仍是两级。多仓库派生行也带 source_project_id，天然被挡。
    if src.source_project_id.is_some() {
        return Err(ApiError::conflict("附属项目不能再派生，请从它的源项目派生"));
    }
    // 多仓库容器在 working_dir 检查之前分叉：容器允许没有工作目录
    if let Some(members) = Db::parse_multi_repos(src.multi_repos.as_deref()) {
        return derive_multi(state, src, members, body.0).await;
    }
    let Some(working_dir) = src.working_dir.clone().filter(|d| !d.is_empty()) else {
        return Err(ApiError::conflict(worktree_failure_text(WorktreeFailure::NoWorkingDir)));
    };

    let input = &body.0;
    let mode = match str_field(input, "mode") {
        Some("new-branch") => WorktreeMode::NewBranch,
        Some("existing-branch") => WorktreeMode::ExistingBranch,
        _ => return Err(ApiError::bad_request("未知的派生方式")),
    };
    let Some(branch) = trimmed(input, "branch").filter(|b| !b.is_empty()).map(str::to_string) else {
        return Err(ApiError::bad_request("分支名不能为空"));
    };
    let name = trimmed_non_empty(input, "name").unwrap_or_else(|| branch.clone());
    let dir_input = trimmed_non_empty(input, "dir");
    let start_point = trimmed_non_empty(input, "startPoint");

    let res = state
        .engine
        .call(move |engine| async move {
            let host = engine.git_host(&src).await.map_err(|e| worktree_api_error(&e))?;
            let main = repo_root(&host, &working_dir, &RunOpts::default()).await.map_err(|e| worktree_api_error(&e))?;
            let kind = host.kind;

            let dir = dir_input.unwrap_or_else(|| sibling_worktree_path(kind, &main, &branch));
            if !is_absolute(kind, &dir) {
                return Err(ApiError::bad_request("目标目录必须是绝对路径"));
            }
            if is_unc(kind, &dir) {
                return Err(ApiError::bad_request("暂不支持在 UNC 网络路径上派生"));
            }
            // 与删除护栏同一个门槛：真实的 worktree 都是"某个仓库目录的同级"，
            // 而仓库不会直接躺在盘符根上。这里挡住，删除那边就永远不会遇到
            if path_depth(kind, &dir) < 2 {
                return Err(ApiError::bad_request("目标目录过于靠近文件系统根，请换一个位置"));
            }
            if is_ancestor(kind, &dir, &main) {
                return Err(ApiError::bad_request("目标目录包含仓库根，拒绝创建"));
            }
            if let Some(veto) = veto_target_dir(kind, &main, &dir) {
                let text = worktree_failure_text(veto);
                return Err(ApiError::conflict(if veto == WorktreeFailure::PathInsideRepo {
                    format!("{text}：源仓库会把它当成一堆未跟踪文件")
                } else {
                    format!("{text}：{} 字符，Windows 上建得出来却删不掉", dir.encode_utf16().count())
                }));
            }

            let created = with_repo_lock(repo_lock_key(&host, &main), async {
                // 锁内再查一次占用：预检与创建之间用户可能刚建了同名目录。
                // 空目录也拒绝——git 的 add 只在非空时 die，但"接管一个已存在的空目录"
                // 不是用户要的语义，也不该继承"删项目会删这个目录"的承诺
                if path_exists(&host, &dir, &RunOpts::default()).await? {
                    return Err(WorktreeError::new(
                        WorktreeFailure::PathOccupied,
                        worktree_failure_text(WorktreeFailure::PathOccupied),
                        Some(dir.clone()),
                    ));
                }
                add_worktree(&host, &main, &AddWorktreeOptions { mode, branch: branch.clone(), start_point, dir: dir.clone() })
                    .await
            })
            .await
            .map_err(|e| worktree_api_error(&e))?;

            let mut row = derived_row(&src, name, created, &branch);
            row.worktree_repo_dir = Some(main);
            engine.db.insert_project(&row);
            Ok(Db::to_project(&row))
        })
        .await?;
    res.map(Json)
}

/// 多仓库容器的批量派生分支：统一分支名、逐成员建树、**全有或全无**。
/// 成功产出**一个**多仓库附属项目（multi 与 worktree 同时存在）；任一成员失败
/// 由 derive_multi_worktrees 回滚，这里只负责翻译三类错误：
/// 容器级校验 → 409 纯文本；成员级失败 → 按 reason 映射 + member 归因
/// （回滚有残留 ⇒ 一律 502 + leftover 路径原样列出）；其余 → 502。
async fn derive_multi(
    state: AppState,
    src: ProjectRow,
    members: Vec<MultiRepoMember>,
    input: Value,
) -> ApiResult<Json<Project>> {
    if members.is_empty() {
        return Err(ApiError::conflict("容器没有成员仓库"));
    }
    let mode = match str_field(&input, "mode") {
        Some("new-branch") => MultiWorktreeMode::NewBranch,
        Some("existing-branch") => MultiWorktreeMode::ExistingBranch,
        Some("auto") => MultiWorktreeMode::Auto,
        _ => return Err(ApiError::bad_request("未知的派生方式")),
    };
    if present(field(&input, "startPoint")) {
        return Err(ApiError::bad_request("批量派生不支持在请求里指定基点，新建分支用源项目的默认 worktree 基点"));
    }
    let Some(branch) = trimmed(&input, "branch").filter(|b| !b.is_empty()).map(str::to_string) else {
        return Err(ApiError::bad_request("分支名不能为空"));
    };
    let multi_input = MultiWorktreeInput {
        name: str_field(&input, "name").map(str::to_string),
        mode,
        branch: branch.clone(),
        dir: str_field(&input, "dir").map(str::to_string),
    };
    let name = trimmed_non_empty(&input, "name").unwrap_or_else(|| branch.clone());

    let res = state
        .engine
        .call(move |engine| async move {
            let host = engine.git_host(&src).await.map_err(|e| worktree_api_error(&e))?;
            let start_point = src.default_worktree_branch.clone().filter(|s| !s.is_empty());
            let outcome = derive_multi_worktrees(&host, &src.name, &members, &multi_input, start_point.as_deref())
                .await
                .map_err(|e| multi_error(&e))?;

            let created: Vec<MultiRepoMember> = outcome
                .members
                .iter()
                .map(|m| MultiRepoMember { dir: m.dir.clone(), repo_dir: Some(m.repo_dir.clone()) })
                .collect();
            let mut row = derived_row(&src, name, outcome.central_dir.clone(), &branch);
            // 单值列对多仓库无意义：逐成员的仓库根在 multi_repos 里
            row.multi_repos = Some(serde_json::to_string(&created).expect("成员清单总能序列化"));

            // 集中目录清单：给 coding agent 的结构说明（见 virtualdir.rs）。写失败**不回滚**：
            // 全有或全无护的是 worktree——建错了要付回滚代价的东西；清单是纯引导文件、
            // 可再生，为它逆序 remove N 棵刚建好的树得不偿失。集中目录必然是本请求
            // 新建的（原先不存在才允许派生），首写无覆盖风险。
            let central: Vec<CentralMember> = outcome
                .members
                .iter()
                .map(|m| CentralMember { base: basename_of(host.kind, &m.dir), repo_dir: m.repo_dir.clone() })
                .collect();
            let files = central_manifest_files(&row.name, &branch, &central);
            if let Err(e) = write_central_manifest(&engine, &src, host.kind, &outcome.central_dir, &files, &host.exec).await
            {
                log::warn!("派生成功但集中目录清单写入失败：{}（{e:#}）", outcome.central_dir);
            }

            engine.db.insert_project(&row);
            Ok(Db::to_project(&row))
        })
        .await?;
    res.map(Json)
}

async fn write_central_manifest(
    engine: &Engine,
    src: &ProjectRow,
    kind: HostKind,
    central_dir: &str,
    files: &[crate::virtualdir::ManifestFile],
    exec: &std::rc::Rc<dyn crate::exec::Exec>,
) -> anyhow::Result<()> {
    let fail = |stderr: &str, code: Option<i32>| {
        let trimmed = crate::term_env::js::trim(stderr);
        if trimmed.is_empty() {
            anyhow::anyhow!("exit {}", code.map(|c| c.to_string()).unwrap_or_else(|| "null".into()))
        } else {
            anyhow::anyhow!("{trimmed}")
        }
    };
    if src.project_type == "local" {
        write_local_manifest(std::path::Path::new(central_dir), files)?;
    } else if kind == HostKind::Windows {
        let link = engine.sessions.get_link(src);
        for w in windows_write_manifest_commands(central_dir, files) {
            let res = link.exec_with_input(&w.cmd, w.stdin_base64.as_bytes()).await?;
            if !res.ok() {
                return Err(fail(&res.stderr, res.code));
            }
        }
    } else {
        let res = exec.exec(&posix_write_manifest_command(central_dir, files), None).await?;
        if !res.ok() {
            return Err(fail(&res.stderr, res.code));
        }
    }
    Ok(())
}

fn multi_error(e: &DeriveMultiError) -> ApiError {
    match e {
        DeriveMultiError::Veto(v) => ApiError::conflict(v.0.clone()),
        DeriveMultiError::Member(m) => {
            // 回滚有残留 ⇒ 一律 502：不管起因是什么，宿主机上已经躺着要人收拾的目录
            let status = if m.leftover.is_empty() { worktree_status_code(m.reason) } else { StatusCode::BAD_GATEWAY };
            let out = MultiDeriveError {
                error: format!("成员 {} 派生失败：{}", m.member_dir, worktree_error_text(&m.message, m.detail.as_deref())),
                member: Some(MultiDeriveFailedMember { dir: m.member_dir.clone(), reason: m.reason, detail: m.detail.clone() }),
                leftover: (!m.leftover.is_empty()).then(|| m.leftover.clone()),
            };
            ApiError { status, body: serde_json::to_value(out).unwrap_or_else(|_| json!({})) }
        }
        DeriveMultiError::Worktree(w) => worktree_api_error(w),
    }
}

/// 附属项目的工作区状态：删除前的预检。
///
/// 读不到状态不阻断删除，只是让确认框改口说"无法确认里面有没有未保存的东西"——
/// 把"读不到"和"是干净的"混成一个答案，才是真会让用户丢东西的做法。
async fn status(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<WorktreeStatus>> {
    let row = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let Some(working_dir) = row.working_dir.clone().filter(|_| row.source_project_id.is_some()).filter(|d| !d.is_empty())
    else {
        return Err(ApiError::bad_request("不是附属项目"));
    };
    let members = Db::parse_multi_repos(row.multi_repos.as_deref());
    let res = state
        .engine
        .call(move |engine| async move {
            let host = engine.git_host(&row).await?;
            let Some(members) = members else {
                return worktree_status(&host, &working_dir, &RunOpts::default()).await;
            };
            // 多仓库派生行：逐成员汇总成同一个 WorktreeStatus 形状，确认框零改动。
            // 样例路径带 <成员目录名>/ 前缀，用户一眼能看出脏文件在哪棵 worktree 里
            let mut merged = WorktreeStatus {
                present: false,
                dirty_count: 0,
                dirty_sample: Vec::new(),
                ignored_count: 0,
                ahead: None,
                error: None,
            };
            for m in &members {
                let s = worktree_status(&host, &m.dir, &RunOpts::default()).await?;
                let base = basename_of(host.kind, &m.dir);
                merged.present |= s.present;
                merged.dirty_count += s.dirty_count;
                merged.ignored_count += s.ignored_count;
                if let Some(ahead) = s.ahead {
                    merged.ahead = Some(merged.ahead.unwrap_or(0) + ahead);
                }
                merged.dirty_sample.extend(s.dirty_sample.iter().map(|f| format!("{base}/{f}")));
                if let Some(err) = s.error {
                    merged.error = Some(match merged.error.take() {
                        Some(prev) => format!("{prev}；{err}"),
                        None => err,
                    });
                }
            }
            merged.dirty_sample.truncate(8);
            Ok(merged)
        })
        .await?;
    Ok(Json(res.unwrap_or_else(|e: WorktreeError| WorktreeStatus {
        present: true,
        dirty_count: 0,
        dirty_sample: Vec::new(),
        ignored_count: 0,
        ahead: None,
        error: Some(detail_of(&e)),
    })))
}

/// 存档附属项目：从侧栏隐藏，worktree 目录与分支原样保留，到期由后台清扫
/// 自动删除（见 archive.rs）；此前随时可恢复。
///
/// 会话与删除一样连坐——项目都收起来了，留着挂在上面的终端只会变成幽灵 tab。
/// （Node 版"终止失败就整体放弃"的分支在这里走不到：terminate 不会失败。）
async fn archive(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Project>> {
    let row = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    if row.source_project_id.is_none() {
        return Err(ApiError::bad_request("只有附属项目能存档"));
    }
    if row.worktree_archived_at.is_some() {
        return Ok(Json(Db::to_project(&row))); // 幂等
    }
    let project = state
        .engine
        .call(move |engine| async move {
            for s in engine.db.list_sessions_by_project(&id) {
                if s.state == "dead" {
                    engine.sessions.delete_dead(&s.id);
                } else {
                    engine.sessions.terminate(&s.id, false).await;
                }
            }
            engine.stop_px0(&id).await;
            engine.db.set_worktree_archived(&id, Some(now_ms()));
            engine.db.get_project(&id).map(|r| Db::to_project(&r))
        })
        .await?;
    project.map(Json).ok_or_else(|| ApiError::not_found("项目不存在"))
}

/// 恢复已存档的附属项目。会话在存档时已经终止，恢复后按需新建。
async fn restore(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Project>> {
    let row = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    if row.worktree_archived_at.is_none() {
        return Ok(Json(Db::to_project(&row))); // 幂等
    }
    state.db.set_worktree_archived(&id, None);
    let row = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    Ok(Json(Db::to_project(&row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_node_table() {
        for r in [
            WorktreeFailure::GitMissing,
            WorktreeFailure::NotARepo,
            WorktreeFailure::NoWorkingDir,
            WorktreeFailure::BranchInUse,
            WorktreeFailure::BranchExists,
            WorktreeFailure::BranchUnknown,
            WorktreeFailure::PathOccupied,
            WorktreeFailure::PathInsideRepo,
            WorktreeFailure::PathTooLong,
        ] {
            assert_eq!(worktree_status_code(r), StatusCode::CONFLICT, "{r:?}");
        }
        assert_eq!(worktree_status_code(WorktreeFailure::WorktreeAddFailed), StatusCode::BAD_GATEWAY);
        assert_eq!(worktree_status_code(WorktreeFailure::LinkFailed), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn error_text_picks_the_fatal_line() {
        assert_eq!(worktree_error_text("x", None), "x");
        assert_eq!(worktree_error_text("x", Some("")), "x");
        assert_eq!(worktree_error_text("x", Some("Preparing worktree\nfatal: nope")), "x：fatal: nope");
    }
}
