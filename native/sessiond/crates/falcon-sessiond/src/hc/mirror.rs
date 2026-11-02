//! 会话镜像与事件总线（设计文档 3.4 节）：
//! - 周期扫描 `<root>/sessiond/sessions/`，把 falcon 会话映射为 `herdr-core` 的 `Terminal` JSON；
//! - 为每个活着的 holder 维护一条后台监视连接（`Role::Hc`），实时接收 `Event::Title`、
//!   `Event::ProgramStatus`、`Event::Size`、`Event::Exit`，并每当根状态离开 `working` 时递增 `turn`；
//! - 有事件订阅者时按 1 Hz 向 holder 轮询 `ClientMsg::Foreground`，发现前台进程/目录变化即发 `foreground` 事件；
//! - 维护单调递增 `seq` 与事件广播，驱动 `events.subscribe` 与 `terminal.wait`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use falcon_proto::program_status::{ProgramBlockKind, ProgramState, ProgramStatusRecord};
use falcon_proto::sessiond::{
    ClientMsg, Event, ExitRecord, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Role, negotiate, read_frame,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast, mpsc};

use crate::hc::registry::{Registry, format_addr, parse_addr, valid_terminal_name};
use crate::hc::rpc::RpcError;
use crate::ipc;
use crate::layout::{Layout, entries, now_ms};

/// 事件通知载荷（不含外层 JSON-RPC 信封）
#[derive(Debug, Clone)]
pub struct EmittedEvent {
    #[allow(dead_code)]
    pub seq: u64,
    /// `params` 对象：`{"seq": N, "type": "...", "terminal": "t1", "data": {...}}`
    pub params: Value,
    /// 产生该事件时的完整 `Terminal` JSON（`doc` 事件为 `Value::Null`）
    pub record: Value,
    /// 变更前的 `Terminal` JSON（用于判断标签变更是否让终端新进入某个订阅过滤器）
    pub was: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HcForeground {
    pub pid: i32,
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HcStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub app: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub msg: String,
}

#[derive(Debug, Clone)]
pub struct TerminalEntry {
    pub sid: String,
    pub session_id: String,
    pub seq: u32,
    pub address: String,
    pub name: Option<String>,
    pub keep: bool,
    pub alive: bool,
    pub pid: Option<u32>,
    pub holder_pid: u32,
    pub created_ms: u64,
    pub finished_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub reason: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub raw_title: String,
    pub title: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[allow(dead_code)]
    pub owner: String,
    pub labels: BTreeMap<String, String>,
    pub status: Option<HcStatus>,
    pub status_ms: Option<u64>,
    pub records: BTreeMap<String, HcStatus>,
    pub raw_records: Vec<ProgramStatusRecord>,
    pub turn: u64,
    pub reported: bool,
    pub foreground: Option<HcForeground>,
}

impl TerminalEntry {
    pub fn to_public_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("address".into(), json!(self.address));
        obj.insert("name".into(), json!(self.name));
        obj.insert("labels".into(), json!(self.labels));
        obj.insert("argv".into(), json!(self.argv));
        obj.insert("cwd".into(), json!(self.cwd));
        obj.insert("keep".into(), json!(self.keep));
        obj.insert("state".into(), json!(if self.alive { "live" } else { "finished" }));
        obj.insert("pid".into(), json!(self.pid));
        obj.insert("exit_code".into(), json!(self.exit_code));
        obj.insert("signal".into(), json!(self.signal));
        obj.insert("reason".into(), json!(self.reason));
        obj.insert("created_ms".into(), json!(self.created_ms));
        obj.insert("finished_ms".into(), json!(self.finished_ms));
        obj.insert("screen".into(), json!("exact"));
        obj.insert("cols".into(), json!(self.cols));
        obj.insert("rows".into(), json!(self.rows));
        obj.insert("identity".into(), json!("falcon"));
        obj.insert("status".into(), json!(self.status));
        obj.insert("status_ms".into(), json!(self.status_ms));
        if !self.records.is_empty() {
            obj.insert("records".into(), json!(self.records));
        }
        obj.insert("turn".into(), json!(self.turn));
        obj.insert("reported".into(), json!(self.reported));
        obj.insert("title".into(), json!(self.title));
        obj.insert("parent".into(), Value::Null);
        obj.insert("notify".into(), json!(false));
        obj.insert("die_with_parent".into(), json!(false));
        obj.insert("rm".into(), json!(false));
        obj.insert("template".into(), Value::Null);
        if let Some(fg) = &self.foreground {
            obj.insert("foreground".into(), json!(fg));
        }
        Value::Object(obj)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub terminal: Option<String>,
    pub children_of: Option<String>,
    pub labels: BTreeMap<String, String>,
}

impl EventFilter {
    pub fn matches(&self, record: &Value) -> bool {
        if record.is_null() {
            // doc 事件只推给无终端/标签过滤条件的订阅者
            return self.terminal.is_none() && self.children_of.is_none() && self.labels.is_empty();
        }
        self.terminal.as_ref().is_none_or(|a| record["address"].as_str() == Some(a.as_str()))
            && self.children_of.as_ref().is_none_or(|p| record["parent"].as_str() == Some(p.as_str()))
            && self.labels.iter().all(|(k, v)| record["labels"][k].as_str() == Some(v.as_str()))
    }
}

pub struct GatewayState {
    pub layout: Layout,
    pub started_at: Instant,
    pub registry: Registry,
    pub terminals: BTreeMap<String, TerminalEntry>,
    /// 已启动监视线程的 sid → 给该监视线程发请求（如 `Foreground`）的通道
    pub monitors: HashMap<String, std::sync::mpsc::Sender<ClientMsg>>,
    /// 单调递增事件序号
    pub seq: u64,
    /// 当前活跃的 `events.subscribe` 订阅者数量（>0 时才轮询 Foreground）
    pub subscriber_count: usize,
    /// 供 `terminal.send` + `terminal.wait` 关联 caller 的最近一次 turn：`(caller, sid) -> turn`
    pub last_send: HashMap<(String, String), u64>,
    /// 供 `terminal.wait --output` 订阅某会话的实时输出字节
    pub output_watchers: HashMap<String, Vec<mpsc::UnboundedSender<Vec<u8>>>>,
    pub event_tx: broadcast::Sender<EmittedEvent>,
    pub changed: Arc<Notify>,
}

pub type SharedGateway = Arc<Mutex<GatewayState>>;

impl GatewayState {
    pub fn new(layout: Layout) -> anyhow::Result<SharedGateway> {
        let registry = Registry::load(layout.clone())?;
        let (event_tx, _) = broadcast::channel(2048);
        let changed = Arc::new(Notify::new());
        let st = Arc::new(Mutex::new(Self {
            layout,
            started_at: Instant::now(),
            registry,
            terminals: BTreeMap::new(),
            monitors: HashMap::new(),
            seq: 0,
            subscriber_count: 0,
            last_send: HashMap::new(),
            output_watchers: HashMap::new(),
            event_tx,
            changed,
        }));
        sync_directory(&st, false);
        Ok(st)
    }

    pub fn emit(&mut self, kind: &str, sid: &str, data: Value, was: Option<Value>) {
        let Some(t) = self.terminals.get(sid) else { return };
        let record = t.to_public_json();
        let address = t.address.clone();
        self.seq += 1;
        let seq = self.seq;
        let params = json!({
            "seq": seq,
            "type": kind,
            "terminal": address,
            "data": data,
        });
        let _ = self.event_tx.send(EmittedEvent { seq, params, record, was });
        self.changed.notify_waiters();
    }

    pub fn emit_removed(&mut self, address: &str, was: Value) {
        self.seq += 1;
        let seq = self.seq;
        let params = json!({
            "seq": seq,
            "type": "removed",
            "terminal": address,
            "data": {},
        });
        let _ = self.event_tx.send(EmittedEvent {
            seq,
            params,
            record: was.clone(),
            was: Some(was),
        });
        self.changed.notify_waiters();
    }

    pub fn emit_doc(&mut self, name: &str, version: u64) {
        self.seq += 1;
        let seq = self.seq;
        let params = json!({
            "seq": seq,
            "type": "doc",
            "data": {
                "name": name,
                "version": version,
            },
        });
        let _ = self.event_tx.send(EmittedEvent {
            seq,
            params,
            record: Value::Null,
            was: None,
        });
        self.changed.notify_waiters();
    }

    pub fn matching(&self, filter: &EventFilter) -> Vec<Value> {
        let mut list: Vec<&TerminalEntry> = self.terminals.values().collect();
        list.sort_by_key(|t| t.seq);
        list.into_iter()
            .map(TerminalEntry::to_public_json)
            .filter(|v| filter.matches(v))
            .collect()
    }

    pub fn snapshot(&self, filter: &EventFilter) -> Value {
        json!({
            "seq": self.seq,
            "terminals": self.matching(filter),
        })
    }

    /// 解析 `target`（支持 `<machine>/<local>` 剥掉本机前缀、`t<seq>`、自定义 `name`、完整 UUID、16 位 `sid` 及唯一前缀）
    pub fn resolve_sid(&self, target: &str) -> Result<String, RpcError> {
        let local = match target.split_once('/') {
            Some((prefix, rest)) => {
                if prefix == self.registry.identity.core_id || prefix == self.registry.identity.name || prefix == "falcon" {
                    rest
                } else {
                    return Err(RpcError::new(
                        -32000,
                        "machine_unknown",
                        format!("unknown machine {prefix:?}"),
                        Some("see `hc machine ls`"),
                    ));
                }
            }
            None => target,
        };
        if local.is_empty() {
            return Err(RpcError::not_found(target));
        }
        // 1. 短地址 t<seq>
        if let Some(seq) = parse_addr(local) {
            if let Some(t) = self.terminals.values().find(|t| t.seq == seq) {
                return Ok(t.sid.clone());
            }
            return Err(RpcError::not_found(target));
        }
        // 2. 自定义 name：优先匹配 live，否则最近创建的 finished
        if let Some(t) = self.terminals.values().find(|t| t.alive && t.name.as_deref() == Some(local)) {
            return Ok(t.sid.clone());
        }
        if let Some(t) = self
            .terminals
            .values()
            .filter(|t| t.name.as_deref() == Some(local))
            .max_by_key(|t| t.seq)
        {
            return Ok(t.sid.clone());
        }
        // 3. 精确 sid 或 session_id
        if let Some(t) = self.terminals.get(local) {
            return Ok(t.sid.clone());
        }
        if let Some(t) = self.terminals.values().find(|t| t.session_id == local) {
            return Ok(t.sid.clone());
        }
        // 4. sid 前缀匹配
        if local.len() >= 4 && local.bytes().all(|b| b.is_ascii_hexdigit()) {
            let matches: Vec<&TerminalEntry> = self.terminals.values().filter(|t| t.sid.starts_with(local)).collect();
            if matches.len() == 1 {
                return Ok(matches[0].sid.clone());
            } else if matches.len() > 1 {
                return Err(RpcError::ambiguous(
                    target,
                    matches.into_iter().map(|t| t.address.clone()).collect(),
                ));
            }
        }
        Err(RpcError::not_found(target))
    }

    /// 检查自定义名字是否已被其他活着的终端占用
    pub fn check_name_free(&self, name: &str, except_sid: Option<&str>) -> Result<(), RpcError> {
        if !valid_terminal_name(name) {
            return Err(RpcError::invalid_params(format!("invalid terminal name {name:?}")));
        }
        if self
            .terminals
            .values()
            .any(|t| t.alive && t.name.as_deref() == Some(name) && Some(t.sid.as_str()) != except_sid)
        {
            return Err(RpcError::name_taken(name));
        }
        Ok(())
    }
}

/// 合成 `herdr.space` / `herdr.tab` / `falcon.space_name` / `where` 标签（当 `synthesize_labels` 开启时）
pub fn synthesize_labels(
    sid: &str,
    raw: &BTreeMap<String, String>,
    synthesize: bool,
) -> BTreeMap<String, String> {
    let mut out = raw.clone();
    if !synthesize {
        return out;
    }
    let project_id = out.get("falcon.project").cloned().unwrap_or_else(|| "standalone".into());
    let project_name = out.get("falcon.project_name").cloned().filter(|s| !s.is_empty());
    let host_name = out.get("falcon.host_name").cloned().filter(|s| !s.is_empty());
    let session_name = out.get("falcon.session").cloned().filter(|s| !s.is_empty());
    let worktree_branch = out.get("falcon.worktree_branch").cloned().filter(|s| !s.is_empty());
    let is_standalone = out.get("falcon.standalone").is_some_and(|v| v == "true")
        || project_id == "standalone"
        || project_id.starts_with("standalone:");

    let space_name = out
        .get("falcon.space_name")
        .cloned()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let proj_label = if is_standalone {
                Some(project_name.unwrap_or_else(|| "无项目".into()))
            } else {
                match (project_name, worktree_branch) {
                    (Some(p), Some(b)) if !p.contains(" / ") => Some(format!("{p} / {b}")),
                    (Some(p), _) => Some(p),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            };
            match (host_name, proj_label) {
                (Some(h), Some(p)) => Some(format!("{h} · {p}")),
                (Some(h), None) => Some(h),
                (None, Some(p)) => Some(p),
                (None, None) => None,
            }
        });

    if let Some(sn) = &space_name {
        out.entry("falcon.space_name".into())
            .or_insert_with(|| sn.clone());
    }
    out.entry("herdr.space".into())
        .or_insert_with(|| format!("falcon.{project_id}"));
    out.entry("herdr.tab".into())
        .or_insert_with(|| format!("falcon.{sid}"));
    if !out.contains_key("where") {
        match (space_name, session_name) {
            (Some(p), Some(s)) => {
                out.insert("where".into(), format!("{p} / {s}"));
            }
            (Some(p), None) => {
                out.insert("where".into(), p);
            }
            (None, Some(s)) => {
                out.insert("where".into(), s);
            }
            (None, None) => {}
        }
    }
    out
}

fn same_alnum_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let na = norm(a);
    !na.is_empty() && na == norm(b)
}

fn app_display_label(app: &str) -> String {
    match app.trim() {
        "claude-code" => "claude".into(),
        other => other.to_string(),
    }
}

fn agent_app_id(cmd: &str) -> Option<&'static str> {
    let leaf = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
    let bare = leaf
        .strip_suffix(".sh")
        .or_else(|| leaf.strip_suffix(".cmd"))
        .or_else(|| leaf.strip_suffix(".exe"))
        .or_else(|| leaf.strip_suffix(".bat"))
        .unwrap_or(leaf);
    match bare.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Some("claude-code"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        "devin" => Some("devin"),
        _ => None,
    }
}

fn extract_agent_from_launcher_path(arg0: &str) -> Option<String> {
    let normalized = arg0.replace('\\', "/");
    let (_, tail) = normalized.rsplit_once("/agents/")?;
    let stem = tail
        .strip_suffix(".sh")
        .or_else(|| tail.strip_suffix(".cmd"))
        .or_else(|| tail.strip_suffix(".ps1"))
        .or_else(|| tail.strip_suffix(".bat"))
        .unwrap_or(tail);
    (!stem.is_empty() && !stem.contains('/')).then(|| stem.to_string())
}

fn shell_leaf(shell: Option<&str>) -> String {
    let leaf = shell.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let bare = if leaf.len() >= 4 && leaf.is_char_boundary(leaf.len() - 4) {
        let (head, tail) = leaf.split_at(leaf.len() - 4);
        if [".exe", ".cmd", ".bat"]
            .iter()
            .any(|s| tail.eq_ignore_ascii_case(s))
        {
            head
        } else {
            leaf
        }
    } else {
        leaf
    };
    let bare = bare.strip_prefix('-').unwrap_or(bare);
    if bare.is_empty() {
        "zsh".to_string()
    } else {
        bare.to_string()
    }
}

fn is_shell_name(name: &str, configured_shell: Option<&str>) -> bool {
    let b = shell_leaf(Some(name)).to_ascii_lowercase();
    if matches!(
        b.as_str(),
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "pwsh" | "powershell" | "cmd"
    ) {
        return true;
    }
    if let Some(cfg) = configured_shell {
        let cfg_b = shell_leaf(Some(cfg)).to_ascii_lowercase();
        if !cfg_b.is_empty() && b == cfg_b {
            return true;
        }
    }
    false
}

/// 清理终端标题：
/// 1. 剥去打头的状态符号（如 Claude Code 的 `✳ `、`◐ `、`◑ `、盲文转圈 `⠋ `）；
/// 2. 剥去 Codex CLI 拼接的 ` | <虚拟项目 UUID 前缀或目录名>` 后缀；
/// 3. 若标题仅为程序自身名字（如 `"Claude Code"`），视为空串以便回退到统一的程序标识。
pub fn clean_terminal_title(
    raw: &str,
    cwd: &str,
    project_id: Option<&str>,
    app_or_agent: Option<&str>,
) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    let s = cleaned.trim();
    if s.is_empty() {
        return String::new();
    }
    let s = match s.split_once(char::is_whitespace) {
        Some((head, rest))
            if head.chars().count() <= 2
                && head.chars().all(|c| !c.is_alphanumeric() && !c.is_ascii())
                && !rest.trim().is_empty() =>
        {
            rest.trim()
        }
        _ => s,
    };
    let s = if let Some((lhs, rhs)) = s.rsplit_once(" | ") {
        let lhs_trim = lhs.trim();
        let rhs_trim = rhs.trim();
        let rhs_no_dots = rhs_trim.trim_end_matches(['.', '…']);
        let is_uuid_like = rhs_no_dots.len() >= 8
            && rhs_no_dots.contains('-')
            && rhs_no_dots.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
        let matches_proj_id = project_id
            .filter(|pid| pid.len() >= 8 && !rhs_no_dots.is_empty())
            .is_some_and(|pid| pid.starts_with(rhs_no_dots));
        let cwd_leaf = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
        let matches_cwd_leaf = !cwd_leaf.is_empty()
            && !rhs_no_dots.is_empty()
            && (rhs_trim == cwd_leaf || cwd_leaf.starts_with(rhs_no_dots));
        if !lhs_trim.is_empty() && (is_uuid_like || matches_proj_id || matches_cwd_leaf) {
            lhs_trim
        } else {
            s
        }
    } else {
        s
    };
    if same_alnum_name(s, "Claude Code")
        || same_alnum_name(s, "Codex")
        || app_or_agent.is_some_and(|a| same_alnum_name(s, a))
    {
        return String::new();
    }
    s.to_string()
}

/// 计算会话在 Falcon 侧栏与 `herdr-app` 标签栏中的显示名称，优先级与 `falcon-core::session_title` 一致：
/// 手动命名 > 清理后的主题标题 > 正在前台运行的程序名 / 7501 状态应用名 > 会话绑定的 agent 名 > shell 名。
pub fn effective_session_name(t: &TerminalEntry) -> String {
    if let Some(n) = t.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    if let Some(n) = t.labels.get("falcon.session").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    if !t.title.trim().is_empty() {
        return t.title.trim().to_string();
    }
    let proj_id = t.labels.get("falcon.project").map(String::as_str);
    let agent_lbl = t
        .labels
        .get("falcon.agent")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| t.argv.first().and_then(|a| extract_agent_from_launcher_path(a)));
    if let Some(ft) = t.labels.get("falcon.title").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let cleaned = clean_terminal_title(ft, &t.cwd, proj_id, agent_lbl.as_deref());
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    if let Some(st) = &t.status {
        if !st.title.trim().is_empty() {
            let cleaned = clean_terminal_title(&st.title, &t.cwd, proj_id, Some(&st.app));
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
        if !st.app.trim().is_empty() {
            return app_display_label(&st.app);
        }
    }
    let cfg_shell = t.labels.get("falcon.shell").map(String::as_str);
    if let Some(fg) = &t.foreground {
        let fg_name = fg.name.trim();
        if !fg_name.is_empty() && !is_shell_name(fg_name, cfg_shell) {
            return app_display_label(fg_name);
        }
    }
    if let Some(agent) = agent_lbl {
        return app_display_label(&agent);
    }
    if let Some(fg) = &t.foreground
        && !fg.name.trim().is_empty()
    {
        return shell_leaf(Some(&fg.name));
    }
    if let Some(sh) = cfg_shell.filter(|s| !s.trim().is_empty()) {
        return shell_leaf(Some(sh));
    }
    if let Some(arg0) = t.argv.first().map(String::as_str).filter(|s| !s.is_empty() && *s != "sh") {
        return shell_leaf(Some(arg0));
    }
    "zsh".to_string()
}

/// 刷新终端的派生展示字段（归一化 `argv`、清理 `title`、补齐活跃 agent 的默认 `status`、更新 `where` 标签）
pub fn refresh_terminal_derived(t: &mut TerminalEntry, synthesize: bool) {
    let agent_lbl = t
        .labels
        .get("falcon.agent")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| t.argv.first().and_then(|a| extract_agent_from_launcher_path(a)));
    if let Some(agent) = &agent_lbl {
        t.labels
            .entry("falcon.agent".into())
            .or_insert_with(|| agent.clone());
    }

    // 归一化 argv：把 `<root>/agents/<agent>.sh` 或空 `["sh"]` 替换为真实的 agent 或 shell 命令
    if let Some(arg0) = t.argv.first()
        && let Some(agent) = extract_agent_from_launcher_path(arg0)
    {
        t.argv = vec![agent];
    } else if t.argv.is_empty() || (t.argv.len() == 1 && t.argv[0] == "sh") {
        if let Some(agent) = &agent_lbl {
            t.argv = vec![agent.clone()];
        } else if let Some(fg) = &t.foreground
            && !fg.name.is_empty()
        {
            t.argv = vec![fg.name.clone()];
        } else if let Some(sh) = t.labels.get("falcon.shell").filter(|s| !s.is_empty()) {
            t.argv = vec![shell_leaf(Some(sh))];
        }
    }

    // 清理标题（剥去转圈符号、虚拟项目 UUID 后缀及重复的程序自身名称）
    let proj_id = t.labels.get("falcon.project").map(String::as_str);
    let app_hint = t
        .status
        .as_ref()
        .map(|s| s.app.as_str())
        .filter(|s| !s.is_empty())
        .or(agent_lbl.as_deref());
    let source_title = if !t.raw_title.is_empty() {
        t.raw_title.clone()
    } else {
        t.labels.get("falcon.title").cloned().unwrap_or_default()
    };
    t.title = clean_terminal_title(&source_title, &t.cwd, proj_id, app_hint);

    // 若会话仍在运行、7501 记录为空，但前台正在运行已知 agent（或尚未完成首次前台探测的 agent 会话），
    // 合成一条默认 idle 状态，使 `herdr-app` 将其识别为 Agent 并展示对应图标。
    if t.alive && t.raw_records.is_empty() {
        let active_agent_app = match &t.foreground {
            Some(fg) => agent_app_id(&fg.name),
            None => agent_lbl.as_deref().and_then(agent_app_id),
        };
        t.status = active_agent_app.map(|app| HcStatus {
            state: "idle".into(),
            kind: None,
            progress: None,
            app: app.into(),
            title: String::new(),
            msg: String::new(),
        });
    }

    if synthesize {
        let eff_name = effective_session_name(t);
        if let Some(space_name) = t.labels.get("falcon.space_name").filter(|s| !s.is_empty()).cloned() {
            t.labels.insert("where".into(), format!("{space_name} / {eff_name}"));
        }
    }
}

/// 将 `ProgramStatusRecord` 列表转换为 `(root_status, sub_records)`
pub fn convert_program_records(records: &[ProgramStatusRecord]) -> (Option<HcStatus>, BTreeMap<String, HcStatus>) {
    let mut root = None;
    let mut map = BTreeMap::new();
    for r in records {
        let state = match r.state {
            ProgramState::Idle => "idle",
            ProgramState::Working => "working",
            ProgramState::Done => "done",
            ProgramState::Blocked => "blocked",
            ProgramState::Error => "error",
            ProgramState::Unknown => continue,
        };
        let kind = r.kind.as_ref().and_then(|k| match k {
            ProgramBlockKind::Permission => Some("permission".to_string()),
            ProgramBlockKind::Question => Some("question".to_string()),
            ProgramBlockKind::Auth => Some("auth".to_string()),
            ProgramBlockKind::Unknown => None,
        });
        let st = HcStatus {
            state: state.into(),
            kind,
            progress: r.progress,
            app: r.app.clone().unwrap_or_default(),
            title: r.title.clone().unwrap_or_default(),
            msg: r.message.clone().unwrap_or_default(),
        };
        if r.id.is_empty() || r.id == "default" {
            root = Some(st);
        } else {
            map.insert(r.id.clone(), st);
        }
    }
    (root, map)
}

fn exit_fields(exited: Option<&ExitRecord>, alive: bool) -> (Option<i32>, Option<i32>, Option<String>, Option<u64>) {
    match exited {
        Some(rec) => {
            let reason = if rec.status.signal.is_some() && rec.status.code.is_none() {
                "killed"
            } else {
                "exited"
            };
            (
                rec.status.code,
                rec.status.signal,
                Some(reason.into()),
                Some(rec.at.max(0) as u64),
            )
        }
        None if !alive => (None, None, Some("holder_lost".into()), Some(now_ms().max(0) as u64)),
        None => (None, None, None, None),
    }
}

/// 扫描 `<root>/sessiond/sessions/`，把新增/变更/退出的会话同步进 `GatewayState`
pub fn sync_directory(gw: &SharedGateway, emit_events: bool) {
    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let layout = st.layout.clone();
    let synthesize = st.registry.identity.synthesize_labels;
    let disk_entries = entries(&layout);
    let mut dirty_reg = false;
    let mut to_monitor = Vec::new();

    for e in disk_entries {
        let p_existing = st.registry.terminals.entries.get(&e.sid).cloned();
        if p_existing.as_ref().is_some_and(|p| p.removed && !e.alive) {
            continue;
        }
        let prev_next_seq = st.registry.terminals.next_seq;
        let persisted = st
            .registry
            .ensure_terminal(
                &e.sid,
                &e.session_id,
                e.created_at,
                &e.argv,
                e.cwd.as_deref(),
                &e.owner,
                e.alive,
            )
            .clone();
        if st.registry.terminals.next_seq != prev_next_seq || p_existing.is_none() {
            dirty_reg = true;
        }

        let mut raw_labels = e.labels.clone();
        let custom_name = raw_labels.remove("hc.name").or_else(|| persisted.name.clone());
        if custom_name != persisted.name {
            if let Some(p) = st.registry.terminals.entries.get_mut(&e.sid) {
                p.name = custom_name.clone();
                dirty_reg = true;
            }
        }
        let labels = synthesize_labels(&e.sid, &raw_labels, synthesize);
        let (exit_code, signal, reason, finished_ms) = exit_fields(e.exited.as_ref(), e.alive);
        let default_cwd = persisted
            .cwd
            .clone()
            .or(e.cwd.clone())
            .unwrap_or_else(|| layout.root.to_string_lossy().into_owned());
        let default_argv = if !e.argv.is_empty() {
            e.argv.clone()
        } else if !persisted.argv.is_empty() {
            persisted.argv.clone()
        } else {
            vec!["sh".into()]
        };
        let created_ms = if e.created_at > 0 {
            e.created_at as u64
        } else {
            persisted.created_at.saturating_mul(1000)
        };

        if let Some(cur) = st.terminals.get_mut(&e.sid) {
            let was = cur.to_public_json();
            let was_alive = cur.alive;
            let old_labels = cur.labels.clone();
            let old_title = cur.title.clone();
            cur.name = custom_name;
            cur.labels = labels.clone();
            cur.holder_pid = e.pid;
            if !e.argv.is_empty() {
                cur.argv = e.argv;
            }
            if let Some(c) = e.cwd {
                cur.cwd = c;
            }
            if !was_alive && e.alive {
                cur.alive = true;
                cur.pid = Some(e.pid);
                cur.finished_ms = None;
                cur.exit_code = None;
                cur.signal = None;
                cur.reason = None;
                refresh_terminal_derived(cur, synthesize);
                if emit_events {
                    let sid = e.sid.clone();
                    let rec = st.terminals[&sid].to_public_json();
                    st.emit("created", &sid, rec, Some(was));
                }
                dirty_reg = true;
            } else {
                refresh_terminal_derived(cur, synthesize);
                let new_labels = cur.labels.clone();
                let new_title = cur.title.clone();
                let sid = e.sid.clone();
                if old_labels != new_labels && emit_events {
                    let lbl_json = json!({ "labels": new_labels });
                    st.emit("labels", &sid, lbl_json, Some(was.clone()));
                }
                if old_title != new_title && emit_events {
                    st.emit("title", &sid, json!({ "title": new_title }), None);
                }
                if was_alive && !e.alive {
                    handle_terminal_exit(&mut st, &sid, exit_code, signal, reason.unwrap_or_else(|| "exited".into()), finished_ms, emit_events);
                    dirty_reg = true;
                }
            }
        } else {
            let addr = format_addr(persisted.seq);
            let mut entry = TerminalEntry {
                sid: e.sid.clone(),
                session_id: persisted.session_id.clone(),
                seq: persisted.seq,
                address: addr,
                name: custom_name,
                keep: persisted.keep,
                alive: e.alive,
                pid: e.alive.then_some(e.pid),
                holder_pid: e.pid,
                created_ms,
                finished_ms,
                exit_code,
                signal,
                reason,
                cols: 80,
                rows: 24,
                raw_title: String::new(),
                title: String::new(),
                argv: default_argv,
                cwd: default_cwd,
                owner: persisted.owner.clone(),
                labels,
                status: None,
                status_ms: None,
                records: BTreeMap::new(),
                raw_records: Vec::new(),
                turn: persisted.turn,
                reported: false,
                foreground: None,
            };
            refresh_terminal_derived(&mut entry, synthesize);
            let sid = e.sid.clone();
            st.terminals.insert(sid.clone(), entry);
            if emit_events {
                let rec = st.terminals[&sid].to_public_json();
                st.emit("created", &sid, rec, None);
            }
        }

        if e.alive && !st.monitors.contains_key(&e.sid) {
            to_monitor.push(e.sid);
        }
    }

    if sync_herdr_app_doc(&mut st, emit_events) {
        dirty_reg = true;
    }

    if dirty_reg {
        st.registry.save_terminals();
    }
    drop(st);

    for sid in to_monitor {
        spawn_holder_monitor(gw.clone(), sid);
    }
}

pub fn handle_terminal_exit(
    st: &mut GatewayState,
    sid: &str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    reason: String,
    finished_ms: Option<u64>,
    emit_events: bool,
) {
    let Some(t) = st.terminals.get_mut(sid) else { return };
    if !t.alive {
        return;
    }
    let was_working = t.status.as_ref().is_some_and(|s| s.state == "working");
    let had_status = t.status.is_some() || !t.records.is_empty();
    t.alive = false;
    t.pid = None;
    t.foreground = None;
    t.exit_code = exit_code;
    t.signal = signal;
    t.reason = Some(reason.clone());
    let fin_ms = finished_ms.unwrap_or_else(|| now_ms().max(0) as u64);
    t.finished_ms = Some(fin_ms);
    t.status = None;
    t.records.clear();
    t.raw_records.clear();
    if had_status {
        t.status_ms = Some(fin_ms);
    }
    if was_working {
        t.turn += 1;
        if let Some(p) = st.registry.terminals.entries.get_mut(sid) {
            p.turn = t.turn;
        }
    }
    let reported = t.reported;
    let turn = t.turn;
    st.monitors.remove(sid);
    st.output_watchers.remove(sid);

    if emit_events {
        if had_status {
            st.emit(
                "status",
                sid,
                json!({
                    "status": Value::Null,
                    "records": {},
                    "reported": reported,
                }),
                None,
            );
        }
        if was_working {
            st.emit("turn", sid, json!({ "turn": turn, "state": Value::Null }), None);
        }
        st.emit(
            "finished",
            sid,
            json!({
                "reason": reason,
                "exit_code": exit_code,
                "signal": signal,
                "finished_ms": fin_ms,
            }),
            None,
        );
    }
}

/// 为每个活着的 holder 启动一条后台监视线程（`Role::Hc`，不收实时输出字节，只收 Welcome + Event + ForegroundIs）
pub fn spawn_holder_monitor(gw: SharedGateway, sid: String) {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ClientMsg>();
    let layout = {
        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        if st.monitors.contains_key(&sid) {
            return;
        }
        st.monitors.insert(sid.clone(), cmd_tx);
        st.layout.clone()
    };

    std::thread::Builder::new()
        .name(format!("hc-mon-{sid}"))
        .spawn(move || {
            run_holder_monitor(&gw, &layout, &sid, cmd_rx);
            let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
            st.monitors.remove(&sid);
            // 若 holder 已死，立刻触发一次状态收敛
            if layout.holder_alive(&sid).is_none() {
                let exited = layout.read_exit(&sid);
                let (code, sig, reason, fin_ms) = exit_fields(exited.as_ref(), false);
                handle_terminal_exit(
                    &mut st,
                    &sid,
                    code,
                    sig,
                    reason.unwrap_or_else(|| "holder_lost".into()),
                    fin_ms,
                    true,
                );
                let _ = sync_herdr_app_doc(&mut st, true);
                st.registry.save_terminals();
            }
        })
        .ok();
}

fn run_holder_monitor(
    gw: &SharedGateway,
    layout: &Layout,
    sid: &str,
    cmd_rx: std::sync::mpsc::Receiver<ClientMsg>,
) {
    let endpoint = layout
        .read_meta(sid)
        .map(|m| m.endpoint)
        .unwrap_or_else(|| layout.endpoint(sid));
    let Ok(stream) = ipc::connect(&endpoint) else { return };
    let Ok((mut reader, mut writer)) = stream.split() else { return };

    let hello = ClientMsg::Hello(Hello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        role: Role::Hc,
        client: "hc-monitor".into(),
    });
    if writer.write_all(&hello.encode()).is_err() {
        return;
    }
    // 握手后立即请求一次前台进程信息，使终端在首次查询时即具备完整的 foreground 与标题
    let _ = writer.write_all(&ClientMsg::Foreground { req: 1 }.encode());

    // 写线程：转发网关下发的探测命令（如 Foreground）
    std::thread::spawn(move || {
        while let Ok(msg) = cmd_rx.recv() {
            if writer.write_all(&msg.encode()).is_err() {
                break;
            }
        }
    });

    while let Ok(Some((kind, payload))) = read_frame(&mut reader) {
        let Ok(msg) = HolderMsg::decode(kind, payload) else { continue };
        match msg {
            HolderMsg::Welcome(w) => {
                if negotiate((PROTO_MIN, PROTO_MAX), (w.proto, w.proto)).is_none() {
                    break;
                }
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(t) = st.terminals.get_mut(sid) {
                    t.pid = Some(w.pid);
                    t.cols = w.cols.max(1);
                    t.rows = w.rows.max(1);
                }
            }
            HolderMsg::Status { records } => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                apply_status_records(&mut st, sid, records, true);
                if let Some(tx) = st.monitors.get(sid) {
                    let _ = tx.send(ClientMsg::Foreground { req: 1 });
                }
                let _ = sync_herdr_app_doc(&mut st, true);
            }
            HolderMsg::Event(ev) => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                let synthesize = st.registry.identity.synthesize_labels;
                match ev {
                    Event::Title { title } => {
                        let changed_title = if let Some(t) = st.terminals.get_mut(sid) {
                            let prev_title = t.title.clone();
                            t.raw_title = title;
                            refresh_terminal_derived(t, synthesize);
                            (t.title != prev_title).then(|| t.title.clone())
                        } else {
                            None
                        };
                        if let Some(new_title) = changed_title {
                            st.emit("title", sid, json!({ "title": new_title }), None);
                        }
                        if let Some(tx) = st.monitors.get(sid) {
                            let _ = tx.send(ClientMsg::Foreground { req: 1 });
                        }
                        let _ = sync_herdr_app_doc(&mut st, true);
                    }
                    Event::Size { cols, rows } => {
                        if let Some(t) = st.terminals.get_mut(sid)
                            && (t.cols != cols || t.rows != rows)
                        {
                            t.cols = cols;
                            t.rows = rows;
                            st.emit("size", sid, json!({ "cols": cols, "rows": rows }), None);
                        }
                    }
                    Event::VtDegraded | Event::VtRestarted { .. } | Event::Bell | Event::Prompt { .. } | Event::Unknown => {}
                }
            }
            HolderMsg::Exit(status) => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                let reason = if status.signal.is_some() && status.code.is_none() {
                    "killed".into()
                } else {
                    "exited".into()
                };
                handle_terminal_exit(&mut st, sid, status.code, status.signal, reason, None, true);
                let _ = sync_herdr_app_doc(&mut st, true);
                st.registry.save_terminals();
                break;
            }
            HolderMsg::ForegroundIs { name, pid, argv, cwd, .. } => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                let synthesize = st.registry.identity.synthesize_labels;
                let (changed, new_fg) = {
                    let Some(t) = st.terminals.get_mut(sid) else { continue };
                    let new_fg = name.map(|n| HcForeground {
                        pid: pid.unwrap_or(t.pid.unwrap_or(0)) as i32,
                        name: n.clone(),
                        argv: argv.unwrap_or_else(|| vec![n]),
                        cwd: cwd.unwrap_or_else(|| t.cwd.clone()),
                    });
                    if t.foreground != new_fg {
                        t.foreground = new_fg.clone();
                        refresh_terminal_derived(t, synthesize);
                        (true, new_fg)
                    } else {
                        (false, None)
                    }
                };
                if changed {
                    st.emit("foreground", sid, json!(new_fg), None);
                    let _ = sync_herdr_app_doc(&mut st, true);
                }
            }
            _ => {}
        }
    }
}

fn collect_node_terminals(node: &Value, out: &mut HashSet<String>) {
    match node.get("type").and_then(Value::as_str) {
        Some("block") => {
            if let Some(addr) = node
                .get("content")
                .and_then(|c| c.get("terminal"))
                .and_then(Value::as_str)
            {
                out.insert(addr.to_string());
            }
        }
        Some("row" | "column" | "stack") => {
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                for child in children {
                    collect_node_terminals(child, out);
                }
            }
        }
        Some("split") => {
            if let Some(first) = node.get("first") {
                collect_node_terminals(first, out);
            }
            if let Some(second) = node.get("second") {
                collect_node_terminals(second, out);
            }
        }
        _ => {}
    }
}

fn prune_node_terminals(node: &Value, valid_addrs: &HashSet<String>) -> Option<Value> {
    match node.get("type").and_then(Value::as_str) {
        Some("block") => {
            if let Some(addr) = node
                .get("content")
                .and_then(|c| c.get("terminal"))
                .and_then(Value::as_str)
            {
                if valid_addrs.contains(addr) {
                    Some(node.clone())
                } else {
                    None
                }
            } else {
                // 非终端面板（如 markdown/image/canvas）原样保留
                Some(node.clone())
            }
        }
        Some("row" | "column" | "stack") => {
            let children = node.get("children").and_then(Value::as_array)?;
            let kept: Vec<Value> = children
                .iter()
                .filter_map(|c| prune_node_terminals(c, valid_addrs))
                .collect();
            if kept.is_empty() {
                None
            } else {
                let mut out = node.clone();
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("children".into(), Value::Array(kept));
                }
                Some(out)
            }
        }
        Some("split") => {
            let first = node.get("first").and_then(|n| prune_node_terminals(n, valid_addrs));
            let second = node.get("second").and_then(|n| prune_node_terminals(n, valid_addrs));
            match (first, second) {
                (Some(f), Some(s)) => {
                    let mut out = node.clone();
                    if let Some(obj) = out.as_object_mut() {
                        obj.insert("first".into(), f);
                        obj.insert("second".into(), s);
                    }
                    Some(out)
                }
                (Some(f), None) => Some(f),
                (None, Some(s)) => Some(s),
                (None, None) => None,
            }
        }
        _ => None,
    }
}

fn ensure_container_root(tab_id: &str, node: Value) -> Value {
    if node.get("type").and_then(Value::as_str) == Some("block") {
        json!({
            "id": tab_id,
            "type": "row",
            "children": [node],
        })
    } else {
        node
    }
}

struct FalconSpaceGroup {
    space_id: String,
    space_name: String,
    sort_key: String,
    folder: String,
    terminals: Vec<FalconTabItem>,
}

struct FalconTabItem {
    sid: String,
    address: String,
    tab_id: String,
    tab_name: String,
}

fn build_normalized_herdr_app_doc(st: &mut GatewayState) -> (Option<Value>, bool) {
    let mut dirty_reg = false;
    let mut sids_to_keep = Vec::new();
    let mut by_space: BTreeMap<String, Vec<(u64, u32, FalconTabItem, String, String, String)>> = BTreeMap::new();

    for t in st.terminals.values() {
        if !t.alive {
            continue;
        }
        let Some(space_id) = t.labels.get("herdr.space").filter(|s| s.starts_with("falcon.")).cloned() else {
            continue;
        };
        if !t.keep {
            sids_to_keep.push(t.sid.clone());
        }
        let space_name = t
            .labels
            .get("falcon.space_name")
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| {
                t.labels
                    .get("where")
                    .and_then(|w| w.split(" / ").next())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
            .or_else(|| t.labels.get("falcon.project_name").filter(|s| !s.is_empty()).cloned())
            .unwrap_or_else(|| space_id.strip_prefix("falcon.").unwrap_or(&space_id).to_string());
        let sort_key = t
            .labels
            .get("falcon.sort_key")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("s9/{space_name}"));
        let tab_id = t
            .labels
            .get("herdr.tab")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("falcon.{}", t.sid));
        let tab_name = effective_session_name(t);
        let item = FalconTabItem {
            sid: t.sid.clone(),
            address: t.address.clone(),
            tab_id,
            tab_name,
        };
        by_space
            .entry(space_id)
            .or_default()
            .push((t.created_ms, t.seq, item, space_name, sort_key, t.cwd.clone()));
    }

    for sid in sids_to_keep {
        if let Some(t) = st.terminals.get_mut(&sid) {
            t.keep = true;
        }
        if let Some(p) = st.registry.terminals.entries.get_mut(&sid)
            && !p.keep
        {
            p.keep = true;
            dirty_reg = true;
        }
    }

    let existing_doc = st.registry.docs.get("herdr-app").map(|r| r.data.clone());
    let had_falcon_spaces = existing_doc
        .as_ref()
        .and_then(|d| d.get("spaces"))
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|sp| {
                sp.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("falcon."))
            })
        });

    if by_space.is_empty() && !had_falcon_spaces {
        return (None, dirty_reg);
    }

    let mut groups: Vec<FalconSpaceGroup> = by_space
        .into_iter()
        .map(|(space_id, mut list)| {
            list.sort_by_key(|(created_ms, seq, _, _, _, _)| (*created_ms, *seq));
            let space_name = list
                .iter()
                .find_map(|(_, _, _, sn, _, _)| (!sn.is_empty()).then(|| sn.clone()))
                .unwrap_or_else(|| space_id.clone());
            let sort_key = list
                .iter()
                .find_map(|(_, _, _, _, sk, _)| (!sk.is_empty()).then(|| sk.clone()))
                .unwrap_or_else(|| format!("s9/{space_name}"));
            let folder = list
                .iter()
                .find_map(|(_, _, _, _, _, cwd)| (!cwd.is_empty()).then(|| cwd.clone()))
                .unwrap_or_default();
            let terminals = list.into_iter().map(|(_, _, item, _, _, _)| item).collect();
            FalconSpaceGroup {
                space_id,
                space_name,
                sort_key,
                folder,
                terminals,
            }
        })
        .collect();

    groups.sort_by(|a, b| {
        a.sort_key
            .cmp(&b.sort_key)
            .then_with(|| a.space_name.cmp(&b.space_name))
            .then_with(|| a.space_id.cmp(&b.space_id))
    });

    let mut existing_falcon_spaces: HashMap<String, Value> = HashMap::new();
    let mut non_falcon_spaces: Vec<Value> = Vec::new();
    if let Some(arr) = existing_doc
        .as_ref()
        .and_then(|d| d.get("spaces"))
        .and_then(Value::as_array)
    {
        for sp in arr {
            let id = sp.get("id").and_then(Value::as_str).unwrap_or_default();
            if id.starts_with("falcon.") {
                existing_falcon_spaces.insert(id.to_string(), sp.clone());
            } else {
                non_falcon_spaces.push(sp.clone());
            }
        }
    }

    let mut new_spaces: Vec<Value> = Vec::with_capacity(groups.len() + non_falcon_spaces.len());
    for g in groups {
        let valid_addrs: HashSet<String> = g.terminals.iter().map(|t| t.address.clone()).collect();
        let tab_names_by_id: HashMap<&str, &str> = g
            .terminals
            .iter()
            .map(|t| (t.tab_id.as_str(), t.tab_name.as_str()))
            .collect();
        let tab_names_by_addr: HashMap<&str, &str> = g
            .terminals
            .iter()
            .map(|t| (t.address.as_str(), t.tab_name.as_str()))
            .collect();

        let mut tabs: Vec<Value> = Vec::new();
        let mut seen_addrs: HashSet<String> = HashSet::new();

        if let Some(old_sp) = existing_falcon_spaces.remove(&g.space_id)
            && let Some(old_tabs) = old_sp.get("tabs").and_then(Value::as_array)
        {
            for old_tab in old_tabs {
                let Some(root) = old_tab.get("root") else { continue };
                let Some(pruned_root) = prune_node_terminals(root, &valid_addrs) else {
                    continue;
                };
                let mut tab_addrs = HashSet::new();
                collect_node_terminals(&pruned_root, &mut tab_addrs);
                seen_addrs.extend(tab_addrs.iter().cloned());
                let mut tab_obj = old_tab.clone();
                if let Some(obj) = tab_obj.as_object_mut() {
                    let tid = obj.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                    let container_root = ensure_container_root(&tid, pruned_root);
                    obj.insert("root".into(), container_root);
                    if let Some(&expected_name) = tab_names_by_id.get(tid.as_str()) {
                        if !expected_name.is_empty() {
                            obj.insert("name".into(), json!(expected_name));
                        }
                    } else if tab_addrs.len() == 1
                        && let Some(addr) = tab_addrs.iter().next()
                        && let Some(&expected_name) = tab_names_by_addr.get(addr.as_str())
                        && !expected_name.is_empty()
                    {
                        obj.insert("name".into(), json!(expected_name));
                    }
                }
                tabs.push(tab_obj);
            }
        }

        for item in &g.terminals {
            if seen_addrs.contains(&item.address) {
                continue;
            }
            seen_addrs.insert(item.address.clone());
            tabs.push(json!({
                "id": item.tab_id,
                "name": item.tab_name,
                "root": {
                    "id": item.tab_id,
                    "type": "row",
                    "children": [
                        {
                            "id": format!("p_{}", item.sid),
                            "type": "block",
                            "content": {
                                "terminal": item.address,
                            }
                        }
                    ]
                }
            }));
        }

        new_spaces.push(json!({
            "id": g.space_id,
            "name": g.space_name,
            "folder": g.folder,
            "tabs": tabs,
        }));
    }

    new_spaces.extend(non_falcon_spaces);
    (Some(json!({ "spaces": new_spaces })), dirty_reg)
}

/// 将 Falcon 左侧栏项目结构（名称与排序）同步至 `herdr-app` 共享文档，返回是否修改了 `terminals` 持久化属性
pub fn sync_herdr_app_doc(st: &mut GatewayState, emit_events: bool) -> bool {
    let (Some(new_data), dirty_reg) = build_normalized_herdr_app_doc(st) else {
        return false;
    };
    let unchanged = st
        .registry
        .docs
        .get("herdr-app")
        .is_some_and(|r| r.data == new_data);
    if !unchanged {
        let next_ver = st
            .registry
            .docs
            .get("herdr-app")
            .map(|r| r.version + 1)
            .unwrap_or(1);
        st.registry.docs.insert(
            "herdr-app".into(),
            crate::hc::registry::DocRecord {
                data: new_data,
                version: next_ver,
                updated_at: (now_ms().max(0) as u64) / 1000,
            },
        );
        st.registry.save_docs();
        if emit_events {
            st.emit_doc("herdr-app", next_ver);
        }
    }
    dirty_reg
}

/// 在 `doc.set("herdr-app")` 写入后原地修正 `falcon.*` 空间的标题与顺序（保持同一版本号以免打断客户端乐观锁）
pub fn normalize_herdr_app_doc_in_place(st: &mut GatewayState) {
    let (Some(new_data), dirty_reg) = build_normalized_herdr_app_doc(st) else {
        return;
    };
    if dirty_reg {
        st.registry.save_terminals();
    }
    if let Some(rec) = st.registry.docs.get_mut("herdr-app")
        && rec.data != new_data
    {
        rec.data = new_data;
        st.registry.save_docs();
    }
}

fn apply_status_records(
    st: &mut GatewayState,
    sid: &str,
    records: Vec<ProgramStatusRecord>,
    emit: bool,
) {
    let synthesize = st.registry.identity.synthesize_labels;
    let Some(t) = st.terminals.get_mut(sid) else { return };
    let was_working = t.status.as_ref().is_some_and(|s| s.state == "working");
    let was_word = t.status.as_ref().map(|s| s.state.clone());
    let (new_root, new_map) = convert_program_records(&records);
    let new_word = new_root.as_ref().map(|s| s.state.clone());
    let now_working = new_word.as_deref() == Some("working");

    if !records.is_empty() {
        t.reported = true;
    }
    let prev_status = t.status.clone();
    let prev_records = t.records.clone();
    t.status = new_root;
    t.records = new_map.clone();
    t.raw_records = records;
    refresh_terminal_derived(t, synthesize);

    let effective_root = t.status.clone();
    let changed = prev_status != effective_root || prev_records != new_map;
    if was_word != new_word && (new_word.is_some() || was_word.is_some()) {
        t.status_ms = Some(now_ms().max(0) as u64);
    }

    let turn_ended = was_working && !now_working;
    if turn_ended {
        t.turn += 1;
        if let Some(p) = st.registry.terminals.entries.get_mut(sid) {
            p.turn = t.turn;
        }
    }
    let reported = t.reported;
    let turn = t.turn;
    if turn_ended {
        st.registry.save_terminals();
    }

    if emit && changed {
        st.emit(
            "status",
            sid,
            json!({
                "status": effective_root,
                "records": new_map,
                "reported": reported,
            }),
            None,
        );
    }
    if emit && turn_ended {
        st.emit(
            "turn",
            sid,
            json!({
                "turn": turn,
                "state": new_word,
            }),
            None,
        );
    }
}

/// 后台周期任务：每秒扫描一次目录，且当有订阅者时向所有活着的监视连接发 `Foreground` 探测
pub async fn background_loop(gw: SharedGateway) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut fg_req = 1u32;
    loop {
        tick.tick().await;
        sync_directory(&gw, true);
        let senders: Vec<std::sync::mpsc::Sender<ClientMsg>> = {
            let st = gw.lock().unwrap_or_else(|p| p.into_inner());
            if st.subscriber_count == 0 {
                Vec::new()
            } else {
                st.monitors.values().cloned().collect()
            }
        };
        if !senders.is_empty() {
            fg_req = fg_req.wrapping_add(1);
            for tx in senders {
                let _ = tx.send(ClientMsg::Foreground { req: fg_req });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_labels_includes_remote_host_name() {
        let mut raw = BTreeMap::new();
        raw.insert("falcon.project".into(), "proj-1".into());
        raw.insert("falcon.project_name".into(), "api".into());
        raw.insert("falcon.session".into(), "deploy".into());
        raw.insert("falcon.remote".into(), "true".into());
        raw.insert("falcon.host_name".into(), "prod-eu".into());

        let out = synthesize_labels("0123456789abcdef", &raw, true);
        assert_eq!(out.get("where").map(String::as_str), Some("prod-eu · api / deploy"));
        assert_eq!(out.get("herdr.space").map(String::as_str), Some("falcon.proj-1"));
        assert_eq!(out.get("herdr.tab").map(String::as_str), Some("falcon.0123456789abcdef"));

        let mut standalone_raw = BTreeMap::new();
        standalone_raw.insert("falcon.project".into(), "standalone:host:h1".into());
        standalone_raw.insert("falcon.standalone".into(), "true".into());
        standalone_raw.insert("falcon.host_name".into(), "dev-box".into());
        let out2 = synthesize_labels("fedcba9876543210", &standalone_raw, true);
        assert_eq!(out2.get("where").map(String::as_str), Some("dev-box · 无项目"));
        assert_eq!(out2.get("falcon.space_name").map(String::as_str), Some("dev-box · 无项目"));
    }

    #[test]
    fn sync_herdr_app_doc_orders_and_names_spaces_by_sidebar_structure() {
        let tmp = std::env::temp_dir().join(format!("falcon-hc-doc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let layout = Layout::new(tmp.clone());
        let gw = GatewayState::new(layout).unwrap();
        let mut st = gw.lock().unwrap();

        let make_entry = |sid: &str, seq: u32, addr: &str, cwd: &str, labels: &[(&str, &str)]| -> TerminalEntry {
            let mut raw = BTreeMap::new();
            for (k, v) in labels {
                raw.insert((*k).to_string(), (*v).to_string());
            }
            TerminalEntry {
                sid: sid.into(),
                session_id: sid.into(),
                seq,
                address: addr.into(),
                name: None,
                keep: false,
                alive: true,
                pid: Some(100 + seq),
                holder_pid: 100 + seq,
                created_ms: (seq as u64) * 1000,
                finished_ms: None,
                exit_code: None,
                signal: None,
                reason: None,
                cols: 80,
                rows: 24,
                raw_title: String::new(),
                title: String::new(),
                argv: vec!["zsh".into()],
                cwd: cwd.into(),
                owner: "falcon".into(),
                labels: synthesize_labels(sid, &raw, true),
                status: None,
                status_ms: None,
                records: BTreeMap::new(),
                raw_records: Vec::new(),
                turn: 0,
                reported: false,
                foreground: None,
            }
        };

        // 1. 远端主机虚拟项目（磁盘目录是 UUID，左侧栏名为 linux · script，排在远端组 s1）
        st.terminals.insert(
            "04f537b6b51b4fa6".into(),
            make_entry(
                "04f537b6b51b4fa6",
                1,
                "t1",
                "/home/fay/.falcon/projects/04f537b6-b51b-4fa6-a61f-ab4169a5db78",
                &[
                    ("falcon.project", "04f537b6-b51b-4fa6-a61f-ab4169a5db78"),
                    ("falcon.project_name", "script"),
                    ("falcon.host_name", "linux"),
                    ("falcon.space_name", "linux · script"),
                    ("falcon.sort_key", "s1:0000/p1:0001:0:0000"),
                    ("falcon.session", "Claude"),
                ],
            ),
        );
        // 2. 本机项目 mojito（排在本机组 s0）
        st.terminals.insert(
            "a0920c4e1160590d".into(),
            make_entry(
                "a0920c4e1160590d",
                2,
                "t2",
                "/Volumes/faye/Code/mojito/native",
                &[
                    ("falcon.project", "17a3ff7a-2fd6-4090-beab-03179a82b574"),
                    ("falcon.project_name", "mojito"),
                    ("falcon.space_name", "mojito"),
                    ("falcon.sort_key", "s0:0000/p1:0000:0:0000"),
                    ("falcon.session", "终端 1"),
                ],
            ),
        );

        sync_herdr_app_doc(&mut st, false);
        let doc = &st.registry.docs["herdr-app"].data;
        let spaces = doc["spaces"].as_array().unwrap();
        assert_eq!(spaces.len(), 2);
        // 本机 mojito 排第一，远端 linux · script 排第二；且显示名为项目名而非实际目录名
        assert_eq!(spaces[0]["id"].as_str(), Some("falcon.17a3ff7a-2fd6-4090-beab-03179a82b574"));
        assert_eq!(spaces[0]["name"].as_str(), Some("mojito"));
        assert_eq!(spaces[0]["tabs"][0]["name"].as_str(), Some("终端 1"));
        assert_eq!(spaces[0]["tabs"][0]["root"]["type"].as_str(), Some("row"));
        assert_eq!(
            spaces[0]["tabs"][0]["root"]["children"][0]["content"]["terminal"].as_str(),
            Some("t2")
        );
        assert_eq!(spaces[1]["id"].as_str(), Some("falcon.04f537b6-b51b-4fa6-a61f-ab4169a5db78"));
        assert_eq!(spaces[1]["name"].as_str(), Some("linux · script"));
        assert_eq!(spaces[1]["tabs"][0]["name"].as_str(), Some("Claude"));
        drop(st);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn clean_terminal_title_strips_spinners_uuid_suffix_and_self_name() {
        let cwd = "/home/fay/.falcon/projects/04f537b6-b51b-4fa6-a61f-ab4169a5db78";
        let proj_id = Some("04f537b6-b51b-4fa6-a61f-ab4169a5db78");
        assert_eq!(
            clean_terminal_title(
                "查看并汇总三个 Bug | 04f537b6-b51b-4fa6-a6...",
                cwd,
                proj_id,
                Some("codex")
            ),
            "查看并汇总三个 Bug"
        );
        assert_eq!(
            clean_terminal_title("✳ Claude Code", cwd, proj_id, Some("claude-code")),
            ""
        );
        assert_eq!(
            clean_terminal_title("◐ 修复登录问题", cwd, proj_id, Some("claude-code")),
            "修复登录问题"
        );
    }
}
