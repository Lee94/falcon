//! 按宿主 `Idiom` 自适应分发的顶层工作台壳层（Desktop/Tablet/Web vs Phone vs TUI）。

pub mod desktop;
pub mod mobile;
pub mod tui;

pub use desktop::{DesktopShell, desktop_shell};
pub use mobile::{MobileShell, mobile_shell};
pub use tui::{TuiShell, tui_shell};
