//! px0 审阅。命令构造与反代头过滤（S1）；二进制下载校验、实例管理（S6）。
//! 路由（`px0/routes.ts`：入口页、反代）挂在 api 层，调 [`manager::Px0Manager`]。
pub mod bin;
pub mod command;
pub mod manager;
pub mod proxy;
