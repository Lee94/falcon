//! git：命令构造与输出解析（command）、错误分类（error）、路径运算（path）——纯函数层；
//! 执行环境（host）、执行与探测（repo）、仓库锁（lock）、删除护栏（remove）、
//! 多仓库批量派生（multi）——运行时层，跑在会话核心的 LocalSet 上（`!Send`）。
pub mod command;
pub mod error;
pub mod host;
pub mod lock;
pub mod multi;
pub mod path;
pub mod remove;
pub mod repo;
