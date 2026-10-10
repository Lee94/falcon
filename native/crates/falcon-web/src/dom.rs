//! DOM 胶水：gpui-web 平台层没覆盖、又必须走浏览器原生能力的那几件事
//! （docs/design/rust-unification.md 附录 B）。

use wasm_bindgen::JsCast;

/// 页面的源（`https://host:port`）
pub fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

/// 页面的主机名（带端口），标题条上称呼这台服务端
pub fn host() -> String {
    web_sys::window().and_then(|w| w.location().host().ok()).unwrap_or_default()
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

/// 让浏览器把一个同源地址当附件下载（web 的 `lib/fileTransfer.ts` 的 `triggerDownload`）。
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
