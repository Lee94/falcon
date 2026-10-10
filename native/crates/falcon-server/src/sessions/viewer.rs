//! Viewer：一条会话 WS 连接在会话引擎里的替身。对应 `manager.ts` 的 `Viewer` 接口与
//! `ws.ts` 里它的实现。
//!
//! 引擎（LocalSet）不碰 socket：Viewer 只是一个发送端 + 一个「已投出、还没写进 socket」的
//! 字节计数，WS 写任务每写完一帧就减掉。这个计数就是 Node 里 `socket.bufferedAmount` 的
//! 等价物——背压判断照搬 ws.ts 的两条水位线。

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use bytes::Bytes;
use falcon_proto::ServerMessage;
use tokio::sync::mpsc;

/// 背压阈值：ws 的发送队列没有上限，慢客户端（手机弱网开着 `cat 大文件`）会让
/// bufferedAmount 无限堆积直至进程 OOM。超过高水位就丢输出帧——数据都在服务端
/// Scrollback 里，manager 会在低水位后用 replay 重新同步。
pub const BACKPRESSURE_HIGH: usize = 4 * 1024 * 1024;
pub const BACKPRESSURE_LOW: usize = 256 * 1024;

/// 投给 WS 写任务的一帧
#[derive(Debug, Clone)]
pub enum ViewerFrame {
    /// 控制消息（JSON 文本帧）
    Text(String),
    /// 终端数据帧（TERM_FRAME_*）。多个 Viewer 共享同一份字节
    Binary(Bytes),
}

impl ViewerFrame {
    pub fn len(&self) -> usize {
        match self {
            ViewerFrame::Text(s) => s.len(),
            ViewerFrame::Binary(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Viewer {
    /// 进程内唯一；ViewerArbiter 按它认人
    pub id: u64,
    tx: mpsc::UnboundedSender<ViewerFrame>,
    queued: Arc<AtomicUsize>,
    /// sendBytes 因背压丢过帧：等它排空后用 replay 整体重同步（TS 里是 manager 上的 WeakSet）
    pub(crate) lagged: Cell<bool>,
}

/// WS 写任务那一头
pub struct ViewerSink {
    pub rx: mpsc::UnboundedReceiver<ViewerFrame>,
    queued: Arc<AtomicUsize>,
}

impl ViewerSink {
    /// 一帧已经写进 socket（或 socket 已死、丢掉了）
    pub fn written(&self, frame: &ViewerFrame) {
        self.queued.fetch_sub(frame.len(), Ordering::AcqRel);
    }
}

impl Viewer {
    pub fn new() -> (Viewer, ViewerSink) {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        (Viewer { id, tx, queued: queued.clone(), lagged: Cell::new(false) }, ViewerSink { rx, queued })
    }

    fn push(&self, frame: ViewerFrame) {
        let len = frame.len();
        self.queued.fetch_add(len, Ordering::AcqRel);
        if self.tx.send(frame).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
        }
    }

    /// 控制类消息（state / reconnecting / error），JSON 文本帧
    pub fn send(&self, msg: &ServerMessage) {
        if self.tx.is_closed() {
            return;
        }
        match serde_json::to_string(msg) {
            Ok(text) => self.push(ViewerFrame::Text(text)),
            Err(e) => log::error!("控制消息序列化失败：{e}"),
        }
    }

    /// 终端数据帧（TERM_FRAME_*，二进制）。返回 false 表示对端发送缓冲已堆积到阈值、
    /// 本帧被丢弃——数据都在 RingBuffer 里，调用方据此把 Viewer 标记为落后，等
    /// drained() 后用 replay 重新同步。
    pub fn send_bytes(&self, frame: Bytes) -> bool {
        if self.tx.is_closed() {
            return true; // socket 已死，无所谓丢不丢
        }
        if self.queued.load(Ordering::Acquire) > BACKPRESSURE_HIGH {
            return false;
        }
        self.push(ViewerFrame::Binary(frame));
        true
    }

    /// 发送缓冲已排空到可以重新同步
    pub fn drained(&self) -> bool {
        !self.tx.is_closed() && self.queued.load(Ordering::Acquire) < BACKPRESSURE_LOW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backpressure_watermarks() {
        let (v, mut sink) = Viewer::new();
        assert!(v.drained());
        let big = Bytes::from(vec![0u8; BACKPRESSURE_HIGH]);
        assert!(v.send_bytes(big.clone()));
        // 恰好等于高水位还能发；超过之后丢帧
        assert!(v.send_bytes(Bytes::from_static(b"x")));
        assert!(!v.send_bytes(Bytes::from_static(b"y")));
        assert!(!v.drained());
        while let Ok(f) = sink.rx.try_recv() {
            sink.written(&f);
        }
        assert!(v.drained());
        drop(sink);
        // socket 已死：照收不误，但不算排空
        assert!(v.send_bytes(big));
        assert!(!v.drained());
    }

    #[test]
    fn control_messages_are_json() {
        let (v, mut sink) = Viewer::new();
        v.send(&ServerMessage::Title { title: None });
        match sink.rx.try_recv().unwrap() {
            ViewerFrame::Text(t) => assert_eq!(t, r#"{"type":"title","title":null}"#),
            other => panic!("{other:?}"),
        }
    }
}
