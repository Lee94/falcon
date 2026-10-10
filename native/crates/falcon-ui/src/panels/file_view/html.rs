//! HTML 预览：平台给一个压在画面上面的网页视图（原生是 wry / WKWebView，见 falcon-desktop 的
//! html_view.rs），加载原始字节地址（ADR 0007，设计文档 §4.4）。安全边界（不带登录 cookie、
//! 只准在同一 raw 前缀内导航、下载一律拒绝）由平台的实现守，这里只管摆位置与显隐。
//!
//! 网页视图是压在 GPUI 画面**上面**的原生视图，GPUI 的任何浮层都盖不住它。所以：对话框 /
//! 抽屉开着时藏起来；画布把它滚出可视区时只露出与可视区相交的那一块；所在的窗口不在场
//! （换了项目、别的窗口最大化）时由 FileView 调 [`HtmlPane::set_shown`] 藏起来。右键菜单、
//! 下拉菜单这类 popover 没有统一的"开着吗"可查，仍会被它挡住。
//!
//! 平台没有网页视图时（关了 `webview` feature 的原生构建、浏览器版在 C3 补上 iframe 之前）
//! 退成"在浏览器中打开"的提示——原始字节地址本来就能直接在浏览器里打开（CSP sandbox 罩着）。

use std::rc::Rc;

use falcon_platform::HtmlView;
use futures::StreamExt;
use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    MouseDownEvent, Pixels, Render, Size, Style, Task, Window, div,
};
use rust_i18n::t;

use crate::theme::Ui;
use crate::zoom::zpx;

enum Surface {
    View(Rc<dyn HtmlView>),
    /// 平台没有网页视图
    Unsupported,
    /// 建网页视图失败
    Failed(String),
}

pub struct HtmlPane {
    surface: Surface,
    shown: bool,
    _opener: Task<()>,
}

impl HtmlPane {
    /// `url` 是完整的原始字节地址，`prefix` 是这个项目的 raw 前缀（`基址 + rawBase`）
    pub fn new(url: String, prefix: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 平台的导航回调在主线程上同步跑，拿不到 App；要开浏览器的地址经通道交给一个任务
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let open_external = Box::new(move |u: String| {
            let _ = tx.unbounded_send(u);
        });
        let surface = match falcon_platform::get(cx).html_view(&url, &prefix, open_external, window) {
            None => Surface::Unsupported,
            Some(Ok(view)) => Surface::View(view),
            Some(Err(err)) => {
                log::error!("HTML 预览建网页视图失败：{err}");
                Surface::Failed(err)
            }
        };
        let opener = cx.spawn(async move |_, cx| {
            while let Some(u) = rx.next().await {
                cx.update(|cx| cx.open_url(&u));
            }
        });
        Self { surface, shown: true, _opener: opener }
    }

    /// 所在窗口不在画布上时藏起来（元素不画就不会再去摆网页视图的位置，它会留在原处）
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            if !shown && let Surface::View(view) = &self.surface {
                view.focus_parent();
                view.set_visible(false);
            }
            cx.notify();
        }
    }
}

impl Drop for HtmlPane {
    fn drop(&mut self) {
        if let Surface::View(view) = &self.surface {
            view.focus_parent();
            view.set_visible(false);
        }
    }
}

impl Render for HtmlPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx);
        match &self.surface {
            Surface::Unsupported => div()
                .px_4()
                .py_4()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(t!("native.files.htmlNoWebview").to_string())
                .into_any_element(),
            Surface::Failed(err) => div()
                .size_full()
                .px_4()
                .py_4()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(t!("files.viewFailed").to_string())
                .child(div().mt_1().text_size(zpx(11.)).child(err.clone()))
                .into_any_element(),
            Surface::View(view) => {
                let overlay = window.has_active_dialog(cx) || window.has_active_sheet(cx);
                HtmlSurface { view: view.clone(), hidden: overlay || !self.shown }.into_any_element()
            }
        }
    }
}

/// 占位元素：自己不画东西，只在每帧把网页视图摆到它的位置（与可视区相交的那一块）
struct HtmlSurface {
    view: Rc<dyn HtmlView>,
    hidden: bool,
}

impl IntoElement for HtmlSurface {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for HtmlSurface {
    type RequestLayoutState = ();
    type PrepaintState = Option<Bounds<Pixels>>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style { size: Size::full(), flex_shrink: 1., ..Default::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        // 原生视图不受 GPUI 的裁剪管：画布横向滚动把窗口滚出去一半时，只摆可见的那一块
        let visible = bounds.intersect(&window.content_mask().bounds);
        if self.hidden || visible.size.width <= Pixels::ZERO || visible.size.height <= Pixels::ZERO {
            self.view.set_visible(false);
            return None;
        }
        self.view.set_bounds(visible);
        self.view.set_visible(true);
        Some(visible)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        visible: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(bounds) = *visible else { return };
        let view = self.view.clone();
        // 点到网页视图外面时把键盘焦点还给 GPUI（否则敲字还进网页里）
        window.on_mouse_event(move |e: &MouseDownEvent, _, _, _| {
            if !bounds.contains(&e.position) {
                view.focus_parent();
            }
        });
    }
}
