//! DOM 胶水：gpui-web 平台层没覆盖、又必须走浏览器原生能力的那几件事
//! （docs/design/rust-unification.md 附录 B）。

use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

/// 页面的源（`https://host:port`）
pub fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

/// 页面的主机名（带端口），标题条上称呼这台服务端
pub fn host() -> String {
    web_sys::window().and_then(|w| w.location().host().ok()).unwrap_or_default()
}

/// 页面地址的查询串（`location.search`，含开头的 `?`；没有就是空串）
pub fn location_search() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// 换到同源的另一个地址，不留历史记录（`location.replace`）。成功发起才返回 true
pub fn location_replace(url: &str) -> bool {
    web_sys::window().is_some_and(|w| w.location().replace(url).is_ok())
}

/// 网络恢复（window 的 `online`）与标签页切回可见（document 的 `visibilitychange` 且不再
/// hidden）时调 `f`。监听跟页面同寿，闭包 `forget` 掉不摘
pub fn on_resume(f: Box<dyn Fn()>) {
    let Some(window) = web_sys::window() else { return };
    let f: Rc<dyn Fn()> = Rc::from(f);
    let online = {
        let f = f.clone();
        Closure::<dyn FnMut()>::new(move || f())
    };
    let _ = window.add_event_listener_with_callback("online", online.as_ref().unchecked_ref());
    online.forget();
    let Some(doc) = window.document() else { return };
    let visible = {
        let doc = doc.clone();
        Closure::<dyn FnMut()>::new(move || {
            if !doc.hidden() {
                f();
            }
        })
    };
    let _ = doc.add_event_listener_with_callback("visibilitychange", visible.as_ref().unchecked_ref());
    visible.forget();
}

/// 浏览器的 localStorage。拿不到（隐私模式、被禁用）时返回 `None`，读写都安静地跳过
pub fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// 浏览器所在的操作系统：`(mac, windows)`。`navigator.platform` 已不推荐，但各家都还给、
/// 也够分辨这两家（键位习惯只看这个）
pub fn os() -> (bool, bool) {
    let platform = web_sys::window().and_then(|w| w.navigator().platform().ok()).unwrap_or_default();
    (platform.starts_with("Mac"), platform.starts_with("Win"))
}

/// 让浏览器把一个同源地址当附件下载（旧 React 版 `lib/fileTransfer.ts` 的 `triggerDownload`）。
/// `<a download>` 点击：不换页、不开新标签、不经 fetch 把整个文件攒进内存——浏览器自己的
/// 下载管理器接手，进度与取消都有。响应带 Content-Disposition，文件名以那边的为准
/// （download 属性留空）。必须在用户手势的同步调用栈里调，否则可能被当成弹窗拦掉。
pub fn trigger_download(url: &str) -> Result<(), String> {
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("没有 document")?;
    let body = doc.body().ok_or("没有 body")?;
    let a = doc
        .create_element("a")
        .map_err(|e| format!("{e:?}"))?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "建 <a> 失败")?;
    a.set_href(url);
    a.set_download("");
    a.set_rel("noopener");
    let _ = a.style().set_property("display", "none");
    body.append_child(&a).map_err(|e| format!("{e:?}"))?;
    a.click();
    a.remove();
    Ok(())
}
