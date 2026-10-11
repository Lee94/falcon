//! 提交图的一格（旧 React 版 GitPanel 的 `GraphCell`）：每行一张独立的小图，泳道与线段来自
//! `falcon_core::git_graph`，这里只把泳道号翻译成坐标、用 GPUI 的 path 画出来。
//!
//! 行高固定（ROW_H），图与行 1:1——React 版为此放弃了可变行高（拉伸 SVG 会把圆点压扁），
//! 这里同理。

use falcon_core::git_graph::{GraphRow, lane_color};
use gpui_kit::prelude::*;
use gpui_kit::{BorderStyle, Bounds, Hsla, IntoElement, PathBuilder, Pixels, canvas, point, px, quad, size};
use crate::zoom::zpx;

pub const ROW_H: f32 = 40.;
pub const LANE_W: f32 = 14.;
const DOT_R: f32 = 3.5;
const STROKE: f32 = 1.5;

fn color(colors: &[Hsla; 6], lane: usize) -> Hsla {
    colors[lane_color(lane) - 1]
}

/// `panel_bg` 给合并提交的空心点填底（React 版的 `var(--sidebar)`：与面板同色才像"空心"）
pub fn graph_cell(row: &GraphRow, colors: [Hsla; 6], panel_bg: Hsla) -> impl IntoElement {
    let row = row.clone();
    let width = row.width.max(1) as f32 * LANE_W;
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            let o = bounds.origin;
            let x = |lane: usize| o.x + zpx(lane as f32 * LANE_W + LANE_W / 2.);
            let mid = zpx(ROW_H / 2.);
            let cx = x(row.lane);
            for seg in &row.segments {
                // from=None 从圆点出发，to=None 汇入圆点；两头都有就是穿过去的直线
                let (x1, y1) = match seg.from {
                    None => (cx, mid),
                    Some(f) => (x(f), px(0.)),
                };
                let (x2, y2) = match seg.to {
                    None => (cx, mid),
                    Some(t) => (x(t), zpx(ROW_H)),
                };
                let mut b = PathBuilder::stroke(zpx(STROKE));
                b.move_to(point(x1, o.y + y1));
                if x1 == x2 {
                    b.line_to(point(x2, o.y + y2));
                } else {
                    // 斜线走三次贝塞尔，控制点取两端的中间高度：直接连直线的话相邻两行的
                    // 折角拼不上，看起来像锯齿
                    let my = o.y + (y1 + y2) / 2.;
                    b.cubic_bezier_to(point(x2, o.y + y2), point(x1, my), point(x2, my));
                }
                if let Ok(path) = b.build() {
                    window.paint_path(path, color(&colors, seg.lane));
                }
            }
            // 圆点：半径 3.5、描边 1.5 骑在半径线上（与 SVG 的 circle 一致），外沿半径 4.25
            let outer = DOT_R + STROKE / 2.;
            let c = color(&colors, row.lane);
            let dot = Bounds::new(
                point(cx - zpx(outer), o.y + mid - zpx(outer)),
                size(zpx(outer * 2.), zpx(outer * 2.)),
            );
            // 合并提交画空心点，与 git 图形客户端的惯例一致
            let fill = if row.merge { panel_bg } else { c };
            window.paint_quad(quad(dot, zpx(outer), fill, zpx(STROKE), c, BorderStyle::default()));
        },
    )
    .flex_none()
    .w(zpx(width))
    .h(zpx(ROW_H))
}
