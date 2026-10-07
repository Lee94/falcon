//! 终端滚动条的纯函数层（ADR 0019）。对应 web 的 `lib/termScroll.ts`，口径逐条对齐，
//! 单测也照那边的用例写。
//!
//! 终端的滚动发生在宿主机的 zellij 里，客户端本地没有 scrollback：位置由服务端问
//! zellij 里的插件得来，单位是 zellij 的显示行。这里只管把它画成滑块、把拖到的像素
//! 换回行数。

/// 服务端推来的滚动位置（`ServerMessage::Scroll`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollState {
    /// 视口下方的行数，0 = 在底部
    pub position: u32,
    /// 视口上方 + 下方的行数，0 = 没有可滚的历史
    pub length: u32,
    /// 视口高度
    pub rows: u32,
}

/// 滑块在轨道里的位置与高度（像素，相对轨道顶边）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbGeometry {
    pub top: f32,
    pub height: f32,
}

/// 滑块最矮多少（逻辑像素，界面缩放前）：一万行历史时按比例只剩一两个像素，抓不住
pub const MIN_THUMB_PX: f32 = 24.0;

/// 没有可滚的历史时为 `None`——不画滑块。
///
/// 高度按「视口 / (历史 + 视口)」的比例；位置按视口上方的行数在「轨道 − 滑块」这段里
/// 线性摆：在最上面时贴顶，在底部（position 0）时贴底。夹了最小高度以后仍然两端对得上。
pub fn thumb_geometry(s: ScrollState, track_px: f32, min_thumb: f32) -> Option<ThumbGeometry> {
    if s.length == 0 || s.rows == 0 || track_px <= 0.0 {
        return None;
    }
    let (length, rows) = (s.length as f32, s.rows as f32);
    let height = (rows / (length + rows) * track_px).max(min_thumb).min(track_px);
    let above = length - (s.position.min(s.length) as f32);
    Some(ThumbGeometry { top: above / length * (track_px - height), height })
}

/// [`thumb_geometry`] 的反函数：滑块顶边拖到 `top_px` 时对应的 position（视口下方的行数）
pub fn position_for_thumb_top(s: ScrollState, track_px: f32, thumb_px: f32, top_px: f32) -> u32 {
    let span = track_px - thumb_px;
    if s.length == 0 || span <= 0.0 {
        return 0;
    }
    let above = (top_px.clamp(0.0, span) / span * s.length as f32).round() as u32;
    s.length - above.min(s.length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(position: u32) -> ScrollState {
        ScrollState { position, length: 470, rows: 30 }
    }

    // ---- thumbGeometry ----

    #[test]
    fn nothing_to_scroll_draws_nothing() {
        assert_eq!(thumb_geometry(ScrollState { position: 0, length: 0, rows: 30 }, 300.0, MIN_THUMB_PX), None);
        assert_eq!(thumb_geometry(ScrollState { position: 0, length: 470, rows: 0 }, 300.0, MIN_THUMB_PX), None);
        assert_eq!(thumb_geometry(at(0), 0.0, MIN_THUMB_PX), None);
    }

    #[test]
    fn bottom_hugs_bottom_and_top_hugs_top() {
        let bottom = thumb_geometry(at(0), 500.0, MIN_THUMB_PX).unwrap();
        assert_eq!(bottom.top + bottom.height, 500.0);
        assert_eq!(thumb_geometry(at(470), 500.0, MIN_THUMB_PX).unwrap().top, 0.0);
    }

    #[test]
    fn height_is_viewport_share() {
        // 30 / (470 + 30) = 6%
        assert_eq!(thumb_geometry(at(0), 500.0, MIN_THUMB_PX).unwrap().height, 30.0);
    }

    #[test]
    fn tiny_ratio_clamps_to_min_height_and_ends_still_meet() {
        let s = ScrollState { position: 0, length: 9970, rows: 30 };
        let bottom = thumb_geometry(s, 500.0, MIN_THUMB_PX).unwrap();
        assert_eq!(bottom.height, MIN_THUMB_PX);
        assert_eq!(bottom.top + bottom.height, 500.0);
        let top = ScrollState { position: 9970, ..s };
        assert_eq!(thumb_geometry(top, 500.0, MIN_THUMB_PX).unwrap().top, 0.0);
    }

    #[test]
    fn short_history_stays_inside_track() {
        let g = thumb_geometry(ScrollState { position: 0, length: 2, rows: 30 }, 100.0, MIN_THUMB_PX).unwrap();
        assert!(g.height <= 100.0);
        assert_eq!(g.top + g.height, 100.0);
    }

    #[test]
    fn out_of_range_position_is_clamped() {
        assert_eq!(thumb_geometry(at(9999), 500.0, MIN_THUMB_PX).unwrap().top, 0.0);
    }

    // ---- positionForThumbTop ----

    #[test]
    fn inverse_of_thumb_geometry() {
        for p in [0, 1, 100, 235, 469, 470] {
            let s = at(p);
            let g = thumb_geometry(s, 500.0, MIN_THUMB_PX).unwrap();
            assert_eq!(position_for_thumb_top(s, 500.0, g.height, g.top), p);
        }
    }

    #[test]
    fn dragging_past_the_ends_clamps() {
        let s = at(100);
        assert_eq!(position_for_thumb_top(s, 500.0, 30.0, -50.0), 470);
        assert_eq!(position_for_thumb_top(s, 500.0, 30.0, 9999.0), 0);
    }

    #[test]
    fn full_height_thumb_has_no_room_to_drag() {
        let s = ScrollState { position: 1, length: 2, rows: 30 };
        assert_eq!(position_for_thumb_top(s, 100.0, 100.0, 0.0), 0);
    }
}
