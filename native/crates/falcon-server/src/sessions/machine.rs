//! 会话 owner 解析。
//!
//! 会话一直都属于某个 owner（`sessions.project_id`）。多数是项目 id。无项目会话的 owner
//! 是合成 id：本机 `standalone:local`，SSH Host `standalone:host:<hostId>`。会话路径上要
//! owner 的 ProjectRow 时一律走 [`owner_row`]：真项目查库，无项目合成一条
//! （本机 = 家目录 + 默认登录 shell；SSH Host = `host_as_project`，id 换成 owner id）。
//!
//! 旧版 AI 助手的对话终端用 `machine:` 前缀（`machine:local` / `machine:host:<id>`）。
//! 功能已经去掉，库里可能还留着这些行：[`is_machine_owner`] 把它们从会话列表里滤掉，
//! 文件接口也不认。`owner_row` 对这种 id 返回 None。

use falcon_proto::{STANDALONE_LOCAL, standalone_host_id};

use crate::db::{Db, ProjectRow};
use crate::sessions::manager::host_as_project;

const MACHINE_PREFIX: &str = "machine:";

/// 这个 project_id 是不是旧版 AI 助手的对话终端。项目 id 是 uuid，撞不上这个前缀
pub fn is_machine_owner(project_id: &str) -> bool {
    project_id.starts_with(MACHINE_PREFIX)
}

/// 本机无项目会话的合成 ProjectRow：工作目录是家目录（rc 文件普遍假设从 home 起步），
/// shell 留空让下游按平台挑默认登录 shell。调用方再盖上自己的 id 与名字
pub fn local_machine_project(home: &str) -> ProjectRow {
    ProjectRow {
        id: String::new(),
        name: String::new(),
        project_type: "local".into(),
        working_dir: Some(home.to_string()).filter(|h| !h.is_empty()),
        ..Default::default()
    }
}

fn local_home() -> String {
    crate::config::home_dir().to_string_lossy().into_owned()
}

/// 会话的 owner → ProjectRow。会话路径（创建、接回、终止、前台探测、断线重连、贴图）上取
/// owner 一律走这里，别直接 `db.get_project`——无项目会话不在 projects 表里。
/// 主机删掉了、或是已去掉的助手对话终端，给 None，与项目被删同一个口径
pub fn owner_row(db: &Db, owner_id: &str) -> Option<ProjectRow> {
    if is_machine_owner(owner_id) {
        return None;
    }
    if owner_id == STANDALONE_LOCAL {
        return Some(ProjectRow {
            id: STANDALONE_LOCAL.into(),
            name: "无项目".into(),
            ..local_machine_project(&local_home())
        });
    }
    if let Some(host_id) = standalone_host_id(owner_id) {
        return db.get_host(host_id).map(|host| ProjectRow {
            id: owner_id.to_string(),
            name: "无项目".into(),
            host_id: Some(host_id.to_string()),
            ..host_as_project(&host)
        });
    }
    db.get_project(owner_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_assistant_owners_are_not_projects() {
        assert!(is_machine_owner("machine:local"));
        assert!(is_machine_owner("machine:host:h1"));
        assert!(!is_machine_owner("0f8c2c1e-1111-4222-8333-444455556666"));
        assert!(!is_machine_owner("host:h1"));
        assert!(!is_machine_owner("standalone:local"));
    }

    #[test]
    fn local_standalone_starts_at_home() {
        let row = local_machine_project("/Users/fay");
        assert_eq!(row.project_type, "local");
        assert_eq!(row.working_dir.as_deref(), Some("/Users/fay"));
        assert_eq!(row.shell, None);
        assert_eq!(local_machine_project("").working_dir, None);
    }
}
