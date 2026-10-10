//! Falcon 浏览器版的 wasm 入口与平台能力的浏览器实现（falcon-platform 的 trait）。界面全在
//! falcon-ui；这里是 localStorage、页面的源、`<a download>` 这些浏览器原生能力
//! （docs/design/rust-unification.md 附录 B）。只在 wasm32 上编译，原生上是个空 lib。
//!
//! 不用 `#[wasm_bindgen(start)]`：宿主页（`native/web/index.html`）要先把首帧底色、加载提示
//! 铺好，再显式调 [`start`]，出错时也好在页面上留一句话，而不是一块白屏。

#![cfg(target_family = "wasm")]

mod dom;
mod platform;

use std::rc::Rc;

use wasm_bindgen::prelude::wasm_bindgen;

/// 单线程 gpui-web（不要 SharedArrayBuffer / COOP·COEP，见设计文档决定七），整个文档一块
/// canvas、一个窗口，连的就是页面所在的 falcon 服务端。
#[wasm_bindgen]
pub fn start() {
    gpui_kit::platform::web_init();

    // 图标 SVG 按需从 `<页面源>/assets/icons/*.svg` 取（gpui-kit-assets 的 wasm 实现），
    // 构建脚本把 gpui-kit-assets 的 icons 拷进产物。它拿 reqwest 去取，相对地址不认，得给全
    let origin = dom::origin();
    gpui_kit::platform::single_threaded_web().with_assets(gpui_kit::assets::Assets::new(origin)).run(|cx| {
        falcon_ui::init(Rc::new(platform::Browser), cx);
        falcon_ui::open_startup_window(cx);
    });
}
