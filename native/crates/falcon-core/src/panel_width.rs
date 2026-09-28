//! 左右两栏的宽度规格。对应 web 的 `lib/panelWidth.ts`。
//!
//! 设计写死了 260 / 220–420（`docs/design/ui-redesign.md` §4），右侧各面板共用同一个
//! 槽位，所以用同一套数字——切面板不能改宽度，否则终端跟着 reflow。

use serde_json::Value;

use crate::js::{js_max, js_min, js_round};

pub const PANEL_WIDTH_DEFAULT: f64 = 260.0;
pub const PANEL_WIDTH_MIN: f64 = 220.0;
pub const PANEL_WIDTH_MAX: f64 = 420.0;

/// 把手在面板哪一侧
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
}

/// 夹到 [min, max] 并取整；NaN / ±∞ 回默认。`min` / `max` 在 TS 里缺省是
/// [`PANEL_WIDTH_MIN`] / [`PANEL_WIDTH_MAX`]。
pub fn clamp_panel_width(n: f64, min: f64, max: f64) -> f64 {
    if !n.is_finite() {
        return PANEL_WIDTH_DEFAULT;
    }
    js_min(max, js_max(min, js_round(n)))
}

/// 用默认范围夹
pub fn clamp_panel_width_default(n: f64) -> f64 {
    clamp_panel_width(n, PANEL_WIDTH_MIN, PANEL_WIDTH_MAX)
}

/// 偏好 JSON 里读出来的值（`None` = 缺字段）：不是数字都回落到默认
pub fn parse_panel_width(v: Option<&Value>) -> f64 {
    match v.and_then(Value::as_f64) {
        Some(n) => clamp_panel_width_default(n),
        None => PANEL_WIDTH_DEFAULT,
    }
}

/// 拖把手时的新宽度：右侧把手往右拉变宽，左侧把手往左拉变宽。
pub struct PanelResize {
    pub start_width: f64,
    pub start_x: f64,
    pub client_x: f64,
    pub edge: ResizeEdge,
    pub min: f64,
    pub max: f64,
}

impl PanelResize {
    /// 默认范围的拖动
    pub fn new(start_width: f64, start_x: f64, client_x: f64, edge: ResizeEdge) -> Self {
        PanelResize { start_width, start_x, client_x, edge, min: PANEL_WIDTH_MIN, max: PANEL_WIDTH_MAX }
    }
}

pub fn resize_panel_width(opts: &PanelResize) -> f64 {
    let delta = opts.client_x - opts.start_x;
    let next = match opts.edge {
        ResizeEdge::Right => opts.start_width + delta,
        ResizeEdge::Left => opts.start_width - delta,
    };
    clamp_panel_width(next, opts.min, opts.max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_passes_through_a_value_in_range_rounded() {
        assert_eq!(clamp_panel_width_default(300.0), 300.0);
        assert_eq!(clamp_panel_width_default(300.6), 301.0);
    }

    #[test]
    fn clamp_clamps_to_min_max() {
        assert_eq!(clamp_panel_width_default(PANEL_WIDTH_MIN - 40.0), PANEL_WIDTH_MIN);
        assert_eq!(clamp_panel_width_default(PANEL_WIDTH_MAX + 80.0), PANEL_WIDTH_MAX);
    }

    #[test]
    fn clamp_falls_back_to_default_on_nan_infinity() {
        assert_eq!(clamp_panel_width_default(f64::NAN), PANEL_WIDTH_DEFAULT);
        assert_eq!(clamp_panel_width_default(f64::INFINITY), PANEL_WIDTH_DEFAULT);
    }

    #[test]
    fn parse_accepts_a_finite_number() {
        assert_eq!(parse_panel_width(Some(&Value::from(320))), 320.0);
    }

    #[test]
    fn parse_rejects_missing_wrong_types() {
        assert_eq!(parse_panel_width(None), PANEL_WIDTH_DEFAULT);
        assert_eq!(parse_panel_width(Some(&Value::from("260"))), PANEL_WIDTH_DEFAULT);
        assert_eq!(parse_panel_width(Some(&Value::Null)), PANEL_WIDTH_DEFAULT);
    }

    #[test]
    fn right_edge_handle_grows_when_dragged_right() {
        assert_eq!(resize_panel_width(&PanelResize::new(260.0, 260.0, 300.0, ResizeEdge::Right)), 300.0);
    }

    #[test]
    fn left_edge_handle_grows_when_dragged_left() {
        assert_eq!(resize_panel_width(&PanelResize::new(260.0, 800.0, 740.0, ResizeEdge::Left)), 320.0);
    }

    #[test]
    fn resize_clamps_at_the_ends() {
        assert_eq!(resize_panel_width(&PanelResize::new(260.0, 0.0, -400.0, ResizeEdge::Right)), PANEL_WIDTH_MIN);
        assert_eq!(resize_panel_width(&PanelResize::new(260.0, 800.0, 0.0, ResizeEdge::Left)), PANEL_WIDTH_MAX);
    }
}
