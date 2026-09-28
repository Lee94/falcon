//! 终端核心：VT 解析与状态（alacritty_terminal）、输入编码、模式跟踪。不依赖 GPUI。
//!
//! 设计见 docs/design/gpui-client.md §3。换 VT 核心（libghostty-vt / rio-vt）时只换
//! [`core`] 的实现，编码层与 app 不动。

pub mod core;
pub mod keyroute;
pub mod keys;
pub mod keystroke;
pub mod links;
pub mod modes;
pub mod mouse;
pub mod paste;
pub mod wheel;

pub use alacritty_terminal;
pub use crate::core::{
    SnapCursor, SnapSelection, Snapshot, TermCore, TermEvent, TermOptions, TermSize,
};
pub use keystroke::{Keystroke, Modifiers};
pub use modes::TermModeTracker;
pub use mouse::{MouseAction, MouseButton, MouseMode, MouseReportEvent, MouseReporter};
pub use wheel::{DeltaMode, WheelAccumulator};

/// 焦点上报（`?1004h` 时）：CSI I / CSI O。
pub fn focus_report(focused: bool) -> &'static str {
    if focused { "\x1b[I" } else { "\x1b[O" }
}
