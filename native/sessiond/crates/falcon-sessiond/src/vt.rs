//! VT 进程（`falcon-sessiond vt`）：吃 holder 转来的输出副本，在 libghostty-vt 里维护终端状态，
//! 回答终端查询、导出接回用的快照。stdin / stdout 是与 holder 之间的帧（vtproto.rs）。
//!
//! 它是会话里唯一碰 libghostty-vt 的进程：Zig 的 panic 是 trap、跨 FFI 接不住，挂了也只挂它，
//! holder 会重启它并用原始尾巴重建（ADR 0020 决定一）。

use std::io::{BufWriter, Write as _};

use anyhow::Result;
use falcon_proto::program_status::{ProgramBlockKind, ProgramState, ProgramStatusReport};
use falcon_proto::sessiond::{Appearance, ExportFormat, FALCON_VERSION, Scheme};
use ghostty_vt::{ColorScheme, Format, FormatOptions, Handler, PromptKind, Rgb, Screen, StatusKind, StatusState, Terminal};

use crate::replay::{rewrite_palette16, strip_kitty_query_reply};
use crate::vtproto::{FromVt, Prompt, Ready, Restored, ToVt, read_frame};

/// 历史的字节上限：行数上限之外再兜一道（满屏样式的宽行一行能占几 KB）
const SCROLLBACK_MAX_BYTES: usize = 32 << 20;
/// continuation 的上限：超了（巨大的 kitty 图片 APC 卡在半截）就只能接受切点处的瑕疵
const CONTINUATION_MAX_BYTES: usize = 1 << 20;

struct VtHandler {
    /// 攒着的回 holder 的帧，每处理完一条命令写出去
    out: Vec<u8>,
    /// 正在重建（喂原始尾巴 / 恢复快照）：不答查询、不报事件
    suppress: bool,
    scheme: Option<ColorScheme>,
    version: String,
}

impl VtHandler {
    fn push(&mut self, msg: FromVt) {
        self.out.extend_from_slice(&msg.encode());
    }
}

impl Handler for VtHandler {
    fn write_pty(&mut self, data: &[u8]) {
        if self.suppress {
            return;
        }
        let data = strip_kitty_query_reply(data);
        if !data.is_empty() {
            self.push(FromVt::PtyWrite(data));
        }
    }

    fn title_changed(&mut self, title: &str) {
        if !self.suppress {
            self.push(FromVt::Title(title.to_string()));
        }
    }

    fn program_status(&mut self, s: ghostty_vt::ProgramStatus) {
        if self.suppress {
            return;
        }
        let state = match s.state {
            StatusState::Idle => Some(ProgramState::Idle),
            StatusState::Working => Some(ProgramState::Working),
            StatusState::Done => Some(ProgramState::Done),
            StatusState::Blocked => Some(ProgramState::Blocked),
            StatusState::Error => Some(ProgramState::Error),
            StatusState::Clear => None,
        };
        let kind = s.kind.map(|k| match k {
            StatusKind::Permission => ProgramBlockKind::Permission,
            StatusKind::Question => ProgramBlockKind::Question,
            StatusKind::Auth => ProgramBlockKind::Auth,
        });
        self.push(FromVt::Status(ProgramStatusReport {
            id: s.id,
            state,
            kind,
            progress: s.progress,
            app: s.app,
            title: s.title,
            message: s.message,
        }));
    }

    fn semantic_prompt(&mut self, p: ghostty_vt::SemanticPrompt) {
        if self.suppress {
            return;
        }
        let kind = match p.kind {
            PromptKind::PromptStart => "A",
            PromptKind::InputStart => "B",
            PromptKind::OutputStart => "C",
            PromptKind::CommandEnd => "D",
        };
        self.push(FromVt::Prompt(Prompt { kind: kind.into(), exit_code: p.exit_code }));
    }

    fn reset(&mut self) {
        if !self.suppress {
            self.push(FromVt::Reset);
        }
    }

    fn color_scheme(&mut self) -> Option<ColorScheme> {
        self.scheme
    }

    fn xtversion(&self) -> Option<&str> {
        // 不冒充 ghostty：程序会据此打开 ghostty 专有特性，而客户端画画的是 alacritty
        Some(&self.version)
    }
}

fn configure(t: &mut Terminal<VtHandler>, scrollback: usize) -> Result<()> {
    t.set_continuation_max_bytes(CONTINUATION_MAX_BYTES)?;
    t.set_scrollback_max_lines(scrollback)?;
    t.set_scrollback_max_bytes(SCROLLBACK_MAX_BYTES)?;
    Ok(())
}

/// 回放给客户端的完整状态（含模式、光标、样式）
fn full_options() -> FormatOptions {
    FormatOptions {
        format: Format::Vt,
        unwrap: true,
        trim: false,
        // 调色板归客户端的主题管；OSC 7 客户端不认（而且钉的 commit 导出 pwd 时多一个 NUL）
        palette: false,
        modes: true,
        scrolling_region: true,
        tabstops: true,
        pwd: false,
        keyboard: false,
        cursor: true,
        style: true,
        hyperlink: true,
        protection: true,
        // 客户端不支持 kitty 键盘，见 replay::strip_kitty_query_reply
        kitty_keyboard: false,
        charsets: true,
    }
}

/// 只要正文（带样式），不要模式、光标这些附加状态
fn content_options() -> FormatOptions {
    FormatOptions { style: true, hyperlink: true, ..FormatOptions::plain(Format::Vt) }
}

/// 格式化器总是省掉末尾的空行（formatter.zig 的 Options.trim 注释："Trailing blank lines are
/// always trimmed"）。有历史时这会让重放少滚几行：整屏上移，光标压到内容行上（语料实测）。
/// 拿一个同尺寸的临时终端重放正文，比对历史行数，差几行就补几个换行
fn missing_rows<H: Handler>(t: &Terminal<H>, content: &[u8]) -> usize {
    let want = t.scrollback_rows();
    if want == 0 {
        return 0;
    }
    let Ok(mut scratch) = Terminal::new(t.cols(), t.rows(), ghostty_vt::NoHandler) else { return 0 };
    let _ = scratch.set_scrollback_max_lines(want + t.rows() as usize + 64);
    scratch.vt_write(content);
    want.saturating_sub(scratch.scrollback_rows())
}

/// 格式化器把模式放在正文**之前**（formatter.zig："for simplicity we just emit them all before"）。
/// 有几个模式会改变正文的排版，必须挪到正文之后：关自动换行（`?7l`，正文里软折行的长行
/// 是靠自动换行折回去的）、插入模式（`4h`）、换行模式（`20h`）
const LATE_MODES: [&[u8]; 3] = [b"\x1b[?7l", b"\x1b[4h", b"\x1b[20h"];

fn split_late_modes(modes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut early = modes.to_vec();
    let mut late = Vec::new();
    for m in LATE_MODES {
        if let Some(i) = early.windows(m.len()).position(|w| w == m) {
            early.drain(i..i + m.len());
            late.extend_from_slice(m);
        }
    }
    (early, late)
}

/// 当前屏：模式 + 正文 + 补回的空行 + 光标等附加状态
fn screen_bytes<H: Handler>(t: &Terminal<H>) -> Vec<u8> {
    let full = t.format(&full_options()).unwrap_or_default();
    let content = t.format(&content_options()).unwrap_or_default();
    let pad = missing_rows(t, &content);
    // 单独导的正文末尾多一个 SGR 复位，整份导出里同一处没有（那里接的是光标位置）
    let body = content.strip_suffix(b"\x1b[0m").unwrap_or(&content);
    // 整份导出 = 模式与制表位 + 正文 + 附加状态；前面那段只有几百字节，在开头一段里找正文的起点
    let at = (0..=full.len().saturating_sub(body.len()).min(16 * 1024)).find(|&i| full[i..].starts_with(body));
    let Some(at) = at else { return full };
    let (modes, rest) = full.split_at(at);
    let extras = &rest[body.len()..];
    let (early, late) = split_late_modes(modes);
    let mut out = early;
    out.extend_from_slice(body);
    if pad > 0 {
        // 先复位样式：滚出来的新行会沾上当前的背景色
        out.extend_from_slice(b"\x1b[0m");
        for _ in 0..pad {
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(extras);
    out.extend_from_slice(&late);
    out
}

/// 设计文档 6.2：接回 / 回放用的 VT 字节。
///
/// 格式化器只导当前活动屏。在备用屏里时：在副本上退出备用屏、导出主屏（含历史），光标放回
/// 主屏进备用屏时存下的位置，拼上 `CSI ? 1049 h CSI H CSI 2 J`，再接当前备用屏的完整导出
/// （原型实测主屏 306 行历史对得上）。
pub fn replay_bytes<H: Handler>(t: &Terminal<H>) -> Vec<u8> {
    let out = match t.active_screen() {
        Screen::Primary => screen_bytes(t),
        Screen::Alternate => {
            let mut out = match t.duplicate() {
                Ok(mut copy) => {
                    copy.vt_write(b"\x1b[?1049l");
                    let mut primary = copy.format(&content_options()).unwrap_or_default();
                    let pad = missing_rows(&copy, &primary);
                    if !primary.ends_with(b"\x1b[0m") {
                        primary.extend_from_slice(b"\x1b[0m");
                    }
                    for _ in 0..pad {
                        primary.extend_from_slice(b"\r\n");
                    }
                    // 退出备用屏时光标回到这里（1049 进出时存取的就是它）
                    let (x, y) = copy.cursor();
                    primary.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
                    primary
                }
                Err(_) => Vec::new(),
            };
            out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
            out.extend_from_slice(&screen_bytes(t));
            out
        }
    };
    rewrite_palette16(&out)
}

fn export(t: &Terminal<VtHandler>, format: ExportFormat) -> Vec<u8> {
    match format {
        ExportFormat::Vt => replay_bytes(t),
        ExportFormat::Plain => t.format(&FormatOptions::plain(Format::Plain)).unwrap_or_default(),
        ExportFormat::Html => t.format(&FormatOptions { style: true, hyperlink: true, ..FormatOptions::plain(Format::Html) }).unwrap_or_default(),
    }
}

fn apply_appearance(t: &mut Terminal<VtHandler>, a: &Appearance) {
    t.handler_mut().scheme = a.scheme.map(|s| match s {
        Scheme::Light => ColorScheme::Light,
        Scheme::Dark => ColorScheme::Dark,
    });
    let c = |v: &Option<String>| v.as_deref().and_then(Rgb::parse_hex);
    let _ = t.set_default_colors(c(&a.foreground), c(&a.background), c(&a.cursor).or(c(&a.foreground)));
}

pub fn run(cols: u16, rows: u16, scrollback: usize) -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = BufWriter::new(std::io::stdout().lock());
    let handler = VtHandler { out: Vec::new(), suppress: false, scheme: None, version: format!("falcon {FALCON_VERSION}") };
    let mut term = Terminal::new(cols, rows, handler)?;
    configure(&mut term, scrollback)?;
    output.write_all(&FromVt::Ready(Ready { ghostty: ghostty_vt::GHOSTTY_COMMIT.into() }).encode())?;
    output.flush()?;

    while let Some((k, p)) = read_frame(&mut input)? {
        let Ok(msg) = ToVt::decode(k, p) else { continue };
        match msg {
            ToVt::Feed { replay, bytes } => {
                term.handler_mut().suppress = replay;
                term.vt_write(&bytes);
                term.handler_mut().suppress = false;
            }
            ToVt::Resize(r) => {
                let _ = term.resize(r.cols, r.rows);
            }
            ToVt::Snapshot { req } => {
                let snapshot = replay_bytes(&term);
                let continuation = term.continuation().unwrap_or_default();
                term.handler_mut().push(FromVt::Snapshotted { req, snapshot, continuation });
            }
            ToVt::Export { req, format } => {
                let bytes = export(&term, format);
                term.handler_mut().push(FromVt::Exported { req, bytes });
            }
            ToVt::Appearance(a) => apply_appearance(&mut term, &a),
            ToVt::Encode { req } => {
                let bytes = term.encode_snapshot().unwrap_or_default();
                term.handler_mut().push(FromVt::Encoded { req, bytes });
            }
            ToVt::Restore(bytes) => {
                let h = VtHandler {
                    out: std::mem::take(&mut term.handler_mut().out),
                    suppress: false,
                    scheme: term.handler().scheme,
                    version: term.handler().version.clone(),
                };
                match Terminal::decode_snapshot(&bytes, h) {
                    Ok(mut restored) => {
                        configure(&mut restored, scrollback)?;
                        term = restored;
                        term.handler_mut().push(FromVt::Restored(Restored { ok: true, message: String::new() }));
                    }
                    Err(e) => term.handler_mut().push(FromVt::Restored(Restored { ok: false, message: e.to_string() })),
                }
            }
            ToVt::Unknown(_) => {}
        }
        let out = std::mem::take(&mut term.handler_mut().out);
        if !out.is_empty() {
            output.write_all(&out)?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Terminal<VtHandler>, s: &str) {
        t.vt_write(s.as_bytes());
    }

    fn term() -> Terminal<VtHandler> {
        let h = VtHandler { out: Vec::new(), suppress: false, scheme: None, version: "falcon test".into() };
        let mut t = Terminal::new(40, 10, h).unwrap();
        configure(&mut t, 1000).unwrap();
        t
    }

    #[test]
    fn suppressed_feeds_do_not_answer_queries() {
        let mut t = term();
        t.handler_mut().suppress = true;
        feed(&mut t, "\x1b[c\x1b]7501;state=working\x07");
        t.handler_mut().suppress = false;
        assert!(t.handler().out.is_empty());
        feed(&mut t, "\x1b[c\x1b[?u");
        let frames = std::mem::take(&mut t.handler_mut().out);
        let mut cur = std::io::Cursor::new(frames);
        let mut writes = Vec::new();
        while let Some((k, p)) = read_frame(&mut cur).unwrap() {
            if let FromVt::PtyWrite(b) = FromVt::decode(k, p).unwrap() {
                writes.extend(b);
            }
        }
        let s = String::from_utf8_lossy(&writes);
        assert!(s.contains("\x1b[?62;"), "{s:?}");
        assert!(!s.contains("u"), "kitty 的回答被滤掉：{s:?}");
    }

    #[test]
    fn replay_in_alt_screen_keeps_the_primary_history() {
        let mut t = term();
        for i in 0..30 {
            feed(&mut t, &format!("\x1b[1;38;5;1mrow{i}\x1b[0m\r\n"));
        }
        feed(&mut t, "\x1b[?1049h\x1b[?1002h\x1b[Hvim");
        let out = String::from_utf8(replay_bytes(&t)).unwrap();
        let alt = out.find("\x1b[?1049h\x1b[H\x1b[2J").expect(&out);
        assert!(out[..alt].contains("row0") && out[..alt].contains("row29"), "{out:?}");
        assert!(out[alt..].contains("vim"), "{out:?}");
        assert!(!out.contains("38;5;1"), "16 色改写过了：{out:?}");
    }
}
