//! 界面缩放的纯档位运算（偏好键 `falcon.native.zoom`）。
//!
//! 界面缩放把根 rem（`16pt × 倍数`）与界面显式尺寸（`zpx`）同步乘上倍数；
//! 画布列/窗口几何（`layout.rs`）、终端画面字号（`term.rs`）与浮动岛圆角/缝隙保持真实像素不参与缩放。

/// 本地偏好里的键名。
pub const PREF_KEY: &str = "falcon.native.zoom";

/// 缩放档位（对齐浏览器 80%–200%）。
pub const STEPS: [f32; 8] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// 吸附到最近的合法档位；非有限值回退到 `1.0`。
pub fn snap(z: f32) -> f32 {
    if !z.is_finite() {
        return 1.0;
    }
    STEPS
        .into_iter()
        .min_by(|a, b| (a - z).abs().total_cmp(&(b - z).abs()))
        .unwrap_or(1.0)
}

/// 从当前缩放倍数按方向走一档（`dir > 0` 放大，`dir < 0` 缩小），到边界停住。
pub fn step_zoom(cur: f32, dir: i32) -> f32 {
    let cur = snap(cur);
    let at = STEPS.iter().position(|s| *s == cur).unwrap_or(2) as i32;
    let next = (at + dir).clamp(0, STEPS.len() as i32 - 1) as usize;
    STEPS[next]
}

/// 设置页与菜单展示的百分比文本（如 `"125%"`）。
pub fn percent(z: f32) -> String {
    format!("{}%", (z * 100.0).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_nearest_step() {
        assert_eq!(snap(1.0), 1.0);
        assert_eq!(snap(1.12), 1.1);
        assert_eq!(snap(1.2), 1.25);
        assert_eq!(snap(9.0), 2.0);
        assert_eq!(snap(0.1), 0.8);
        assert_eq!(snap(f32::NAN), 1.0);
        assert_eq!(snap(f32::INFINITY), 1.0);
    }

    #[test]
    fn steps_clamp_at_bounds() {
        assert_eq!(step_zoom(1.0, 1), 1.1);
        assert_eq!(step_zoom(1.0, -1), 0.9);
        assert_eq!(step_zoom(2.0, 1), 2.0);
        assert_eq!(step_zoom(0.8, -1), 0.8);
    }

    #[test]
    fn percent_label() {
        assert_eq!(percent(1.0), "100%");
        assert_eq!(percent(1.25), "125%");
        assert_eq!(percent(0.9), "90%");
    }
}
