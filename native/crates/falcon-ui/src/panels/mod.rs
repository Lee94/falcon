//! 右侧栏的面板（文件 / 修改 / 历史 / 飞书项目）与画布上的文件、差异窗口。转发与公网发布
//! 挪进了设置的「中转」页（dialogs/settings/relays.rs，ADR 0016）。
//!
//! 每个面板都是一个 GPUI 视图：`new(ws, window, cx)` 建出来，observe 工作区自己刷新。
//! 右侧栏开着时切走不卸载（web 同理：飞书项目的 CLI 往返 2–6s），关掉右侧栏才卸。

pub mod changes;
pub mod diff_view;
pub mod file_view;
pub mod files;
pub mod git;
pub mod meegle;
