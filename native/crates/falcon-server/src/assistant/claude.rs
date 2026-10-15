//! Claude Messages API 的流式客户端（内置 agent 用，docs/design/ai-assistant.md 6.1）。
//!
//! Rust 没有官方 SDK，走原始 HTTP：`POST {base}/v1/messages`，`stream: true`，SSE。这里分三层：
//! - [`request_body`] / [`headers`]：纯函数拼请求；
//! - [`SseParser`]：把字节流切成 SSE 事件的 data（跨块拼行）；
//! - [`Accumulator`]：按 `content_block_start / _delta / _stop` 把内容块**原样**攒起来——历史要
//!   原样回传（thinking 块带签名，preserved thinking 要求只追加不改写），所以块一律存成 JSON，
//!   不认识的块类型也照存；同时吐出界面要的增量（文字、思考、工具调用开始 / 结束）。
//!
//! 几条从文档里来的规矩（claude-api skill，改之前先读）：
//! - Opus 5.5 不能关思考，深度只靠 `output_config.effort`，它的缺省是 medium，这里显式给；
//!   `display: "summarized"` 才有看得见的思考摘要；
//! - `eager_input_streaming` 与 `fallbacks: "default"`（beta `server-side-fallback-2026-07-01`）
//!   **只在官方地址上开**：网关 / 代理可能不认这两个字段，直接 400；
//! - 开了 eager 之后工具参数不再由服务端校验：攒出来的 JSON 自己严格解析，坏了就把原文当错误
//!   结果回给模型（[`Block::invalid_input`]），不执行；
//! - 中途换了模型（`fallback` 块）之后，回传时分界点之前只留 text 块（[`echo_content`]），那些
//!   tool_use 也不执行。

use serde_json::{Map, Value, json};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// 流式请求给足输出空间（文档：流式默认 ~64000，不怕超时）
const MAX_TOKENS: u32 = 64000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    /// 空 = 官方地址
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub effort: String,
}

impl ApiConfig {
    pub fn endpoint(&self) -> String {
        let base = if self.base_url.trim().is_empty() { DEFAULT_BASE_URL } else { self.base_url.trim() };
        format!("{}/v1/messages", base.trim_end_matches('/'))
    }

    /// 打的是不是官方地址：只有它保证认 eager 与 fallbacks
    pub fn official(&self) -> bool {
        let base = self.base_url.trim().trim_end_matches('/');
        base.is_empty() || base == DEFAULT_BASE_URL
    }
}

pub fn headers(cfg: &ApiConfig) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("content-type", "application/json".to_string()),
        ("x-api-key", cfg.api_key.clone()),
        ("anthropic-version", API_VERSION.to_string()),
    ];
    if cfg.official() {
        h.push(("anthropic-beta", FALLBACK_BETA.to_string()));
    }
    h
}

/// 请求体。`tools` 是不带 eager 标记的定义，这里按地址决定加不加。
///
/// 缓存：system 块上一个断点（渲染顺序 tools → system → messages，这一个就盖住了工具与系统
/// 提示词），再加顶层的自动断点盖住不断变长的历史。system 里不放任何每轮都变的东西
pub fn request_body(cfg: &ApiConfig, system: &str, tools: &[Value], messages: &[Value]) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            let mut t = t.clone();
            if cfg.official()
                && let Some(obj) = t.as_object_mut()
            {
                obj.insert("eager_input_streaming".into(), Value::Bool(true));
            }
            t
        })
        .collect();
    let mut body = json!({
        "model": cfg.model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "thinking": { "type": "adaptive", "display": "summarized" },
        "output_config": { "effort": cfg.effort },
        "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
        "tools": tools,
        "messages": messages,
        "cache_control": { "type": "ephemeral" },
    });
    if cfg.official() {
        body["fallbacks"] = Value::String("default".into());
    }
    body
}

/// 出错时 API 给的 `{"type":"error","error":{"type","message"}}` → 一句能给人看的话
pub fn error_message(status: u16, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let msg = parsed
        .as_ref()
        .and_then(|v| v["error"]["message"].as_str().or(v["message"].as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(300).collect());
    format!("HTTP {status}：{msg}")
}

/// 值得重试的状态码：限流、过载、服务端错误
pub fn retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

// ---------------------------------------------------------------- SSE

/// 把响应体切成一条条 SSE 事件的 data。只看 `data:` 行（事件名在 data 的 `type` 里也有）
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    data: String,
}

impl SseParser {
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(std::mem::take(&mut self.data));
                }
            } else if let Some(d) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(d.strip_prefix(' ').unwrap_or(d));
            }
        }
        out
    }
}

// ---------------------------------------------------------------- 累积

/// 界面关心的增量
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// 新的文字块 / 思考块 / 工具调用开始了
    TextStart { index: usize },
    ThinkingStart { index: usize },
    ToolStart { index: usize, id: String, name: String },
    Text { index: usize, text: String },
    Thinking { index: usize, text: String },
    /// 工具调用的参数收齐了（或者坏了）
    ToolDone { index: usize },
    /// 中途换了模型：之前流出来的内容作废（界面上标一下，历史里剔掉）
    Fallback { index: usize },
}

/// 一个内容块：原样的 JSON，外加工具参数的拼接缓冲
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub value: Value,
    partial_json: String,
    /// 工具参数不是合法 JSON：原文（不执行，原文回给模型）
    pub invalid_input: Option<String>,
}

impl Block {
    pub fn kind(&self) -> &str {
        self.value["type"].as_str().unwrap_or("")
    }
}

#[derive(Debug, Default)]
pub struct Accumulator {
    pub blocks: Vec<Block>,
    pub stop_reason: Option<String>,
    pub model: Option<String>,
    /// API 在流里报的错（`{"type":"error"}` 事件）
    pub error: Option<String>,
}

impl Accumulator {
    /// 吃一条 SSE 事件的 data
    pub fn apply(&mut self, data: &str) -> Vec<Delta> {
        let Ok(ev) = serde_json::from_str::<Value>(data) else { return Vec::new() };
        let index = ev["index"].as_u64().unwrap_or(0) as usize;
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.model = ev["message"]["model"].as_str().map(str::to_string);
                Vec::new()
            }
            "content_block_start" => {
                let value = ev["content_block"].clone();
                let delta = match value["type"].as_str().unwrap_or("") {
                    "text" => Some(Delta::TextStart { index }),
                    "thinking" => Some(Delta::ThinkingStart { index }),
                    "tool_use" => Some(Delta::ToolStart {
                        index,
                        id: value["id"].as_str().unwrap_or("").to_string(),
                        name: value["name"].as_str().unwrap_or("").to_string(),
                    }),
                    "fallback" => Some(Delta::Fallback { index }),
                    _ => None,
                };
                while self.blocks.len() <= index {
                    self.blocks.push(Block { value: Value::Null, partial_json: String::new(), invalid_input: None });
                }
                self.blocks[index] = Block { value, partial_json: String::new(), invalid_input: None };
                delta.into_iter().collect()
            }
            "content_block_delta" => {
                let Some(block) = self.blocks.get_mut(index) else { return Vec::new() };
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append_str(&mut block.value, "text", t);
                        vec![Delta::Text { index, text: t.to_string() }]
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        append_str(&mut block.value, "thinking", t);
                        vec![Delta::Thinking { index, text: t.to_string() }]
                    }
                    "signature_delta" => {
                        append_str(&mut block.value, "signature", d["signature"].as_str().unwrap_or(""));
                        Vec::new()
                    }
                    "input_json_delta" => {
                        block.partial_json.push_str(d["partial_json"].as_str().unwrap_or(""));
                        Vec::new()
                    }
                    // 引文之类的增量：v1 不显示，原样丢掉（块本身照存）
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let Some(block) = self.blocks.get_mut(index) else { return Vec::new() };
                if block.kind() != "tool_use" {
                    return Vec::new();
                }
                // 严格解析攒出来的参数（eager 模式下服务端不校验）。空串 = 没有参数
                let raw = std::mem::take(&mut block.partial_json);
                let input = if raw.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str::<Value>(&raw) };
                match input {
                    Ok(v) if v.is_object() => block.value["input"] = v,
                    _ => {
                        block.value["input"] = json!({});
                        block.invalid_input = Some(raw);
                    }
                }
                vec![Delta::ToolDone { index }]
            }
            "message_delta" => {
                if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
                Vec::new()
            }
            "error" => {
                self.error = Some(ev["error"]["message"].as_str().unwrap_or("流式响应出错").to_string());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// 最后一个 `fallback` 块的位置：它之前的是被拒的那次尝试流出来的
    pub fn fallback_boundary(&self) -> Option<usize> {
        self.blocks.iter().rposition(|b| b.kind() == "fallback")
    }

    /// 要执行的工具调用：分界点之后的 tool_use 块
    pub fn tool_uses(&self) -> Vec<&Block> {
        let start = self.fallback_boundary().map_or(0, |i| i + 1);
        self.blocks[start.min(self.blocks.len())..].iter().filter(|b| b.kind() == "tool_use").collect()
    }
}

fn append_str(v: &mut Value, key: &str, s: &str) {
    if let Some(obj) = v.as_object_mut() {
        let cur = obj.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        obj.insert(key.into(), Value::String(cur + s));
    }
}

/// 回传进历史的 content：分界点之前只留 text 块（文档：中途换模型后，之前的 thinking /
/// tool_use / 不认识的块都不回传），`fallback` 块本身也不留；分界点之后原样
pub fn echo_content(blocks: &[Block]) -> Vec<Value> {
    let boundary = blocks.iter().rposition(|b| b.kind() == "fallback");
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !b.value.is_null())
        .filter(|(i, b)| match boundary {
            Some(at) if *i < at => b.kind() == "text",
            Some(at) if *i == at => false,
            _ => true,
        })
        .map(|(_, b)| b.value.clone())
        .collect()
}

/// 历史里最后一条 assistant 消息有没有没配上结果的 tool_use：有就补上 is_error 的结果
/// （被拒绝、被截断、被停止的那一轮），否则下一条用户消息发出去就是 400
pub fn close_dangling_tool_uses(messages: &mut Vec<Value>, why: &str) {
    let Some(last) = messages.last() else { return };
    if last["role"] != "assistant" {
        return;
    }
    let ids: Vec<String> = last["content"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let results: Vec<Value> = ids
        .into_iter()
        .map(|id| json!({ "type": "tool_result", "tool_use_id": id, "is_error": true, "content": why }))
        .collect();
    messages.push(json!({ "role": "user", "content": results }));
}

/// 往历史里加一条用户消息。上一条也是用户消息（工具结果、或上一轮被拒之后留下的）就并进去：
/// 角色要交替，tool_result 必须紧跟在对应的 tool_use 后面那条 user 消息里
pub fn push_user_content(messages: &mut Vec<Value>, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last["role"] == "user"
    {
        let content = match last["content"].take() {
            Value::String(s) => vec![json!({ "type": "text", "text": s })],
            Value::Array(a) => a,
            _ => Vec::new(),
        };
        last["content"] = Value::Array(content.into_iter().chain(blocks).collect());
        return;
    }
    messages.push(json!({ "role": "user", "content": blocks }));
}

/// 发请求的 HTTP 客户端：TLS 与 px0 下载同一套（rustls + ring + 系统信任库，只讲 HTTP/1.1）。
/// **不设整体超时**：一轮长回答能流好几分钟，空闲超时由调用方逐块计（hub.rs 的 STREAM_IDLE）
pub fn http_client() -> Result<reqwest::Client, String> {
    use std::sync::{Arc, LazyLock};
    static CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
        use rustls_platform_verifier::BuilderVerifierExt as _;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_platform_verifier())
            .map_err(|e| format!("TLS 初始化失败：{e}"))?
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .connect_timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("falcon/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("建 HTTP 客户端失败：{e}"))
    });
    CLIENT.clone()
}

/// 工具定义的 JSON（不带 eager，[`request_body`] 按地址补）
pub fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    schema.insert("properties".into(), properties);
    schema.insert("required".into(), json!(required));
    schema.insert("additionalProperties".into(), Value::Bool(false));
    json!({ "name": name, "description": description, "input_schema": Value::Object(schema) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base: &str) -> ApiConfig {
        ApiConfig { base_url: base.into(), api_key: "k".into(), model: "claude-opus-5-5".into(), effort: "high".into() }
    }

    #[test]
    fn official_endpoint_gets_eager_and_fallbacks() {
        let t = [tool("run_command", "d", json!({ "command": { "type": "string" } }), &["command"])];
        let b = request_body(&cfg(""), "sys", &t, &[]);
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(headers(&cfg("")).iter().any(|(k, v)| *k == "anthropic-beta" && v == FALLBACK_BETA));
        assert_eq!(cfg("").endpoint(), "https://api.anthropic.com/v1/messages");

        // 网关：两个字段与 beta 头都不带
        let g = cfg("https://gw.example.com/");
        let b = request_body(&g, "sys", &t, &[]);
        assert!(b["tools"][0].get("eager_input_streaming").is_none());
        assert!(b.get("fallbacks").is_none());
        assert!(!headers(&g).iter().any(|(k, _)| *k == "anthropic-beta"));
        assert_eq!(g.endpoint(), "https://gw.example.com/v1/messages");
    }

    #[test]
    fn sse_lines_split_across_chunks() {
        let mut p = SseParser::default();
        let mut got = p.push("event: ping\ndata: {\"type\":\"pi");
        got.extend(p.push("ng\"}\r\n\r\nevent: x\ndata: {\"a\":1}\n"));
        got.extend(p.push("\n"));
        assert_eq!(got, ["{\"type\":\"ping\"}", "{\"a\":1}"]);
    }

    fn feed(acc: &mut Accumulator, events: &[Value]) -> Vec<Delta> {
        events.iter().flat_map(|e| acc.apply(&e.to_string())).collect()
    }

    #[test]
    fn accumulates_thinking_text_and_tool_use_verbatim() {
        let mut acc = Accumulator::default();
        let deltas = feed(
            &mut acc,
            &[
                json!({"type":"message_start","message":{"model":"claude-opus-5-5"}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想一想"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"先看看"}}),
                json!({"type":"content_block_stop","index":1}),
                json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"run_command","input":{}}}),
                json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"ls"}}),
                json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":" -la\"}"}}),
                json!({"type":"content_block_stop","index":2}),
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
                json!({"type":"message_stop"}),
            ],
        );
        assert_eq!(acc.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(acc.stop_reason.as_deref(), Some("tool_use"));
        assert!(deltas.contains(&Delta::ToolStart { index: 2, id: "toolu_1".into(), name: "run_command".into() }));
        assert!(deltas.contains(&Delta::Thinking { index: 0, text: "想一想".into() }));
        let echo = echo_content(&acc.blocks);
        assert_eq!(echo[0], json!({"type":"thinking","thinking":"想一想","signature":"sig"}));
        assert_eq!(echo[1], json!({"type":"text","text":"先看看"}));
        assert_eq!(echo[2]["input"], json!({"command":"ls -la"}));
        assert_eq!(acc.tool_uses().len(), 1);
    }

    #[test]
    fn invalid_tool_json_is_kept_raw_and_not_parsed() {
        let mut acc = Accumulator::default();
        feed(
            &mut acc,
            &[
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"write_file","input":{}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"a\", \"content\": \"x"}}),
                json!({"type":"content_block_stop","index":0}),
            ],
        );
        let b = &acc.blocks[0];
        assert_eq!(b.invalid_input.as_deref(), Some("{\"path\": \"a\", \"content\": \"x"));
        assert_eq!(b.value["input"], json!({}));
    }

    #[test]
    fn content_before_a_fallback_boundary_echoes_only_text() {
        let mut acc = Accumulator::default();
        feed(
            &mut acc,
            &[
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"x","signature":"s"}}),
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"partial"}}),
                json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"a","name":"run_command","input":{}}}),
                json!({"type":"content_block_stop","index":2}),
                json!({"type":"content_block_start","index":3,"content_block":{"type":"fallback","from":{"model":"m1"},"to":{"model":"m2"}}}),
                json!({"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"b","name":"run_command","input":{}}}),
                json!({"type":"content_block_stop","index":4}),
            ],
        );
        let echo = echo_content(&acc.blocks);
        let kinds: Vec<&str> = echo.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["text", "tool_use"]);
        assert_eq!(echo[1]["id"], "b");
        let tools: Vec<&str> = acc.tool_uses().iter().map(|b| b.value["id"].as_str().unwrap()).collect();
        assert_eq!(tools, ["b"]);
    }

    #[test]
    fn dangling_tool_uses_get_error_results_and_user_turns_merge() {
        let mut m = vec![
            json!({"role":"user","content":[{"type":"text","text":"hi"}]}),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"x","input":{}}]}),
        ];
        close_dangling_tool_uses(&mut m, "用户拒绝了");
        assert_eq!(m[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(m[2]["content"][0]["is_error"], true);
        push_user_content(&mut m, vec![json!({"type":"text","text":"换个办法"})]);
        assert_eq!(m.len(), 3);
        assert_eq!(m[2]["content"][1]["text"], "换个办法");
        // 没有悬着的就什么都不加
        close_dangling_tool_uses(&mut m, "x");
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn api_errors_read_like_sentences() {
        assert_eq!(
            error_message(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#),
            "HTTP 401：invalid x-api-key"
        );
        assert_eq!(error_message(502, "bad gateway"), "HTTP 502：bad gateway");
        assert!(retryable(529) && retryable(429) && !retryable(400));
    }
}
