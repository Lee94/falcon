//! 内置插件「Agent 用量」：统计本机 Coding Agent（Claude Code、Codex、Grok、Devin、OpenCode）的 Token 使用计量。
//!
//! 只读本机已落盘的会话数据（`~/.claude`、`~/.codex`、`~/.grok`、`~/.local/share/devin`、
//! `~/.local/share/opencode`），不调各家在线账单接口、也不动正在跑的会话。
//!
//! 增量扫描：Claude / Grok 的 JSONL 会话文件攒久了有几百 MB，每次请求全量重读会卡住面板。
//! [`Scanner`] 按文件路径记 `(mtime_ns, len)`，没变的文件直接沿用上次解析出的 [`SessionRecord`]；
//! 紧接着的请求（[`REUSE`] 之内）直接共用整份扫描结果。

pub mod parse;
pub mod render;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use falcon_proto::{PluginInfo, PluginKind, PluginRenderInput, PluginScope, PluginView};

use self::parse::{FileStamp, Scanner, SessionRecord};
use crate::db::Db;
use crate::sessions::local::BaseEnvFn;

pub const ID: &str = "tokens";

/// 这么久之内的请求直接共用上一次扫描结果（切换筛选项或两扇窗口同看时不必再扫一遍目录）
const REUSE: Duration = Duration::from_secs(3);

pub fn info(enabled: bool) -> PluginInfo {
    PluginInfo {
        id: ID.into(),
        name: "Agent 用量".into(),
        description: Some("本机 Coding Agent（Claude Code、Codex、Grok、Devin、OpenCode）的 Token 用量、每日走势与会话明细".into()),
        icon: "coins".into(),
        kind: PluginKind::Builtin,
        scope: PluginScope::Project,
        refresh: Some(render::REFRESH),
        enabled,
        error: None,
        dir: None,
    }
}

#[derive(Default)]
struct State {
    home: Option<PathBuf>,
    scanner: Scanner,
    last: Option<(Instant, Arc<Vec<SessionRecord>>)>,
}

pub struct Tokens {
    base_env: BaseEnvFn,
    state: Arc<Mutex<State>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 本机家目录：优先取插件环境里的 `HOME` / `USERPROFILE`（测试可覆写），再退回进程自己的环境变量
fn resolve_home(env: &[(String, String)]) -> Option<PathBuf> {
    let get = |k: &str| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };
    if let Some(h) = get("HOME").or_else(|| get("USERPROFILE")) {
        return Some(PathBuf::from(h));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Tokens {
    pub fn new(base_env: BaseEnvFn) -> Self {
        Tokens { base_env, state: Arc::new(Mutex::new(State::default())) }
    }

    async fn records(&self) -> Vec<SessionRecord> {
        let env = (self.base_env)().await;
        let Some(home) = resolve_home(&env) else {
            return Vec::new();
        };
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut st = lock(&state);
            if st.home.as_ref() != Some(&home) {
                st.home = Some(home.clone());
                st.scanner = Scanner::default();
                st.last = None;
            }
            if let Some((t, cached)) = &st.last
                && t.elapsed() < REUSE
            {
                return (**cached).clone();
            }
            let list = Arc::new(st.scanner.scan(&home));
            st.last = Some((Instant::now(), list.clone()));
            (*list).clone()
        })
        .await
        .unwrap_or_default()
    }

    pub async fn render(&self, db: &Db, input: &PluginRenderInput) -> PluginView {
        let project = input
            .project_id
            .as_deref()
            .and_then(|id| db.get_project(id))
            .filter(|p| p.project_type == "local")
            .and_then(|p| Some((p.name, p.working_dir?)));
        let records = self.records().await;
        render::view(&records, &input.params, project.as_ref().map(|(n, d)| (n.as_str(), d.as_str())), now_secs())
    }
}

/// 读文件元数据的 `(mtime_ns, len)`，用来判文件有没有变过
pub(crate) fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    Some(FileStamp { mtime_ns: mtime, len: meta.len() })
}
