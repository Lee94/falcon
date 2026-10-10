//! 接回时的终端尺寸。移植自 `packages/server/src/sessions/termSize.ts`。纯函数，零 I/O。
//!
//! 客户端已经拒绝把未量好的 80×24 发给 PTY（见 web termFit），但服务端
//! liveEntry 以前硬编码 80×24，ensureAttached 会立刻按这个尺寸 attach。
//! Zellij 跟着 SIGWINCH，grok / vim / htop 被挤成 24 行，状态就没了。

/// 终端格子数。行列用 `u16`：与 falcon-proto 的 `ClientMessage::Resize`、PTY 的 winsize 同宽
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// 新会话、从没量到过格子时的兜底尺寸
pub const FALLBACK_TERM_SIZE: TermSize = TermSize { cols: 80, rows: 24 };

/// 库里存的上次尺寸（`sessions.cols / rows`）。
///
/// TS 收的是 `unknown`（库里的列可能是 null）：这里 `None` = null / 缺省，数值按 JS 的
/// `Number.isInteger` 判整。缺失或垃圾值返回 `None`，免得把 80×24 的兜底当成量过的。
///
/// 与 TS 的出入：超出 `u16` 的整数 TS 会原样收下，这里当垃圾值丢掉——PTY 的 winsize
/// 本来就是 16 位，那种值不可能是真量出来的。
pub fn parse_stored_term_size(cols: Option<f64>, rows: Option<f64>) -> Option<TermSize> {
    let (c, r) = (cols?, rows?);
    if !is_integer(c) || !is_integer(r) {
        return None;
    }
    if c < 2.0 || r < 1.0 {
        return None;
    }
    if c > f64::from(u16::MAX) || r > f64::from(u16::MAX) {
        return None;
    }
    Some(TermSize { cols: c as u16, rows: r as u16 })
}

/// `Number.isInteger`
fn is_integer(x: f64) -> bool {
    x.is_finite() && x.trunc() == x
}

pub fn fallback_term_size(size: Option<TermSize>) -> TermSize {
    size.unwrap_or(FALLBACK_TERM_SIZE)
}

/// [`decide_viewer_attach`] 的结论
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerAttach {
    /// 后端已经活着：直接 hello + replay
    Hello,
    /// 等这条连接自己报格子再 attach
    WaitSize,
    /// 非持久会话的后端没了，会话已死
    Dead,
}

impl ViewerAttach {
    /// TS 里的字面量
    pub fn as_str(self) -> &'static str {
        match self {
            ViewerAttach::Hello => "hello",
            ViewerAttach::WaitSize => "wait-size",
            ViewerAttach::Dead => "dead",
        }
    }
}

/// [`decide_viewer_attach`] 的输入（TS 的 `opts`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerAttachInput {
    pub durable: bool,
    pub has_backend: bool,
}

/// 一个 Viewer 连上来时要不要立刻 attach。
///
/// 即使库里存了上次尺寸，也等这条连接自己报格子：xterm 在 fit 之前是 80×24，
/// 先 attach 再 replay，205 列的 dump-screen 会在窄屏上折行，TUI 看起来像没恢复。
/// 库里的尺寸只给「概览里点接回 / SSH 自动重连」这种当时没有新 Viewer 在量格子的路径。
pub fn decide_viewer_attach(opts: ViewerAttachInput) -> ViewerAttach {
    if opts.has_backend {
        return ViewerAttach::Hello;
    }
    if !opts.durable {
        return ViewerAttach::Dead;
    }
    ViewerAttach::WaitSize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> TermSize {
        TermSize { cols, rows }
    }

    // describe("parseStoredTermSize")

    #[test]
    fn parse_stored_term_size_accepts_a_previously_measured_size() {
        assert_eq!(parse_stored_term_size(Some(205.0), Some(57.0)), Some(size(205, 57)));
    }

    #[test]
    fn parse_stored_term_size_rejects_missing_or_junk_so_we_do_not_treat_80x24_fallback_as_measured() {
        assert_eq!(parse_stored_term_size(None, None), None);
        assert_eq!(parse_stored_term_size(None, Some(24.0)), None);
        assert_eq!(parse_stored_term_size(Some(80.0), Some(0.0)), None);
        assert_eq!(parse_stored_term_size(Some(1.5), Some(24.0)), None);
    }

    // describe("fallbackTermSize")

    #[test]
    fn fallback_term_size_only_used_when_nothing_was_ever_measured() {
        assert_eq!(fallback_term_size(None), size(80, 24));
        assert_eq!(fallback_term_size(Some(size(205, 57))), size(205, 57));
    }

    // describe("decideViewerAttach")

    #[test]
    fn decide_viewer_attach_waits_for_this_viewers_resize_before_reattach_even_if_a_size_is_stored() {
        assert_eq!(
            decide_viewer_attach(ViewerAttachInput { durable: true, has_backend: false }),
            ViewerAttach::WaitSize
        );
        assert_eq!(ViewerAttach::WaitSize.as_str(), "wait-size");
    }

    #[test]
    fn decide_viewer_attach_replays_to_a_new_viewer_when_the_backend_is_already_live() {
        assert_eq!(decide_viewer_attach(ViewerAttachInput { durable: true, has_backend: true }), ViewerAttach::Hello);
    }

    #[test]
    fn decide_viewer_attach_marks_a_non_durable_session_dead_once_its_backend_is_gone() {
        assert_eq!(decide_viewer_attach(ViewerAttachInput { durable: false, has_backend: false }), ViewerAttach::Dead);
    }
}
