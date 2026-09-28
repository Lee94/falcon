//! 鼠标按键上报：鼠标事件 → 发给程序的 VT 报文。`packages/web/src/lib/rio/mouse.ts` 的移植，
//! 纯函数、零 GPUI。
//!
//! 为什么不抄 Zed 的 `mappings/mouse.rs`：web 的两个终端引擎（xterm / rio）已经按 xterm.js
//! 的口径（MouseStateService 的协议筛选 / 编码 + MouseService 的移动去重）对齐过，原生客户端
//! 与它们发出同样的字节，排查时才能互相对照。协议与编码从 [`crate::modes::TermModeTracker`]
//! 读——alacritty 的 TermMode 分不出 ?9 也不认 ?1016。
//!
//! - 协议决定哪些事件出门：?9 只报按下且抹掉修饰键；?1000 报按下 / 松开；?1002 再加按住时的
//!   拖动；?1003 连无按键移动也报。移动报文按格（?1016 按像素）去重。
//! - 编码决定报文形状：默认是 CSI M 加三个 (值+32) 的单字节，松开一律报 3（无按键）；
//!   ?1006 SGR 是 CSI < b;x;y M / m，松开能带按键号；?1016 同 SGR 但坐标是像素。
//! - 默认编码超过 ASCII 的报文丢掉：往后端的输入通道是 JSON 文本帧，0x80 以上的字节会被当成
//!   码点 UTF-8 编码成两字节，程序按单字节解析必错。现代 TUI 都开 ?1006，实际只影响不开 SGR
//!   的老程序在第 96 列 / 行之后的点击。
//!
//! 滚轮不在这里，见 [`crate::wheel`]。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Down,
    Up,
    Move,
}

/// 0 左 / 1 中 / 2 右 / 3 无按键（只在 ?1003 的移动里出现）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    None = 3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseReportEvent {
    pub action: MouseAction,
    pub button: MouseButton,
    /// 0 起的格坐标；报文里 +1
    pub col: i32,
    pub row: i32,
    /// 终端左上角起的逻辑像素坐标，只有 ?1016 用；调用方先夹到画布内
    pub x: i32,
    pub y: i32,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MouseMode {
    /// 0 / 9 / 1000 / 1002 / 1003，见 TermModeTracker::mouse_protocol
    pub protocol: u16,
    /// 0 / 1006 / 1016，见 TermModeTracker::mouse_encoding
    pub encoding: u16,
}

const MOD_SHIFT: u32 = 4;
const MOD_ALT: u32 = 8;
const MOD_CTRL: u32 = 16;
/// 移动报文在按键号上加的位
const MOTION: u32 = 32;
const BUTTON_NONE: u32 = 3;
/// 默认编码把值 +32 塞进单字节；文本通道发不出 0x80 以上的字节，见文件头
const DEFAULT_MAX_BYTE: u32 = 0x7f;

#[derive(Default, Debug)]
pub struct MouseReporter {
    /// 最近一条通过筛选的事件，移动报文按它去重（xterm.js 的 _lastEvent）
    last: Option<MouseReportEvent>,
}

impl MouseReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `grid` 是当前格数（cols, rows），格坐标出界的事件丢掉。
    /// 返回要发给程序的报文；协议不要这个事件、与上一条移动重复或编码不了时为 None。
    pub fn report(
        &mut self,
        e: MouseReportEvent,
        mode: MouseMode,
        grid: (usize, usize),
    ) -> Option<String> {
        let (cols, rows) = (grid.0 as i32, grid.1 as i32);
        if e.col < 0 || e.col >= cols || e.row < 0 || e.row >= rows {
            return None;
        }
        // 无按键只有移动一种形态
        if e.button == MouseButton::None && e.action != MouseAction::Move {
            return None;
        }
        let ev = restrict(e, mode.protocol)?;
        if ev.action == MouseAction::Move
            && self
                .last
                .is_some_and(|last| same_event(&last, &ev, mode.encoding == 1016))
        {
            return None;
        }
        self.last = Some(ev);
        encode(&ev, mode.encoding)
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// 协议筛选（xterm.js 的 DEFAULT_PROTOCOLS[*].restrict）；返回可能被改写的副本
fn restrict(e: MouseReportEvent, protocol: u16) -> Option<MouseReportEvent> {
    match protocol {
        // X10：只报按下，没有修饰键
        9 => (e.action == MouseAction::Down).then_some(MouseReportEvent {
            shift: false,
            alt: false,
            ctrl: false,
            ..e
        }),
        1000 => (e.action != MouseAction::Move).then_some(e),
        // 只报按住时的拖动
        1002 => (!(e.action == MouseAction::Move && e.button == MouseButton::None)).then_some(e),
        1003 => Some(e),
        _ => None,
    }
}

fn same_event(a: &MouseReportEvent, b: &MouseReportEvent, pixels: bool) -> bool {
    let same_pos = if pixels {
        a.x == b.x && a.y == b.y
    } else {
        a.col == b.col && a.row == b.row
    };
    same_pos
        && a.button == b.button
        && a.action == b.action
        && a.shift == b.shift
        && a.alt == b.alt
        && a.ctrl == b.ctrl
}

fn event_code(e: &MouseReportEvent, sgr: bool) -> u32 {
    let mut code = if e.ctrl { MOD_CTRL } else { 0 }
        | if e.shift { MOD_SHIFT } else { 0 }
        | if e.alt { MOD_ALT } else { 0 }
        | (e.button as u32 & 3);
    if e.action == MouseAction::Move {
        code |= MOTION;
    } else if e.action == MouseAction::Up && !sgr {
        // 只有 SGR 能在松开时报按键号，其余编码一律报"无按键"
        code |= BUTTON_NONE;
    }
    code
}

fn encode(e: &MouseReportEvent, encoding: u16) -> Option<String> {
    if encoding == 1006 || encoding == 1016 {
        let fin = if e.action == MouseAction::Up { 'm' } else { 'M' };
        let (px, py) = if encoding == 1016 {
            (e.x, e.y)
        } else {
            (e.col + 1, e.row + 1)
        };
        return Some(format!("\x1b[<{};{px};{py}{fin}", event_code(e, true)));
    }
    let b = event_code(e, false) + 32;
    let x = (e.col + 1 + 32) as u32;
    let y = (e.row + 1 + 32) as u32;
    if b > DEFAULT_MAX_BYTE || x > DEFAULT_MAX_BYTE || y > DEFAULT_MAX_BYTE {
        return None;
    }
    let mut s = String::from("\x1b[M");
    for v in [b, x, y] {
        s.push(char::from_u32(v)?);
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    //! 用例逐条移植自 packages/web/src/lib/rio/mouse.test.ts。
    use super::*;

    const GRID: (usize, usize) = (80, 24);
    const SGR_DRAG: MouseMode = MouseMode {
        protocol: 1002,
        encoding: 1006,
    };

    fn ev() -> MouseReportEvent {
        MouseReportEvent {
            action: MouseAction::Down,
            button: MouseButton::Left,
            col: 4,
            row: 2,
            x: 0,
            y: 0,
            shift: false,
            alt: false,
            ctrl: false,
        }
    }

    fn mode(protocol: u16, encoding: u16) -> MouseMode {
        MouseMode { protocol, encoding }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn sgr_coordinates_and_finals() {
        let mut r = MouseReporter::new();
        assert_eq!(r.report(ev(), SGR_DRAG, GRID), s("\x1b[<0;5;3M"));
        assert_eq!(
            r.report(
                MouseReportEvent { action: MouseAction::Move, col: 6, ..ev() },
                SGR_DRAG,
                GRID
            ),
            s("\x1b[<32;7;3M")
        );
        assert_eq!(
            r.report(
                MouseReportEvent {
                    action: MouseAction::Up,
                    button: MouseButton::Right,
                    col: 6,
                    ..ev()
                },
                SGR_DRAG,
                GRID
            ),
            s("\x1b[<2;7;3m")
        );
    }

    #[test]
    fn modifiers_in_button_code() {
        let mut r = MouseReporter::new();
        assert_eq!(
            r.report(
                MouseReportEvent { button: MouseButton::Right, ctrl: true, ..ev() },
                SGR_DRAG,
                GRID
            ),
            s("\x1b[<18;5;3M")
        );
        assert_eq!(
            r.report(
                MouseReportEvent {
                    button: MouseButton::Middle,
                    shift: true,
                    alt: true,
                    ..ev()
                },
                SGR_DRAG,
                GRID
            ),
            s("\x1b[<13;5;3M")
        );
    }

    #[test]
    fn x10_only_presses_without_modifiers() {
        let mut r = MouseReporter::new();
        let m = mode(9, 1006);
        assert_eq!(
            r.report(MouseReportEvent { ctrl: true, shift: true, ..ev() }, m, GRID),
            s("\x1b[<0;5;3M")
        );
        assert_eq!(r.report(MouseReportEvent { action: MouseAction::Up, ..ev() }, m, GRID), None);
        assert_eq!(r.report(MouseReportEvent { action: MouseAction::Move, ..ev() }, m, GRID), None);
    }

    #[test]
    fn mode_1000_press_release_only() {
        let mut r = MouseReporter::new();
        let m = mode(1000, 1006);
        assert_eq!(r.report(ev(), m, GRID), s("\x1b[<0;5;3M"));
        assert_eq!(
            r.report(MouseReportEvent { action: MouseAction::Move, col: 9, ..ev() }, m, GRID),
            None
        );
        assert_eq!(
            r.report(MouseReportEvent { action: MouseAction::Up, ..ev() }, m, GRID),
            s("\x1b[<0;5;3m")
        );
    }

    #[test]
    fn mode_1002_drag_only() {
        let mut r = MouseReporter::new();
        assert_eq!(
            r.report(
                MouseReportEvent { action: MouseAction::Move, button: MouseButton::None, ..ev() },
                SGR_DRAG,
                GRID
            ),
            None
        );
        assert_eq!(
            r.report(MouseReportEvent { action: MouseAction::Move, ..ev() }, SGR_DRAG, GRID),
            s("\x1b[<32;5;3M")
        );
    }

    #[test]
    fn mode_1003_buttonless_motion() {
        let mut r = MouseReporter::new();
        assert_eq!(
            r.report(
                MouseReportEvent { action: MouseAction::Move, button: MouseButton::None, ..ev() },
                mode(1003, 1006),
                GRID
            ),
            s("\x1b[<35;5;3M")
        );
    }

    #[test]
    fn no_protocol_no_report() {
        let mut r = MouseReporter::new();
        assert_eq!(r.report(ev(), mode(0, 1006), GRID), None);
        assert_eq!(r.report(ev(), mode(0, 0), GRID), None);
    }

    #[test]
    fn buttonless_non_move_is_invalid() {
        let mut r = MouseReporter::new();
        assert_eq!(
            r.report(MouseReportEvent { button: MouseButton::None, ..ev() }, mode(1003, 1006), GRID),
            None
        );
    }

    #[test]
    fn move_dedup_by_cell() {
        let mut r = MouseReporter::new();
        let mv = MouseReportEvent { action: MouseAction::Move, ..ev() };
        assert_eq!(r.report(mv, SGR_DRAG, GRID), s("\x1b[<32;5;3M"));
        assert_eq!(r.report(mv, SGR_DRAG, GRID), None);
        assert_eq!(r.report(MouseReportEvent { col: 5, ..mv }, SGR_DRAG, GRID), s("\x1b[<32;6;3M"));
        assert_eq!(
            r.report(MouseReportEvent { col: 5, shift: true, ..mv }, SGR_DRAG, GRID),
            s("\x1b[<36;6;3M")
        );
        assert_eq!(r.report(MouseReportEvent { col: 5, ..ev() }, SGR_DRAG, GRID), s("\x1b[<0;6;3M"));
        assert_eq!(r.report(MouseReportEvent { col: 5, ..ev() }, SGR_DRAG, GRID), s("\x1b[<0;6;3M"));
    }

    #[test]
    fn reset_allows_same_cell_again() {
        let mut r = MouseReporter::new();
        let mv = MouseReportEvent { action: MouseAction::Move, ..ev() };
        r.report(mv, SGR_DRAG, GRID);
        r.reset();
        assert_eq!(r.report(mv, SGR_DRAG, GRID), s("\x1b[<32;5;3M"));
    }

    #[test]
    fn sgr_pixels_dedup_by_pixel() {
        let mut r = MouseReporter::new();
        let m = mode(1003, 1016);
        assert_eq!(r.report(MouseReportEvent { x: 41, y: 37, ..ev() }, m, GRID), s("\x1b[<0;41;37M"));
        let mv = MouseReportEvent { action: MouseAction::Move, x: 41, y: 37, ..ev() };
        assert_eq!(r.report(mv, m, GRID), s("\x1b[<32;41;37M"));
        // 同一格内挪了 1 像素也算新位置
        assert_eq!(r.report(MouseReportEvent { x: 42, ..mv }, m, GRID), s("\x1b[<32;42;37M"));
        assert_eq!(r.report(MouseReportEvent { x: 42, ..mv }, m, GRID), None);
    }

    #[test]
    fn default_encoding_single_bytes() {
        let mut r = MouseReporter::new();
        let m = mode(1000, 0);
        // 按键 0 → 32 " "，列 5 → 37 "%"，行 3 → 35 "#"
        assert_eq!(r.report(ev(), m, GRID), s("\x1b[M %#"));
        // 松开：按键号一律 3 → 35 "#"，中键也一样
        assert_eq!(
            r.report(
                MouseReportEvent { action: MouseAction::Up, button: MouseButton::Middle, ..ev() },
                m,
                GRID
            ),
            s("\x1b[M#%#")
        );
    }

    #[test]
    fn default_encoding_drops_high_bytes() {
        let mut r = MouseReporter::new();
        let m = mode(1000, 0);
        let wide = (200, 24);
        assert_eq!(r.report(MouseReportEvent { col: 94, ..ev() }, m, wide), s("\x1b[M \x7f#"));
        assert_eq!(r.report(MouseReportEvent { col: 95, ..ev() }, m, wide), None);
    }

    #[test]
    fn out_of_grid_dropped() {
        let mut r = MouseReporter::new();
        assert_eq!(r.report(MouseReportEvent { col: 80, ..ev() }, SGR_DRAG, GRID), None);
        assert_eq!(r.report(MouseReportEvent { row: -1, ..ev() }, SGR_DRAG, GRID), None);
    }
}
