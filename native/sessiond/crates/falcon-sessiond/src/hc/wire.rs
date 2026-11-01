//! `herdr-core` (`hc`) 兼容网关的双模式线协议（设计文档 3.2 节）：
//! - **Plain 模式**（握手缺省）：按 `\n` 分割的 JSON-RPC 2.0 文本行；
//! - **Framed 模式**（`hello` 带 `"framed": true`）：9 字节定长头
//!   `[channel: u32 LE][kind: u8][len: u32 LE][payload]`。
//!
//! 信用窗口（Credit Flow Control）：
//! - 每个数据通道（`channel >= 1`）初始拥有 256 KiB 的发送配额（`INITIAL_WINDOW`）；
//! - 网关发出的每个 `DATA` 帧最多 16 KiB（`CHUNK_SIZE`），并扣减该通道的配额；
//! - 客户端消费后发 `CREDIT`（4 字节 `u32 LE`）归还配额；
//! - 若某通道待发积压超过 4 MiB（`MAX_BEHIND`），丢弃积压并标记 `dropped = true`，
//!   待客户端归还配额后由 attach 会话重发一帧完整的首帧（First Paint）。

use std::collections::VecDeque;

pub const HEADER_LEN: usize = 9;
pub const INITIAL_WINDOW: usize = 256 * 1024;
pub const MAX_BEHIND: usize = 4 * 1024 * 1024;
pub const CHUNK_SIZE: usize = 16 * 1024;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
#[allow(dead_code)]
pub const MAX_LINE: usize = 8 * 1024 * 1024;

pub mod kind {
    pub const DATA: u8 = 0;
    pub const CREDIT: u8 = 1;
    pub const CLOSE: u8 = 2;
    pub const RESIZE: u8 = 3;
    pub const FOCUS: u8 = 4;
    pub const FACTS: u8 = 5;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub channel: u32,
    pub kind: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    #[allow(dead_code)]
    pub fn encode(&self) -> Vec<u8> {
        encode_raw(self.channel, self.kind, &self.payload)
    }

    pub fn control_json(json_bytes: &[u8]) -> Vec<u8> {
        encode_raw(0, kind::DATA, json_bytes)
    }
}

pub fn encode_raw(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&channel.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_credit(payload: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = payload.get(..4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

#[allow(dead_code)]
pub fn encode_credit(channel: u32, bytes: u32) -> Vec<u8> {
    encode_raw(channel, kind::CREDIT, &bytes.to_le_bytes())
}

pub fn decode_resize(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() < 4 {
        return None;
    }
    let cols = u16::from_le_bytes([payload[0], payload[1]]);
    let rows = u16::from_le_bytes([payload[2], payload[3]]);
    Some((cols.max(1), rows.max(1)))
}

#[allow(dead_code)]
pub fn encode_resize(channel: u32, cols: u16, rows: u16) -> Vec<u8> {
    let mut p = [0u8; 4];
    p[..2].copy_from_slice(&cols.to_le_bytes());
    p[2..4].copy_from_slice(&rows.to_le_bytes());
    encode_raw(channel, kind::RESIZE, &p)
}

/// 流式拆帧器（Framed 模式）
#[derive(Debug, Default)]
pub struct FramedDecoder {
    buf: Vec<u8>,
    start: usize,
}

impl FramedDecoder {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_leftover(bytes: Vec<u8>) -> Self {
        Self { buf: bytes, start: 0 }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start >= self.buf.len() / 2 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> anyhow::Result<Option<Frame>> {
        let avail = &self.buf[self.start..];
        if avail.len() < HEADER_LEN {
            return Ok(None);
        }
        let channel = u32::from_le_bytes([avail[0], avail[1], avail[2], avail[3]]);
        let k = avail[4];
        let len = u32::from_le_bytes([avail[5], avail[6], avail[7], avail[8]]) as usize;
        if len > MAX_FRAME {
            anyhow::bail!("hc 帧长度 {len} 超出上限 {MAX_FRAME}");
        }
        if avail.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let payload = avail[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.start += HEADER_LEN + len;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        Ok(Some(Frame { channel, kind: k, payload }))
    }
}

/// 单条终端数据通道的信用窗口状态
#[derive(Debug)]
pub struct ChannelCredit {
    pub channel: u32,
    pub credit: usize,
    pub pending: VecDeque<u8>,
    /// 因积压超 `MAX_BEHIND` 丢弃过输出，下次有信用时需要先请求重发首帧
    pub dropped: bool,
}

impl ChannelCredit {
    pub fn new(channel: u32) -> Self {
        Self {
            channel,
            credit: INITIAL_WINDOW,
            pending: VecDeque::new(),
            dropped: false,
        }
    }

    /// 推入一段终端输出。若积压超 `MAX_BEHIND`，清空缓冲并置 `dropped = true`
    pub fn push_data(&mut self, data: &[u8]) {
        if self.dropped {
            return;
        }
        if self.pending.len() + data.len() > MAX_BEHIND {
            self.pending.clear();
            self.dropped = true;
            return;
        }
        self.pending.extend(data);
    }

    /// 用新的首帧重置被丢弃的通道
    pub fn reset_with_first_paint(&mut self, paint: &[u8]) {
        self.dropped = false;
        self.pending.clear();
        self.pending.extend(paint);
    }

    /// 客户端发来 `CREDIT`
    pub fn add_credit(&mut self, amount: u32) {
        self.credit = self.credit.saturating_add(amount as usize);
    }

    /// 按当前配额与 `CHUNK_SIZE` 吐出可立即发走的数据帧
    pub fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while self.credit > 0 && !self.pending.is_empty() {
            let n = self.pending.len().min(self.credit).min(CHUNK_SIZE);
            let chunk: Vec<u8> = self.pending.drain(..n).collect();
            self.credit -= n;
            out.push(encode_raw(self.channel, kind::DATA, &chunk));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let frames = vec![
            Frame { channel: 0, kind: kind::DATA, payload: br#"{"jsonrpc":"2.0"}"#.to_vec() },
            Frame { channel: 1, kind: kind::DATA, payload: b"hello world".to_vec() },
            Frame { channel: 1, kind: kind::RESIZE, payload: vec![80, 0, 24, 0] },
            Frame { channel: 1, kind: kind::CLOSE, payload: br#"{"reason":"exited","exit_code":0}"#.to_vec() },
        ];
        let mut dec = FramedDecoder::new();
        for f in &frames {
            for b in f.encode() {
                dec.push(&[b]);
            }
        }
        let mut got = Vec::new();
        while let Some(f) = dec.next_frame().unwrap() {
            got.push(f);
        }
        assert_eq!(got, frames);
    }

    #[test]
    fn credit_window_chunks_and_drops_when_behind() {
        let mut cc = ChannelCredit::new(3);
        cc.credit = 20_000;
        cc.push_data(&vec![b'a'; 25_000]);
        let f1 = cc.drain_frames();
        assert_eq!(f1.len(), 2);
        // 第一片 16384，第二片 3616，还剩 5000 字节等信用
        assert_eq!(f1[0].len(), HEADER_LEN + CHUNK_SIZE);
        assert_eq!(f1[1].len(), HEADER_LEN + (20_000 - CHUNK_SIZE));
        assert_eq!(cc.pending.len(), 5_000);
        assert_eq!(cc.credit, 0);

        cc.add_credit(10_000);
        let f2 = cc.drain_frames();
        assert_eq!(f2.len(), 1);
        assert_eq!(f2[0].len(), HEADER_LEN + 5_000);
        assert!(cc.pending.is_empty());

        // 超过 4 MiB 触发 dropped
        cc.credit = 0;
        cc.push_data(&vec![b'x'; MAX_BEHIND + 1]);
        assert!(cc.dropped);
        assert!(cc.pending.is_empty());
    }
}
