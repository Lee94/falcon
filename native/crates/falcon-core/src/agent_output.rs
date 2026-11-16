//! 原生 Agent 视图的纯逻辑：时间线分组与输出预览、命令输出的 ANSI SGR 着色解析，以及文件改动的行级 Diff 计算。

use std::ops::Range;

use falcon_proto::{AgentEntry, AgentNoticeLevel, AgentToolStatus};

/// 时间线保留对话正文，把连续的辅助记录收进可展开的执行过程；需要用户处理的条目始终单独显示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineItem {
    Entry(usize),
    Activity(Range<usize>),
}

pub fn compact_timeline(entries: &[AgentEntry]) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let auxiliary = match entry {
            AgentEntry::Thought { text, .. } => !text.trim().is_empty(),
            AgentEntry::ToolCall(call) => !call.permission_pending
                && !matches!(call.status, AgentToolStatus::Failed | AgentToolStatus::InProgress),
            AgentEntry::Plan { .. } => true,
            AgentEntry::Notice { level, .. } => *level == AgentNoticeLevel::Info,
            _ => false,
        };
        if matches!(entry, AgentEntry::Unknown)
            || matches!(entry, AgentEntry::Message { text, .. } | AgentEntry::Thought { text, .. } if text.trim().is_empty())
        {
            continue;
        }
        if auxiliary {
            if let Some(TimelineItem::Activity(range)) = items.last_mut()
                && range.end == index
            {
                range.end += 1;
            } else {
                items.push(TimelineItem::Activity(index..index + 1));
            }
        } else {
            items.push(TimelineItem::Entry(index));
        }
    }
    items
}

/// 工具详情先展示有限行数与字符数，展开后仍可查看全文。字符边界兼容中文与 emoji。
pub fn text_preview(text: &str, max_lines: usize, max_chars: usize) -> &str {
    let mut lines = 1;
    for (count, (offset, ch)) in text.char_indices().enumerate() {
        if count >= max_chars || (ch == '\n' && lines >= max_lines) {
            return &text[..offset];
        }
        if ch == '\n' {
            lines += 1;
        }
    }
    text
}

/// 终端 8 基色（明暗由调色板定，不硬编码 RGB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub range: Range<usize>,
    pub fg: Option<AnsiColor>,
    pub bold: bool,
}

/// 剥掉控制序列后的纯文本 + 字节区间样式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledOutput {
    pub text: String,
    pub spans: Vec<StyledSpan>,
}

/// 解析命令输出中的 `\r` 覆写与常见 SGR（粗体、暗淡、30–37/90–97 前景色），其余 CSI/OSC 序列剥除。
pub fn styled_output(raw: &str) -> StyledOutput {
    let mut text = String::with_capacity(raw.len());
    let mut spans: Vec<StyledSpan> = Vec::new();
    let mut fg: Option<AnsiColor> = None;
    let mut bold = false;
    let mut span_start = 0usize;
    let mut line_start = 0usize;

    let flush = |spans: &mut Vec<StyledSpan>,
                 start: &mut usize,
                 end: usize,
                 fg: Option<AnsiColor>,
                 bold: bool| {
        if end > *start && (fg.is_some() || bold) {
            spans.push(StyledSpan {
                range: *start..end,
                fg,
                bold,
            });
        }
        *start = end;
    };

    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\x1b' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'[' {
                    i += 1;
                    let param_start = i;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        let cmd = bytes[i];
                        let params = &raw[param_start..i];
                        i += 1;
                        if cmd == b'm' {
                            flush(&mut spans, &mut span_start, text.len(), fg, bold);
                            apply_sgr(params, &mut fg, &mut bold);
                        }
                    }
                } else if i < bytes.len() && bytes[i] == b']' {
                    while i < bytes.len() && bytes[i] != b'\x07' {
                        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                } else {
                    text.truncate(line_start);
                    spans.retain(|s| s.range.start < line_start);
                    if let Some(last) = spans.last_mut() {
                        last.range.end = last.range.end.min(line_start);
                    }
                    span_start = line_start;
                    i += 1;
                }
            }
            b'\n' => {
                text.push('\n');
                line_start = text.len();
                i += 1;
            }
            c if c < 0x20 && c != b'\t' => {
                i += 1;
            }
            _ => {
                let ch = raw[i..].chars().next().unwrap();
                text.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    flush(&mut spans, &mut span_start, text.len(), fg, bold);
    StyledOutput { text, spans }
}

fn apply_sgr(params: &str, fg: &mut Option<AnsiColor>, bold: &mut bool) {
    if params.is_empty() {
        *fg = None;
        *bold = false;
        return;
    }
    for p in params.split(';') {
        match p.parse::<u16>().unwrap_or(0) {
            0 => {
                *fg = None;
                *bold = false;
            }
            1 => *bold = true,
            2 => *fg = Some(AnsiColor::Dim),
            22 => {
                *bold = false;
                if *fg == Some(AnsiColor::Dim) {
                    *fg = None;
                }
            }
            31 | 91 => *fg = Some(AnsiColor::Red),
            32 | 92 => *fg = Some(AnsiColor::Green),
            33 | 93 => *fg = Some(AnsiColor::Yellow),
            34 | 94 => *fg = Some(AnsiColor::Blue),
            35 | 95 => *fg = Some(AnsiColor::Magenta),
            36 | 96 => *fg = Some(AnsiColor::Cyan),
            30 | 90 => *fg = Some(AnsiColor::Dim),
            39 => *fg = None,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// 行级 LCS 对比（超过 400 行时退化为整块删加，避免长文件阻塞界面）。
pub fn line_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old_text.lines().collect();
    let b: Vec<&str> = new_text.lines().collect();
    let (n, m) = (a.len(), b.len());

    if n + m > 400 {
        let mut out = Vec::with_capacity(n + m);
        for l in a {
            out.push(DiffLine {
                kind: DiffKind::Delete,
                text: l.to_string(),
            });
        }
        for l in b {
            out.push(DiffLine {
                kind: DiffKind::Insert,
                text: l.to_string(),
            });
        }
        return out;
    }

    let w = m + 1;
    let mut dp = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + (j + 1)] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + (j + 1)])
            };
        }
    }

    let mut raw = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            raw.push(DiffLine {
                kind: DiffKind::Context,
                text: a[i].to_string(),
            });
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + (j + 1)] {
            raw.push(DiffLine {
                kind: DiffKind::Delete,
                text: a[i].to_string(),
            });
            i += 1;
        } else {
            raw.push(DiffLine {
                kind: DiffKind::Insert,
                text: b[j].to_string(),
            });
            j += 1;
        }
    }
    while i < n {
        raw.push(DiffLine {
            kind: DiffKind::Delete,
            text: a[i].to_string(),
        });
        i += 1;
    }
    while j < m {
        raw.push(DiffLine {
            kind: DiffKind::Insert,
            text: b[j].to_string(),
        });
        j += 1;
    }

    // 折叠连续超过 5 行的未改动上下文，保留前后各 2 行
    let changed: Vec<bool> = raw.iter().map(|l| l.kind != DiffKind::Context).collect();
    if changed.iter().all(|&c| !c) {
        return raw;
    }
    let mut keep = vec![false; raw.len()];
    for (idx, &c) in changed.iter().enumerate() {
        if c {
            let lo = idx.saturating_sub(2);
            let hi = (idx + 2).min(raw.len() - 1);
            for k in &mut keep[lo..=hi] {
                *k = true;
            }
        }
    }
    let mut out = Vec::new();
    let mut skipped = false;
    for (line, k) in raw.into_iter().zip(keep) {
        if k {
            skipped = false;
            out.push(line);
        } else if !skipped {
            skipped = true;
            out.push(DiffLine {
                kind: DiffKind::Context,
                text: "…".to_string(),
            });
        }
    }
    out
}

/// 将统一 diff 文本（如 Codex `fileChange` 的 diff）解析为 [`DiffLine`] 序列。
pub fn unified_diff_lines(patch: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    for line in patch.lines() {
        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            continue;
        }
        if line.starts_with("@@") {
            out.push(DiffLine {
                kind: DiffKind::Context,
                text: line.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            out.push(DiffLine {
                kind: DiffKind::Insert,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            out.push(DiffLine {
                kind: DiffKind::Delete,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix(' ') {
            out.push(DiffLine {
                kind: DiffKind::Context,
                text: rest.to_string(),
            });
        } else {
            out.push(DiffLine {
                kind: DiffKind::Context,
                text: line.to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_timeline_keeps_replies_and_actionable_entries_visible() {
        let tool = |id: &str, status, permission_pending| AgentEntry::ToolCall(falcon_proto::AgentToolCall {
            id: id.into(), title: id.into(), description: None, file_path: None,
            kind: falcon_proto::AgentToolKind::Read, status, content: Vec::new(),
            permission_pending, permission_reason: None,
        });
        let entries = vec![
            AgentEntry::User { id: "u".into(), text: "检查".into() },
            AgentEntry::Thought { id: "thought".into(), text: "分析".into() },
            tool("read", AgentToolStatus::Completed, false),
            AgentEntry::Notice { id: "info".into(), level: AgentNoticeLevel::Info, text: "已连接".into() },
            tool("permission", AgentToolStatus::Pending, true),
            tool("failed", AgentToolStatus::Failed, false),
            tool("running", AgentToolStatus::InProgress, false),
            AgentEntry::Notice { id: "warning".into(), level: AgentNoticeLevel::Warning, text: "警告".into() },
            AgentEntry::Message { id: "reply".into(), text: "完成".into() },
            AgentEntry::User { id: "u2".into(), text: "检查".into() },
            AgentEntry::Message { id: "reply2".into(), text: "完成".into() },
            AgentEntry::Message { id: "empty".into(), text: " \n".into() },
        ];
        assert_eq!(compact_timeline(&entries), vec![
            TimelineItem::Entry(0), TimelineItem::Activity(1..4), TimelineItem::Entry(4),
            TimelineItem::Entry(5), TimelineItem::Entry(6), TimelineItem::Entry(7),
            TimelineItem::Entry(8), TimelineItem::Entry(9), TimelineItem::Entry(10),
        ]);
    }

    #[test]
    fn preview_limits_lines_and_unicode_without_losing_full_output() {
        let text = "第一行\n第二行\n第三行";
        assert_eq!(text_preview(text, 2, 100), "第一行\n第二行");
        assert_eq!(text_preview("中文🙂abcdef", 12, 3), "中文🙂");
        assert_eq!(text_preview("a\r\nb", 2, 100), "a\r\nb");
        assert_eq!(text_preview("short", 12, 100), "short");
        assert_eq!(text_preview("", 12, 100), "");
    }

    #[test]
    fn styled_output_sgr_and_cr() {
        let s = styled_output("a\r\x1b[1;32mok\x1b[0m!\r\nnext");
        assert_eq!(s.text, "ok!\nnext");
        assert_eq!(
            s.spans,
            vec![StyledSpan {
                range: 0..2,
                fg: Some(AnsiColor::Green),
                bold: true,
            }]
        );
    }

    #[test]
    fn line_diff_basic() {
        let d = line_diff("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            d,
            vec![
                DiffLine {
                    kind: DiffKind::Context,
                    text: "a".into()
                },
                DiffLine {
                    kind: DiffKind::Delete,
                    text: "b".into()
                },
                DiffLine {
                    kind: DiffKind::Insert,
                    text: "B".into()
                },
                DiffLine {
                    kind: DiffKind::Context,
                    text: "c".into()
                },
            ]
        );
    }

    #[test]
    fn unified_diff_basic() {
        let patch = "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx";
        let d = unified_diff_lines(patch);
        assert_eq!(
            d,
            vec![
                DiffLine {
                    kind: DiffKind::Context,
                    text: "@@ -1,2 +1,2 @@".into()
                },
                DiffLine {
                    kind: DiffKind::Delete,
                    text: "old".into()
                },
                DiffLine {
                    kind: DiffKind::Insert,
                    text: "new".into()
                },
                DiffLine {
                    kind: DiffKind::Context,
                    text: "ctx".into()
                },
            ]
        );
    }
}
