//! Falcon 浏览器版的 wasm 入口。一切都在 falcon-app 里（`falcon_app::run_web`），这里只负责
//! 让 wasm-bindgen 导出一个启动函数给宿主页（`native/web/index.html`）调用。
//!
//! 不用 `#[wasm_bindgen(start)]`：宿主页要先把首帧底色、加载提示铺好，再显式调 [`start`]，
//! 出错时也好在页面上留一句话，而不是一块白屏。

#[cfg(target_family = "wasm")]
use wasm_bindgen::prelude::wasm_bindgen;

#[cfg(target_family = "wasm")]
#[wasm_bindgen]
pub fn start() {
    falcon_app::run_web();
}
