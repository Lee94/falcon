//! Claude 对话窗口的记录模型：把 `/ws/projects/:id/claude` 推来的事件流整理成一轮一轮的
//! 「用户消息 → 过程（文字与工具步骤）→ 最终回复」，界面（falcon-ui 的 `panels/claude_chat.rs`）
//! 只管画。
//!
//! 一轮的边界是宿主机的 `User` 回显与 `Done`。回放历史与实时事件走同一条路：历史里也存着工具
//! 结果与耗时，接回的对话和亲眼看着跑完的长得一样。旧历史只有工具名（没有 id、没有结果），
//! 照样画成步骤卡片，等这一轮的 `Done` 一起收尾。

use falcon_proto::claude::{ClaudeEvent, ClaudeModel, ClaudeOptions, ClaudeToolCall};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
    /// 这一轮结束（出错、被打断、连接断开）时还没等到结果
    Interrupted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolStep {
    pub call: ClaudeToolCall,
    pub status: ToolStatus,
    pub output: String,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Text(String),
    Tool(ToolStep),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turn {
    /// 没有提示词的一轮：宿主机先推了回复（不该发生，但别丢内容）
    pub prompt: Option<String>,
    /// 提示词带的附件（只有文件名）
    pub attachments: Vec<String>,
    pub steps: Vec<Step>,
    /// 还在流式输出、没等到完整消息的文字
    pub draft: String,
    pub done: bool,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

impl Turn {
    /// 结束了的一轮里算「最终回复」的那段文字：最后一步是文字才算，以工具收尾的一轮没有回复
    pub fn reply_index(&self) -> Option<usize> {
        (self.done && matches!(self.steps.last(), Some(Step::Text(_)))).then(|| self.steps.len() - 1)
    }

    /// 可以折叠起来的过程（最终回复之前的全部步骤）
    pub fn process(&self) -> &[Step] {
        &self.steps[..self.reply_index().unwrap_or(self.steps.len())]
    }

    pub fn tool_count(&self) -> usize {
        self.steps.iter().filter(|s| matches!(s, Step::Tool(_))).count()
    }

    fn finish_tools(&mut self, status: ToolStatus) {
        for step in &mut self.steps {
            if let Step::Tool(tool) = step
                && tool.status == ToolStatus::Running
            {
                tool.status = status;
            }
        }
    }

    fn flush_draft(&mut self) {
        if !self.draft.is_empty() {
            self.steps.push(Step::Text(std::mem::take(&mut self.draft)));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Turn(Turn),
    /// 连接断开之类不属于哪一轮的提示
    Notice(String),
}

#[derive(Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 还没结束的那一轮
    pub fn open_turn(&self) -> Option<&Turn> {
        match self.entries.last() {
            Some(Entry::Turn(turn)) if !turn.done => Some(turn),
            _ => None,
        }
    }

    fn open_turn_mut(&mut self) -> &mut Turn {
        if self.open_turn().is_none() {
            self.entries.push(Entry::Turn(Turn::default()));
        }
        match self.entries.last_mut() {
            Some(Entry::Turn(turn)) => turn,
            _ => unreachable!(),
        }
    }

    /// 收一个事件。返回值：记录变了没有（Ready / 权限这类不进记录的返回 false）
    pub fn apply(&mut self, event: &ClaudeEvent) -> bool {
        match event {
            ClaudeEvent::User { text, attachments } => {
                // 上一轮没收尾就来了新提示词（旧历史缺 Done）：当它结束了
                if let Some(Entry::Turn(turn)) = self.entries.last_mut()
                    && !turn.done
                {
                    turn.flush_draft();
                    turn.finish_tools(ToolStatus::Interrupted);
                    turn.done = true;
                }
                self.entries.push(Entry::Turn(Turn {
                    prompt: Some(text.clone()),
                    attachments: attachments.clone(),
                    ..Turn::default()
                }));
            }
            ClaudeEvent::Delta { text } => self.open_turn_mut().draft.push_str(text),
            ClaudeEvent::Assistant { text, tools, calls } => {
                let turn = self.open_turn_mut();
                turn.draft.clear();
                if !text.is_empty() {
                    turn.steps.push(Step::Text(text.clone()));
                }
                let calls = if calls.is_empty() {
                    // 旧历史：只有工具名
                    tools
                        .iter()
                        .map(|name| ClaudeToolCall { id: String::new(), name: name.clone(), title: String::new(), detail: None })
                        .collect()
                } else {
                    calls.clone()
                };
                turn.steps.extend(calls.into_iter().map(|call| {
                    Step::Tool(ToolStep { call, status: ToolStatus::Running, output: String::new(), elapsed_ms: None })
                }));
            }
            ClaudeEvent::ToolResult { id, error, output, elapsed_ms } => {
                // 结果只会落在最近一轮：一轮结束时宿主机清掉了在途的调用
                let Some(Entry::Turn(turn)) = self.entries.last_mut() else { return false };
                let Some(tool) = turn.steps.iter_mut().rev().find_map(|step| match step {
                    Step::Tool(tool) if !id.is_empty() && tool.call.id == *id => Some(tool),
                    _ => None,
                }) else {
                    return false;
                };
                tool.status = if *error { ToolStatus::Failed } else { ToolStatus::Ok };
                tool.output = output.clone();
                tool.elapsed_ms = *elapsed_ms;
            }
            ClaudeEvent::Done { error, duration_ms } => {
                let turn = self.open_turn_mut();
                turn.flush_draft();
                turn.finish_tools(if error.is_some() { ToolStatus::Interrupted } else { ToolStatus::Ok });
                turn.done = true;
                turn.error = error.clone();
                turn.duration_ms = *duration_ms;
            }
            ClaudeEvent::Closed { reason } => {
                if let Some(Entry::Turn(turn)) = self.entries.last_mut()
                    && !turn.done
                {
                    turn.flush_draft();
                    turn.finish_tools(ToolStatus::Interrupted);
                    turn.done = true;
                }
                self.entries.push(Entry::Notice(reason.clone()));
            }
            ClaudeEvent::ThreadHistory
            | ClaudeEvent::Ready
            | ClaudeEvent::Permission { .. }
            | ClaudeEvent::CancelPermission { .. }
            | ClaudeEvent::Options(_)
            | ClaudeEvent::Context { .. }
            | ClaudeEvent::Warning { .. } => return false,
        }
        true
    }
}

/// 图片附件上限：Claude 的 API 单张图片不收超过 5 MB 的
pub const ATTACHMENT_IMAGE_MAX: usize = 5 * 1024 * 1024;
pub const ATTACHMENT_PDF_MAX: usize = 20 * 1024 * 1024;
/// 文本附件整份塞进提示词，与宿主机 `api/claude.rs` 的上限一致
pub const ATTACHMENT_TEXT_MAX: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentReject {
    /// 超过这一类的上限（字节）
    TooLarge(usize),
    /// 不是图片、PDF，也不是 UTF-8 文本
    Unsupported,
}

/// 附件交给宿主机的 media type（`ClaudeAttachment::media_type`）：图片与 PDF 看扩展名，
/// 其余能按 UTF-8 读的当文本（源码、日志、配置……扩展名列不全）
pub fn attachment_media_type(name: &str, bytes: &[u8]) -> Result<&'static str, AttachmentReject> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let (media, max) = match ext.as_str() {
        "png" => ("image/png", ATTACHMENT_IMAGE_MAX),
        "jpg" | "jpeg" => ("image/jpeg", ATTACHMENT_IMAGE_MAX),
        "gif" => ("image/gif", ATTACHMENT_IMAGE_MAX),
        "webp" => ("image/webp", ATTACHMENT_IMAGE_MAX),
        "pdf" => ("application/pdf", ATTACHMENT_PDF_MAX),
        // NUL 基本只出现在二进制里；UTF-8 合法的二进制（全 ASCII 的数据文件）当文本也无妨
        _ if std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0) => ("text/plain", ATTACHMENT_TEXT_MAX),
        _ => return Err(AttachmentReject::Unsupported),
    };
    if bytes.len() > max { Err(AttachmentReject::TooLarge(max)) } else { Ok(media) }
}

/// 正在用的模型：选过的按 value 找；没选过（或选的是 `default` 这种别名）按 Claude Code 报的
/// 实际模型名找那一项（跳过 `default` 本身，它的显示名是「Default (recommended)」，说明不了是谁）
pub fn current_model(options: &ClaudeOptions) -> Option<&ClaudeModel> {
    let chosen = options.model.as_deref().and_then(|v| options.models.iter().find(|m| m.value == v));
    // 先找 value 就是这个模型的那一项，再找解析到它的别名：`opus` / `sonnet` 这类别名指向自定义
    // 模型时，显示名就是原始 id（实测），排在前面会把好好的显示名盖掉
    let concrete = |name: &str| {
        let name = name.trim_end_matches("[1m]");
        let models = || options.models.iter().filter(|m| m.value != "default");
        models().find(|m| m.value.trim_end_matches("[1m]") == name).or_else(|| {
            models().find(|m| m.resolved_model.as_deref().is_some_and(|r| r.trim_end_matches("[1m]") == name))
        })
    };
    match chosen {
        Some(m) if m.value != "default" => Some(m),
        _ => options
            .resolved_model
            .as_deref()
            .and_then(concrete)
            .or(chosen)
            .or_else(|| options.models.iter().find(|m| m.value == "default")),
    }
}

/// 一个模型在菜单与按钮上的名字。别名指向自定义模型时 Claude Code 报的显示名就是原始 id，
/// 几个别名会叫同一个名字分不清，前面带上别名（`opus · <原始 id>`）
pub fn model_name(m: &ClaudeModel) -> String {
    match m.display_name.as_str() {
        "" => m.value.clone(),
        name if m.resolved_model.as_deref() == Some(name) && m.value != name => format!("{} · {name}", m.value),
        name => name.to_owned(),
    }
}

/// 底栏上模型按钮的名字：当前模型的 [`model_name`]，列表里找不到就是实际模型名
pub fn model_label(options: &ClaudeOptions) -> Option<String> {
    current_model(options).map(model_name).or_else(|| options.resolved_model.clone())
}

/// 当前模型能选的 effort 档位；空 = 不支持调
pub fn effort_levels(options: &ClaudeOptions) -> &[String] {
    current_model(options).map(|m| m.effort_levels.as_slice()).unwrap_or_default()
}

/// 上下文用量的百分比（0–100），给圆环用
pub fn context_percent(used: u64, max: u64) -> f32 {
    if max == 0 { 0. } else { (used as f64 / max as f64 * 100.).clamp(0., 100.) as f32 }
}

/// token 数的简写：`950`、`12.3k`、`1M`
pub fn format_tokens(n: u64) -> String {
    let trim = |v: f64, unit: &str| {
        let s = format!("{v:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    };
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => trim(n as f64 / 1_000., "k"),
        _ => trim(n as f64 / 1_000_000., "M"),
    }
}

/// 「工作了 8m 0s」里的时长：不到一分钟 `42s`，不到一小时 `8m 0s`，再长 `1h 5m`。
/// 单位用字母，与语言无关
pub fn format_elapsed(ms: u64) -> String {
    let s = ms / 1000;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) if ms < 1000 => format!("{}.{}s", s, ms / 100 % 10),
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str) -> ClaudeToolCall {
        ClaudeToolCall { id: id.into(), name: name.into(), title: String::new(), detail: None }
    }

    fn turn(t: &Transcript, i: usize) -> &Turn {
        match &t.entries[i] {
            Entry::Turn(turn) => turn,
            other => panic!("expected turn, got {other:?}"),
        }
    }

    #[test]
    fn turn_collects_process_and_reply() {
        let mut t = Transcript::default();
        t.apply(&ClaudeEvent::User { text: "go".into(), attachments: vec![] });
        t.apply(&ClaudeEvent::Delta { text: "看".into() });
        t.apply(&ClaudeEvent::Assistant {
            text: "看一下".into(),
            tools: vec!["Bash".into(), "Read".into()],
            calls: vec![call("a", "Bash"), call("b", "Read")],
        });
        assert!(turn(&t, 0).draft.is_empty());
        t.apply(&ClaudeEvent::ToolResult { id: "a".into(), error: false, output: "ok".into(), elapsed_ms: Some(42) });
        t.apply(&ClaudeEvent::ToolResult { id: "b".into(), error: true, output: "no".into(), elapsed_ms: None });
        t.apply(&ClaudeEvent::Delta { text: "完成".into() });
        assert_eq!(turn(&t, 0).reply_index(), None, "没结束不算回复");
        t.apply(&ClaudeEvent::Done { error: None, duration_ms: Some(480_000) });
        let turn = turn(&t, 0);
        assert_eq!(turn.reply_index(), Some(3));
        assert_eq!(turn.process().len(), 3);
        assert_eq!(turn.tool_count(), 2);
        assert!(matches!(&turn.steps[1], Step::Tool(s) if s.status == ToolStatus::Ok && s.elapsed_ms == Some(42)));
        assert!(matches!(&turn.steps[2], Step::Tool(s) if s.status == ToolStatus::Failed));
        assert!(t.open_turn().is_none());
    }

    #[test]
    fn old_history_and_interruptions() {
        let mut t = Transcript::default();
        t.apply(&ClaudeEvent::User { text: "a".into(), attachments: vec!["x.png".into()] });
        t.apply(&ClaudeEvent::Assistant { text: String::new(), tools: vec!["Read".into()], calls: vec![] });
        t.apply(&ClaudeEvent::Done { error: None, duration_ms: None });
        assert!(matches!(&turn(&t, 0).steps[0], Step::Tool(s) if s.status == ToolStatus::Ok && s.call.name == "Read"));
        assert_eq!(turn(&t, 0).attachments, ["x.png"]);
        assert_eq!(turn(&t, 0).reply_index(), None, "以工具收尾的一轮没有回复");

        t.apply(&ClaudeEvent::User { text: "b".into(), attachments: vec![] });
        t.apply(&ClaudeEvent::Assistant { text: String::new(), tools: vec![], calls: vec![call("x", "Bash")] });
        t.apply(&ClaudeEvent::Delta { text: "半截".into() });
        t.apply(&ClaudeEvent::Closed { reason: "断了".into() });
        let b = turn(&t, 1);
        assert!(b.done);
        assert!(matches!(&b.steps[0], Step::Tool(s) if s.status == ToolStatus::Interrupted));
        assert!(matches!(&b.steps[1], Step::Text(s) if s == "半截"));
        assert!(matches!(&t.entries[2], Entry::Notice(r) if r == "断了"));
        assert!(!t.apply(&ClaudeEvent::Ready));
    }

    #[test]
    fn model_label_and_effort() {
        let model = |value: &str, name: &str, resolved: &str, efforts: &[&str]| ClaudeModel {
            value: value.into(),
            display_name: name.into(),
            description: String::new(),
            resolved_model: Some(resolved.into()),
            effort_levels: efforts.iter().map(|s| s.to_string()).collect(),
        };
        let mut o = ClaudeOptions {
            models: vec![
                model("default", "Default (recommended)", "ds[1m]", &["low", "xhigh"]),
                // 别名指向自定义模型：显示名是原始 id，不能拿它
                model("opus", "ds[1m]", "ds[1m]", &["low", "xhigh"]),
                model("haiku", "Haiku", "claude-haiku", &[]),
                model("ds[1m]", "DeepSeek", "ds[1m]", &["low", "xhigh"]),
            ],
            resolved_model: Some("ds[1m]".into()),
            ..Default::default()
        };
        // 没选过：按实际模型找具体的那一项，不显示「Default」
        assert_eq!(model_label(&o).as_deref(), Some("DeepSeek"));
        assert_eq!(model_name(&o.models[1]), "opus · ds[1m]");
        assert_eq!(effort_levels(&o), ["low", "xhigh"]);
        o.model = Some("haiku".into());
        assert_eq!(model_label(&o).as_deref(), Some("Haiku"));
        assert!(effort_levels(&o).is_empty());
        // 实际模型不在列表里：退回 default 那一项
        o.model = None;
        o.resolved_model = Some("other".into());
        assert_eq!(model_label(&o).as_deref(), Some("Default (recommended)"));
        o.models.clear();
        assert_eq!(model_label(&o).as_deref(), Some("other"));
    }

    #[test]
    fn attachment_types() {
        assert_eq!(attachment_media_type("a.PNG", &[0x89, 0x50]), Ok("image/png"));
        assert_eq!(attachment_media_type("doc.pdf", b"%PDF"), Ok("application/pdf"));
        assert_eq!(attachment_media_type("main.rs", "fn 主() {}".as_bytes()), Ok("text/plain"));
        assert_eq!(attachment_media_type("Makefile", b"all:"), Ok("text/plain"));
        assert_eq!(attachment_media_type("a.bin", &[0, 1, 2]), Err(AttachmentReject::Unsupported));
        assert_eq!(attachment_media_type("x.zip", &[0xff, 0xfe]), Err(AttachmentReject::Unsupported));
        let big = vec![b'a'; ATTACHMENT_TEXT_MAX + 1];
        assert_eq!(attachment_media_type("big.txt", &big), Err(AttachmentReject::TooLarge(ATTACHMENT_TEXT_MAX)));
    }

    #[test]
    fn token_format() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(10_307), "10.3k");
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(context_percent(250, 1000), 25.);
        assert_eq!(context_percent(1, 0), 0.);
    }

    #[test]
    fn elapsed_format() {
        assert_eq!(format_elapsed(420), "0.4s");
        assert_eq!(format_elapsed(42_000), "42s");
        assert_eq!(format_elapsed(480_000), "8m 0s");
        assert_eq!(format_elapsed(95_000), "1m 35s");
        assert_eq!(format_elapsed(3_900_000), "1h 5m");
    }
}
