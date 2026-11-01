//! 会话镜像与事件总线（设计文档 3.4 节）：
//! - 周期扫描 `<root>/sessiond/sessions/`，把 falcon 会话映射为 `herdr-core` 的 `Terminal` JSON；
//! - 为每个活着的 holder 维护一条后台监视连接（`Role::Hc`），实时接收 `Event::Title`、
//!   `Event::ProgramStatus`、`Event::Size`、`Event::Exit`，并每当根状态离开 `working` 时递增 `turn`；
//! - 有事件订阅者时按 1 Hz 向 holder 轮询 `ClientMsg::Foreground`，发现前台进程/目录变化即发 `foreground` 事件；
//! - 维护单调递增 `seq` 与事件广播，驱动 `events.subscribe` 与 `terminal.wait`。

use std::collections::{BTreeMap, HashMap};
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

/// 合成 `herdr.space` / `herdr.tab` / `where` 标签（当 `synthesize_labels` 开启时）
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
    out.entry("herdr.space".into())
        .or_insert_with(|| format!("falcon.{project_id}"));
    out.entry("herdr.tab".into())
        .or_insert_with(|| format!("falcon.{sid}"));
    if !out.contains_key("where") {
        let prefix = match (host_name, project_name) {
            (Some(h), Some(p)) => Some(format!("{h} · {p}")),
            (Some(h), None) => Some(h),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        };
        match (prefix, session_name) {
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
            cur.name = custom_name;
            cur.labels = labels.clone();
            cur.holder_pid = e.pid;
            if !e.argv.is_empty() {
                cur.argv = e.argv;
            }
            if let Some(c) = e.cwd {
                cur.cwd = c;
            }
            if old_labels != cur.labels && emit_events {
                let lbl_json = json!({ "labels": cur.labels });
                let sid = e.sid.clone();
                st.emit("labels", &sid, lbl_json, Some(was.clone()));
            }
            if was_alive && !e.alive {
                let sid = e.sid.clone();
                handle_terminal_exit(&mut st, &sid, exit_code, signal, reason.unwrap_or_else(|| "exited".into()), finished_ms, emit_events);
                dirty_reg = true;
            }
        } else {
            let addr = format_addr(persisted.seq);
            let entry = TerminalEntry {
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
            }
            HolderMsg::Event(ev) => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                match ev {
                    Event::Title { title } => {
                        if let Some(t) = st.terminals.get_mut(sid)
                            && t.title != title
                        {
                            t.title = title.clone();
                            st.emit("title", sid, json!({ "title": title }), None);
                        }
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
                st.registry.save_terminals();
                break;
            }
            HolderMsg::ForegroundIs { name, pid, argv, cwd, .. } => {
                let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
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
                        (true, new_fg)
                    } else {
                        (false, None)
                    }
                };
                if changed {
                    st.emit("foreground", sid, json!(new_fg), None);
                }
            }
            _ => {}
        }
    }
}

fn apply_status_records(
    st: &mut GatewayState,
    sid: &str,
    records: Vec<ProgramStatusRecord>,
    emit: bool,
) {
    let Some(t) = st.terminals.get_mut(sid) else { return };
    let was_working = t.status.as_ref().is_some_and(|s| s.state == "working");
    let was_word = t.status.as_ref().map(|s| s.state.clone());
    let (new_root, new_map) = convert_program_records(&records);
    let new_word = new_root.as_ref().map(|s| s.state.clone());
    let now_working = new_word.as_deref() == Some("working");

    if !records.is_empty() {
        t.reported = true;
    }
    let changed = t.status != new_root || t.records != new_map;
    if was_word != new_word && (new_word.is_some() || was_word.is_some()) {
        t.status_ms = Some(now_ms().max(0) as u64);
    }
    t.status = new_root.clone();
    t.records = new_map.clone();
    t.raw_records = records;

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
                "status": new_root,
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
        assert_eq!(out2.get("where").map(String::as_str), Some("dev-box"));
    }
}
