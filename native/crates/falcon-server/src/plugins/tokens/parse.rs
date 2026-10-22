//! 各家 Coding Agent 本地会话记录的解析与增量缓存。
//!
//! 统一归一成 [`SessionRecord`]：
//! - **Claude Code**（`~/.claude/projects/<slug>/<id>.jsonl` 与 `<id>/subagents/*.jsonl`）：
//!   流式写盘时同一个 `message.id` 会写多行，按 `message.id` 去重留最后一条 `usage`；
//! - **Codex**（`~/.codex/state_*.sqlite` 的 `threads` 表 + `rollout_path` 指向的 JSONL）：
//!   JSONL 的 `token_usage_record` 有每轮的 `usage`（含输入、缓存命中、输出、推理），缺失时用表里的 `tokens_used` 兜底；
//! - **Grok CLI**（`~/.grok/sessions/<cwd_encoded>/<id>/summary.json` + `updates.jsonl`）：
//!   累加 `updates.jsonl` 里每条 `turn_completed` 的 `usage`，缺失时读 `signals.json` 的 `contextTokensUsed`；
//! - **Devin CLI**（`~/.local/share/devin/cli/sessions.db` + `transcripts/*.json`）：
//!   `message_nodes` 里同一个 `request_id` 会有文字与工具调用两条节点，按 `request_id` 去重累加 `metadata.metrics`；
//! - **OpenCode**（`~/.local/share/opencode/opencode.db`）：
//!   读 `session_v2` / `session` 表的 `tokens_input`、`tokens_output`、`tokens_reasoning`、`tokens_cache_read`、`tokens_cache_write`。

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::file_stamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
    Grok,
    Devin,
    OpenCode,
}

impl AgentKind {
    pub const ALL: [AgentKind; 5] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Grok,
        AgentKind::Devin,
        AgentKind::OpenCode,
    ];

    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Grok => "grok",
            AgentKind::Devin => "devin",
            AgentKind::OpenCode => "opencode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Grok => "Grok",
            AgentKind::Devin => "Devin",
            AgentKind::OpenCode => "OpenCode",
        }
    }

    pub fn from_param(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(AgentKind::Claude),
            "codex" => Some(AgentKind::Codex),
            "grok" => Some(AgentKind::Grok),
            "devin" => Some(AgentKind::Devin),
            "opencode" => Some(AgentKind::OpenCode),
            _ => None,
        }
    }
}

/// 各项 Token 计数。
///
/// 注意各家对 `input_tokens` 的口径不同：Claude / Devin / OpenCode 的 `input_tokens` 是**未命中缓存的新输入**，
/// 缓存读与缓存写单独算；而 Codex / Grok 的 `input_tokens` 已经**包含了** `cache_read_tokens`。
/// 这里统一把 [`Usage::input`] 规整为「非缓存输入」（Codex / Grok 先减去 `cache_read`），
/// 这样无论哪一家，都有 [`Usage::total`] = `input + cache_read + cache_write + output`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// 未命中缓存的输入 Token
    pub input: u64,
    /// 缓存命中读取的输入 Token
    pub cache_read: u64,
    /// 写入缓存的输入 Token
    pub cache_write: u64,
    /// 输出 Token（含思考 / 推理 Token）
    pub output: u64,
    /// 思考 / 推理 Token（`output` 的子集，单独记作明细参考）
    pub reasoning: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.output)
    }

    pub fn total_input(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input = self.input.saturating_add(other.input);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
}

/// 一段对话 / 会话的完整计量记录
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    /// 全局唯一键：`<agent>:<session_id>`
    pub key: String,
    pub id: String,
    pub agent: AgentKind,
    pub title: String,
    pub cwd: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub turns: u32,
    pub usage: Usage,
    /// 按模型名拆分的用量
    pub models: BTreeMap<String, Usage>,
    /// 按 UTC 日期（`epoch_secs / 86400`）拆分的用量，供时间范围过滤与每日走势图使用
    pub by_day: BTreeMap<i64, Usage>,
    /// 按 `(day, model)` 拆分的用量，供特定时间范围内的模型排行榜使用
    pub by_day_model: BTreeMap<(i64, String), Usage>,
    pub cost_usd: Option<f64>,
}

impl SessionRecord {
    fn new(agent: AgentKind, id: String) -> Self {
        SessionRecord {
            key: format!("{}:{id}", agent.id()),
            id,
            agent,
            title: String::new(),
            cwd: None,
            created_at: 0,
            updated_at: 0,
            turns: 0,
            usage: Usage::default(),
            models: BTreeMap::new(),
            by_day: BTreeMap::new(),
            by_day_model: BTreeMap::new(),
            cost_usd: None,
        }
    }

    fn record_turn(&mut self, ts: i64, model: &str, u: &Usage) {
        if u.is_empty() {
            return;
        }
        self.turns = self.turns.saturating_add(1);
        if ts > 0 {
            if self.created_at == 0 || ts < self.created_at {
                self.created_at = ts;
            }
            if ts > self.updated_at {
                self.updated_at = ts;
            }
        }
        self.usage.add(u);
        let model_name = clean_model_name(model);
        if !model_name.is_empty() {
            self.models.entry(model_name.clone()).or_default().add(u);
        }
        let day = if ts > 0 { ts / 86400 } else if self.updated_at > 0 { self.updated_at / 86400 } else { 0 };
        if day > 0 {
            self.by_day.entry(day).or_default().add(u);
            if !model_name.is_empty() {
                self.by_day_model.entry((day, model_name)).or_default().add(u);
            }
        }
    }

    /// 只有整段会话汇总、没有逐轮明细时（例如只有数据库里的总计数），归入 `updated_at` 那一天
    fn set_summary_usage(&mut self, model: &str, u: Usage) {
        if u.is_empty() {
            return;
        }
        self.usage = u;
        let model_name = clean_model_name(model);
        if !model_name.is_empty() {
            self.models.insert(model_name.clone(), u);
        }
        let day = if self.updated_at > 0 { self.updated_at / 86400 } else { 0 };
        if day > 0 {
            self.by_day.insert(day, u);
            if !model_name.is_empty() {
                self.by_day_model.insert((day, model_name), u);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub mtime_ns: u128,
    pub len: u64,
}

/// 增量扫描器：按路径缓存单文件 / 会话目录解析结果
#[derive(Default)]
pub struct Scanner {
    claude: HashMap<PathBuf, (Vec<FileStamp>, Option<SessionRecord>)>,
    grok: HashMap<PathBuf, (Vec<FileStamp>, Option<SessionRecord>)>,
    codex_rollouts: HashMap<PathBuf, (FileStamp, CodexRollout)>,
}

impl Scanner {
    pub fn scan(&mut self, home: &Path) -> Vec<SessionRecord> {
        let mut out = Vec::new();
        self.scan_claude(home, &mut out);
        self.scan_codex(home, &mut out);
        self.scan_grok(home, &mut out);
        scan_devin(home, &mut out);
        scan_opencode(home, &mut out);
        out.retain(|r| !r.usage.is_empty());
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.key.cmp(&b.key)));
        out
    }

    fn scan_claude(&mut self, home: &Path, out: &mut Vec<SessionRecord>) {
        let projects_dir = home.join(".claude").join("projects");
        let Ok(projects) = std::fs::read_dir(&projects_dir) else { return };
        let mut seen = Vec::new();
        for proj in projects.filter_map(Result::ok) {
            let proj_path = proj.path();
            if !proj_path.is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&proj_path) else { continue };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") || !path.is_file() {
                    continue;
                }
                let stamps = claude_bundle_stamps(&path);
                if stamps.is_empty() {
                    continue;
                }
                seen.push(path.clone());
                if let Some((old_stamps, cached)) = self.claude.get(&path)
                    && *old_stamps == stamps
                {
                    if let Some(r) = cached {
                        out.push(r.clone());
                    }
                    continue;
                }
                let parsed = parse_claude_session(&path);
                if let Some(r) = &parsed {
                    out.push(r.clone());
                }
                self.claude.insert(path, (stamps, parsed));
            }
        }
        self.claude.retain(|k, _| seen.contains(k));
    }

    fn scan_codex(&mut self, home: &Path, out: &mut Vec<SessionRecord>) {
        let codex_dir = home.join(".codex");
        if !codex_dir.is_dir() {
            return;
        }
        let mut recorded_paths = Vec::new();
        // 找最新的一份 state_*.sqlite
        if let Some(db_path) = latest_matching_file(&codex_dir, "state_", ".sqlite")
            && let Some(conn) = open_readonly(&db_path)
            && let Ok(mut stmt) = conn.prepare(
                "SELECT id, rollout_path, created_at, updated_at, cwd, title, tokens_used, model FROM threads",
            )
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1).unwrap_or_default(),
                    row.get::<_, i64>(2).unwrap_or(0),
                    row.get::<_, i64>(3).unwrap_or(0),
                    row.get::<_, String>(4).ok().filter(|s| !s.is_empty()),
                    row.get::<_, String>(5).unwrap_or_default(),
                    row.get::<_, i64>(6).unwrap_or(0).max(0) as u64,
                    row.get::<_, String>(7).unwrap_or_default(),
                ))
            })
        {
            for row in rows.filter_map(Result::ok) {
                let (id, rollout_str, created_at, updated_at, cwd, raw_title, tokens_used, db_model) = row;
                let mut rec = SessionRecord::new(AgentKind::Codex, id);
                rec.created_at = normalize_ts(created_at);
                rec.updated_at = normalize_ts(updated_at);
                rec.cwd = cwd;
                rec.title = clean_title(&raw_title);

                let rollout_path = PathBuf::from(&rollout_str);
                if !rollout_str.is_empty() && rollout_path.is_file() {
                    recorded_paths.push(rollout_path.clone());
                    if let Some(rollout) = self.read_codex_rollout(&rollout_path) {
                        let model = if !db_model.is_empty() {
                            db_model.as_str()
                        } else {
                            rollout.model.as_deref().unwrap_or("codex")
                        };
                        if rec.cwd.is_none() {
                            rec.cwd = rollout.cwd;
                        }
                        if rec.title.is_empty()
                            && let Some(t) = rollout.title
                        {
                            rec.title = t;
                        }
                        for (ts, u) in &rollout.turns {
                            rec.record_turn(*ts, model, u);
                        }
                        if rec.usage.is_empty()
                            && let Some(total) = rollout.last_total
                        {
                            rec.turns = rec.turns.max(1);
                            rec.set_summary_usage(model, total);
                        }
                    }
                }
                if rec.usage.is_empty() && tokens_used > 0 {
                    rec.turns = 1;
                    let model = if db_model.is_empty() { "codex" } else { db_model.as_str() };
                    rec.set_summary_usage(model, Usage { input: tokens_used, ..Default::default() });
                }
                if rec.title.is_empty() {
                    rec.title = short_id(&rec.id);
                }
                if !rec.usage.is_empty() {
                    out.push(rec);
                }
            }
        }

        // 没有进 state_*.sqlite 的孤立 rollout jsonl 也扫一遍
        let mut orphan_files = Vec::new();
        collect_jsonl_files(&codex_dir.join("sessions"), 4, &mut orphan_files);
        for path in orphan_files {
            if recorded_paths.contains(&path) {
                continue;
            }
            let Some(rollout) = self.read_codex_rollout(&path) else { continue };
            let id = rollout
                .session_id
                .clone()
                .unwrap_or_else(|| file_stem_str(&path));
            let mut rec = SessionRecord::new(AgentKind::Codex, id);
            rec.cwd = rollout.cwd;
            rec.title = rollout.title.unwrap_or_else(|| short_id(&rec.id));
            let model = rollout.model.as_deref().unwrap_or("codex");
            for (ts, u) in &rollout.turns {
                rec.record_turn(*ts, model, u);
            }
            if rec.usage.is_empty()
                && let Some(total) = rollout.last_total
            {
                rec.turns = 1;
                rec.set_summary_usage(model, total);
            }
            if !rec.usage.is_empty() {
                out.push(rec);
            }
        }
    }

    fn read_codex_rollout(&mut self, path: &Path) -> Option<CodexRollout> {
        let stamp = file_stamp(path)?;
        if let Some((old, cached)) = self.codex_rollouts.get(path)
            && *old == stamp
        {
            return Some(cached.clone());
        }
        let parsed = parse_codex_rollout(path)?;
        self.codex_rollouts.insert(path.to_path_buf(), (stamp, parsed.clone()));
        Some(parsed)
    }

    fn scan_grok(&mut self, home: &Path, out: &mut Vec<SessionRecord>) {
        let sessions_dir = home.join(".grok").join("sessions");
        let Ok(cwds) = std::fs::read_dir(&sessions_dir) else { return };
        let mut seen = Vec::new();
        for cwd_entry in cwds.filter_map(Result::ok) {
            let cwd_path = cwd_entry.path();
            if !cwd_path.is_dir() {
                continue;
            }
            let Ok(sessions) = std::fs::read_dir(&cwd_path) else { continue };
            for s_entry in sessions.filter_map(Result::ok) {
                let s_dir = s_entry.path();
                if !s_dir.is_dir() {
                    continue;
                }
                let stamps = grok_dir_stamps(&s_dir);
                if stamps.is_empty() {
                    continue;
                }
                seen.push(s_dir.clone());
                if let Some((old, cached)) = self.grok.get(&s_dir)
                    && *old == stamps
                {
                    if let Some(r) = cached {
                        out.push(r.clone());
                    }
                    continue;
                }
                let parsed = parse_grok_session(&s_dir);
                if let Some(r) = &parsed {
                    out.push(r.clone());
                }
                self.grok.insert(s_dir, (stamps, parsed));
            }
        }
        self.grok.retain(|k, _| seen.contains(k));
    }
}

// ---- Claude Code ----

/// 主会话文件 + 同名目录下的 `subagents/*.jsonl` 的全部时间戳
fn claude_bundle_stamps(main_jsonl: &Path) -> Vec<FileStamp> {
    let mut stamps = Vec::new();
    let Some(s) = file_stamp(main_jsonl) else { return stamps };
    stamps.push(s);
    for sub in claude_subagent_files(main_jsonl) {
        if let Some(st) = file_stamp(&sub) {
            stamps.push(st);
        }
    }
    stamps
}

fn claude_subagent_files(main_jsonl: &Path) -> Vec<PathBuf> {
    let Some(parent) = main_jsonl.parent() else { return Vec::new() };
    let Some(stem) = main_jsonl.file_stem() else { return Vec::new() };
    let sub_dir = parent.join(stem).join("subagents");
    let Ok(rd) = std::fs::read_dir(sub_dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl") && p.is_file())
        .collect();
    files.sort();
    files
}

struct ClaudeMsgUsage {
    ts: i64,
    model: String,
    usage: Usage,
}

pub fn parse_claude_session(main_jsonl: &Path) -> Option<SessionRecord> {
    let id = file_stem_str(main_jsonl);
    let mut rec = SessionRecord::new(AgentKind::Claude, id);
    let mut msg_map: HashMap<String, ClaudeMsgUsage> = HashMap::new();
    let mut msg_order: Vec<String> = Vec::new();
    let mut anon_turns: Vec<ClaudeMsgUsage> = Vec::new();
    let mut ai_title: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut first_user: Option<String> = None;

    scan_claude_jsonl(
        main_jsonl,
        &mut rec,
        &mut msg_map,
        &mut msg_order,
        &mut anon_turns,
        &mut ai_title,
        &mut last_prompt,
        &mut first_user,
    );
    for sub in claude_subagent_files(main_jsonl) {
        scan_claude_jsonl(
            &sub,
            &mut rec,
            &mut msg_map,
            &mut msg_order,
            &mut anon_turns,
            &mut ai_title,
            &mut last_prompt,
            &mut first_user,
        );
    }

    for mid in msg_order {
        if let Some(m) = msg_map.remove(&mid) {
            rec.record_turn(m.ts, &m.model, &m.usage);
        }
    }
    for m in anon_turns {
        rec.record_turn(m.ts, &m.model, &m.usage);
    }

    rec.title = ai_title
        .or(first_user)
        .or(last_prompt)
        .map(|s| clean_title(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(&rec.id));

    if rec.updated_at == 0
        && let Some(st) = file_stamp(main_jsonl)
    {
        rec.updated_at = (st.mtime_ns / 1_000_000_000) as i64;
        if rec.created_at == 0 {
            rec.created_at = rec.updated_at;
        }
    }

    (!rec.usage.is_empty()).then_some(rec)
}

#[allow(clippy::too_many_arguments)]
fn scan_claude_jsonl(
    path: &Path,
    rec: &mut SessionRecord,
    msg_map: &mut HashMap<String, ClaudeMsgUsage>,
    msg_order: &mut Vec<String>,
    anon_turns: &mut Vec<ClaudeMsgUsage>,
    ai_title: &mut Option<String>,
    last_prompt: &mut Option<String>,
    first_user: &mut Option<String>,
) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        // 快速跳过与用量、标题、工作目录无关的大行（如 tool_result 文件内容）
        let has_interest = line.contains("\"usage\"")
            || line.contains("\"ai-title\"")
            || line.contains("\"custom-title\"")
            || line.contains("\"last-prompt\"")
            || (rec.cwd.is_none() && line.contains("\"cwd\""))
            || (first_user.is_none() && line.contains("\"user\""));
        if !has_interest {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if rec.cwd.is_none()
            && let Some(cwd) = v.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            rec.cwd = Some(cwd.to_string());
        }
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "ai-title" | "custom-title" => {
                if let Some(t) = v
                    .get("aiTitle")
                    .or_else(|| v.get("customTitle"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    *ai_title = Some(t.to_string());
                }
            }
            "last-prompt" => {
                if let Some(p) = v.get("lastPrompt").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    *last_prompt = Some(p.to_string());
                }
            }
            "user" if first_user.is_none() && v.get("isMeta").and_then(Value::as_bool) != Some(true) => {
                if let Some(text) = extract_claude_user_text(&v) {
                    *first_user = Some(text);
                }
            }
            "assistant" => {
                let Some(msg) = v.get("message") else { continue };
                let Some(u) = msg.get("usage") else { continue };
                let usage = parse_claude_usage(u);
                if usage.is_empty() {
                    continue;
                }
                let ts = v
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339_secs)
                    .unwrap_or(0);
                let model = msg
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("claude")
                    .to_string();
                let item = ClaudeMsgUsage { ts, model, usage };
                if let Some(mid) = msg.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    if !msg_map.contains_key(mid) {
                        msg_order.push(mid.to_string());
                    }
                    msg_map.insert(mid.to_string(), item);
                } else {
                    anon_turns.push(item);
                }
            }
            _ => {}
        }
    }
}

fn parse_claude_usage(u: &Value) -> Usage {
    let input = u64_field(u, "input_tokens");
    let cache_read = u64_field(u, "cache_read_input_tokens");
    let cache_write = u64_field(u, "cache_creation_input_tokens");
    let output = u64_field(u, "output_tokens");
    let reasoning = u
        .get("output_tokens_details")
        .map(|d| u64_field(d, "thinking_tokens"))
        .unwrap_or(0);
    Usage { input, cache_read, cache_write, output, reasoning }
}

fn extract_claude_user_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let raw = match content {
        Value::String(s) => s.as_str(),
        Value::Array(arr) => arr
            .iter()
            .find_map(|item| item.get("text").and_then(Value::as_str))?,
        _ => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('<') {
        return None;
    }
    Some(trimmed.to_string())
}

// ---- Codex ----

#[derive(Clone, Default)]
struct CodexRollout {
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    title: Option<String>,
    turns: Vec<(i64, Usage)>,
    last_total: Option<Usage>,
}

fn parse_codex_rollout(path: &Path) -> Option<CodexRollout> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut out = CodexRollout::default();
    for line in reader.lines().map_while(Result::ok) {
        let has_interest = line.contains("\"token_usage_record\"")
            || line.contains("\"token_count\"")
            || line.contains("\"session_meta\"")
            || (out.model.is_none() && line.contains("\"model\""))
            || (out.title.is_none() && line.contains("\"UserMessage\""));
        if !has_interest {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_secs)
            .unwrap_or(0);
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = v.get("payload").unwrap_or(&Value::Null);
        match ty {
            "session_meta" => {
                if out.session_id.is_none() {
                    out.session_id = payload.get("id").or_else(|| payload.get("session_id")).and_then(Value::as_str).map(str::to_string);
                }
                if out.cwd.is_none() {
                    out.cwd = payload.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
                }
            }
            "token_usage_record" => {
                if let Some(u) = payload.get("usage").map(parse_codex_usage)
                    && !u.is_empty()
                {
                    out.turns.push((ts, u));
                }
            }
            "event_msg" => match payload.get("type").and_then(Value::as_str) {
                Some("token_count") => {
                    if let Some(total) = payload.get("info").and_then(|i| i.get("total_token_usage")).map(parse_codex_usage)
                        && !total.is_empty()
                    {
                        out.last_total = Some(total);
                    }
                }
                Some("item_completed") if out.title.is_none() => {
                    if let Some(item) = payload.get("item")
                        && item.get("type").and_then(Value::as_str) == Some("UserMessage")
                        && let Some(content) = item.get("content").and_then(Value::as_array)
                        && let Some(text) = content.iter().find_map(|c| c.get("text").and_then(Value::as_str))
                    {
                        let cleaned = clean_title(text);
                        if !cleaned.is_empty() {
                            out.title = Some(cleaned);
                        }
                    }
                }
                _ => {}
            },
            "turn_context" | "world_state" if out.model.is_none() => {
                if let Some(m) = payload
                    .get("model")
                    .or_else(|| payload.get("collaboration_mode").and_then(|c| c.get("model")))
                    .or_else(|| payload.get("state").and_then(|s| s.get("collaboration_mode")).and_then(|c| c.get("model")))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    out.model = Some(m.to_string());
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Codex 的 `input_tokens` 已经包含了 `cached_input_tokens`，这里拆出非缓存部分
fn parse_codex_usage(u: &Value) -> Usage {
    let raw_input = u64_field(u, "input_tokens");
    let cache_read = u64_field(u, "cached_input_tokens");
    let cache_write = u64_field(u, "cache_write_input_tokens");
    let output = u64_field(u, "output_tokens");
    let reasoning = u64_field(u, "reasoning_output_tokens");
    Usage {
        input: raw_input.saturating_sub(cache_read),
        cache_read,
        cache_write,
        output,
        reasoning,
    }
}

// ---- Grok CLI ----

fn grok_dir_stamps(dir: &Path) -> Vec<FileStamp> {
    ["summary.json", "updates.jsonl", "signals.json"]
        .into_iter()
        .filter_map(|name| file_stamp(&dir.join(name)))
        .collect()
}

pub fn parse_grok_session(dir: &Path) -> Option<SessionRecord> {
    let summary_path = dir.join("summary.json");
    let summary: Value = std::fs::read_to_string(&summary_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);

    let id = summary
        .get("info")
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| file_stem_str(dir));

    let mut rec = SessionRecord::new(AgentKind::Grok, id);
    rec.cwd = summary
        .get("info")
        .and_then(|i| i.get("cwd"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    rec.created_at = summary
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_secs)
        .unwrap_or(0);
    rec.updated_at = summary
        .get("updated_at")
        .or_else(|| summary.get("last_active_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_secs)
        .unwrap_or(rec.created_at);

    let default_model = summary
        .get("current_model_id")
        .and_then(Value::as_str)
        .unwrap_or("grok");

    rec.title = summary
        .get("generated_title")
        .or_else(|| summary.get("session_summary"))
        .or_else(|| summary.get("last_turn_summary"))
        .and_then(Value::as_str)
        .map(clean_title)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(&rec.id));

    let mut cost_ticks: u64 = 0;
    let updates_path = dir.join("updates.jsonl");
    if let Ok(file) = std::fs::File::open(&updates_path) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"turn_completed\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let ts = v
                .get("timestamp")
                .and_then(Value::as_i64)
                .map(normalize_ts)
                .unwrap_or(rec.updated_at);
            let Some(update) = v.get("params").and_then(|p| p.get("update")) else { continue };
            if update.get("sessionUpdate").and_then(Value::as_str) != Some("turn_completed") {
                continue;
            }
            let Some(usage) = update.get("usage") else { continue };
            cost_ticks = cost_ticks.saturating_add(u64_field(usage, "costUsdTicks"));
            if let Some(Value::Object(models)) = usage.get("modelUsage")
                && !models.is_empty()
            {
                for (m_name, m_usage) in models {
                    let u = parse_grok_usage(m_usage);
                    rec.record_turn(ts, m_name, &u);
                }
            } else {
                let u = parse_grok_usage(usage);
                rec.record_turn(ts, default_model, &u);
            }
        }
    }

    // 早期版本或未写入 turn_completed 时，用 signals.json 兜底
    if rec.usage.is_empty() {
        let signals_path = dir.join("signals.json");
        if let Ok(text) = std::fs::read_to_string(&signals_path)
            && let Ok(sig) = serde_json::from_str::<Value>(&text)
        {
            let ctx = u64_field(&sig, "contextTokensUsed").saturating_add(u64_field(&sig, "totalTokensBeforeCompaction"));
            if ctx > 0 {
                rec.turns = (u64_field(&sig, "turnCount") as u32).max(1);
                let model = sig
                    .get("primaryModelId")
                    .and_then(Value::as_str)
                    .unwrap_or(default_model);
                rec.set_summary_usage(model, Usage { input: ctx, ..Default::default() });
            }
        }
    }

    if cost_ticks > 0 {
        // Grok 的 costUsdTicks 是 1e-10 美元（10_000_000_000 ticks = $1）
        rec.cost_usd = Some(cost_ticks as f64 / 10_000_000_000.0);
    }

    (!rec.usage.is_empty()).then_some(rec)
}

/// Grok 的 `inputTokens` 包含了 `cachedReadTokens`
fn parse_grok_usage(u: &Value) -> Usage {
    let raw_input = u64_field(u, "inputTokens");
    let cache_read = u64_field(u, "cachedReadTokens");
    let cache_write = u64_field(u, "cacheCreationTokens");
    let output = u64_field(u, "outputTokens");
    let reasoning = u64_field(u, "reasoningTokens");
    Usage {
        input: raw_input.saturating_sub(cache_read),
        cache_read,
        cache_write,
        output,
        reasoning,
    }
}

// ---- Devin CLI ----

fn scan_devin(home: &Path, out: &mut Vec<SessionRecord>) {
    let base = home.join(".local").join("share").join("devin");
    let mut seen_ids = Vec::new();
    for sub in ["cli", "cli-next"] {
        let dir = base.join(sub);
        let db_path = dir.join("sessions.db");
        if let Some(conn) = open_readonly(&db_path) {
            scan_devin_db(&conn, &mut seen_ids, out);
        }
        // transcripts/*.json 兜底（ATIF 格式）
        let transcripts_dir = dir.join("transcripts");
        if let Ok(rd) = std::fs::read_dir(&transcripts_dir) {
            for entry in rd.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(rec) = parse_devin_transcript(&path)
                    && !seen_ids.contains(&rec.id)
                {
                    seen_ids.push(rec.id.clone());
                    out.push(rec);
                }
            }
        }
    }
}

fn scan_devin_db(conn: &Connection, seen_ids: &mut Vec<String>, out: &mut Vec<SessionRecord>) {
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, working_directory, model, created_at, last_activity_at, COALESCE(title, '') FROM sessions",
    ) else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1).ok().filter(|s| !s.is_empty()),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, i64>(4).unwrap_or(0),
            row.get::<_, String>(5).unwrap_or_default(),
        ))
    }) else {
        return;
    };

    for row in rows.filter_map(Result::ok) {
        let (id, cwd, session_model, created_at, last_activity, title) = row;
        if seen_ids.contains(&id) {
            continue;
        }
        let mut rec = SessionRecord::new(AgentKind::Devin, id.clone());
        rec.cwd = cwd;
        rec.created_at = normalize_ts(created_at);
        rec.updated_at = normalize_ts(last_activity);
        rec.title = clean_title(&title);
        if rec.title.is_empty() || rec.title == "Untitled" {
            rec.title = short_id(&rec.id);
        }

        if let Ok(mut node_stmt) = conn.prepare("SELECT chat_message FROM message_nodes WHERE session_id = ?1 ORDER BY node_id")
            && let Ok(nodes) = node_stmt.query_map([&id], |r| r.get::<_, String>(0))
        {
            let mut req_order: Vec<String> = Vec::new();
            let mut req_map: HashMap<String, (i64, String, Usage)> = HashMap::new();
            let mut anon: Vec<(i64, String, Usage)> = Vec::new();
            for msg_str in nodes.filter_map(Result::ok) {
                if !msg_str.contains("\"metrics\"") {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&msg_str) else { continue };
                let Some(meta) = v.get("metadata") else { continue };
                let Some(metrics) = meta.get("metrics") else { continue };
                let u = Usage {
                    input: u64_field(metrics, "input_tokens"),
                    cache_read: u64_field(metrics, "cache_read_tokens"),
                    cache_write: u64_field(metrics, "cache_creation_tokens"),
                    output: u64_field(metrics, "output_tokens"),
                    reasoning: 0,
                };
                if u.is_empty() {
                    continue;
                }
                let ts = meta
                    .get("created_at")
                    .or_else(|| meta.get("started_generation_at"))
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339_secs)
                    .unwrap_or(rec.updated_at);
                let model = meta
                    .get("generation_model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(if session_model.is_empty() { "devin" } else { session_model.as_str() })
                    .to_string();
                if let Some(rid) = meta.get("request_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    if !req_map.contains_key(rid) {
                        req_order.push(rid.to_string());
                    }
                    req_map.insert(rid.to_string(), (ts, model, u));
                } else {
                    anon.push((ts, model, u));
                }
            }
            for rid in req_order {
                if let Some((ts, model, u)) = req_map.remove(&rid) {
                    rec.record_turn(ts, &model, &u);
                }
            }
            for (ts, model, u) in anon {
                rec.record_turn(ts, &model, &u);
            }
        }

        if !rec.usage.is_empty() {
            seen_ids.push(id);
            out.push(rec);
        }
    }
}

fn parse_devin_transcript(path: &Path) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let id = v.get("session_id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| file_stem_str(path));
    let fm = v.get("final_metrics")?;
    let raw_prompt = u64_field(fm, "total_prompt_tokens");
    let cached = u64_field(fm, "total_cached_tokens");
    let completion = u64_field(fm, "total_completion_tokens");
    let u = Usage {
        input: raw_prompt.saturating_sub(cached),
        cache_read: cached,
        cache_write: 0,
        output: completion,
        reasoning: 0,
    };
    if u.is_empty() {
        return None;
    }
    let mut rec = SessionRecord::new(AgentKind::Devin, id);
    rec.title = short_id(&rec.id);
    rec.turns = (u64_field(fm, "total_steps") as u32).max(1);
    if let Some(st) = file_stamp(path) {
        rec.updated_at = (st.mtime_ns / 1_000_000_000) as i64;
        rec.created_at = rec.updated_at;
    }
    let model = v
        .get("agent")
        .and_then(|a| a.get("model_name"))
        .and_then(Value::as_str)
        .unwrap_or("devin");
    rec.set_summary_usage(model, u);
    Some(rec)
}

// ---- OpenCode ----

fn scan_opencode(home: &Path, out: &mut Vec<SessionRecord>) {
    let db_path = home.join(".local").join("share").join("opencode").join("opencode.db");
    let Some(conn) = open_readonly(&db_path) else { return };
    let mut seen_ids = Vec::new();
    for table in ["session_v2", "session"] {
        let sql = format!(
            "SELECT id, directory, COALESCE(title, ''), COALESCE(model, ''), \
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, \
             cost, time_created, time_updated FROM {table}"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1).ok().filter(|s| !s.is_empty()),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, i64>(4).unwrap_or(0).max(0) as u64,
                row.get::<_, i64>(5).unwrap_or(0).max(0) as u64,
                row.get::<_, i64>(6).unwrap_or(0).max(0) as u64,
                row.get::<_, i64>(7).unwrap_or(0).max(0) as u64,
                row.get::<_, i64>(8).unwrap_or(0).max(0) as u64,
                row.get::<_, f64>(9).unwrap_or(0.0),
                row.get::<_, i64>(10).unwrap_or(0),
                row.get::<_, i64>(11).unwrap_or(0),
            ))
        }) else {
            continue;
        };

        for row in rows.filter_map(Result::ok) {
            let (id, cwd, title, raw_model, t_in, t_out, t_reas, t_cr, t_cw, cost, created, updated) = row;
            if seen_ids.contains(&id) {
                continue;
            }
            let u = Usage {
                input: t_in,
                cache_read: t_cr,
                cache_write: t_cw,
                output: t_out,
                reasoning: t_reas,
            };
            if u.is_empty() {
                continue;
            }
            seen_ids.push(id.clone());
            let mut rec = SessionRecord::new(AgentKind::OpenCode, id);
            rec.cwd = cwd;
            rec.created_at = normalize_ts(created);
            rec.updated_at = normalize_ts(updated);
            rec.title = clean_title(&title);
            if rec.title.is_empty() {
                rec.title = short_id(&rec.id);
            }
            rec.turns = 1;
            if cost > 0.0 {
                rec.cost_usd = Some(cost);
            }
            let model = parse_opencode_model(&raw_model);
            rec.set_summary_usage(&model, u);
            out.push(rec);
        }
    }
}

fn parse_opencode_model(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(trimmed)
        && let Some(id) = v.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
    {
        return id.to_string();
    }
    if trimmed.is_empty() { "opencode".into() } else { trimmed.to_string() }
}

// ---- 辅助函数 ----

fn open_readonly(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(500));
    Some(conn)
}

fn latest_matching_file(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut candidates: Vec<(u128, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with(prefix) && name.ends_with(suffix) {
                let p = e.path();
                let st = file_stamp(&p)?;
                Some((st.mtime_ns, p))
            } else {
                None
            }
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, p)| p)
}

fn collect_jsonl_files(dir: &Path, max_depth: usize, out: &mut Vec<PathBuf>) {
    if max_depth == 0 || !dir.is_dir() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            collect_jsonl_files(&p, max_depth - 1, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key)
        .and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f.max(0.0) as u64)))
        .unwrap_or(0)
}

/// 秒或毫秒时间戳统一归到秒
fn normalize_ts(ts: i64) -> i64 {
    if ts > 10_000_000_000 { ts / 1000 } else { ts.max(0) }
}

fn file_stem_str(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// 清理模型名：去掉 `<synthetic>` 之类占位词，保留有意义的名字
pub fn clean_model_name(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() || s.starts_with('<') {
        return String::new();
    }
    s.to_string()
}

/// 清理会话标题：Codex 等常把附件列表贴在最前面、真正的问题写在 `## My request:` 后面
pub fn clean_title(raw: &str) -> String {
    let body = if let Some((_, after)) = raw.rsplit_once("## My request:") {
        after
    } else {
        raw
    };
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('<'))
        .unwrap_or_else(|| body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""));
    let mut out: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        out.push('…');
    }
    out
}

/// RFC 3339 时间戳（`2026-10-22T16:57:23.396Z`）→ unix 秒。纯函数，不拉 chrono 依赖
pub fn parse_rfc3339_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let mut ts = days * 86400 + hour * 3600 + min * 60 + sec;

    // 跳过小数秒，解析尾部时区偏移（`Z` 或 `+08:00` / `-05:00`）
    let tail = &s[19..];
    let tz_part = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(rest) = tz_part.strip_prefix('+') {
        ts -= parse_tz_offset(rest)?;
    } else if let Some(rest) = tz_part.strip_prefix('-') {
        ts += parse_tz_offset(rest)?;
    }
    Some(ts)
}

fn parse_tz_offset(s: &str) -> Option<i64> {
    if s.len() < 2 {
        return None;
    }
    let h: i64 = s[0..2].parse().ok()?;
    let m: i64 = if s.len() >= 5 && s.as_bytes()[2] == b':' {
        s[3..5].parse().ok()?
    } else if s.len() >= 4 {
        s[2..4].parse().ok()?
    } else {
        0
    };
    Some(h * 3600 + m * 60)
}

/// Howard Hinnant 的公历转 epoch 天数算法（1970-01-01 = 0）
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// epoch 天数（`epoch_secs / 86400`）→ `(年, 月, 日)`
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_and_civil_dates_round_trip() {
        // 2026-10-22T16:57:23.396Z
        let ts = parse_rfc3339_secs("2026-10-22T16:57:23.396Z").unwrap();
        assert_eq!(civil_from_days(ts / 86400), (2026, 10, 22));
        // 带 +08:00 偏移：2026-10-20T08:00:00.5+08:00 == 2026-10-20T00:00:00Z
        let ts_tz = parse_rfc3339_secs("2026-10-20T08:00:00.5+08:00").unwrap();
        let ts_utc = parse_rfc3339_secs("2026-10-20T00:00:00Z").unwrap();
        assert_eq!(ts_tz, ts_utc);
    }

    #[test]
    fn claude_deduplicates_streaming_messages_and_includes_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join(".claude/projects/demo");
        std::fs::create_dir_all(&proj).unwrap();
        let main = proj.join("sess-1.jsonl");
        std::fs::write(
            &main,
            concat!(
                r#"{"type":"user","cwd":"/work/demo","message":{"content":"帮我查一下原因"}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"排查原因"}"#,
                "\n",
                // 同一个 message.id 的流式中间行与最终行：只算最后一条
                r#"{"type":"assistant","timestamp":"2026-10-22T10:00:00Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":100,"cache_read_input_tokens":500,"cache_creation_input_tokens":200,"output_tokens":10}}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-10-22T10:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":100,"cache_read_input_tokens":500,"cache_creation_input_tokens":200,"output_tokens":50,"output_tokens_details":{"thinking_tokens":20}}}}"#,
                "\n",
            ),
        )
        .unwrap();
        let sub_dir = proj.join("sess-1/subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(
            sub_dir.join("agent-1.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-10-22T10:01:00Z","message":{"id":"m2","model":"claude-haiku-5-5","usage":{"input_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":30}}}"#,
        )
        .unwrap();

        let rec = parse_claude_session(&main).unwrap();
        assert_eq!(rec.title, "排查原因");
        assert_eq!(rec.cwd.as_deref(), Some("/work/demo"));
        assert_eq!(rec.turns, 2);
        assert_eq!(
            rec.usage,
            Usage { input: 150, cache_read: 500, cache_write: 200, output: 80, reasoning: 20 }
        );
        assert_eq!(rec.usage.total(), 930);
        assert_eq!(rec.models.len(), 2);
    }

    #[test]
    fn grok_accumulates_turn_completed_and_normalizes_cached_input() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join("sess");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            sdir.join("summary.json"),
            r#"{"info":{"id":"g-1","cwd":"/work/demo"},"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T01:00:00Z","current_model_id":"grok-4.7","generated_title":"Grok 测试"}"#,
        )
        .unwrap();
        std::fs::write(
            sdir.join("updates.jsonl"),
            r#"{"timestamp":1792454400,"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":1000,"outputTokens":200,"totalTokens":1200,"cachedReadTokens":800,"cacheCreationTokens":0,"reasoningTokens":50,"costUsdTicks":500000000,"modelUsage":{"grok-4.7-build":{"inputTokens":1000,"outputTokens":200,"totalTokens":1200,"cachedReadTokens":800,"cacheCreationTokens":0,"reasoningTokens":50}}}}}}"#,
        )
        .unwrap();

        let rec = parse_grok_session(&sdir).unwrap();
        assert_eq!(rec.title, "Grok 测试");
        assert_eq!(rec.turns, 1);
        // inputTokens(1000) - cachedReadTokens(800) = 200
        assert_eq!(
            rec.usage,
            Usage { input: 200, cache_read: 800, cache_write: 0, output: 200, reasoning: 50 }
        );
        assert_eq!(rec.usage.total(), 1200);
        assert!((rec.cost_usd.unwrap() - 0.05).abs() < 1e-6);
    }

    #[test]
    fn scanner_reads_codex_devin_and_opencode() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // 1. Codex: state_5.sqlite + rollout.jsonl
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let rollout_path = codex_dir.join("rollout-1.jsonl");
        std::fs::write(
            &rollout_path,
            concat!(
                r#"{"timestamp":"2026-10-21T12:00:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-10-21T12:00:10Z","type":"token_usage_record","payload":{"usage":{"input_tokens":1500,"cached_input_tokens":1000,"cache_write_input_tokens":0,"output_tokens":300,"reasoning_output_tokens":100,"total_tokens":1800}}}"#,
                "\n",
            ),
        )
        .unwrap();
        {
            let conn = Connection::open(codex_dir.join("state_5.sqlite")).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (id TEXT, rollout_path TEXT, created_at INTEGER, updated_at INTEGER, cwd TEXT, title TEXT, tokens_used INTEGER, model TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO threads VALUES ('cx-1', ?1, 1792584000, 1792584010, '/work/demo', '## My request:\n修复构建报错', 1800, '')",
                [rollout_path.to_string_lossy().as_ref()],
            )
            .unwrap();
        }

        // 2. Devin: ~/.local/share/devin/cli/sessions.db
        let devin_dir = home.join(".local/share/devin/cli");
        std::fs::create_dir_all(&devin_dir).unwrap();
        {
            let conn = Connection::open(devin_dir.join("sessions.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (id TEXT, working_directory TEXT, model TEXT, created_at INTEGER, last_activity_at INTEGER, title TEXT);
                 CREATE TABLE message_nodes (node_id TEXT, session_id TEXT, chat_message TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('dv-1', '/work/demo', 'claude-opus-4-6', 1792584000, 1792584020, 'Devin 任務')",
                [],
            )
            .unwrap();
            // 同一 request_id 插两条节点，验证按 request_id 去重
            let msg = r#"{"role":"assistant","metadata":{"request_id":"req-1","generation_model":"claude-opus-4-6","created_at":"2026-10-21T12:00:20Z","metrics":{"input_tokens":400,"output_tokens":200,"cache_read_tokens":600,"cache_creation_tokens":100}}}"#;
            conn.execute("INSERT INTO message_nodes VALUES ('n1', 'dv-1', ?1)", [msg]).unwrap();
            conn.execute("INSERT INTO message_nodes VALUES ('n2', 'dv-1', ?1)", [msg]).unwrap();
        }

        // 3. OpenCode: ~/.local/share/opencode/opencode.db
        let oc_dir = home.join(".local/share/opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        {
            let conn = Connection::open(oc_dir.join("opencode.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_v2 (id TEXT, directory TEXT, title TEXT, model TEXT, tokens_input INTEGER, tokens_output INTEGER, tokens_reasoning INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER, cost REAL, time_created INTEGER, time_updated INTEGER);",
            )
            .unwrap();
            conn.execute(
                r#"INSERT INTO session_v2 VALUES ('oc-1', '/work/demo', 'OpenCode 会话', '{"id":"kimi-k2.5"}', 800, 250, 50, 1200, 0, 0.02, 1792584000000, 1792584030000)"#,
                [],
            )
            .unwrap();
        }

        let mut scanner = Scanner::default();
        let records = scanner.scan(home);
        assert_eq!(records.len(), 3);

        let cx = records.iter().find(|r| r.agent == AgentKind::Codex).unwrap();
        assert_eq!(cx.title, "修复构建报错");
        assert_eq!(cx.usage, Usage { input: 500, cache_read: 1000, cache_write: 0, output: 300, reasoning: 100 });
        assert_eq!(cx.usage.total(), 1800);

        let dv = records.iter().find(|r| r.agent == AgentKind::Devin).unwrap();
        assert_eq!(dv.turns, 1);
        assert_eq!(dv.usage.total(), 1300);

        let oc = records.iter().find(|r| r.agent == AgentKind::OpenCode).unwrap();
        assert_eq!(oc.usage.total(), 2250);
        assert!(oc.models.contains_key("kimi-k2.5"));
    }
}
