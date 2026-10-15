//! 按机器挂的会话：AI 助手里每段对话的终端（docs/design/ai-assistant.md 第 3 节）。
//!
//! 会话一直都属于某个项目（`sessions.project_id`）。助手要的是「某台机器上的一个终端」，不属于
//! 任何项目，也不该出现在侧栏的项目树、会话总览、命令面板里。做法是**沿用 sessions 表与
//! SessionManager 的全部机制，只把 owner 换成合成 id**：
//!
//! - 本机 `machine:local`，SSH Host `machine:host:<hostId>`；
//! - 会话路径上要 owner 的 ProjectRow 时一律走 [`owner_row`]：真项目查库，机器合成一条
//!   （本机 = 家目录 + 默认登录 shell；SSH Host = `host_as_project`，id 换成 owner id）；
//! - 不加列：`machine:` 前缀本身就是标记，`GET /api/sessions` 据此把它们滤掉。
//!
//! SSH Host 的 owner id 故意不用 `host_as_project` 给的 `host:<id>`：那是中转与浏览目录共用的
//! 主机链路的 key（`host_links`），这里的链路是项目链路那一套（`links`，断线重连并接回会话），
//! 两套语义不同，不共用一条连接。

use crate::db::{Db, ProjectRow};
use crate::sessions::manager::host_as_project;

pub const LOCAL_OWNER: &str = "machine:local";
const HOST_OWNER_PREFIX: &str = "machine:host:";
const MACHINE_PREFIX: &str = "machine:";

/// 助手挂的机器：falcon 后端本机，或一台已保存的 SSH Host
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Machine {
    Local,
    Host(String),
}

impl Machine {
    /// 路由参数：`local` 是本机，其余当 hostId
    pub fn from_param(s: &str) -> Machine {
        if s == "local" { Machine::Local } else { Machine::Host(s.to_string()) }
    }

    pub fn owner_id(&self) -> String {
        match self {
            Machine::Local => LOCAL_OWNER.to_string(),
            Machine::Host(id) => format!("{HOST_OWNER_PREFIX}{id}"),
        }
    }

    /// 会话的 project_id → 机器；真项目的 id 给 None
    pub fn from_owner_id(id: &str) -> Option<Machine> {
        if id == LOCAL_OWNER {
            return Some(Machine::Local);
        }
        id.strip_prefix(HOST_OWNER_PREFIX).filter(|h| !h.is_empty()).map(|h| Machine::Host(h.to_string()))
    }
}

/// 这个 project_id 是不是机器（对话终端），不是项目。项目 id 是 uuid，撞不上这个前缀
pub fn is_machine_owner(project_id: &str) -> bool {
    project_id.starts_with(MACHINE_PREFIX)
}

/// 本机的合成 ProjectRow：工作目录是家目录（rc 文件普遍假设从 home 起步），shell 留空让
/// 下游按平台挑默认登录 shell
pub fn local_machine_project(home: &str) -> ProjectRow {
    ProjectRow {
        id: LOCAL_OWNER.into(),
        name: String::new(),
        project_type: "local".into(),
        working_dir: Some(home.to_string()).filter(|h| !h.is_empty()),
        ..Default::default()
    }
}

#[cfg(test)]
thread_local! {
    /// 测试用：本机对话终端的起始目录。对话终端起的是用户的登录 shell，起在真实的家目录里、
    /// 读真实的 rc 文件，测试不该这么干
    pub static TEST_HOME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn local_home() -> String {
    #[cfg(test)]
    if let Some(h) = TEST_HOME.with(|h| h.borrow().clone()) {
        return h;
    }
    crate::config::home_dir().to_string_lossy().into_owned()
}

/// 会话的 owner → ProjectRow。会话路径（创建、接回、终止、前台探测、断线重连、贴图）上取
/// owner 一律走这里，别直接 `db.get_project`——那样机器上的会话会被当成「项目不存在」。
/// 主机删掉了给 None，与项目被删同一个口径
pub fn owner_row(db: &Db, owner_id: &str) -> Option<ProjectRow> {
    match Machine::from_owner_id(owner_id) {
        None => db.get_project(owner_id),
        Some(Machine::Local) => Some(local_machine_project(&local_home())),
        Some(Machine::Host(host_id)) => {
            db.get_host(&host_id).map(|host| ProjectRow { id: owner_id.to_string(), ..host_as_project(&host) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_ids_roundtrip() {
        for m in [Machine::Local, Machine::Host("h1".into())] {
            assert_eq!(Machine::from_owner_id(&m.owner_id()), Some(m.clone()));
            assert!(is_machine_owner(&m.owner_id()));
        }
        assert_eq!(Machine::from_param("local"), Machine::Local);
        assert_eq!(Machine::from_param("abc"), Machine::Host("abc".into()));
    }

    #[test]
    fn project_ids_are_not_machines() {
        assert_eq!(Machine::from_owner_id("0f8c2c1e-1111-4222-8333-444455556666"), None);
        assert!(!is_machine_owner("0f8c2c1e-1111-4222-8333-444455556666"));
        // 中转那条主机链路的 key 不是会话 owner
        assert_eq!(Machine::from_owner_id("host:h1"), None);
        assert_eq!(Machine::from_owner_id("machine:host:"), None);
    }

    #[test]
    fn local_machine_starts_at_home() {
        let row = local_machine_project("/Users/fay");
        assert_eq!(row.id, LOCAL_OWNER);
        assert_eq!(row.project_type, "local");
        assert_eq!(row.working_dir.as_deref(), Some("/Users/fay"));
        assert_eq!(row.shell, None);
        assert_eq!(local_machine_project("").working_dir, None);
    }
}
