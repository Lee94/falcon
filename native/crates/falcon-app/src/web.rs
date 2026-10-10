//! 浏览器版的 DOM 胶水：gpui-web 平台层没覆盖、又必须走浏览器原生能力的那几件事
//! （docs/design/rust-unification.md 附录 B）。只在 wasm32 上编译。

use wasm_bindgen::JsCast;

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
