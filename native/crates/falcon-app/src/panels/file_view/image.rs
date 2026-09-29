//! 图片预览（web FileView.tsx 的 ImagePane，ADR 0007 决定四）。
//!
//! 字节走原始字节路由（`rawBase + path`，URL 里带作用域令牌），不经 JSON；解码放后台线程，
//! 解完换上。缩放模型照 web：`fit`（贴合窗口，但不放大小图——16px 的图标撑满屏幕只是一团
//! 马赛克）或一个具体倍率。⌘/Ctrl + 滚轮与触控板捏合缩放、双击在 fit 与 1:1 之间切换、放大后
//! 拖拽平移；缩放以指针位置为锚点——放大时光标底下那个像素不动。
//!
//! web 靠浏览器的滚动容器（scrollLeft / scrollTop）；这里没有现成的，就自己记一个等价的
//! "滚动量"：内容盒 = max(视口, 图 + 两边留白)，图在内容盒里居中，滚动量夹在 [0, 内容 − 视口]。
//! 几何全是纯函数（[`Geometry`]），带单测。

use std::sync::Arc;

use falcon_client::FalconClient;
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    Bounds, Context, CursorStyle, Image, ImageFormat, ImageSource, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, PinchEvent, Pixels, Point, Render, RenderImage,
    ScrollWheelEvent, SharedString, Window, canvas, checkerboard, div, img, px,
};
use rust_i18n::t;

use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

const ZOOM_MIN: f32 = 0.05;
const ZOOM_MAX: f32 = 32.;
/// 贴合时四周留的空（web 容器的 p-6）
const FIT_PAD: f32 = 24.;

fn clamp_zoom(z: f32) -> f32 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Zoom {
    Fit,
    Scale(f32),
}

/// 一帧的几何：视口、图的固有尺寸、倍率、滚动量 → 图在视口里的位置
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    view_w: f32,
    view_h: f32,
    nat_w: f32,
    nat_h: f32,
}

impl Geometry {
    fn fit_scale(&self) -> f32 {
        if self.view_w <= 0. || self.view_h <= 0. || self.nat_w <= 0. || self.nat_h <= 0. {
            return 1.;
        }
        (1f32).min((self.view_w - FIT_PAD * 2.) / self.nat_w).min((self.view_h - FIT_PAD * 2.) / self.nat_h)
    }

    fn scale(&self, zoom: Zoom) -> f32 {
        match zoom {
            Zoom::Fit => self.fit_scale(),
            Zoom::Scale(s) => s,
        }
    }

    /// 内容盒：图比视口小时就是视口（图居中），大时是图加两边留白（能滚）
    fn content(&self, scale: f32) -> (f32, f32) {
        (
            self.view_w.max(self.nat_w * scale + FIT_PAD * 2.),
            self.view_h.max(self.nat_h * scale + FIT_PAD * 2.),
        )
    }

    fn clamp_scroll(&self, scale: f32, s: (f32, f32)) -> (f32, f32) {
        let (cw, ch) = self.content(scale);
        (s.0.clamp(0., (cw - self.view_w).max(0.)), s.1.clamp(0., (ch - self.view_h).max(0.)))
    }

    /// 图的左上角（视口坐标）
    fn image_origin(&self, scale: f32, scroll: (f32, f32)) -> (f32, f32) {
        let (cw, ch) = self.content(scale);
        ((cw - self.nat_w * scale) / 2. - scroll.0, (ch - self.nat_h * scale) / 2. - scroll.1)
    }

    /// 换倍率时保持视口坐标 `at` 底下的那个图上位置不动，返回新的滚动量
    fn anchored_scroll(&self, from: f32, to: f32, scroll: (f32, f32), at: (f32, f32)) -> (f32, f32) {
        let (x0, y0) = self.image_origin(from, scroll);
        let (w0, h0) = (self.nat_w * from, self.nat_h * from);
        let fx = if w0 > 0. { (at.0 - x0) / w0 } else { 0.5 };
        let fy = if h0 > 0. { (at.1 - y0) / h0 } else { 0.5 };
        let (cw, ch) = self.content(to);
        let (w1, h1) = (self.nat_w * to, self.nat_h * to);
        let sx = (cw - w1) / 2. - at.0 + fx * w1;
        let sy = (ch - h1) / 2. - at.1 + fy * h1;
        self.clamp_scroll(to, (sx, sy))
    }
}

enum Status {
    Loading,
    Failed,
    Ready { image: Arc<RenderImage>, nat_w: f32, nat_h: f32 },
}

pub struct ImagePane {
    status: Status,
    zoom: Zoom,
    scroll: (f32, f32),
    /// 视口在窗口里的位置与大小（每帧由 canvas 量回来）
    view: Bounds<Pixels>,
    /// 拖拽平移：按下时的指针位置与滚动量
    drag: Option<(Point<Pixels>, (f32, f32))>,
}

impl ImagePane {
    pub fn new(client: FalconClient, url: String, path: String, cx: &mut Context<Self>) -> Self {
        let svg = cx.svg_renderer();
        let fut = client.raw_bytes(&url);
        cx.spawn(async move |this, cx| {
            let decoded = match fut.await {
                Ok(raw) => {
                    let format = raw
                        .content_type
                        .as_deref()
                        .and_then(|ct| ImageFormat::from_mime_type(ct.split(';').next().unwrap_or("").trim()))
                        .or_else(|| format_by_ext(&path));
                    match format {
                        Some(format) => {
                            let bytes = raw.bytes.to_vec();
                            cx.background_executor()
                                .spawn(async move {
                                    let image = Image::from_bytes(format, bytes).to_image_data(svg).ok()?;
                                    let size = image.size(0);
                                    // SVG 按 2 倍光栅化（清晰），固有尺寸要除回去；位图 1 像素 = 1 逻辑点
                                    let k = if format == ImageFormat::Svg { gpui_kit::SMOOTH_SVG_SCALE_FACTOR } else { 1. };
                                    Some((image, size.width.0 as f32 / k, size.height.0 as f32 / k))
                                })
                                .await
                        }
                        None => None,
                    }
                }
                Err(err) => {
                    log::warn!("图片预览取字节失败：{err}");
                    None
                }
            };
            this.update(cx, |this, cx| {
                this.status = match decoded {
                    Some((image, nat_w, nat_h)) if nat_w > 0. && nat_h > 0. => Status::Ready { image, nat_w, nat_h },
                    _ => Status::Failed,
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self { status: Status::Loading, zoom: Zoom::Fit, scroll: (0., 0.), view: Bounds::default(), drag: None }
    }

    fn geometry(&self) -> Option<Geometry> {
        match &self.status {
            Status::Ready { nat_w, nat_h, .. } => Some(Geometry {
                view_w: self.view.size.width.as_f32(),
                view_h: self.view.size.height.as_f32(),
                nat_w: *nat_w,
                nat_h: *nat_h,
            }),
            _ => None,
        }
    }

    /// 窗口坐标 → 视口坐标；没给就用视口中心（按钮缩放围着中间放大，而不是往左上角缩）
    fn local(&self, at: Option<Point<Pixels>>) -> (f32, f32) {
        match at {
            Some(p) => ((p.x - self.view.origin.x).as_f32(), (p.y - self.view.origin.y).as_f32()),
            None => (self.view.size.width.as_f32() / 2., self.view.size.height.as_f32() / 2.),
        }
    }

    fn zoom_to(&mut self, next: Zoom, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let Some(g) = self.geometry() else { return };
        let next = match next {
            Zoom::Scale(s) => Zoom::Scale(clamp_zoom(s)),
            fit => fit,
        };
        let (from, to) = (g.scale(self.zoom), g.scale(next));
        self.scroll = g.anchored_scroll(from, to, self.scroll, self.local(at));
        self.zoom = next;
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let Some(g) = self.geometry() else { return };
        self.zoom_to(Zoom::Scale(g.scale(self.zoom) * factor), at, cx);
    }

    fn on_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.geometry() else { return };
        // 鼠标滚轮一格是一"行"，按 web 那边一格约 100px 折算；触控板给的是像素
        let delta = e.delta.pixel_delta(px(100.));
        cx.stop_propagation();
        if e.modifiers.platform || e.modifiers.control {
            // GPUI 的 delta.y 与 DOM 的 deltaY 反号：往上滚为正 = 放大
            self.zoom_by((delta.y.as_f32() * 0.0025).exp(), Some(e.position), cx);
            return;
        }
        let scale = g.scale(self.zoom);
        self.scroll = g.clamp_scroll(scale, (self.scroll.0 - delta.x.as_f32(), self.scroll.1 - delta.y.as_f32()));
        cx.notify();
    }

    fn on_pinch(&mut self, e: &PinchEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.zoom_by(1. + e.delta, Some(e.position), cx);
    }

    fn on_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.geometry() else { return };
        if e.click_count >= 2 {
            let next = if self.zoom == Zoom::Fit && g.fit_scale() < 1. { Zoom::Scale(1.) } else { Zoom::Fit };
            self.drag = None;
            self.zoom_to(next, Some(e.position), cx);
            return;
        }
        self.drag = Some((e.position, self.scroll));
    }

    fn on_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((start, from)) = self.drag else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(g) = self.geometry() else { return };
        let scale = g.scale(self.zoom);
        let d = e.position - start;
        self.scroll = g.clamp_scroll(scale, (from.0 - d.x.as_f32(), from.1 - d.y.as_f32()));
        cx.notify();
    }
}

impl Render for ImagePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let view = cx.entity();
        let measure = canvas(
            move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    if this.view != bounds {
                        this.view = bounds;
                        cx.notify();
                    }
                })
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let (image, g) = match (&self.status, self.geometry()) {
            (Status::Ready { image, .. }, Some(g)) => (image.clone(), g),
            (Status::Failed, _) => {
                return div().size_full().child(hint(t!("files.viewImageFailed").to_string(), &ui)).child(measure);
            }
            _ => return div().size_full().child(hint(t!("files.viewLoading").to_string(), &ui)).child(measure),
        };

        let scale = g.scale(self.zoom);
        // 视口变了（拖窗口）之后滚动量可能越界，画的时候夹一下
        self.scroll = g.clamp_scroll(scale, self.scroll);
        let (x, y) = g.image_origin(scale, self.scroll);
        let (w, h) = (g.nat_w * scale, g.nat_h * scale);
        let overflowing = w > g.view_w || h > g.view_h;
        let dragging = self.drag.is_some();

        let picture = img(ImageSource::Render(image))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .rounded(zpx(4.))
            .border_1()
            .border_color(ui.border)
            // 棋盘底衬让透明 PNG 看得出边界
            .bg(checkerboard(ui.muted, 8.));

        let stage = div()
            .id("image-stage")
            .size_full()
            .relative()
            .overflow_hidden()
            .when(overflowing, |d| d.cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::OpenHand }))
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .on_pinch(cx.listener(Self::on_pinch))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
            .child(picture)
            .child(measure);

        div().size_full().relative().child(stage).child(self.zoom_bar(g, scale, &ui, cx))
    }
}

impl ImagePane {
    /// 底部居中的缩放条：尺寸 | − 百分比 + | 1:1 适应窗口
    fn zoom_bar(&self, g: Geometry, scale: f32, ui: &Ui, cx: &mut Context<Self>) -> impl IntoElement {
        let fit = self.zoom == Zoom::Fit;
        let actual = !fit && (scale - 1.).abs() < f32::EPSILON;
        let sep = || div().mx(zpx(2.)).h(zpx(14.)).w(zpx(1.)).bg(ui.border);
        let pressed_bg = ui.muted;
        let text_btn = |id: &'static str, label: SharedString, tip: SharedString, on: bool| {
            let fg = ui.foreground;
            div()
                .id(id)
                .h(zpx(22.))
                .px(zpx(6.))
                .flex()
                .items_center()
                .rounded(zpx(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(pressed_bg).text_color(fg))
                .hover(move |s| s.bg(pressed_bg).text_color(fg))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(label)
        };
        let icon_btn = |id: &'static str, name: IconName, tip: SharedString, disabled: bool| {
            let fg = ui.foreground;
            let b = div()
                .id(id)
                .size(zpx(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(zpx(6.))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(icon(name).size(zpx(14.)));
            if disabled { b.opacity(0.5) } else { b.cursor_pointer().hover(move |s| s.bg(pressed_bg).text_color(fg)) }
        };
        let dims: SharedString = format!("{} × {}", g.nat_w.round(), g.nat_h.round()).into();
        let dims_tip: SharedString = t!("files.imageDims").to_string().into();

        div().absolute().bottom(zpx(12.)).left_0().right_0().flex().justify_center().child(
            div()
                .id("image-zoom-bar")
                .flex()
                .items_center()
                .gap(zpx(2.))
                .px_1()
                .py(zpx(2.))
                .rounded(radius::SM)
                .border_1()
                .border_color(ui.border)
                .bg(ui.popover.opacity(0.9))
                .shadow_sm()
                .text_size(zpx(11.))
                .text_color(ui.muted_foreground)
                // 条上的点击别漏到底下的画布（双击会切缩放、按下会开始拖）
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("image-dims")
                        .px(zpx(6.))
                        .tooltip(move |window, cx| Tooltip::new(dims_tip.clone()).build(window, cx))
                        .child(dims),
                )
                .child(sep())
                .child(
                    icon_btn("image-zoom-out", IconName::ZoomOut, t!("files.zoomOut").to_string().into(), scale <= ZOOM_MIN)
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1. / 1.25, None, cx))),
                )
                .child(
                    text_btn("image-zoom-pct", format!("{}%", (scale * 100.).round()).into(), t!("files.zoomReset").to_string().into(), false)
                        .min_w(zpx(44.))
                        .justify_center()
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_to(Zoom::Fit, None, cx))),
                )
                .child(
                    icon_btn("image-zoom-in", IconName::ZoomIn, t!("files.zoomIn").to_string().into(), scale >= ZOOM_MAX)
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.25, None, cx))),
                )
                .child(sep())
                .child(
                    text_btn("image-zoom-actual", "1:1".into(), t!("files.zoomActual").to_string().into(), actual)
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_to(Zoom::Scale(1.), None, cx))),
                )
                .child(
                    text_btn("image-zoom-fit", t!("files.zoomFit").to_string().into(), t!("files.zoomFit").to_string().into(), fit)
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_to(Zoom::Fit, None, cx))),
                ),
        )
    }
}

fn hint(text: String, ui: &Ui) -> impl IntoElement {
    div().px_4().py_4().text_xs().text_color(ui.muted_foreground).child(text)
}

/// 服务端没给（或给了认不出的）Content-Type 时按扩展名猜
pub(crate) fn format_by_ext(path: &str) -> Option<ImageFormat> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "ico" => ImageFormat::Ico,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(view: (f32, f32), nat: (f32, f32)) -> Geometry {
        Geometry { view_w: view.0, view_h: view.1, nat_w: nat.0, nat_h: nat.1 }
    }

    #[test]
    fn fit_never_upscales_small_images() {
        assert_eq!(g((800., 600.), (16., 16.)).fit_scale(), 1.);
        // 留白 24px：(648 - 48) / 1200 = 0.5
        assert!((g((648., 1000.), (1200., 300.)).fit_scale() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn small_image_is_centered_and_unscrollable() {
        let geo = g((400., 300.), (100., 50.));
        assert_eq!(geo.image_origin(1., (0., 0.)), (150., 125.));
        assert_eq!(geo.clamp_scroll(1., (50., 50.)), (0., 0.));
    }

    #[test]
    fn zoom_keeps_anchor_pixel_still() {
        let geo = g((400., 300.), (1000., 800.));
        let from = geo.fit_scale();
        let at = (100., 80.);
        let s0 = (0., 0.);
        let (x0, y0) = geo.image_origin(from, s0);
        let (fx, fy) = ((at.0 - x0) / (1000. * from), (at.1 - y0) / (800. * from));
        let s1 = geo.anchored_scroll(from, 2., s0, at);
        let (x1, y1) = geo.image_origin(2., s1);
        assert!((x1 + fx * 2000. - at.0).abs() < 0.01);
        assert!((y1 + fy * 1600. - at.1).abs() < 0.01);
    }

    #[test]
    fn scroll_is_clamped_to_content() {
        let geo = g((400., 300.), (1000., 800.));
        // 内容 = 1000 + 48，最多滚 648
        assert_eq!(geo.clamp_scroll(1., (5000., -10.)), (648., 0.));
    }

    #[test]
    fn ext_fallback() {
        assert_eq!(format_by_ext("a/b.SVG"), Some(ImageFormat::Svg));
        assert_eq!(format_by_ext("a/b.avif"), None);
    }
}
