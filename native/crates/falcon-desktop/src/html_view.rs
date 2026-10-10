//! HTML 预览的原生网页视图：wry（macOS 上是 WKWebView）加载原始字节地址（ADR 0007，设计文档 §4.4）。
//! 摆位置与显隐在 falcon-ui 的 `panels/file_view/html.rs`，这里只建视图、守安全边界。
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
//! 挂在默认开启的 `webview` feature 上；关掉时界面退成"在浏览器中打开"的提示。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use falcon_platform::HtmlView;
use gpui_kit::{Bounds, Pixels, Window};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebViewBuilder, dpi};

struct WryView(wry::WebView);

impl HtmlView for WryView {
    fn set_visible(&self, visible: bool) {
        let _ = self.0.set_visible(visible);
    }

    fn set_bounds(&self, b: Bounds<Pixels>) {
        let _ = self.0.set_bounds(Rect {
            size: dpi::Size::Logical(dpi::LogicalSize::new(b.size.width.into(), b.size.height.into())),
            position: dpi::Position::Logical(dpi::LogicalPosition::new(b.origin.x.into(), b.origin.y.into())),
        });
    }

    fn focus_parent(&self) {
        let _ = self.0.focus_parent();
    }
}

/// `url` 是完整的原始字节地址，`prefix` 是这个项目的 raw 前缀（`基址 + rawBase`）
pub fn create(
    url: &str,
    prefix: &str,
    open_external: Box<dyn Fn(String) + Send + Sync>,
    window: &mut Window,
) -> Result<Rc<dyn HtmlView>, String> {
    let open_external: Arc<dyn Fn(String) + Send + Sync> = Arc::from(open_external);
    let loaded = Rc::new(Cell::new(false));
    let prefix = prefix.to_string();
    let (open_nav, loaded_nav) = (open_external.clone(), loaded.clone());
    WebViewBuilder::new()
        .with_url(url)
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
                open_nav(u);
            }
            false
        })
        .with_on_page_load_handler(move |ev, _| loaded.set(matches!(ev, PageLoadEvent::Finished)))
        .with_new_window_req_handler(move |u, _| {
            if is_web(&u) {
                open_external(u);
            }
            NewWindowResponse::Deny
        })
        .with_download_started_handler(|_, _| false)
        .build_as_child(window)
        .map(|w| Rc::new(WryView(w)) as Rc<dyn HtmlView>)
        .map_err(|err| err.to_string())
}

/// 同一 raw 前缀下的地址（相对引用解析出来的 CSS / 页面跳转）放行；about:blank / srcdoc 这类
/// 不出网的也放行（页面自己造的空 iframe）
fn in_scope(url: &str, prefix: &str) -> bool {
    url.starts_with(prefix) || url == "about:blank" || url == "about:srcdoc"
}

fn is_web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
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
