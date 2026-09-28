//! 一个会话的终端状态：alacritty 的 `Term` + VT 解析器 + 模式跟踪 + 事件队列。
//!
//! 线程模型（设计见 docs/design/gpui-client.md §3.1）：
//! - 网络线程收到 OUTPUT 帧直接调 [`TermCore::advance`] 加锁解析，UI 线程不碰解析；
//! - REPLAY 帧（最大 4MB，可能在连接中途任意时刻到达——背压重同步）走
//!   [`TermCore::replace_with_replay`]：在一个**新 Term** 上离锁解析，完成后加锁整体换上。语义就是
//!   web 的"reset 再 write"，但 UI 线程既不会被 4MB 解析卡住，也不会画出半截画面；
//! - UI 线程每帧 [`TermCore::snapshot`]：加锁把可见区拷出来立即解锁，排版绘制只用快照。
//!
//! 同一个会话的 advance / replace 由同一条 socket 任务顺序调用，不会并发；resize / 选区 / 滚动
//! 来自 UI 线程，靠 FairMutex 与解析互斥。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor, StdSyncHandler};
use parking_lot::Mutex;

use crate::mouse::MouseMode;
use crate::modes::TermModeTracker;

/// 终端格数。只有可见区——scrollback 由 Term 自己按 `Config::scrolling_history` 管。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl TermSize {
    pub fn new(cols: usize, rows: usize) -> Self {
        // alacritty 在 0 行 / 0 列上会越界 panic
        Self {
            cols: cols.max(2),
            rows: rows.max(1),
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// 需要 app 处理的终端事件。颜色查询（OSC 10/11/12）、文本区尺寸查询（CSI 14t）、剪贴板读取
/// （OSC 52 查询）在这里就被丢掉了，理由见 [`Listener`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// OSC 52 写剪贴板（只写不读，与 web 一致）
    ClipboardStore(String),
    /// 终端要回写给程序的应答（DA / DSR 等），app 按 `input` 发回会话
    PtyWrite(String),
    /// OSC 0/2 标题；None 是重置。界面标题以服务端推的前台命令为准，这个只作备用
    Title(Option<String>),
    Bell,
}

/// alacritty 的事件出口。所有 Term 共享同一个队列；`muted` 只在回放解析期间为真。
///
/// 回放期间一律静音：回放里带着 zellij attach 时发的查询（DA、DSR 之类），再答一次就是往
/// zellij 里敲一串垃圾（`termEnv.ts` 的 OscColorGate 注释写过同一个坑）；历史里的 OSC 52 也
/// 不该在重连时再写一遍剪贴板。
///
/// 颜色查询不答：服务端的 OscColorGate 在 Viewer 发过 `appearance` 之后代答 OSC 10/11/12，
/// 客户端再答一次，多个 Viewer 就会各答一遍。CSI 14t（文本区像素尺寸）照 xterm.js 默认
/// （windowOptions 关）不答。
#[derive(Clone)]
pub struct Listener {
    queue: Arc<Mutex<Vec<TermEvent>>>,
    muted: Arc<AtomicBool>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if self.muted.load(Ordering::Relaxed) {
            return;
        }
        let ev = match event {
            Event::ClipboardStore(_, text) => TermEvent::ClipboardStore(text),
            Event::PtyWrite(text) => TermEvent::PtyWrite(text),
            Event::Title(t) => TermEvent::Title(Some(t)),
            Event::ResetTitle => TermEvent::Title(None),
            Event::Bell => TermEvent::Bell,
            _ => return,
        };
        self.queue.lock().push(ev);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermOptions {
    /// 与 web 一致取 10000（`TerminalView.tsx` 对齐 zellij 的 scroll_buffer）。zellij 会话在
    /// alt screen 里，本地 scrollback 实际用不上，按需增长不占内存。
    pub scrollback: usize,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
}

impl Default for TermOptions {
    fn default() -> Self {
        Self {
            scrollback: 10_000,
            cursor_shape: CursorShape::Block,
            cursor_blinking: true,
        }
    }
}

fn term_config(opts: &TermOptions) -> Config {
    Config {
        scrolling_history: opts.scrollback,
        default_cursor_style: CursorStyle {
            shape: opts.cursor_shape,
            blinking: opts.cursor_blinking,
        },
        // 不编 kitty 键盘协议（keys.rs 只有 legacy 编码）：开了它 zellij 协商成功就会期待
        // kitty 格式的按键
        kitty_keyboard: false,
        osc52: Osc52::OnlyCopy,
        ..Config::default()
    }
}

struct Parser {
    processor: Processor<StdSyncHandler>,
    modes: TermModeTracker,
}

impl Parser {
    fn new() -> Self {
        Self {
            processor: Processor::new(),
            modes: TermModeTracker::new(),
        }
    }
}

/// 可见区里的一格。
#[derive(Clone, Debug)]
pub struct SnapCell {
    pub cell: Cell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapCursor {
    /// 可见区坐标（0 起）
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// 光标所在格是宽字符（块光标要画两格宽）
    pub wide: bool,
}

/// 选区，可见区坐标，end 含。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapSelection {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub is_block: bool,
}

/// 一帧要画的全部东西。拷贝出来之后与 Term 无关，可以离锁排版。
/// （alacritty 的 `Colors` 没有 Debug，所以这里也不派生。）
#[derive(Clone)]
pub struct Snapshot {
    pub size: TermSize,
    /// 行优先，rows × cols
    pub cells: Vec<Cell>,
    pub cursor: Option<SnapCursor>,
    pub selection: Option<SnapSelection>,
    pub mode: TermMode,
    pub display_offset: usize,
    pub history_size: usize,
    /// 程序用 OSC 4/10/11 改过的颜色（None = 用主题色）
    pub colors: Colors,
    /// 每次回放整体替换 Term 都 +1；UI 据此丢掉本地的选区 / 悬停等派生状态
    pub generation: u64,
}

impl Snapshot {
    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[row * self.size.cols + col]
    }

    /// 一行的文本（宽字符占位格跳过），以及每个 char 对应的列号。给链接识别与选区用。
    pub fn row_text(&self, row: usize) -> (String, Vec<usize>) {
        let mut text = String::with_capacity(self.size.cols);
        let mut cols = Vec::with_capacity(self.size.cols);
        for col in 0..self.size.cols {
            let cell = self.cell(row, col);
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
            cols.push(col);
        }
        (text, cols)
    }
}

pub struct TermCore {
    term: FairMutex<Term<Listener>>,
    parser: Mutex<Parser>,
    queue: Arc<Mutex<Vec<TermEvent>>>,
    size: Mutex<TermSize>,
    options: Mutex<TermOptions>,
    dirty: AtomicBool,
    generation: AtomicU64,
}

impl TermCore {
    pub fn new(size: TermSize, options: TermOptions) -> Self {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener {
            queue: queue.clone(),
            muted: Arc::new(AtomicBool::new(false)),
        };
        let term = Term::new(term_config(&options), &size, listener);
        Self {
            term: FairMutex::new(term),
            parser: Mutex::new(Parser::new()),
            queue,
            size: Mutex::new(size),
            options: Mutex::new(options),
            dirty: AtomicBool::new(true),
            generation: AtomicU64::new(0),
        }
    }

    /// 解析一段实时输出。返回 true 表示脏位由 0 变 1——调用方据此唤醒 UI，每帧至多一次。
    pub fn advance(&self, bytes: &[u8]) -> bool {
        let mut parser = self.parser.lock();
        parser.modes.track(bytes);
        let mut term = self.term.lock();
        // 同步输出（?2026）的超时只有在解析时才检查：zellij 的一帧若开了同步却迟迟不收尾，
        // 下一段输出到来时先把过期的缓冲冲掉，免得画面停住
        flush_expired_sync(&mut parser.processor, &mut term);
        parser.processor.advance(&mut *term, bytes);
        drop(term);
        drop(parser);
        self.mark_dirty()
    }

    /// 回放：在新 Term 上离锁解析整份快照，完成后整体替换。返回值同 [`Self::advance`]。
    pub fn replace_with_replay(&self, bytes: &[u8]) -> bool {
        let size = *self.size.lock();
        let options = *self.options.lock();
        let muted = Arc::new(AtomicBool::new(true));
        let listener = Listener {
            queue: self.queue.clone(),
            muted: muted.clone(),
        };
        let mut fresh = Term::new(term_config(&options), &size, listener);
        let mut parser = Parser::new();
        parser.modes.track(bytes);
        parser.processor.advance(&mut fresh, bytes);
        // 回放末尾若停在未收尾的同步块里，别让它把之后的实时输出一起憋住
        if parser.processor.sync_timeout().sync_timeout().is_some() {
            parser.processor.stop_sync(&mut fresh);
        }
        muted.store(false, Ordering::Relaxed);

        // 解析期间 UI 可能又 resize 过：换上之前补到最新尺寸
        let latest = *self.size.lock();
        if latest != size {
            fresh.resize(latest);
        }
        {
            let mut guard = self.parser.lock();
            let mut term = self.term.lock();
            *term = fresh;
            *guard = parser;
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.mark_dirty()
    }

    /// 同步输出块超时的兜底冲刷；app 可以在定时器里调（例如光标闪烁的节拍上）。
    pub fn flush_sync_if_expired(&self) -> bool {
        let mut parser = self.parser.lock();
        let mut term = self.term.lock();
        if flush_expired_sync(&mut parser.processor, &mut term) {
            drop(term);
            drop(parser);
            return self.mark_dirty();
        }
        false
    }

    fn mark_dirty(&self) -> bool {
        !self.dirty.swap(true, Ordering::AcqRel)
    }

    /// UI 取帧前调用：返回是否有新内容，并清掉脏位。
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn take_events(&self) -> Vec<TermEvent> {
        std::mem::take(&mut *self.queue.lock())
    }

    pub fn size(&self) -> TermSize {
        *self.size.lock()
    }

    /// 改格数。返回 false 表示没变（调用方据此决定要不要给服务端发 resize）。
    pub fn resize(&self, size: TermSize) -> bool {
        {
            let mut cur = self.size.lock();
            if *cur == size {
                return false;
            }
            *cur = size;
        }
        self.term.lock().resize(size);
        self.mark_dirty();
        true
    }

    pub fn set_cursor_style(&self, shape: CursorShape, blinking: bool) {
        let mut options = self.options.lock();
        options.cursor_shape = shape;
        options.cursor_blinking = blinking;
        let config = term_config(&options);
        self.term.lock().set_options(config);
        self.mark_dirty();
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// 鼠标协议与编码，来自模式跟踪器（口径与 web 的 rio 引擎一致）。
    pub fn mouse_mode(&self) -> MouseMode {
        let parser = self.parser.lock();
        MouseMode {
            protocol: parser.modes.mouse_protocol(),
            encoding: parser.modes.mouse_encoding(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let size = TermSize::new(term.columns(), term.screen_lines());
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let mut cells = vec![Cell::default(); size.cols * size.rows];
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + display_offset as i32;
            if row < 0 || row as usize >= size.rows {
                continue;
            }
            let col = indexed.point.column.0;
            if col >= size.cols {
                continue;
            }
            cells[row as usize * size.cols + col] = indexed.cell.clone();
        }

        let to_view = |p: Point| -> Option<(usize, usize)> {
            let row = p.line.0 + display_offset as i32;
            (row >= 0 && (row as usize) < size.rows).then_some((row as usize, p.column.0))
        };

        let cursor = {
            let c = content.cursor;
            let visible = c.shape != CursorShape::Hidden
                && content.mode.contains(TermMode::SHOW_CURSOR);
            if visible {
                to_view(c.point).map(|(row, col)| {
                    let wide = term.grid()[c.point].flags.contains(Flags::WIDE_CHAR);
                    SnapCursor {
                        row,
                        col,
                        shape: c.shape,
                        wide,
                    }
                })
            } else {
                None
            }
        };

        // 选区可能一端在可见区外：夹到可见区边缘
        let selection = content.selection.and_then(|range| {
            let top = -(display_offset as i32);
            let bottom = top + size.rows as i32 - 1;
            if range.end.line.0 < top || range.start.line.0 > bottom {
                return None;
            }
            let start = if range.start.line.0 < top {
                (0, 0)
            } else {
                to_view(range.start)?
            };
            let end = if range.end.line.0 > bottom {
                (size.rows - 1, size.cols - 1)
            } else {
                to_view(range.end)?
            };
            Some(SnapSelection {
                start,
                end,
                is_block: range.is_block,
            })
        });

        Snapshot {
            size,
            cells,
            cursor,
            selection,
            mode: content.mode,
            display_offset,
            history_size: term.history_size(),
            colors: *content.colors,
            generation: self.generation.load(Ordering::Relaxed),
        }
    }

    // ---------- 选区（本地，不进 PTY）----------

    /// 可见区坐标 → Term 的网格坐标（带历史偏移）。
    fn grid_point(term: &Term<Listener>, row: usize, col: usize) -> Point {
        let offset = term.grid().display_offset() as i32;
        let row = row.min(term.screen_lines().saturating_sub(1));
        let col = col.min(term.columns().saturating_sub(1));
        Point::new(Line(row as i32 - offset), Column(col))
    }

    pub fn start_selection(&self, ty: SelectionType, row: usize, col: usize, side: Side) {
        let mut term = self.term.lock();
        let point = Self::grid_point(&term, row, col);
        term.selection = Some(Selection::new(ty, point, side));
        drop(term);
        self.mark_dirty();
    }

    pub fn update_selection(&self, row: usize, col: usize, side: Side) {
        let mut term = self.term.lock();
        let point = Self::grid_point(&term, row, col);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
        drop(term);
        self.mark_dirty();
    }

    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let top = term.topmost_line();
        let bottom = term.bottommost_line();
        let last = term.last_column();
        let mut sel = Selection::new(SelectionType::Simple, Point::new(top, Column(0)), Side::Left);
        sel.update(Point::new(bottom, last), Side::Right);
        term.selection = Some(sel);
        drop(term);
        self.mark_dirty();
    }

    pub fn clear_selection(&self) {
        let mut term = self.term.lock();
        if term.selection.take().is_some() {
            drop(term);
            self.mark_dirty();
        }
    }

    pub fn has_selection(&self) -> bool {
        self.term
            .lock()
            .selection
            .as_ref()
            .is_some_and(|s| !s.is_empty())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    /// 清屏（右键菜单的"清屏"）：只清本地显示，与 xterm 的 `clear()` 一致，不给程序发任何东西。
    pub fn clear_screen(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let mut term = self.term.lock();
        term.clear_screen(ClearMode::Saved);
        term.clear_screen(ClearMode::All);
        term.goto(0, 0);
        drop(term);
        self.mark_dirty();
    }

    // ---------- 本地 scrollback ----------

    /// 正数朝历史方向。zellij 会话在 alt screen 里没有本地历史，这里只对普通缓冲有效。
    pub fn scroll_display(&self, lines: i32) {
        if lines == 0 {
            return;
        }
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.mark_dirty();
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.mark_dirty();
    }
}

fn flush_expired_sync(processor: &mut Processor<StdSyncHandler>, term: &mut Term<Listener>) -> bool {
    match processor.sync_timeout().sync_timeout() {
        Some(deadline) if deadline <= Instant::now() => {
            processor.stop_sync(term);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_string(s: &Snapshot, row: usize) -> String {
        s.row_text(row).0.trim_end().to_string()
    }

    #[test]
    fn advance_renders_text_and_marks_dirty_once() {
        let core = TermCore::new(TermSize::new(20, 4), TermOptions::default());
        assert!(core.take_dirty());
        assert!(core.advance(b"hello\r\nworld"));
        // 脏位已经是 1：第二段不再要求唤醒
        assert!(!core.advance(b"!"));
        let snap = core.snapshot();
        assert_eq!(row_string(&snap, 0), "hello");
        assert_eq!(row_string(&snap, 1), "world!");
        assert_eq!(snap.cursor.map(|c| (c.row, c.col)), Some((1, 6)));
    }

    #[test]
    fn replay_replaces_state_and_mutes_replies() {
        let core = TermCore::new(TermSize::new(20, 4), TermOptions::default());
        core.advance(b"old content");
        // 回放里带 DA 查询（CSI c）与 DSR（CSI 6n）：静音，不应产生 PtyWrite
        core.replace_with_replay(b"\x1b[c\x1b[6nfresh");
        assert!(core.take_events().is_empty());
        let snap = core.snapshot();
        assert_eq!(row_string(&snap, 0), "fresh");
        assert_eq!(snap.generation, 1);
        // 回放之后的实时输出里的查询照常应答
        core.advance(b"\x1b[6n");
        let events = core.take_events();
        assert!(matches!(events.as_slice(), [TermEvent::PtyWrite(s)] if s.starts_with("\x1b[")));
    }

    #[test]
    fn replay_tracks_modes_from_prefix() {
        let core = TermCore::new(TermSize::new(20, 4), TermOptions::default());
        core.advance(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(core.mouse_mode(), MouseMode { protocol: 1002, encoding: 1006 });
        // 回放是整份快照：模式从回放前缀重新跟踪（服务端把 modes.prefix() 拼在前面）
        core.replace_with_replay(b"\x1b[?1049h\x1b[?1003h\x1b[?1006hscreen");
        assert_eq!(core.mouse_mode(), MouseMode { protocol: 1003, encoding: 1006 });
        assert!(core.mode().contains(TermMode::ALT_SCREEN));
    }

    #[test]
    fn color_queries_are_not_answered() {
        let core = TermCore::new(TermSize::new(20, 4), TermOptions::default());
        core.advance(b"\x1b]11;?\x07\x1b]10;?\x1b\\");
        assert!(core.take_events().is_empty());
    }

    #[test]
    fn osc52_is_write_only() {
        let core = TermCore::new(TermSize::new(20, 4), TermOptions::default());
        // "hi" 的 base64 是 aGk=
        core.advance(b"\x1b]52;c;aGk=\x07\x1b]52;c;?\x07");
        assert_eq!(core.take_events(), vec![TermEvent::ClipboardStore("hi".into())]);
    }

    #[test]
    fn wide_chars_and_row_text() {
        let core = TermCore::new(TermSize::new(10, 2), TermOptions::default());
        core.advance("中文ab".as_bytes());
        let snap = core.snapshot();
        let (text, cols) = snap.row_text(0);
        assert!(text.starts_with("中文ab"));
        assert_eq!(&cols[..4], &[0, 2, 4, 5]);
        assert!(snap.cell(0, 0).flags.contains(Flags::WIDE_CHAR));
    }

    #[test]
    fn resize_reports_change_once() {
        let core = TermCore::new(TermSize::new(10, 2), TermOptions::default());
        assert!(core.resize(TermSize::new(12, 3)));
        assert!(!core.resize(TermSize::new(12, 3)));
        assert_eq!(core.snapshot().size, TermSize::new(12, 3));
    }

    #[test]
    fn selection_roundtrip() {
        let core = TermCore::new(TermSize::new(20, 3), TermOptions::default());
        core.advance(b"hello world");
        core.start_selection(SelectionType::Simple, 0, 0, Side::Left);
        core.update_selection(0, 4, Side::Right);
        assert_eq!(core.selection_text().as_deref(), Some("hello"));
        let snap = core.snapshot();
        assert_eq!(snap.selection.map(|s| (s.start, s.end)), Some(((0, 0), (0, 4))));
        core.clear_selection();
        assert!(!core.has_selection());
    }

    #[test]
    fn hidden_cursor_is_none() {
        let core = TermCore::new(TermSize::new(10, 2), TermOptions::default());
        core.advance(b"\x1b[?25l");
        assert!(core.snapshot().cursor.is_none());
    }
}
