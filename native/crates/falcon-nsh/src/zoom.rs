//! 界面缩放（`⌘+` / `⌘−` / `⌘0`）：组件尺寸一律走 [`zpx`]，真实像素留给 [`pt_px`]。
//!
//! 约定与 `CLAUDE.md` 一致：
//! - 界面内间距、行高、圆角、侧栏/面板尺寸写 [`zpx`]，随缩放档位同步放大缩小；
//! - [`pt_px`] 仅用于画布几何运算、终端字符网格像素定位与窗口外框。
//! - 注意：`nsh_core::prelude::Styled` 自带 `.px(..)`（水平内边距），切勿直接导入 `geometry::px`
//!   以免同名遮蔽。

use falcon_core::zoom::{PREF_KEY, percent, snap, step_zoom};
use nsh_core::prelude::{Points, Signal, provide_context, signal, use_context};

use crate::theme::{load_pref_item, save_pref_item};

const DEFAULT_ZOOM: f32 = 1.0;

/// 挂在根组件上下文中的响应式界面缩放控制器。
#[derive(Clone, Copy)]
pub struct ZoomController {
    factor: Signal<f32>,
}

impl ZoomController {
    pub fn new(initial: f32) -> Self {
        Self {
            factor: signal(snap(initial)),
        }
    }

    pub fn get(self) -> f32 {
        self.factor.get()
    }

    pub fn set(self, next: f32) {
        let snapped = snap(next);
        self.factor.set(snapped);
        persist_zoom(snapped);
    }

    pub fn step_in(self) {
        self.set(step_zoom(self.get(), 1));
    }

    pub fn step_out(self) {
        self.set(step_zoom(self.get(), -1));
    }

    pub fn reset(self) {
        self.set(DEFAULT_ZOOM);
    }

    pub fn label(self) -> String {
        percent(self.get())
    }
}

fn load_saved_zoom() -> f32 {
    load_pref_item(PREF_KEY)
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(snap)
        .unwrap_or(DEFAULT_ZOOM)
}

fn persist_zoom(zoom: f32) {
    if (zoom - DEFAULT_ZOOM).abs() < 1e-4 {
        save_pref_item(PREF_KEY, None);
    } else {
        save_pref_item(PREF_KEY, Some(&format!("{zoom:.2}")));
    }
}

/// 在根组件内初始化并提供缩放上下文。
pub fn install_zoom() -> ZoomController {
    let ctrl = ZoomController::new(load_saved_zoom());
    provide_context(ctrl);
    ctrl
}

/// 读取当前表面的缩放控制器（组件树外回退为默认 1.0）。
pub fn current_zoom() -> f32 {
    use_context::<ZoomController>().map_or(DEFAULT_ZOOM, ZoomController::get)
}

/// 随界面缩放档位等比缩放的逻辑尺寸（组件间距、字号、圆角、控件宽高一律用它）。
#[inline]
pub fn zpx(base: f32) -> Points {
    let scale = current_zoom();
    Points((base * scale * 2.0).round() * 0.5)
}

/// 不随界面缩放档位变化的几何点（仅限画布内部坐标、终端网格数学运算、窗口最外层缝隙）。
#[inline]
pub const fn pt_px(raw: f32) -> Points {
    Points(raw)
}
