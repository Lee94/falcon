//! 宿主机项目清单（`<falcon 根>/host-projects.json`）：跨「本机 Server 访问」与「远端 SSH 连接」同步项目。
//!
//! 为什么在宿主机 `<falcon 根>`（`~/.falcon`）落一份 `host-projects.json`：
//! - 主机 A 通过 SSH 连接主机 C 并创建项目后，主机 B 无论通过 SSH 连接主机 C、还是直接连接主机 C
//!   上的 `falcon-server`，本质上访问的都是主机 C 这台宿主机；
//! - 主机 C 上不一定装有 `falcon-server` 或系统 `sqlite3` 命令行，但一定有 `<falcon 根>` 目录
//!   （`SshLink::probe` 探测出的 `facts.root` 与 `falcon-server` 默认的 `data_dir` 都是 `~/.falcon`）；
//! - 清单只存宿主机维度的项目属性（名称、宿主机上的工作目录、shell、worktree 与多仓库元数据）与删除墓碑，
//!   **绝不写入任何 SSH 凭据或本机 `host_id`**：
//!   - 主机 C 本地的 `falcon-server` 将其映射为 `type = "local"` 项目；
//!   - 主机 A / B 通过 SSH 连接主机 C 时，将其映射为绑定各自 `SshHost` 的 `type = "ssh"` 项目。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::askpass::hub::uuid_v4;
use crate::db::{Db, ProjectRow, SshHostRow, SyncProjectRow};
use crate::exec::Exec as _;
use crate::git::path::join_path;
use crate::host::{HostKind, encode_powershell, quote_posix, quote_powershell};
use crate::sessions::ssh::SshLink;

pub const HOST_PROJECTS_FILE: &str = "host-projects.json";

static LOCAL_SYNC_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostProjectEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_repo_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_created_by_mojito: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_archived_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_repos: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_worktree_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostProjectsManifest {
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<HostProjectEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deleted: BTreeMap<String, i64>,
}

impl Default for HostProjectsManifest {
    fn default() -> Self {
        Self { version: 1, projects: Vec::new(), deleted: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SyncScope<'a> {
    Local,
    SshHost(&'a SshHostRow),
}

impl SyncScope<'_> {
    pub fn key(&self) -> String {
        match self {
            SyncScope::Local => "local".to_string(),
            SyncScope::SshHost(host) => format!("host:{}", host.id),
        }
    }

    fn matches(&self, row: &ProjectRow) -> bool {
        match self {
            SyncScope::Local => row.project_type == "local",
            SyncScope::SshHost(host) => {
                row.project_type == "ssh" && row.host_id.as_deref() == Some(host.id.as_str())
            }
        }
    }
}

pub fn remote_projects_path(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, HOST_PROJECTS_FILE])
}

pub fn remote_read_projects_command(kind: HostKind, root: &str) -> String {
    let file = remote_projects_path(kind, root);
    if kind == HostKind::Windows {
        let q = quote_powershell(&file);
        let script = format!(
            "$p = {q}; \
             if (Test-Path -LiteralPath $p -PathType Leaf) {{ \
                 [Console]::Out.Write([IO.File]::ReadAllText($p, [Text.Encoding]::UTF8)) \
             }}"
        );
        return encode_powershell(&script);
    }
    let q = quote_posix(&file);
    format!("if [ -f {q} ]; then cat {q}; fi")
}

pub fn remote_write_projects_command(kind: HostKind, root: &str, json: &str) -> (String, Vec<u8>) {
    let file = remote_projects_path(kind, root);
    if kind == HostKind::Windows {
        let r = quote_powershell(root);
        let f = quote_powershell(&file);
        let tmp = quote_powershell(&format!("{file}.tmp.{}", uuid_v4()));
        let script = format!(
            "$ErrorActionPreference = 'Stop'; \
             $b64 = [Console]::In.ReadToEnd(); \
             $bytes = [Convert]::FromBase64String($b64.Trim()); \
             [IO.Directory]::CreateDirectory({r}) | Out-Null; \
             $tmp = {tmp}; \
             [IO.File]::WriteAllBytes($tmp, $bytes); \
             Move-Item -LiteralPath $tmp -Destination {f} -Force"
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        return (encode_powershell(&script), b64.into_bytes());
    }
    let r = quote_posix(root);
    let f = quote_posix(&file);
    let cmd = format!("mkdir -p {r} && tmp={f}.tmp.$$ && cat > \"$tmp\" && mv -f \"$tmp\" {f}");
    (cmd, json.as_bytes().to_vec())
}

fn parse_manifest(raw: &str) -> Result<HostProjectsManifest, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(HostProjectsManifest::default());
    }
    let mut m: HostProjectsManifest = serde_json::from_str(raw)?;
    if m.version == 0 {
        m.version = 1;
    }
    Ok(m)
}

fn canonical_sort(projects: &mut [HostProjectEntry]) {
    projects.sort_by(|a, b| {
        a.source_project_id
            .is_some()
            .cmp(&b.source_project_id.is_some())
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// 将 `db` 中属于 `scope` 的项目与宿主机清单 `remote` 双向合并。
/// 返回 `Some(next)` 表示宿主机清单有变更、需要写回；`None` 表示宿主机清单已是最新的规范形态。
pub fn merge_scope(db: &Db, scope: SyncScope<'_>, remote: &HostProjectsManifest) -> Option<HostProjectsManifest> {
    let scope_key = scope.key();
    let scope_rows: Vec<SyncProjectRow> =
        db.list_sync_projects().into_iter().filter(|sp| scope.matches(&sp.row)).collect();
    let local_deleted = db.list_deleted_projects(&scope_key);

    let mut deleted = remote.deleted.clone();
    for (rid, &ts) in &local_deleted {
        deleted.entry(rid.clone()).and_modify(|e| *e = (*e).max(ts)).or_insert(ts);
    }

    // 1. 远端已删除且墓碑比本地更新的项目：若本机没有活跃/待接回会话，从本机 DB 删除
    for sp in &scope_rows {
        if let Some(&del_ts) = remote.deleted.get(&sp.remote_id) {
            if del_ts > sp.updated_at {
                let has_live = db
                    .list_sessions_by_project(&sp.row.id)
                    .iter()
                    .any(|s| s.state != "dead");
                if !has_live {
                    db.delete_synced_project(&sp.row.id, &scope_key, &sp.remote_id, del_ts);
                }
            } else {
                deleted.remove(&sp.remote_id);
                db.clear_deleted_project(&scope_key, &sp.remote_id);
            }
        }
    }

    for (rid, &del_ts) in &remote.deleted {
        if local_deleted.get(rid).is_none_or(|&t| del_ts > t)
            && !scope_rows.iter().any(|sp| sp.remote_id == *rid && sp.updated_at >= del_ts)
        {
            db.upsert_deleted_project(&scope_key, rid, del_ts);
        }
    }

    // 2. 把远端清单中的新增或更新项目合入本机 DB（先源项目、后附属项目，保证 source_project_id 映射就绪）
    let scope_rows: Vec<SyncProjectRow> =
        db.list_sync_projects().into_iter().filter(|sp| scope.matches(&sp.row)).collect();
    let mut remote_to_local_id: HashMap<String, String> = scope_rows
        .iter()
        .map(|sp| (sp.remote_id.clone(), sp.row.id.clone()))
        .collect();

    let mut ordered_remote = remote.projects.clone();
    canonical_sort(&mut ordered_remote);

    for rp in &ordered_remote {
        let rp_ts = rp.updated_at.max(rp.created_at);
        if deleted.get(&rp.id).is_some_and(|&del_ts| del_ts > rp_ts) {
            continue;
        }
        if let Some(sp) = scope_rows.iter().find(|sp| sp.remote_id == rp.id) {
            remote_to_local_id.insert(rp.id.clone(), sp.row.id.clone());
            if rp_ts > sp.updated_at {
                let mut updated = sp.row.clone();
                updated.name = rp.name.clone();
                updated.shell = rp.shell.clone();
                updated.default_worktree_branch = rp.default_worktree_branch.clone();
                if updated.source_project_id.is_none() {
                    updated.working_dir = rp.working_dir.clone();
                    updated.multi_repos = rp.multi_repos.clone();
                } else {
                    updated.worktree_archived_at = rp.worktree_archived_at;
                }
                db.update_synced_project(&updated, rp_ts);
            }
        } else {
            let local_id = if db.get_project(&rp.id).is_none() {
                rp.id.clone()
            } else {
                uuid_v4()
            };
            let source_project_id = rp
                .source_project_id
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|src_rid| remote_to_local_id.get(src_rid).cloned().unwrap_or_else(|| src_rid.clone()));
            let mut row = ProjectRow {
                id: local_id.clone(),
                name: rp.name.clone(),
                project_type: match scope {
                    SyncScope::Local => "local".to_string(),
                    SyncScope::SshHost(_) => "ssh".to_string(),
                },
                working_dir: rp.working_dir.clone(),
                shell: rp.shell.clone(),
                created_at: rp.created_at,
                source_project_id,
                worktree_branch: rp.worktree_branch.clone(),
                worktree_repo_dir: rp.worktree_repo_dir.clone(),
                worktree_created_by_mojito: rp.worktree_created_by_mojito,
                worktree_archived_at: rp.worktree_archived_at,
                multi_repos: rp.multi_repos.clone(),
                default_worktree_branch: rp.default_worktree_branch.clone(),
                ..Default::default()
            };
            if let SyncScope::SshHost(host) = scope {
                row.host_id = Some(host.id.clone());
                row.ssh_host = Some(host.host.clone());
                row.ssh_port = Some(host.port);
                row.ssh_username = Some(host.username.clone());
                row.ssh_auth_method = Some(host.auth_method.clone());
                row.ssh_key_path = host.key_path.clone();
                row.ssh_secret_enc = host.secret_enc.clone();
            }
            db.clear_deleted_project(&scope_key, &rp.id);
            db.insert_synced_project(&row, &rp.id, rp_ts);
            remote_to_local_id.insert(rp.id.clone(), local_id);
        }
    }

    // 3. 从合并后的本机 DB 生成规范清单
    let final_rows: Vec<SyncProjectRow> =
        db.list_sync_projects().into_iter().filter(|sp| scope.matches(&sp.row)).collect();
    let local_to_remote_id: HashMap<String, String> = final_rows
        .iter()
        .map(|sp| (sp.row.id.clone(), sp.remote_id.clone()))
        .collect();

    let mut projects: Vec<HostProjectEntry> = final_rows
        .into_iter()
        .map(|sp| {
            let r = sp.row;
            let source_project_id = r
                .source_project_id
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|src_id| local_to_remote_id.get(src_id).cloned().unwrap_or_else(|| src_id.clone()));
            HostProjectEntry {
                id: sp.remote_id,
                name: r.name,
                working_dir: r.working_dir,
                shell: r.shell,
                created_at: r.created_at,
                updated_at: sp.updated_at.max(r.created_at),
                source_project_id,
                worktree_branch: r.worktree_branch,
                worktree_repo_dir: r.worktree_repo_dir,
                worktree_created_by_mojito: r.worktree_created_by_mojito,
                worktree_archived_at: r.worktree_archived_at,
                multi_repos: r.multi_repos,
                default_worktree_branch: r.default_worktree_branch,
            }
        })
        .collect();
    canonical_sort(&mut projects);

    for p in &projects {
        deleted.remove(&p.id);
    }

    let next = HostProjectsManifest { version: 1, projects, deleted };
    let mut normalized_remote = remote.clone();
    if normalized_remote.version == 0 {
        normalized_remote.version = 1;
    }
    canonical_sort(&mut normalized_remote.projects);

    (next != normalized_remote).then_some(next)
}

/// 同步本机 `data_dir/host-projects.json` 与 `db` 中的本地项目（`type = "local"`）
pub fn sync_local(db: &Db, data_dir: &Path) {
    let _guard = LOCAL_SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = data_dir.join(HOST_PROJECTS_FILE);
    let remote = match std::fs::read_to_string(&path) {
        Ok(raw) => match parse_manifest(&raw) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("解析本机 {} 失败，跳过同步以免覆盖：{}", path.display(), e);
                return;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HostProjectsManifest::default(),
        Err(e) => {
            log::warn!("读取本机 {} 失败：{}", path.display(), e);
            return;
        }
    };

    let Some(next) = merge_scope(db, SyncScope::Local, &remote) else {
        return;
    };
    if next.projects.is_empty() && next.deleted.is_empty() && !path.exists() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&next) else { return };
    let tmp = data_dir.join(format!("{HOST_PROJECTS_FILE}.tmp.{}", uuid_v4()));
    if let Err(e) = std::fs::write(&tmp, format!("{json}\n")).and_then(|()| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        log::warn!("写入本机 {} 失败：{}", path.display(), e);
    }
}

/// 经 SSH 链路同步远端 `<root>/host-projects.json` 与 `db` 中绑定该 `host` 的 SSH 项目
pub async fn sync_remote(db: &Db, host: &SshHostRow, link: &SshLink, kind: HostKind, root: &str) {
    let read_cmd = remote_read_projects_command(kind, root);
    let Ok(res) = link.exec(&read_cmd, None).await else {
        return;
    };
    if !res.ok() {
        log::debug!("读取远端 {} 的 {} 失败（code={:?}）：{}", host.name, HOST_PROJECTS_FILE, res.code, res.stderr.trim());
        return;
    }
    let remote = match parse_manifest(&res.stdout) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("解析远端 {} 的 {} 失败，跳过同步以免覆盖：{e}", host.name, HOST_PROJECTS_FILE);
            return;
        }
    };

    let Some(next) = merge_scope(db, SyncScope::SshHost(host), &remote) else {
        return;
    };
    if next.projects.is_empty() && next.deleted.is_empty() && res.stdout.trim().is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&next) else { return };
    let payload = format!("{json}\n");
    let (write_cmd, stdin_bytes) = remote_write_projects_command(kind, root, &payload);
    match link.exec_with_input(&write_cmd, &stdin_bytes).await {
        Ok(w) if !w.ok() => {
            log::warn!("写入远端 {} 的 {} 失败（code={:?}）：{}", host.name, HOST_PROJECTS_FILE, w.code, w.stderr.trim());
        }
        Err(e) => {
            log::debug!("写入远端 {} 的 {} 链路中断：{e}", host.name, HOST_PROJECTS_FILE);
        }
        Ok(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn host_row(id: &str, name: &str) -> SshHostRow {
        SshHostRow {
            id: id.into(),
            name: name.into(),
            host: "10.0.0.9".into(),
            port: 22,
            username: "fay".into(),
            auth_method: "agent".into(),
            created_at: 1,
            ..Default::default()
        }
    }

    #[test]
    fn ssh_host_a_to_host_c_to_ssh_host_b_and_local_server_c() {
        // 模拟三台机器各自的 falcon.db，共享主机 C 上的同一个 host-projects.json 目录
        let (_dir_a, db_a) = tmp_db();
        let (_dir_b, db_b) = tmp_db();
        let (dir_c, db_c) = tmp_db();

        let host_c_on_a = host_row("host-c-a", "HostC-from-A");
        let host_c_on_b = host_row("host-c-b", "HostC-from-B");
        db_a.insert_host(&host_c_on_a);
        db_b.insert_host(&host_c_on_b);

        // 1. 主机 A 连接主机 C 并创建 SSH 项目 p1
        db_a.insert_project(&ProjectRow {
            id: "p1".into(),
            name: "Alpha".into(),
            project_type: "ssh".into(),
            working_dir: Some("/home/fay/alpha".into()),
            host_id: Some(host_c_on_a.id.clone()),
            ssh_host: Some(host_c_on_a.host.clone()),
            ssh_port: Some(host_c_on_a.port),
            ssh_username: Some(host_c_on_a.username.clone()),
            ssh_auth_method: Some(host_c_on_a.auth_method.clone()),
            created_at: 100,
            ..Default::default()
        });

        // 主机 A 将项目同步到主机 C 的清单
        let manifest_after_a = merge_scope(&db_a, SyncScope::SshHost(&host_c_on_a), &HostProjectsManifest::default())
            .expect("主机 A 新建项目后应产出清单");
        std::fs::write(
            dir_c.path().join(HOST_PROJECTS_FILE),
            serde_json::to_string_pretty(&manifest_after_a).unwrap(),
        )
        .unwrap();

        // 2. 主机 B 通过 SSH 连接主机 C 后，能够读到主机 A 创建的项目 p1，并绑定到主机 B 自己的 host_id
        let next_b = merge_scope(&db_b, SyncScope::SshHost(&host_c_on_b), &manifest_after_a);
        assert_eq!(next_b, None, "主机 B 只读入且无本地新增时无需回写清单");
        let b_projects = db_b.list_projects();
        assert_eq!(b_projects.len(), 1);
        assert_eq!(b_projects[0].id, "p1");
        assert_eq!(b_projects[0].name, "Alpha");
        assert_eq!(b_projects[0].project_type, "ssh");
        assert_eq!(b_projects[0].host_id.as_deref(), Some("host-c-b"));
        assert_eq!(b_projects[0].working_dir.as_deref(), Some("/home/fay/alpha"));

        // 3. 直接通过 Server 访问主机 C 时，主机 C 的 falcon-server 也能读到 p1（作为本地项目）
        sync_local(&db_c, dir_c.path());
        let c_projects = db_c.list_projects();
        assert_eq!(c_projects.len(), 1);
        assert_eq!(c_projects[0].id, "p1");
        assert_eq!(c_projects[0].name, "Alpha");
        assert_eq!(c_projects[0].project_type, "local");
        assert_eq!(c_projects[0].working_dir.as_deref(), Some("/home/fay/alpha"));

        // 4. 在主机 C 的 Server 上新建本地项目 p2，主机 A 和主机 B 通过 SSH 同步后也能读到 p2
        db_c.insert_project(&ProjectRow {
            id: "p2".into(),
            name: "Beta".into(),
            project_type: "local".into(),
            working_dir: Some("/home/fay/beta".into()),
            created_at: 200,
            ..Default::default()
        });
        sync_local(&db_c, dir_c.path());
        let manifest_from_c: HostProjectsManifest =
            serde_json::from_str(&std::fs::read_to_string(dir_c.path().join(HOST_PROJECTS_FILE)).unwrap()).unwrap();
        assert_eq!(manifest_from_c.projects.len(), 2);

        merge_scope(&db_a, SyncScope::SshHost(&host_c_on_a), &manifest_from_c);
        merge_scope(&db_b, SyncScope::SshHost(&host_c_on_b), &manifest_from_c);
        assert_eq!(db_a.list_projects().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p1", "p2"]);
        assert_eq!(db_b.list_projects().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p1", "p2"]);

        // 5. 在主机 B 上删除 p1，同步后主机 A 与主机 C 的 p1 一并删除
        db_b.delete_project("p1");
        let manifest_after_del = merge_scope(&db_b, SyncScope::SshHost(&host_c_on_b), &manifest_from_c)
            .expect("删除项目后应产出带墓碑的新清单");
        std::fs::write(
            dir_c.path().join(HOST_PROJECTS_FILE),
            serde_json::to_string_pretty(&manifest_after_del).unwrap(),
        )
        .unwrap();
        merge_scope(&db_a, SyncScope::SshHost(&host_c_on_a), &manifest_after_del);
        sync_local(&db_c, dir_c.path());
        assert_eq!(db_a.list_projects().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p2"]);
        assert_eq!(db_c.list_projects().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p2"]);
    }
}
