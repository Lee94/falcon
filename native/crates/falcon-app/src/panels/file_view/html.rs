//! HTML 预览：原生 WebView（wry / WKWebView）加载原始字节地址（ADR 0007，设计文档 §4.4）。
//!
//! 安全边界与 web 的沙箱 iframe 等价，逐条对应：
//! - **不带登录 cookie**：无痕（非持久）数据存储，WebView 与 falcon-client 的 cookie 罐本来就
//!   不相通；凭据只有 URL 里那枚**只能读这个项目文件**的作用域令牌。页面里的脚本拿到它，能做的
//!   也只是读同项目的其它文件（ADR 0007 决定二）；
//! - **只准在同一 raw 前缀内导航**：页面里点到别处，WebView 里不跳；是 http(s) 就交给系统
//!   浏览器（页面加载完之后才转交——加载途中的 iframe 也走导航回调，那时转交会平白弹出浏览器）；
//!   `window.open` / `target=_blank` 同样交给系统浏览器；下载一律拒绝；
//! - 原始字节路由自己的 CSP `sandbox` 响应头照样生效（页面跑在 opaque origin 里）。
//!
//! WebView 是压在 GPUI 画面**上面**的原生视图，GPUI 的任何浮层都盖不住它。所以：对话框 /
//! 抽屉开着时藏起来；画布把它滚出可视区时只露出与可视区相交的那一块；所在的窗口不在场
//! （换了项目、别的窗口最大化）时由 FileView 调 [`HtmlPane::set_shown`] 藏起来。右键菜单、
//! 下拉菜单这类 popover 没有统一的"开着吗"可查，仍会被它挡住。
//!
//! WebView 挂在 falcon-app 默认开启的 `webview` feature 上；关掉时退成"在浏览器中打开"的
//! 提示——原始字节地址本来就能直接在浏览器里打开（CSP sandbox 罩着）。

#[cfg(feature = "webview")]
pub use imp::HtmlPane;

#[cfg(not(feature = "webview"))]
pub use fallback::HtmlPane;

/// 同一 raw 前缀下的地址（相对引用解析出来的 CSS / 页面跳转）放行；about:blank / srcdoc 这类
/// 不出网的也放行（页面自己造的空 iframe）
#[cfg_attr(not(feature = "webview"), allow(dead_code))]
fn in_scope(url: &str, prefix: &str) -> bool {
    url.starts_with(prefix) || url == "about:blank" || url == "about:srcdoc"
}

#[cfg_attr(not(feature = "webview"), allow(dead_code))]
fn is_web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(feature = "webview")]
mod imp {
    use std::cell::Cell;
    use std::rc::Rc;

    use futures::StreamExt;
    use gpui_kit::component::WindowExt;
    use gpui_kit::prelude::*;
    use gpui_kit::{
        App, Bounds, Context, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
        LayoutId, MouseDownEvent, Pixels, Render, Size, Style, Task, Window, div,
    };
    use rust_i18n::t;
    use wry::{NewWindowResponse, PageLoadEvent, Rect, WebViewBuilder, dpi};

    use super::{in_scope, is_web};
    use crate::zoom::zpx;

    pub struct HtmlPane {
        webview: Option<Rc<wry::WebView>>,
        error: Option<String>,
        shown: bool,
        _opener: Task<()>,
    }

    impl HtmlPane {
        /// `url` 是完整的原始字节地址，`prefix` 是这个项目的 raw 前缀（`基址 + rawBase`）
        pub fn new(url: String, prefix: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
            // 导航回调在主线程上同步跑，拿不到 App；要开浏览器的地址经通道交给一个任务
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
            let loaded = Rc::new(Cell::new(false));
            let (tx_nav, loaded_nav) = (tx.clone(), loaded.clone());
            let built = WebViewBuilder::new()
                .with_url(&url)
                .with_incognito(true)
                .with_devtools(false)
                .with_visible(false)
                // 画布默认色而不是界面色：没设背景的 HTML 在浏览器里就是白底黑字（ADR 0007 已知取舍）
                .with_background_color((255, 255, 255, 255))
                .with_navigation_handler(move |u| {
                    if in_scope(&u, &prefix) {
                        return true;
                    }
                    if loaded_nav.get() && is_web(&u) {
                        let _ = tx_nav.unbounded_send(u);
                    }
                    false
                })
                .with_on_page_load_handler(move |ev, _| loaded.set(matches!(ev, PageLoadEvent::Finished)))
                .with_new_window_req_handler(move |u, _| {
                    if is_web(&u) {
                        let _ = tx.unbounded_send(u);
                    }
                    NewWindowResponse::Deny
                })
                .with_download_started_handler(|_, _| false)
                .build_as_child(window);
            let (webview, error) = match built {
                Ok(w) => (Some(Rc::new(w)), None),
                Err(err) => {
                    log::error!("HTML 预览建 WebView 失败：{err}");
                    (None, Some(err.to_string()))
                }
            };
            let opener = cx.spawn(async move |_, cx| {
                while let Some(u) = rx.next().await {
                    cx.update(|cx| cx.open_url(&u));
                }
            });
            Self { webview, error, shown: true, _opener: opener }
        }

        /// 所在窗口不在画布上时藏起来（元素不画就不会再去摆 WebView 的位置，它会留在原处）
        pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
            if self.shown != shown {
                self.shown = shown;
                if !shown && let Some(w) = &self.webview {
                    let _ = w.focus_parent();
                    let _ = w.set_visible(false);
                }
                cx.notify();
            }
        }
    }

    impl Drop for HtmlPane {
        fn drop(&mut self) {
            if let Some(w) = &self.webview {
                let _ = w.focus_parent();
                let _ = w.set_visible(false);
            }
        }
    }

    impl Render for HtmlPane {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if let Some(err) = &self.error {
                return div()
                    .size_full()
                    .px_4()
                    .py_4()
                    .text_xs()
                    .text_color(crate::theme::Ui::global(cx).muted_foreground)
                    .child(t!("files.viewFailed").to_string())
                    .child(div().mt_1().text_size(zpx(11.)).child(err.clone()))
                    .into_any_element();
            }
            let overlay = window.has_active_dialog(cx) || window.has_active_sheet(cx);
            HtmlSurface { view: self.webview.clone(), hidden: overlay || !self.shown }.into_any_element()
        }
    }

    /// 占位元素：自己不画东西，只在每帧把 WebView 摆到它的位置（与可视区相交的那一块）
    struct HtmlSurface {
        view: Option<Rc<wry::WebView>>,
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
            let view = self.view.as_ref()?;
            // 原生视图不受 GPUI 的裁剪管：画布横向滚动把窗口滚出去一半时，只摆可见的那一块
            let visible = bounds.intersect(&window.content_mask().bounds);
            if self.hidden || visible.size.width <= Pixels::ZERO || visible.size.height <= Pixels::ZERO {
                let _ = view.set_visible(false);
                return None;
            }
            let _ = view.set_bounds(Rect {
                size: dpi::Size::Logical(dpi::LogicalSize::new(visible.size.width.into(), visible.size.height.into())),
                position: dpi::Position::Logical(dpi::LogicalPosition::new(visible.origin.x.into(), visible.origin.y.into())),
            });
            let _ = view.set_visible(true);
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
            let (Some(view), Some(bounds)) = (self.view.clone(), *visible) else { return };
            // 点到 WebView 外面时把键盘焦点还给 GPUI（否则敲字还进网页里）
            window.on_mouse_event(move |e: &MouseDownEvent, _, _, _| {
                if !bounds.contains(&e.position) {
                    let _ = view.focus_parent();
                }
            });
        }
    }
}

#[cfg(not(feature = "webview"))]
mod fallback {
    use gpui_kit::prelude::*;
    use gpui_kit::{Context, IntoElement, Render, Window, div};
    use rust_i18n::t;

    use crate::theme::Ui;

    pub struct HtmlPane;

    impl HtmlPane {
        pub fn new(_url: String, _prefix: String, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
            Self
        }

        pub fn set_shown(&mut self, _shown: bool, _cx: &mut Context<Self>) {}
    }

    impl Render for HtmlPane {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .px_4()
                .py_4()
                .text_xs()
                .text_color(Ui::global(cx).muted_foreground)
                .child(t!("native.files.htmlNoWebview").to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_the_raw_prefix() {
        let prefix = "http://127.0.0.1:4933/api/projects/p1/raw/tok/";
        assert!(in_scope("http://127.0.0.1:4933/api/projects/p1/raw/tok/docs/a.html", prefix));
        assert!(!in_scope("http://127.0.0.1:4933/api/projects", prefix));
        assert!(!in_scope("http://127.0.0.1:4933/api/projects/p1/raw/other/a.html", prefix));
        assert!(in_scope("about:blank", prefix));
        assert!(is_web("HTTPS://example.com"));
        assert!(!is_web("javascript:alert(1)"));
    }
}
