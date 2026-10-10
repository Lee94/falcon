//! 应用图标（ADR 0018）：内置图标嵌进二进制、运行时换 Dock 图标、自定义图标上传前的规整
//! 与放进 Dock 前的套版式。
//!
//! 内置图标是 `cargo xtask icons`（图形在 native/xtask/src/icons.rs）按 macOS 网格出的 512 PNG
//! （824/1024 的圆角块、四周留透明边、带投影），直接能进 Dock。装好的 App 没启动时 Dock / 访达显示的是安装包里的
//! AppIcon.icns（默认图标）；启动、连上服务端后才换成服务端上选的——这是 macOS 的限制：
//! 改 .app 自己的图标要写进包里，会弄坏签名。

use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use anyhow::{Context as _, Result, anyhow};
use falcon_core::app_icon::{
    APP_ICON_CUSTOM_SIZE, MAC_INSET, MAC_RADIUS, contain_rect, has_own_shape, rounded_square_coverage,
};
use gpui_kit::{Image, ImageFormat, SMOOTH_SVG_SCALE_FACTOR, SvgRenderer};
use image::{GrayImage, Luma, Rgba, RgbaImage, imageops};

macro_rules! builtin {
    ($($id:literal),* $(,)?) => {
        [$(($id, include_bytes!(concat!("../assets/app-icons/", $id, ".png")) as &[u8])),*]
    };
}

/// 顺序与 falcon-core 的 `APP_ICON_IDS` 相同（测试对账）
static BUILTIN: [(&str, &[u8]); 8] = builtin!(
    "emberwing",
    "voltwing",
    "voltwing-spark",
    "voltwing-night",
    "voltwing-ivory",
    "glyph",
    "flash",
    "thunderbird",
);

pub fn builtin_png(id: &str) -> Option<&'static [u8]> {
    BUILTIN.iter().find(|(k, _)| *k == id).map(|(_, png)| *png)
}

/// 设置里的预览：GPUI 按 Image 的 id（字节哈希）缓存解码结果，这里只建一次
static PREVIEWS: LazyLock<Vec<(&'static str, Arc<Image>)>> = LazyLock::new(|| {
    BUILTIN.iter().map(|(id, png)| (*id, Arc::new(Image::from_bytes(ImageFormat::Png, png.to_vec())))).collect()
});

pub fn builtin_image(id: &str) -> Option<Arc<Image>> {
    PREVIEWS.iter().find(|(k, _)| *k == id).map(|(_, img)| img.clone())
}

/// 换 Dock 图标（整个 App 一个；几台服务端各开一个窗口时，最后换的那台说了算）
pub fn set_dock_icon(png: &[u8]) {
    #[cfg(target_os = "macos")]
    mac::set_dock_icon(png);
    #[cfg(not(target_os = "macos"))]
    let _ = png;
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::AnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    pub fn set_dock_icon(png: &[u8]) {
        // GPUI 的前台回调都在主线程上；拿不到标记说明被挪到了别处，宁可不换也别碰 AppKit
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("不在主线程上，没换 Dock 图标");
            return;
        };
        let data = NSData::with_bytes(png);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            log::warn!("Dock 图标解不开（{} 字节）", png.len());
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }

    /// 从 AppKit 读回当前的 Dock 图标（TIFF）。锁屏时截不到屏幕，自动化的 `dock:` 步骤靠它验证
    #[cfg(feature = "automation")]
    pub fn current_dock_tiff() -> Option<Vec<u8>> {
        let mtm = MainThreadMarker::new()?;
        let image = NSApplication::sharedApplication(mtm).applicationIconImage()?;
        Some(image.TIFFRepresentation()?.to_vec())
    }
}

#[cfg(all(feature = "automation", target_os = "macos"))]
pub use mac::current_dock_tiff;

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).context("编码 PNG")?;
    Ok(out.into_inner())
}

/// 用户挑的图（GPUI 能解码的格式，含 SVG）→ 512 见方的 PNG：等比缩放、居中、不裁切。
/// web `normalizeIconImage` 的原生版，服务端只收这个。
pub fn normalize_upload(bytes: Vec<u8>, format: ImageFormat, svg: SvgRenderer) -> Result<Vec<u8>> {
    let size = APP_ICON_CUSTOM_SIZE;
    let render = if format == ImageFormat::Svg {
        // SVG 按目标尺寸光栅化：先量固有尺寸，再画到约两倍目标大小，缩下来才清楚。
        // 只写 viewBox 的小图标（24×24）按 1 倍画出来再放大会糊成一片
        let probe = svg.render_single_frame(&bytes, 1.).map_err(|e| anyhow!("SVG 解析失败：{e}"))?;
        let s = probe.size(0);
        let intrinsic = (s.width.0.max(s.height.0).max(1) as f32) / SMOOTH_SVG_SCALE_FACTOR;
        svg.render_single_frame(&bytes, size as f32 / intrinsic).map_err(|e| anyhow!("SVG 渲染失败：{e}"))?
    } else {
        Image::from_bytes(format, bytes).to_image_data(svg).context("解码图片")?
    };
    let s = render.size(0);
    let (w, h) = (s.width.0 as u32, s.height.0 as u32);
    // GPUI 解出来的帧是直通 alpha 的 BGRA，换回 RGBA
    let mut px = render.as_bytes(0).ok_or_else(|| anyhow!("图片没有帧"))?.to_vec();
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let src = RgbaImage::from_raw(w, h, px).ok_or_else(|| anyhow!("帧大小不对"))?;
    let r = contain_rect(w, h, size);
    let scaled = imageops::resize(&src, r.w, r.h, imageops::FilterType::Lanczos3);
    let mut out = RgbaImage::new(size, size);
    imageops::overlay(&mut out, &scaled, r.x as i64, r.y as i64);
    encode_png(&out)
}

/// 服务端上的自定义图（正方形 PNG）→ 放进 Dock 的 PNG。自带形状的（四角透明）原样用；
/// 满版方块套 macOS 版式——缩进 824/1024 的圆角块、四周留边、垫一层投影，与内置图标一致。
pub fn dock_png(png: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).context("解码自定义图标")?.into_rgba8();
    if has_own_shape(img.as_raw(), img.width(), img.height()) {
        return Ok(png.to_vec());
    }
    encode_png(&mac_frame(&img))
}

/// straight alpha 的 src over dst
fn over(dst: &mut Rgba<u8>, src: [f64; 4]) {
    let sa = src[3];
    let da = dst[3] as f64 / 255.;
    let a = sa + da * (1. - sa);
    if a <= 0. {
        *dst = Rgba([0, 0, 0, 0]);
        return;
    }
    let mut out = [0u8; 4];
    for i in 0..3 {
        let c = (src[i] * sa + dst[i] as f64 * da * (1. - sa)) / a;
        out[i] = c.round().clamp(0., 255.) as u8;
    }
    out[3] = (a * 255.).round().clamp(0., 255.) as u8;
    *dst = Rgba(out);
}

fn mac_frame(src: &RgbaImage) -> RgbaImage {
    let n = src.width();
    let side = (n as f64 * (1. - 2. * MAC_INSET)).round() as u32;
    let x0 = ((n - side) / 2) as f64;
    let r = n as f64 * MAC_RADIUS;
    // 投影参数与 xtask icons.rs 的 feDropShadow 同比例：下移 10/1024、模糊 12/1024、30% 黑
    let dy = n as f64 * 10. / 1024.;
    let mut shadow = GrayImage::new(n, n);
    for (x, y, p) in shadow.enumerate_pixels_mut() {
        *p = Luma([(rounded_square_coverage(x, y, x0, x0 + dy, side as f64, r) * 255.).round() as u8]);
    }
    let shadow = imageops::blur(&shadow, n as f32 * 12. / 1024.);
    let mut out = RgbaImage::new(n, n);
    for (x, y, p) in out.enumerate_pixels_mut() {
        *p = Rgba([0, 0, 0, (shadow.get_pixel(x, y)[0] as f64 * 0.3).round() as u8]);
    }
    let body = imageops::resize(src, side, side, imageops::FilterType::Lanczos3);
    for (bx, by, p) in body.enumerate_pixels() {
        let (x, y) = (bx + x0 as u32, by + x0 as u32);
        let cov = rounded_square_coverage(x, y, x0, x0, side as f64, r);
        if cov <= 0. {
            continue;
        }
        let a = p[3] as f64 / 255. * cov;
        over(out.get_pixel_mut(x, y), [p[0] as f64, p[1] as f64, p[2] as f64, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_core::app_icon::APP_ICON_IDS;

    #[test]
    fn builtin_matches_core_catalog() {
        let ids: Vec<&str> = BUILTIN.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, APP_ICON_IDS.to_vec());
    }

    /// xtask icons.rs 多出了图标、这里没跟上时红
    #[test]
    fn every_generated_png_is_embedded() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app-icons");
        for entry in std::fs::read_dir(dir).expect("assets/app-icons") {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let Some(id) = name.strip_suffix(".png") else { continue };
            assert!(builtin_png(id).is_some(), "{name} 没嵌进 BUILTIN");
        }
    }

    #[test]
    fn builtin_pngs_already_have_mac_shape() {
        for (id, png) in BUILTIN {
            let img = image::load_from_memory(png).unwrap().into_rgba8();
            assert!(has_own_shape(img.as_raw(), img.width(), img.height()), "{id} 应该是 macOS 版式（四角透明）");
        }
    }

    #[test]
    fn full_bleed_custom_gets_framed() {
        let solid = RgbaImage::from_pixel(512, 512, Rgba([200, 30, 30, 255]));
        let framed = image::load_from_memory(&dock_png(&encode_png(&solid).unwrap()).unwrap()).unwrap().into_rgba8();
        assert_eq!(framed.dimensions(), (512, 512));
        assert!(has_own_shape(framed.as_raw(), 512, 512), "套版式后四角应透明");
        // 中心是原图的颜色、完全不透明
        assert_eq!(*framed.get_pixel(256, 256), Rgba([200, 30, 30, 255]));
        // 透明边里有一点投影（下方比上方深）
        assert!(framed.get_pixel(256, 470)[3] > framed.get_pixel(256, 42)[3]);
    }

    #[test]
    fn shaped_custom_is_left_alone() {
        let mut img = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        for y in 16..48 {
            for x in 16..48 {
                img.put_pixel(x, y, Rgba([1, 2, 3, 255]));
            }
        }
        let png = encode_png(&img).unwrap();
        assert_eq!(dock_png(&png).unwrap(), png);
    }
}
