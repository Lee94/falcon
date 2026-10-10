//! 按字节数封顶的输出环形缓冲，用于 Viewer 重连时回放 Scrollback。移植自
//! `packages/server/src/ringbuffer.ts`。纯数据结构，零 I/O。
//!
//! 块是 `String`：PTY 输出在进来之前已经做过 UTF-8 跨块拼接（附录 A），每个块都是完整的
//! UTF-8，所以快照拼起来也是。字节数按 UTF-8 算（TS 的 `Buffer.byteLength`），即 `str::len`。
//!
//! 容量与 zellij 的 scrollback 深度（`zellij/command.ts` 的 `SCROLL_BUFFER` = 10000 行）是
//! 一起定的，两个数要一起改。

use std::collections::VecDeque;

/// 4MB：要装得下 dump-screen 的整份快照（scroll_buffer_size 10000 行、
/// 含 ANSI 时约 1-2MB）再留出后续实时输出的余量。快照是单个 chunk，
/// 上限太小的话它会在之后第一次触顶时被整块淘汰，历史瞬间清零。
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// 按字节数封顶的输出环形缓冲。超出上限时从最老的块开始整块淘汰，但至少留一块
/// （单块比上限还大时原样留着——那多半就是 dump-screen 的快照）。
#[derive(Debug, Clone)]
pub struct RingBuffer {
    chunks: VecDeque<String>,
    bytes: usize,
    max_bytes: usize,
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RingBuffer {
    pub fn new() -> Self {
        Self::with_max_bytes(DEFAULT_MAX_BYTES)
    }

    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self { chunks: VecDeque::new(), bytes: 0, max_bytes }
    }

    pub fn append(&mut self, data: impl Into<String>) {
        let data = data.into();
        self.bytes += data.len();
        self.chunks.push_back(data);
        while self.bytes > self.max_bytes && self.chunks.len() > 1 {
            if let Some(removed) = self.chunks.pop_front() {
                self.bytes -= removed.len();
            }
        }
    }

    pub fn snapshot(&self) -> String {
        let mut out = String::with_capacity(self.bytes);
        for chunk in &self.chunks {
            out.push_str(chunk);
        }
        out
    }

    /// 清空；`data` 非空时把它作为唯一的块放进去（TS 的 `if (data)`：空串等同不给）
    pub fn reset(&mut self, data: Option<&str>) {
        self.chunks.clear();
        self.bytes = 0;
        if let Some(d) = data
            && !d.is_empty()
        {
            self.append(d);
        }
    }

    /// 当前占用的字节数（UTF-8）
    pub fn len_bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    // TS 没有 ringbuffer.test.ts，以下用例按 TS 实现的行为补写
    use super::*;

    #[test]
    fn default_cap_is_4mb() {
        assert_eq!(DEFAULT_MAX_BYTES, 4 * 1024 * 1024);
        assert_eq!(RingBuffer::new().max_bytes, DEFAULT_MAX_BYTES);
    }

    #[test]
    fn snapshot_joins_chunks_in_order() {
        let mut b = RingBuffer::new();
        b.append("ab");
        b.append("中");
        b.append("c");
        assert_eq!(b.snapshot(), "ab中c");
        // 字节数按 UTF-8：「中」是 3 字节
        assert_eq!(b.len_bytes(), 6);
    }

    #[test]
    fn evicts_whole_chunks_from_the_front() {
        let mut b = RingBuffer::with_max_bytes(5);
        b.append("abc");
        b.append("de");
        assert_eq!(b.snapshot(), "abcde");
        b.append("f");
        assert_eq!(b.snapshot(), "def");
        assert_eq!(b.len_bytes(), 3);
    }

    #[test]
    fn keeps_a_single_oversized_chunk() {
        let mut b = RingBuffer::with_max_bytes(4);
        b.append("0123456789");
        assert_eq!(b.snapshot(), "0123456789");
        b.append("x");
        assert_eq!(b.snapshot(), "x");
    }

    #[test]
    fn reset_replaces_with_data_and_ignores_empty() {
        let mut b = RingBuffer::with_max_bytes(100);
        b.append("old");
        b.reset(Some("dump"));
        assert_eq!(b.snapshot(), "dump");
        b.reset(Some(""));
        assert_eq!(b.snapshot(), "");
        assert_eq!(b.len_bytes(), 0);
        b.append("x");
        b.reset(None);
        assert_eq!(b.snapshot(), "");
    }
}
