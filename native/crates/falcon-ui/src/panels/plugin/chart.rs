//! 走势图：一条或几条折线，线下铺一层同色的淡填充。点的坐标是 falcon-core 的
//! `plugin::chart_points` 算的（攒满之前线从右边长出来），这里只把它们画成 GPUI 的 path。

use gpui_kit::prelude::*;
use gpui_kit::{Bounds, Hsla, IntoElement, PathBuilder, Pixels, canvas, point, px};

use crate::zoom::zpx;

const STROKE: f32 = 1.25;
/// 线下填充的不透明度：几条线叠在一起时还分得清
const FILL_ALPHA: f32 = 0.14;

/// `lines`：每条线的数据（从旧到新）与颜色；`top` 是纵轴上限；`slots` 是横轴格数（0 = 按点数铺满）
pub fn chart(lines: Vec<(Vec<f64>, Hsla)>, top: f64, slots: usize) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            let o = bounds.origin;
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            for (values, color) in &lines {
                let pts = falcon_core::plugin::chart_points(values, top, slots, w, h);
                // 一个点画不成线；也不画孤零零的一个点，第二次采样两秒就来
                if pts.len() < 2 {
                    continue;
                }
                let at = |(x, y): (f32, f32)| point(o.x + px(x), o.y + px(y));
                let mut fill = PathBuilder::fill();
                fill.move_to(at((pts[0].0, h)));
                for p in &pts {
                    fill.line_to(at(*p));
                }
                fill.line_to(at((pts[pts.len() - 1].0, h)));
                fill.close();
                if let Ok(path) = fill.build() {
                    window.paint_path(path, color.opacity(FILL_ALPHA));
                }
                let mut line = PathBuilder::stroke(zpx(STROKE));
                line.move_to(at(pts[0]));
                for p in &pts[1..] {
                    line.line_to(at(*p));
                }
                if let Ok(path) = line.build() {
                    window.paint_path(path, *color);
                }
            }
        },
    )
    .size_full()
}
