//! 安卓端终端会话内核封装：桥接 [`falcon_term::TermCore`]、[`falcon_theme::ResolvedTheme`] 与
//! [`falcon_proto`] 线上帧协议。
//!
//! 关键协议口径与桌面端（`falcon-ui/src/terminal/`）完全对齐：
//! - 二进制帧经 [`falcon_proto::decode_term_frame`] 解码：`TERM_FRAME_OUTPUT`（0x01）走
//!   [`TermCore::advance`]，`TERM_FRAME_REPLAY`（0x02）走 [`TermCore::replace_with_replay`]；
//! - 每次解析后立即 [`TermCore::take_events`]；持久会话（`durable == true`）丢弃
//!   [`TermEvent::PtyWrite`]（由宿主机 VT 进程一处回答，ADR 0020 决定五），非持久会话转成
//!   `ClientMessage::Input` 发回服务端；
//! - 新 WebSocket 连接先发 `appearance`（带深浅与 `#rrggbb` 前景/背景色），处于后台时再发
//!   `visibility: false`，最后发 `resize`（触发持久会话懒惰接回）；
//! - 多 Viewer 尺寸仲裁（ADR 0022）：`owner == true` 时网格按本机视口格数构建，`owner == false`
//!   时网格按服务端推来的生效尺寸构建，由安卓 Canvas 视图按比例缩放或平移到光标可见区域；
//! - 渲染快照将每个格子的颜色（含 OSC 4/10/11 覆盖、粗体 0–7 亮色、dim 0.5 透明度、反色、选区）
//!   一次性结算为 ARGB 32 位整数数组（16 个头部整数 + 每格 4 个整数），免去逐格跨 JNI 调用。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use falcon_proto::{
    ClientMessage, ServerMessage, TermAppearance, TermFrameKind, WS_MAX_PAYLOAD_BYTES,
    decode_term_frame,
};
use falcon_term::alacritty_terminal::index::Side;
use falcon_term::alacritty_terminal::selection::SelectionType;
use falcon_term::alacritty_terminal::term::TermMode;
use falcon_term::alacritty_terminal::term::cell::Flags;
use falcon_term::alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use falcon_term::{
    DeltaMode, Keystroke, Modifiers, MouseAction, MouseButton, MouseReportEvent,
    MouseReporter, SnapSelection, Snapshot, TermCore, TermEvent, TermOptions, TermSize,
    WheelAccumulator, WheelRoute, arrow_for_wheel, focus_report, route_wheel,
};
use falcon_theme::{
    Appearance, FALCON_DARK, FALCON_LIGHT, Rgb, ResolvedTheme, derive_theme, find_theme,
    load_catalog,
};
use parking_lot::Mutex;
use serde::Serialize;

/// 快照头部固定长度（16 个 i32）
pub const SNAPSHOT_HEADER_INTS: usize = 16;
/// 每个网格单元占用的 i32 数量：`[codepoint, fg_argb, bg_argb, flags]`
pub const SNAPSHOT_CELL_INTS: usize = 4;

pub const CELL_FLAG_BOLD: i32 = 1 << 0;
pub const CELL_FLAG_ITALIC: i32 = 1 << 1;
pub const CELL_FLAG_UNDERLINE: i32 = 1 << 2;
pub const CELL_FLAG_STRIKEOUT: i32 = 1 << 3;
pub const CELL_FLAG_WIDE_CHAR: i32 = 1 << 4;
pub const CELL_FLAG_WIDE_CHAR_SPACER: i32 = 1 << 5;
pub const CELL_FLAG_SELECTED: i32 = 1 << 6;
pub const CELL_FLAG_DEFAULT_BG: i32 = 1 << 7;
pub const CELL_FLAG_HIDDEN: i32 = 1 << 8;

pub const MODE_BIT_ALT_SCREEN: i32 = 1 << 0;
pub const MODE_BIT_APP_CURSOR: i32 = 1 << 1;
pub const MODE_BIT_BRACKETED_PASTE: i32 = 1 << 2;
pub const MODE_BIT_FOCUS_IN_OUT: i32 = 1 << 3;
pub const MODE_BIT_MOUSE_ACTIVE: i32 = 1 << 4;

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<i64, Arc<AndroidTermSession>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<i64, Arc<AndroidTermSession>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerGridSize {
    pub cols: u16,
    pub rows: u16,
    pub owner: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FrameEffect {
    /// 需要经 WebSocket 发出的客户端控制帧 JSON 字符串（如非持久会话的 DA/DSR 应答）
    SendWs { json: String },
    /// OSC 52 写入系统剪贴板
    Clipboard { text: String },
    /// OSC 0/2 终端标题更新
    OscTitle { title: Option<String> },
    /// 终端响铃
    Bell,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrameProcessResult {
    pub dirty: bool,
    pub effects: Vec<FrameEffect>,
}

pub struct AndroidTermSession {
    pub core: TermCore,
    pub durable: AtomicBool,
    pub theme: Mutex<ResolvedTheme>,
    pub wheel: Mutex<WheelAccumulator>,
    pub mouse: Mutex<MouseReporter>,
    /// 本机终端视图按当前字号与视口能放下的格数（报给服务端的 resize 值）
    pub local_size: Mutex<(u16, u16)>,
    /// 服务端最近一次广播的 PTY 生效尺寸与 owner 标记（ADR 0022）
    pub server_size: Mutex<Option<ServerGridSize>>,
}

impl AndroidTermSession {
    pub fn new(cols: u16, rows: u16, durable: bool, theme_name: &str) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let theme = resolve_theme_by_name(theme_name);
        Self {
            core: TermCore::new(TermSize::new(cols as usize, rows as usize), TermOptions::default()),
            durable: AtomicBool::new(durable),
            theme: Mutex::new(theme),
            wheel: Mutex::new(WheelAccumulator::new()),
            mouse: Mutex::new(MouseReporter::new()),
            local_size: Mutex::new((cols, rows)),
            server_size: Mutex::new(None),
        }
    }

    pub fn set_durable(&self, durable: bool) {
        self.durable.store(durable, Ordering::Release);
    }

    pub fn set_theme(&self, theme_name: &str) -> String {
        let resolved = resolve_theme_by_name(theme_name);
        let msg = appearance_message(&resolved);
        *self.theme.lock() = resolved;
        serde_json::to_string(&msg).unwrap_or_default()
    }

    /// 新连接建立时的标准握手消息序列：
    /// 1. `appearance`（必须在 `resize` 之前，供接回瞬间的颜色查询使用）
    /// 2. 窗口不可见时发 `visibility: false`
    /// 3. `resize`（触发服务端持久会话懒惰接回）
    pub fn handshake_messages(&self, cols: u16, rows: u16, visible: bool) -> Vec<String> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        *self.local_size.lock() = (cols, rows);
        let theme = self.theme.lock().clone();
        let mut out = Vec::with_capacity(3);
        if let Ok(s) = serde_json::to_string(&appearance_message(&theme)) {
            out.push(s);
        }
        if !visible
            && let Ok(s) = serde_json::to_string(&ClientMessage::Visibility { visible: false })
        {
            out.push(s);
        }
        if let Ok(s) = serde_json::to_string(&ClientMessage::Resize { cols, rows }) {
            out.push(s);
        }
        out
    }

    /// 视口尺寸变化：更新本机可容纳格数，若当前是尺寸定夺方（或尚未收到服务端 `size`），立即调整本地网格。
    /// 若本机格数发生变化，返回需发送给服务端的 `resize` JSON 消息。
    pub fn on_viewport_resize(&self, cols: u16, rows: u16) -> Option<String> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let changed = {
            let mut local = self.local_size.lock();
            if *local == (cols, rows) {
                false
            } else {
                *local = (cols, rows);
                true
            }
        };
        let is_owner = self
            .server_size
            .lock()
            .map(|s| s.owner)
            .unwrap_or(true);
        if is_owner {
            self.core.resize(TermSize::new(cols as usize, rows as usize));
        }
        if changed {
            serde_json::to_string(&ClientMessage::Resize { cols, rows }).ok()
        } else {
            None
        }
    }

    /// 收到服务端的 `ServerMessage::Size`（ADR 0022）：
    /// - `owner == true`：按本机视口格数构建网格；
    /// - `owner == false`：按服务端推来的生效尺寸构建网格，不在本地强行重排。
    pub fn on_server_size(&self, cols: u16, rows: u16, owner: bool) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        *self.server_size.lock() = Some(ServerGridSize { cols, rows, owner });
        let (target_cols, target_rows) = if owner {
            *self.local_size.lock()
        } else {
            (cols, rows)
        };
        self.core
            .resize(TermSize::new(target_cols as usize, target_rows as usize));
    }

    /// 处理一条来自 `/ws/sessions/:id` 的二进制帧（`TERM_FRAME_OUTPUT` / `TERM_FRAME_REPLAY`）。
    pub fn on_binary_frame(&self, frame: &[u8]) -> FrameProcessResult {
        let Some(decoded) = decode_term_frame(frame) else {
            return FrameProcessResult {
                dirty: false,
                effects: Vec::new(),
            };
        };
        match decoded.kind {
            TermFrameKind::Output => {
                self.core.advance(decoded.payload);
            }
            TermFrameKind::Replay => {
                self.core.replace_with_replay(decoded.payload);
            }
        }
        let dirty = true;
        let events = self.core.take_events();
        let is_durable = self.durable.load(Ordering::Acquire);
        let mut effects = Vec::new();
        for ev in events {
            match ev {
                TermEvent::PtyWrite(reply) => {
                    // ADR 0020 决定五：持久会话的终端查询由宿主机 VT 进程统一回答，客户端不答
                    if !is_durable {
                        for json in encode_input_chunks(&reply) {
                            effects.push(FrameEffect::SendWs { json });
                        }
                    }
                }
                TermEvent::ClipboardStore(text) => {
                    effects.push(FrameEffect::Clipboard { text });
                }
                TermEvent::Title(title) => {
                    effects.push(FrameEffect::OscTitle { title });
                }
                TermEvent::Bell => {
                    effects.push(FrameEffect::Bell);
                }
            }
        }
        FrameProcessResult { dirty, effects }
    }

    /// 键盘按键或扩展栏按键 → 编码为 `ClientMessage::Input` JSON 列表。
    pub fn encode_key(&self, key: &str, ctrl: bool, alt: bool, shift: bool) -> Vec<String> {
        self.core.scroll_to_bottom();
        let mode = self.core.mode();
        let normalized_key = key.to_lowercase();
        let ks = Keystroke {
            modifiers: Modifiers {
                control: ctrl,
                alt,
                shift,
                platform: false,
                function: false,
            },
            key: normalized_key.clone(),
            key_char: if !ctrl && !alt && key.chars().count() == 1 {
                Some(key.to_string())
            } else {
                None
            },
        };
        if let Some(esc) = falcon_term::keys::to_esc_str(&ks, mode, true) {
            return encode_input_chunks(&esc);
        }
        // 单字符修饰兜底（例如软键盘或扩展栏组合 Ctrl / Alt + 字符）
        if key.chars().count() == 1 {
            let ch = key.chars().next().unwrap();
            if ctrl {
                let upper = ch.to_ascii_uppercase();
                if ('@'..='_').contains(&upper) {
                    let ctl = ((upper as u8) & 0x1f) as char;
                    let s = if alt {
                        format!("\x1b{ctl}")
                    } else {
                        ctl.to_string()
                    };
                    return encode_input_chunks(&s);
                }
            }
            if alt {
                return encode_input_chunks(&format!("\x1b{ch}"));
            }
            return encode_input_chunks(key);
        }
        Vec::new()
    }

    /// 输入法上屏文本或直接输入文本 → `ClientMessage::Input` JSON 列表。
    pub fn encode_text(&self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        self.core.scroll_to_bottom();
        encode_input_chunks(text)
    }

    /// 粘贴文本（自动检测 `?2004h` Bracketed Paste 模式并消毒 ESC）→ `ClientMessage::Input` JSON 列表。
    pub fn encode_paste(&self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        self.core.scroll_to_bottom();
        let bracketed = self.core.mode().contains(TermMode::BRACKETED_PASTE);
        let encoded = falcon_term::paste::encode_paste(text, bracketed);
        encode_input_chunks(&encoded)
    }

    /// 焦点变化时若开启了 `?1004h`（FOCUS_IN_OUT），返回焦点上报的 `ClientMessage::Input` JSON。
    pub fn on_focus_change(&self, focused: bool) -> Option<String> {
        if self.core.mode().contains(TermMode::FOCUS_IN_OUT) {
            serde_json::to_string(&ClientMessage::Input {
                data: focus_report(focused).to_string(),
            })
            .ok()
        } else {
            None
        }
    }

    /// 触控滑动手势：按 [`route_wheel`] 决定走鼠标滚轮上报、备用屏方向键还是本地历史回滚。
    /// 返回需要发给服务端的 `ClientMessage::Input` JSON（若为本地回滚则返回 `None`）。
    pub fn on_scroll_gesture(
        &self,
        delta_y_pixels: f64,
        cell_height: f64,
        row: usize,
        col: usize,
    ) -> Option<String> {
        let mode = self.core.mode();
        let mouse = self.core.mouse_mode();
        let size = self.core.size();
        match route_wheel(mode, mouse.protocol, false) {
            WheelRoute::Report => {
                let dir = self
                    .wheel
                    .lock()
                    .click(DeltaMode::Pixel, delta_y_pixels, cell_height, size.rows);
                if dir != 0 {
                    let bytes = wheel_report(dir > 0, row, col, mouse.encoding)?;
                    return serde_json::to_string(&ClientMessage::Input { data: bytes }).ok();
                }
                None
            }
            WheelRoute::Arrows { app_cursor } => {
                let dir = self
                    .wheel
                    .lock()
                    .click(DeltaMode::Pixel, delta_y_pixels, cell_height, size.rows);
                if dir != 0 {
                    let seq = arrow_for_wheel(dir > 0, app_cursor);
                    return serde_json::to_string(&ClientMessage::Input {
                        data: seq.to_string(),
                    })
                    .ok();
                }
                None
            }
            WheelRoute::Scrollback => {
                let lines = self
                    .wheel
                    .lock()
                    .push(DeltaMode::Pixel, delta_y_pixels, cell_height, size.rows);
                if lines != 0 {
                    self.core.scroll_display(lines);
                }
                None
            }
        }
    }

    /// 鼠标点击 / 拖动上报（当 TUI 开启鼠标模式时）。
    /// `action`: `0 = Down`, `1 = Move`, `2 = Up`。
    pub fn on_mouse_event(
        &self,
        action: i32,
        row: i32,
        col: i32,
        pixel_x: i32,
        pixel_y: i32,
    ) -> Option<String> {
        let mouse_mode = self.core.mouse_mode();
        if mouse_mode.protocol == 0 {
            return None;
        }
        let act = match action {
            0 => MouseAction::Down,
            1 => MouseAction::Move,
            2 => MouseAction::Up,
            _ => return None,
        };
        let size = self.core.size();
        let ev = MouseReportEvent {
            action: act,
            button: MouseButton::Left,
            col,
            row,
            x: pixel_x,
            y: pixel_y,
            shift: false,
            alt: false,
            ctrl: false,
        };
        let seq = self
            .mouse
            .lock()
            .report(ev, mouse_mode, (size.cols, size.rows))?;
        serde_json::to_string(&ClientMessage::Input { data: seq }).ok()
    }

    /// 本地选区操作：`kind`: `0 = Simple`, `1 = Semantic(词)`, `2 = Lines(整行)`。
    pub fn start_selection(&self, kind: i32, row: usize, col: usize, right_half: bool) {
        let ty = match kind {
            1 => SelectionType::Semantic,
            2 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let side = if right_half { Side::Right } else { Side::Left };
        self.core.start_selection(ty, row, col, side);
    }

    pub fn update_selection(&self, row: usize, col: usize, right_half: bool) {
        let side = if right_half { Side::Right } else { Side::Left };
        self.core.update_selection(row, col, side);
    }

    pub fn clear_selection(&self) {
        self.core.clear_selection();
    }

    pub fn selection_text(&self) -> Option<String> {
        self.core.selection_text()
    }

    /// 导出紧凑的整帧渲染快照：
    /// 前 16 个 `i32` 为头部元数据，后续 `rows * cols * 4` 个 `i32` 为按行优先排列的单元格数据。
    pub fn pack_render_snapshot(&self) -> Vec<i32> {
        let _ = self.core.flush_sync_if_expired();
        let _ = self.core.take_dirty();
        let snap = self.core.snapshot();
        let theme = self.theme.lock().clone();
        let cols = snap.size.cols;
        let rows = snap.size.rows;
        let mut out = Vec::with_capacity(SNAPSHOT_HEADER_INTS + rows * cols * SNAPSHOT_CELL_INTS);

        let (cursor_row, cursor_col, cursor_shape, cursor_wide) = match snap.cursor {
            Some(c) => {
                let shape_code = match c.shape {
                    CursorShape::Block => 0,
                    CursorShape::Underline => 1,
                    CursorShape::Beam => 2,
                    CursorShape::HollowBlock => 3,
                    CursorShape::Hidden => -1,
                };
                (c.row as i32, c.col as i32, shape_code, i32::from(c.wide))
            }
            None => (-1, -1, -1, 0),
        };

        let mouse_mode = self.core.mouse_mode();
        let mut mode_bits = 0i32;
        if snap.mode.contains(TermMode::ALT_SCREEN) {
            mode_bits |= MODE_BIT_ALT_SCREEN;
        }
        if snap.mode.contains(TermMode::APP_CURSOR) {
            mode_bits |= MODE_BIT_APP_CURSOR;
        }
        if snap.mode.contains(TermMode::BRACKETED_PASTE) {
            mode_bits |= MODE_BIT_BRACKETED_PASTE;
        }
        if snap.mode.contains(TermMode::FOCUS_IN_OUT) {
            mode_bits |= MODE_BIT_FOCUS_IN_OUT;
        }
        if mouse_mode.protocol != 0 {
            mode_bits |= MODE_BIT_MOUSE_ACTIVE;
        }

        let is_owner = self
            .server_size
            .lock()
            .map(|s| s.owner)
            .unwrap_or(true);

        let default_bg = resolve_named_color(&snap, &theme, NamedColor::Background, false);
        let default_fg = resolve_named_color(&snap, &theme, NamedColor::Foreground, true);
        let cursor_argb = resolve_named_color(&snap, &theme, NamedColor::Cursor, true);
        let cursor_text_argb = rgb_to_argb(theme.terminal.cursor_accent, 255);
        let selection_bg_argb = rgb_to_argb(theme.terminal.selection_background, 255);

        // Header (16 ints)
        out.push(cols as i32); // 0
        out.push(rows as i32); // 1
        out.push(cursor_row); // 2
        out.push(cursor_col); // 3
        out.push(cursor_shape); // 4
        out.push(cursor_wide); // 5
        out.push(snap.display_offset as i32); // 6
        out.push(snap.history_size as i32); // 7
        out.push(mode_bits); // 8
        out.push(default_bg); // 9
        out.push(default_fg); // 10
        out.push(cursor_argb); // 11
        out.push(cursor_text_argb); // 12
        out.push(selection_bg_argb); // 13
        out.push(i32::from(is_owner)); // 14
        out.push(snap.generation as i32); // 15

        // Grid cells
        for row in 0..rows {
            for col in 0..cols {
                let cell = snap.cell(row, col);
                let selected = snap
                    .selection
                    .as_ref()
                    .is_some_and(|s| in_selection(s, row, col, cols));

                let mut fg_color = cell.fg;
                let mut bg_color = cell.bg;

                // xterm drawBoldTextInBrightColors：粗体 + ANSI 0–7 前景画成对应亮色
                if cell.flags.contains(Flags::BOLD)
                    && let Color::Named(n) = fg_color
                    && (n as usize) < 8
                {
                    fg_color = Color::Indexed(n as u8 + 8);
                }

                let inverse = cell.flags.contains(Flags::INVERSE);
                if inverse {
                    std::mem::swap(&mut fg_color, &mut bg_color);
                }

                let is_default_bg =
                    !inverse && !selected && matches!(bg_color, Color::Named(NamedColor::Background));

                let mut fg_argb = resolve_color(&snap, &theme, &fg_color, true);
                let mut bg_argb = resolve_color(&snap, &theme, &bg_color, false);

                if cell.flags.contains(Flags::DIM) {
                    fg_argb = apply_half_alpha(fg_argb);
                }

                if selected {
                    bg_argb = selection_bg_argb;
                    if let Some(sel_fg) = theme.terminal.selection_foreground {
                        fg_argb = rgb_to_argb(sel_fg, 255);
                    }
                }

                let mut flags = 0i32;
                if cell.flags.contains(Flags::BOLD) {
                    flags |= CELL_FLAG_BOLD;
                }
                if cell.flags.contains(Flags::ITALIC) {
                    flags |= CELL_FLAG_ITALIC;
                }
                if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                    flags |= CELL_FLAG_UNDERLINE;
                }
                if cell.flags.contains(Flags::STRIKEOUT) {
                    flags |= CELL_FLAG_STRIKEOUT;
                }
                if cell.flags.contains(Flags::WIDE_CHAR) {
                    flags |= CELL_FLAG_WIDE_CHAR;
                }
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    flags |= CELL_FLAG_WIDE_CHAR_SPACER;
                }
                if selected {
                    flags |= CELL_FLAG_SELECTED;
                }
                if is_default_bg {
                    flags |= CELL_FLAG_DEFAULT_BG;
                }
                if cell.flags.contains(Flags::HIDDEN) {
                    flags |= CELL_FLAG_HIDDEN;
                }

                out.push(cell.c as i32);
                out.push(fg_argb);
                out.push(bg_argb);
                out.push(flags);
            }
        }

        out
    }
}

pub fn create_session(cols: u16, rows: u16, durable: bool, theme_name: &str) -> i64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let session = Arc::new(AndroidTermSession::new(cols, rows, durable, theme_name));
    registry().lock().insert(handle, session);
    handle
}

pub fn get_session(handle: i64) -> Option<Arc<AndroidTermSession>> {
    registry().lock().get(&handle).cloned()
}

pub fn destroy_session(handle: i64) {
    registry().lock().remove(&handle);
}

pub fn resolve_theme_by_name(name: &str) -> ResolvedTheme {
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("Falcon Light") {
        return derive_theme(&FALCON_LIGHT.colors);
    }
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Falcon Dark") {
        return derive_theme(&FALCON_DARK.colors);
    }
    if let Some(entry) = find_theme(load_catalog(), trimmed) {
        return derive_theme(&entry.colors);
    }
    derive_theme(&FALCON_DARK.colors)
}

fn appearance_message(theme: &ResolvedTheme) -> ClientMessage {
    let appearance = match theme.hint.appearance {
        Appearance::Light => TermAppearance::Light,
        Appearance::Dark => TermAppearance::Dark,
    };
    ClientMessage::Appearance {
        appearance,
        background: Some(theme.hint.background.to_hex()),
        foreground: Some(theme.hint.foreground.to_hex()),
    }
}

/// 将输入拆分为不超过 [`WS_MAX_PAYLOAD_BYTES`] 的 `ClientMessage::Input` JSON 帧。
pub fn encode_input_chunks(data: &str) -> Vec<String> {
    if data.is_empty() {
        return Vec::new();
    }
    // 预留足够的转义膨胀空间（控制字符 `\u001b` 会膨胀 6 倍）
    const MAX_CHUNK_BYTES: usize = WS_MAX_PAYLOAD_BYTES / 8;
    if data.len() <= MAX_CHUNK_BYTES {
        return serde_json::to_string(&ClientMessage::Input {
            data: data.to_string(),
        })
        .into_iter()
        .collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let mut end = (start + MAX_CHUNK_BYTES).min(data.len());
        while !data.is_char_boundary(end) {
            end -= 1;
        }
        if let Ok(json) = serde_json::to_string(&ClientMessage::Input {
            data: data[start..end].to_string(),
        }) {
            out.push(json);
        }
        start = end;
    }
    out
}

fn wheel_report(up: bool, row: usize, col: usize, encoding: u16) -> Option<String> {
    let code: u32 = if up { 64 } else { 65 };
    if encoding == 1006 || encoding == 1016 {
        return Some(format!("\x1b[<{code};{};{}M", col + 1, row + 1));
    }
    let (b, x, y) = (code + 32, col as u32 + 1 + 32, row as u32 + 1 + 32);
    if b > 0x7f || x > 0x7f || y > 0x7f {
        return None;
    }
    Some(format!(
        "\x1b[M{}{}{}",
        char::from_u32(b)?,
        char::from_u32(x)?,
        char::from_u32(y)?
    ))
}

fn rgb_to_argb(rgb: Rgb, alpha: u8) -> i32 {
    ((alpha as u32) << 24 | (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | (rgb.b as u32)) as i32
}

fn apply_half_alpha(argb: i32) -> i32 {
    let u = argb as u32;
    let a = ((u >> 24) & 0xff) / 2;
    ((a << 24) | (u & 0x00ff_ffff)) as i32
}

/// xterm 256 色表里 16–255 的部分：6×6×6 色立方 + 24 级灰（与 falcon-ui/src/terminal/element.rs 同口径）。
fn indexed_rgb(index: u8) -> Rgb {
    if index >= 232 {
        let v = 8 + (index - 232) * 10;
        return Rgb::new(v, v, v);
    }
    let i = index - 16;
    let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    Rgb::new(step(i / 36), step((i / 6) % 6), step(i % 6))
}

fn resolve_overridden(snap: &Snapshot, index: usize) -> Option<i32> {
    snap.colors[index].map(|c| rgb_to_argb(Rgb::new(c.r, c.g, c.b), 255))
}

fn resolve_color(snap: &Snapshot, theme: &ResolvedTheme, color: &Color, is_fg: bool) -> i32 {
    match color {
        Color::Spec(rgb) => rgb_to_argb(Rgb::new(rgb.r, rgb.g, rgb.b), 255),
        Color::Indexed(i) => resolve_indexed_color(snap, theme, *i),
        Color::Named(named) => resolve_named_color(snap, theme, *named, is_fg),
    }
}

fn resolve_indexed_color(snap: &Snapshot, theme: &ResolvedTheme, i: u8) -> i32 {
    if let Some(c) = resolve_overridden(snap, i as usize) {
        return c;
    }
    if i < 16 {
        return rgb_to_argb(theme.terminal.ansi[i as usize], 255);
    }
    if let Some(c) = theme
        .terminal
        .extended_ansi
        .as_ref()
        .and_then(|ext| ext.get(i as usize - 16))
    {
        return rgb_to_argb(*c, 255);
    }
    rgb_to_argb(indexed_rgb(i), 255)
}

fn resolve_named_color(
    snap: &Snapshot,
    theme: &ResolvedTheme,
    named: NamedColor,
    is_fg: bool,
) -> i32 {
    let index = named as usize;
    if let Some(c) = resolve_overridden(snap, index) {
        return c;
    }
    match named {
        NamedColor::Foreground | NamedColor::BrightForeground => {
            rgb_to_argb(theme.terminal.foreground, 255)
        }
        NamedColor::DimForeground => rgb_to_argb(theme.terminal.foreground, 128),
        NamedColor::Background => rgb_to_argb(theme.terminal.background, 255),
        NamedColor::Cursor => rgb_to_argb(theme.terminal.cursor, 255),
        n if (n as usize) < 16 => rgb_to_argb(theme.terminal.ansi[n as usize], 255),
        n => {
            let base = (n as usize).saturating_sub(NamedColor::DimBlack as usize) % 8;
            let rgb = theme.terminal.ansi[base];
            rgb_to_argb(rgb, if is_fg { 128 } else { 255 })
        }
    }
}

fn in_selection(sel: &SnapSelection, row: usize, col: usize, cols: usize) -> bool {
    let (sr, sc) = sel.start;
    let (er, ec) = sel.end;
    if row < sr || row > er {
        return false;
    }
    if sel.is_block {
        let (lo, hi) = (sc.min(ec), sc.max(ec));
        return col >= lo && col <= hi;
    }
    let from = if row == sr { sc } else { 0 };
    let to = if row == er { ec } else { cols.saturating_sub(1) };
    col >= from && col <= to
}

/// 解析一条服务端发来的控制帧 JSON（[`ServerMessage`]），并附带计算好的程序状态徽标。
pub fn parse_server_control_message(raw_json: &str) -> String {
    let Ok(msg) = serde_json::from_str::<ServerMessage>(raw_json) else {
        return serde_json::json!({ "kind": "invalid" }).to_string();
    };
    match msg {
        ServerMessage::State { state, dead_reason } => serde_json::json!({
            "kind": "state",
            "state": state.as_str(),
            "deadReason": dead_reason.map(|r| r.as_str()),
        })
        .to_string(),
        ServerMessage::Reconnecting { attempt } => serde_json::json!({
            "kind": "reconnecting",
            "attempt": attempt,
        })
        .to_string(),
        ServerMessage::Error { message } => serde_json::json!({
            "kind": "error",
            "message": message,
        })
        .to_string(),
        ServerMessage::Title { title } => serde_json::json!({
            "kind": "title",
            "title": title,
        })
        .to_string(),
        ServerMessage::Askpass { id, prompt } => serde_json::json!({
            "kind": "askpass",
            "id": id,
            "prompt": prompt,
        })
        .to_string(),
        ServerMessage::ProgramStatus { records } => {
            let summary = falcon_proto::summarize(&records);
            let badge = summary
                .as_ref()
                .and_then(crate::dashboard::badge_to_json);
            serde_json::json!({
                "kind": "programStatus",
                "badge": badge,
            })
            .to_string()
        }
        ServerMessage::Pong => serde_json::json!({ "kind": "pong" }).to_string(),
        ServerMessage::Size { cols, rows, owner } => serde_json::json!({
            "kind": "size",
            "cols": cols,
            "rows": rows,
            "owner": owner,
        })
        .to_string(),
        ServerMessage::Unknown => serde_json::json!({ "kind": "unknown" }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::encode_term_frame;

    #[test]
    fn handshake_sends_appearance_then_visibility_then_resize() {
        let session = AndroidTermSession::new(80, 24, true, "Falcon Dark");
        let msgs = session.handshake_messages(80, 24, false);
        assert_eq!(msgs.len(), 3);
        let m0: ClientMessage = serde_json::from_str(&msgs[0]).unwrap();
        let m1: ClientMessage = serde_json::from_str(&msgs[1]).unwrap();
        let m2: ClientMessage = serde_json::from_str(&msgs[2]).unwrap();
        assert!(matches!(m0, ClientMessage::Appearance { appearance: TermAppearance::Dark, .. }));
        assert_eq!(m1, ClientMessage::Visibility { visible: false });
        assert_eq!(m2, ClientMessage::Resize { cols: 80, rows: 24 });
    }

    #[test]
    fn durable_session_suppresses_pty_write_while_non_durable_emits_it() {
        let durable = AndroidTermSession::new(80, 24, true, "Falcon Dark");
        let frame = encode_term_frame(TermFrameKind::Output, b"\x1b[6nhello");
        let res = durable.on_binary_frame(&frame);
        assert!(res.dirty);
        assert!(res.effects.is_empty(), "持久会话不应由客户端回答 DSR");

        let non_durable = AndroidTermSession::new(80, 24, false, "Falcon Dark");
        let res2 = non_durable.on_binary_frame(&frame);
        assert!(res2.dirty);
        assert!(
            res2.effects
                .iter()
                .any(|e| matches!(e, FrameEffect::SendWs { json } if json.contains("input"))),
            "非持久会话应由客户端回答 DSR"
        );
    }

    #[test]
    fn size_arbitration_follows_server_when_not_owner() {
        let session = AndroidTermSession::new(80, 24, true, "Falcon Dark");
        session.on_server_size(120, 40, false);
        let snap = session.pack_render_snapshot();
        assert_eq!(snap[0], 120);
        assert_eq!(snap[1], 40);
        assert_eq!(snap[14], 0); // is_size_owner == false

        // 成为 owner 后回到本机视口尺寸
        session.on_server_size(80, 24, true);
        let snap2 = session.pack_render_snapshot();
        assert_eq!(snap2[0], 80);
        assert_eq!(snap2[1], 24);
        assert_eq!(snap2[14], 1);
    }

    #[test]
    fn packed_snapshot_encodes_bold_bright_and_characters() {
        let session = AndroidTermSession::new(10, 2, true, "Falcon Dark");
        let frame = encode_term_frame(TermFrameKind::Output, b"\x1b[1;31mA\x1b[0mB");
        session.on_binary_frame(&frame);
        let packed = session.pack_render_snapshot();
        assert_eq!(packed.len(), SNAPSHOT_HEADER_INTS + 10 * 2 * SNAPSHOT_CELL_INTS);
        let c0_cp = packed[SNAPSHOT_HEADER_INTS];
        let c0_flags = packed[SNAPSHOT_HEADER_INTS + 3];
        assert_eq!(c0_cp, 'A' as i32);
        assert_ne!(c0_flags & CELL_FLAG_BOLD, 0);
        let c1_cp = packed[SNAPSHOT_HEADER_INTS + 4];
        assert_eq!(c1_cp, 'B' as i32);
    }

    #[test]
    fn key_and_paste_encoding_respect_terminal_modes() {
        let session = AndroidTermSession::new(80, 24, true, "Falcon Dark");
        let up_normal = session.encode_key("up", false, false, false);
        assert_eq!(up_normal, vec![r#"{"type":"input","data":"\u001b[A"}"#]);

        // 开启 DECCKM (?1h) 与 Bracketed Paste (?2004h)
        let mode_frame = encode_term_frame(TermFrameKind::Output, b"\x1b[?1h\x1b[?2004h");
        session.on_binary_frame(&mode_frame);
        let up_app = session.encode_key("up", false, false, false);
        assert_eq!(up_app, vec![r#"{"type":"input","data":"\u001bOA"}"#]);

        let pasted = session.encode_paste("echo hi\n");
        assert_eq!(
            pasted,
            vec![r#"{"type":"input","data":"\u001b[200~echo hi\r\u001b[201~"}"#]
        );
    }
}
