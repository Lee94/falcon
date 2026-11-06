//! Incremental TSP output observation and bounded, replayable surface documents.
//!
//! This module never answers queries or acknowledges frames. Feed its tokens to the
//! holder's query gate and apply only `Tsp` tokens to `State`. Snapshot writers must
//! append `Parser::replay_pending()` after `State::replay()` and propagate errors:
//! a partial APC or chunk sequence belongs to the same output offset as the tree.
//! Holder upgrades use `State::checkpoint()` instead: display replay rebases ages,
//! while checkpoints also retain the original ages used to recognize agent patches.
//! Picker deltas are folded into catalogs; ordinary text appends never scan timers.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};

/// Shared with the holder's maximum snapshot payload. The caller still checks
/// the combined VT + native payload against its own remaining space.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_APC_BYTES: usize = 262_144;
const CHUNK_BYTES: usize = 65_536;
const MAX_DEPTH: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error(message: impl Into<String>) -> Error {
    Error(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub verb: u8,
    pub params: Vec<(String, String)>,
    /// Raw JSON UTF-8, or base64 for `b`. Joined before decoding either format.
    pub body: Vec<u8>,
}

impl Message {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Result<Value, Error> {
        serde_json::from_slice(&self.body).map_err(|err| error(format!("malformed TSP body: {err}")))
    }

    /// Queries and listening opens re-enable terminal replies. This deliberately
    /// does not manufacture a hello reply; the attached Tern must negotiate it.
    pub fn enables_replies(&self) -> bool {
        match self.verb {
            b'q' => true,
            b'o' => serde_json::from_slice::<Value>(&self.body)
                .ok().is_some_and(|body| body.get("listen") != Some(&Value::Bool(false))),
            _ => false,
        }
    }

    pub fn is_hello(&self) -> bool {
        self.verb == b'q' && serde_json::from_slice::<Value>(&self.body)
            .ok().is_some_and(|body| body.get("q").and_then(Value::as_str) == Some("hello"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Tsp(Message),
    /// A primary DA request, exactly CSI c or CSI 0 c (never a reply).
    Da1,
    Reset,
    Error(Error),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Escape,
    Csi,
    Apc,
    String,
    Discard,
}

struct Chunk {
    id: String,
    message: Message,
    updated: Instant,
    dropped: bool,
}

#[derive(Default)]
pub struct Parser {
    scan: Scan,
    string_escape: bool,
    pending: Vec<u8>,
    chunk: Option<Chunk>,
    failure: Option<Error>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fatal resource-limit failures only; interrupted/timed-out chunks recover.
    pub fn failure(&self) -> Option<&Error> { self.failure.as_ref() }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `end` is exclusive in this call's `bytes`, including a terminator that
    /// finishes a sequence begun in an earlier call. Ordinary text allocates no
    /// storage; OSC/DCS/PM/SOS are skipped without interpreting their contents.
    pub fn push(&mut self, bytes: &[u8], mut visit: impl FnMut(usize, Token)) {
        for (index, &byte) in bytes.iter().enumerate() {
            let end = index + 1;
            match self.scan {
                Scan::Ground => {
                    if byte == 0x1b {
                        self.scan = Scan::Escape;
                    }
                }
                Scan::Escape => match byte {
                    b'_' => {
                        self.pending.clear();
                        self.scan = Scan::Apc;
                        self.string_escape = false;
                    }
                    b'[' => {
                        self.pending.clear();
                        self.scan = Scan::Csi;
                    }
                    b']' | b'P' | b'^' | b'X' => {
                        self.scan = Scan::String;
                        self.string_escape = false;
                    }
                    b'c' => {
                        self.reset();
                        visit(end, Token::Reset);
                    }
                    0x1b => {}
                    _ => self.scan = Scan::Ground,
                },
                Scan::Csi => {
                    if byte == 0x1b {
                        self.pending.clear();
                        self.scan = Scan::Escape;
                    } else if (0x40..=0x7e).contains(&byte) {
                        if byte == b'c' && (self.pending.is_empty() || self.pending == b"0") {
                            visit(end, Token::Da1);
                        }
                        self.pending.clear();
                        self.scan = Scan::Ground;
                    } else if self.pending.len() < 32 {
                        self.pending.push(byte);
                    } else {
                        // A marker keeps an overlong CSI from becoming an empty DA1.
                        self.pending[0] = b'?';
                    }
                }
                Scan::Apc | Scan::String | Scan::Discard => {
                    let terminated = byte == 7 || (self.string_escape && byte == b'\\');
                    if terminated {
                        if self.scan == Scan::Apc {
                            if self.string_escape {
                                self.pending.pop();
                            }
                            let raw = std::mem::take(&mut self.pending);
                            self.finish(&raw, end, &mut visit);
                            self.pending = raw;
                            self.pending.clear();
                        }
                        self.scan = Scan::Ground;
                        self.string_escape = false;
                    } else {
                        self.string_escape = byte == 0x1b;
                        if self.scan == Scan::Apc {
                            if self.pending.len() > MAX_APC_BYTES || (self.pending.len() == MAX_APC_BYTES && byte != 0x1b) {
                                let failure = error("TSP APC exceeds 262144 bytes; native snapshot is incomplete");
                                self.failure = Some(failure.clone());
                                visit(end, Token::Error(failure));
                                self.pending.clear();
                                self.scan = Scan::Discard;
                            } else {
                                self.pending.push(byte);
                                if self.pending.len() <= 4 && !b"tsp;".starts_with(&self.pending) {
                                    self.pending.clear();
                                    self.scan = Scan::String;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self, raw: &[u8], end: usize, visit: &mut impl FnMut(usize, Token)) {
        let Some(message) = parse_message(raw) else { return };
        if self.chunk.as_ref().is_some_and(|chunk| chunk.updated.elapsed() > Duration::from_secs(1)) {
            self.chunk = None;
            visit(end, Token::Error(error("TSP chunk sequence timed out")));
        }
        let chunk_id = message.param("c").map(str::to_owned);
        let more = message.param("m") == Some("1");
        if self.chunk.as_ref().is_some_and(|chunk| {
            Some(chunk.id.as_str()) != chunk_id.as_deref() || chunk.message.verb != message.verb
        }) {
            self.chunk = None;
            visit(end, Token::Error(error("TSP chunk sequence interrupted")));
        }
        if let Some(chunk) = self.chunk.as_mut() {
            chunk.updated = Instant::now();
            if !chunk.dropped {
                if chunk.message.body.len().saturating_add(message.body.len()) > MAX_SNAPSHOT_BYTES {
                    chunk.dropped = true;
                    chunk.message.body.clear();
                    let failure = error("TSP joined message exceeds native snapshot limit");
                    self.failure = Some(failure.clone());
                    visit(end, Token::Error(failure));
                } else {
                    chunk.message.body.extend_from_slice(&message.body);
                }
            }
            if !more {
                let chunk = self.chunk.take().unwrap();
                if !chunk.dropped {
                    visit(end, Token::Tsp(chunk.message));
                }
            }
        } else if let Some(id) = chunk_id {
            if more {
                self.chunk = Some(Chunk { id, message, updated: Instant::now(), dropped: false });
            } else {
                visit(end, Token::Tsp(message));
            }
        } else {
            visit(end, Token::Tsp(message));
        }
    }

    /// Replays pending chunks before an unfinished APC. Never UTF-8 decodes these
    /// bytes: a write boundary may be in the middle of a JSON string or scalar.
    pub fn replay_pending(&self) -> Result<Vec<u8>, Error> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let mut out = Limited::new(MAX_SNAPSHOT_BYTES);
        if let Some(chunk) = &self.chunk {
            if chunk.updated.elapsed() > Duration::from_secs(1) {
                return Err(error("TSP pending chunk sequence timed out"));
            }
            let params: Vec<_> = chunk.message.params.iter()
                .filter(|(key, _)| key != "c" && key != "m").cloned()
                .chain([(String::from("c"), chunk.id.clone()), (String::from("m"), String::from("1"))])
                .collect();
            emit_chunks(&mut out, chunk.message.verb, &params, &chunk.message.body, true)?;
        }
        match self.scan {
            Scan::Escape => out.extend(b"\x1b")?,
            Scan::Apc => {
                out.extend(b"\x1b_")?;
                out.extend(&self.pending)?;
            }
            // The VT snapshot owns other terminal control strings. This scanner
            // must not replay an incomplete CSI twice or append foreign strings.
            _ => {}
        }
        Ok(out.bytes)
    }

    /// Whether the current unfinished string is recognizably TSP. A lone ESC
    /// is intentionally false: the grid continuation already owns that prefix.
    pub fn pending_is_tsp(&self) -> bool {
        self.scan == Scan::Apc && (self.pending.starts_with(b"tsp;") || b"tsp;".starts_with(&self.pending))
    }
}

fn parameter_prefix(body: &[u8]) -> bool {
    let key_end = body.iter().position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_' && *byte != b'-');
    let Some(key_end) = key_end.filter(|&end| end > 0 && body[end] == b'=') else { return false };
    let value = &body[key_end + 1..];
    let Some(end) = value.iter().position(|&byte| byte == b';') else { return false };
    value[..end].iter().all(|byte| (0x21..=0x7e).contains(byte))
}

fn parse_message(raw: &[u8]) -> Option<Message> {
    let rest = raw.strip_prefix(b"tsp;")?;
    if rest.len() < 2 || rest[1] != b';' { return None }
    let verb = rest[0];
    let mut body = &rest[2..];
    let mut params = Vec::new();
    while let Some(end) = body.iter().position(|&byte| byte == b';') {
        let part = &body[..end];
        let Some(equal) = part.iter().position(|&byte| byte == b'=') else { break };
        if equal == 0 || !part[..equal].iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
            || !part[equal + 1..].iter().all(|byte| (0x21..=0x7e).contains(byte) && *byte != b';') {
            break;
        }
        params.push((String::from_utf8(part[..equal].to_vec()).ok()?, String::from_utf8(part[equal + 1..].to_vec()).ok()?));
        body = &body[end + 1..];
    }
    Some(Message { verb, params, body: body.to_vec() })
}

struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}

impl Limited {
    fn new(limit: usize) -> Self { Self { bytes: Vec::new(), limit } }
    fn extend(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(error("TSP snapshot exceeds byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.extend(bytes).map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn emit_chunks(out: &mut Limited, verb: u8, params: &[(String, String)], body: &[u8], pending: bool) -> Result<(), Error> {
    let header = 6 + params.iter().filter(|(key, _)| key != "c" && key != "m")
        .map(|(key, value)| key.len() + value.len() + 2).sum::<usize>();
    let available = MAX_APC_BYTES.checked_sub(header).ok_or_else(|| error("TSP replay parameters exceed APC limit"))?;
    let chunked = body.len() > CHUNK_BYTES.min(available) || pending;
    let id = params.iter().find(|(key, _)| key == "c").map(|(_, value)| value.as_bytes()).unwrap_or(b"falcon-replay");
    let chunk_bytes = if chunked {
        CHUNK_BYTES.min(available.checked_sub(id.len() + 7).ok_or_else(|| error("TSP replay chunk header exceeds APC limit"))?)
    } else { CHUNK_BYTES.min(available) };
    if chunk_bytes == 0 && !body.is_empty() { return Err(error("TSP replay has no room for APC payload")); }
    let mut offset = 0;
    loop {
        let mut end = (offset + chunk_bytes).min(body.len());
        // A continuation must not look like a parameter. UTF-8 boundaries are
        // irrelevant here: Tern joins bytes before JSON/base64 decoding.
        if end < body.len() && parameter_prefix(&body[end..(end + chunk_bytes).min(body.len())]) {
            while end > offset && (body[end].is_ascii_alphanumeric() || matches!(body[end], b'_' | b'-')) {
                end -= 1;
            }
        }
        if end == offset && end < body.len() {
            return Err(error("TSP cannot choose a safe chunk boundary"));
        }
        out.extend(b"\x1b_tsp;")?;
        out.extend(&[verb, b';'])?;
        for (key, value) in params {
            if key == "c" || key == "m" { continue }
            out.extend(key.as_bytes())?;
            out.extend(b"=")?;
            out.extend(value.as_bytes())?;
            out.extend(b";")?;
        }
        if chunked {
            out.extend(b"c=")?;
            out.extend(id)?;
            out.extend(b";")?;
            if end != body.len() || pending { out.extend(b"m=1;")?; }
        }
        out.extend(&body[offset..end])?;
        out.extend(b"\x1b\\")?;
        if end == body.len() { break }
        offset = end;
    }
    Ok(())
}

#[derive(Debug)]
struct Node {
    /// All original fields except `c`; future node fields survive snapshots.
    fields: Map<String, Value>,
    field_bytes: usize,
    children: Vec<String>,
    parent: Option<String>,
    settled: bool,
    /// Direct age, then agent stats/tool/retry ages. Streamed text never scans timers.
    clocks: [Option<AgeClock>; 4],
}

#[derive(Clone, Copy, Debug)]
struct AgeClock {
    age: f64,
    source_age: f64,
    received: Instant,
}

impl AgeClock {
    fn new(age: f64, received: Instant) -> Self { Self { age, source_age: age, received } }

    fn at(self, now: Instant) -> f64 {
        let age = self.age + now.saturating_duration_since(self.received).as_secs_f64() * 1000.0;
        // The protocol distinguishes countdowns by the age's sign alone. Keep
        // the last millisecond after expiry so replay cannot start counting up;
        // the display rounds this to zero and the timing error is at most 1 ms.
        if self.age < 0.0 { age.min(-1.0) }
        else { age.min(f64::MAX) }
    }
}

impl Node {
    fn observe_clocks(kind: Option<&str>, clocks: &mut [Option<AgeClock>; 4], updates: &Map<String, Value>, now: Instant) {
        match kind {
            Some("elapsed") if updates.contains_key("age") => {
                clocks[0] = Some(AgeClock::new(updates["age"].as_f64().unwrap_or(0.0), now));
            }
            Some("tool") if updates.contains_key("age") => {
                clocks[0] = updates["age"].as_f64().map(|age| AgeClock::new(age, now));
            }
            Some("agent") => {
                for (index, key) in ["stats", "tool", "retry"].iter().enumerate() {
                    let Some(value) = updates.get(*key) else { continue };
                    let age = if *key == "retry" && value.is_object() {
                        Some(value.get("age").and_then(Value::as_f64).unwrap_or(0.0))
                    } else { value.get("age").and_then(Value::as_f64) };
                    let previous = clocks[index + 1].map(|clock| clock.source_age);
                    if age != previous || clocks[index + 1].is_none() {
                        clocks[index + 1] = age.map(|age| AgeClock::new(age, now));
                    }
                }
            }
            _ => {}
        }
    }

    fn restore_source_ages(&mut self) {
        if self.fields.get("k").and_then(Value::as_str) != Some("agent") { return }
        let Some(metadata) = self.fields.get("_falconReplayAges").and_then(Value::as_object) else { return };
        let Some(ages) = metadata.get("ages").and_then(Value::as_array).filter(|ages| ages.len() == 3) else { return };
        if metadata.get("v").and_then(Value::as_u64) != Some(1) { return }
        for (index, age) in ages.iter().enumerate() {
            if let (Some(clock), Some(age)) = (&mut self.clocks[index + 1], age.as_f64()) { clock.source_age = age; }
        }
        // An application field with this name is carried inside the envelope.
        let original = metadata.get("original").cloned();
        self.fields.remove("_falconReplayAges");
        if let Some(original) = original { self.fields.insert("_falconReplayAges".into(), original); }
        self.field_bytes = object_cost(&self.fields) + 256;
    }
}

#[derive(Debug)]
struct Surface {
    open: Map<String, Value>,
    nodes: HashMap<String, Node>,
    stored: usize,
    styles: Vec<(String, String)>,
    sequence: Value,
    focus: Option<String>,
    reveal: Option<Value>,
    scroll: Option<Value>,
    suspended: bool,
    closed: bool,
}

impl Surface {
    fn id(&self) -> &str { self.open["id"].as_str().unwrap() }
    fn mode(&self) -> &str { self.open.get("mode").and_then(Value::as_str).unwrap_or("inline") }

    fn new(open: Map<String, Value>) -> Self {
        let id = open["id"].as_str().unwrap().to_owned();
        let fields = json!({"id":id,"k":"col"}).as_object().unwrap().clone();
        let field_bytes = object_cost(&fields) + 256;
        let root = Node { fields, field_bytes, children: Vec::new(), parent: None, settled: false, clocks: [None; 4] };
        let stored = node_cost(&root);
        Self { open, nodes: HashMap::from([(id, root)]), stored, styles: Vec::new(), sequence: json!(0), focus: None, reveal: None, scroll: None, suspended: false, closed: false }
    }

    fn wake(&mut self, id: &str) {
        let Some(node) = self.nodes.get_mut(id) else { return };
        node.settled = false;
        // Move the parent link out while walking: no id copies on each streamed
        // append, and no mutable map borrow survives the recursive lookup.
        if let Some(parent) = node.parent.take() {
            self.wake(&parent);
            self.nodes.get_mut(id).unwrap().parent = Some(parent);
        }
    }

    fn remove(&mut self, id: &str) {
        let Some(node) = self.nodes.remove(id) else { return };
        self.stored -= node_cost(&node);
        if let Some(parent) = node.parent.as_ref().and_then(|id| self.nodes.get_mut(id)) {
            parent.children.retain(|child| child != id);
            self.stored -= id.len();
        }
        if self.focus.as_deref() == Some(id) { self.focus = None; }
        if self.reveal.as_ref().and_then(|op| op.get(1)).and_then(Value::as_str) == Some(id) { self.reveal = None; }
        if self.scroll.as_ref().and_then(|op| op.get(1)).and_then(Value::as_str) == Some(id) { self.scroll = None; }
        for child in node.children { self.remove(&child); }
    }

    fn close(&mut self) {
        for id in ["dock", "layer"] {
            if self.nodes.get(id).is_some_and(|node| node.parent.as_deref() == Some(self.id())) {
                self.remove(id);
            }
        }
        self.focus = None;
        self.suspended = false;
        self.closed = true;
    }

    fn depth(&self, id: &str) -> usize {
        let mut depth = 0;
        let mut current = Some(id);
        while let Some(id) = current {
            depth += 1;
            current = self.nodes.get(id).and_then(|node| node.parent.as_deref());
        }
        depth
    }

    fn subtree_depth(&self, id: &str) -> usize {
        self.nodes[id].children.iter().map(|child| self.subtree_depth(child)).max().unwrap_or(0) + 1
    }

    fn op(&mut self, op: &Value, checkpoint: bool) -> Result<(), Error> {
        let args = op.as_array().ok_or_else(|| error("TSP op is not an array"))?;
        let verb = arg_str(args, 0)?;
        match verb {
            "suspend" => self.suspended = true,
            "resume" => self.suspended = false,
            "focus" => {
                let target = args.get(1).ok_or_else(|| error("missing focus target"))?;
                if target.is_null() { self.focus = None; }
                else {
                    let id = target.as_str().ok_or_else(|| error("invalid focus target"))?;
                    let node = self.nodes.get(id).ok_or_else(|| error("unknown focus target"))?;
                    if !matches!(node.fields.get("k").and_then(Value::as_str), Some("editor" | "input")) {
                        return Err(error("focus target is not editable"));
                    }
                    self.focus = Some(id.to_owned());
                }
            }
            "add" => {
                let id = arg_str(args, 1)?;
                let parent = arg_str(args, 2)?;
                let before = before_arg(args, 3)?;
                let parent_node = self.nodes.get(parent).ok_or_else(|| error("unknown add parent"))?;
                let position = insertion_position(&parent_node.children, before)?;
                let tree = args.get(4).ok_or_else(|| error("missing add tree"))?;
                if tree.get("id").and_then(Value::as_str) != Some(id) { return Err(error("add id does not match node")); }
                let mut incoming = HashMap::new();
                collect_nodes(tree.clone(), Some(parent.to_owned()), &self.nodes, &mut incoming, self.depth(parent) + 1, checkpoint)?;
                self.wake(parent);
                self.stored += incoming.values().map(node_cost).sum::<usize>() + id.len();
                self.nodes.extend(incoming);
                self.nodes.get_mut(parent).unwrap().children.insert(position, id.to_owned());
            }
            "set" | "text" | "splice" => {
                let id = arg_str(args, 1)?;
                let node = self.nodes.get_mut(id).ok_or_else(|| error("unknown node"))?;
                let old_cost = node.field_bytes;
                if verb == "set" {
                    let updates = args.get(2).and_then(Value::as_object).ok_or_else(|| error("invalid set props"))?;
                    Node::observe_clocks(node.fields.get("k").and_then(Value::as_str), &mut node.clocks, updates, Instant::now());
                    let picker = node.fields.get("k").and_then(Value::as_str) == Some("picker");
                    for (key, value) in updates {
                        if !picker || !matches!(key.as_str(), "items" | "itemsAdd" | "itemsDel") {
                            update_prop(node, key, value.clone());
                        }
                    }
                    if picker { update_picker(node, updates); }
                } else {
                    if !matches!(node.fields.get("k").and_then(Value::as_str), Some("text" | "md" | "code" | "ansi" | "math" | "editor" | "input" | "shimmer" | "el")) {
                        return Err(error("node has no primary text"));
                    }
                    if verb == "text" {
                        let mode = arg_str(args, 2)?;
                        let text = arg_str(args, 3)?;
                        if !matches!(mode, "append" | "replace") { return Err(error("invalid text mode")); }
                        if mode == "replace" {
                            update_prop(node, "text", Value::String(text.to_owned()));
                        } else if node.fields.get("p").and_then(|props| props.get("text")).is_none() {
                            update_prop(node, "text", Value::String(text.to_owned()));
                        } else {
                            let value = props_mut(node).get_mut("text").unwrap();
                            let Value::String(value) = value else { return Err(error("primary text is not a string")) };
                            value.push_str(text);
                            node.field_bytes += text.len();
                        }
                    } else {
                        let at = args.get(2).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| error("invalid splice offset"))?;
                        let del = args.get(3).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| error("invalid splice length"))?;
                        let replacement = arg_str(args, 4)?;
                        let text = node.fields.get("p").and_then(|props| props.get("text")).map_or(Some(""), Value::as_str).ok_or_else(|| error("primary text is not a string"))?;
                        let start = utf16_offset(text, at).ok_or_else(|| error("splice starts inside surrogate pair or outside text"))?;
                        let end_units = at.checked_add(del).ok_or_else(|| error("splice length overflow"))?;
                        let end = utf16_offset(text, end_units).ok_or_else(|| error("splice ends inside surrogate pair or outside text"))?;
                        if node.fields.get("p").and_then(|props| props.get("text")).is_none() {
                            update_prop(node, "text", Value::String(replacement.to_owned()));
                        } else {
                            let Value::String(text) = props_mut(node).get_mut("text").unwrap() else { unreachable!() };
                            text.replace_range(start..end, replacement);
                            node.field_bytes = node.field_bytes - (end - start) + replacement.len();
                        }
                    }
                }
                self.stored = self.stored - old_cost + node.field_bytes;
                self.wake(id);
            }
            "move" => {
                let id = arg_str(args, 1)?;
                let parent = arg_str(args, 2)?;
                let before = before_arg(args, 3)?;
                let old_parent = self.nodes.get(id).and_then(|node| node.parent.clone()).ok_or_else(|| error("cannot move root or unknown node"))?;
                let new_parent = self.nodes.get(parent).ok_or_else(|| error("unknown move parent"))?;
                insertion_position(&new_parent.children, before)?;
                if before == Some(id) { return Err(error("cannot move before itself")); }
                let mut ancestor = Some(parent);
                while let Some(current) = ancestor {
                    if current == id { return Err(error("cannot move node into its subtree")); }
                    ancestor = self.nodes.get(current).and_then(|node| node.parent.as_deref());
                }
                if self.depth(parent) + self.subtree_depth(id) > MAX_DEPTH { return Err(error("moved tree exceeds depth limit")); }
                self.wake(&old_parent);
                self.wake(parent);
                self.wake(id);
                self.nodes.get_mut(&old_parent).unwrap().children.retain(|child| child != id);
                let children = &mut self.nodes.get_mut(parent).unwrap().children;
                let position = insertion_position(children, before)?;
                children.insert(position, id.to_owned());
                self.stored = self.stored - old_parent.len() + parent.len();
                self.nodes.get_mut(id).unwrap().parent = Some(parent.to_owned());
            }
            "del" => {
                let id = arg_str(args, 1)?;
                let parent = self.nodes.get(id).and_then(|node| node.parent.clone()).ok_or_else(|| error("cannot delete root or unknown node"))?;
                self.wake(&parent);
                self.remove(id);
            }
            "settle" => {
                let id = arg_str(args, 1)?;
                self.nodes.get_mut(id).ok_or_else(|| error("unknown settle node"))?.settled = true;
            }
            "reveal" | "scroll" => {
                let id = arg_str(args, 1)?;
                if !self.nodes.contains_key(id) { return Err(error("unknown scroll/reveal node")); }
                let mode = arg_str(args, 2)?;
                let valid = if verb == "reveal" { matches!(mode, "start" | "end" | "nearest") }
                    else { matches!(mode, "line-up" | "line-down" | "page-up" | "page-down" | "start" | "end") };
                if !valid { return Err(error("invalid scroll/reveal mode")); }
                if verb == "reveal" { self.reveal = Some(op.clone()); }
                else { self.scroll = Some(op.clone()); }
            }
            _ => {}
        }
        Ok(())
    }
}

fn value_cost(value: &Value) -> usize {
    match value {
        Value::String(text) => 24 + text.len(),
        Value::Array(values) => 24 + values.iter().map(value_cost).sum::<usize>(),
        Value::Object(fields) => object_cost(fields),
        _ => 16,
    }
}

fn object_cost(fields: &Map<String, Value>) -> usize {
    24 + fields.iter().map(|(key, value)| 64 + key.len() + value_cost(value)).sum::<usize>()
}

fn node_cost(node: &Node) -> usize {
    node.field_bytes + node.parent.as_ref().map_or(0, String::len)
        + node.fields["id"].as_str().unwrap().len()
        + node.children.iter().map(String::len).sum::<usize>()
}

fn update_prop(node: &mut Node, key: &str, value: Value) {
    if !node.fields.contains_key("p") {
        node.fields.insert("p".into(), Value::Object(Map::new()));
        node.field_bytes += 64 + 1 + 24;
    }
    let props = props_mut(node);
    let old = props.get(key).map_or(0, |value| 64 + key.len() + value_cost(value));
    let new = if value.is_null() { 0 } else { 64 + key.len() + value_cost(&value) };
    if value.is_null() { props.remove(key); }
    else if let Some(previous) = props.get_mut(key) { *previous = value; }
    else { props.insert(key.to_owned(), value); }
    node.field_bytes = node.field_bytes - old + new;
}

// Picker deltas are commands, not persistent props. Apply replacement, upserts,
// then removals, retaining complete item objects and all unrelated future props.
fn fold_picker(props: &mut Map<String, Value>, additions: Option<&Value>, deletions: Option<&Value>) {
    let old = props.remove("items").and_then(|value| match value { Value::Array(items) => Some(items), _ => None }).unwrap_or_default();
    let additions = additions.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let deletions = deletions.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let removed: std::collections::HashSet<&str> = deletions.iter().filter_map(Value::as_str).collect();
    let mut positions = HashMap::new();
    let mut items = Vec::with_capacity(old.len() + additions.len());
    for item in old.into_iter().chain(additions.iter().cloned()) {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        if removed.contains(id) { continue }
        if let Some(&index) = positions.get(id) { items[index] = item; }
        else {
            positions.insert(id.to_owned(), items.len());
            items.push(item);
        }
    }
    props.insert("items".into(), Value::Array(items));
    props.remove("itemsAdd");
    props.remove("itemsDel");
}

fn update_picker(node: &mut Node, updates: &Map<String, Value>) {
    if !["items", "itemsAdd", "itemsDel"].iter().any(|key| updates.contains_key(*key)) { return }
    if let Some(items) = updates.get("items") { update_prop(node, "items", items.clone()); }
    if !node.fields.contains_key("p") { update_prop(node, "items", Value::Array(Vec::new())); }
    let old = object_cost(props_mut(node));
    fold_picker(props_mut(node), updates.get("itemsAdd"), updates.get("itemsDel"));
    node.field_bytes = node.field_bytes - old + object_cost(props_mut(node));
}

fn props_mut(node: &mut Node) -> &mut Map<String, Value> {
    let props = node.fields.entry("p").or_insert_with(|| Value::Object(Map::new()));
    // Input nodes are validated before insertion, and set never changes `p` itself.
    props.as_object_mut().unwrap()
}

fn arg_str(args: &[Value], index: usize) -> Result<&str, Error> {
    args.get(index).and_then(Value::as_str).ok_or_else(|| error("missing or invalid TSP string argument"))
}

fn before_arg(args: &[Value], index: usize) -> Result<Option<&str>, Error> {
    match args.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(error("missing or invalid before sibling")),
    }
}

fn insertion_position(children: &[String], before: Option<&str>) -> Result<usize, Error> {
    before.map_or(Ok(children.len()), |id| children.iter().position(|child| child == id).ok_or_else(|| error("before is not a child of parent")))
}

fn collect_nodes(value: Value, parent: Option<String>, existing: &HashMap<String, Node>, incoming: &mut HashMap<String, Node>, depth: usize, checkpoint: bool) -> Result<(), Error> {
    if depth > MAX_DEPTH { return Err(error("TSP tree exceeds depth limit")); }
    let Value::Object(mut fields) = value else { return Err(error("node is not an object")) };
    let id = fields.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| error("invalid node id"))?.to_owned();
    if fields.get("k").and_then(Value::as_str).is_none_or(str::is_empty) { return Err(error("invalid node kind")); }
    if existing.contains_key(&id) || incoming.contains_key(&id) { return Err(error("duplicate node id")); }
    if let Some(props) = fields.get_mut("p") {
        let props = props.as_object_mut().ok_or_else(|| error("node props are not an object"))?;
        props.retain(|_, value| !value.is_null());
    }
    let children = match fields.remove("c") {
        None => Vec::new(),
        Some(Value::Array(children)) => children,
        _ => return Err(error("node children are not an array")),
    };
    let child_ids = children.iter().map(|child| child.get("id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| error("invalid child id"))).collect::<Result<Vec<_>, _>>()?;
    let kind = fields.get("k").and_then(Value::as_str);
    let picker = kind == Some("picker");
    let mut clocks = [None; 4];
    if let Some(props) = fields.get("p").and_then(Value::as_object) {
        Node::observe_clocks(kind, &mut clocks, props, Instant::now());
    }
    if kind == Some("elapsed") && clocks[0].is_none() {
        clocks[0] = Some(AgeClock::new(0.0, Instant::now()));
    }
    if picker {
        if let Some(props) = fields.get_mut("p").and_then(Value::as_object_mut) {
            let additions = props.remove("itemsAdd");
            let deletions = props.remove("itemsDel");
            fold_picker(props, additions.as_ref(), deletions.as_ref());
        }
    }
    let field_bytes = object_cost(&fields) + 256;
    let mut node = Node { fields, field_bytes, children: child_ids, parent, settled: false, clocks };
    if checkpoint { node.restore_source_ages(); }
    incoming.insert(id.clone(), node);
    for child in children { collect_nodes(child, Some(id.clone()), existing, incoming, depth + 1, checkpoint)?; }
    Ok(())
}

fn utf16_offset(text: &str, target: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == target { return Some(byte); }
        units += ch.len_utf16();
        if units > target { return None; }
    }
    (units == target).then_some(text.len())
}

/// The current semantic state, not an output log. Invalid individual frame ops
/// return the first error only after subsequent ops have still been applied.
#[derive(Default)]
pub struct State {
    hello: Option<Value>,
    surfaces: Vec<Surface>,
    blobs: Vec<Message>,
    failure: Option<Error>,
}

impl State {
    pub fn new() -> Self { Self::default() }
    pub fn reset(&mut self) { *self = Self::default(); }

    pub fn apply(&mut self, message: &Message) -> Result<(), Error> {
        if let Some(failure) = &self.failure { return Err(failure.clone()); }
        let result = self.apply_inner(message);
        // Count stored representation without allocating a second document or
        // serializing the entire replay on each streaming text append.
        if self.storage_bytes()? > MAX_SNAPSHOT_BYTES {
            let failure = error("TSP retained state exceeds 64 MiB; native snapshot is incomplete");
            self.failure = Some(failure.clone());
            self.hello = None;
            self.surfaces.clear();
            self.blobs.clear();
            return Err(failure);
        }
        result
    }

    fn apply_inner(&mut self, message: &Message) -> Result<(), Error> {
        if !matches!(message.verb, b'q' | b'o' | b'f' | b'b' | b's' | b'x') { return Ok(()) }
        if message.body.len() > MAX_SNAPSHOT_BYTES {
            let failure = error("TSP message exceeds 64 MiB native snapshot limit");
            self.failure = Some(failure.clone());
            return Err(failure);
        }
        if message.verb == b'b' {
            if message.body.len() > 24 * 1024 * 1024 { return Err(error("TSP blob body exceeds 24 MiB")); }
            let mut decoded = 0usize;
            if message.body.len() % 4 != 0 { return Err(error("invalid TSP base64 length")); }
            let mut scratch = [0u8; 3];
            for (index, group) in message.body.chunks_exact(4).enumerate() {
                let count = base64::engine::general_purpose::STANDARD.decode_slice(group, &mut scratch)
                    .map_err(|err| error(format!("invalid TSP blob: {err}")))?;
                if count < 3 && (index + 1) * 4 != message.body.len() { return Err(error("TSP blob has interior padding")); }
                decoded += count;
            }
            if decoded > 16 * 1024 * 1024 { return Err(error("TSP decoded blob exceeds 16 MiB")); }
            if !self.blobs.iter().any(|blob| blob.body == message.body) { self.blobs.push(message.clone()); }
            return Ok(())
        }
        let body: Value = serde_json::from_slice(&message.body).map_err(|err| error(format!("malformed TSP {} body: {err}", message.verb as char)))?;
        match message.verb {
            b'q' => if body.get("q").and_then(Value::as_str) == Some("hello") { self.hello = Some(body); },
            b'o' => self.open(body)?,
            b'f' => {
                let id = body.get("sf").and_then(Value::as_str).ok_or_else(|| error("frame has no sf"))?;
                let ops = body.get("ops").and_then(Value::as_array).ok_or_else(|| error("frame ops are not an array"))?;
                let surface = self.surfaces.iter_mut().find(|surface| !surface.closed && surface.id() == id).ok_or_else(|| error("frame surface is unknown or closed"))?;
                if let Some(sequence) = body.get("s") { surface.sequence = sequence.clone(); }
                let mut first_error = None;
                let checkpoint = body.get("_falconCheckpoint") == Some(&Value::Bool(true));
                for op in ops {
                    if let Err(failure) = surface.op(op, checkpoint) { first_error.get_or_insert(failure); }
                }
                if let Some(failure) = first_error { return Err(failure); }
            }
            b's' => {
                let id = body.get("sf").and_then(Value::as_str);
                let surface = if let Some(id) = id {
                    self.surfaces.iter_mut().rev().find(|surface| surface.id() == id)
                } else { self.surfaces.iter_mut().rev().find(|surface| !surface.closed) }.ok_or_else(|| error("stylesheet has no surface"))?;
                let name = body.get("name").and_then(Value::as_str).filter(|name| !name.is_empty() && name.len() <= 64 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')).ok_or_else(|| error("invalid stylesheet name"))?;
                let css = match body.get("css") { None | Some(Value::Null) => "", Some(Value::String(css)) => css, _ => return Err(error("invalid stylesheet CSS")) };
                let position = surface.styles.iter().position(|(key, _)| key == name);
                let total = surface.styles.iter().filter(|(key, _)| key != name).map(|(_, css)| css.len()).sum::<usize>() + css.len();
                if total > 256 * 1024 { return Err(error("surface CSS exceeds 256 KiB")); }
                if css.is_empty() { if let Some(position) = position { surface.styles.remove(position); } }
                else if let Some(position) = position { surface.styles[position].1 = css.to_owned(); }
                else { surface.styles.push((name.to_owned(), css.to_owned())); }
            }
            b'x' => {
                let id = body.get("id").and_then(Value::as_str).ok_or_else(|| error("close has no id"))?;
                let position = self.surfaces.iter().position(|surface| !surface.closed && surface.id() == id).ok_or_else(|| error("close surface is unknown or closed"))?;
                if body.get("keep") == Some(&Value::Bool(false)) || self.surfaces[position].mode() == "screen" {
                    self.surfaces.remove(position);
                } else { self.surfaces[position].close(); }
            }
            _ => {}
        }
        Ok(())
    }

    fn open(&mut self, body: Value) -> Result<(), Error> {
        let Value::Object(mut open) = body else { return Err(error("surface open is not an object")) };
        let id = open.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| error("surface has no id"))?.to_owned();
        let adopting = open.remove("adopt") == Some(Value::Bool(true));
        let adopted = if adopting {
            if self.surfaces.iter().any(|surface| !surface.closed && surface.id() == id) { return Ok(()) }
            Some(self.surfaces.iter().rposition(|surface| surface.closed && surface.mode() == "inline" && surface.id() == id).ok_or_else(|| error("adopt surface is gone"))?)
        } else { None };
        let mode = if adopting { "inline" } else { open.get("mode").and_then(Value::as_str).unwrap_or("inline") }.to_owned();
        if !matches!(mode.as_str(), "inline" | "screen" | "flow") { return Err(error("unknown surface mode")); }
        // Take the adopted document before removing/reordering replacements.
        let adopted = adopted.map(|position| self.surfaces.remove(position));
        let mut retained = Vec::with_capacity(self.surfaces.len() + 1);
        for mut surface in self.surfaces.drain(..) {
            if !surface.closed && (surface.mode() == mode || (mode == "inline" && surface.mode() == "screen")) {
                if surface.mode() == "screen" { continue }
                surface.close();
            }
            retained.push(surface);
        }
        self.surfaces = retained;
        if let Some(mut surface) = adopted {
            for key in ["title", "role"] {
                if let Some(value) = open.remove(key) { surface.open.insert(key.to_owned(), value); }
            }
            for key in ["listen", "closable", "font"] {
                surface.open.remove(key);
                if let Some(value) = open.remove(key) { surface.open.insert(key.to_owned(), value); }
            }
            surface.closed = false;
            self.surfaces.push(surface);
        } else { self.surfaces.push(Surface::new(open)); }
        Ok(())
    }

    fn storage_bytes(&self) -> Result<usize, Error> {
        let mut total = self.hello.as_ref().map_or(0, value_cost);
        for surface in &self.surfaces {
            total = total.saturating_add(surface.stored + object_cost(&surface.open) + value_cost(&surface.sequence));
            total = total.saturating_add(surface.styles.iter().map(|(name, css)| name.len() + css.len() + 64).sum::<usize>());
            total = total.saturating_add(surface.focus.as_ref().map_or(0, String::len));
            total = total.saturating_add(surface.reveal.as_ref().map_or(0, value_cost));
            total = total.saturating_add(surface.scroll.as_ref().map_or(0, value_cost));
        }
        total = total.saturating_add(self.blobs.iter().map(|blob| blob.body.len() + blob.params.iter().map(|(key, value)| key.len() + value.len() + 64).sum::<usize>()).sum::<usize>());
        Ok(total)
    }

    pub fn replay(&self) -> Result<Vec<u8>, Error> {
        self.replay_with_limit(MAX_SNAPSHOT_BYTES)
    }

    /// Lets the VT reserve the exact space left after its grid snapshot.
    pub fn replay_with_limit(&self, limit: usize) -> Result<Vec<u8>, Error> {
        self.replay_inner(limit, false)
    }

    /// Internal holder-upgrade stream. Feed back into `State`, never to Tern:
    /// it includes original agent ages so identical object updates keep running.
    pub fn checkpoint(&self) -> Result<Vec<u8>, Error> {
        self.replay_inner(MAX_SNAPSHOT_BYTES, true)
    }

    fn replay_inner(&self, limit: usize, checkpoint: bool) -> Result<Vec<u8>, Error> {
        if let Some(failure) = &self.failure { return Err(failure.clone()); }
        let mut out = Limited::new(limit.min(MAX_SNAPSHOT_BYTES));
        let now = Instant::now();
        if let Some(hello) = &self.hello { emit_json(&mut out, b'q', hello)?; }
        for blob in &self.blobs { emit_chunks(&mut out, b'b', &blob.params, &blob.body, false)?; }
        for surface in &self.surfaces {
            emit_json(&mut out, b'o', &surface.open)?;
            for (name, css) in &surface.styles { emit_json(&mut out, b's', &json!({"sf":surface.id(),"name":name,"css":css}))?; }
            emit_json(&mut out, b'f', &FrameReplay(surface, now, checkpoint))?;
            if surface.closed { emit_json(&mut out, b'x', &json!({"id":surface.id(),"keep":true}))?; }
        }
        Ok(out.bytes)
    }
}


fn emit_json(out: &mut Limited, verb: u8, value: &impl Serialize) -> Result<(), Error> {
    let mut body = Limited::new(out.limit.saturating_sub(out.bytes.len()));
    serde_json::to_writer(&mut body, value).map_err(|err| error(format!("TSP replay serialization: {err}")))?;
    emit_chunks(out, verb, &[], &body.bytes, false)
}

/// Serialize time-dependent props without cloning the document or changing the
/// stored wire ages (agent objects compare against those ages on later patches).
struct PropsReplay<'a> { node: &'a Node, now: Instant }
impl Serialize for PropsReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let props = self.node.fields.get("p").and_then(Value::as_object);
        let kind = self.node.fields.get("k").and_then(Value::as_str);
        let status = props.and_then(|p| p.get("status")).and_then(Value::as_str);
        let live = match kind {
            Some("tool") => !matches!(status, Some("done" | "error" | "cancelled" | "ask")),
            Some("agent") => !matches!(status, Some("done" | "failed" | "aborted" | "idle" | "parked")),
            _ => true,
        };
        let direct = if kind == Some("elapsed") || (kind == Some("tool") && live) { self.node.clocks[0] } else { None };
        let add_age = direct.is_some() && props.is_none_or(|p| !p.contains_key("age"));
        let mut map = serializer.serialize_map(Some(props.map_or(0, Map::len) + usize::from(add_age)))?;
        if let Some(props) = props {
            for (key, value) in props {
                if key == "age" {
                    if let Some(clock) = direct { map.serialize_entry(key, &clock.at(self.now))?; continue }
                }
                if kind == Some("agent") {
                    let index = match key.as_str() { "stats" if live => Some(1), "tool" => Some(2), "retry" => Some(3), _ => None };
                    if let Some(clock) = index.and_then(|index| self.node.clocks[index]) {
                        if let Some(fields) = value.as_object() {
                            map.serialize_entry(key, &AgePropsReplay { fields, clock, now: self.now })?;
                            continue
                        }
                    }
                }
                map.serialize_entry(key, value)?;
            }
        }
        if add_age { map.serialize_entry("age", &direct.unwrap().at(self.now))?; }
        map.end()
    }
}

struct AgePropsReplay<'a> { fields: &'a Map<String, Value>, clock: AgeClock, now: Instant }
impl Serialize for AgePropsReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let add_age = !self.fields.contains_key("age");
        let mut map = serializer.serialize_map(Some(self.fields.len() + usize::from(add_age)))?;
        for (key, value) in self.fields {
            if key == "age" { map.serialize_entry(key, &self.clock.at(self.now))?; }
            else { map.serialize_entry(key, value)?; }
        }
        if add_age { map.serialize_entry("age", &self.clock.at(self.now))?; }
        map.end()
    }
}


struct SourceAgesReplay<'a>(&'a Node);
impl Serialize for SourceAgesReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let original = self.0.fields.get("_falconReplayAges");
        let mut map = serializer.serialize_map(Some(2 + usize::from(original.is_some())))?;
        map.serialize_entry("v", &1)?;
        let ages = [self.0.clocks[1], self.0.clocks[2], self.0.clocks[3]].map(|clock| clock.map(|clock| clock.source_age));
        map.serialize_entry("ages", &ages)?;
        if let Some(original) = original { map.serialize_entry("original", original)?; }
        map.end()
    }
}
struct TreeReplay<'a> { surface: &'a Surface, id: &'a str, now: Instant, checkpoint: bool }
impl Serialize for TreeReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let node = &self.surface.nodes[self.id];
        let add_props = node.clocks[0].is_some() && !node.fields.contains_key("p");
        let source_ages = self.checkpoint && node.fields.get("k").and_then(Value::as_str) == Some("agent") && node.clocks[1..].iter().any(Option::is_some);
        let add_source_ages = source_ages && !node.fields.contains_key("_falconReplayAges");
        let mut map = serializer.serialize_map(Some(node.fields.len() + usize::from(!node.children.is_empty()) + usize::from(add_props) + usize::from(add_source_ages)))?;
        for (key, value) in &node.fields {
            if key == "_falconReplayAges" && source_ages { continue }
            if key == "p" { map.serialize_entry(key, &PropsReplay { node, now: self.now })?; }
            else { map.serialize_entry(key, value)?; }
        }
        if add_props { map.serialize_entry("p", &PropsReplay { node, now: self.now })?; }
        if source_ages { map.serialize_entry("_falconReplayAges", &SourceAgesReplay(node))?; }
        if !node.children.is_empty() { map.serialize_entry("c", &ChildrenReplay { surface: self.surface, children: &node.children, now: self.now, checkpoint: self.checkpoint })?; }
        map.end()
    }
}
struct ChildrenReplay<'a> { surface: &'a Surface, children: &'a [String], now: Instant, checkpoint: bool }
impl Serialize for ChildrenReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.children.len()))?;
        for id in self.children { seq.serialize_element(&TreeReplay { surface: self.surface, id, now: self.now, checkpoint: self.checkpoint })?; }
        seq.end()
    }
}
struct FrameReplay<'a>(&'a Surface, Instant, bool);
impl Serialize for FrameReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3 + usize::from(self.2)))?;
        map.serialize_entry("sf", self.0.id())?;
        map.serialize_entry("s", &self.0.sequence)?;
        if self.2 { map.serialize_entry("_falconCheckpoint", &true)?; }
        map.serialize_entry("ops", &OpsReplay(self.0, self.1, self.2))?;
        map.end()
    }
}
struct OpsReplay<'a>(&'a Surface, Instant, bool);
impl Serialize for OpsReplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let surface = self.0;
        let mut seq = serializer.serialize_seq(None)?;
        let root = &surface.nodes[surface.id()];
        if let Some(props) = root.fields.get("p") { seq.serialize_element(&("set", surface.id(), props))?; }
        for id in &root.children {
            seq.serialize_element(&("add", id, surface.id(), Value::Null, TreeReplay { surface, id, now: self.1, checkpoint: self.2 }))?;
        }
        // Traversal order, rather than hash iteration order, keeps replay stable.
        fn settled<'a>(surface: &'a Surface, id: &'a str, ids: &mut Vec<&'a str>) {
            let node = &surface.nodes[id];
            for child in &node.children { settled(surface, child, ids); }
            if node.settled { ids.push(id); }
        }
        let mut ids = Vec::new();
        settled(surface, surface.id(), &mut ids);
        for id in ids { seq.serialize_element(&("settle", id))?; }
        if let Some(focus) = &surface.focus { seq.serialize_element(&("focus", focus))?; }
        if let Some(reveal) = &surface.reveal { seq.serialize_element(reveal)?; }
        if let Some(scroll) = &surface.scroll { seq.serialize_element(scroll)?; }
        if surface.suspended { seq.serialize_element(&["suspend"])?; }
        seq.end()
    }
}

/// Whether a snapshot restores an open surface. Closed `keep:true`
/// transcripts do not prevent the attach path from requesting a fresh handshake.
pub fn snapshot_has_live_surface(bytes: &[u8]) -> bool {
    snapshot_parts_have_live_surface(&[bytes])
}

/// Scans snapshot plus continuation without allocating a concatenation. Frames
/// are never JSON-decoded here, so transcript size does not build a document.
/// `listen:false` still counts: any already-open GUI suppresses a new probe.
pub fn snapshot_parts_have_live_surface(parts: &[&[u8]]) -> bool {
    let mut live: Vec<(String, String)> = Vec::new();
    let mut valid = true;
    let mut parser = Parser::new();
    for part in parts {
        parser.push(part, |_, token| match token {
            Token::Reset => { live.clear(); valid = true; }
            Token::Error(_) => valid = false,
            Token::Tsp(message) if matches!(message.verb, b'o' | b'x') => {
                let Ok(body) = message.json() else { return };
                let Some(id) = body.get("id").and_then(Value::as_str) else { return };
                if message.verb == b'x' {
                    if let Some(position) = live.iter().position(|(current, _)| current == id) { live.remove(position); }
                } else {
                    let adopting = body.get("adopt") == Some(&Value::Bool(true));
                    if adopting && live.iter().any(|(current, _)| current == id) { return }
                    let mode = if adopting { "inline" } else { body.get("mode").and_then(Value::as_str).unwrap_or("inline") };
                    if !matches!(mode, "inline" | "screen" | "flow") { return }
                    live.retain(|(_, current_mode)| current_mode != mode && !(mode == "inline" && current_mode == "screen"));
                    live.push((id.to_owned(), mode.to_owned()));
                }
            }
            _ => {}
        });
    }
    valid && !live.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(verb: u8, body: Value) -> Message {
        Message { verb, params: Vec::new(), body: serde_json::to_vec(&body).unwrap() }
    }

    fn apply(state: &mut State, verb: u8, body: Value) {
        state.apply(&message(verb, body)).unwrap();
    }

    fn wire(verb: u8, params: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!("\x1b_tsp;{};{params}", verb as char).into_bytes();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\x1b\\");
        bytes
    }

    fn feed(parser: &mut Parser, state: &mut State, bytes: &[u8]) {
        parser.push(bytes, |_, token| match token {
            Token::Tsp(message) => state.apply(&message).unwrap(),
            Token::Reset => state.reset(),
            Token::Error(failure) => panic!("{failure}"),
            Token::Da1 => {}
        });
    }

    fn roundtrip(state: &State) -> State {
        let mut restored = State::new();
        feed(&mut Parser::new(), &mut restored, &state.replay().unwrap());
        restored
    }

    fn initial() -> State {
        let mut state = State::new();
        apply(&mut state, b'q', json!({"q":"hello","v":[1],"app":"omp","features":["edit","undo","send"],"future":{"a":1}}));
        apply(&mut state, b'o', json!({"id":"s","mode":"inline","role":"omp.session","future":7}));
        apply(&mut state, b'f', json!({"sf":"s","s":17,"ops":[
            ["add","main","s",null,{"id":"main","k":"col","p":{"role":"transcript"},"c":[
                {"id":"a","k":"md","future":{"raw":true},"p":{"text":"你好😀世界","unknown":[1,{"a":2}],"drop":null}},
                {"id":"b","k":"card","c":[{"id":"c","k":"text","p":{"text":"child"}}]},
                {"id":"d","k":"future-kind","p":{"arbitrary":{"x":[1,2]}}}
            ]}],
            ["add","dock","s",null,{"id":"dock","k":"col","c":[{"id":"ed","k":"editor","p":{"text":"compose"}}]}],
            ["add","layer","s",null,{"id":"layer","k":"col"}],
            ["settle","b"],["focus","ed"],["reveal","a","end"],["scroll","main","end"],["suspend"]
        ]}));
        state
    }

    fn tree_at(state: &State, id: &str, now: Instant, checkpoint: bool) -> Value {
        serde_json::to_value(TreeReplay { surface: &state.surfaces[0], id, now, checkpoint }).unwrap()
    }

    #[test]
    fn elapsed_defaults_reanchors_and_preserves_stopped_and_unknown_props() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[
            ["add","timer","main",null,{"id":"timer","k":"elapsed"}],
            ["add","fixed","main",null,{"id":"fixed","k":"elapsed","p":{"age":1200,"stopped":3400,"future":{"x":1}}}]
        ]}));
        let anchor = state.surfaces[0].nodes["timer"].clocks[0].unwrap().received;
        assert_eq!(tree_at(&state, "timer", anchor + Duration::from_secs(2), false)["p"]["age"].as_f64(), Some(2000.0));
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","timer",{"format":"clock"}],["text","a","append"," streamed"]]}));
        assert_eq!(state.surfaces[0].nodes["timer"].clocks[0].unwrap().received, anchor);
        let fixed = tree_at(&state, "fixed", Instant::now(), false);
        assert_eq!(fixed["p"]["stopped"], 3400);
        assert_eq!(fixed["p"]["future"], json!({"x":1}));

        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","timer",{"age":700}]]}));
        let node = state.surfaces[0].nodes.get_mut("timer").unwrap();
        node.clocks[0].as_mut().unwrap().received -= Duration::from_secs(10);
        let old = node.clocks[0].unwrap().received;
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","timer",{"age":700}]]}));
        let clock = state.surfaces[0].nodes["timer"].clocks[0].unwrap();
        assert!(clock.received > old);
        assert_eq!(tree_at(&state, "timer", clock.received + Duration::from_secs(2), false)["p"]["age"].as_f64(), Some(2700.0));
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","timer",{"age":null}]]}));
        let clock = state.surfaces[0].nodes["timer"].clocks[0].unwrap();
        assert_eq!(tree_at(&state, "timer", clock.received + Duration::from_secs(1), false)["p"]["age"].as_f64(), Some(1000.0));
    }

    #[test]
    fn expired_countdown_stays_expired_across_replays_and_accepts_new_age() {
        let boundary = -1.0;
        let encoded = serde_json::to_vec(&boundary).unwrap();
        assert_eq!(serde_json::from_slice::<f64>(&encoded).unwrap(), boundary);
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[["add","timer","main",null,{"id":"timer","k":"elapsed","p":{"age":-1000}}]]}));
        state.surfaces[0].nodes.get_mut("timer").unwrap().clocks[0].as_mut().unwrap().received -= Duration::from_secs(2);
        for _ in 0..3 {
            let original = state.surfaces[0].nodes["timer"].fields.clone();
            let replay = state.replay().unwrap();
            assert_eq!(state.surfaces[0].nodes["timer"].fields, original);
            let mut restored = State::new();
            feed(&mut Parser::new(), &mut restored, &replay);
            let props = &restored.surfaces[0].nodes["timer"].fields["p"];
            assert_eq!(props["age"].as_f64(), Some(boundary));
            assert!(props.get("stopped").is_none());
            state = restored;
        }
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","timer",{"age":-9000}]]}));
        let clock = state.surfaces[0].nodes["timer"].clocks[0].unwrap();
        assert_eq!(tree_at(&state, "timer", clock.received + Duration::from_secs(1), false)["p"]["age"].as_f64(), Some(-8000.0));
    }

    #[test]
    fn running_tool_and_agent_ages_advance_but_final_durations_are_frozen() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[
            ["add","live","main",null,{"id":"live","k":"tool","p":{"status":"running","age":1000}}],
            ["add","done","main",null,{"id":"done","k":"tool","p":{"status":"done","age":1000,"took":8000}}],
            ["add","worker","main",null,{"id":"worker","k":"agent","p":{"status":"running","stats":{"age":2000,"tools":3,"future":true},"tool":{"age":3000,"name":"read"},"retry":{"age":4000,"delay":10000}}}],
            ["add","finished","main",null,{"id":"finished","k":"agent","p":{"status":"done","stats":{"age":2000,"took":9000}}}]
        ]}));
        for id in ["live", "done", "worker", "finished"] {
            let node = state.surfaces[0].nodes.get_mut(id).unwrap();
            for clock in node.clocks.iter_mut().flatten() { clock.received -= Duration::from_secs(5); }
        }
        let node = &state.surfaces[0].nodes["live"];
        let now = node.clocks[0].unwrap().received + Duration::from_secs(5);
        assert_eq!(tree_at(&state, "live", now, false)["p"]["age"].as_f64(), Some(6000.0));
        let done = tree_at(&state, "done", now, false);
        assert_eq!(done["p"]["age"], 1000);
        assert_eq!(done["p"]["took"], 8000);
        let node = &state.surfaces[0].nodes["worker"];
        let now = node.clocks[1].unwrap().received + Duration::from_secs(5);
        let worker = tree_at(&state, "worker", now, false);
        for (key, expected) in [("stats", 7000.0), ("tool", 8000.0), ("retry", 9000.0)] {
            assert_eq!(worker["p"][key]["age"].as_f64(), Some(expected));
        }
        assert_eq!(worker["p"]["retry"]["delay"], 10000);
        assert_eq!(worker["p"]["stats"]["future"], true);
        assert_eq!(tree_at(&state, "finished", now, false)["p"]["stats"], json!({"age":2000,"took":9000}));
    }

    #[test]
    fn retry_without_an_age_keeps_counting_through_object_updates() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[["add","worker","main",null,{"id":"worker","k":"agent","p":{"retry":{"delay":5000}}}]]}));
        let clock = state.surfaces[0].nodes["worker"].clocks[3].unwrap();
        let props = tree_at(&state, "worker", clock.received + Duration::from_secs(2), false);
        assert_eq!(props["p"]["retry"]["age"].as_f64(), Some(2000.0));
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","worker",{"retry":{"delay":9000,"attempt":2}}]]}));
        assert_eq!(state.surfaces[0].nodes["worker"].clocks[3].unwrap().received, clock.received);
        let mut restored = State::new();
        feed(&mut Parser::new(), &mut restored, &state.checkpoint().unwrap());
        let before = restored.surfaces[0].nodes["worker"].clocks[3].unwrap();
        apply(&mut restored, b'f', json!({"sf":"s","ops":[["set","worker",{"retry":{"delay":12000}}]]}));
        assert_eq!(restored.surfaces[0].nodes["worker"].clocks[3].unwrap().received, before.received);
    }

    #[test]
    fn agent_same_age_object_patches_survive_repeated_internal_checkpoints() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[["add","worker","main",null,{"id":"worker","k":"agent","future":[1,2],"p":{"status":"running","stats":{"age":100,"tools":1},"tool":{"age":200,"name":"read"},"retry":{"age":300,"delay":9000}}}]]}));
        for _ in 0..3 {
            for clock in state.surfaces[0].nodes.get_mut("worker").unwrap().clocks.iter_mut().flatten() {
                clock.received -= Duration::from_secs(1);
            }
            let tree = tree_at(&state, "worker", Instant::now(), false);
            assert!(tree.get("_falconReplayAges").is_none());
            let internal = tree_at(&state, "worker", Instant::now(), true);
            assert_eq!(internal["_falconReplayAges"]["ages"], json!([100.0,200.0,300.0]));
            let mut restored = State::new();
            feed(&mut Parser::new(), &mut restored, &state.checkpoint().unwrap());
            let before = restored.surfaces[0].nodes["worker"].clocks;
            apply(&mut restored, b'f', json!({"sf":"s","ops":[["set","worker",{"stats":{"age":100,"tools":2,"unknown":7},"tool":{"age":200,"name":"write"},"retry":{"age":300,"delay":12000}}]]}));
            let node = &restored.surfaces[0].nodes["worker"];
            for index in 1..4 { assert_eq!(node.clocks[index].unwrap().received, before[index].unwrap().received); }
            assert_eq!(node.fields["future"], json!([1,2]));
            assert_eq!(node.fields["p"]["stats"]["unknown"], 7);
            assert!(node.clocks[1].unwrap().age > 1000.0);
            state = restored;
        }
        let old_tool = state.surfaces[0].nodes["worker"].clocks[2].unwrap().received;
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","worker",{"stats":{"age":1},"retry":null}]]}));
        assert_eq!(state.surfaces[0].nodes["worker"].clocks[1].unwrap().age, 1.0);
        assert_eq!(state.surfaces[0].nodes["worker"].clocks[2].unwrap().received, old_tool);
        assert!(state.surfaces[0].nodes["worker"].clocks[3].is_none());
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","worker",{"stats":{"tools":4}}]]}));
        assert!(state.surfaces[0].nodes["worker"].clocks[1].is_none());
    }

    #[test]
    fn internal_age_metadata_preserves_colliding_unknown_node_fields() {
        let mut state = initial();
        let original = json!({"v":1,"ages":[77,88,99],"application":"value"});
        apply(&mut state, b'f', json!({"sf":"s","ops":[["add","worker","main",null,{"id":"worker","k":"agent","_falconReplayAges":original,"p":{"stats":{"age":100}}}]]}));
        assert_eq!(state.surfaces[0].nodes["worker"].clocks[1].unwrap().source_age, 100.0);
        let mut restored = State::new();
        feed(&mut Parser::new(), &mut restored, &state.checkpoint().unwrap());
        let tree = tree_at(&restored, "worker", Instant::now(), false);
        assert_eq!(tree["_falconReplayAges"], original);
        let mut public = State::new();
        feed(&mut Parser::new(), &mut public, &state.replay().unwrap());
        assert_eq!(public.surfaces[0].nodes["worker"].fields["_falconReplayAges"], original);
    }

    #[test]
    fn picker_deltas_fold_by_id_and_never_replay_twice() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","ops":[
            ["add","picker","layer",null,{"id":"picker","k":"picker","future":true,"p":{"items":[{"id":"a","label":"A"},{"id":"b","label":"B"}],"itemsAdd":[{"id":"c","label":"C"}],"itemsDel":["b"],"unknown":{"raw":true}}}],
            ["set","picker",{"itemsAdd":[{"id":"a","label":"updated","future":[1,2]},{"id":"d","label":"D"}]}],
            ["set","picker",{"itemsAdd":[{"id":"e","label":"E"}],"itemsDel":["missing","c"]}],
            ["set","picker",{"selected":"e"}]
        ]}));
        let expected = json!([{"id":"a","label":"updated","future":[1,2]},{"id":"d","label":"D"},{"id":"e","label":"E"}]);
        for _ in 0..3 {
            let node = &state.surfaces[0].nodes["picker"];
            assert_eq!(node.fields["p"]["items"], expected);
            assert!(node.fields["p"].get("itemsAdd").is_none());
            assert!(node.fields["p"].get("itemsDel").is_none());
            assert_eq!(node.fields["p"]["unknown"], json!({"raw":true}));
            assert_eq!(node.fields["future"], true);
            assert_eq!(node.field_bytes, object_cost(&node.fields) + 256);
            let surface = &state.surfaces[0];
            assert_eq!(surface.stored, surface.nodes.values().map(node_cost).sum::<usize>());
            state = roundtrip(&state);
        }
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","picker",{
            "items":[{"id":"z","label":"old"},{"label":"missing id"},{"id":"z","label":"new"}],
            "itemsAdd":[{"id":"z","label":"final"},{"id":"y"}],"itemsDel":["y"]
        }]]}));
        assert_eq!(state.surfaces[0].nodes["picker"].fields["p"]["items"], json!([{"id":"z","label":"final"}]));
        apply(&mut state, b'f', json!({"sf":"s","ops":[["set","picker",{"items":null,"itemsAdd":[{"id":"new"}]}],["set","d",{"itemsAdd":[{"id":"opaque"}]}]]}));
        assert_eq!(state.surfaces[0].nodes["picker"].fields["p"]["items"], json!([{"id":"new"}]));
        assert_eq!(state.surfaces[0].nodes["d"].fields["p"]["itemsAdd"], json!([{"id":"opaque"}]));
        apply(&mut state, b'f', json!({"sf":"s","ops":[["add","empty","layer",null,{"id":"empty","k":"picker"}],["set","empty",{"itemsAdd":[{"id":"first"}]}]]}));
        let node = &state.surfaces[0].nodes["empty"];
        assert_eq!(node.fields["p"]["items"], json!([{"id":"first"}]));
        assert_eq!(node.field_bytes, object_cost(&node.fields) + 256);
    }

    #[test]
    fn replays_complete_tree_unknown_fields_styles_blobs_and_sequence() {
        let mut state = initial();
        apply(&mut state, b's', json!({"sf":"s","name":"one","css":".a{color:red}"}));
        apply(&mut state, b's', json!({"sf":"s","name":"two","css":".b{color:blue}"}));
        apply(&mut state, b's', json!({"sf":"s","name":"one","css":".a{color:green}"}));
        state.apply(&Message { verb: b'b', params: vec![("mime".into(), "image/png".into())], body: b"aGVsbG8=".to_vec() }).unwrap();
        let restored = roundtrip(&state);
        let surface = &restored.surfaces[0];
        assert_eq!(surface.sequence, json!(17));
        assert_eq!(surface.nodes["main"].children, ["a", "b", "d"]);
        assert_eq!(surface.nodes["a"].fields["future"], json!({"raw":true}));
        assert_eq!(surface.nodes["a"].fields["p"]["unknown"], json!([1,{"a":2}]));
        assert!(surface.nodes["a"].fields["p"].get("drop").is_none());
        assert!(surface.nodes["b"].settled);
        assert_eq!(surface.focus.as_deref(), Some("ed"));
        assert!(surface.suspended);
        assert_eq!(surface.styles.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(restored.blobs.len(), 1);
    }

    #[test]
    fn set_append_replace_move_delete_and_wake_settled_ancestors() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","s":99,"ops":[
            ["set","a",{"unknown":null,"bold":true}],
            ["text","a","replace","start"],["text","a","append"," finish"],
            ["move","d","main","a"],["move","c","main","b"],
            ["del","b"],["resume"],["focus",null]
        ]}));
        let restored = roundtrip(&state);
        let surface = &restored.surfaces[0];
        assert_eq!(surface.nodes["main"].children, ["d", "a", "c"]);
        assert_eq!(surface.nodes["a"].fields["p"]["text"], "start finish");
        assert!(surface.nodes["a"].fields["p"].get("unknown").is_none());
        assert!(!surface.nodes.contains_key("b"));
        assert_eq!(surface.nodes["c"].parent.as_deref(), Some("main"));
        assert!(!surface.suspended);
        assert_eq!(surface.sequence, json!(99));
    }

    #[test]
    fn splice_uses_utf16_and_rejects_surrogate_splits_without_stopping_frame() {
        let mut state = initial();
        apply(&mut state, b'f', json!({"sf":"s","s":18,"ops":[["splice","a",2,2,"🦅中"]]}));
        assert_eq!(state.surfaces[0].nodes["a"].fields["p"]["text"], "你好🦅中世界");
        let invalid = message(b'f', json!({"sf":"s","s":19,"ops":[
            ["splice","a",3,1,"bad"],["splice","a",2,1,"bad"],
            ["splice","a",999,0,"bad"],["text","c","append"," valid"]
        ]}));
        assert!(state.apply(&invalid).is_err());
        assert_eq!(state.surfaces[0].nodes["a"].fields["p"]["text"], "你好🦅中世界");
        assert_eq!(state.surfaces[0].nodes["c"].fields["p"]["text"], "child valid");
        assert!(!state.surfaces[0].nodes["b"].settled);
        roundtrip(&state);
    }

    #[test]
    fn invalid_tree_move_and_delete_are_individual_rejections() {
        let mut state = initial();
        let frame = message(b'f', json!({"sf":"s","s":20,"ops":[
            ["add","new","main",null,{"id":"new","k":"col","c":[{"id":"a","k":"text"}]}],
            ["move","b","c",null],["move","a","main","a"],["del","s"],
            ["set","c",{"ok":true}],["del","dock"]
        ]}));
        assert!(state.apply(&frame).is_err());
        let surface = &state.surfaces[0];
        assert!(!surface.nodes.contains_key("new"));
        assert_eq!(surface.nodes["b"].parent.as_deref(), Some("main"));
        assert_eq!(surface.nodes["c"].fields["p"]["ok"], true);
        assert!(!surface.nodes.contains_key("ed"));
        assert!(surface.focus.is_none());
        roundtrip(&state);
    }

    #[test]
    fn close_keep_adopt_and_reopen_preserve_only_main() {
        let mut state = initial();
        apply(&mut state, b's', json!({"sf":"s","name":"main","css":".a{opacity:1}"}));
        apply(&mut state, b'x', json!({"id":"s","keep":true}));
        let closed = roundtrip(&state);
        assert!(closed.surfaces[0].closed);
        assert_eq!(closed.surfaces[0].nodes["s"].children, ["main"]);
        assert!(!closed.surfaces[0].nodes.contains_key("ed"));
        apply(&mut state, b'o', json!({"id":"s","adopt":true,"mode":"screen","title":"returned","font":"sans"}));
        apply(&mut state, b'f', json!({"sf":"s","s":18,"ops":[["text","a","append"," again"]]}));
        let restored = roundtrip(&state);
        assert_eq!(restored.surfaces.len(), 1);
        assert!(!restored.surfaces[0].closed);
        assert_eq!(restored.surfaces[0].mode(), "inline");
        assert_eq!(restored.surfaces[0].nodes["a"].fields["p"]["text"], "你好😀世界 again");
        assert_eq!(restored.surfaces[0].styles.len(), 1);
        apply(&mut state, b'x', json!({"id":"s","keep":false}));
        apply(&mut state, b'o', json!({"id":"s"}));
        assert_eq!(state.surfaces[0].nodes.len(), 1);
    }

    #[test]
    fn replacement_closes_inline_and_discards_covering_screen() {
        let mut state = initial();
        apply(&mut state, b'o', json!({"id":"cover","mode":"screen"}));
        assert_eq!(state.surfaces.len(), 2);
        apply(&mut state, b'o', json!({"id":"second","mode":"inline"}));
        assert_eq!(state.surfaces.len(), 2);
        assert!(state.surfaces[0].closed);
        assert_eq!(state.surfaces[1].id(), "second");
        roundtrip(&state);
    }

    #[test]
    fn scanner_matches_all_byte_split_points_and_exact_token_offsets() {
        let hello = wire(b'q', "", br#"{"q":"hello","app":"omp","text":";x=y"}"#);
        let frame = wire(b'f', "", "{\"sf\":\"s\",\"s\":1,\"ops\":[],\"text\":\"汉😀\u{009c}\"}".as_bytes());
        let mut bytes = b"ordinary\x1b]title\x07\x1b_ignore\x1b\\\x1b[31m".to_vec();
        bytes.extend_from_slice(&hello);
        let hello_end = bytes.len();
        bytes.extend_from_slice(b"\x1b[c\x1b[0c\x1b[?1c");
        bytes.extend_from_slice(&frame);
        bytes.extend_from_slice(b"\x1bc");
        let mut expected = Vec::new();
        Parser::new().push(&bytes, |end, token| expected.push((end, token)));
        assert_eq!(expected.len(), 5);
        assert_eq!(expected[0].0, hello_end);
        assert_eq!(expected[1], (hello_end + 3, Token::Da1));
        assert_eq!(expected[2], (hello_end + 7, Token::Da1));
        assert!(matches!(expected[4].1, Token::Reset));
        for cut in 0..=bytes.len() {
            let mut parser = Parser::new();
            let mut actual = Vec::new();
            parser.push(&bytes[..cut], |end, token| actual.push((end, token)));
            parser.push(&bytes[cut..], |end, token| actual.push((cut + end, token)));
            assert_eq!(actual, expected, "cut={cut}");
        }
        let mut actual = Vec::new();
        let mut parser = Parser::new();
        for (index, byte) in bytes.iter().enumerate() {
            parser.push(&[*byte], |end, token| actual.push((index + end, token)));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn chunks_join_inside_utf8_and_keep_first_parameters() {
        let body = "{\"sf\":\"s\",\"text\":\"中文😀\"}".as_bytes();
        let cut = body.iter().position(|&byte| byte >= 128).unwrap() + 1;
        let mut bytes = wire(b'f', "c=one;m=1;mime=first;", &body[..cut]);
        bytes.extend(wire(b'f', "c=one;mime=ignored;", &body[cut..]));
        for cut in 0..=bytes.len() {
            let mut parser = Parser::new();
            let mut tokens = Vec::new();
            parser.push(&bytes[..cut], |_, token| tokens.push(token));
            parser.push(&bytes[cut..], |_, token| tokens.push(token));
            assert_eq!(tokens.len(), 1);
            let Token::Tsp(message) = &tokens[0] else { panic!("missing joined message") };
            assert_eq!(message.body, body);
            assert_eq!(message.param("mime"), Some("first"));
        }
    }

    #[test]
    fn snapshot_continuation_keeps_every_partial_chunk_and_apc_byte() {
        let hello = wire(b'q', "", br#"{"q":"hello","app":"omp","v":[1],"features":["edit"]}"#);
        let open = wire(b'o', "", br#"{"id":"s"}"#);
        let body = "{\"sf\":\"s\",\"s\":7,\"ops\":[[\"add\",\"main\",\"s\",null,{\"id\":\"main\",\"k\":\"md\",\"p\":{\"text\":\"汉😀字\"}}]]}".as_bytes();
        let utf8_cut = body.iter().position(|&byte| byte >= 128).unwrap() + 2;
        let mut bytes = hello;
        bytes.extend(open);
        bytes.extend(wire(b'f', "c=stream;m=1;", &body[..utf8_cut]));
        bytes.extend(wire(b'f', "c=stream;", &body[utf8_cut..]));
        let mut expected = State::new();
        feed(&mut Parser::new(), &mut expected, &bytes);
        for cut in 0..=bytes.len() {
            let mut parser = Parser::new();
            let mut state = State::new();
            feed(&mut parser, &mut state, &bytes[..cut]);
            let mut snapshot = state.replay().unwrap();
            snapshot.extend(parser.replay_pending().unwrap());
            let mut restored = State::new();
            let mut restore_parser = Parser::new();
            feed(&mut restore_parser, &mut restored, &snapshot);
            feed(&mut restore_parser, &mut restored, &bytes[cut..]);
            assert_eq!(restored.replay().unwrap(), expected.replay().unwrap(), "cut={cut}");
        }
    }

    #[test]
    fn chunk_interruption_reset_and_query_gate() {
        let mut parser = Parser::new();
        let mut tokens = Vec::new();
        parser.push(&wire(b'f', "c=one;m=1;", b"{"), |_, token| tokens.push(token));
        parser.push(&wire(b'q', "", br#"{"q":"hello"}"#), |_, token| tokens.push(token));
        assert!(matches!(tokens[0], Token::Error(_)));
        assert!(parser.failure().is_none());
        let Token::Tsp(hello) = &tokens[1] else { panic!("missing hello") };
        assert!(hello.is_hello());
        assert!(hello.enables_replies());
        assert!(!message(b'o', json!({"id":"s","listen":false})).enables_replies());
        assert!(message(b'o', json!({"id":"s"})).enables_replies());
        parser.push(&wire(b'f', "c=two;m=1;", b"{"), |_, _| {});
        parser.push(b"\x1bc", |_, token| assert_eq!(token, Token::Reset));
        assert!(parser.replay_pending().unwrap().is_empty());
    }

    #[test]
    fn timed_out_chunks_are_recoverable_without_copying_pending_bytes() {
        let mut parser = Parser::new();
        parser.push(&wire(b'f', "c=old;m=1;", b"{"), |_, _| {});
        parser.chunk.as_mut().unwrap().updated -= Duration::from_secs(2);
        let mut tokens = Vec::new();
        parser.push(&wire(b'q', "", br#"{"q":"hello"}"#), |_, token| tokens.push(token));
        assert!(matches!(tokens[0], Token::Error(_)));
        assert!(matches!(&tokens[1], Token::Tsp(message) if message.is_hello()));
        assert!(parser.failure().is_none());
        assert!(parser.chunk.is_none());
    }

    #[test]
    fn replay_chunking_handles_parameter_shaped_continuations() {
        let mut state = initial();
        let text = format!("{}x=1;y汉😀{}", "a".repeat(CHUNK_BYTES - 100), "b".repeat(CHUNK_BYTES + 20));
        apply(&mut state, b'f', json!({"sf":"s","s":22,"ops":[["text","a","replace",text]]}));
        roundtrip(&state);
        let mut out = Limited::new(MAX_SNAPSHOT_BYTES);
        let body = [vec![b'!'; CHUNK_BYTES], b"_key=value;tail".to_vec()].concat();
        emit_chunks(&mut out, b'b', &[], &body, false).unwrap();
        let mut tokens = Vec::new();
        Parser::new().push(&out.bytes, |_, token| tokens.push(token));
        assert_eq!(tokens.len(), 1);
        let Token::Tsp(joined) = &tokens[0] else { panic!("missing joined payload") };
        assert_eq!(joined.body, body);
    }

    #[test]
    fn limits_are_visible_and_reset_recovers_parser() {
        assert!(initial().replay_with_limit(32).is_err());
        let mut parser = Parser::new();
        let oversized = [b"\x1b_tsp;f;".as_slice(), &vec![b'a'; MAX_APC_BYTES]].concat();
        let mut saw_error = false;
        parser.push(&oversized, |_, token| saw_error |= matches!(token, Token::Error(_)));
        assert!(saw_error);
        assert!(parser.replay_pending().is_err());
        assert!(parser.failure().is_some());
        parser.push(b"\x1b\\\x1bc", |_, _| {});
        assert!(parser.replay_pending().unwrap().is_empty());
        assert!(parser.failure().is_none());
        let mut state = initial();
        assert!(state.apply(&Message { verb: b'b', params: Vec::new(), body: b"a===aGVsbG8=".to_vec() }).is_err());
        assert!(state.apply(&Message { verb: b'b', params: Vec::new(), body: b"aA==aA==".to_vec() }).is_err());
        roundtrip(&state);
    }

    #[test]
    fn detects_open_surfaces_across_snapshot_parts() {
        let mut state = initial();
        assert!(snapshot_has_live_surface(&state.replay().unwrap()));
        apply(&mut state, b'x', json!({"id":"s","keep":true}));
        assert!(!snapshot_has_live_surface(&state.replay().unwrap()));
        apply(&mut state, b'o', json!({"id":"static","mode":"flow","listen":false}));
        assert!(snapshot_has_live_surface(&state.replay().unwrap()));
        apply(&mut state, b'o', json!({"id":"s","adopt":true}));
        assert!(snapshot_has_live_surface(&state.replay().unwrap()));
        let replay = state.replay().unwrap();
        for cut in 0..=replay.len() {
            assert!(snapshot_parts_have_live_surface(&[&replay[..cut], &replay[cut..]]));
        }
        let mut replay = state.replay().unwrap();
        replay.extend_from_slice(b"\x1bc");
        assert!(!snapshot_has_live_surface(&replay));
    }

    #[test]
    fn close_does_not_confuse_surface_id_with_live_region_id() {
        for id in ["dock", "layer"] {
            let mut state = State::new();
            apply(&mut state, b'o', json!({"id":id}));
            apply(&mut state, b'f', json!({"sf":id,"s":1,"ops":[["add","main",id,null,{"id":"main","k":"text","p":{"text":"retained"}}]]}));
            apply(&mut state, b'x', json!({"id":id,"keep":true}));
            let restored = roundtrip(&state);
            assert_eq!(restored.surfaces[0].nodes[id].children, ["main"]);
        }
    }
}
