//! 终端输出流上的 shell 标记（AI 助手，docs/design/ai-assistant.md 4.2）：服务端的
//! `sessions/shell_marks.rs` 拿它跟踪对话终端的提示符与命令块，客户端拿它在实时区域里找
//! 「当前提示符从哪一行开始」。
//!
//! 标记由注入的 shell integration 打出来：OSC 133 的 `A`（提示符开始）、`C`（命令开始输出）、
//! `D;<退出码>`（命令结束），OSC 633 的 `E;<命令行>`，OSC 7 的 `file://主机/当前目录`。
//!
//! [`MarkScanner`] 把一块块输出切成「文字」与「标记」。一个 OSC 可能被切在两块输出之间，认不全的
//! 尾巴留到下一块再拼（有上限，防止一个永远不结束的 OSC 把内存吃光）。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    PromptStart,
    CommandStart,
    /// 退出码；shell 没给（`133;D` 不带参数）是 None
    CommandEnd(Option<i32>),
    Cwd(String),
    /// 这条命令的命令行（OSC 633;E，VS Code 的写法）：命令块的标题
    CommandLine(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Mark(Mark),
}

/// 认不全的序列最多攒这么多字节（OSC 7 带的是路径，远小于它）
const MAX_CARRY: usize = 8192;

/// 把输出切成文字与标记，跨块拼序列
#[derive(Debug, Default)]
pub struct MarkScanner {
    carry: String,
}

enum Classify {
    /// 不是我们认的 OSC（或者是别的转义序列）：当文字
    NotOurs,
    /// 可能是我们的，但还没收全
    Incomplete,
    /// 收全了：整段序列的字节数，以及认出来的标记（`133;B` 之类不关心的是 None）
    Complete { len: usize, mark: Option<Mark> },
}

impl MarkScanner {
    pub fn feed(&mut self, data: &str) -> Vec<Piece> {
        let mut input = std::mem::take(&mut self.carry);
        input.push_str(data);
        let mut out = Vec::new();
        let mut text_start = 0;
        let mut i = 0;
        while let Some(rel) = input[i..].find('\x1b') {
            let esc = i + rel;
            match classify(&input[esc..]) {
                Classify::NotOurs => i = esc + 1,
                Classify::Incomplete if input.len() - esc <= MAX_CARRY => {
                    push_text(&mut out, &input[text_start..esc]);
                    self.carry = input[esc..].to_string();
                    return out;
                }
                Classify::Incomplete => i = esc + 1,
                Classify::Complete { len, mark } => {
                    push_text(&mut out, &input[text_start..esc]);
                    if let Some(m) = mark {
                        out.push(Piece::Mark(m));
                    }
                    text_start = esc + len;
                    i = text_start;
                }
            }
        }
        push_text(&mut out, &input[text_start..]);
        out
    }
}

fn push_text(out: &mut Vec<Piece>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(Piece::Text(t)) = out.last_mut() {
        t.push_str(s);
    } else {
        out.push(Piece::Text(s.to_string()));
    }
}

/// `rest` 以 ESC 开头
fn classify(rest: &str) -> Classify {
    const PREFIXES: [&str; 3] = ["133;", "7;", "633;"];
    let Some(after) = rest.strip_prefix("\x1b") else { return Classify::NotOurs };
    if after.is_empty() {
        return Classify::Incomplete;
    }
    let Some(body) = after.strip_prefix(']') else { return Classify::NotOurs };
    if !PREFIXES.iter().any(|p| body.starts_with(p)) {
        // 还短于前缀、且对得上其中一个的开头：等下一块
        return if PREFIXES.iter().any(|p| p.starts_with(body)) { Classify::Incomplete } else { Classify::NotOurs };
    }
    // 找终止符：BEL，或 ST（ESC \）
    let bytes = body.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        match bytes[j] {
            0x07 => return Classify::Complete { len: 2 + j + 1, mark: parse(&body[..j]) },
            0x1b => {
                return match bytes.get(j + 1) {
                    None => Classify::Incomplete,
                    Some(b'\\') => Classify::Complete { len: 2 + j + 2, mark: parse(&body[..j]) },
                    // 中途插进来别的转义：这段 OSC 是坏的，当文字
                    Some(_) => Classify::NotOurs,
                };
            }
            _ => j += 1,
        }
    }
    Classify::Incomplete
}

fn parse(payload: &str) -> Option<Mark> {
    if let Some(rest) = payload.strip_prefix("133;") {
        let mut parts = rest.split(';');
        return match parts.next()? {
            "A" => Some(Mark::PromptStart),
            "C" => Some(Mark::CommandStart),
            "D" => Some(Mark::CommandEnd(parts.next().and_then(|c| c.trim().parse().ok()))),
            _ => None,
        };
    }
    if let Some(rest) = payload.strip_prefix("633;") {
        // 633 里只认 E（命令行）；别的（A/B/C/D 的 VS Code 版）我们自己的脚本不打
        let cmd = rest.strip_prefix("E;")?;
        // E;<命令行>[;nonce]：命令行里的 `;` 与 `\` 都转义过，第一个裸 `;` 之后是 nonce
        let cmd = cmd.split(';').next().unwrap_or("");
        return Some(Mark::CommandLine(unescape_633(cmd)));
    }
    let url = payload.strip_prefix("7;")?;
    let rest = url.strip_prefix("file://")?;
    // file://主机/路径：主机可以为空（file:///tmp）
    let path = &rest[rest.find('/')?..];
    Some(Mark::Cwd(percent_decode(path)))
}

/// OSC 7 的路径可能是百分号编码的（VS Code、iTerm 的集成这么打；我们自己的脚本打的是原样）。
/// 解不成 UTF-8 就原样用
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// OSC 633 的转义：`\\` 是反斜杠，`\xNN` 是一个字节（`;`、换行、控制字符都这么写）
fn unescape_633(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if bytes.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'x')
                && let (Some(h), Some(l)) = (bytes.get(i + 2).and_then(|b| hex(*b)), bytes.get(i + 3).and_then(|b| hex(*b)))
            {
                out.push(h * 16 + l);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn marks(pieces: &[Piece]) -> Vec<Mark> {
        pieces.iter().filter_map(|p| if let Piece::Mark(m) = p { Some(m.clone()) } else { None }).collect()
    }

    fn text(pieces: &[Piece]) -> String {
        pieces.iter().filter_map(|p| if let Piece::Text(t) = p { Some(t.as_str()) } else { None }).collect()
    }

    #[test]
    fn scans_marks_with_bel_and_st() {
        let mut s = MarkScanner::default();
        let p = s.feed("a\x1b]133;D;2\x07\x1b]133;A\x1b\\b\x1b]7;file://h/Users/fay\x07\x1b[31mred");
        assert_eq!(
            marks(&p),
            [Mark::CommandEnd(Some(2)), Mark::PromptStart, Mark::Cwd("/Users/fay".into())]
        );
        // 别的转义原样当文字
        assert_eq!(text(&p), "ab\x1b[31mred");
    }

    #[test]
    fn sequences_split_across_chunks_are_reassembled() {
        let whole = "out\x1b]133;D;0\x07\x1b]7;file://host/tmp/a b\x07$ ";
        for cut in 0..whole.len() {
            if !whole.is_char_boundary(cut) {
                continue;
            }
            let mut s = MarkScanner::default();
            let mut p = s.feed(&whole[..cut]);
            p.extend(s.feed(&whole[cut..]));
            assert_eq!(marks(&p), [Mark::CommandEnd(Some(0)), Mark::Cwd("/tmp/a b".into())], "cut at {cut}");
            assert_eq!(text(&p), "out$ ", "cut at {cut}");
        }
    }

    #[test]
    fn other_oscs_and_lone_escapes_are_text() {
        let mut s = MarkScanner::default();
        let p = s.feed("\x1b]0;title\x07\x1b]1337;x\x07\x1b]13x");
        assert!(marks(&p).is_empty());
        assert_eq!(text(&p), "\x1b]0;title\x07\x1b]1337;x\x07\x1b]13x");
        // 末尾一个孤零零的 ESC 要等下一块
        assert_eq!(text(&s.feed("q\x1b")), "q");
        assert_eq!(text(&s.feed("[0m")), "\x1b[0m");
    }

    #[test]
    fn unterminated_osc_does_not_grow_forever() {
        let mut s = MarkScanner::default();
        let big = format!("\x1b]7;file://h/{}", "x".repeat(MAX_CARRY + 10));
        let p = s.feed(&big);
        assert_eq!(text(&p), big);
    }

    #[test]
    fn cwd_is_percent_decoded() {
        assert_eq!(parse("7;file://h/tmp/a%20b/%E4%BD%A0"), Some(Mark::Cwd("/tmp/a b/你".into())));
        assert_eq!(parse("7;file:///tmp"), Some(Mark::Cwd("/tmp".into())));
        assert_eq!(parse("7;file://h"), None);
        assert_eq!(parse("133;D"), Some(Mark::CommandEnd(None)));
        assert_eq!(parse("133;B"), None);
    }

    #[test]
    fn command_lines_are_unescaped() {
        assert_eq!(parse("633;E;ls -la"), Some(Mark::CommandLine("ls -la".into())));
        assert_eq!(parse("633;E;a\\x3bb\\\\c\\x0ad;nonce"), Some(Mark::CommandLine("a;b\\c\nd".into())));
        assert_eq!(parse("633;E;echo 你好"), Some(Mark::CommandLine("echo 你好".into())));
        assert_eq!(parse("633;P;Cwd=/x"), None);
        let mut s = MarkScanner::default();
        let p = s.feed("\x1b]633;E;git status\x07\x1b]133;C\x07out");
        assert_eq!(p, [Piece::Mark(Mark::CommandLine("git status".into())), Piece::Mark(Mark::CommandStart), Piece::Text("out".into())]);
    }
}
