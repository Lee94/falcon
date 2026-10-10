//! 构建期把 vendor 好的 woff2 解成 TTF，嵌进二进制（src/fonts.rs；浏览器版只嵌正文字体，
//! 其余由 build-web.sh 从这个 OUT_DIR 拷出去按需拉）。
//!
//! GPUI 的 `TextSystem::add_fonts` 不认 WOFF2（macOS 实测 parse error，Windows / Linux 源码里
//! 也没有 WOFF2 解包），而字体资源的唯一来源是 `native/assets/fonts/`（由 `cargo xtask vendor-fonts`
//! 从官方发行包生成、已进仓库）。在这里解压，就不必再往仓库里放一份商业字体的 TTF，
//! 重新 vendor 之后重编一次即可跟上。

use std::path::{Path, PathBuf};

/// (native/assets/fonts 里的相对路径, 输出文件名)
const FONTS: &[(&str, &str)] = &[
    ("berkeley-mono/TX-02-Regular.woff2", "TX-02-Regular.ttf"),
    ("berkeley-mono/TX-02-Bold.woff2", "TX-02-Bold.ttf"),
    ("berkeley-mono/TX-02-Oblique.woff2", "TX-02-Oblique.ttf"),
    ("berkeley-mono/TX-02-BoldOblique.woff2", "TX-02-BoldOblique.ttf"),
    ("ioskeley-mono/IoskeleyMonoTermNFM-Latin.woff2", "IoskeleyMonoTerm.ttf"),
    ("maple-mono/MapleMonoNL-NF-CN-Regular.woff2", "MapleMonoNL-NF-CN.ttf"),
    ("nerd-symbols/SymbolsNerdFontMono-Regular.woff2", "SymbolsNerdFontMono.ttf"),
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let fonts_dir = manifest.join("../../assets/fonts");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for (src, dst) in FONTS {
        let src_path = fonts_dir.join(src);
        println!("cargo:rerun-if-changed={}", src_path.display());
        convert(&src_path, &out.join(dst));
    }
}

fn convert(src: &Path, dst: &Path) {
    let data = std::fs::read(src).unwrap_or_else(|e| {
        panic!(
            "读不到 {}：{e}。字体由 `cargo xtask vendor-fonts <字族>` 生成，先在 native/ 下跑一遍",
            src.display()
        )
    });
    let mut buf = bytes::Bytes::from(data);
    let ttf = woff2::convert_woff2_to_ttf(&mut buf)
        .unwrap_or_else(|e| panic!("{} 解 woff2 失败：{e:?}", src.display()));
    std::fs::write(dst, ttf).unwrap();
}
