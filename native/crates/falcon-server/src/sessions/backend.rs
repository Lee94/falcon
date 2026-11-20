//! 一个已附着的底层终端：本地 PTY、SSH channel（非持久会话），或宿主机上的会话守护进程
//! （持久会话，ADR 0020）。移植自 `packages/server/src/sessions/backend.ts`。
//!
//! 会话核心跑在单线程的 LocalSet 上，backend 与回调都是 `!Send` 的 `Rc`。

use std::rc::Rc;

use falcon_proto::sessiond::ExitStatus;
use falcon_proto::{OscColorHint, ProgramStatusRecord};

use crate::exec::LocalBoxFuture;

pub trait Backend {
    fn write(&self, data: &str);
    fn resize(&self, cols: u16, rows: u16);
    /// 仅断开本端附着，不终止底层（用于持久会话链路重建前的清理）
    fn destroy(&self);
    /// tty 前台进程名。只有本地 POSIX PTY 提供；持久会话走 [`Backend::foreground`]
    fn process_name(&self) -> Option<String> {
        None
    }

    // ---- 以下只有守护进程会话有（缺省 = 不支持），见 sessiond/backend.rs ----

    /// 请求一份接回用的快照。true = 快照会经 `on_event(BackendEvent::Synced)` 按流的顺序回来：
    /// 在它之前到的输出都已含在快照里，在它之后到的都在快照之后（设计文档 5.5）
    fn request_sync(&self) -> bool {
        false
    }
    /// 宿主机上问前台命令（守护进程直接 tcgetpgrp，不再每次 exec 一条命令）
    fn foreground(&self) -> Option<LocalBoxFuture<'static, Option<String>>> {
        None
    }
    /// 终端深浅与前景 / 背景色：宿主机的 VT 进程按它回答 OSC 10/11/12 与 `CSI ? 996 n`，
    /// 深浅翻转时由守护进程注入 2031 通知
    fn set_appearance(&self, _hint: &OscColorHint) {}
    /// 用户看过了：清掉程序状态里的 done / error
    fn status_seen(&self, _ids: &[String]) {}
    /// 由 backend 自己结束会话（守护进程会给 shell 的进程组发 SIGHUP，2 秒后补 SIGKILL）。
    /// false = 不支持，调用方自己想办法
    fn terminate(&self) -> bool {
        false
    }
    /// 把宿主机上这个会话的 VT 进程换成另一个二进制（热升级 ghostty，设计文档 2.3）
    fn upgrade_vt(&self, _binary: &str) {}
}

/// backend 推给会话核心的、字节流之外的事
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    /// [`Backend::request_sync`] 的快照（已经是可以直接回放的文本）
    Synced(String),
    /// 程序状态（OSC 7501）全集
    ProgramStatus(Vec<ProgramStatusRecord>),
    /// 程序设的终端标题（OSC 0 / 2）；空串 = 没有。连上时守护进程先发一次当前的，旧版守护进程不发
    Title(String),
    /// shell 退出了（守护进程随后关闭连接）
    Exit(ExitStatus),
    /// 宿主机上的 VT 进程挂了、已经用原始尾巴重建
    VtRestarted,
    /// VT 进程反复崩溃，只剩纯转发
    VtDegraded,
    /// SSH 上 bridge 的退出码（连接结束时给；本机连接没有）
    BridgeExit(Option<u32>),
}

/// backend 推给会话核心的几件事。在 LocalSet 上顺序调用
#[derive(Clone)]
pub struct BackendCallbacks {
    pub on_data: Rc<dyn Fn(String)>,
    /// 底层附着结束（shell 退出 / 链路关闭）。持久会话由上层判断是链路问题还是真退出
    pub on_exit: Rc<dyn Fn()>,
    /// 守护进程会话的事件；本地 PTY / SSH channel 从不调用
    pub on_event: Rc<dyn Fn(BackendEvent)>,
}

pub struct AttachResult {
    pub backend: Rc<dyn Backend>,
    pub durable: bool,
}

/// 接回持久会话时，宿主机上的会话已经不在了（没有守护进程、也没有退出记录）
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("会话在宿主机上已经不存在")]
pub struct SessionGoneError;

/// 接回持久会话时，发现它在断线期间正常结束了（守护进程留下了退出记录）
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("会话已经结束")]
pub struct SessionExitedError;
