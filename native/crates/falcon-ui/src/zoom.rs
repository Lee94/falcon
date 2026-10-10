//! 界面缩放：浏览器 ⌘+ / ⌘− 的原生等价物。全 App 一个倍数，存在偏好 `falcon.native.zoom`。
//!
//! 界面尺寸有两个来源，缩放两边一起动：
//! - **rem**：组件库（按钮、输入框、菜单、对话框）和我们自己的 `text_xs()` / `p_2()` / `h_8()`
//!   都按 rem 算。gpui-component 的 Root 每帧把 `theme.font_size` 设成窗口的 rem，所以它必须是
//!   16px × 倍数，与浏览器同一个基准。它曾被设成 13 当"正文 13px"用，结果所有 rem 尺寸一起缩成
//!   web 的 81%（text-xs 只剩 9.75px，按钮矮了一截）——正文字号现在显式挂在窗口根上。
//! - **写死的像素**：界面代码里的字号、行高、图标、内边距、列宽一律写 [`zpx`]（px × 倍数）。
//!
//! 不缩的是真实几何与另有设置的东西：画布的列 / 窗口坐标与拖拽（按视口像素排布，同 web）、
//! 终端画面（字号在「终端」设置里单独调）、窗口外框与红绿灯、岛的圆角与缝。
//!
//! 读倍数不需要 cx（界面代码里到处是 `zpx(..)`），所以放在一个原子量里；只在主线程写，
//! 改完 `refresh_windows`——它无视视图缓存，侧栏、右面板这些缓存视图也会按新倍数重排。

use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::{App, Pixels, px};

use crate::prefs::Prefs;

pub const PREF_KEY: &str = "falcon.native.zoom";

/// 档位照浏览器（Chrome 的 80%–200% 那一段）
pub const STEPS: [f32; 8] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

static ZOOM: AtomicU32 = AtomicU32::new(1f32.to_bits());

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

/// 界面像素：设计稿上的 `v` px 乘上当前缩放倍数
pub fn zpx(v: f32) -> Pixels {
    px(v * zoom())
}

/// 落到最近的档位上；读进来的值不认识（手改偏好、旧版本）就回 100%
fn snap(z: f32) -> f32 {
    if !z.is_finite() {
        return 1.;
    }
    STEPS
        .into_iter()
        .min_by(|a, b| (a - z).abs().total_cmp(&(b - z).abs()))
        .unwrap_or(1.)
}

/// 启动时读偏好。要在 theme::init 之前：主题要按倍数设组件库的字号
pub fn init(cx: &mut App) {
    let z = Prefs::global(cx).get(PREF_KEY).and_then(|s| s.parse::<f32>().ok()).map_or(1., snap);
    ZOOM.store(z.to_bits(), Ordering::Relaxed);
}

pub fn set(z: f32, cx: &mut App) {
    let z = snap(z);
    if z == zoom() {
        return;
    }
    ZOOM.store(z.to_bits(), Ordering::Relaxed);
    cx.global_mut::<Prefs>().set(PREF_KEY, z.to_string());
    // 组件库的字号（= rem）跟着倍数重设，顺带 refresh_windows
    crate::theme::reapply(cx);
}

/// 往大 / 往小走一档（到头了不动）
pub fn step(dir: i32, cx: &mut App) {
    let cur = zoom();
    let at = STEPS.iter().position(|s| *s == cur).unwrap_or(2) as i32;
    let next = (at + dir).clamp(0, STEPS.len() as i32 - 1) as usize;
    set(STEPS[next], cx);
}

pub fn reset(cx: &mut App) {
    set(1., cx);
}

/// 设置页上显示的百分比
pub fn percent(z: f32) -> String {
    format!("{}%", (z * 100.).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_nearest_step() {
        assert_eq!(snap(1.), 1.);
        assert_eq!(snap(1.12), 1.1);
        assert_eq!(snap(1.2), 1.25);
        assert_eq!(snap(9.), 2.);
        assert_eq!(snap(0.1), 0.8);
        assert_eq!(snap(f32::NAN), 1.);
    }

    #[test]
    fn percent_label() {
        assert_eq!(percent(1.), "100%");
        assert_eq!(percent(1.25), "125%");
        assert_eq!(percent(0.9), "90%");
    }
}
