//! 按宿主 `Idiom` 自适应分发的顶层工作台壳层（Desktop/Tablet/Web vs Phone vs TUI）。

pub mod desktop;
pub mod mobile;
mod overview;
mod sidebar;
pub mod tui;
pub(crate) mod widgets;

pub use desktop::{DesktopShell, desktop_shell};
pub use mobile::{MobileShell, mobile_shell};
pub use tui::{TuiShell, tui_shell};
