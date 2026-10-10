//! 项目的增删改查（routes.ts 的 `---- projects ----` 一段）。
//!
//! 附属项目（worktree 派生）只能经 `POST /api/projects/:id/worktrees` 创建（git_routes.rs），
//! 这里的写入路径刻意碰不到 worktree 四列与派生产物的成员清单——那是删除目标（ADR 0002 / 0003）。

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use falcon_proto::{DeleteProjectResult, MultiRepoMember, Project};
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult};
use super::hosts::validate_ssh_fields;
use super::input::{field, integer, present, str_field, trimmed, trimmed_non_empty, truthy};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::db::{Db, ProjectRow, SshHostRow};
use crate::git::command::parse_default_worktree_branch;
use crate::git::multi::validate_member_list;
use crate::git::path::is_absolute;
use crate::sessions::local::local_kind;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list).post(create))
        .route("/api/projects/{id}", axum::routing::put(update).delete(remove))
}

/// 成员仓库清单的校验 + 归一化。Ok(None) 表示请求里没带（不是多仓库容器 /
/// 编辑时成员不变）。local 逐成员 stat；ssh 成员刻意不预检——与 ssh 项目的
/// workingDir 零校验同理，连通性与"是不是仓库"都是派生时的事。
fn resolve_repos(input: &Value) -> Result<Option<Vec<String>>, String> {
    let raw = field(input, "repos");
    if !present(raw) {
        return Ok(None);
    }
    let repos = validate_member_list(raw.unwrap_or(&Value::Null))?;
    if str_field(input, "type") == Some("local") {
        for dir in &repos {
            // stat 会把相对路径解析到后端进程的 cwd 上，必须先拦绝对性
            if !is_absolute(local_kind(), dir) {
                return Err(format!("成员仓库必须是绝对路径：{dir}"));
            }
            match std::fs::metadata(dir) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(format!("成员路径不是文件夹：{dir}")),
                Err(_) => return Err(format!("成员文件夹不存在或不可访问：{dir}")),
            }
        }
    }
    Ok(Some(repos))
}

/// container = 多仓库容器：workingDir 只是可选的会话 cwd（留空 = 家目录，ssh 先例）
fn validate_project_input(input: &Value, container: bool) -> Option<String> {
    if trimmed(input, "name").is_none_or(str::is_empty) {
        return Some("项目名称不能为空".into());
    }
    match str_field(input, "type") {
        Some("local") => match trimmed(input, "workingDir").filter(|s| !s.is_empty()) {
            None => {
                if !container {
                    return Some("本地项目必须指定文件夹路径".into());
                }
            }
            // Node 版 stat 的是原文（没 trim）
            Some(_) => match std::fs::metadata(str_field(input, "workingDir").unwrap_or_default()) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Some("路径不是文件夹".into()),
                Err(_) => return Some("文件夹路径不存在或不可访问".into()),
            },
        },
        Some("ssh") => {
            // 选了已保存主机时连接配置从主机复制，ssh 字段忽略
            let host_id = truthy(field(input, "hostId"));
            let ssh = field(input, "ssh").filter(|v| truthy(Some(v)));
            if !host_id && ssh.is_none() {
                return Some("请选择一台已保存的远端主机".into());
            }
            if !host_id && let Some(err) = ssh.and_then(validate_ssh_fields) {
                return Some(err.into());
            }
        }
        _ => return Some("未知项目类型".into()),
    }
    parse_default_worktree_branch(field(input, "defaultWorktreeBranch")).err()
}

/// 项目行上的 SSH 列（从已保存主机复制，或手写）
#[derive(Default)]
struct ProjectSsh {
    host_id: Option<String>,
    ssh_host: Option<String>,
    ssh_port: Option<i64>,
    ssh_username: Option<String>,
    ssh_auth_method: Option<String>,
    ssh_key_path: Option<String>,
    ssh_secret_enc: Option<String>,
}

impl ProjectSsh {
    fn from_host(host: &SshHostRow) -> Self {
        ProjectSsh {
            host_id: Some(host.id.clone()),
            ssh_host: Some(host.host.clone()),
            ssh_port: Some(host.port),
            ssh_username: Some(host.username.clone()),
            ssh_auth_method: Some(host.auth_method.clone()),
            ssh_key_path: host.key_path.clone(),
            ssh_secret_enc: host.secret_enc.clone(),
        }
    }

    fn apply(self, row: &mut ProjectRow) {
        row.host_id = self.host_id;
        row.ssh_host = self.ssh_host;
        row.ssh_port = self.ssh_port;
        row.ssh_username = self.ssh_username;
        row.ssh_auth_method = self.ssh_auth_method;
        row.ssh_key_path = self.ssh_key_path;
        row.ssh_secret_enc = self.ssh_secret_enc;
    }
}

fn resolve_project_ssh(state: &AppState, input: &Value, existing: Option<&ProjectRow>) -> Result<ProjectSsh, String> {
    if str_field(input, "type") != Some("ssh") {
        return Ok(ProjectSsh::default());
    }
    if let Some(host_id) = str_field(input, "hostId").filter(|s| !s.is_empty()) {
        let host = state.db.get_host(host_id).ok_or("所选主机不存在")?;
        return Ok(ProjectSsh::from_host(&host));
    }
    let ssh = field(input, "ssh").filter(|v| truthy(Some(v)));
    let method = ssh.and_then(|s| str_field(s, "authMethod"));
    Ok(ProjectSsh {
        host_id: None,
        ssh_host: ssh
            .and_then(|s| trimmed(s, "host"))
            .map(str::to_string)
            .or_else(|| existing.and_then(|e| e.ssh_host.clone())),
        ssh_port: ssh
            .and_then(|s| field(s, "port"))
            .filter(|p| present(Some(p)))
            .and_then(|p| integer(Some(p)))
            .or_else(|| existing.and_then(|e| e.ssh_port))
            .or(Some(22)),
        ssh_username: ssh
            .and_then(|s| trimmed(s, "username"))
            .map(str::to_string)
            .or_else(|| existing.and_then(|e| e.ssh_username.clone())),
        ssh_auth_method: method.map(str::to_string).or_else(|| existing.and_then(|e| e.ssh_auth_method.clone())),
        ssh_key_path: if method == Some("key") {
            ssh.and_then(|s| trimmed(s, "keyPath"))
                .map(str::to_string)
                .or_else(|| existing.and_then(|e| e.ssh_key_path.clone()))
        } else if ssh.is_some() {
            None
        } else {
            existing.and_then(|e| e.ssh_key_path.clone())
        },
        ssh_secret_enc: match ssh.and_then(|s| str_field(s, "secret")).filter(|s| !s.is_empty()) {
            Some(secret) => Some(state.secrets.encrypt(secret)),
            None => existing.and_then(|e| e.ssh_secret_enc.clone()),
        },
    })
}

fn members_json(repos: &[String]) -> String {
    let members: Vec<MultiRepoMember> = repos.iter().map(|dir| MultiRepoMember { dir: dir.clone(), repo_dir: None }).collect();
    serde_json::to_string(&members).expect("成员清单总能序列化")
}

async fn list(State(state): State<AppState>) -> Json<Vec<Project>> {
    Json(state.db.list_projects().iter().map(Db::to_project).collect())
}

async fn create(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<Project>> {
    let input = &body.0;
    let repos = resolve_repos(input).map_err(ApiError::bad_request)?;
    if let Some(err) = validate_project_input(input, repos.is_some()) {
        return Err(ApiError::bad_request(err));
    }
    let ssh = resolve_project_ssh(&state, input, None).map_err(ApiError::bad_request)?;
    let default_worktree_branch = parse_default_worktree_branch(field(input, "defaultWorktreeBranch")).unwrap_or(None);

    let mut row = ProjectRow {
        id: uuid_v4(),
        name: trimmed(input, "name").unwrap_or_default().to_string(),
        project_type: str_field(input, "type").unwrap_or_default().to_string(),
        working_dir: trimmed_non_empty(input, "workingDir"),
        shell: trimmed_non_empty(input, "shell"),
        created_at: now_ms(),
        // 普通项目：worktree 四列一律 None。附属项目只能经
        // POST /api/projects/:id/worktrees 创建，绝不从这个端点进来
        source_project_id: None,
        worktree_branch: None,
        worktree_repo_dir: None,
        worktree_created_by_mojito: None,
        worktree_archived_at: None,
        // repos 有值 ⇒ 多仓库容器。派生产物的 multi_repos（带 repoDir 的那种）
        // 同样只能经 worktrees 端点进来
        multi_repos: repos.as_deref().map(members_json),
        default_worktree_branch,
        ..Default::default()
    };
    ssh.apply(&mut row);
    state.db.insert_project(&row);
    Ok(Json(Db::to_project(&row)))
}

async fn update(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Project>> {
    let existing = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let input = &body.0;
    if str_field(input, "type") != Some(existing.project_type.as_str()) {
        return Err(ApiError::bad_request("项目类型不可更改"));
    }
    // 附属项目的工作目录就是删除目标。它一旦能从这里改写，"删项目会删目录"
    // 就变成了"删项目会删你填的任意路径"——这是本功能唯一的灾难性风险。
    // 护栏还有深度 / 仓库根 / home / worktree 注册这几层，但第一层必须在这儿。
    if existing.source_project_id.is_some() && trimmed_non_empty(input, "workingDir") != existing.working_dir {
        return Err(ApiError::bad_request("附属项目的工作目录由 worktree 决定，不可修改"));
    }
    // 成员清单的写入护栏，与 working_dir 同构：派生行的成员是删除目标，显式拒绝
    // 是第一道防线（update_multi_repos 的 SQL 条件是第二道）；普通项目也不能凭空
    // 变成容器——判别式在创建时定死，与 worktree 同一条规矩。
    if field(input, "repos").is_some() {
        if existing.source_project_id.is_some() {
            return Err(ApiError::bad_request("附属项目的成员由派生决定，不可修改"));
        }
        if existing.multi_repos.is_none() {
            return Err(ApiError::bad_request("普通项目不能改成多仓库项目"));
        }
    }
    let repos = resolve_repos(input).map_err(ApiError::bad_request)?;
    let container = existing.multi_repos.is_some() && existing.source_project_id.is_none();
    if let Some(err) = validate_project_input(input, container) {
        return Err(ApiError::bad_request(err));
    }
    let ssh = resolve_project_ssh(&state, input, Some(&existing)).map_err(ApiError::bad_request)?;
    let default_worktree_branch = parse_default_worktree_branch(field(input, "defaultWorktreeBranch")).unwrap_or(None);

    let mut row = ProjectRow {
        name: trimmed(input, "name").unwrap_or_default().to_string(),
        working_dir: trimmed_non_empty(input, "workingDir"),
        shell: trimmed_non_empty(input, "shell"),
        // 附属项目不能再派生，这项对它们没有意义；仍照单全收，避免 PUT 形状因行而异
        default_worktree_branch,
        ..existing.clone()
    };
    ssh.apply(&mut row);
    state.db.update_project(&row);
    if container && let Some(repos) = &repos {
        let members: Vec<MultiRepoMember> =
            repos.iter().map(|dir| MultiRepoMember { dir: dir.clone(), repo_dir: None }).collect();
        state.db.update_multi_repos(&row.id, &members);
        row.multi_repos = Some(members_json(repos));
    }
    if row.project_type == "ssh" {
        // 附属项目的 ssh_* 是从源项目复制来的（让 SshLink / get_link / GET host 全都
        // 不用改），代价就是这条手动传播。本地项目没有可传播的东西。
        state.db.update_children_ssh(&row.id, &row);
    }
    // px0 开的是旧目录 / 旧 shell / 旧主机：停掉，下次打开按新配置起。只改名不打扰
    let stop_px0 = row.working_dir != existing.working_dir
        || row.shell != existing.shell
        || row.ssh_host != existing.ssh_host
        || row.ssh_port != existing.ssh_port
        || row.ssh_username != existing.ssh_username;
    let refreshed = row.clone();
    state.engine.send(move |engine| {
        let id = refreshed.id.clone();
        if container {
            // 名字与成员都会进虚拟目录的清单（见 virtualdir.rs），编辑后作废缓存。
            // 刷新是 fire-and-forget：宿主离线不能挡编辑；正在跑的会话尽快看到新清单
            // 即可，失败也无妨——attach 路径不走缓存，下次开会话/文件面板会重写
            engine.sessions.invalidate_virtual_dir(&refreshed.id);
            if refreshed.working_dir.is_none() {
                let sessions = engine.sessions.clone();
                tokio::task::spawn_local(async move {
                    if let Err(e) = sessions.ensure_virtual_dir(&refreshed, true).await {
                        log::warn!("虚拟项目目录刷新失败（下次开会话时会重写）：{e:#}");
                    }
                });
            }
        }
        if stop_px0 {
            let engine = engine.clone();
            tokio::task::spawn_local(async move { engine.stop_px0(&id).await });
        }
    });
    Ok(Json(Db::to_project(&row)))
}

#[derive(serde::Deserialize)]
struct DeleteQuery {
    force: Option<String>,
}

/// 删除项目，连坐它的附属项目。
///
/// 顺序上有三个硬约束：
/// 1. 会话必须先死透，再动目录（Zellij pane 的 cwd 就在里面，见 terminate 的注释）。
/// 2. dispose_link 必须排在 git 命令之后——那些命令要经这条 SSH 链路跑。
/// 3. DB 行无条件删，文件系统清理 best-effort。留一条删不掉的项目行，用户唯一的
///    出路是去改 SQLite；残留目录他自己删得掉，路径已经写进 warnings 了。
async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<DeleteProjectResult>> {
    let project = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;

    // 附属项目连坐。会话数要把它们的也算进去，否则确认框上的数字对不上
    let mut targets =
        if project.source_project_id.is_some() { Vec::new() } else { state.db.list_worktree_children(&id) };
    targets.push(project); // 先删附属，最后删自己
    let alive: Vec<String> = targets
        .iter()
        .flat_map(|p| state.db.list_sessions_by_project(&p.id))
        .filter(|s| s.state != "dead")
        .map(|s| s.id)
        .collect();
    if !alive.is_empty() && q.force.as_deref() != Some("true") {
        return Err(ApiError::conflict(format!("项目仍有 {} 个未终止的会话", alive.len())));
    }

    let res = state
        .engine
        .call(move |engine| async move {
            // 先把所有会话终止掉再动任何目录。terminate 不会失败（链路故障只是等不到消失），
            // Node 版那条"终止失败就整体放弃"的 502 在这里走不到
            for sid in &alive {
                engine.sessions.terminate(sid, true).await;
            }
            // px0 同理：它开着 worktree 目录（索引、git 监听），先停再删
            futures::future::join_all(targets.iter().map(|p| engine.stop_px0(&p.id))).await;

            let doomed: Vec<&str> = targets.iter().map(|p| p.id.as_str()).collect();
            // guard_dirs_of 除 working_dir 外还收多仓库项目的成员路径——容器的成员是
            // 用户的真仓库，别的删除不许踩上去
            let other_dirs: Vec<String> = engine
                .db
                .list_projects()
                .iter()
                .filter(|p| !doomed.contains(&p.id.as_str()))
                .flat_map(Db::guard_dirs_of)
                .collect();

            let mut warnings = Vec::new();
            for row in &targets {
                // 只有附属项目才碰文件系统。源项目的目录不是 falcon 建的，永远不动
                if row.source_project_id.is_some() {
                    match engine.cleanup_worktree(row, &other_dirs).await {
                        Ok(w) => warnings.extend(w),
                        Err(e) => warnings.push(format!(
                            "没能在宿主机上执行清理，目录未删除：{}（{}）",
                            row.working_dir.as_deref().unwrap_or_default(),
                            e.detail.clone().unwrap_or_else(|| e.message.clone())
                        )),
                    }
                } else if row.multi_repos.is_some() {
                    // 容器：清 falcon 根下的虚拟项目目录（克制删除，非空保留 + warning，
                    // 见 manager.remove_virtual_dir）。它在 falcon 自己的数据根里，链路都
                    // 连不上时连目录在不在都不知道，只记日志不打扰用户
                    match engine.sessions.remove_virtual_dir(row).await {
                        Ok(Some(w)) => warnings.push(w),
                        Ok(None) => {}
                        Err(e) => log::warn!("虚拟项目目录清理失败：{e:#}"),
                    }
                }
                engine.sessions.dispose_link(&row.id);
                engine.db.delete_project(&row.id);
            }
            warnings
        })
        .await;
    let warnings = res.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(DeleteProjectResult { ok: true, warnings: (!warnings.is_empty()).then_some(warnings) }))
}
