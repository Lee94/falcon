//! `hc` JSON-RPC 2.0 方法处理器（一、二、三期全量方法，设计文档 3.6 节）：
//! - 一期：`hello`、`server.status`、`rpc.discover`、`terminal.list`、`terminal.get`、
//!   `terminal.read`、`terminal.attach`、`terminal.kill`、`terminal.rm`、`events.subscribe`、`doc.get`、`doc.set`
//! - 二期：`terminal.send`、`terminal.keys`、`terminal.interrupt`、`terminal.wait`、`terminal.label`、`terminal.keep`
//! - 三期：`terminal.run`、`terminal.put`、`templates.list`

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::{Duration, Instant};

use base64::Engine as _;
use falcon_proto::sessiond::{
    ClientMsg, ExportFormat, Hello, HolderMsg, ModesAre, PROTO_MAX, PROTO_MIN, Role, SpawnSpec, negotiate, read_frame,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::hc::attach::{AttachOptions, ViewerFrame, spawn_attach_session};
use crate::hc::mirror::{EventFilter, SharedGateway, sync_directory, synthesize_labels};
use crate::hc::registry::{format_addr, gen_uuid_v4, valid_terminal_name};
use crate::hc::rpc::{HC_GENERATION, RpcError, discover_schema, hc_features, hc_version_string};
use crate::ipc;
use crate::layout::Layout;
use crate::spawn;

/// `hello` 返回的完整方法名列表
pub fn supported_methods() -> Vec<&'static str> {
    vec![
        "hello",
        "server.status",
        "rpc.discover",
        "terminal.run",
        "templates.list",
        "terminal.list",
        "terminal.get",
        "terminal.kill",
        "terminal.rm",
        "terminal.keep",
        "terminal.label",
        "terminal.read",
        "terminal.attach",
        "terminal.send",
        "terminal.wait",
        "terminal.keys",
        "terminal.interrupt",
        "terminal.put",
        "doc.get",
        "doc.set",
        "events.subscribe",
    ]
}

/// 单条客户端连接的上下文（供 `terminal.attach` 和 `events.subscribe` 注册通道）
pub struct ConnContext {
    pub framed: bool,
    pub next_channel: std::sync::Arc<std::sync::atomic::AtomicU32>,
    pub frame_out_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    pub channels: std::sync::Arc<std::sync::Mutex<BTreeMap<u32, mpsc::UnboundedSender<ViewerFrame>>>>,
    pub plain_event_filter: std::sync::Arc<std::sync::Mutex<Option<EventFilter>>>,
}

impl ConnContext {
    pub fn alloc_channel(&self) -> u32 {
        self.next_channel.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

pub async fn dispatch(
    gw: &SharedGateway,
    ctx: &ConnContext,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    match method {
        "hello" => handle_hello(gw, params),
        "server.status" => handle_server_status(gw),
        "server.stop" | "server.restart" => Err(RpcError::refused(
            "falcon-sessiond gateway keeps sessions running across restarts",
            Some("use `falcon-sessiond kill <sid>` or `hc kill <target>` to stop individual sessions"),
        )),
        "rpc.discover" => Ok(discover_schema()),
        "templates.list" => Ok(json!({ "templates": [] })),
        "terminal.list" => handle_terminal_list(gw, params),
        "terminal.get" => handle_terminal_get(gw, params),
        "terminal.read" => handle_terminal_read(gw, params).await,
        "terminal.attach" => handle_terminal_attach(gw, ctx, params),
        "terminal.kill" => handle_terminal_kill(gw, params).await,
        "terminal.rm" => handle_terminal_rm(gw, params).await,
        "terminal.keep" => handle_terminal_keep(gw, params),
        "terminal.label" => handle_terminal_label(gw, params),
        "terminal.send" => handle_terminal_send(gw, params).await,
        "terminal.keys" => handle_terminal_keys(gw, params).await,
        "terminal.interrupt" => handle_terminal_interrupt(gw, params).await,
        "terminal.wait" => handle_terminal_wait(gw, params).await,
        "terminal.run" => handle_terminal_run(gw, params).await,
        "terminal.put" => handle_terminal_put(gw, params),
        "doc.get" => handle_doc_get(gw, params),
        "doc.set" => handle_doc_set(gw, params),
        "events.subscribe" => handle_events_subscribe(gw, ctx, params),
        _ => Err(RpcError::unknown_method(method)),
    }
}

fn handle_hello(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    // 绝不因 generation 报错（防止触发远端 hc 覆盖安装流程）
    let framed = params.get("framed").and_then(Value::as_bool).unwrap_or(false);
    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    Ok(json!({
        "generation": HC_GENERATION,
        "version": hc_version_string(),
        "core": {
            "id": st.registry.identity.core_id,
            "name": st.registry.identity.name,
        },
        "framed": framed,
        "methods": supported_methods(),
        "features": hc_features(),
    }))
}

fn handle_server_status(gw: &SharedGateway) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let live = st.terminals.values().filter(|t| t.alive).count();
    let finished = st.terminals.len().saturating_sub(live);
    let holders: Vec<Value> = st
        .terminals
        .values()
        .filter(|t| t.alive)
        .map(|t| {
            json!({
                "pid": t.holder_pid,
                "instance": t.sid,
                "current": true,
                "terminals": 1,
            })
        })
        .collect();
    Ok(json!({
        "core": {
            "id": st.registry.identity.core_id,
            "name": st.registry.identity.name,
        },
        "version": hc_version_string(),
        "pid": std::process::id(),
        "uptime_ms": st.started_at.elapsed().as_millis() as u64,
        "holder": holders.first().cloned(),
        "holders": holders,
        "live": live,
        "finished": finished,
        "installed_by": "falcon",
    }))
}

fn parse_label_filter(v: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    match v {
        Some(Value::Object(map)) => {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    out.insert(k.clone(), s.to_string());
                } else if val.is_null() {
                    out.insert(k.clone(), String::new());
                }
            }
        }
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if let Some((k, v)) = s.split_once('=') {
                        out.insert(k.to_string(), v.to_string());
                    } else if let Some(k) = s.strip_suffix('-') {
                        out.insert(k.to_string(), String::new());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn handle_terminal_list(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
    let filter_labels = parse_label_filter(params.get("labels"));

    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let mut entries: Vec<_> = st
        .terminals
        .values()
        .filter(|t| filter_labels.iter().all(|(k, v)| t.labels.get(k) == Some(v)))
        .collect();
    entries.sort_by_key(|t| t.seq);

    let hidden_finished = if all {
        0
    } else {
        entries.iter().filter(|t| !t.alive && !t.keep).count()
    };
    let terminals: Vec<Value> = entries
        .into_iter()
        .filter(|t| all || t.alive || t.keep)
        .map(|t| t.to_public_json())
        .collect();

    Ok(json!({
        "terminals": terminals,
        "hidden_finished": hidden_finished,
    }))
}

fn target_param(params: &Value) -> Result<String, RpcError> {
    params
        .get("terminal")
        .or_else(|| params.get("target"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| RpcError::invalid_params("missing `terminal` parameter"))
}

fn handle_terminal_get(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let sid = st.resolve_sid(&target)?;
    let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
    Ok(t.to_public_json())
}

async fn handle_terminal_read(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let lines = params.get("lines").and_then(Value::as_u64).unwrap_or(0) as usize;
    let (sid, alive, rows, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        (sid, t.alive, t.rows.max(1) as usize, st.layout.clone())
    };

    if alive {
        let layout_c = layout.clone();
        let sid_c = sid.clone();
        let exported = tokio::task::spawn_blocking(move || export_plain_from_holder(&layout_c, &sid_c))
            .await
            .ok()
            .flatten();
        if let Some((raw_text, _modes)) = exported {
            let all_lines: Vec<&str> = raw_text.trim_end_matches('\n').lines().collect();
            let keep = rows.saturating_add(lines);
            let start = all_lines.len().saturating_sub(keep);
            let text = all_lines[start..].join("\n");
            return Ok(json!({
                "text": text,
                "complete": true,
                "how": if lines == 0 { "screen" } else { "history" },
            }));
        }
    }

    // 已退出或连接失败：读 `<sid>.screen.txt`
    match layout.read_screen(&sid) {
        Some(saved) => {
            let capped_lines = lines.min(1000);
            let all_lines: Vec<&str> = saved.trim_end_matches('\n').lines().collect();
            let keep = rows.saturating_add(capped_lines);
            let start = all_lines.len().saturating_sub(keep);
            let text = all_lines[start..].join("\n");
            Ok(json!({
                "text": text,
                "complete": lines <= 1000,
                "how": if lines == 0 { "screen" } else { "history" },
            }))
        }
        None => Ok(json!({
            "text": "",
            "complete": false,
            "how": "screen",
        })),
    }
}

fn handle_terminal_attach(
    gw: &SharedGateway,
    ctx: &ConnContext,
    params: Value,
) -> Result<Value, RpcError> {
    if !ctx.framed {
        return Err(RpcError::needs_framed());
    }
    let Some(frame_out_tx) = ctx.frame_out_tx.clone() else {
        return Err(RpcError::needs_framed());
    };
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let observe = params.get("observe").and_then(Value::as_bool).unwrap_or(false);
    let history = params
        .get("history")
        .and_then(Value::as_u64)
        .map(|h| h.min(u32::MAX as u64) as u32);
    let cols = params.get("cols").and_then(Value::as_u64).unwrap_or(80) as u16;
    let rows = params.get("rows").and_then(Value::as_u64).unwrap_or(24) as u16;
    let resumed = params.get("resumed").and_then(Value::as_bool).unwrap_or(false);
    let caller = params.get("caller").and_then(Value::as_str);

    let (sid, endpoint, pub_json) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        if let Some(c) = caller
            && (c == t.address || c == t.sid)
        {
            return Err(RpcError::refused(
                format!("{} is the terminal you are in", t.address),
                Some("attach from another terminal"),
            ));
        }
        if !t.alive {
            return Err(RpcError::not_alive(&t.address));
        }
        let endpoint = st
            .layout
            .read_meta(&sid)
            .map(|m| m.endpoint)
            .unwrap_or_else(|| st.layout.endpoint(&sid));
        (sid, endpoint, t.to_public_json())
    };

    let channel = ctx.alloc_channel();
    let v_tx = spawn_attach_session(
        gw.clone(),
        AttachOptions {
            sid,
            endpoint,
            channel,
            observe,
            history,
            cols,
            rows,
            resumed,
        },
        frame_out_tx,
    );
    ctx.channels.lock().unwrap_or_else(|p| p.into_inner()).insert(channel, v_tx);

    Ok(json!({
        "channel": channel,
        "terminal": pub_json,
    }))
}

async fn handle_terminal_kill(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let (sid, alive, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        (sid, t.alive, st.layout.clone())
    };
    if alive {
        let layout_c = layout.clone();
        let sid_c = sid.clone();
        let _ = tokio::task::spawn_blocking(move || crate::attach::kill(&layout_c, &sid_c)).await;
        sync_directory(gw, true);
    }
    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
    Ok(t.to_public_json())
}

async fn handle_terminal_rm(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    let (sid, alive, address, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        (sid, t.alive, t.address.clone(), st.layout.clone())
    };
    if alive {
        if !force {
            return Err(RpcError::still_live(&address));
        }
        let layout_c = layout.clone();
        let sid_c = sid.clone();
        let _ = tokio::task::spawn_blocking(move || crate::attach::kill(&layout_c, &sid_c)).await;
    }

    let _ = std::fs::remove_file(layout.exit_path(&sid));
    let _ = std::fs::remove_file(layout.labels_path(&sid));
    let _ = std::fs::remove_file(layout.screen_path(&sid));
    let _ = std::fs::remove_file(layout.meta_path(&sid));

    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(p) = st.registry.terminals.entries.get_mut(&sid) {
        p.removed = true;
    }
    st.registry.save_terminals();
    if let Some(removed_entry) = st.terminals.remove(&sid) {
        st.emit_removed(&address, removed_entry.to_public_json());
    }
    Ok(json!({ "removed": address }))
}

fn handle_terminal_keep(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let keep = params.get("keep").and_then(Value::as_bool).unwrap_or(true);
    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let sid = st.resolve_sid(&target)?;
    let t = st.terminals.get_mut(&sid).ok_or_else(|| RpcError::not_found(&target))?;
    t.keep = keep;
    let pub_json = t.to_public_json();
    if let Some(p) = st.registry.terminals.entries.get_mut(&sid) {
        p.keep = keep;
    }
    st.registry.save_terminals();
    st.emit("keep", &sid, json!({ "keep": keep }), None);
    Ok(pub_json)
}

fn handle_terminal_label(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let mut changes = parse_label_filter(params.get("labels"));
    for (k, v) in parse_label_filter(params.get("set")) {
        changes.insert(k, v);
    }
    if let Some(Value::Array(unset)) = params.get("unset") {
        for item in unset {
            if let Some(k) = item.as_str() {
                changes.insert(k.to_string(), String::new());
            }
        }
    }

    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let sid = st.resolve_sid(&target)?;
    let layout = st.layout.clone();
    let synthesize = st.registry.identity.synthesize_labels;

    let mut disk_labels = layout.read_labels(&sid);
    for (k, v) in changes {
        if k.is_empty() || k.contains('=') {
            return Err(RpcError::invalid_params(format!("bad label key {k:?}")));
        }
        if v.is_empty() {
            disk_labels.remove(&k);
        } else {
            disk_labels.insert(k, v);
        }
    }
    let _ = layout.write_labels(&sid, &disk_labels);

    let custom_name = disk_labels.get("hc.name").cloned();
    if let Some(n) = &custom_name {
        st.check_name_free(n, Some(&sid))?;
    }
    let mut raw_without_name = disk_labels;
    raw_without_name.remove("hc.name");
    let merged = synthesize_labels(&sid, &raw_without_name, synthesize);

    let (was, pub_json) = {
        let Some(t) = st.terminals.get_mut(&sid) else {
            return Err(RpcError::not_found(&target));
        };
        let was = t.to_public_json();
        if let Some(n) = &custom_name {
            t.name = Some(n.clone());
        }
        t.labels = merged.clone();
        let pub_json = t.to_public_json();
        (was, pub_json)
    };
    if let Some(n) = custom_name
        && let Some(p) = st.registry.terminals.entries.get_mut(&sid)
    {
        p.name = Some(n);
        st.registry.save_terminals();
    }
    st.emit("labels", &sid, json!({ "labels": merged }), Some(was));
    Ok(pub_json)
}

async fn handle_terminal_send(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing `text` parameter"))?
        .to_string();
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    let only_if = params.get("if").and_then(Value::as_str).map(String::from);
    let enter = params.get("enter").and_then(Value::as_bool).unwrap_or(true);
    let wait = params.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let timeout_secs = params.get("timeout").and_then(Value::as_f64).unwrap_or(180.0);
    let caller = params.get("caller").and_then(Value::as_str).map(String::from);

    let (sid, address, cur_status, cur_turn, reported, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        if !t.alive {
            return Err(RpcError::not_alive(&t.address));
        }
        let state_word = t.status.as_ref().map(|s| s.state.clone());
        if let Some(exp) = &only_if
            && state_word.as_deref() != Some(exp.as_str())
        {
            return Err(RpcError::status_mismatch(state_word.as_deref(), exp));
        }
        if !force && state_word.as_deref() == Some("blocked") {
            return Err(RpcError::blocked(format!("{} is blocked waiting for input", t.address)));
        }
        (sid, t.address.clone(), state_word, t.turn, t.reported, st.layout.clone())
    };

    // 查询终端模式并等待静默期（默认最多等 3s 让正在输出的程序安静下来，若已经安静超过 800ms 则无需等）
    let mut modes = query_holder_modes(&layout, &sid).await;
    if !force {
        let wait_deadline = Instant::now() + Duration::from_secs(3);
        while modes.as_ref().is_some_and(|m| m.quiet_ms < 800) && Instant::now() < wait_deadline {
            let remain = 800u64.saturating_sub(modes.as_ref().map(|m| m.quiet_ms).unwrap_or(800));
            tokio::time::sleep(Duration::from_millis(remain.clamp(30, 250))).await;
            modes = query_holder_modes(&layout, &sid).await;
        }
    }

    let bracketed = modes.as_ref().is_some_and(|m| m.bracketed_paste);
    let payload = if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    };

    send_bytes_to_holder(&layout, &sid, payload).await?;
    if enter {
        tokio::time::sleep(Duration::from_millis(200)).await;
        send_bytes_to_holder(&layout, &sid, b"\r".to_vec()).await?;
    }

    if let Some(c) = &caller {
        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        st.last_send.insert((c.clone(), sid.clone()), cur_turn);
    }

    let started = if reported { Some(true) } else { None };
    let wait_res = if wait {
        let w = wait_for_terminal(
            gw,
            &sid,
            &address,
            None,
            Some(cur_turn),
            Duration::from_secs_f64(timeout_secs.max(0.0)),
        )
        .await;
        Some(w)
    } else {
        None
    };

    let mut out = json!({
        "status": cur_status,
        "turn": cur_turn,
        "started": started,
    });
    if let Some(w) = wait_res {
        out["wait"] = w;
    }
    Ok(out)
}

async fn handle_terminal_keys(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let keys: Vec<String> = params
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::invalid_params("missing `keys` array"))?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let (sid, cur_status, cur_turn, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        if !t.alive {
            return Err(RpcError::not_alive(&t.address));
        }
        (
            sid,
            t.status.as_ref().map(|s| s.state.clone()),
            t.turn,
            st.layout.clone(),
        )
    };

    let modes = query_holder_modes(&layout, &sid).await;
    let app_cursor = modes.as_ref().is_some_and(|m| m.app_cursor);
    let mut encoded_steps = Vec::with_capacity(keys.len());
    for k in &keys {
        encoded_steps.push(encode_named_key(k, app_cursor)?);
    }
    for (i, bytes) in encoded_steps.into_iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        send_bytes_to_holder(&layout, &sid, bytes).await?;
    }

    Ok(json!({
        "status": cur_status,
        "turn": cur_turn,
    }))
}

async fn handle_terminal_interrupt(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = target_param(&params)?;
    let (sid, reported, layout) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        if !t.alive {
            return Err(RpcError::not_alive(&t.address));
        }
        (sid, t.reported, st.layout.clone())
    };
    let (bytes, key_name) = if reported {
        (b"\x1b".to_vec(), "esc")
    } else {
        (b"\x03".to_vec(), "ctrl+c")
    };
    send_bytes_to_holder(&layout, &sid, bytes).await?;
    Ok(json!({
        "outcome": if reported { "stopped" } else { "unknown" },
        "key": key_name,
    }))
}

async fn handle_terminal_wait(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let target = params
        .get("terminal")
        .or_else(|| params.get("target"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::new(-32000, "needs_target", "missing terminal target", None))?;
    let output_needle = params.get("output").and_then(Value::as_str).map(String::from);
    let timeout_secs = params.get("timeout").and_then(Value::as_f64).unwrap_or(180.0);
    let caller = params.get("caller").and_then(Value::as_str);

    let (sid, address, after_turn) = {
        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(target))?;
        let address = t.address.clone();
        let after_turn = caller.and_then(|c| st.last_send.remove(&(c.to_string(), sid.clone())));
        (sid, address, after_turn)
    };

    let res = wait_for_terminal(
        gw,
        &sid,
        &address,
        output_needle,
        after_turn,
        Duration::from_secs_f64(timeout_secs.max(0.0)),
    )
    .await;
    Ok(res)
}

async fn wait_for_terminal(
    gw: &SharedGateway,
    sid: &str,
    address: &str,
    output_needle: Option<String>,
    after_turn: Option<u64>,
    timeout: Duration,
) -> Value {
    let build_res = |st: &crate::hc::mirror::GatewayState, ended: bool, reason: &str| -> Value {
        let t = st.terminals.get(sid);
        json!({
            "terminal": address,
            "ended": ended,
            "reason": reason,
            "status": t.and_then(|t| t.status.as_ref().map(|s| s.state.clone())),
            "turn": t.map(|t| t.turn).unwrap_or(0),
            "exit_code": t.and_then(|t| t.exit_code),
            "signal": t.and_then(|t| t.signal),
            "children": [],
        })
    };

    let (mut ev_rx, mut out_rx, changed) = {
        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let Some(t) = st.terminals.get(sid) else {
            return build_res(&st, true, "exit");
        };
        if !t.alive {
            return build_res(&st, true, "exit");
        }
        if after_turn.is_some_and(|k| t.turn > k) {
            return build_res(&st, true, "status");
        }
        let state_word = t.status.as_ref().map(|s| s.state.as_str());
        let stuck = matches!(state_word, Some("blocked" | "error"));
        let pending = after_turn.is_some() && !stuck;
        let already = if output_needle.is_some() {
            stuck
        } else {
            t.reported && !pending && state_word != Some("working")
        };
        if already {
            return build_res(&st, true, "status");
        }

        let ev_rx = st.event_tx.subscribe();
        let out_rx = if output_needle.is_some() {
            let (tx, rx) = mpsc::unbounded_channel();
            st.output_watchers.entry(sid.to_string()).or_default().push(tx);
            Some(rx)
        } else {
            None
        };
        (ev_rx, out_rx, st.changed.clone())
    };

    let deadline = tokio::time::Instant::now() + timeout;
    let mut seen_out = String::new();

    loop {
        let notified = changed.notified();
        tokio::select! {
            _ = notified => {
                let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                let Some(t) = st.terminals.get(sid) else {
                    return build_res(&st, true, "exit");
                };
                if !t.alive {
                    return build_res(&st, true, "exit");
                }
                if after_turn.is_some_and(|k| t.turn > k) {
                    return build_res(&st, true, "status");
                }
                let word = t.status.as_ref().map(|s| s.state.as_str());
                if output_needle.is_none() && t.reported && after_turn.is_none() && word != Some("working") {
                    return build_res(&st, true, "status");
                }
            }
            ev = ev_rx.recv() => {
                if let Ok(ev) = ev
                    && ev.params["terminal"].as_str() == Some(address)
                {
                    match ev.params["type"].as_str() {
                        Some("finished") | Some("removed") => {
                            let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                            return build_res(&st, true, "exit");
                        }
                        Some("turn") => {
                            let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                            return build_res(&st, true, "status");
                        }
                        Some("status") => {
                            let new_state = ev.params["data"]["status"]["state"].as_str();
                            if matches!(new_state, Some("blocked" | "error"))
                                || (output_needle.is_none() && after_turn.is_none() && new_state != Some("working"))
                            {
                                let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                                return build_res(&st, true, "status");
                            }
                        }
                        _ => {}
                    }
                }
            }
            bytes = async {
                match &mut out_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(b) = bytes {
                    seen_out.push_str(&String::from_utf8_lossy(&b));
                    if let Some(needle) = &output_needle
                        && seen_out.contains(needle)
                    {
                        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                        return build_res(&st, true, "output");
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let st = gw.lock().unwrap_or_else(|p| p.into_inner());
                return build_res(&st, false, "timeout");
            }
        }
    }
}

async fn handle_terminal_run(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let mut argv: Vec<String> = params
        .get("argv")
        .or_else(|| params.get("command"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let line = params.get("line").and_then(Value::as_str).map(String::from);
    if !argv.is_empty() && line.is_some() {
        return Err(RpcError::invalid_params("give argv or line, not both"));
    }
    if let Some(l) = line {
        argv = vec!["sh".into(), "-c".into(), l];
    } else if argv.is_empty() {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        argv = vec![sh, "-l".into()];
    }

    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let explicit_cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let cwd = explicit_cwd.clone().or_else(|| std::env::var("HOME").ok());
    let keep = params.get("keep").and_then(Value::as_bool).unwrap_or(false);
    let wait = params.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let timeout_secs = params.get("timeout").and_then(Value::as_f64).unwrap_or(180.0);
    let mut labels = parse_label_filter(params.get("labels"));

    let mut env = Vec::new();
    if let Some(Value::Object(env_obj)) = params.get("env") {
        for (k, v) in env_obj {
            if let Some(s) = v.as_str() {
                env.push((k.clone(), s.to_string()));
            }
        }
    }

    let (layout, core_id) = {
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = &name {
            st.check_name_free(n, None)?;
        }
        (st.layout.clone(), st.registry.identity.core_id.clone())
    };

    if let Some(n) = &name {
        labels.insert("hc.name".into(), n.clone());
    }

    let wants_server_route = labels.contains_key("falcon.host")
        || labels.contains_key("falcon.host_name")
        || labels.get("falcon.remote").is_some_and(|v| v == "true")
        || labels.contains_key("falcon.project")
        || labels
            .get("herdr.space")
            .is_some_and(|s| s.starts_with("falcon."));
    let server_sock = layout.base().join("server-hc.sock");
    let delegated_sid = if wants_server_route && server_sock.exists() {
        delegate_spawn_to_server(
            &server_sock,
            &argv,
            explicit_cwd.as_deref(),
            &env,
            name.as_deref(),
            &labels,
        )
        .await?
    } else {
        None
    };

    let sid = if let Some(sid) = delegated_sid {
        sid
    } else {
        let session_id = gen_uuid_v4();
        let sid = falcon_proto::sessiond::sid_of(&session_id);
        let spec = SpawnSpec {
            session_id: session_id.clone(),
            owner: format!("hc:{core_id}"),
            argv: argv.clone(),
            cwd: cwd.clone(),
            env,
            env_clear: false,
            cols: 120,
            rows: 36,
            scrollback_lines: 10_000,
            appearance: None,
            labels: labels.clone(),
        };

        let layout_c = layout.clone();
        let sid_c = sid.clone();
        let spec_c = spec.clone();
        let spawned = tokio::task::spawn_blocking(move || spawn::spawn(&layout_c, &sid_c, &spec_c))
            .await
            .map_err(|e| RpcError::new(-32000, "spawn_failed", e.to_string(), None))?
            .map_err(|e| RpcError::new(-32000, "spawn_failed", e.to_string(), None))?;
        if !spawned.ok {
            return Err(RpcError::new(
                -32000,
                "spawn_failed",
                spawned.error.unwrap_or_else(|| "spawn failed".into()),
                None,
            ));
        }
        sid
    };

    sync_directory(gw, true);
    let (address, mut pub_json) = {
        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = st.terminals.get_mut(&sid) {
            t.keep = keep;
            if let Some(n) = &name {
                t.name = Some(n.clone());
            }
        }
        if let Some(p) = st.registry.terminals.entries.get_mut(&sid) {
            p.keep = keep;
            if let Some(n) = &name {
                p.name = Some(n.clone());
            }
            st.registry.save_terminals();
        }
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&sid))?;
        (t.address.clone(), t.to_public_json())
    };

    if wait {
        let w = wait_for_terminal(
            gw,
            &sid,
            &address,
            None,
            None,
            Duration::from_secs_f64(timeout_secs.max(0.0)),
        )
        .await;
        let read_res = handle_terminal_read(
            gw,
            json!({
                "terminal": address,
                "lines": 1000,
            }),
        )
        .await
        .ok();
        if let Some(obj) = pub_json.as_object_mut() {
            obj.insert("wait".into(), w);
            if let Some(r) = read_res
                && let Some(txt) = r.get("text").cloned()
            {
                obj.insert("output".into(), txt);
            }
        }
    }

    Ok(pub_json)
}

async fn delegate_spawn_to_server(
    server_sock: &std::path::Path,
    argv: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    name: Option<&str>,
    labels: &BTreeMap<String, String>,
) -> Result<Option<String>, RpcError> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    let Ok(stream) = tokio::net::UnixStream::connect(server_sock).await else {
        return Ok(None);
    };
    let (r, mut w) = stream.into_split();
    let req = json!({
        "action": "spawn",
        "argv": argv,
        "cwd": cwd,
        "env": env,
        "name": name,
        "labels": labels,
    });
    let mut line = serde_json::to_vec(&req).unwrap_or_default();
    line.push(b'\n');
    if w.write_all(&line).await.is_err() || w.flush().await.is_err() {
        return Ok(None);
    }
    let mut reader = BufReader::new(r);
    let mut resp_line = String::new();
    let read_res = tokio::time::timeout(Duration::from_secs(30), reader.read_line(&mut resp_line)).await;
    let Ok(Ok(n)) = read_res else {
        return Ok(None);
    };
    if n == 0 {
        return Ok(None);
    }
    let Ok(resp) = serde_json::from_str::<Value>(resp_line.trim()) else {
        return Ok(None);
    };
    if resp.get("handled").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    if resp.get("ok").and_then(Value::as_bool) == Some(true)
        && let Some(sid) = resp.get("sid").and_then(Value::as_str).filter(|s| !s.is_empty())
    {
        return Ok(Some(sid.to_string()));
    }
    let err_msg = resp
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("remote spawn failed");
    Err(RpcError::new(-32000, "spawn_failed", err_msg, None))
}

/// `terminal.put`：支持两种调用形态：
/// 1. `hc put <terminal> <file>`：参数含 `terminal`、`name`（文件名）、`data`（base64 字节）→ 落盘到 `<hc_dir>/files/<addr>/<name>`，返回 `{path, bytes}`；
/// 2. 按名字幂等创建/更新终端：参数不含 `data`，按 `name` 查找已有 live 终端，不存在则走 `terminal.run`。
fn handle_terminal_put(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    if let Some(data_b64) = params.get("data").and_then(Value::as_str) {
        let target = target_param(&params)?;
        let file_name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("missing `name` parameter"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_b64.trim())
            .map_err(|_| RpcError::invalid_params("data is not base64"))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(RpcError::invalid_params("at most 8 MB per put"));
        }
        let st = gw.lock().unwrap_or_else(|p| p.into_inner());
        let sid = st.resolve_sid(&target)?;
        let t = st.terminals.get(&sid).ok_or_else(|| RpcError::not_found(&target))?;
        if !t.alive {
            return Err(RpcError::not_alive(&t.address));
        }
        let dir = st.layout.hc_dir().join("files").join(format_addr(t.seq));
        std::fs::create_dir_all(&dir)
            .map_err(|e| RpcError::new(-32603, "internal", format!("cannot create staging dir: {e}"), None))?;
        let clean_name: String = file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("file")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let clean_name = clean_name.trim_start_matches('.').to_string();
        let clean_name = if clean_name.is_empty() { "file".into() } else { clean_name };
        let path = dir.join(clean_name);
        std::fs::write(&path, &bytes)
            .map_err(|e| RpcError::new(-32603, "internal", format!("cannot write file: {e}"), None))?;
        return Ok(json!({
            "path": path.display().to_string(),
            "bytes": bytes.len(),
        }));
    }

    // 幂等按名查找
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| valid_terminal_name(s))
        .ok_or_else(|| RpcError::invalid_params("missing or invalid `name`"))?;
    sync_directory(gw, true);
    let st = gw.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = st.terminals.values().find(|t| t.alive && t.name.as_deref() == Some(name)) {
        return Ok(t.to_public_json());
    }
    Err(RpcError::not_found(name))
}

fn handle_doc_get(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing `name` parameter"))?;
    if name == "herdr-app" {
        sync_directory(gw, true);
    }
    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    if name == "herdr-app" {
        let _ = crate::hc::mirror::sync_herdr_app_doc(&mut st, false);
    }
    st.registry.doc_get(name)
}

fn handle_doc_set(gw: &SharedGateway, params: Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing `name` parameter"))?
        .to_string();
    let value = params.get("value").or_else(|| params.get("data")).cloned().unwrap_or(Value::Null);
    let version = params.get("version").and_then(Value::as_u64).unwrap_or(0);
    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let new_ver = st.registry.doc_set(&name, value, version)?;
    if name == "herdr-app" {
        crate::hc::mirror::normalize_herdr_app_doc_in_place(&mut st);
    }
    st.emit_doc(&name, new_ver);
    Ok(json!({ "version": new_ver }))
}

fn handle_events_subscribe(
    gw: &SharedGateway,
    ctx: &ConnContext,
    params: Value,
) -> Result<Value, RpcError> {
    sync_directory(gw, true);
    let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
    let terminal = match params.get("terminal").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(t) => {
            let sid = st.resolve_sid(t)?;
            Some(st.terminals[&sid].address.clone())
        }
        None => None,
    };
    let children_of = match params.get("children_of").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(t) => {
            let sid = st.resolve_sid(t)?;
            Some(st.terminals[&sid].address.clone())
        }
        None => None,
    };
    let labels = parse_label_filter(params.get("labels"));
    let filter = EventFilter {
        terminal,
        children_of,
        labels,
    };
    let seq = st.seq;
    let terminals = st.matching(&filter);

    if ctx.framed {
        let Some(frame_out_tx) = ctx.frame_out_tx.clone() else {
            return Err(RpcError::needs_framed());
        };
        let channel = ctx.alloc_channel();
        st.subscriber_count += 1;
        let ev_rx = st.event_tx.subscribe();
        drop(st);

        let v_tx = spawn_framed_event_stream(gw.clone(), channel, filter, ev_rx, frame_out_tx);
        ctx.channels.lock().unwrap_or_else(|p| p.into_inner()).insert(channel, v_tx);

        Ok(json!({
            "seq": seq,
            "terminals": terminals,
            "channel": channel,
        }))
    } else {
        *ctx.plain_event_filter.lock().unwrap_or_else(|p| p.into_inner()) = Some(filter);
        Ok(json!({
            "seq": seq,
            "terminals": terminals,
            "channel": Value::Null,
        }))
    }
}

fn spawn_framed_event_stream(
    gw: SharedGateway,
    channel: u32,
    filter: EventFilter,
    mut ev_rx: tokio::sync::broadcast::Receiver<crate::hc::mirror::EmittedEvent>,
    frame_out_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> mpsc::UnboundedSender<ViewerFrame> {
    use crate::hc::wire::{ChannelCredit, encode_raw, kind as wkind};
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut credit = ChannelCredit::new(channel);
        let heartbeat_ms = std::env::var("HC_HEARTBEAT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15_000);
        let mut hb_tick = tokio::time::interval(Duration::from_millis(heartbeat_ms.max(50)));
        hb_tick.tick().await;

        let flush = |cc: &mut ChannelCredit, tx: &mpsc::UnboundedSender<Vec<u8>>| {
            for f in cc.drain_frames() {
                let _ = tx.send(f);
            }
        };

        loop {
            tokio::select! {
                vf = in_rx.recv() => {
                    let Some(vf) = vf else { break };
                    match vf {
                        ViewerFrame::Credit(amount) => {
                            credit.add_credit(amount);
                            if credit.dropped && credit.credit > 0 {
                                let snap = gw.lock().unwrap_or_else(|p| p.into_inner()).snapshot(&filter);
                                let line = format!("{}\n", json!({
                                    "jsonrpc": "2.0",
                                    "method": "missed",
                                    "params": snap,
                                }));
                                credit.reset_with_first_paint(line.as_bytes());
                            }
                            flush(&mut credit, &frame_out_tx);
                        }
                        ViewerFrame::Close => {
                            let _ = frame_out_tx.send(encode_raw(channel, wkind::CLOSE, b""));
                            break;
                        }
                        _ => {}
                    }
                }
                ev = ev_rx.recv() => {
                    match ev {
                        Ok(ev) => {
                            let now_match = filter.matches(&ev.record);
                            let before_match = ev.was.as_ref().is_some_and(|w| filter.matches(w));
                            if !now_match && !before_match {
                                continue;
                            }
                            let mut params = ev.params;
                            if now_match && ev.was.is_some() && !before_match {
                                params["data"]["record"] = ev.record;
                            }
                            let line = format!("{}\n", json!({
                                "jsonrpc": "2.0",
                                "method": "event",
                                "params": params,
                            }));
                            credit.push_data(line.as_bytes());
                            if credit.dropped && credit.credit > 0 {
                                let snap = gw.lock().unwrap_or_else(|p| p.into_inner()).snapshot(&filter);
                                let mline = format!("{}\n", json!({
                                    "jsonrpc": "2.0",
                                    "method": "missed",
                                    "params": snap,
                                }));
                                credit.reset_with_first_paint(mline.as_bytes());
                            }
                            flush(&mut credit, &frame_out_tx);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let snap = gw.lock().unwrap_or_else(|p| p.into_inner()).snapshot(&filter);
                            let mline = format!("{}\n", json!({
                                "jsonrpc": "2.0",
                                "method": "missed",
                                "params": snap,
                            }));
                            credit.reset_with_first_paint(mline.as_bytes());
                            flush(&mut credit, &frame_out_tx);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = hb_tick.tick() => {
                    if credit.pending.is_empty() && !credit.dropped {
                        let seq = gw.lock().unwrap_or_else(|p| p.into_inner()).seq;
                        let line = format!("{}\n", json!({
                            "jsonrpc": "2.0",
                            "method": "heartbeat",
                            "params": { "seq": seq },
                        }));
                        credit.push_data(line.as_bytes());
                        flush(&mut credit, &frame_out_tx);
                    }
                }
            }
        }

        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        st.subscriber_count = st.subscriber_count.saturating_sub(1);
    });
    in_tx
}

/// 同步连接 holder 导出纯文本历史与当前模式
fn export_plain_from_holder(layout: &Layout, sid: &str) -> Option<(String, Option<ModesAre>)> {
    let endpoint = layout
        .read_meta(sid)
        .map(|m| m.endpoint)
        .unwrap_or_else(|| layout.endpoint(sid));
    let stream = ipc::connect(&endpoint).ok()?;
    let (mut reader, mut writer) = stream.split().ok()?;
    let hello = ClientMsg::Hello(Hello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        role: Role::Hc,
        client: "hc-export".into(),
    });
    writer.write_all(&hello.encode()).ok()?;
    writer.write_all(&ClientMsg::Modes { req: 1 }.encode()).ok()?;
    writer
        .write_all(&ClientMsg::Export { req: 2, format: ExportFormat::Plain }.encode())
        .ok()?;

    let mut modes = None;
    while let Ok(Some((kind, payload))) = read_frame(&mut reader) {
        let Ok(msg) = HolderMsg::decode(kind, payload) else { continue };
        match msg {
            HolderMsg::Welcome(w) => {
                if negotiate((PROTO_MIN, PROTO_MAX), (w.proto, w.proto)).is_none() {
                    return None;
                }
            }
            HolderMsg::ModesAre(m) => {
                modes = Some(m);
            }
            HolderMsg::Exported { bytes, .. } => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                return Some((text, modes));
            }
            _ => {}
        }
    }
    None
}

async fn query_holder_modes(layout: &Layout, sid: &str) -> Option<ModesAre> {
    let layout = layout.clone();
    let sid = sid.to_string();
    tokio::task::spawn_blocking(move || {
        let endpoint = layout
            .read_meta(&sid)
            .map(|m| m.endpoint)
            .unwrap_or_else(|| layout.endpoint(&sid));
        let stream = ipc::connect(&endpoint).ok()?;
        let (mut reader, mut writer) = stream.split().ok()?;
        let hello = ClientMsg::Hello(Hello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            role: Role::Hc,
            client: "hc-modes".into(),
        });
        writer.write_all(&hello.encode()).ok()?;
        writer.write_all(&ClientMsg::Modes { req: 1 }.encode()).ok()?;
        while let Ok(Some((kind, payload))) = read_frame(&mut reader) {
            let Ok(msg) = HolderMsg::decode(kind, payload) else { continue };
            match msg {
                HolderMsg::Welcome(w) => {
                    if negotiate((PROTO_MIN, PROTO_MAX), (w.proto, w.proto)).is_none() {
                        return None;
                    }
                }
                HolderMsg::ModesAre(m) => return Some(m),
                _ => {}
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn send_bytes_to_holder(layout: &Layout, sid: &str, bytes: Vec<u8>) -> Result<(), RpcError> {
    let layout = layout.clone();
    let sid = sid.to_string();
    tokio::task::spawn_blocking(move || {
        let endpoint = layout
            .read_meta(&sid)
            .map(|m| m.endpoint)
            .unwrap_or_else(|| layout.endpoint(&sid));
        let stream = ipc::connect(&endpoint)
            .map_err(|_| RpcError::not_alive(&sid))?;
        let (mut reader, mut writer) = stream
            .split()
            .map_err(|_| RpcError::not_alive(&sid))?;
        let hello = ClientMsg::Hello(Hello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            role: Role::Hc,
            client: "hc-send".into(),
        });
        writer
            .write_all(&hello.encode())
            .map_err(|_| RpcError::not_alive(&sid))?;
        writer
            .write_all(&ClientMsg::Input(bytes).encode())
            .map_err(|_| RpcError::not_alive(&sid))?;
        writer
            .write_all(&ClientMsg::Ping.encode())
            .map_err(|_| RpcError::not_alive(&sid))?;
        let _ = writer.flush();
        while let Ok(Some((kind, payload))) = read_frame(&mut reader) {
            let Ok(msg) = HolderMsg::decode(kind, payload) else { continue };
            if matches!(msg, HolderMsg::Pong) {
                break;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| RpcError::new(-32603, "internal", e.to_string(), None))?
}

/// 将 `terminal.keys` 的按键名编码为字节序列（支持 `enter`/`esc`/`tab`/`btab`/`backspace`/`space`、
/// 方向键、导航键、`f1..f12`、`ctrl+c`/`C-c`/`^C`、`alt+x`/`M-x`、`shift+a` 及单字符）
pub fn encode_named_key(spec: &str, app_cursor: bool) -> Result<Vec<u8>, RpcError> {
    let bad = || {
        RpcError::invalid_params(format!(
            "unknown key {spec:?} (try enter, esc, up, ctrl+c, alt+x, f5, a)"
        ))
    };
    if spec.is_empty() {
        return Err(bad());
    }
    // 兼容 ^C 写法
    if let Some(rest) = spec.strip_prefix('^')
        && rest.len() == 1
    {
        let ch = rest.as_bytes()[0].to_ascii_lowercase();
        if ch.is_ascii_alphabetic() {
            return Ok(vec![ch - b'a' + 1]);
        }
    }
    // 兼容 tmux 风格 C-x / M-x
    if let Some(rest) = spec.strip_prefix("C-").or_else(|| spec.strip_prefix("c-")) {
        return encode_with_mods(rest, false, true, false, app_cursor).ok_or_else(bad);
    }
    if let Some(rest) = spec.strip_prefix("M-").or_else(|| spec.strip_prefix("m-")) {
        return encode_with_mods(rest, false, false, true, app_cursor).ok_or_else(bad);
    }

    let lower = spec.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').collect();
    let key = parts.pop().filter(|n| !n.is_empty()).ok_or_else(bad)?;
    let (mut shift, mut ctrl, mut alt) = (false, false, false);
    for m in parts {
        match m {
            "shift" => shift = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "meta" | "opt" => alt = true,
            "super" | "cmd" => {}
            _ => return Err(bad()),
        }
    }
    encode_with_mods(key, shift, ctrl, alt, app_cursor).ok_or_else(bad)
}

fn encode_with_mods(
    key: &str,
    shift: bool,
    ctrl: bool,
    alt: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let lower = key.to_ascii_lowercase();
    let base: Vec<u8> = match lower.as_str() {
        "enter" | "return" => b"\r".to_vec(),
        "esc" | "escape" => b"\x1b".to_vec(),
        "tab" => {
            if shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        "btab" => b"\x1b[Z".to_vec(),
        "backspace" | "bspace" => b"\x7f".to_vec(),
        "space" => {
            if ctrl {
                vec![0]
            } else {
                b" ".to_vec()
            }
        }
        "delete" | "del" | "dc" => b"\x1b[3~".to_vec(),
        "insert" | "ic" => b"\x1b[2~".to_vec(),
        "home" => {
            if app_cursor {
                b"\x1bOH".to_vec()
            } else {
                b"\x1b[H".to_vec()
            }
        }
        "end" => {
            if app_cursor {
                b"\x1bOF".to_vec()
            } else {
                b"\x1b[F".to_vec()
            }
        }
        "pageup" | "pgup" | "ppage" => b"\x1b[5~".to_vec(),
        "pagedown" | "pgdn" | "npage" => b"\x1b[6~".to_vec(),
        "up" => arrow_seq(b'A', shift, ctrl, alt, app_cursor),
        "down" => arrow_seq(b'B', shift, ctrl, alt, app_cursor),
        "right" => arrow_seq(b'C', shift, ctrl, alt, app_cursor),
        "left" => arrow_seq(b'D', shift, ctrl, alt, app_cursor),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        "backslash" => b"\\".to_vec(),
        _ if key.chars().count() == 1 => {
            let c = key.chars().next()?;
            if ctrl {
                let lc = c.to_ascii_lowercase();
                if lc.is_ascii_alphabetic() {
                    vec![(lc as u8) - b'a' + 1]
                } else {
                    return None;
                }
            } else if shift || c.is_ascii_uppercase() {
                c.to_ascii_uppercase().to_string().into_bytes()
            } else {
                c.to_string().into_bytes()
            }
        }
        _ => return None,
    };

    if alt && !matches!(lower.as_str(), "up" | "down" | "left" | "right") {
        let mut with_esc = Vec::with_capacity(1 + base.len());
        with_esc.push(0x1b);
        with_esc.extend_from_slice(&base);
        Some(with_esc)
    } else {
        Some(base)
    }
}

fn arrow_seq(letter: u8, shift: bool, ctrl: bool, alt: bool, app_cursor: bool) -> Vec<u8> {
    let modif = 1 + (if shift { 1 } else { 0 }) + (if alt { 2 } else { 0 }) + (if ctrl { 4 } else { 0 });
    if modif > 1 {
        format!("\x1b[1;{modif}{}", letter as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_encoding_matches_expected_sequences() {
        assert_eq!(encode_named_key("enter", false).unwrap(), b"\r");
        assert_eq!(encode_named_key("esc", false).unwrap(), b"\x1b");
        assert_eq!(encode_named_key("tab", false).unwrap(), b"\t");
        assert_eq!(encode_named_key("shift+tab", false).unwrap(), b"\x1b[Z");
        assert_eq!(encode_named_key("backspace", false).unwrap(), b"\x7f");
        assert_eq!(encode_named_key("a", false).unwrap(), b"a");
        assert_eq!(encode_named_key("shift+a", false).unwrap(), b"A");
        assert_eq!(encode_named_key("ctrl+c", false).unwrap(), b"\x03");
        assert_eq!(encode_named_key("C-c", false).unwrap(), b"\x03");
        assert_eq!(encode_named_key("^C", false).unwrap(), b"\x03");
        assert_eq!(encode_named_key("alt+x", false).unwrap(), b"\x1bx");
        assert_eq!(encode_named_key("up", false).unwrap(), b"\x1b[A");
        assert_eq!(encode_named_key("up", true).unwrap(), b"\x1bOA");
        assert_eq!(encode_named_key("f1", false).unwrap(), b"\x1bOP");
        assert_eq!(encode_named_key("space", false).unwrap(), b" ");
    }
}
