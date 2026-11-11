//! 官方 Claude 交互终端的只读 transcript 镜像。
//!
//! 这里没有 PTY、CLI 控制请求或进程操作。解析器可留在 Rc/RefCell 会话引擎里；
//! [`LocalTranscriptSource::read_chunk`] 的目录扫描与文件读取必须在 spawn_blocking 中运行，
//! 把 source/cursor 克隆进去，返回后再短暂借用解析器。只有 ingest 成功后才能提交新游标。
//! 输入是原始字节，半行与分片 UTF-8 一起保留到换行，不能在 reader 中做 lossy 解码。

use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use falcon_proto::{
    AgentEntry, AgentToolCall, AgentToolContent, AgentToolKind, AgentToolStatus, AgentTurnStatus,
    AgentViewState, ProgramBlockKind, ProgramState, ProgramStatusRecord, ServerMessage, SessionAgent,
};
use serde_json::Value;

use super::agent_driver::claude_transcript_tool_call;

pub const MAX_READ_BYTES: usize = 1024 * 1024;
const MAX_PROJECT_DIRS: usize = 65_536;
const GUARD_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("transcript requires a fixed canonical session UUID")]
    InvalidSessionId,
    #[error("transcript requires an explicit CLAUDE_CONFIG_DIR or HOME")]
    MissingConfigDirectory,
    #[error("transcript configuration directory must be absolute: {0}")]
    RelativeConfigDirectory(PathBuf),
    #[error("invalid transcript limit: {0}")]
    InvalidLimit(&'static str),
    #[error("transcript mirror reached its {resource} limit ({limit}); use the original terminal")]
    ResourceLimit { resource: &'static str, limit: usize },
    #[error("invalid transcript JSON at line {line}: {source}")]
    InvalidJson { line: usize, #[source] source: serde_json::Error },
    #[error("invalid transcript record at line {line}: {reason}")]
    InvalidRecord { line: usize, reason: &'static str },
    #[error("cannot read transcript path {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("multiple transcript files match the fixed session UUID; use the original terminal")]
    AmbiguousFiles,
    #[error("transcript changed during the read; retry with the previous cursor")]
    ChangedDuringRead,
}

fn valid_session_uuid(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, b)| {
        if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }
    })
}

/// 资源上限是明确的失败条件，不是内容预览长度；失败时不提交该增量的任何修改。
#[derive(Debug, Clone, Copy)]
pub struct TranscriptLimits {
    pub max_chunk_bytes: usize,
    pub max_line_bytes: usize,
    pub max_total_bytes: usize,
    pub max_records: usize,
    pub max_entries: usize,
}

impl Default for TranscriptLimits {
    fn default() -> Self {
        Self {
            max_chunk_bytes: MAX_READ_BYTES,
            max_line_bytes: 8 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
            max_records: 100_000,
            max_entries: 200_000,
        }
    }
}

pub struct ClaudeTranscript {
    session_id: String,
    state: AgentViewState,
    pending: Vec<u8>,
    seen_uuids: HashSet<String>,
    entry_indices: HashMap<String, usize>,
    total_bytes: usize,
    lines: usize,
    limits: TranscriptLimits,
}

enum Change {
    Entry(AgentEntry),
    ToolResult { id: String, text: String, failed: bool },
}

impl Change {
    fn id(&self) -> &str {
        match self {
            Self::Entry(entry) => entry.id().expect("transcript entries have IDs"),
            Self::ToolResult { id, .. } => id,
        }
    }
}

impl ClaudeTranscript {
    pub fn new(session_id: &str, cwd: Option<String>) -> Result<Self, TranscriptError> {
        Self::with_limits(session_id, cwd, TranscriptLimits::default())
    }

    pub fn with_limits(session_id: &str, cwd: Option<String>, limits: TranscriptLimits) -> Result<Self, TranscriptError> {
        if !valid_session_uuid(session_id) { return Err(TranscriptError::InvalidSessionId); }
        if [limits.max_chunk_bytes, limits.max_line_bytes, limits.max_total_bytes, limits.max_records, limits.max_entries].contains(&0) {
            return Err(TranscriptError::InvalidLimit("all parser limits must be positive"));
        }
        let mut state = AgentViewState::new(SessionAgent::Claude, true);
        state.terminal_backed = true;
        state.cwd = cwd;
        // 文件只证明某条历史发生过，不能证明当前空闲或正在执行。
        state.status = AgentTurnStatus::Unknown;
        Ok(Self {
            session_id: session_id.to_string(), state, pending: Vec::new(), seen_uuids: HashSet::new(),
            entry_indices: HashMap::new(), total_bytes: 0, lines: 0, limits,
        })
    }

    pub fn snapshot(&self) -> AgentViewState { self.state.clone() }

    /// 只有管理器提供的真实程序记录能更新当前状态。历史工具记录不生成 OSC 状态，
    /// blocked 的非审批场景也不能伪造可在 GUI 中处理的权限请求。
    pub fn update_program_status(&mut self, records: &[ProgramStatusRecord]) -> Vec<ServerMessage> {
        let status = if records.iter().any(|r| r.state == ProgramState::Blocked && r.kind == Some(ProgramBlockKind::Permission)) {
            AgentTurnStatus::AwaitingPermission
        } else if records.iter().any(|r| r.state == ProgramState::Blocked) {
            AgentTurnStatus::Unknown
        } else if records.iter().any(|r| r.state == ProgramState::Working) {
            AgentTurnStatus::Running
        } else if records.iter().any(|r| r.state == ProgramState::Error) {
            AgentTurnStatus::Error
        } else if records.iter().any(|r| matches!(r.state, ProgramState::Idle | ProgramState::Done)) {
            AgentTurnStatus::Idle
        } else {
            AgentTurnStatus::Unknown
        };
        if status == self.state.status { return Vec::new(); }
        self.state.status = status;
        vec![ServerMessage::AgentSnapshot { state: self.snapshot() }]
    }

    /// 增量是原始文件字节；reset 用于首次发现、截断、替换或文件消失。
    /// 整个调用先校验再提交，错误后可用原游标重试，不会重复半行或丢失 UTF-8。
    /// reset 清时间线与幂等集合，但保留最新的真实程序状态和传入的 cwd。
    pub fn ingest(&mut self, bytes: &[u8], reset: bool) -> Result<Vec<ServerMessage>, TranscriptError> {
        check_limit(bytes.len(), self.limits.max_chunk_bytes, "chunk bytes")?;
        let total_bytes = (if reset { 0 } else { self.total_bytes }).checked_add(bytes.len())
            .ok_or(TranscriptError::ResourceLimit { resource: "total bytes", limit: self.limits.max_total_bytes })?;
        check_limit(total_bytes, self.limits.max_total_bytes, "total bytes")?;
        let mut pending = if reset { Vec::new() } else { self.pending.clone() };
        pending.extend_from_slice(bytes);
        let mut start = 0;
        let mut lines = if reset { 0 } else { self.lines };
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for (end, byte) in pending.iter().enumerate() {
            if *byte != b'\n' { continue; }
            lines += 1;
            let line = &pending[start..end];
            start = end + 1;
            check_limit(line.len(), self.limits.max_line_bytes, "line bytes")?;
            if line.iter().all(u8::is_ascii_whitespace) { continue; }
            let record: Value = serde_json::from_slice(line).map_err(|source| TranscriptError::InvalidJson { line: lines, source })?;
            if !self.accept_record(&record) { continue; }
            let uuid = record["uuid"].as_str().filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or(TranscriptError::InvalidRecord { line: lines, reason: "message UUID is missing or exceeds 256 bytes" })?;
            if (!reset && self.seen_uuids.contains(uuid)) || !seen.insert(uuid.to_string()) { continue; }
            check_limit((if reset { 0 } else { self.seen_uuids.len() }) + seen.len(), self.limits.max_records, "message records")?;
            parse_record(&record, uuid, self.state.cwd.as_deref(), lines, &mut changes)?;
        }
        check_limit(pending.len() - start, self.limits.max_line_bytes, "line bytes")?;
        let new_ids: HashSet<&str> = changes.iter().map(Change::id)
            .filter(|id| reset || !self.entry_indices.contains_key(*id)).collect();
        check_limit((if reset { 0 } else { self.state.entries.len() }) + new_ids.len(), self.limits.max_entries, "timeline entries")?;
        // 所有可失败操作已结束；只克隆当前增量，不为每次轮询克隆整段历史。
        if reset {
            self.state.entries.clear();
            self.entry_indices.clear();
            self.seen_uuids.clear();
        }
        self.seen_uuids.extend(seen);
        self.total_bytes = total_bytes;
        self.lines = lines;
        pending.drain(..start);
        self.pending = pending;
        let mut messages = Vec::new();
        for change in changes {
            let entry = match change {
                Change::Entry(entry) => entry,
                Change::ToolResult { id, text, failed } => {
                    let mut tool = self.entry_indices.get(&id).and_then(|index| match &self.state.entries[*index] {
                        AgentEntry::ToolCall(tool) => Some(tool.clone()), _ => None,
                    }).unwrap_or_else(|| AgentToolCall {
                        id: id.clone(), title: "Tool result".into(), description: None, file_path: None,
                        kind: AgentToolKind::Other, status: AgentToolStatus::Unknown, content: Vec::new(),
                        permission_pending: false, permission_reason: None,
                    });
                    tool.status = if failed { AgentToolStatus::Failed } else { AgentToolStatus::Completed };
                    tool.permission_pending = false;
                    if !text.is_empty() {
                        if let Some(AgentToolContent::Terminal { output, .. }) = tool.content.iter_mut().find(|c| matches!(c, AgentToolContent::Terminal { .. })) {
                            *output = text;
                        } else {
                            // Edit/Write 的结果也保留；差异内容不是执行结果的替代品。
                            tool.content.push(AgentToolContent::Text { text });
                        }
                    }
                    AgentEntry::ToolCall(tool)
                }
            };
            let id = entry.id().expect("transcript entries have IDs").to_string();
            if let Some(index) = self.entry_indices.get(&id) {
                self.state.entries[*index] = entry.clone();
            } else {
                self.entry_indices.insert(id, self.state.entries.len());
                self.state.entries.push(entry.clone());
            }
            if !reset { messages.push(ServerMessage::AgentEntry { entry }); }
        }
        if reset { messages.push(ServerMessage::AgentSnapshot { state: self.snapshot() }); }
        Ok(messages)
    }

    fn accept_record(&self, v: &Value) -> bool {
        // 独立 allowlist：绝不能把 control_request/response、system/init、stream_event
        // 或进程哨兵交给现有双向驱动器；那些记录不能带来初始化、审批或 PTY 写入。
        matches!(v["type"].as_str(), Some("assistant" | "user"))
            && v["sessionId"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(&self.session_id))
            && ["isSidechain", "isMeta", "isCompactSummary"].iter().all(|key| {
                v.get(*key).is_none_or(|value| value.is_null() || value.as_bool() == Some(false))
            })
            && ["parentToolUseId", "parent_tool_use_id"].iter().all(|key| v.get(*key).is_none_or(Value::is_null))
            && v["message"].get("role").is_none_or(|role| role.as_str() == v["type"].as_str())
    }
}

fn check_limit(actual: usize, limit: usize, resource: &'static str) -> Result<(), TranscriptError> {
    if actual > limit { Err(TranscriptError::ResourceLimit { resource, limit }) } else { Ok(()) }
}

fn tool_id(id: &str) -> String { format!("claude-transcript-tool-{id}") }

fn parse_record(v: &Value, uuid: &str, cwd: Option<&str>, line: usize, changes: &mut Vec<Change>) -> Result<(), TranscriptError> {
    let content = &v["message"]["content"];
    let assistant = v["type"].as_str() == Some("assistant");
    if !assistant && let Some(text) = content.as_str() {
        changes.push(Change::Entry(AgentEntry::User { id: format!("claude-transcript-user-{uuid}"), text: text.to_string() }));
        return Ok(());
    }
    let blocks = content.as_array().ok_or(TranscriptError::InvalidRecord { line, reason: "message content is not a supported string or block array" })?;
    for (index, block) in blocks.iter().enumerate() {
        let id = format!("claude-transcript-{uuid}-{index}");
        let entry = match (assistant, block["type"].as_str()) {
            (true, Some("text")) => block["text"].as_str().map(|text| AgentEntry::Message { id, text: text.to_string() }),
            (true, Some("thinking")) => block["thinking"].as_str().map(|text| AgentEntry::Thought { id, text: text.to_string() }),
            (true, Some("tool_use")) => {
                let id = block["id"].as_str().filter(|s| !s.is_empty()).ok_or(TranscriptError::InvalidRecord { line, reason: "tool use ID is missing" })?;
                let name = block["name"].as_str().ok_or(TranscriptError::InvalidRecord { line, reason: "tool name is missing" })?;
                Some(AgentEntry::ToolCall(claude_transcript_tool_call(&tool_id(id), name, &block["input"], cwd)))
            }
            (false, Some("text")) => block["text"].as_str().map(|text| AgentEntry::User { id, text: text.to_string() }),
            (false, Some("tool_result")) => {
                let id = block["tool_use_id"].as_str().filter(|s| !s.is_empty()).ok_or(TranscriptError::InvalidRecord { line, reason: "tool result ID is missing" })?;
                changes.push(Change::ToolResult { id: tool_id(id), text: tool_result_text(block, &v["toolUseResult"]), failed: block["is_error"].as_bool().unwrap_or(false) });
                None
            }
            _ => None,
        };
        if let Some(entry) = entry { changes.push(Change::Entry(entry)); }
    }
    Ok(())
}

fn tool_result_text(block: &Value, tool_use_result: &Value) -> String {
    if let Some(text) = block["content"].as_str() { return text.to_string(); }
    if let Some(blocks) = block["content"].as_array() {
        return blocks.iter().filter(|v| v["type"].as_str() == Some("text"))
            .filter_map(|v| v["text"].as_str()).collect::<Vec<_>>().join("\n");
    }
    [tool_use_result["stdout"].as_str(), tool_use_result["stderr"].as_str()]
        .into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

/// 显式目录来源。不调用 env::var，不猜测后端进程的 HOME，也不读取设置或凭据。
#[derive(Debug, Clone)]
pub struct LocalTranscriptSource {
    projects_dir: PathBuf,
    file_name: String,
}

/// 文件身份与消费位置；只在对应字节已被解析器接受后提交。
#[derive(Debug, Clone, Default)]
pub struct TranscriptCursor {
    pub path: Option<PathBuf>,
    pub offset: u64,
    identity: Option<FileIdentity>,
    observed_len: u64,
    modified: Option<SystemTime>,
    prefix: Vec<u8>,
    tail: Vec<u8>,
}

#[derive(Debug)]
pub struct TranscriptChunk {
    pub bytes: Vec<u8>,
    pub reset: bool,
    pub cursor: TranscriptCursor,
    /// false 表示尚无记录；调用者应明确显示此状态，并保留真实终端供继续操作。
    pub found: bool,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
    created: Option<SystemTime>,
}

impl LocalTranscriptSource {
    pub fn new(config_dir: Option<PathBuf>, home: Option<PathBuf>, session_id: &str) -> Result<Self, TranscriptError> {
        if !valid_session_uuid(session_id) { return Err(TranscriptError::InvalidSessionId); }
        let config_dir = config_dir.filter(|p| !p.as_os_str().is_empty())
            .or_else(|| home.filter(|p| !p.as_os_str().is_empty()).map(|p| p.join(".claude")))
            .ok_or(TranscriptError::MissingConfigDirectory)?;
        if !config_dir.is_absolute() { return Err(TranscriptError::RelativeConfigDirectory(config_dir)); }
        Ok(Self { projects_dir: config_dir.join("projects"), file_name: format!("{session_id}.jsonl") })
    }

    /// 同步阻塞 I/O，必须在会话引擎外运行。只扫描 projects 的下一层，并且只打开指定 UUID
    /// 的普通文件；不跟随项目目录或 transcript 文件的符号链接，也不读任何其它对话正文。
    pub fn read_chunk(&self, cursor: &TranscriptCursor, max_bytes: usize) -> Result<TranscriptChunk, TranscriptError> {
        if max_bytes == 0 || max_bytes > MAX_READ_BYTES { return Err(TranscriptError::InvalidLimit("read size must be 1..=MAX_READ_BYTES")); }
        let Some(path) = self.locate()? else { return Ok(missing_chunk(cursor)); };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(missing_chunk(cursor)),
            Err(source) => return Err(io_error(&path, source)),
        };
        let metadata = file.metadata().map_err(|e| io_error(&path, e))?;
        if !metadata.is_file() { return Err(io_error(&path, std::io::Error::other("not a regular transcript file"))); }
        let identity = file_identity(&file, &metadata).map_err(|e| io_error(&path, e))?;
        let mut reset = cursor.path.as_ref() != Some(&path) || cursor.identity.as_ref() != Some(&identity)
            || metadata.len() < cursor.observed_len || metadata.len() < cursor.offset;
        // inode 不变的 copytruncate 可能在两次轮询间重新长到旧 offset；首尾小窗口校验避免
        // 把新文件尾巴拼到旧半行上。只改 mtime 的 touch 最多触发一次无损重建。
        if !reset {
            reset = (metadata.len() == cursor.offset && metadata.modified().ok() != cursor.modified)
                || read_at(&mut file, 0, cursor.prefix.len(), &path)? != cursor.prefix
                || read_at(&mut file, cursor.offset.saturating_sub(cursor.tail.len() as u64), cursor.tail.len(), &path)? != cursor.tail;
        }
        let offset = if reset { 0 } else { cursor.offset };
        let size = (metadata.len() - offset).min(max_bytes as u64) as usize;
        let bytes = read_at(&mut file, offset, size, &path)?;
        let offset = offset + bytes.len() as u64;
        let guard_len = offset.min(GUARD_BYTES as u64) as usize;
        let prefix = read_at(&mut file, 0, guard_len, &path)?;
        let tail = read_at(&mut file, offset - guard_len as u64, guard_len, &path)?;
        let after = file.metadata().map_err(|e| io_error(&path, e))?;
        if after.len() < offset { return Err(TranscriptError::ChangedDuringRead); }
        Ok(TranscriptChunk {
            bytes, reset, found: true, has_more: after.len() > offset,
            cursor: TranscriptCursor { path: Some(path), offset, identity: Some(identity), observed_len: after.len(), modified: after.modified().ok(), prefix, tail },
        })
    }

    fn locate(&self) -> Result<Option<PathBuf>, TranscriptError> {
        let entries = match std::fs::read_dir(&self.projects_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(&self.projects_dir, e)),
        };
        let mut found = None;
        for (index, entry) in entries.enumerate() {
            check_limit(index + 1, MAX_PROJECT_DIRS, "project directory entries")?;
            let entry = entry.map_err(|e| io_error(&self.projects_dir, e))?;
            if !entry.file_type().map_err(|e| io_error(&entry.path(), e))?.is_dir() { continue; }
            let path = entry.path().join(&self.file_name);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    if found.is_some() { return Err(TranscriptError::AmbiguousFiles); }
                    found = Some(path);
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(&path, e)),
            }
        }
        Ok(found)
    }
}

fn missing_chunk(cursor: &TranscriptCursor) -> TranscriptChunk {
    TranscriptChunk { bytes: Vec::new(), reset: cursor.identity.is_some(), cursor: TranscriptCursor::default(), found: false, has_more: false }
}

fn io_error(path: &Path, source: std::io::Error) -> TranscriptError { TranscriptError::Io { path: path.to_path_buf(), source } }

fn read_at(file: &mut File, offset: u64, size: usize, path: &Path) -> Result<Vec<u8>, TranscriptError> {
    file.seek(SeekFrom::Start(offset)).map_err(|e| io_error(path, e))?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof {
        TranscriptError::ChangedDuringRead
    } else { io_error(path, e) })?;
    Ok(bytes)
}

fn file_identity(_file: &File, metadata: &Metadata) -> std::io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity { device: metadata.dev(), inode: metadata.ino(), created: metadata.created().ok() })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // std MetadataExt 的 file_index/volume_serial_number 目前仍受不稳定特性限制。
        // 只用只读句柄查询稳定的系统文件身份，不能用 mtime 代替（每次追加都会改变它）。
        #[repr(C)]
        #[derive(Default)]
        struct FileInformation {
            attributes: u32, creation: [u32; 2], access: [u32; 2], write: [u32; 2], volume: u32,
            size_high: u32, size_low: u32, links: u32, index_high: u32, index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" { fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut FileInformation) -> i32; }
        let mut info = FileInformation::default();
        if unsafe { GetFileInformationByHandle(_file.as_raw_handle(), &mut info) } == 0 { return Err(std::io::Error::last_os_error()); }
        Ok(FileIdentity { volume: info.volume, index: (u64::from(info.index_high) << 32) | u64::from(info.index_low), created: metadata.created().ok() })
    }
    #[cfg(not(any(unix, windows)))]
    { Ok(FileIdentity { created: metadata.created().ok() }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    const SESSION: &str = "12345678-1234-4321-9876-123456789abc";
    const OTHER: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    fn row(kind: &str, uuid: &str, content: Value) -> Value {
        json!({"type":kind,"sessionId":SESSION,"uuid":uuid,"message":{"role":kind,"content":content}})
    }

    fn line(value: Value) -> Vec<u8> { format!("{value}\n").into_bytes() }
    fn mirror() -> ClaudeTranscript { ClaudeTranscript::new(SESSION, Some("/project".into())).unwrap() }

    #[test]
    fn transcript_parses_full_history_and_never_infers_live_execution() {
        let mut mirror = mirror();
        let long_text = "完整输出\n".repeat(5000);
        let mut bytes = line(row("user", "u1", json!("相同正文")));
        bytes.extend(line(row("assistant", "a1", json!([
            {"type":"thinking","thinking":"先检查"}, {"type":"text","text":"相同正文"},
            {"type":"tool_use","id":"bash","name":"Bash","input":{"command":"pwd","description":"路径"}},
            {"type":"tool_use","id":"write","name":"Write","input":{"file_path":"/project/a","content":long_text}},
            {"type":"tool_use","id":"unresolved","name":"Read","input":{"file_path":"/project/b"}}
        ]))));
        bytes.extend(line(row("user", "r1", json!([
            {"type":"text","text":"继续"},
            {"type":"tool_result","tool_use_id":"bash","content":[{"type":"text","text":long_text}]},
            {"type":"tool_result","tool_use_id":"write","is_error":true,"content":"failed"}
        ]))));
        let events = mirror.ingest(&bytes, true).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ServerMessage::AgentSnapshot { .. }));
        let state = mirror.snapshot();
        assert!(state.active && state.terminal_backed);
        assert_eq!(state.status, AgentTurnStatus::Unknown);
        assert!(state.models.is_empty() && state.model_options.is_empty() && state.commands.is_empty());
        assert!(state.permission_mode.is_none());
        assert_eq!(state.entries.len(), 7);
        let tools: Vec<_> = state.entries.iter().filter_map(|e| if let AgentEntry::ToolCall(t) = e { Some(t) } else { None }).collect();
        assert_eq!(tools.iter().map(|t| t.status).collect::<Vec<_>>(), [AgentToolStatus::Completed, AgentToolStatus::Failed, AgentToolStatus::Unknown]);
        assert!(tools.iter().all(|t| !t.permission_pending));
        assert!(matches!(&tools[0].content[0], AgentToolContent::Terminal { output, .. } if output == &long_text));
        assert!(matches!(&tools[1].content[0], AgentToolContent::Diff { new_text: Some(text), .. } if text == &long_text));
        assert!(matches!(&tools[1].content[1], AgentToolContent::Text { text } if text == "failed"));
    }

    #[test]
    fn transcript_allowlist_rejects_controls_other_sessions_sidechains_and_metadata() {
        let mut mirror = mirror();
        let base = row("assistant", "a1", json!([{"type":"text","text":"visible"}]));
        for key in ["isSidechain", "isMeta", "isCompactSummary"] {
            let mut ignored = base.clone();
            ignored[key] = json!(true);
            assert!(mirror.ingest(&line(ignored), false).unwrap().is_empty());
        }
        for kind in ["control_request", "control_response", "system", "stream_event", "progress", "summary", "file-history-snapshot", "result"] {
            let mut ignored = base.clone();
            ignored["type"] = json!(kind);
            ignored["message"]["role"] = json!(kind);
            ignored["request"] = json!({"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"x"});
            assert!(mirror.ingest(&line(ignored), false).unwrap().is_empty());
        }
        let mut other = base.clone(); other["sessionId"] = json!(OTHER);
        mirror.ingest(&line(other), false).unwrap();
        let mut snake = base.clone(); snake.as_object_mut().unwrap().remove("sessionId"); snake["session_id"] = json!(SESSION);
        mirror.ingest(&line(snake), false).unwrap();
        let mut subagent = base.clone(); subagent["parent_tool_use_id"] = json!("parent");
        mirror.ingest(&line(subagent), false).unwrap();
        assert!(mirror.snapshot().entries.is_empty());
        assert_eq!(mirror.ingest(&line(base), false).unwrap().len(), 1, "ignored UUIDs must not poison idempotency");
        assert_eq!(mirror.snapshot().status, AgentTurnStatus::Unknown);
    }

    #[test]
    fn transcript_uuid_is_idempotent_but_repeated_bodies_are_independent() {
        let mut mirror = mirror();
        let a = row("assistant", "a", json!([{"type":"text","text":"same"}, {"type":"text","text":"same"}]));
        mirror.ingest(&line(a.clone()), false).unwrap();
        assert!(mirror.ingest(&line(a), false).unwrap().is_empty());
        mirror.ingest(&line(row("assistant", "b", json!([{"type":"text","text":"same"}]))), false).unwrap();
        let state = mirror.snapshot();
        assert_eq!(state.entries.len(), 3);
        let ids: HashSet<_> = state.entries.iter().filter_map(AgentEntry::id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn transcript_handles_every_utf8_byte_boundary_and_crlf() {
        let bytes = line(row("assistant", "a", json!([{"type":"text","text":"中文🙂\n末尾"}])));
        for boundary in 0..=bytes.len() {
            let mut mirror = mirror();
            mirror.ingest(&bytes[..boundary], false).unwrap();
            mirror.ingest(&bytes[boundary..], false).unwrap();
            assert!(matches!(&mirror.snapshot().entries[0], AgentEntry::Message { text, .. } if text == "中文🙂\n末尾"));
        }
        let mut mirror = mirror();
        for byte in bytes[..bytes.len()-1].iter() { mirror.ingest(&[*byte], false).unwrap(); }
        assert!(mirror.snapshot().entries.is_empty());
        mirror.ingest(b"\r\n", false).unwrap();
        assert_eq!(mirror.snapshot().entries.len(), 1);
    }

    fn program(state: ProgramState, kind: Option<ProgramBlockKind>) -> ProgramStatusRecord {
        ProgramStatusRecord { id: String::new(), state, kind, progress: None, app: Some("Claude Code".into()), title: None, message: None, updated_at: 1 }
    }

    #[test]
    fn transcript_status_comes_from_real_records_and_survives_file_reset() {
        let mut mirror = mirror();
        assert_eq!(mirror.update_program_status(&[program(ProgramState::Working, None)]).len(), 1);
        let bytes = line(row("assistant", "a", json!([{"type":"text","text":"done long ago"}])));
        mirror.ingest(&bytes, true).unwrap();
        assert_eq!(mirror.snapshot().status, AgentTurnStatus::Running);
        assert_eq!(mirror.update_program_status(&[program(ProgramState::Blocked, Some(ProgramBlockKind::Permission))]).len(), 1);
        mirror.ingest(b"", true).unwrap();
        assert_eq!(mirror.snapshot().status, AgentTurnStatus::AwaitingPermission);
        assert!(mirror.snapshot().entries.is_empty());
        mirror.update_program_status(&[program(ProgramState::Done, None)]);
        mirror.ingest(&bytes, false).unwrap();
        assert_eq!(mirror.snapshot().status, AgentTurnStatus::Idle);
        mirror.update_program_status(&[]);
        assert_eq!(mirror.snapshot().status, AgentTurnStatus::Unknown);
    }

    #[test]
    fn transcript_limits_and_parse_errors_are_transactional_and_retryable() {
        let bytes = line(row("user", "u", json!("hello")));
        let mut mirror = ClaudeTranscript::with_limits(SESSION, None, TranscriptLimits { max_entries: 1, ..Default::default() }).unwrap();
        let mut oversized = bytes.clone(); oversized.extend(line(row("user", "v", json!("world"))));
        assert!(matches!(mirror.ingest(&oversized, false), Err(TranscriptError::ResourceLimit { resource: "timeline entries", .. })));
        assert!(mirror.snapshot().entries.is_empty());
        mirror.ingest(&bytes[..3], false).unwrap();
        let mut bad = bytes[3..].to_vec(); bad.extend(b"{bad json}\n");
        assert!(matches!(mirror.ingest(&bad, false), Err(TranscriptError::InvalidJson { .. })));
        mirror.ingest(&bytes[3..], false).unwrap();
        assert_eq!(mirror.snapshot().entries.len(), 1);
        let before = mirror.snapshot();
        assert!(mirror.ingest(b"\xff\n", true).is_err());
        assert_eq!(mirror.snapshot(), before, "failed resets preserve the last readable mirror");
        mirror.ingest(&bytes, true).unwrap();
        assert_eq!(mirror.snapshot().entries.len(), 1, "reset clears UUID idempotency");
        for (limits, bytes) in [
            (TranscriptLimits { max_chunk_bytes: 1, ..Default::default() }, b"{}".as_slice()),
            (TranscriptLimits { max_line_bytes: 1, ..Default::default() }, b"{}\n".as_slice()),
            (TranscriptLimits { max_total_bytes: 1, ..Default::default() }, b"  ".as_slice()),
        ] {
            let mut mirror = ClaudeTranscript::with_limits(SESSION, None, limits).unwrap();
            assert!(matches!(mirror.ingest(bytes, false), Err(TranscriptError::ResourceLimit { .. })));
        }
    }

    #[test]
    fn transcript_orphan_tool_results_preserve_text_and_failures() {
        let mut mirror = mirror();
        let mut record = row("user", "r", json!([{"type":"tool_result","tool_use_id":"missing","is_error":true}]));
        record["toolUseResult"] = json!({"stdout":"all stdout","stderr":"all stderr"});
        mirror.ingest(&line(record), false).unwrap();
        assert!(matches!(&mirror.snapshot().entries[0], AgentEntry::ToolCall(tool)
            if tool.status == AgentToolStatus::Failed && tool.content == [AgentToolContent::Text { text: "all stdout\nall stderr".into() }]));
    }

    fn source_fixture() -> (tempfile::TempDir, LocalTranscriptSource, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let source = LocalTranscriptSource::new(Some(temp.path().join("config")), None, SESSION).unwrap();
        let dir = temp.path().join("config/projects/-project");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SESSION}.jsonl"));
        (temp, source, path)
    }

    #[test]
    fn transcript_reader_is_bounded_and_preserves_split_utf8_and_missing_files() {
        let (_temp, source, path) = source_fixture();
        let missing = source.read_chunk(&TranscriptCursor::default(), 2).unwrap();
        assert!(!missing.found && !missing.reset && missing.bytes.is_empty());
        let bytes = line(row("user", "u", json!("中文🙂")));
        std::fs::write(&path, &bytes).unwrap();
        let mut cursor = missing.cursor;
        let mut mirror = mirror();
        let mut read = Vec::new();
        loop {
            let chunk = source.read_chunk(&cursor, 2).unwrap();
            assert!(chunk.found && chunk.bytes.len() <= 2);
            assert_eq!(chunk.reset, cursor.path.is_none());
            mirror.ingest(&chunk.bytes, chunk.reset).unwrap();
            read.extend_from_slice(&chunk.bytes);
            cursor = chunk.cursor;
            if !chunk.has_more { break; }
        }
        assert_eq!(read, bytes);
        assert_eq!(cursor.offset, bytes.len() as u64);
        assert_eq!(mirror.snapshot().entries.len(), 1);
        let empty = source.read_chunk(&cursor, 2).unwrap();
        assert!(empty.bytes.is_empty() && !empty.reset);
        std::fs::remove_file(&path).unwrap();
        let deleted = source.read_chunk(&cursor, 2).unwrap();
        assert!(!deleted.found && deleted.reset);
        mirror.ingest(&deleted.bytes, deleted.reset).unwrap();
        assert!(mirror.snapshot().entries.is_empty());
    }

    #[test]
    fn transcript_reader_appends_resets_on_truncation_and_replacement() {
        let (_temp, source, path) = source_fixture();
        std::fs::write(&path, b"first\n").unwrap();
        let first = source.read_chunk(&TranscriptCursor::default(), 1024).unwrap();
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(b"second\n").unwrap();
        let second = source.read_chunk(&first.cursor, 1024).unwrap();
        assert_eq!(second.bytes, b"second\n");
        assert!(!second.reset);
        std::fs::write(&path, b"x\n").unwrap();
        let short = source.read_chunk(&second.cursor, 1024).unwrap();
        assert!(short.reset);
        assert_eq!(short.bytes, b"x\n");
        // 相同 inode 在两次轮询间截断再增长到旧长度以上。
        std::fs::write(&path, b"a much longer replacement\n").unwrap();
        let regrown = source.read_chunk(&short.cursor, 1024).unwrap();
        assert!(regrown.reset);
        assert_eq!(regrown.bytes, b"a much longer replacement\n");
        let renamed = path.with_extension("old");
        std::fs::rename(&path, &renamed).unwrap();
        std::fs::write(&path, &regrown.bytes).unwrap();
        let replaced = source.read_chunk(&regrown.cursor, 1024).unwrap();
        assert!(replaced.reset, "same bytes at a new file identity still reset");
        assert_eq!(replaced.bytes, regrown.bytes);
    }

    #[test]
    fn transcript_reader_refuses_ambiguity_and_does_not_guess_unrelated_sessions() {
        let (temp, source, path) = source_fixture();
        std::fs::write(path.with_file_name(format!("{OTHER}.jsonl")), b"unrelated").unwrap();
        assert!(!source.read_chunk(&TranscriptCursor::default(), 1).unwrap().found);
        std::fs::write(&path, b"target").unwrap();
        let duplicate = temp.path().join("config/projects/other");
        std::fs::create_dir_all(&duplicate).unwrap();
        std::fs::write(duplicate.join(path.file_name().unwrap()), b"duplicate").unwrap();
        assert!(matches!(source.read_chunk(&TranscriptCursor::default(), 1), Err(TranscriptError::AmbiguousFiles)));
    }

    #[test]
    fn transcript_reader_requires_explicit_absolute_roots_and_valid_uuid() {
        assert!(matches!(LocalTranscriptSource::new(None, None, SESSION), Err(TranscriptError::MissingConfigDirectory)));
        assert!(matches!(LocalTranscriptSource::new(Some("relative".into()), None, SESSION), Err(TranscriptError::RelativeConfigDirectory(_))));
        for id in ["../other", "", "12345678-1234-4321-9876-123456789abz", "$(touch /tmp/x)"] {
            assert!(matches!(LocalTranscriptSource::new(Some("/tmp".into()), None, id), Err(TranscriptError::InvalidSessionId)));
            assert!(matches!(ClaudeTranscript::new(id, None), Err(TranscriptError::InvalidSessionId)));
        }
        let temp = tempfile::tempdir().unwrap();
        let fallback = LocalTranscriptSource::new(None, Some(temp.path().to_path_buf()), SESSION).unwrap();
        assert_eq!(fallback.projects_dir, temp.path().join(".claude/projects"));
        let source = LocalTranscriptSource::new(Some(temp.path().to_path_buf()), Some("/unused-home".into()), SESSION).unwrap();
        assert_eq!(source.projects_dir, temp.path().join("projects"));
        assert!(matches!(source.read_chunk(&TranscriptCursor::default(), MAX_READ_BYTES + 1), Err(TranscriptError::InvalidLimit(_))));
    }

    #[cfg(unix)]
    #[test]
    fn transcript_reader_ignores_symlinks_and_non_regular_files() {
        use std::os::unix::fs::symlink;
        let (temp, source, path) = source_fixture();
        let target = temp.path().join("outside.jsonl");
        std::fs::write(&target, b"must not read").unwrap();
        symlink(&target, &path).unwrap();
        assert!(!source.read_chunk(&TranscriptCursor::default(), 1024).unwrap().found);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!source.read_chunk(&TranscriptCursor::default(), 1024).unwrap().found);
        std::fs::remove_dir(&path).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join(path.file_name().unwrap()), b"must not read").unwrap();
        symlink(&outside, source.projects_dir.join("linked-project")).unwrap();
        assert!(!source.read_chunk(&TranscriptCursor::default(), 1024).unwrap().found);
    }
}
