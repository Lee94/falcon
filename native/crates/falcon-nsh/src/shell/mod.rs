//! 按宿主 `Idiom` 自适应分发的顶层工作台壳层（Desktop/Tablet/Web vs Phone vs TUI）。

pub mod desktop;
pub mod mobile;
pub(crate) mod action_owner;
pub(crate) mod menus;
pub(crate) mod popups;
pub(crate) mod overview;
mod sidebar;
pub mod tui;
pub(crate) mod widgets;
mod title_status;
mod motion;

pub use desktop::{DesktopShell, desktop_shell};
pub use mobile::{MobileShell, mobile_shell};
pub use tui::{TuiShell, tui_shell};
