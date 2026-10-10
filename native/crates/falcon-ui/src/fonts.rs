//! 内嵌字体：启动时一次注册完，没有 web 那套"字到了再重建图集"的等待。
//!
//! 字体栈照 web 的 `lib/term.ts` / `styles.css`：界面与终端默认都是 Berkeley Mono（TX-02），
//! 缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
//!
//! 与 web 的一处差别：web 的 CSS 字体栈把 Symbols Nerd Font Mono 排在**主字体前面**——Maple
//! 自带的 NF 图标是宽形，xterm 把 PUA 当 1 格且拒绝缩放，图标会被裁掉。GPUI 永远先查主字体，
//! 而且拒绝把没有 `m` 字形的字体当主字体（实测日志 "has no 'm' character and was not loaded"，
//! Symbols 正是这种纯图标字体），所以把它放在**回退链第一位**：主字体缺的码位先查图标字体，
//! 效果与 web 的"图标排最前"相同（Berkeley 本身不含 Nerd 图标）。

use std::borrow::Cow;

use falcon_platform::FontSource;
use gpui_kit::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, SharedString};

/// 内嵌字体的 family 名（取自字体文件的 name 表，改字体前先确认）。
pub const BERKELEY: &str = "TX-02";
pub const IOSKELEY: &str = "IoskeleyMonoTerm Nerd Font Mono";
pub const MAPLE: &str = "Maple Mono NL NF CN";
pub const NERD_SYMBOLS: &str = "Symbols Nerd Font Mono";

/// build.rs 解好放在 OUT_DIR 里的 TTF
macro_rules! embedded {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name)).as_slice()
    };
}

/// 注册字体。正文字体（TX-02 四个字重，共约 330KB）永远嵌着；回退字体看平台
/// （[`FontSource`]）：原生把嵌着的字节交进来，浏览器版启动后按需拉——wasm 要整个下载完才能
/// 启动，嵌全套会多出 24MB（Maple 中文一份就 21MB）
pub fn register(cx: &mut App) {
    let mut fonts: Vec<Cow<'static, [u8]>> = [
        embedded!("TX-02-Regular.ttf"),
        embedded!("TX-02-Bold.ttf"),
        embedded!("TX-02-Oblique.ttf"),
        embedded!("TX-02-BoldOblique.ttf"),
    ]
    .into_iter()
    .map(Cow::Borrowed)
    .collect();
    let source = falcon_platform::get(cx).font_source();
    let base_url = match source {
        FontSource::Embedded(fallbacks) => {
            fonts.extend(fallbacks.into_iter().map(Cow::Borrowed));
            None
        }
        FontSource::Fetch { base_url } => Some(base_url),
    };
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        log::error!("注册内嵌字体失败：{err:#}");
    }
    if let Some(base_url) = base_url {
        fetch_fallbacks(&base_url, cx);
    }
}

/// 嵌进二进制的回退字体，给原生平台交回 [`FontSource::Embedded`]。界面层自己不调它：
/// 浏览器版没人引用，链接时整段丢掉
pub fn embedded_fallbacks() -> Vec<&'static [u8]> {
    vec![
        embedded!("IoskeleyMonoTerm.ttf"),
        embedded!("MapleMonoNL-NF-CN.ttf"),
        embedded!("SymbolsNerdFontMono.ttf"),
    ]
}

/// 回退字体从 `<base>/fonts/*.ttf` 拉（浏览器版的构建脚本从 OUT_DIR 拷进产物），到了再注册、
/// 重画——到之前缺的字形由 gpui-web 的 Canvas 回落顶着，与 web 按 unicode-range 懒加载同一个思路
fn fetch_fallbacks(base_url: &str, cx: &mut App) {
    // 小的先到：图标与拉丁回退几百 KB，中文 21MB 放最后
    for name in ["SymbolsNerdFontMono.ttf", "IoskeleyMonoTerm.ttf", "MapleMonoNL-NF-CN.ttf"] {
        let url = format!("{base_url}/fonts/{name}");
        let http = cx.http_client();
        cx.spawn(async move |cx| {
            use futures::AsyncReadExt as _;
            let started = web_time::Instant::now();
            let bytes = async {
                let mut resp = http.get(&url, Default::default(), true).await?;
                anyhow::ensure!(resp.status().is_success(), "HTTP {}", resp.status());
                let mut buf = Vec::new();
                resp.body_mut().read_to_end(&mut buf).await?;
                anyhow::Ok(buf)
            }
            .await;
            match bytes {
                Ok(bytes) => {
                    let len = bytes.len();
                    cx.update(|cx| {
                        if let Err(err) = cx.text_system().add_fonts(vec![Cow::Owned(bytes)]) {
                            log::error!("注册字体 {name} 失败：{err:#}");
                        }
                        cx.refresh_windows();
                    });
                    log::info!("字体 {name} 到位：{len} 字节，{} ms", started.elapsed().as_millis());
                }
                Err(err) => log::warn!("拉字体 {url} 失败：{err:#}"),
            }
        })
        .detach();
    }
}

/// 主字体之后的回退链。系统 CJK / emoji 兜 Maple 没覆盖的生僻字、假名、谚文、emoji——与
/// web 的 FALLBACK_STACK 同一个理由（Maple CN 不是全集）。
pub fn fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(vec![
        NERD_SYMBOLS.to_string(),
        IOSKELEY.to_string(),
        MAPLE.to_string(),
        "PingFang SC".to_string(),
        "Hiragino Sans GB".to_string(),
        "Apple Color Emoji".to_string(),
    ])
}

/// 终端 / 界面用的等宽字体。`family` 是用户在设置里选的（默认 Berkeley）。
pub fn mono(family: impl Into<SharedString>) -> Font {
    Font {
        family: family.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: Some(fallbacks()),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}
