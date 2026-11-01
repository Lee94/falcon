//! `herdr-core` JSON-RPC 2.0 消息、错误码与 `rpc.discover` 模式定义。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 对齐 herdr-core v0.1.16 的代数与版本标识：
/// `generation = 1` 是 hc machine 握手必须匹配的值（不匹配会被 hc 当作旧版并尝试重装）
pub const HC_GENERATION: u32 = 1;
pub const HC_COMPAT_VERSION: &str = "0.1.16";

pub fn hc_version_string() -> String {
    format!("{HC_COMPAT_VERSION}-falcon-{}", falcon_proto::sessiond::FALCON_VERSION)
}

pub fn hc_features() -> Vec<&'static str> {
    vec!["framed", "attach", "events", "status", "shell"]
}

pub mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const UNKNOWN_METHOD: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const NOT_FOUND: i32 = -32001;
    pub const AMBIGUOUS: i32 = -32002;
    pub const CONFLICT: i32 = -32004;
    #[allow(dead_code)]
    pub const TIMEOUT: i32 = -32008;
    pub const BLOCKED: i32 = -32009;
    pub const NOT_ALIVE: i32 = -32010;
    pub const REFUSED: i32 = -32011;
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, kind: &str, message: impl Into<String>, hint: Option<&str>) -> Self {
        let mut data = serde_json::Map::new();
        data.insert("kind".into(), Value::String(kind.into()));
        if let Some(h) = hint {
            data.insert("hint".into(), Value::String(h.into()));
        }
        Self {
            code,
            message: message.into(),
            data: Some(Value::Object(data)),
        }
    }

    pub fn with_extra(mut self, k: &str, v: Value) -> Self {
        if let Some(Value::Object(map)) = &mut self.data {
            map.insert(k.into(), v);
        }
        self
    }

    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(code::PARSE_ERROR, "parse_error", msg, None)
    }

    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(code::INVALID_REQUEST, "invalid_request", msg, None)
    }

    pub fn unknown_method(method: &str) -> Self {
        Self::new(
            code::UNKNOWN_METHOD,
            "unknown_method",
            format!("unknown method: {method}"),
            Some("this endpoint is a falcon-sessiond herdr-core compatibility gateway"),
        )
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, "invalid_params", msg, None)
    }

    pub fn not_found(target: &str) -> Self {
        Self::new(
            code::NOT_FOUND,
            "not_found",
            format!("terminal not found: {target}"),
            Some("run `hc ls` to see available terminals"),
        )
    }

    pub fn ambiguous(target: &str, candidates: Vec<String>) -> Self {
        Self::new(
            code::AMBIGUOUS,
            "ambiguous",
            format!("ambiguous terminal target: {target}"),
            Some("specify the short address (e.g. t1) or full UUID"),
        )
        .with_extra("candidates", json!(candidates))
    }

    pub fn conflict(msg: impl Into<String>, current_version: Option<u64>) -> Self {
        let mut e = Self::new(code::CONFLICT, "conflict", msg, None);
        if let Some(v) = current_version {
            e = e.with_extra("version", json!(v));
        }
        e
    }

    #[allow(dead_code)]
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::new(code::TIMEOUT, "timeout", msg, Some("increase --timeout or pass --force"))
    }

    pub fn blocked(msg: impl Into<String>) -> Self {
        Self::new(
            code::BLOCKED,
            "blocked",
            msg,
            Some("terminal is blocked waiting for user input; pass force=true to send anyway"),
        )
    }

    pub fn not_alive(target: &str) -> Self {
        Self::new(
            code::NOT_ALIVE,
            "finished",
            format!("{target} has finished"),
            Some(&format!("hc read {target}")),
        )
    }

    pub fn still_live(target: &str) -> Self {
        Self::new(
            -32000,
            "live",
            format!("{target} is still running"),
            Some(&format!("kill it first: hc kill {target}")),
        )
    }

    pub fn name_taken(name: &str) -> Self {
        Self::new(
            code::CONFLICT,
            "name_taken",
            format!("a live terminal is already named {name:?}"),
            Some("pick another name, or kill the one holding it"),
        )
    }

    pub fn needs_framed() -> Self {
        Self::new(
            -32000,
            "needs_framed",
            "attach needs a framed connection",
            Some("send hello with framed: true first, or use `hc attach`"),
        )
    }

    pub fn status_mismatch(actual: Option<&str>, expected: &str) -> Self {
        Self::new(
            -32000,
            "status_mismatch",
            format!("terminal status is {:?}, not {expected:?}", actual.unwrap_or("none")),
            None,
        )
    }

    pub fn refused(msg: impl Into<String>, hint: Option<&str>) -> Self {
        Self::new(code::REFUSED, "refused", msg, hint)
    }
}

pub fn ok_response(id: &Value, result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("序列化 JSON-RPC 响应")
}

pub fn err_response(id: &Value, err: RpcError) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err,
    }))
    .expect("序列化 JSON-RPC 错误")
}

#[allow(dead_code)]
pub fn event_notification(params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": params,
    }))
    .expect("序列化 JSON-RPC 事件通知")
}

/// `rpc.discover` 返回的 OpenRPC 摘要文档
pub fn discover_schema() -> Value {
    json!({
        "openrpc": "1.2.6",
        "info": {
            "title": "herdr-core (falcon-sessiond gateway)",
            "version": hc_version_string(),
            "x-generation": HC_GENERATION,
            "x-features": hc_features(),
        },
        "methods": [
            { "name": "hello", "summary": "Handshake and capability negotiation" },
            { "name": "server.status", "summary": "Server status and terminal count" },
            { "name": "rpc.discover", "summary": "OpenRPC schema discovery" },
            { "name": "terminal.list", "summary": "List terminals" },
            { "name": "terminal.get", "summary": "Get terminal details" },
            { "name": "terminal.read", "summary": "Read terminal screen or scrollback" },
            { "name": "terminal.attach", "summary": "Attach a framed viewer channel to a terminal" },
            { "name": "terminal.send", "summary": "Send text to a terminal with quiet and blocked checks" },
            { "name": "terminal.keys", "summary": "Send named keystrokes to a terminal" },
            { "name": "terminal.interrupt", "summary": "Send SIGINT (Ctrl-C) to a terminal" },
            { "name": "terminal.wait", "summary": "Wait for terminal status, turn, or exit" },
            { "name": "terminal.label", "summary": "Set or remove terminal labels" },
            { "name": "terminal.keep", "summary": "Mark whether an exited terminal stays in the list" },
            { "name": "terminal.kill", "summary": "Terminate one or more terminals" },
            { "name": "terminal.rm", "summary": "Remove exited terminals (or kill + remove with force)" },
            { "name": "terminal.run", "summary": "Spawn a new terminal session" },
            { "name": "terminal.put", "summary": "Idempotently create or update a terminal by name" },
            { "name": "templates.list", "summary": "List terminal templates" },
            { "name": "events.subscribe", "summary": "Subscribe to real-time terminal and document events" },
            { "name": "doc.get", "summary": "Get a named JSON document" },
            { "name": "doc.set", "summary": "Set a named JSON document with optional CAS version" }
        ]
    })
}
