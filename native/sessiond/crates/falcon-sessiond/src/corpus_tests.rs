//! 接回比对语料（设计文档第 15 节）：`tests/corpus/*.bin` 是录下来的 PTY 原始输出（100×30，
//! 由 tests/holder.rs 的 `record_corpus` 录）。每份分两路喂：
//! - 直接喂给客户端用的那份 vendor 的 alacritty；
//! - 喂给 libghostty-vt、按 VT 进程的做法导出（[`crate::vt::replay_bytes`]）、再喂给一个新的 alacritty。
//!
//! 两边逐行比：文字、软折行、样式、光标、模式、备用屏下的主屏历史。允许的差异只限设计文档
//! 6.2 列出的那几类（正文里的 OSC 8 链接不导出、`CSI 2J` 清掉的那一屏不进历史、从没写过的
//! 空格带着前一段的前景色），比较前先把它们归一掉。

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use ghostty_vt::{NoHandler, Terminal};

const COLS: u16 = 100;
const ROWS: u16 = 30;

struct Size;

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        ROWS as usize
    }
    fn screen_lines(&self) -> usize {
        ROWS as usize
    }
    fn columns(&self) -> usize {
        COLS as usize
    }
}

fn alacritty(input: &[u8]) -> Term<VoidListener> {
    let cfg = Config { scrolling_history: 10_000, ..Config::default() };
    let mut term = Term::new(cfg, &Size, VoidListener);
    let mut p: Processor = Processor::new();
    p.advance(&mut term, input);
    term
}

fn advance(term: &mut Term<VoidListener>, input: &[u8]) {
    let mut p: Processor = Processor::new();
    p.advance(term, input);
}

/// 0–15 号色：Indexed(N) 与 Named 同一个调色板槽位（导出时已改写成 3N，这里再归一一次以防万一）
fn norm(c: Color) -> String {
    match c {
        Color::Indexed(i) if i < 16 => format!("P{i}"),
        Color::Named(n) if (n as usize) < 16 => format!("P{}", n as usize),
        Color::Named(NamedColor::Foreground) | Color::Named(NamedColor::Background) => format!("{c:?}"),
        other => format!("{other:?}"),
    }
}

/// 每行一条：文字、软折行标记、样式。空格只看背景与反显 / 下划线（前景色不可见，见 6.2）
fn rows(term: &Term<VoidListener>) -> Vec<String> {
    let grid = term.grid();
    let mut out = Vec::new();
    for l in -(grid.history_size() as i32)..grid.screen_lines() as i32 {
        let row = &grid[Line(l)];
        let mut text = String::new();
        let mut style = String::new();
        for c in 0..grid.columns() {
            let cell = &row[Column(c)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
            if let Some(z) = cell.zerowidth() {
                text.extend(z);
            }
            let f = cell.flags & (Flags::BOLD | Flags::UNDERLINE | Flags::INVERSE | Flags::ITALIC);
            if cell.c == ' ' {
                style.push_str(&format!("_/{}/{:?};", norm(cell.bg), f & (Flags::INVERSE | Flags::UNDERLINE)));
            } else {
                style.push_str(&format!("{}/{}/{:?};", norm(cell.fg), norm(cell.bg), f));
            }
        }
        // 只看到最后一个非空格为止的样式：行尾空白的样式与内容无关
        let wrapped = row[Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
        let trimmed = text.trim_end();
        let keep = style.split(';').take(trimmed.chars().count()).collect::<Vec<_>>().join(";");
        out.push(format!("{trimmed}{}|{keep}", if wrapped { " ⏎" } else { "" }));
    }
    out
}

fn modes(term: &Term<VoidListener>) -> String {
    let m = term.mode();
    format!(
        "cursor={:?} alt={} bpaste={} sgr_mouse={} drag={} app_cursor={} show_cursor={}",
        term.grid().cursor.point,
        m.contains(TermMode::ALT_SCREEN),
        m.contains(TermMode::BRACKETED_PASTE),
        m.contains(TermMode::SGR_MOUSE),
        m.contains(TermMode::MOUSE_DRAG),
        m.contains(TermMode::APP_CURSOR),
        m.contains(TermMode::SHOW_CURSOR),
    )
}

/// `b` 是不是 `a` 的子序列（ED2 清掉的屏在 ghostty 里不进历史，alacritty 会推进去）
fn is_subsequence(a: &[String], b: &[String]) -> bool {
    let mut it = a.iter();
    b.iter().all(|x| it.any(|y| y == x))
}

fn replay_of(input: &[u8]) -> Vec<u8> {
    let mut t = Terminal::new(COLS, ROWS, NoHandler).unwrap();
    t.set_continuation_max_bytes(1 << 20).unwrap();
    t.set_scrollback_max_lines(10_000).unwrap();
    t.vt_write(input);
    let mut out = crate::vt::replay_bytes(&t);
    out.extend(t.continuation().unwrap_or_default());
    out
}

fn compare(name: &str, input: &[u8]) -> Vec<String> {
    let mut problems = Vec::new();
    let replay = replay_of(input);
    let direct = alacritty(input);
    let replayed = alacritty(&replay);
    let (a, b) = (rows(&direct), rows(&replayed));
    let screen = ROWS as usize;
    let (a_hist, a_screen) = a.split_at(a.len() - screen);
    let (b_hist, b_screen) = b.split_at(b.len() - screen);
    for (i, (x, y)) in a_screen.iter().zip(b_screen).enumerate() {
        if x != y {
            problems.push(format!("{name}：可见区第 {i} 行\n  直接 {x}\n  重放 {y}"));
        }
    }
    if modes(&direct) != modes(&replayed) {
        problems.push(format!("{name}：模式\n  直接 {}\n  重放 {}", modes(&direct), modes(&replayed)));
    }
    if !is_subsequence(a_hist, b_hist) || (b_hist.is_empty() && !a_hist.is_empty() && !name.contains("cleared")) {
        problems.push(format!("{name}：历史对不上（直接 {} 行 / 重放 {} 行）", a_hist.len(), b_hist.len()));
    }
    // 在备用屏里：退出备用屏后再比一次主屏
    if direct.mode().contains(TermMode::ALT_SCREEN) {
        let mut d = alacritty(input);
        let mut r = alacritty(&replay);
        advance(&mut d, b"\x1b[?1049l");
        advance(&mut r, b"\x1b[?1049l");
        let (a, b) = (rows(&d), rows(&r));
        let (a_hist, a_screen) = a.split_at(a.len() - screen);
        let (b_hist, b_screen) = b.split_at(b.len() - screen);
        for (i, (x, y)) in a_screen.iter().zip(b_screen).enumerate() {
            if x != y {
                problems.push(format!("{name}：退出备用屏后主屏第 {i} 行\n  直接 {x}\n  重放 {y}"));
            }
        }
        if !is_subsequence(a_hist, b_hist) {
            problems.push(format!("{name}：退出备用屏后主屏历史对不上（直接 {} / 重放 {}）", a_hist.len(), b_hist.len()));
        }
    }
    problems
}

#[test]
fn corpus_replays_identically_except_for_known_differences() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus");
    let mut files: Vec<_> = std::fs::read_dir(dir).expect("tests/corpus").flatten().map(|e| e.path()).collect();
    files.sort();
    assert!(files.len() >= 5, "语料太少：{files:?}");
    let mut problems = Vec::new();
    for f in &files {
        let input = std::fs::read(f).unwrap();
        // OSC 8 链接不导出是已知差异：比较前把链接从直接喂的那一路里去掉
        let input = strip_osc8(&input);
        problems.extend(compare(&f.file_stem().unwrap().to_string_lossy(), &input));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// 去掉 OSC 8 超链接序列（只留文字）
fn strip_osc8(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(b"\x1b]8;") {
            let mut j = i + 4;
            while j < input.len() && input[j] != 0x07 && !input[j..].starts_with(b"\x1b\\") {
                j += 1;
            }
            i = if j < input.len() && input[j] == 0x07 { j + 1 } else { j + 2 };
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

/// 切点落在半截序列上：快照 + continuation，再接上后半截，与一口气喂完的结果相同
#[test]
fn split_points_inside_sequences_are_continued() {
    let full = "前文\x1b[38;2;1;2;3m彩色\x1b]0;标题\x07后文\x1b[1m粗\x1b[0m".as_bytes();
    for cut in 1..full.len() {
        let mut t = Terminal::new(COLS, ROWS, NoHandler).unwrap();
        t.set_continuation_max_bytes(1 << 20).unwrap();
        t.vt_write(&full[..cut]);
        let mut replay = crate::vt::replay_bytes(&t);
        replay.extend(t.continuation().unwrap_or_default());
        replay.extend_from_slice(&full[cut..]);
        let (a, b) = (rows(&alacritty(full)), rows(&alacritty(&replay)));
        assert_eq!(a, b, "切在第 {cut} 字节");
    }
}

/// 有历史、屏幕底部是空行、光标停在空行上（敲了几下回车、`cat` 在等输入）：格式化器省掉的
/// 末尾空行要补回来，否则重放后整屏上移、光标压在内容行上
#[test]
fn trailing_blank_rows_are_kept() {
    let mut s = String::new();
    for i in 0..100 {
        s.push_str(&format!("\x1b[4{}m{i}\x1b[0m\r\n", i % 8));
    }
    s.push_str("\r\n\r\n   ");
    let input = s.as_bytes();
    assert_eq!(compare("trailing-blank", input), Vec::<String>::new());
    // 关了自动换行的程序：长行不能在重放时被截断
    let mut s = "x".repeat(250);
    s.push_str("\r\n\x1b[?7l");
    assert_eq!(compare("nowrap", s.as_bytes()), Vec::<String>::new());
}
