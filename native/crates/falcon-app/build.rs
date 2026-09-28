//! 构建期把 web 已经 vendor 好的 woff2 解成 TTF，嵌进二进制。
//!
//! GPUI 的 `TextSystem::add_fonts` 不认 WOFF2（macOS 实测 parse error，Windows / Linux 源码里
//! 也没有 WOFF2 解包），而字体资源的唯一来源是 `packages/web/src/assets/fonts/`（由
//! `pnpm vendor-*` 从官方发行包生成、已进仓库）。在这里解压，就不必再往仓库里放一份
//! 商业字体的 TTF，web 重新 vendor 之后原生客户端重编一次即可跟上。

use std::path::{Path, PathBuf};

/// (web 资源里的相对路径, 输出文件名)
const FONTS: &[(&str, &str)] = &[
    ("berkeley-mono/TX-02-Regular.woff2", "TX-02-Regular.ttf"),
    ("berkeley-mono/TX-02-Bold.woff2", "TX-02-Bold.ttf"),
    ("berkeley-mono/TX-02-Oblique.woff2", "TX-02-Oblique.ttf"),
    ("berkeley-mono/TX-02-BoldOblique.woff2", "TX-02-BoldOblique.ttf"),
    ("ioskeley-mono/IoskeleyMonoTermNFM-Latin.woff2", "IoskeleyMonoTerm.ttf"),
    // web 按 unicode-range 拆成拉丁 / CJK 两片是为了浏览器按需下载；原生一次注册整份
    ("maple-mono/MapleMonoNL-NF-CN-Regular.woff2", "MapleMonoNL-NF-CN.ttf"),
    ("nerd-symbols/SymbolsNerdFontMono-Regular.woff2", "SymbolsNerdFontMono.ttf"),
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let fonts_dir = manifest.join("../../../packages/web/src/assets/fonts");
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
            "读不到 {}：{e}。字体由 `pnpm vendor-*` 生成，先在仓库根目录跑对应脚本",
            src.display()
        )
    });
    let mut buf = bytes::Bytes::from(data);
    let ttf = woff2::convert_woff2_to_ttf(&mut buf)
        .unwrap_or_else(|e| panic!("{} 解 woff2 失败：{e:?}", src.display()));
    std::fs::write(dst, ttf).unwrap();
}
