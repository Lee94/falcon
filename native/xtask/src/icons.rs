//! 内置应用图标（ADR 0018）：图形定义与各种形状（原 scripts/app-icons.mjs）+ 出全部图标文件
//! （原 scripts/gen-icons.mjs）。`cargo xtask pkg` 也用这里的形状现画安装包的 AppIcon。
//!
//! 每个图标 = 底色 `bg`（SVG fill，可以是渐变引用）+ 画在 1024 网格上的 `mark`（+ 可选 `defs`）。
//! id 列表与 falcon-core 的 `APP_ICON_IDS`（native/crates/falcon-core/src/app_icon.rs）一一对应
//! （falcon-ui 有测试对账）。光栅化用 rsvg-convert（librsvg）。
//!
//! 生成的 SVG 文本与原 JS 逐字相同（数字按 JS 的 `${x}` 规则输出：Rust 的 f64 Display 也是
//! 最短往返表示），同一个 rsvg-convert 画出来的 PNG 因此也相同。
//!
//! 出的文件：
//! - `native/web/icons/<id>/icon-{192,512}.png`       圆角方块：浏览器版的标签页图标（192）、Windows exe 的图标（512）
//! - `native/crates/falcon-ui/assets/app-icons/<id>.png`   原生客户端运行时的 Dock 图标（macOS 版式）
//!
//! 标签页图标用 PNG 不用 SVG：默认图标是栅格插画，塞进 SVG 就是一个 1MB 多的 favicon。
//! 安装包的 AppIcon.icns 由 `cargo xtask pkg` 直接从这里现画，不在这里出。PWA 用的 maskable
//! 与 iOS 主屏幕图标（满版方块）随 PWA 在 2026-10-11 删掉了。

use std::path::Path;

use anyhow::{Result, bail};
use base64::Engine as _;

use crate::util::{native, pipe, require_tool, root};

/// 默认图标。必须与 falcon-core 的 DEFAULT_APP_ICON 相同
pub const DEFAULT_ID: &str = "emberwing";

pub struct Icon {
    bg: String,
    mark: String,
    defs: Option<String>,
}

// ---- 橙翼（设计稿「闪电鸟 | 品牌视觉完整设计稿」v02）----
// 标志原图是 1254 见方的深色底栅格插画（设计稿自己说明没有矢量稿与 32px 简化版），
// 原样存在 native/xtask/assets/app-icons/emberwing.png。图标规范：深炭黑容器里，原方图居中缩至 0.80 S。
// 容器取 #0F0F0F 而不是稿里的 #101010：原图外圈实测均值 rgb(15.5,15.2,14.9)，
// 用 #101010 在把暗部拉亮后能看出一圈方块边，#0F0F0F 与它差不到半个色阶。
fn emberwing() -> Icon {
    let png = std::fs::read(native().join("xtask/assets/app-icons/emberwing.png")).expect("emberwing.png");
    let png = base64::engine::general_purpose::STANDARD.encode(png);
    let side = 1024.0 * 0.8;
    let at = (1024.0 - side) / 2.0;
    Icon {
        bg: "#0F0F0F".into(),
        mark: format!(r#"<image href="data:image/png;base64,{png}" x="{at}" y="{at}" width="{side}" height="{side}"/>"#),
        defs: None,
    }
}

// ---- 电翼（设计稿「闪电鸟：电翼」，1024 网格）----
// 鸟举起一道闪电当翅膀。头是半径 118.7 的正圆，喙上缘与它相切；背线由尾尖向头圆作切线，
// 腹线是一段二次曲线；眼睛半径 23.7。数值照设计稿原样，别手调。
const INDIGO: &str = "#1D1A45";
const VOLT: &str = "#FFD60A";
const IVORY: &str = "#FFF6DD";
const NIGHT: &str = "#0F0D26";
const WING: &str = "M351.8 751.9L560.8 751.9L411.2 501.2L536.5 501.2L197 182.1L325.7 432.8L189.9 432.8Z";
// 单色版的翅膀：翅根在背线处留出 14 的缝，否则同色的翅膀和鸟身会粘成一团
const WING_MONO: &str = "M411.2 501.2L536.5 501.2L197 182.1L325.7 432.8L189.9 432.8L332.9 714.6L465.4 592Z";
const BODY: &str = "M200.2 856.4L577.3 507.6A118.7 118.7 0 0 1 710.3 488.2L834.1 549.1L776.3 586.4A118.7 118.7 0 0 1 682.6 710.8Q539.8 872.9 200.2 856.4Z";

fn eye(fill: &str) -> String {
    format!(r#"<circle cx="677.2" cy="566.4" r="23.7" fill="{fill}"/>"#)
}

fn voltwing(bg: &str) -> Icon {
    Icon {
        bg: bg.into(),
        mark: format!(r#"<path d="{WING}" fill="{VOLT}"/><path d="{BODY}" fill="{IVORY}"/>{}"#, eye(bg)),
        defs: None,
    }
}

fn voltwing_mono(bg: &str, ink: &str) -> Icon {
    Icon {
        bg: bg.into(),
        mark: format!(r#"<path d="{WING_MONO}" fill="{ink}"/><path d="{BODY}" fill="{ink}"/>{}"#, eye(bg)),
        defs: None,
    }
}

// ---- 雷鸟（512 网格画的，放大一倍进 1024）----
// 正面展翅：三叉冠羽、怒目、橙色尖喙，翅膀与尾巴是锯齿状的闪电羽。只定义右半边（x ≥ 256）
// 再镜像；整只先涂亮面，再裁出左半涂暗面（左暗右亮的折纸感）。
fn thunderbird() -> Icon {
    type Half = &'static [(i32, i32)];
    const WING_R: Half = &[(300, 246), (466, 76), (410, 160), (484, 156), (404, 222), (454, 250), (360, 274), (390, 306), (294, 300)];
    const BODY_R: Half = &[(256, 212), (298, 244), (294, 300), (330, 400), (284, 372), (256, 454)];
    const CREST_R: Half = &[(256, 30), (272, 122), (314, 66), (298, 148), (256, 148)];
    const HEAD_R: Half = &[(256, 116), (302, 140), (308, 188), (286, 220), (256, 232)];
    const EYE_R: Half = &[(262, 174), (302, 160), (292, 190), (268, 188)];
    let pts = |half: &[(i32, i32)]| half.iter().map(|(x, y)| format!("{x},{y}")).collect::<Vec<_>>().join(" ");
    let mirror = |half: Half| half.iter().map(|&(x, y)| (512 - x, y)).collect::<Vec<_>>();
    let both = |half: Half| format!(r#"<polygon points="{}"/><polygon points="{}"/>"#, pts(half), pts(&mirror(half)));
    let silhouette: String = [WING_R, BODY_R, CREST_R, HEAD_R].iter().map(|h| both(h)).collect();
    // 同色描边盖住相邻多边形之间的抗锯齿拼缝，否则放大后能看到一道道细线
    let sil = |color: &str| {
        format!(r#"<g fill="{color}" stroke="{color}" stroke-width="2" stroke-linejoin="round">{silhouette}</g>"#)
    };
    Icon {
        bg: "url(#tb-glow)".into(),
        defs: Some(
            concat!(
                r#"<radialGradient id="tb-glow" cx="512" cy="440" r="500" gradientUnits="userSpaceOnUse">"#,
                r##"<stop offset="0" stop-color="#3b37b0"/><stop offset="0.55" stop-color="#1c1a58"/><stop offset="1" stop-color="#0c0b24"/>"##,
                r#"</radialGradient>"#,
                r#"<clipPath id="tb-left"><rect width="256" height="512"/></clipPath>"#,
            )
            .into(),
        ),
        // 翼展原本占满 89% 画布，缩一点留出呼吸感，并把鸟（y 30–454）挪到垂直居中
        mark: format!(
            concat!(
                r#"<g transform="scale(2) translate(256 256) scale(0.92) translate(-256 -242)">"#,
                "{}",
                r#"<g clip-path="url(#tb-left)">{}</g>"#,
                r##"<g fill="#0a0a0a">{}</g>"##,
                r##"<polygon fill="#ff7a1a" points="234,196 278,196 256,250"/>"##,
                r##"<polygon fill="#e2560a" points="256,196 278,196 256,250"/>"##,
                "</g>",
            ),
            sil("#ffd21f"),
            sil("#f2a900"),
            both(EYE_R),
        ),
    }
}

/// 全部内置图标，顺序即设置里的排列顺序
pub fn icons() -> Vec<(&'static str, Icon)> {
    vec![
        ("emberwing", emberwing()),
        ("voltwing", voltwing(INDIGO)),
        ("voltwing-spark", voltwing_mono(VOLT, INDIGO)),
        ("voltwing-night", voltwing(NIGHT)),
        ("voltwing-ivory", voltwing_mono(IVORY, INDIGO)),
        // 鸟字：「鸟」字里的竖折折钩本来就是一道闪电，把它点亮；那一点正好是眼睛（电翼稿方向 B）
        (
            "glyph",
            Icon {
                bg: INDIGO.into(),
                mark: format!(
                    concat!(
                        r#"<path d="M592.3 186.4L469.6 272.4L498.9 297L456.1 297L445.7 370.7L640.1 370.7L622.9 493.6L696.6 493.6L724.2 297L530.5 297L623.1 229.4ZM223.1 714.8L561.1 714.8L571.4 641L233.5 641Z" fill="{ivory}"/>"#,
                        r#"<circle cx="528.7" cy="419.8" r="44.3" fill="{ivory}"/>"#,
                        r#"<path d="M441.6 297L367.9 297L333.3 542.7L328.2 579.6L323 616.4L660.9 616.4L539.4 825.3L765.3 616.4L775.7 542.7L407.1 542.7Z" fill="{volt}"/>"#,
                    ),
                    ivory = IVORY,
                    volt = VOLT,
                ),
                defs: None,
            },
        ),
        // 一闪：整只鸟就是一个闪电符号，只加了喙和眼（电翼稿方向 C）
        (
            "flash",
            Icon {
                bg: INDIGO.into(),
                mark: format!(
                    concat!(
                        r#"<path d="M399.8 223.6L596.4 223.6L753.7 276.1L609.5 315.4L530.8 472.7L701.2 472.7L308 839.7L426 557.9L255.6 557.9Z" fill="{volt}"/>"#,
                        r#"<circle cx="543.9" cy="269.5" r="22.5" fill="{indigo}"/>"#,
                    ),
                    volt = VOLT,
                    indigo = INDIGO,
                ),
                defs: None,
            },
        ),
        ("thunderbird", thunderbird()),
    ]
}

fn svg(icon: &Icon, body: &str, extra_defs: &str) -> String {
    let defs = match (&icon.defs, extra_defs) {
        (None, "") => String::new(),
        (d, extra) => format!("<defs>{}{extra}</defs>", d.as_deref().unwrap_or("")),
    };
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">{defs}{body}</svg>"#) + "\n"
}

fn scaled(k: f64, inner: &str) -> String {
    format!(r#"<g transform="translate(512 512) scale({k}) translate(-512 -512)">{inner}</g>"#)
}

/// 圆角方块，圆角 229（1024 网格）：标签页图标、Windows exe 的图标
pub fn rounded(icon: &Icon) -> String {
    svg(icon, &format!(r#"<rect width="1024" height="1024" rx="229" fill="{}"/>{}"#, icon.bg, icon.mark), "")
}

/// macOS 版式（Big Sur 起的图标网格）：824 见方的圆角块居中、四周留透明边、带一点投影。
/// 满版的图标放进 Dock 会比别的应用大一圈。原生 falcon-core 的 MAC_INSET / MAC_RADIUS 是同一组数
pub fn macos(icon: &Icon) -> String {
    svg(
        icon,
        &format!(
            r#"<rect x="100" y="100" width="824" height="824" rx="185" fill="{}" filter="url(#mac-shadow)"/>{}"#,
            icon.bg,
            scaled(824.0 / 1024.0, &icon.mark)
        ),
        r##"<filter id="mac-shadow" x="-10%" y="-10%" width="120%" height="125%"><feDropShadow dx="0" dy="10" stdDeviation="12" flood-color="#000" flood-opacity="0.3"/></filter>"##,
    )
}

/// 默认图标
pub fn default_icon() -> Icon {
    icons().into_iter().find(|(id, _)| *id == DEFAULT_ID).map(|(_, i)| i).expect("默认图标在列表里")
}

/// 用 rsvg-convert 光栅化成 size 见方的 PNG
pub fn raster(svg_text: &str, dest: &Path, size: u32) -> Result<()> {
    require_tool("rsvg-convert", "brew install librsvg（Linux：librsvg2-bin）")?;
    let size = size.to_string();
    let png = pipe("rsvg-convert", ["-w", size.as_str(), "-h", size.as_str()], svg_text.as_bytes())?;
    if png.is_empty() {
        bail!("rsvg-convert 没有输出");
    }
    std::fs::write(dest, png)?;
    Ok(())
}

/// `cargo xtask icons`：出全部内置图标
pub fn run(args: &[String]) -> Result<()> {
    if let Some(a) = args.first() {
        bail!("未知参数：{a}");
    }
    let web_out = native().join("web/icons");
    let native_out = native().join("crates/falcon-ui/assets/app-icons");
    // 从头出：删掉的图标不留旧文件
    let _ = std::fs::remove_dir_all(&web_out);
    let _ = std::fs::remove_dir_all(&native_out);
    std::fs::create_dir_all(&native_out)?;
    let all = icons();
    for (id, icon) in &all {
        let dir = web_out.join(id);
        std::fs::create_dir_all(&dir)?;
        let any = rounded(icon);
        raster(&any, &dir.join("icon-192.png"), 192)?;
        raster(&any, &dir.join("icon-512.png"), 512)?;
        raster(&macos(icon), &native_out.join(format!("{id}.png")), 512)?;
    }
    let rel = |p: &Path| p.strip_prefix(root()).unwrap_or(p).display().to_string();
    println!("wrote {} icons → {}, {}", all.len(), rel(&web_out), rel(&native_out));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_javascript() {
        // JS：(1024 - 1024 * 0.8) / 2 === 102.39999999999998，824 / 1024 === 0.8046875
        let side = 1024.0 * 0.8;
        assert_eq!(format!("{side} {}", (1024.0 - side) / 2.0), "819.2 102.39999999999998");
        assert_eq!(format!("{}", 824.0 / 1024.0), "0.8046875");
    }

    #[test]
    fn ids_match_falcon_core() {
        let ids: Vec<&str> = icons().iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            ["emberwing", "voltwing", "voltwing-spark", "voltwing-night", "voltwing-ivory", "glyph", "flash", "thunderbird"]
        );
    }
}
