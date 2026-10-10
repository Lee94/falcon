//! 飞书项目（meegle CLI）。命令构造与输出归一化（S1）；可执行文件定位、进程、缓存（S6）。
//! 路由（`meegle/routes.ts`）挂在 api 层，调 [`client::MeegleClient`]。
pub mod bin;
pub mod client;
pub mod command;
pub mod pagination;
