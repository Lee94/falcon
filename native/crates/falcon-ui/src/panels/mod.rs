//! 右侧栏的面板（文件 / 修改 / 插件）与画布上的文件、差异窗口。转发与公网发布
//! 挪进了设置的「中转」页（dialogs/settings/relays.rs，ADR 0016）。
//!
//! 每个面板都是一个 GPUI 视图：`new(ws, window, cx)` 建出来，observe 工作区自己刷新。
//! 右侧栏开着时切走不卸载，关掉右侧栏才卸。

pub mod changes;
pub mod diff_view;
pub mod file_view;
pub mod files;
pub mod git;
pub mod plugin;
pub mod widgets;

/// 分栏主区的像素长度。`ratio` 是主区占（去掉把手之后）可用长度的比例。
/// 两侧都够 `min_*` 时主区限制在这个范围里；总长不够时按比例分，不再硬撑到下限。
pub(crate) fn split_primary(total: f32, ratio: f32, handle: f32, min_primary: f32, min_secondary: f32) -> f32 {
    if !total.is_finite() || total <= 0. {
        return 0.;
    }
    let ratio = if ratio.is_finite() { ratio.clamp(0., 1.) } else { 0.5 };
    let available = (total - handle.max(0.)).max(0.);
    let min_primary = min_primary.max(0.);
    let min_secondary = min_secondary.max(0.);
    if available <= min_primary + min_secondary {
        return available * ratio;
    }
    (available * ratio).clamp(min_primary, available - min_secondary)
}

#[cfg(test)]
mod tests {
    use super::split_primary;

    #[test]
    fn split_primary_uses_ratio_inside_the_mins() {
        assert_eq!(split_primary(400., 0.5, 6., 100., 100.), 197.);
        assert_eq!(split_primary(400., 0.25, 0., 80., 80.), 100.);
    }

    #[test]
    fn split_primary_clamps_each_side() {
        assert_eq!(split_primary(400., 0.1, 0., 100., 120.), 100.);
        assert_eq!(split_primary(400., 0.95, 0., 100., 120.), 280.);
    }

    #[test]
    fn split_primary_shares_when_the_pane_is_shorter_than_both_mins() {
        assert_eq!(split_primary(220., 0.5, 6., 132., 120.), 107.);
    }

    #[test]
    fn split_primary_rejects_non_finite_input() {
        assert_eq!(split_primary(f32::NAN, 0.5, 6., 10., 10.), 0.);
        assert_eq!(split_primary(200., f32::NAN, 0., 10., 10.), 100.);
        assert_eq!(split_primary(0., 0.5, 0., 10., 10.), 0.);
    }
}
