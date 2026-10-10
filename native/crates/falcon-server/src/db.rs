//! SQLite 存储。移植自 `packages/server/src/db.ts`。
//!
//! 必须与 Node 版共用同一个库文件（附录 A"数据兼容"）：
//! - 外键约束显式关闭——本仓库从未启用过，级联在应用层手写；
//! - WAL + `synchronous = NORMAL`；
//! - 迁移是幂等的 `CREATE TABLE IF NOT EXISTS` + 按 `PRAGMA table_info` 补列，没有版本号迁移表，
//!   改表结构就往这套里加；
//! - 没有 falcon.db 而有旧的 mojito.db 时接着用旧的。
//!
//! 连接放在一把锁里：Node 版是单线程同步调用，这里各个 handler 并发进来，排队即可
//! （库里是个位数到几十行的配置数据，没有长事务）。语句缓存用 rusqlite 的 `prepare_cached`。

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

use anyhow::Context;
use falcon_proto::{
    DeadReason, MeeglePin, MeeglePinKind, MultiRepoInfo, MultiRepoMember, NonDurableReason, Project, ProjectType,
    Session, SessionAgent, SessionState, SshAuthMethod, SshConfig, SshHost, WorktreeInfo,
};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::auth::SettingsStore;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    /// "local" | "ssh"
    pub project_type: String,
    pub working_dir: Option<String>,
    pub shell: Option<String>,
    pub ssh_host: Option<String>,
    pub ssh_port: Option<i64>,
    pub ssh_username: Option<String>,
    pub ssh_auth_method: Option<String>,
    pub ssh_key_path: Option<String>,
    pub ssh_secret_enc: Option<String>,
    /// 已保存主机；存量项目与手写 ssh 的请求为 None
    pub host_id: Option<String>,
    pub created_at: i64,
    // ---- 附属项目（git worktree）。四列全可空，普通项目一律为 None ----
    pub source_project_id: Option<String>,
    pub worktree_branch: Option<String>,
    /// 派生时记录的仓库根。删除护栏拿它比对，不读 working_dir
    pub worktree_repo_dir: Option<String>,
    /// 1 = falcon 建的目录，删除项目时才允许删它；None / 0 一律不删（列名是 Mojito 时代留下的）
    pub worktree_created_by_mojito: Option<i64>,
    /// 存档时间（unix 毫秒）。Some ⇔ 已存档，到期由后台清扫删除；普通项目恒为 None
    pub worktree_archived_at: Option<i64>,
    /// 多仓库项目的成员清单（JSON 的 MultiRepoMember[]）。Some ⇔ 多仓库项目；
    /// 容器与派生产物共用这一列，语义由 source_project_id 判别（见 shared 的注释）。
    /// 派生行上它是删除目标清单——写入路径与 worktree 四列同样必须不可达。
    pub multi_repos: Option<String>,
    /// 源项目的派生基点。空 = 用当前 HEAD。不是删除护栏的判据，所以走 update_project。
    /// 附属项目恒为 None（不能再派生）。
    pub default_worktree_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRow {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub state: String,
    pub durable: i64,
    pub dead_reason: Option<String>,
    pub non_durable_reason: Option<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    /// 上次 Viewer 量到的格子；None = 从未量过，接回时不能当 80×24 用
    pub cols: Option<i64>,
    pub rows: Option<i64>,
    /// 开场跑的 CLI（claude / codex / grok）；None = 普通 shell
    pub agent: Option<String>,
    /// 1 = 建会话时滚动位置插件已就位，会话用 scroll.kdl 那套配置（ADR 0019）；
    /// 0 / None（升级前建的）= 老配置。接回时必须照建会话时的那套来：拿新配置去接回
    /// 跑在老 zellij 上的会话会画出整圈边框（见 zellij::command::scroll_config_body）。
    pub scroll_plugin: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshHostRow {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub auth_method: String,
    pub key_path: Option<String>,
    pub secret_enc: Option<String>,
    pub created_at: i64,
}

/// 端口转发规则，挂在已保存的 SSH Host 上（不是项目）
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostForwardRow {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub kind: String,
    pub bind_host: String,
    pub bind_port: i64,
    pub dest_host: String,
    pub dest_port: i64,
    pub enabled: i64,
    pub created_at: i64,
}

/// 公网发布规则，挂在本机或已保存的 SSH Host 上（不是项目）
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostShareRow {
    pub id: String,
    /// None = falcon 后端本机
    pub host_id: Option<String>,
    pub name: Option<String>,
    pub dest_host: String,
    pub dest_port: i64,
    pub enabled: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeeglePinRow {
    pub id: String,
    pub kind: String,
    pub space_key: String,
    pub space_name: Option<String>,
    pub target_id: String,
    pub type_key: Option<String>,
    pub label: String,
    pub url: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ZellijHostRow {
    pub host: String,
    pub port: i64,
    pub username: String,
    /// None = 未询问过；1 = 已授权在该主机安装；0 = 已拒绝
    pub authorized: Option<i64>,
    pub installed_version: Option<String>,
    /// 该主机的下载源覆盖；None 表示用官方地址
    pub base_url: Option<String>,
    /// Windows 远端的真实断线验证结果；None = 未验证
    pub verified_durable: Option<i64>,
    pub updated_at: i64,
}

/// `upsert_zellij_host` 的部分更新：外层 None = 不动这一列，`Some(None)` = 清空。
///
/// 判据是"字段给没给"而不是"值是不是空"：显式清空（作废安装记录、作废持久性判定）
/// 用合并语义会被旧值顶回去、清空静默失效——这正是 Node 版曾经的 bug。
#[derive(Debug, Clone, Default)]
pub struct ZellijHostPatch {
    pub authorized: Option<Option<i64>>,
    pub installed_version: Option<Option<String>>,
    pub base_url: Option<Option<String>>,
    pub verified_durable: Option<Option<i64>>,
}

fn project_from_row(r: &Row) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get("id")?,
        name: r.get("name")?,
        project_type: r.get("type")?,
        working_dir: r.get("working_dir")?,
        shell: r.get("shell")?,
        ssh_host: r.get("ssh_host")?,
        ssh_port: r.get("ssh_port")?,
        ssh_username: r.get("ssh_username")?,
        ssh_auth_method: r.get("ssh_auth_method")?,
        ssh_key_path: r.get("ssh_key_path")?,
        ssh_secret_enc: r.get("ssh_secret_enc")?,
        host_id: r.get("host_id")?,
        created_at: r.get("created_at")?,
        source_project_id: r.get("source_project_id")?,
        worktree_branch: r.get("worktree_branch")?,
        worktree_repo_dir: r.get("worktree_repo_dir")?,
        worktree_created_by_mojito: r.get("worktree_created_by_mojito")?,
        worktree_archived_at: r.get("worktree_archived_at")?,
        multi_repos: r.get("multi_repos")?,
        default_worktree_branch: r.get("default_worktree_branch")?,
    })
}

fn session_from_row(r: &Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        name: r.get("name")?,
        state: r.get("state")?,
        durable: r.get("durable")?,
        dead_reason: r.get("dead_reason")?,
        non_durable_reason: r.get("non_durable_reason")?,
        created_at: r.get("created_at")?,
        last_active_at: r.get("last_active_at")?,
        cols: r.get("cols")?,
        rows: r.get("rows")?,
        agent: r.get("agent")?,
        scroll_plugin: r.get("scroll_plugin")?,
    })
}

fn host_from_row(r: &Row) -> rusqlite::Result<SshHostRow> {
    Ok(SshHostRow {
        id: r.get("id")?,
        name: r.get("name")?,
        host: r.get("host")?,
        port: r.get("port")?,
        username: r.get("username")?,
        auth_method: r.get("auth_method")?,
        key_path: r.get("key_path")?,
        secret_enc: r.get("secret_enc")?,
        created_at: r.get("created_at")?,
    })
}

fn forward_from_row(r: &Row) -> rusqlite::Result<HostForwardRow> {
    Ok(HostForwardRow {
        id: r.get("id")?,
        host_id: r.get("host_id")?,
        name: r.get("name")?,
        kind: r.get("kind")?,
        bind_host: r.get("bind_host")?,
        bind_port: r.get("bind_port")?,
        dest_host: r.get("dest_host")?,
        dest_port: r.get("dest_port")?,
        enabled: r.get("enabled")?,
        created_at: r.get("created_at")?,
    })
}

fn share_from_row(r: &Row) -> rusqlite::Result<HostShareRow> {
    Ok(HostShareRow {
        id: r.get("id")?,
        host_id: r.get("host_id")?,
        name: r.get("name")?,
        dest_host: r.get("dest_host")?,
        dest_port: r.get("dest_port")?,
        enabled: r.get("enabled")?,
        created_at: r.get("created_at")?,
    })
}

fn pin_from_row(r: &Row) -> rusqlite::Result<MeeglePinRow> {
    Ok(MeeglePinRow {
        id: r.get("id")?,
        kind: r.get("kind")?,
        space_key: r.get("space_key")?,
        space_name: r.get("space_name")?,
        target_id: r.get("target_id")?,
        type_key: r.get("type_key")?,
        label: r.get("label")?,
        url: r.get("url")?,
        created_at: r.get("created_at")?,
    })
}

fn zellij_host_from_row(r: &Row) -> rusqlite::Result<ZellijHostRow> {
    Ok(ZellijHostRow {
        host: r.get("host")?,
        port: r.get("port")?,
        username: r.get("username")?,
        authorized: r.get("authorized")?,
        installed_version: r.get("installed_version")?,
        base_url: r.get("base_url")?,
        verified_durable: r.get("verified_durable")?,
        updated_at: r.get("updated_at")?,
    })
}

/// 新库名 falcon.db；旧安装只有 mojito.db 时接着用，避免改名后打开空库。
pub fn db_file(data_dir: &Path) -> PathBuf {
    let next = data_dir.join("falcon.db");
    let prev = data_dir.join("mojito.db");
    if !next.exists() && prev.exists() {
        return prev;
    }
    next
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let path = db_file(data_dir);
        let conn = Connection::open(&path).with_context(|| format!("打开 {} 失败", path.display()))?;
        Self::with_connection(conn)
    }

    pub fn with_connection(conn: Connection) -> anyhow::Result<Self> {
        // 外键约束从未启用过（级联在应用层手写），显式关掉保持语义不变
        conn.execute_batch("PRAGMA foreign_keys = OFF")?;
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        // WAL 下默认仍是 synchronous=FULL（每条隐式事务一次 fsync）。NORMAL 在 WAL 下
        // 不会损坏数据库，最坏掉电丢最后几笔——对本库存的数据完全可接受。
        conn.execute_batch("PRAGMA synchronous = NORMAL")?;
        let db = Db { conn: Mutex::new(conn) };
        db.migrate()?;
        Ok(db)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn migrate(&self) -> anyhow::Result<()> {
        {
            let conn = self.lock();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS projects (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    type TEXT NOT NULL,
                    working_dir TEXT,
                    shell TEXT,
                    ssh_host TEXT,
                    ssh_port INTEGER,
                    ssh_username TEXT,
                    ssh_auth_method TEXT,
                    ssh_key_path TEXT,
                    ssh_secret_enc TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES projects(id),
                    name TEXT NOT NULL,
                    state TEXT NOT NULL,
                    durable INTEGER NOT NULL,
                    dead_reason TEXT,
                    created_at INTEGER NOT NULL,
                    last_active_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS known_hosts (
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    fingerprint TEXT NOT NULL,
                    PRIMARY KEY (host, port)
                );
                CREATE TABLE IF NOT EXISTS ssh_hosts (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    username TEXT NOT NULL,
                    auth_method TEXT NOT NULL,
                    key_path TEXT,
                    secret_enc TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS zellij_hosts (
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    username TEXT NOT NULL,
                    authorized INTEGER,
                    installed_version TEXT,
                    base_url TEXT,
                    verified_durable INTEGER,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (host, port, username)
                );",
            )?;
            add_column(&conn, "sessions", "non_durable_reason", "TEXT")?;
            add_column(&conn, "sessions", "cols", "INTEGER")?;
            add_column(&conn, "sessions", "rows", "INTEGER")?;
            add_column(&conn, "sessions", "agent", "TEXT")?;
            add_column(&conn, "sessions", "scroll_plugin", "INTEGER")?;
            // v1 用 tmux，接不回来的会话原因是 tmux-gone；改用 Zellij 后统一为 session-gone
            conn.execute("UPDATE sessions SET dead_reason = 'session-gone' WHERE dead_reason = 'tmux-gone'", [])?;
        }
        self.clear_auto_names()?;
        {
            let conn = self.lock();
            // 附属项目（git worktree）。四列全可空，存量行天然是"普通项目"。
            //
            // 不写 REFERENCES projects(id)：本仓库外键从未开启，写了不生效，只会给人"有约束"
            // 的错觉；级联与 sessions 一样在应用层手写（见路由的 DELETE）。
            // 也不建索引：项目数量是个位数到几十，全表扫比多维护一处 schema 便宜。
            add_column(&conn, "projects", "source_project_id", "TEXT")?;
            add_column(&conn, "projects", "worktree_branch", "TEXT")?;
            add_column(&conn, "projects", "worktree_repo_dir", "TEXT")?;
            add_column(&conn, "projects", "worktree_created_by_mojito", "INTEGER")?;
            add_column(&conn, "projects", "worktree_archived_at", "INTEGER")?;
            add_column(&conn, "projects", "host_id", "TEXT")?;
            // 多仓库项目的成员清单（JSON）。容器与派生产物共用，语义由 source_project_id 判别
            add_column(&conn, "projects", "multi_repos", "TEXT")?;
            // 源项目的派生基点。空 = HEAD。不是 worktree 四列那种删除护栏，用户可改。
            add_column(&conn, "projects", "default_worktree_branch", "TEXT")?;

            // 中转（端口转发 + 公网发布）挂在机器上，不挂项目（ADR 0016）：
            // host_forwards.host_id 是已保存的 SSH Host，host_shares.host_id 为 null 表示后端本机。
            // 不写 REFERENCES：外键从未开启，删主机时的级联在应用层手写。
            // 公网 URL 是运行时事实，不入库（ADR 0014）。
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS host_forwards (
                    id TEXT PRIMARY KEY,
                    host_id TEXT NOT NULL,
                    name TEXT,
                    kind TEXT NOT NULL,
                    bind_host TEXT NOT NULL,
                    bind_port INTEGER NOT NULL,
                    dest_host TEXT NOT NULL,
                    dest_port INTEGER NOT NULL,
                    enabled INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS host_shares (
                    id TEXT PRIMARY KEY,
                    host_id TEXT,
                    name TEXT,
                    dest_host TEXT NOT NULL,
                    dest_port INTEGER NOT NULL,
                    enabled INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                );",
            )?;
        }
        self.migrate_relays_to_hosts()?;
        // 飞书项目面板的固定列表（ADR 0010）。不挂在项目上：CLI 的登录态是整台机器一份
        self.lock().execute_batch(
            "CREATE TABLE IF NOT EXISTS meegle_pins (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                space_key TEXT NOT NULL,
                space_name TEXT,
                target_id TEXT NOT NULL,
                type_key TEXT,
                label TEXT NOT NULL,
                url TEXT,
                created_at INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    /// 中转从「挂项目」改成「挂机器」（ADR 0016）：把旧表 ssh_forwards / public_shares
    /// 并进 host_forwards / host_shares，然后删掉旧表。
    ///
    /// 靠旧表在不在决定跑不跑，不用 settings 标记：老版本二进制若又建出旧表、加了规则，
    /// 下次启动照样并过来再删掉，不会丢。id 原样沿用，重复跑也只会 INSERT OR IGNORE。
    /// 落不到机器上的旧规则（项目没绑主机、也找不到连接三元组一样的已保存主机）无处
    /// 可挂，只能丢弃并打一行日志。
    ///
    fn migrate_relays_to_hosts(&self) -> anyhow::Result<()> {
        use crate::sessions::relay_spec::{
            ForwardSlotRow, HostConn, LegacyProjectConn, ShareSlotRow, excess_enabled, forward_slot,
            legacy_forward_host, share_slot,
        };
        let mut conn = self.lock();
        let exists = |c: &Connection, table: &str| -> rusqlite::Result<bool> {
            c.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", [table], |_| Ok(()))
                .optional()
                .map(|r| r.is_some())
        };
        let has_forwards = exists(&conn, "ssh_forwards")?;
        let has_shares = exists(&conn, "public_shares")?;
        if !has_forwards && !has_shares {
            return Ok(());
        }

        let hosts: Vec<HostConn> = conn
            .prepare("SELECT id, host, port, username FROM ssh_hosts")?
            .query_map([], |r| Ok(HostConn { id: r.get(0)?, host: r.get(1)?, port: r.get(2)?, username: r.get(3)? }))?
            .collect::<Result<_, _>>()?;
        let host_of = |c: &Connection, project_id: &str| -> rusqlite::Result<Option<String>> {
            let conn_row = c
                .query_row(
                    "SELECT host_id, ssh_host, ssh_port, ssh_username FROM projects WHERE id = ?",
                    [project_id],
                    |r| {
                        Ok(LegacyProjectConn {
                            host_id: r.get(0)?,
                            ssh_host: r.get(1)?,
                            ssh_port: r.get(2)?,
                            ssh_username: r.get(3)?,
                        })
                    },
                )
                .optional()?;
            Ok(conn_row.and_then(|p| legacy_forward_host(&p, &hosts)))
        };
        let mut dropped: Vec<String> = Vec::new();

        let tx = conn.transaction()?;
        if has_forwards {
            type Legacy = (String, String, Option<String>, String, String, i64, String, i64, i64, i64);
            let rows: Vec<Legacy> = tx
                .prepare(
                    "SELECT id, project_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at FROM ssh_forwards",
                )?
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
                })?
                .collect::<Result<_, _>>()?;
            for (id, project_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at) in rows {
                let Some(host_id) = host_of(&tx, &project_id)? else {
                    dropped.push(format!("转发 {kind} {bind_port}→{dest_port}"));
                    continue;
                };
                tx.execute(
                    "INSERT OR IGNORE INTO host_forwards
                       (id, host_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![id, host_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at],
                )?;
            }
            tx.execute_batch("DROP TABLE ssh_forwards")?;
        }
        if has_shares {
            type Legacy = (String, String, Option<String>, String, String, i64, i64, i64);
            let rows: Vec<Legacy> = tx
                .prepare("SELECT id, project_id, name, origin, dest_host, dest_port, enabled, created_at FROM public_shares")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
                .collect::<Result<_, _>>()?;
            for (id, project_id, name, origin, dest_host, dest_port, enabled, created_at) in rows {
                // origin=local 本来就是后端本机，与项目是本地还是 SSH 无关
                let host_id = if origin == "remote" { host_of(&tx, &project_id)? } else { None };
                if origin == "remote" && host_id.is_none() {
                    dropped.push(format!("公网发布 {dest_port}"));
                    continue;
                }
                tx.execute(
                    "INSERT OR IGNORE INTO host_shares
                       (id, host_id, name, dest_host, dest_port, enabled, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![id, host_id, name, dest_host, dest_port, enabled, created_at],
                )?;
            }
            tx.execute_batch("DROP TABLE public_shares")?;
        }
        // 几个项目的规则并到一台机器上，可能撞出同端口的两条 enabled：留最早的那条
        let forwards: Vec<ForwardSlotRow> = tx
            .prepare("SELECT id, host_id, kind, bind_port, enabled, created_at FROM host_forwards")?
            .query_map([], |r| {
                Ok(ForwardSlotRow {
                    id: r.get(0)?,
                    host_id: r.get(1)?,
                    kind: r.get(2)?,
                    bind_port: r.get(3)?,
                    enabled: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        for id in excess_enabled(&forwards, forward_slot) {
            tx.execute("UPDATE host_forwards SET enabled = 0 WHERE id = ?", [id])?;
        }
        let shares: Vec<ShareSlotRow> = tx
            .prepare("SELECT id, host_id, dest_port, enabled, created_at FROM host_shares")?
            .query_map([], |r| {
                Ok(ShareSlotRow { id: r.get(0)?, host_id: r.get(1)?, dest_port: r.get(2)?, enabled: r.get(3)?, created_at: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
        for id in excess_enabled(&shares, share_slot) {
            tx.execute("UPDATE host_shares SET enabled = 0 WHERE id = ?", [id])?;
        }
        tx.commit()?;
        if !dropped.is_empty() {
            log::warn!("中转迁移：{} 条旧规则找不到所属主机，已丢弃：{}", dropped.len(), dropped.join("，"));
        }
        Ok(())
    }

    /// 存量会话的自动名（`Terminal 3` / `Claude 1`）一次性清空——现在没起名就是空串，
    /// UI 走自动标题。
    ///
    /// 只跑一次，用 settings 里的标记记着：这条清理认不出"用户手动起的名字恰好
    /// 长这样"，每次启动都跑会把人家改回去的名字又抹掉。没有版本号迁移表（见 migrate），
    /// 标记就是最轻的一次性开关。
    fn clear_auto_names(&self) -> anyhow::Result<()> {
        const KEY: &str = "migration.sessions.clearAutoNames";
        if self.get_setting(KEY).is_some() {
            return Ok(());
        }
        // 在代码里按正则筛，不在 SQL 里拼 GLOB：会话数量是个位数到几十，精确匹配
        // 比省几次查询值钱（`Terminal 2 号` 这种手起的名字不该被误伤）
        static AUTO: LazyLock<Regex> = LazyLock::new(|| Regex::new("^(?:Terminal|Claude|Codex|Grok) [0-9]+$").unwrap());
        let rows: Vec<(String, String)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare_cached("SELECT id, name FROM sessions")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        for (id, name) in rows {
            if AUTO.is_match(&name) {
                self.rename_session(&id, "");
            }
        }
        self.set_setting(KEY, "1");
        Ok(())
    }

    // ---- settings ----

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.lock();
        conn.prepare_cached("SELECT value FROM settings WHERE key = ?")
            .and_then(|mut s| s.query_row([key], |r| r.get(0)).optional())
            .unwrap_or_else(|e| {
                log::error!("读 settings.{key} 失败：{e}");
                None
            })
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        self.exec(
            "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    /// 写语句。失败只能是磁盘 / 库损坏这类环境问题，Node 版会把异常一路抛成 500；
    /// 这里记日志并照样往下走，调用方的后续读会如实反映库里的状态
    fn exec(&self, sql: &str, p: impl rusqlite::Params) {
        let conn = self.lock();
        if let Err(e) = conn.prepare_cached(sql).and_then(|mut s| s.execute(p)) {
            log::error!("SQL 执行失败：{e}（{sql}）");
        }
    }

    fn query_all<T>(&self, sql: &str, p: impl rusqlite::Params, f: fn(&Row) -> rusqlite::Result<T>) -> Vec<T> {
        let conn = self.lock();
        let result = conn
            .prepare_cached(sql)
            .and_then(|mut s| s.query_map(p, f).and_then(|rows| rows.collect::<Result<Vec<_>, _>>()));
        result.unwrap_or_else(|e| {
            log::error!("SQL 查询失败：{e}（{sql}）");
            Vec::new()
        })
    }

    fn query_one<T>(&self, sql: &str, p: impl rusqlite::Params, f: fn(&Row) -> rusqlite::Result<T>) -> Option<T> {
        let conn = self.lock();
        let result = conn.prepare_cached(sql).and_then(|mut s| s.query_row(p, f).optional());
        result.unwrap_or_else(|e| {
            log::error!("SQL 查询失败：{e}（{sql}）");
            None
        })
    }

    // ---- projects ----

    /// 解析 multi_repos。损坏的 JSON / 形状不对一律返回 None：to_project 侧降级成
    /// "看起来是普通项目"（普通项目永不删目录，方向安全）；删除侧拿到 None 直接
    /// veto（见 git::remove 的 veto_multi_removal），最坏是拒删并告警，不会删错。
    pub fn parse_multi_repos(multi_repos: Option<&str>) -> Option<Vec<MultiRepoMember>> {
        let parsed: serde_json::Value = serde_json::from_str(multi_repos?).ok()?;
        let items = parsed.as_array()?;
        let mut members = Vec::with_capacity(items.len());
        for item in items {
            let obj = item.as_object()?;
            let dir = obj.get("dir")?.as_str().filter(|d| !d.is_empty())?;
            let repo_dir = match obj.get("repoDir") {
                None => None,
                Some(v) => Some(v.as_str()?.to_string()),
            };
            members.push(MultiRepoMember { dir: dir.to_string(), repo_dir });
        }
        Some(members)
    }

    pub fn to_project(row: &ProjectRow) -> Project {
        let members = Self::parse_multi_repos(row.multi_repos.as_deref());
        let ssh_type = row.project_type == "ssh";
        Project {
            id: row.id.clone(),
            name: row.name.clone(),
            project_type: if ssh_type { ProjectType::Ssh } else { ProjectType::Local },
            working_dir: row.working_dir.clone(),
            shell: row.shell.clone(),
            ssh: ssh_type.then(|| SshConfig {
                host: row.ssh_host.clone().unwrap_or_default(),
                port: row.ssh_port.unwrap_or(22) as u16,
                username: row.ssh_username.clone().unwrap_or_default(),
                auth_method: row
                    .ssh_auth_method
                    .as_deref()
                    .and_then(SshAuthMethod::from_wire)
                    .unwrap_or(SshAuthMethod::Agent),
                key_path: row.ssh_key_path.clone(),
                has_secret: row.ssh_secret_enc.is_some(),
            }),
            host_id: row.host_id.clone(),
            worktree: row.source_project_id.as_ref().filter(|s| !s.is_empty()).map(|src| WorktreeInfo {
                source_project_id: src.clone(),
                branch: row.worktree_branch.clone().unwrap_or_default(),
                repo_dir: row.worktree_repo_dir.clone().unwrap_or_default(),
                created_by_falcon: row.worktree_created_by_mojito == Some(1),
                archived_at: row.worktree_archived_at,
            }),
            multi: members.map(|repos| MultiRepoInfo { repos }),
            default_worktree_branch: row.default_worktree_branch.clone(),
            created_at: row.created_at,
        }
    }

    pub fn list_projects(&self) -> Vec<ProjectRow> {
        self.query_all("SELECT * FROM projects ORDER BY created_at ASC", [], project_from_row)
    }

    pub fn get_project(&self, id: &str) -> Option<ProjectRow> {
        self.query_one("SELECT * FROM projects WHERE id = ?", [id], project_from_row)
    }

    pub fn insert_project(&self, row: &ProjectRow) {
        self.exec(
            "INSERT INTO projects (id, name, type, working_dir, shell, ssh_host, ssh_port, ssh_username, ssh_auth_method, ssh_key_path, ssh_secret_enc, host_id, created_at,
               source_project_id, worktree_branch, worktree_repo_dir, worktree_created_by_mojito, worktree_archived_at, multi_repos, default_worktree_branch)
             VALUES (:id, :name, :type, :working_dir, :shell, :ssh_host, :ssh_port, :ssh_username, :ssh_auth_method, :ssh_key_path, :ssh_secret_enc, :host_id, :created_at,
               :source_project_id, :worktree_branch, :worktree_repo_dir, :worktree_created_by_mojito, :worktree_archived_at, :multi_repos, :default_worktree_branch)",
            named_params! {
                ":id": row.id, ":name": row.name, ":type": row.project_type, ":working_dir": row.working_dir,
                ":shell": row.shell, ":ssh_host": row.ssh_host, ":ssh_port": row.ssh_port,
                ":ssh_username": row.ssh_username, ":ssh_auth_method": row.ssh_auth_method,
                ":ssh_key_path": row.ssh_key_path, ":ssh_secret_enc": row.ssh_secret_enc,
                ":host_id": row.host_id, ":created_at": row.created_at,
                ":source_project_id": row.source_project_id, ":worktree_branch": row.worktree_branch,
                ":worktree_repo_dir": row.worktree_repo_dir,
                ":worktree_created_by_mojito": row.worktree_created_by_mojito,
                ":worktree_archived_at": row.worktree_archived_at, ":multi_repos": row.multi_repos,
                ":default_worktree_branch": row.default_worktree_branch,
            },
        );
    }

    /// 刻意不更新 source_project_id / worktree_* 四列，也不更新 multi_repos。
    ///
    /// 它们是删除护栏的判据（"这个目录是 falcon 建的、属于那个仓库"）。一旦能从
    /// PUT /api/projects/:id 改写，护栏就等于不存在——攻击面是"改一行 JSON 让 falcon
    /// 去 rm -rf 任意路径"。附属项目的这些属性在创建时定死，此后只读；
    /// ProjectInput 里也没有对应字段，所以这条从类型层面就够不到。
    /// multi_repos 在派生行上是删除目标清单，同罪；容器改成员走 update_multi_repos。
    /// default_worktree_branch 不在此列：它只是派生时的默认基点，改了改不了任何路径。
    pub fn update_project(&self, row: &ProjectRow) {
        self.exec(
            "UPDATE projects SET name=:name, working_dir=:working_dir, shell=:shell, ssh_host=:ssh_host, ssh_port=:ssh_port,
             ssh_username=:ssh_username, ssh_auth_method=:ssh_auth_method, ssh_key_path=:ssh_key_path, ssh_secret_enc=:ssh_secret_enc,
             host_id=:host_id, default_worktree_branch=:default_worktree_branch
             WHERE id=:id",
            named_params! {
                ":id": row.id, ":name": row.name, ":working_dir": row.working_dir, ":shell": row.shell,
                ":ssh_host": row.ssh_host, ":ssh_port": row.ssh_port, ":ssh_username": row.ssh_username,
                ":ssh_auth_method": row.ssh_auth_method, ":ssh_key_path": row.ssh_key_path,
                ":ssh_secret_enc": row.ssh_secret_enc, ":host_id": row.host_id,
                ":default_worktree_branch": row.default_worktree_branch,
            },
        );
    }

    /// 替换多仓库**容器**的成员清单。
    ///
    /// SQL 里的 `source_project_id IS NULL` 不是防御式编程，是护栏本体：派生行的
    /// multi_repos 是删除目标清单，写入路径必须不可达——即使路由层将来写错，
    /// 这条 UPDATE 也够不到派生行（与 update_project 刻意不更新 worktree 四列同理）。
    pub fn update_multi_repos(&self, id: &str, repos: &[MultiRepoMember]) {
        let json = serde_json::to_string(repos).expect("成员清单总能序列化");
        self.exec("UPDATE projects SET multi_repos = ? WHERE id = ? AND source_project_id IS NULL", params![json, id]);
    }

    /// 一行项目贡献给"别删到我"清单（other_dirs）的全部路径：working_dir、
    /// 容器成员的仓库路径、派生产物成员的 worktree 路径与仓库根。
    /// 容器的成员是用户的真仓库，必须能挡住别的删除踩上去。
    /// worktree_repo_dir 维持现状不收——单仓库附属项目的仓库根本来就没进过这个清单，
    /// 这里只为新增的多仓库形态补齐，不悄悄改既有语义。
    pub fn guard_dirs_of(row: &ProjectRow) -> Vec<String> {
        let mut dirs = Vec::new();
        if let Some(wd) = row.working_dir.as_ref().filter(|w| !w.is_empty()) {
            dirs.push(wd.clone());
        }
        for m in Self::parse_multi_repos(row.multi_repos.as_deref()).unwrap_or_default() {
            dirs.push(m.dir);
            if let Some(r) = m.repo_dir.filter(|r| !r.is_empty()) {
                dirs.push(r);
            }
        }
        dirs
    }

    /// 存档 / 恢复附属项目。ts=None 即恢复。
    /// 只动这一列：worktree 四列仍是删除护栏的只读判据（见 update_project 的注释），
    /// 存档时间不参与任何路径判断，可写不构成攻击面。
    pub fn set_worktree_archived(&self, id: &str, ts: Option<i64>) {
        self.exec("UPDATE projects SET worktree_archived_at = ? WHERE id = ?", params![ts, id]);
    }

    /// 存档已到期（archived_at ≤ archived_before）的附属项目，供后台清扫
    pub fn list_archived_expired(&self, archived_before: i64) -> Vec<ProjectRow> {
        self.query_all(
            "SELECT * FROM projects
             WHERE source_project_id IS NOT NULL AND worktree_archived_at IS NOT NULL
               AND worktree_archived_at <= ?
             ORDER BY worktree_archived_at ASC",
            [archived_before],
            project_from_row,
        )
    }

    /// 某源项目的全部附属项目。删除级联与确认框都要用。
    pub fn list_worktree_children(&self, source_id: &str) -> Vec<ProjectRow> {
        self.query_all(
            "SELECT * FROM projects WHERE source_project_id = ? ORDER BY created_at ASC",
            [source_id],
            project_from_row,
        )
    }

    /// 源项目改了 SSH 配置后，把五列刷到它的全部附属项目。
    ///
    /// 附属项目的 ssh_* 是从源项目**复制**来的，不是解引用——SshLink 由 ProjectRow
    /// 构造、按 project.id 缓存，get_link / prepare_zellij / GET /host 全部直接读
    /// project.ssh_host。复制让这些点一处都不用改，代价就是这条手动传播。
    pub fn update_children_ssh(&self, source_id: &str, src: &ProjectRow) {
        self.exec(
            "UPDATE projects SET ssh_host=:ssh_host, ssh_port=:ssh_port, ssh_username=:ssh_username,
               ssh_auth_method=:ssh_auth_method, ssh_key_path=:ssh_key_path, ssh_secret_enc=:ssh_secret_enc,
               host_id=:host_id
             WHERE source_project_id=:source_id",
            named_params! {
                ":ssh_host": src.ssh_host, ":ssh_port": src.ssh_port, ":ssh_username": src.ssh_username,
                ":ssh_auth_method": src.ssh_auth_method, ":ssh_key_path": src.ssh_key_path,
                ":ssh_secret_enc": src.ssh_secret_enc, ":host_id": src.host_id, ":source_id": source_id,
            },
        );
    }

    /// 已保存主机改了连接配置后，把五列刷到引用它的全部项目。
    /// 与 update_children_ssh 同构：项目上的 ssh_* 是复制，不是解引用。
    pub fn update_projects_from_host(&self, host: &SshHostRow) {
        self.exec(
            "UPDATE projects SET ssh_host=:host, ssh_port=:port, ssh_username=:username,
               ssh_auth_method=:auth_method, ssh_key_path=:key_path, ssh_secret_enc=:secret_enc
             WHERE host_id=:id",
            named_params! {
                ":host": host.host, ":port": host.port, ":username": host.username,
                ":auth_method": host.auth_method, ":key_path": host.key_path,
                ":secret_enc": host.secret_enc, ":id": host.id,
            },
        );
    }

    pub fn delete_project(&self, id: &str) {
        self.exec("DELETE FROM sessions WHERE project_id = ?", [id]);
        self.exec("DELETE FROM projects WHERE id = ?", [id]);
    }

    // ---- 中转：端口转发（挂 SSH Host） ----
    // 规则数量是个位数到几十，一律全表读、在代码里筛，不为每种查询各写一条 SQL。

    pub fn list_forwards(&self) -> Vec<HostForwardRow> {
        self.query_all("SELECT * FROM host_forwards ORDER BY created_at ASC", [], forward_from_row)
    }

    pub fn get_forward(&self, id: &str) -> Option<HostForwardRow> {
        self.query_one("SELECT * FROM host_forwards WHERE id = ?", [id], forward_from_row)
    }

    pub fn insert_forward(&self, row: &HostForwardRow) {
        self.exec(
            "INSERT INTO host_forwards
               (id, host_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at)
             VALUES
               (:id, :host_id, :name, :kind, :bind_host, :bind_port, :dest_host, :dest_port, :enabled, :created_at)",
            named_params! {
                ":id": row.id, ":host_id": row.host_id, ":name": row.name, ":kind": row.kind,
                ":bind_host": row.bind_host, ":bind_port": row.bind_port, ":dest_host": row.dest_host,
                ":dest_port": row.dest_port, ":enabled": row.enabled, ":created_at": row.created_at,
            },
        );
    }

    /// host_id 不在 SET 里：规则建好后不能换主机
    pub fn update_forward(&self, row: &HostForwardRow) {
        self.exec(
            "UPDATE host_forwards SET name=:name, kind=:kind, bind_host=:bind_host, bind_port=:bind_port,
               dest_host=:dest_host, dest_port=:dest_port, enabled=:enabled
             WHERE id=:id",
            named_params! {
                ":id": row.id, ":name": row.name, ":kind": row.kind, ":bind_host": row.bind_host,
                ":bind_port": row.bind_port, ":dest_host": row.dest_host, ":dest_port": row.dest_port,
                ":enabled": row.enabled,
            },
        );
    }

    pub fn set_forward_enabled(&self, id: &str, enabled: bool) {
        self.exec("UPDATE host_forwards SET enabled = ? WHERE id = ?", params![enabled as i64, id]);
    }

    pub fn delete_forward(&self, id: &str) {
        self.exec("DELETE FROM host_forwards WHERE id = ?", [id]);
    }

    // ---- 中转：公网发布（挂本机或 SSH Host） ----

    pub fn list_shares(&self) -> Vec<HostShareRow> {
        self.query_all("SELECT * FROM host_shares ORDER BY created_at ASC", [], share_from_row)
    }

    pub fn get_share(&self, id: &str) -> Option<HostShareRow> {
        self.query_one("SELECT * FROM host_shares WHERE id = ?", [id], share_from_row)
    }

    pub fn insert_share(&self, row: &HostShareRow) {
        self.exec(
            "INSERT INTO host_shares
               (id, host_id, name, dest_host, dest_port, enabled, created_at)
             VALUES
               (:id, :host_id, :name, :dest_host, :dest_port, :enabled, :created_at)",
            named_params! {
                ":id": row.id, ":host_id": row.host_id, ":name": row.name, ":dest_host": row.dest_host,
                ":dest_port": row.dest_port, ":enabled": row.enabled, ":created_at": row.created_at,
            },
        );
    }

    /// host_id 不在 SET 里：规则建好后不能换机器
    pub fn update_share(&self, row: &HostShareRow) {
        self.exec(
            "UPDATE host_shares SET name=:name, dest_host=:dest_host, dest_port=:dest_port, enabled=:enabled
             WHERE id=:id",
            named_params! {
                ":id": row.id, ":name": row.name, ":dest_host": row.dest_host, ":dest_port": row.dest_port,
                ":enabled": row.enabled,
            },
        );
    }

    pub fn set_share_enabled(&self, id: &str, enabled: bool) {
        self.exec("UPDATE host_shares SET enabled = ? WHERE id = ?", params![enabled as i64, id]);
    }

    pub fn delete_share(&self, id: &str) {
        self.exec("DELETE FROM host_shares WHERE id = ?", [id]);
    }

    /// 删主机时的级联。调用方先把活着的隧道停掉
    pub fn delete_relays_of_host(&self, host_id: &str) {
        self.exec("DELETE FROM host_forwards WHERE host_id = ?", [host_id]);
        self.exec("DELETE FROM host_shares WHERE host_id = ?", [host_id]);
    }

    // ---- 飞书项目面板的固定列表 ----

    pub fn to_meegle_pin(row: &MeeglePinRow) -> MeeglePin {
        MeeglePin {
            id: row.id.clone(),
            kind: MeeglePinKind::from_wire(&row.kind).unwrap_or(MeeglePinKind::Unknown),
            space_key: row.space_key.clone(),
            space_name: row.space_name.clone(),
            target_id: row.target_id.clone(),
            type_key: row.type_key.clone(),
            label: row.label.clone(),
            url: row.url.clone(),
            created_at: row.created_at,
        }
    }

    pub fn list_meegle_pins(&self) -> Vec<MeeglePinRow> {
        self.query_all("SELECT * FROM meegle_pins ORDER BY created_at ASC", [], pin_from_row)
    }

    pub fn get_meegle_pin(&self, id: &str) -> Option<MeeglePinRow> {
        self.query_one("SELECT * FROM meegle_pins WHERE id = ?", [id], pin_from_row)
    }

    /// 同一个东西只固定一次
    pub fn find_meegle_pin(&self, kind: &str, space_key: &str, target_id: &str) -> Option<MeeglePinRow> {
        self.query_one(
            "SELECT * FROM meegle_pins WHERE kind = ? AND space_key = ? AND target_id = ?",
            [kind, space_key, target_id],
            pin_from_row,
        )
    }

    pub fn insert_meegle_pin(&self, row: &MeeglePinRow) {
        self.exec(
            "INSERT INTO meegle_pins
               (id, kind, space_key, space_name, target_id, type_key, label, url, created_at)
             VALUES
               (:id, :kind, :space_key, :space_name, :target_id, :type_key, :label, :url, :created_at)",
            named_params! {
                ":id": row.id, ":kind": row.kind, ":space_key": row.space_key, ":space_name": row.space_name,
                ":target_id": row.target_id, ":type_key": row.type_key, ":label": row.label, ":url": row.url,
                ":created_at": row.created_at,
            },
        );
    }

    pub fn rename_meegle_pin(&self, id: &str, label: &str) {
        self.exec("UPDATE meegle_pins SET label = ? WHERE id = ?", [label, id]);
    }

    pub fn delete_meegle_pin(&self, id: &str) {
        self.exec("DELETE FROM meegle_pins WHERE id = ?", [id]);
    }

    // ---- saved SSH hosts ----

    pub fn to_ssh_host(row: &SshHostRow, project_count: u32) -> SshHost {
        SshHost {
            id: row.id.clone(),
            name: row.name.clone(),
            host: row.host.clone(),
            port: row.port as u16,
            username: row.username.clone(),
            auth_method: SshAuthMethod::from_wire(&row.auth_method).unwrap_or(SshAuthMethod::Agent),
            key_path: row.key_path.clone(),
            has_secret: row.secret_enc.is_some(),
            project_count,
            created_at: row.created_at,
        }
    }

    pub fn list_hosts(&self) -> Vec<SshHost> {
        let rows = self.query_all("SELECT * FROM ssh_hosts ORDER BY created_at ASC", [], host_from_row);
        let counts: Vec<(String, i64)> = self.query_all(
            "SELECT host_id AS id, COUNT(*) AS n FROM projects WHERE host_id IS NOT NULL GROUP BY host_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        rows.iter()
            .map(|row| {
                let n = counts.iter().find(|(id, _)| *id == row.id).map(|(_, n)| *n).unwrap_or(0);
                Self::to_ssh_host(row, n as u32)
            })
            .collect()
    }

    pub fn get_host(&self, id: &str) -> Option<SshHostRow> {
        self.query_one("SELECT * FROM ssh_hosts WHERE id = ?", [id], host_from_row)
    }

    pub fn find_host_by_name(&self, name: &str, except_id: Option<&str>) -> Option<SshHostRow> {
        match except_id.filter(|e| !e.is_empty()) {
            Some(except) => self.query_one(
                "SELECT * FROM ssh_hosts WHERE lower(name) = lower(?) AND id != ?",
                [name, except],
                host_from_row,
            ),
            None => self.query_one("SELECT * FROM ssh_hosts WHERE lower(name) = lower(?)", [name], host_from_row),
        }
    }

    pub fn insert_host(&self, row: &SshHostRow) {
        self.exec(
            "INSERT INTO ssh_hosts (id, name, host, port, username, auth_method, key_path, secret_enc, created_at)
             VALUES (:id, :name, :host, :port, :username, :auth_method, :key_path, :secret_enc, :created_at)",
            named_params! {
                ":id": row.id, ":name": row.name, ":host": row.host, ":port": row.port, ":username": row.username,
                ":auth_method": row.auth_method, ":key_path": row.key_path, ":secret_enc": row.secret_enc,
                ":created_at": row.created_at,
            },
        );
    }

    pub fn update_host(&self, row: &SshHostRow) {
        self.exec(
            "UPDATE ssh_hosts SET name=:name, host=:host, port=:port, username=:username,
             auth_method=:auth_method, key_path=:key_path, secret_enc=:secret_enc
             WHERE id=:id",
            named_params! {
                ":id": row.id, ":name": row.name, ":host": row.host, ":port": row.port, ":username": row.username,
                ":auth_method": row.auth_method, ":key_path": row.key_path, ":secret_enc": row.secret_enc,
            },
        );
    }

    pub fn count_projects_by_host(&self, host_id: &str) -> i64 {
        self.query_one("SELECT COUNT(*) AS n FROM projects WHERE host_id = ?", [host_id], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn delete_host(&self, id: &str) {
        self.exec("DELETE FROM ssh_hosts WHERE id = ?", [id]);
    }

    // ---- sessions ----

    pub fn to_session(row: &SessionRow) -> Session {
        Session {
            id: row.id.clone(),
            project_id: row.project_id.clone(),
            name: row.name.clone(),
            title: None,
            state: SessionState::from_wire(&row.state).unwrap_or(SessionState::Unknown),
            agent: row.agent.as_deref().and_then(SessionAgent::from_wire),
            durable: row.durable == 1,
            non_durable_reason: row
                .non_durable_reason
                .as_deref()
                .map(|r| NonDurableReason::from_wire(r).unwrap_or(NonDurableReason::Unknown)),
            dead_reason: row.dead_reason.as_deref().map(|r| DeadReason::from_wire(r).unwrap_or(DeadReason::Unknown)),
            created_at: row.created_at,
            last_active_at: row.last_active_at,
        }
    }

    pub fn list_sessions(&self) -> Vec<SessionRow> {
        self.query_all("SELECT * FROM sessions ORDER BY created_at ASC", [], session_from_row)
    }

    pub fn list_sessions_by_project(&self, project_id: &str) -> Vec<SessionRow> {
        self.query_all(
            "SELECT * FROM sessions WHERE project_id = ? ORDER BY created_at ASC",
            [project_id],
            session_from_row,
        )
    }

    pub fn get_session(&self, id: &str) -> Option<SessionRow> {
        self.query_one("SELECT * FROM sessions WHERE id = ?", [id], session_from_row)
    }

    pub fn insert_session(&self, row: &SessionRow) {
        self.exec(
            "INSERT INTO sessions (id, project_id, name, state, durable, dead_reason, non_durable_reason, created_at, last_active_at, cols, rows, agent, scroll_plugin)
             VALUES (:id, :project_id, :name, :state, :durable, :dead_reason, :non_durable_reason, :created_at, :last_active_at, :cols, :rows, :agent, :scroll_plugin)",
            named_params! {
                ":id": row.id, ":project_id": row.project_id, ":name": row.name, ":state": row.state,
                ":durable": row.durable, ":dead_reason": row.dead_reason,
                ":non_durable_reason": row.non_durable_reason, ":created_at": row.created_at,
                ":last_active_at": row.last_active_at, ":cols": row.cols, ":rows": row.rows,
                ":agent": row.agent, ":scroll_plugin": row.scroll_plugin,
            },
        );
    }

    pub fn update_session_state(&self, id: &str, state: SessionState, dead_reason: Option<DeadReason>) {
        self.exec(
            "UPDATE sessions SET state = ?, dead_reason = ? WHERE id = ?",
            params![state.as_str(), dead_reason.map(|d| d.as_str()), id],
        );
    }

    pub fn rename_session(&self, id: &str, name: &str) {
        self.exec("UPDATE sessions SET name = ? WHERE id = ?", [name, id]);
    }

    pub fn touch_session(&self, id: &str, ts: i64) {
        self.exec("UPDATE sessions SET last_active_at = ? WHERE id = ?", params![ts, id]);
    }

    pub fn update_session_size(&self, id: &str, cols: u16, rows: u16) {
        self.exec("UPDATE sessions SET cols = ?, rows = ? WHERE id = ?", params![cols, rows, id]);
    }

    pub fn delete_session(&self, id: &str) {
        self.exec("DELETE FROM sessions WHERE id = ?", [id]);
    }

    /// 启动恢复：上次仍 active 的会话，持久 → unverified，非持久 → dead
    pub fn recover_sessions_on_startup(&self) {
        self.exec("UPDATE sessions SET state = 'unverified' WHERE state = 'active' AND durable = 1", []);
        self.exec(
            "UPDATE sessions SET state = 'dead', dead_reason = 'backend-restart' WHERE state = 'active' AND durable = 0",
            [],
        );
    }

    // ---- known hosts (TOFU) ----

    pub fn get_known_host(&self, host: &str, port: u16) -> Option<String> {
        self.query_one("SELECT fingerprint FROM known_hosts WHERE host = ? AND port = ?", params![host, port], |r| {
            r.get(0)
        })
    }

    pub fn save_known_host(&self, host: &str, port: u16, fingerprint: &str) {
        self.exec(
            "INSERT INTO known_hosts (host, port, fingerprint) VALUES (?, ?, ?) ON CONFLICT(host, port) DO UPDATE SET fingerprint = excluded.fingerprint",
            params![host, port, fingerprint],
        );
    }

    // ---- Zellij 主机状态 ----
    //
    // zellij_hosts / known_hosts / ssh_hosts 三张表键不同，不合：
    // known_hosts 表达"这台机器的身份可信"（主机级事实，与登录用户无关，键是 host+port），
    // zellij_hosts 表达"这个账户下装了什么"（账户级事实，键必须含 username——
    // alice@srv 授权过不代表 bob@srv 的 home 里也有二进制），
    // ssh_hosts 表达"用户预先保存的连接配置"（有别名、可改、被项目引用，键是自己的 id）。

    pub fn get_zellij_host(&self, host: &str, port: u16, username: &str) -> Option<ZellijHostRow> {
        self.query_one(
            "SELECT * FROM zellij_hosts WHERE host = ? AND port = ? AND username = ?",
            params![host, port, username],
            zellij_host_from_row,
        )
    }

    /// 部分更新：只写给了的字段，其余保持原值（见 [`ZellijHostPatch`]）。
    pub fn upsert_zellij_host(&self, host: &str, port: u16, username: &str, patch: ZellijHostPatch) {
        let cur = self.get_zellij_host(host, port, username).unwrap_or_default();
        let row = ZellijHostRow {
            host: host.to_string(),
            port: port as i64,
            username: username.to_string(),
            authorized: patch.authorized.unwrap_or(cur.authorized),
            installed_version: patch.installed_version.unwrap_or(cur.installed_version),
            base_url: patch.base_url.unwrap_or(cur.base_url),
            verified_durable: patch.verified_durable.unwrap_or(cur.verified_durable),
            updated_at: crate::auth::now_ms(),
        };
        self.exec(
            "INSERT INTO zellij_hosts (host, port, username, authorized, installed_version, base_url, verified_durable, updated_at)
             VALUES (:host, :port, :username, :authorized, :installed_version, :base_url, :verified_durable, :updated_at)
             ON CONFLICT(host, port, username) DO UPDATE SET
               authorized = excluded.authorized,
               installed_version = excluded.installed_version,
               base_url = excluded.base_url,
               verified_durable = excluded.verified_durable,
               updated_at = excluded.updated_at",
            named_params! {
                ":host": row.host, ":port": row.port, ":username": row.username, ":authorized": row.authorized,
                ":installed_version": row.installed_version, ":base_url": row.base_url,
                ":verified_durable": row.verified_durable, ":updated_at": row.updated_at,
            },
        );
    }
}

impl SettingsStore for std::sync::Arc<Db> {
    fn get_setting(&self, key: &str) -> Option<String> {
        Db::get_setting(self, key)
    }
    fn set_setting(&self, key: &str, value: &str) {
        Db::set_setting(self, key, value)
    }
}

/// zellij_hosts / known_hosts / ssh_hosts 三张表的注释见上；这里是 `ALTER TABLE ADD COLUMN` 的幂等版
fn add_column(conn: &Connection, table: &str, column: &str, ty: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let has = stmt.query_map([], |r| r.get::<_, String>("name"))?.collect::<Result<Vec<_>, _>>()?.iter().any(|c| c == column);
    if !has {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn project_row() -> ProjectRow {
        ProjectRow {
            id: "p1".into(),
            name: "p".into(),
            project_type: "local".into(),
            working_dir: Some("/w".into()),
            created_at: 1,
            ..Default::default()
        }
    }

    fn members(json: &str) -> Option<Vec<MultiRepoMember>> {
        Db::parse_multi_repos(Some(json))
    }

    #[test]
    fn upsert_zellij_host_keeps_omitted_clears_null() {
        let (_d, db) = tmp_db();
        db.upsert_zellij_host(
            "h",
            22,
            "u",
            ZellijHostPatch {
                authorized: Some(Some(1)),
                installed_version: Some(Some("0.44.3".into())),
                verified_durable: Some(Some(0)),
                ..Default::default()
            },
        );
        // 省略的字段保持原值
        db.upsert_zellij_host(
            "h",
            22,
            "u",
            ZellijHostPatch { base_url: Some(Some("https://mirror.example".into())), ..Default::default() },
        );
        let row = db.get_zellij_host("h", 22, "u").unwrap();
        assert_eq!(row.verified_durable, Some(0));
        assert_eq!(row.installed_version.as_deref(), Some("0.44.3"));
        assert_eq!(row.base_url.as_deref(), Some("https://mirror.example"));
        // 显式 null = 清空：重试前作废持久性判定、换下载源作废安装记录
        db.upsert_zellij_host(
            "h",
            22,
            "u",
            ZellijHostPatch {
                verified_durable: Some(None),
                installed_version: Some(None),
                base_url: Some(None),
                ..Default::default()
            },
        );
        let row = db.get_zellij_host("h", 22, "u").unwrap();
        assert_eq!((row.verified_durable, row.installed_version, row.base_url), (None, None, None));
        assert_eq!(row.authorized, Some(1));
    }

    #[test]
    fn parse_multi_repos_round_trip() {
        let (_d, db) = tmp_db();
        let json = r#"[{"dir":"/a/web"},{"dir":"/c/app-x/web","repoDir":"/a/web"}]"#;
        db.insert_project(&ProjectRow { multi_repos: Some(json.into()), ..project_row() });
        let row = db.get_project("p1").unwrap();
        let expected = vec![
            MultiRepoMember { dir: "/a/web".into(), repo_dir: None },
            MultiRepoMember { dir: "/c/app-x/web".into(), repo_dir: Some("/a/web".into()) },
        ];
        assert_eq!(Db::parse_multi_repos(row.multi_repos.as_deref()), Some(expected.clone()));
        assert_eq!(Db::to_project(&row).multi, Some(MultiRepoInfo { repos: expected }));
    }

    #[test]
    fn parse_multi_repos_rejects_bad_shapes() {
        assert_eq!(members("not json"), None);
        assert_eq!(members("{}"), None);
        assert_eq!(members(r#"["a"]"#), None);
        assert_eq!(members(r#"[{"dir":""}]"#), None);
        assert_eq!(members(r#"[{"dir":1}]"#), None);
        assert_eq!(members(r#"[{"dir":"/a","repoDir":2}]"#), None);
        assert_eq!(Db::parse_multi_repos(None), None);
    }

    #[test]
    fn update_multi_repos_container_only() {
        let (_d, db) = tmp_db();
        db.insert_project(&ProjectRow { multi_repos: Some(r#"[{"dir":"/a"}]"#.into()), ..project_row() });
        let two = [MultiRepoMember { dir: "/a".into(), repo_dir: None }, MultiRepoMember { dir: "/b".into(), repo_dir: None }];
        db.update_multi_repos("p1", &two);
        assert_eq!(Db::parse_multi_repos(db.get_project("p1").unwrap().multi_repos.as_deref()), Some(two.to_vec()));
    }

    #[test]
    fn update_multi_repos_cannot_reach_derived_row() {
        let (_d, db) = tmp_db();
        let stored = r#"[{"dir":"/c/app-x/web","repoDir":"/a/web"}]"#;
        db.insert_project(&ProjectRow {
            source_project_id: Some("src".into()),
            worktree_created_by_mojito: Some(1),
            multi_repos: Some(stored.into()),
            ..project_row()
        });
        db.update_multi_repos("p1", &[MultiRepoMember { dir: "/tmp/evil".into(), repo_dir: None }]);
        assert_eq!(db.get_project("p1").unwrap().multi_repos.as_deref(), Some(stored));
    }

    #[test]
    fn default_worktree_branch_round_trip() {
        let (_d, db) = tmp_db();
        db.insert_project(&ProjectRow { default_worktree_branch: Some("main".into()), ..project_row() });
        assert_eq!(Db::to_project(&db.get_project("p1").unwrap()).default_worktree_branch.as_deref(), Some("main"));
        let row = db.get_project("p1").unwrap();
        db.update_project(&ProjectRow { default_worktree_branch: Some("origin/main".into()), ..row });
        assert_eq!(db.get_project("p1").unwrap().default_worktree_branch.as_deref(), Some("origin/main"));
        let row = db.get_project("p1").unwrap();
        db.update_project(&ProjectRow { default_worktree_branch: None, ..row });
        assert_eq!(db.get_project("p1").unwrap().default_worktree_branch, None);
        assert_eq!(Db::to_project(&db.get_project("p1").unwrap()).default_worktree_branch, None);
    }

    #[test]
    fn guard_dirs_of_collects_members() {
        let derived = ProjectRow {
            working_dir: Some("/c/app-x".into()),
            source_project_id: Some("src".into()),
            multi_repos: Some(r#"[{"dir":"/c/app-x/web","repoDir":"/a/web"}]"#.into()),
            ..project_row()
        };
        assert_eq!(Db::guard_dirs_of(&derived), ["/c/app-x", "/c/app-x/web", "/a/web"]);
        assert_eq!(Db::guard_dirs_of(&project_row()), ["/w"]);
        assert!(Db::guard_dirs_of(&ProjectRow { working_dir: None, ..project_row() }).is_empty());
        assert_eq!(Db::guard_dirs_of(&ProjectRow { multi_repos: Some("broken".into()), ..project_row() }), ["/w"]);
    }

    #[test]
    fn delete_relays_of_host_keeps_others() {
        let (_d, db) = tmp_db();
        let fwd = |id: &str, host: &str| HostForwardRow {
            id: id.into(),
            host_id: host.into(),
            kind: "local".into(),
            bind_host: "127.0.0.1".into(),
            bind_port: 1,
            dest_host: "127.0.0.1".into(),
            dest_port: 1,
            created_at: 1,
            ..Default::default()
        };
        let share = |id: &str, host: Option<&str>| HostShareRow {
            id: id.into(),
            host_id: host.map(Into::into),
            dest_host: "127.0.0.1".into(),
            dest_port: 1,
            created_at: 1,
            ..Default::default()
        };
        db.insert_forward(&fwd("f1", "h1"));
        db.insert_forward(&fwd("f2", "h2"));
        db.insert_share(&share("s1", Some("h1")));
        db.insert_share(&share("s2", None));
        db.delete_relays_of_host("h1");
        assert_eq!(db.list_forwards().iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["f2"]);
        assert_eq!(db.list_shares().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s2"]);
    }

    #[test]
    fn clears_auto_names_once_and_recovers_sessions() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::open(dir.path()).unwrap();
            // 已经跑过一次清理：手起的同形名字不该再被抹掉，下面换一个新库验证清理本身
            let s = |id: &str, name: &str, state: &str, durable: i64| SessionRow {
                id: id.into(),
                project_id: "p".into(),
                name: name.into(),
                state: state.into(),
                durable,
                created_at: 1,
                last_active_at: 1,
                ..Default::default()
            };
            db.insert_session(&s("a", "Terminal 3", "active", 1));
            db.insert_session(&s("b", "Terminal 2 号", "active", 0));
            db.recover_sessions_on_startup();
            assert_eq!(db.get_session("a").unwrap().state, "unverified");
            let b = db.get_session("b").unwrap();
            assert_eq!((b.state.as_str(), b.dead_reason.as_deref()), ("dead", Some("backend-restart")));
            db.lock().execute("DELETE FROM settings WHERE key = 'migration.sessions.clearAutoNames'", []).unwrap();
        }
        let db = Db::open(dir.path()).unwrap();
        assert_eq!(db.get_session("a").unwrap().name, "");
        assert_eq!(db.get_session("b").unwrap().name, "Terminal 2 号");
        db.rename_session("a", "Terminal 9");
        let db = Db::open(dir.path()).unwrap();
        assert_eq!(db.get_session("a").unwrap().name, "Terminal 9");
    }

    #[test]
    fn known_hosts_and_settings() {
        let (_d, db) = tmp_db();
        assert_eq!(db.get_known_host("h", 22), None);
        db.save_known_host("h", 22, "aa");
        db.save_known_host("h", 22, "bb");
        assert_eq!(db.get_known_host("h", 22).as_deref(), Some("bb"));
        db.set_setting("k", "1");
        db.set_setting("k", "2");
        assert_eq!(db.get_setting("k").as_deref(), Some("2"));
    }

    #[test]
    fn falls_back_to_mojito_db() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mojito.db"), b"").unwrap();
        assert_eq!(db_file(dir.path()), dir.path().join("mojito.db"));
        std::fs::write(dir.path().join("falcon.db"), b"").unwrap();
        assert_eq!(db_file(dir.path()), dir.path().join("falcon.db"));
    }

    /// 先让 Db 建好全套 schema，再用第二条原始连接补出旧版的两张表与数据，
    /// 最后重新打开触发迁移——与老用户升级时的顺序一致。
    fn legacy_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        {
            let seed = Db::open(dir.path()).unwrap();
            seed.insert_host(&SshHostRow {
                id: "h1".into(),
                name: "linux".into(),
                host: "10.0.0.1".into(),
                port: 22,
                username: "fay".into(),
                auth_method: "agent".into(),
                created_at: 1,
                ..Default::default()
            });
            // p1 绑了主机；p2 没绑但连接三元组对得上；p3 哪台都对不上
            let ssh = |id: &str, host_id: Option<&str>, user: &str| ProjectRow {
                id: id.into(),
                project_type: "ssh".into(),
                host_id: host_id.map(Into::into),
                ssh_host: Some("10.0.0.1".into()),
                ssh_port: Some(22),
                ssh_username: Some(user.into()),
                ..project_row()
            };
            seed.insert_project(&ssh("p1", Some("h1"), "fay"));
            seed.insert_project(&ssh("p2", None, "fay"));
            seed.insert_project(&ssh("p3", None, "bob"));
        }
        let raw = Connection::open(dir.path().join("falcon.db")).unwrap();
        raw.execute_batch(
            "CREATE TABLE ssh_forwards (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT, kind TEXT NOT NULL,
               bind_host TEXT NOT NULL, bind_port INTEGER NOT NULL, dest_host TEXT NOT NULL, dest_port INTEGER NOT NULL,
               enabled INTEGER NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE public_shares (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT, origin TEXT NOT NULL,
               dest_host TEXT NOT NULL, dest_port INTEGER NOT NULL, enabled INTEGER NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO ssh_forwards VALUES
               ('f1', 'p1', 'db', 'local', '127.0.0.1', 5432, '127.0.0.1', 5432, 1, 10),
               ('f2', 'p2', NULL, 'local', '127.0.0.1', 5432, '127.0.0.1', 15432, 1, 20),
               ('f3', 'p3', NULL, 'local', '127.0.0.1', 9000, '127.0.0.1', 9000, 1, 30);
             INSERT INTO public_shares VALUES
               ('s1', 'p1', NULL, 'remote', '127.0.0.1', 3000, 1, 10),
               ('s2', 'p3', NULL, 'local', '127.0.0.1', 6789, 1, 20),
               ('s3', 'p3', NULL, 'remote', '127.0.0.1', 3000, 1, 30);",
        )
        .unwrap();
        dir
    }

    #[test]
    fn relays_move_onto_hosts_one_enabled_per_port() {
        let dir = legacy_dir();
        let db = Db::open(dir.path()).unwrap();
        let f: Vec<_> = db.list_forwards().into_iter().map(|f| (f.id, f.host_id, f.enabled)).collect();
        // 并到同一台机器后与 f1 抢本机 5432：留最早的 f1
        assert_eq!(f, [("f1".to_string(), "h1".to_string(), 1), ("f2".to_string(), "h1".to_string(), 0)]);
        let s: Vec<_> = db.list_shares().into_iter().map(|s| (s.id, s.host_id, s.enabled)).collect();
        // origin=local 本来就是后端本机，与项目挂哪台主机无关
        assert_eq!(s, [("s1".to_string(), Some("h1".to_string()), 1), ("s2".to_string(), None, 1)]);
    }

    #[test]
    fn relays_migration_runs_once_and_is_harmless_fresh() {
        let dir = legacy_dir();
        drop(Db::open(dir.path()).unwrap());
        let again = Db::open(dir.path()).unwrap();
        assert_eq!(again.list_forwards().len(), 2);
        let raw = Connection::open(dir.path().join("falcon.db")).unwrap();
        let n: i64 = raw
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('ssh_forwards', 'public_shares')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        assert!(tmp_db().1.list_forwards().is_empty());
    }
}
