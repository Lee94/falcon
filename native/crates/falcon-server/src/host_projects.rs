//! 旧宿主机项目清单（`<falcon 根>/host-projects.json`）的一次性迁移格式。
//!
//! 项目由宿主机节点保存，网关经节点访问；清单不再参与正常请求的双向同步。
//! `merge_import` 只根据值规划新增行，不写数据库、不删除项目、不写回清单。
//! 迁移的备份、事务与完成标记由 nodes::migration 和 Db 的专用方法负责。
//! 清单只含宿主机属性，永不携带 SSH 凭据或网关自己的 host_id。
//!
//! `merge_scope` 及远端命令构造暂留供旧调用清理时参考；迁移不调用旧的双向合并。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::askpass::hub::uuid_v4;
use crate::db::{Db, ProjectRow, SshHostRow, SyncProjectRow};
use crate::git::path::join_path;
use crate::host::{HostKind, encode_powershell, quote_posix, quote_powershell};
use crate::sessions::ssh::SshLink;

pub const HOST_PROJECTS_FILE: &str = "host-projects.json";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
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

pub(crate) fn parse_manifest(raw: &str) -> Result<HostProjectsManifest, serde_json::Error> {
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
                sort_order: sp.sort_order,
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

/// 迁移纯函数：只规划新增的本地项目。同宿主 ID 已存在时元数据必须相同；
/// 绝不采用 last-write-wins 去覆盖 working_dir、worktree 或多仓库删除目标。
/// `remote.deleted` 只阻止清单中已删除项目被首次导入，不删除任何既有数据库记录。
pub fn merge_import(existing: &[SyncProjectRow], remote: &HostProjectsManifest) -> anyhow::Result<Vec<SyncProjectRow>> {
    anyhow::ensure!(remote.version <= 1, "不支持项目清单版本 {}", remote.version);
    let by_id: HashMap<&str, &SyncProjectRow> = existing.iter().map(|p| (p.row.id.as_str(), p)).collect();
    let mut remote_to_local = HashMap::<String, String>::new();
    for p in existing.iter().filter(|p| p.row.project_type == "local") {
        anyhow::ensure!(
            remote_to_local.insert(p.remote_id.clone(), p.row.id.clone()).is_none(),
            "本地项目的宿主 ID 重复：{}", p.remote_id
        );
    }
    let mut entries = BTreeMap::<String, &HostProjectEntry>::new();
    for p in &remote.projects {
        anyhow::ensure!(!p.id.is_empty(), "项目 ID 为空");
        if let Some(previous) = entries.insert(p.id.clone(), p) {
            // 同一批内重复也必须逐字段相同。时间戳不代表项目属性，旧网关的时钟可能不同。
            let mut normalized = previous.clone();
            normalized.updated_at = p.updated_at;
            normalized.sort_order = p.sort_order;
            if same_multi_metadata(normalized.multi_repos.as_deref(), p.multi_repos.as_deref()) {
                normalized.multi_repos = p.multi_repos.clone();
            }
            anyhow::ensure!(normalized == *p, "导入清单有同 ID 的不同项目：{}", p.id);
            if previous.updated_at > p.updated_at {
                entries.insert(p.id.clone(), previous);
            }
        }
    }
    entries.retain(|id, p| !remote.deleted.get(id).is_some_and(|ts| *ts > p.updated_at.max(p.created_at)));
    for p in entries.values() {
        if !remote_to_local.contains_key(&p.id) {
            anyhow::ensure!(!by_id.contains_key(p.id.as_str()), "项目 ID 已被其他项目占用：{}", p.id);
            remote_to_local.insert(p.id.clone(), p.id.clone());
        }
    }
    let mut added = Vec::new();
    for p in entries.values() {
        let id = remote_to_local[&p.id].clone();
        let source_project_id = p.source_project_id.as_ref().map(|source| {
            anyhow::ensure!(!source.is_empty() && source != &p.id, "项目 {} 的源项目无效", p.id);
            remote_to_local.get(source).cloned().ok_or_else(|| anyhow::anyhow!("项目 {} 的源项目不存在：{}", p.id, source))
        }).transpose()?;
        let row = ProjectRow {
            id: id.clone(), name: p.name.clone(), project_type: "local".into(),
            working_dir: p.working_dir.clone(), shell: p.shell.clone(), created_at: p.created_at,
            source_project_id, worktree_branch: p.worktree_branch.clone(),
            worktree_repo_dir: p.worktree_repo_dir.clone(), worktree_created_by_mojito: p.worktree_created_by_mojito,
            worktree_archived_at: p.worktree_archived_at, multi_repos: p.multi_repos.clone(),
            default_worktree_branch: p.default_worktree_branch.clone(), ..Default::default()
        };
        if let Some(stored) = by_id.get(id.as_str()) {
            // updated_at 是各网关的修改时钟，sort_order 是各自列表的位置；相同宿主项目
            // 在 A/C 的这两项可能不同。已有 B 记录的时钟和排序原样保留，新增记录才复制它们。
            let mut comparable = row;
            if same_multi_metadata(stored.row.multi_repos.as_deref(), comparable.multi_repos.as_deref()) {
                comparable.multi_repos = stored.row.multi_repos.clone();
            }
            anyhow::ensure!(stored.row == comparable, "项目 {} 的元数据与本地记录冲突，原记录未修改", p.id);
        } else {
            added.push(SyncProjectRow {
                row, remote_id: p.id.clone(), updated_at: p.updated_at.max(p.created_at), sort_order: p.sort_order,
            });
        }
    }
    // ADR 0002 只允许派生一层；不能把缺失的源项目静默改成普通项目或接到另一台机器上。
    for p in &added {
        if let Some(source) = p.row.source_project_id.as_deref() {
            let src = added.iter().find(|p| p.row.id == source)
                .or_else(|| by_id.get(source).copied())
                .ok_or_else(|| anyhow::anyhow!("项目 {} 的源项目不存在：{source}", p.row.id))?;
            anyhow::ensure!(src.row.project_type == "local" && src.row.source_project_id.is_none(),
                "项目 {} 不能从附属项目派生", p.row.id);
        }
    }
    added.sort_by(|a, b| a.row.source_project_id.is_some().cmp(&b.row.source_project_id.is_some())
        .then_with(|| a.row.created_at.cmp(&b.row.created_at)).then_with(|| a.row.id.cmp(&b.row.id)));
    Ok(added)
}

/// multi_repos 的空白和对象键顺序不改变成员与删除目标；仅比较 JSON 值，绝不改写保存的原文。
/// 损坏 JSON 只有原文完全相同时才算重复，保留既有删除侧 veto 的行为。
fn same_multi_metadata(a: Option<&str>, b: Option<&str>) -> bool {
    if a == b { return true; }
    match (a.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
           b.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// 过渡兼容入口：清单不再参与正常请求。唯一导入入口为 nodes::migration::migrate_local。
/// 保留签名直到旧调用全部移除，避免残留调用重新写文件或传播墓碑。
pub fn sync_local(_db: &Db, _data_dir: &Path) {}

/// 远端项目改由宿主机节点保存；旧调用既不执行 SSH，也不写回清单。
pub async fn sync_remote(_db: &Db, _host: &SshHostRow, _link: &SshLink, _kind: HostKind, _root: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn entry(id: &str, source: Option<&str>) -> HostProjectEntry {
        HostProjectEntry { id: id.into(), name: id.into(), working_dir: Some(format!("/home/fay/{id}")),
            shell: None, created_at: 100, updated_at: 200, source_project_id: source.map(Into::into),
            worktree_branch: source.map(|_| "feature".into()), worktree_repo_dir: source.map(|_| "/home/fay/root".into()),
            worktree_created_by_mojito: source.map(|_| 1), worktree_archived_at: None,
            multi_repos: None, default_worktree_branch: None, sort_order: None }
    }

    #[test]
    fn import_plan_is_pure_and_resolves_sources_before_inserting() {
        let (_dir, db) = tmp_db();
        let manifest = HostProjectsManifest { projects: vec![entry("child", Some("root")), entry("root", None)], ..Default::default() };
        let planned = merge_import(&[], &manifest).unwrap();
        assert_eq!(planned.iter().map(|p| p.row.id.as_str()).collect::<Vec<_>>(), ["root", "child"]);
        assert_eq!(planned[1].row.source_project_id.as_deref(), Some("root"));
        assert!(planned.iter().all(|p| p.row.project_type == "local" && p.row.ssh_secret_enc.is_none()));
        assert!(db.list_projects().is_empty());
        assert!(merge_import(&planned, &manifest).unwrap().is_empty());
        let mut other_gateway = manifest.clone();
        for p in &mut other_gateway.projects {
            p.updated_at += 50_000;
            p.sort_order = Some(42);
        }
        assert!(merge_import(&planned, &other_gateway).unwrap().is_empty(), "网关自己的排序/时钟不能覆盖节点记录");
    }

    #[test]
    fn import_plan_rejects_collision_missing_source_and_second_level_derivation() {
        let root = entry("root", None);
        let manifest = HostProjectsManifest { projects: vec![root.clone()], ..Default::default() };
        let planned = merge_import(&[], &manifest).unwrap();
        let mut conflict = root.clone();
        conflict.working_dir = Some("/another/path".into());
        assert!(merge_import(&planned, &HostProjectsManifest { projects: vec![conflict], ..Default::default() }).is_err());
        let missing = HostProjectsManifest { projects: vec![entry("child", Some("missing"))], ..Default::default() };
        assert!(merge_import(&[], &missing).is_err());
        let nested = HostProjectsManifest { projects: vec![root, entry("child", Some("root")), entry("nested", Some("child"))], ..Default::default() };
        assert!(merge_import(&[], &nested).is_err());
    }

    #[test]
    fn tombstones_do_not_delete_existing_projects_and_compatibility_sync_never_writes() {
        let (dir, db) = tmp_db();
        db.insert_project(&ProjectRow { id: "root".into(), name: "root".into(), project_type: "local".into(), created_at: 100,
            working_dir: Some("/home/fay/root".into()), ..Default::default() });
        let manifest = HostProjectsManifest { projects: vec![entry("root", None), entry("deleted", None)],
            deleted: [("root".into(), 500), ("deleted".into(), 500)].into(), ..Default::default() };
        let raw = serde_json::to_string(&manifest).unwrap();
        let path = dir.path().join(HOST_PROJECTS_FILE);
        std::fs::write(&path, &raw).unwrap();
        let before = db.list_projects();
        assert!(merge_import(&db.list_sync_projects(), &manifest).unwrap().is_empty());
        sync_local(&db, dir.path());
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        assert_eq!(db.list_projects(), before);
    }
}
