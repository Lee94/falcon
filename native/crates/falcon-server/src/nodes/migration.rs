//! 从旧清单与网关 SSH 项目迁移到宿主机节点。只复制元数据，不操作 shell、会话或目录。
//! worktree 所有权只保留原记录明确携带的值；不探测文件系统来推断删除权限。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context as _;

use crate::db::{Db, SshHostRow, SyncProjectRow};
use crate::host_projects::{HOST_PROJECTS_FILE, HostProjectEntry, HostProjectsManifest, parse_manifest};

/// 首次启动宿主机节点时导入本地旧清单。完成标记与项目写入同一事务；
/// backup API 备份在任何迁移写入之前完成，清单原文与旧数据库记录始终保留。
/// 清单不存在也记录已检查，避免日后旧网关写出的文件重新成为第二个数据来源。
pub fn migrate_local(db: &Db, data_dir: &Path) -> anyhow::Result<()> {
    if db.node_local_projects_migrated()? {
        return Ok(());
    }
    let path = data_dir.join(HOST_PROJECTS_FILE);
    let manifest = match std::fs::read_to_string(&path) {
        Ok(raw) => parse_manifest(&raw).with_context(|| format!("读取项目迁移清单 {} 失败", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HostProjectsManifest::default(),
        Err(e) => return Err(e).with_context(|| format!("读取项目迁移清单 {} 失败", path.display())),
    };
    db.node_migrate_local_projects_once(data_dir, &manifest)
}

/// 为旧的手写 SSH 项目补保存主机。调用顺序为 migrate_local -> bind_legacy_hosts，
/// 错误应阻止启动并告知项目 ID；旧项目/凭据完整保留，不以空列表掩盖不可迁移的记录。
pub fn bind_legacy_hosts(db: &Db) -> anyhow::Result<()> {
    db.node_bind_legacy_hosts()
}

/// 只导出该宿主机的项目属性。id/sourceProjectId 使用宿主 remote_id，
/// SSH 的地址、私钥路径、密文与网关 host_id 在类型上均无法进入导出结果。
/// 需要显式处理数据库读取失败的迁移端点应使用 try_export_host。
pub fn export_host(db: &Db, host: &SshHostRow) -> Vec<HostProjectEntry> {
    export_rows(db.list_sync_projects(), &host.id)
}

/// 与 export_host 相同的格式，但数据库读取失败会阻止迁移端点继续标记成功。
pub fn try_export_host(db: &Db, host: &SshHostRow) -> anyhow::Result<Vec<HostProjectEntry>> {
    let rows = db.node_migration_projects()?;
    let scope: HashMap<_, _> = rows.iter()
        .filter(|p| p.row.project_type == "ssh" && p.row.host_id.as_deref() == Some(host.id.as_str()))
        .map(|p| (p.row.id.as_str(), p)).collect();
    for p in scope.values() {
        if let Some(source) = p.row.source_project_id.as_deref() {
            let root = scope.get(source).with_context(|| format!("项目 {} 的源项目不在同一宿主机：{source}", p.row.id))?;
            anyhow::ensure!(root.row.source_project_id.is_none() && source != p.row.id,
                "项目 {} 的源项目关系无效", p.row.id);
        }
    }
    Ok(export_rows(rows, &host.id))
}

fn export_rows(rows: Vec<SyncProjectRow>, host_id: &str) -> Vec<HostProjectEntry> {
    let rows: Vec<_> = rows.into_iter()
        .filter(|p| p.row.project_type == "ssh" && p.row.host_id.as_deref() == Some(host_id)).collect();
    let local_to_remote: HashMap<_, _> = rows.iter().map(|p| (p.row.id.as_str(), p.remote_id.as_str())).collect();
    rows.iter().map(|p| {
        let r = &p.row;
        HostProjectEntry {
            id: p.remote_id.clone(), name: r.name.clone(), working_dir: r.working_dir.clone(), shell: r.shell.clone(),
            created_at: r.created_at, updated_at: p.updated_at.max(r.created_at),
            source_project_id: r.source_project_id.as_ref()
                .map(|source| local_to_remote.get(source.as_str()).copied().unwrap_or(source).to_string()),
            worktree_branch: r.worktree_branch.clone(), worktree_repo_dir: r.worktree_repo_dir.clone(),
            worktree_created_by_mojito: r.worktree_created_by_mojito, worktree_archived_at: r.worktree_archived_at,
            multi_repos: r.multi_repos.clone(), default_worktree_branch: r.default_worktree_branch.clone(),
            sort_order: p.sort_order,
        }
    }).collect()
}

/// 宿主机节点原子导入。只新增 type=local 项目，既有项目必须元数据一致。
/// 冲突不重命名、不覆盖、不删除；sourceProjectId 按宿主 ID 映射到节点已有的真实主键。
/// 调用者必须在迁移专用端点验证 SSH 可读取的令牌，不能将本方法接到普通项目编辑 API。
pub fn import_projects(db: &Db, entries: Vec<HostProjectEntry>) -> anyhow::Result<()> {
    db.node_import_projects(&HostProjectsManifest { projects: entries, ..Default::default() })
}

/// 只能在节点确认导入成功后调用。旧项目与旧终端保留为缓存，网关据标记过滤/代理；
/// 迁移本身从不调用 Terminate、DELETE 或文件系统清理。
pub fn mark_migrated(db: &Db, host_id: &str, node_id: &str) -> anyhow::Result<()> {
    db.node_mark_host_migrated(host_id, node_id)
}

/// 网关旧项目主键 -> 宿主 remote_id，包含恒等映射，调用方无需猜测哪些 ID 曾被改写。
pub fn host_project_mapping(db: &Db, host_id: &str) -> anyhow::Result<BTreeMap<String, String>> {
    db.node_host_project_mapping(host_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ProjectRow, SessionRow};

    fn tmp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn host(id: &str) -> SshHostRow {
        SshHostRow { id: id.into(), name: "Host B".into(), host: "10.0.0.9".into(), port: 22,
            username: "fay".into(), auth_method: "key".into(), key_path: Some("/private/ssh-key".into()),
            secret_enc: Some("encrypted-secret-never-export".into()), created_at: 1 }
    }

    fn source(id: &str) -> ProjectRow {
        ProjectRow { id: id.into(), name: "Source".into(), project_type: "local".into(),
            working_dir: Some("/home/fay/code/source".into()), shell: Some("/bin/zsh".into()),
            created_at: 100, default_worktree_branch: Some("main".into()), ..Default::default() }
    }

    fn derived(id: &str, source_id: &str) -> ProjectRow {
        ProjectRow { id: id.into(), name: "Feature".into(), working_dir: Some("/home/fay/code/source-feature".into()),
            created_at: 200, source_project_id: Some(source_id.into()), worktree_branch: Some("feature".into()),
            worktree_repo_dir: Some("/home/fay/code/source".into()), worktree_created_by_mojito: Some(1),
            default_worktree_branch: None, ..source(id) }
    }

    fn on_host(mut row: ProjectRow, host: &SshHostRow) -> ProjectRow {
        row.project_type = "ssh".into();
        row.host_id = Some(host.id.clone());
        row.ssh_host = Some(host.host.clone());
        row.ssh_port = Some(host.port);
        row.ssh_username = Some(host.username.clone());
        row.ssh_auth_method = Some(host.auth_method.clone());
        row.ssh_key_path = host.key_path.clone();
        row.ssh_secret_enc = host.secret_enc.clone();
        row
    }

    fn install_gateway(db: &Db, host: &SshHostRow, prefix: &str) {
        db.insert_host(host);
        db.insert_synced_project(&on_host(source(&format!("{prefix}-source")), host), "host-source", 300);
        db.insert_synced_project(&on_host(derived(&format!("{prefix}-feature"), &format!("{prefix}-source")), host), "host-feature", 400);
    }

    fn backup_files(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir).unwrap().map(|p| p.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "bak")
                && p.file_name().unwrap().to_string_lossy().starts_with("falcon.db.before-node-migration.")).collect()
    }

    #[test]
    fn bind_legacy_hosts_reuses_exact_connection_groups_children_and_preserves_all_other_fields() {
        let (dir, db) = tmp_db();
        let saved = host("saved");
        let mut collision = host("collision");
        collision.name = "legacy.example".into();
        collision.host = "different.example".into();
        db.insert_host(&saved);
        db.insert_host(&collision);
        let mut root = on_host(source("root"), &saved);
        root.host_id = None;
        let mut child = on_host(derived("child", "root"), &saved);
        child.host_id = Some(String::new());
        db.insert_synced_project(&root, "host-root", 777);
        db.insert_synced_project(&child, "host-child", 888);
        let mut standalone = on_host(source("new-host-project"), &saved);
        standalone.host_id = None;
        standalone.ssh_host = Some("legacy.example".into());
        standalone.ssh_port = Some(2222);
        standalone.ssh_auth_method = Some("agent".into());
        standalone.ssh_key_path = None;
        standalone.ssh_secret_enc = None;
        db.insert_synced_project(&standalone, "host-other", 999);
        db.insert_project(&source("local"));
        db.set_project_order(&["child".into(), "root".into(), "new-host-project".into(), "local".into()]);
        db.insert_session(&SessionRow { id: "old-shell".into(), project_id: "child".into(), state: "active".into(),
            durable: 1, created_at: 1, last_active_at: 1, ..Default::default() });
        let before = db.node_migration_projects().unwrap();
        let sessions = db.list_sessions();
        bind_legacy_hosts(&db).unwrap();
        let new_host_id = db.get_project("new-host-project").unwrap().host_id.unwrap();
        assert_eq!(db.get_host(&new_host_id).unwrap().name, "legacy.example (2)");
        assert_eq!(db.get_project("root").unwrap().host_id.as_deref(), Some("saved"));
        assert_eq!(db.get_project("child").unwrap().host_id.as_deref(), Some("saved"));
        let mut expected = before;
        for p in &mut expected {
            p.row.host_id = match p.row.id.as_str() {
                "root" | "child" => Some("saved".into()),
                "new-host-project" => Some(new_host_id.clone()),
                _ => p.row.host_id.clone(),
            };
        }
        assert_eq!(db.node_migration_projects().unwrap(), expected);
        assert_eq!(db.list_sessions(), sessions);
        assert_eq!(db.list_host_rows().len(), 3);
        assert_eq!(backup_files(dir.path()).len(), 1);
        let backup = rusqlite::Connection::open(&backup_files(dir.path())[0]).unwrap();
        assert_eq!(backup.query_row("SELECT COUNT(*) FROM ssh_hosts", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(backup.query_row("SELECT host_id FROM projects WHERE id = 'root'", [], |r| r.get::<_, Option<String>>(0)).unwrap(), None);
        bind_legacy_hosts(&db).unwrap();
        assert_eq!(backup_files(dir.path()).len(), 1);
        assert_eq!(db.node_migration_projects().unwrap(), expected);
        let exported = serde_json::to_string(&try_export_host(&db, &saved).unwrap()).unwrap();
        assert!(!exported.contains("encrypted-secret") && !exported.contains("ssh-key"));
    }

    #[test]
    fn legacy_binding_prefers_source_host_alias_and_never_reuses_different_credentials() {
        let (_dir, db) = tmp_db();
        let mut one = host("one");
        one.name = "first".into();
        let mut two = host("two");
        two.name = "second".into();
        db.insert_host(&one);
        db.insert_host(&two);
        db.insert_synced_project(&on_host(source("root"), &two), "remote-root", 1);
        let mut child = on_host(derived("child", "root"), &two);
        child.host_id = None;
        db.insert_synced_project(&child, "remote-child", 2);
        let mut other_credentials = on_host(source("other"), &one);
        other_credentials.host_id = None;
        other_credentials.ssh_secret_enc = Some("different-opaque-ciphertext".into());
        db.insert_synced_project(&other_credentials, "remote-other", 3);
        bind_legacy_hosts(&db).unwrap();
        assert_eq!(db.get_project("child").unwrap().host_id.as_deref(), Some("two"));
        assert_eq!(try_export_host(&db, &two).unwrap().len(), 2);
        let different_id = db.get_project("other").unwrap().host_id.unwrap();
        assert!(different_id != "one" && different_id != "two");
        assert_eq!(db.get_host(&different_id).unwrap().secret_enc, other_credentials.ssh_secret_enc);
    }

    #[test]
    fn invalid_legacy_ssh_fields_abort_the_whole_batch_without_changing_projects_or_credentials() {
        for field in ["host", "username", "port-missing", "port-range", "authMethod", "keyPath"] {
            let (dir, db) = tmp_db();
            let h = host("not-saved");
            let mut valid = on_host(source("valid"), &h);
            valid.host_id = None;
            let mut invalid = on_host(source("invalid"), &h);
            invalid.host_id = None;
            match field {
                "host" => invalid.ssh_host = None,
                "username" => invalid.ssh_username = Some(String::new()),
                "port-missing" => invalid.ssh_port = None,
                "port-range" => invalid.ssh_port = Some(65536),
                "authMethod" => invalid.ssh_auth_method = Some("unsupported".into()),
                "keyPath" => invalid.ssh_key_path = None,
                _ => unreachable!(),
            }
            db.insert_synced_project(&valid, "remote-valid", 12);
            db.insert_synced_project(&invalid, "remote-invalid", 34);
            let before = db.node_migration_projects().unwrap();
            let error = bind_legacy_hosts(&db).unwrap_err().to_string();
            assert!(error.contains("invalid"), "{field}: {error}");
            assert_eq!(db.node_migration_projects().unwrap(), before, "{field}");
            assert!(db.list_host_rows().is_empty(), "{field}");
            assert_eq!(backup_files(dir.path()).len(), 1);
        }
    }

    #[test]
    fn three_databases_local_and_two_ssh_gateways_preserve_ids_and_metadata() {
        let (_a_dir, a) = tmp_db();
        let (b_dir, b) = tmp_db();
        let (_c_dir, c) = tmp_db();
        let a_host = host("b-from-a");
        let c_host = host("b-from-c");
        install_gateway(&a, &a_host, "a");
        install_gateway(&c, &c_host, "c");
        b.insert_synced_project(&source("b-local-source"), "host-source", 123);
        let mut entries = export_host(&a, &a_host);
        entries.reverse(); // 清单里的附属项目先到，不能依赖数组顺序解析关系。
        let manifest = HostProjectsManifest { projects: entries.clone(), ..Default::default() };
        let raw = serde_json::to_string(&manifest).unwrap();
        let path = b_dir.path().join(HOST_PROJECTS_FILE);
        std::fs::write(&path, &raw).unwrap();
        migrate_local(&b, b_dir.path()).unwrap();
        import_projects(&b, export_host(&c, &c_host)).unwrap();
        import_projects(&b, entries).unwrap();
        assert_eq!(b.list_projects().len(), 2);
        assert_eq!(b.local_project_id("host-source").unwrap().as_deref(), Some("b-local-source"));
        let feature = b.get_project("host-feature").unwrap();
        assert_eq!(feature.source_project_id.as_deref(), Some("b-local-source"));
        assert_eq!(feature, derived("host-feature", "b-local-source"));
        let exported = serde_json::to_string(&export_host(&a, &a_host)).unwrap();
        assert!(!exported.contains("ssh") && !exported.contains("encrypted-secret") && !exported.contains("b-from-a"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        assert_eq!(backup_files(b_dir.path()).len(), 1);
        a.insert_session(&SessionRow { id: "old-terminal".into(), project_id: "a-feature".into(), state: "active".into(),
            durable: 1, created_at: 1, last_active_at: 1, ..Default::default() });
        let old_projects = a.list_projects();
        let old_sessions = a.list_sessions();
        mark_migrated(&a, &a_host.id, "node-b").unwrap();
        mark_migrated(&a, &a_host.id, "node-b").unwrap();
        assert!(a.is_host_migrated(&a_host.id).unwrap());
        assert_eq!(a.migrated_project_target("a-feature").unwrap(), Some(("node-b".into(), "host-feature".into())));
        assert_eq!(host_project_mapping(&a, &a_host.id).unwrap()["a-source"], "host-source");
        assert_eq!(a.list_projects(), old_projects);
        assert_eq!(a.list_sessions(), old_sessions);
        assert!(mark_migrated(&a, &a_host.id, "another-node").is_err());
        // 成功后即使旧清单损坏也不再读取。
        std::fs::write(path, "invalid old manifest").unwrap();
        migrate_local(&b, b_dir.path()).unwrap();
        assert_eq!(backup_files(b_dir.path()).len(), 1);
    }

    #[test]
    fn backup_contains_wal_and_precedes_import_and_marker() {
        let (dir, db) = tmp_db();
        db.insert_project(&source("already-local"));
        db.set_setting("backup-evidence", "uncheckpointed");
        let source_conn = rusqlite::Connection::open(crate::db::db_file(dir.path())).unwrap();
        source_conn.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
        source_conn.execute("INSERT INTO settings VALUES ('other-connection-wal', 'present')", []).unwrap();
        assert!(dir.path().join("falcon.db-wal").metadata().unwrap().len() > 0);
        std::fs::write(dir.path().join(HOST_PROJECTS_FILE),
            r#"{"version":1,"projects":[{"id":"from-manifest","name":"Imported","createdAt":1000}]}"#).unwrap();
        migrate_local(&db, dir.path()).unwrap();
        assert!(db.get_project("from-manifest").is_some());
        let backups = backup_files(dir.path());
        assert_eq!(backups.len(), 1);
        let backup = rusqlite::Connection::open(&backups[0]).unwrap();
        assert_eq!(backup.query_row("SELECT value FROM settings WHERE key = 'other-connection-wal'", [], |r| r.get::<_, String>(0)).unwrap(), "present");
        assert_eq!(backup.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(backup.query_row("SELECT COUNT(*) FROM settings WHERE key = 'migration.nodes.local-projects.v1'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(backup.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
    }

    #[test]
    fn conflicting_batch_keeps_all_rows_manifest_and_backup_for_retry() {
        let (dir, db) = tmp_db();
        db.insert_project(&source("occupied"));
        let (_gateway_dir, gateway) = tmp_db();
        let host = host("b");
        install_gateway(&gateway, &host, "a");
        let mut entries = export_host(&gateway, &host);
        let mut conflict = entries[0].clone();
        conflict.id = "occupied".into();
        conflict.name = "conflict".into();
        entries.push(conflict);
        let manifest = HostProjectsManifest { projects: entries.clone(), ..Default::default() };
        let raw = serde_json::to_string(&manifest).unwrap();
        let path = dir.path().join(HOST_PROJECTS_FILE);
        std::fs::write(&path, &raw).unwrap();
        assert!(migrate_local(&db, dir.path()).is_err());
        assert!(import_projects(&db, entries).is_err());
        assert_eq!(db.list_projects(), [source("occupied")]);
        assert!(!db.node_local_projects_migrated().unwrap());
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        assert_eq!(backup_files(dir.path()).len(), 1);
    }

    #[test]
    fn strict_export_rejects_cross_host_source_instead_of_guessing_an_id() {
        let (_dir, db) = tmp_db();
        let one = host("one");
        let two = host("two");
        db.insert_synced_project(&on_host(source("old-source"), &two), "different-host-source", 1);
        db.insert_synced_project(&on_host(derived("child", "old-source"), &one), "child", 2);
        assert!(try_export_host(&db, &one).is_err());
        assert_eq!(db.list_projects().len(), 2);
    }

    #[test]
    fn malformed_manifest_or_failed_backup_never_mark_success() {
        let (dir, db) = tmp_db();
        std::fs::write(dir.path().join(HOST_PROJECTS_FILE), "{broken").unwrap();
        assert!(migrate_local(&db, dir.path()).is_err());
        assert!(!db.node_local_projects_migrated().unwrap());
        assert!(migrate_local(&db, &dir.path().join("missing-data-directory")).is_err());
        assert!(!db.node_local_projects_migrated().unwrap());
        assert!(db.list_projects().is_empty());
    }

    #[test]
    fn ownership_multi_metadata_and_archived_gateway_cache_keep_guards() {
        let (_gateway_dir, gateway) = tmp_db();
        let (_node_dir, node) = tmp_db();
        let h = host("host");
        gateway.insert_host(&h);
        let multi = r#"[{"dir":"/home/fay/code/source-feature/web","repoDir":"/home/fay/code/web"}]"#;
        let root_multi = r#"[{"dir":"/home/fay/code/web"}]"#;
        let mut src = source("container");
        src.multi_repos = Some(root_multi.into());
        gateway.insert_synced_project(&on_host(src, &h), "container", 300);
        let mut child = derived("multi-feature", "container");
        child.worktree_repo_dir = None;
        child.multi_repos = Some(multi.into());
        child.worktree_archived_at = Some(1);
        gateway.insert_synced_project(&on_host(child, &h), "multi-feature", 400);
        import_projects(&node, export_host(&gateway, &h)).unwrap();
        let mut same_metadata = export_host(&gateway, &h);
        for p in &mut same_metadata {
            if let Some(raw) = &p.multi_repos {
                let value: serde_json::Value = serde_json::from_str(raw).unwrap();
                p.multi_repos = Some(serde_json::to_string_pretty(&value).unwrap());
            }
        }
        import_projects(&node, same_metadata).unwrap();
        let imported = node.get_project("multi-feature").unwrap();
        assert_eq!(imported.multi_repos.as_deref(), Some(multi));
        assert_eq!(imported.worktree_created_by_mojito, Some(1));
        node.update_multi_repos("multi-feature", &[falcon_proto::MultiRepoMember { dir: "/tampered".into(), repo_dir: None }]);
        let mut edit = imported.clone();
        edit.worktree_created_by_mojito = Some(0);
        edit.worktree_repo_dir = Some("/tampered".into());
        edit.source_project_id = None;
        edit.multi_repos = None;
        node.update_project(&edit);
        assert_eq!(node.get_project("multi-feature").unwrap(), imported);
        assert!(gateway.list_archived_expired(2).is_empty(), "迁移前的 SSH 缓存也不能由网关清扫");
        assert_eq!(node.list_archived_expired(2).len(), 1, "宿主节点导入后仍按本地项目清扫");
        mark_migrated(&gateway, &h.id, "node").unwrap();
        assert!(gateway.list_archived_expired(2).is_empty());
        assert_eq!(gateway.list_projects().len(), 2);
        // 未声明 Falcon 所有权时，迁移不根据路径或 .git 文件补充所有权。
        let mut entries = export_host(&gateway, &h);
        let child = entries.iter_mut().find(|p| p.id == "multi-feature").unwrap();
        child.id = "unowned".into();
        child.worktree_created_by_mojito = None;
        child.worktree_archived_at = None;
        import_projects(&node, entries).unwrap();
        assert_eq!(node.get_project("unowned").unwrap().worktree_created_by_mojito, None);
    }
}
