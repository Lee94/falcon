use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use image::{GrayImage, ImageFormat, ImageReader, Limits, Luma, Rgba, RgbaImage, imageops};

use crate::app_icon::{
    APP_ICON_CUSTOM_SIZE, MAC_INSET, MAC_RADIUS, contain_rect, has_own_shape,
    rounded_square_coverage,
};

pub const MAX_UPLOAD_INPUT_BYTES: usize = 20 * 1024 * 1024;

const MAX_NORMALIZED_PNG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SVG_XML_NODES: u32 = 100_000;

pub fn normalize_upload(bytes: &[u8], name: &str, mime: &str) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_UPLOAD_INPUT_BYTES {
        return Err("icon input exceeds size limit".into());
    }
    let size = APP_ICON_CUSTOM_SIZE;
    let svg = mime.eq_ignore_ascii_case("image/svg+xml")
        || name.rsplit('.').next().is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
    let source = if svg {
        render_svg(bytes)?
    } else {
        decode_raster(bytes, name)?.into_rgba8()
    };
    let (width, height) = source.dimensions();
    if width == 0 || height == 0 {
        return Err("empty image".into());
    }
    let rect = contain_rect(width, height, size);
    let scaled_width = rect.w.max(1);
    let scaled_height = rect.h.max(1);
    let scaled = imageops::resize(
        &source,
        scaled_width,
        scaled_height,
        imageops::FilterType::Lanczos3,
    );
    let mut canvas = RgbaImage::new(size, size);
    imageops::overlay(&mut canvas, &scaled, rect.x as i64, rect.y as i64);
    let png = encode_png(&canvas)?;
    if png.len() > MAX_NORMALIZED_PNG_BYTES {
        return Err("normalized PNG exceeds server limit".into());
    }
    Ok(png)
}

pub fn dock_png(png: &[u8]) -> Result<Vec<u8>, String> {
    let source = decode_png(png)?.into_rgba8();
    if source.width() != source.height() {
        return Err("custom icon must be square".into());
    }
    if has_own_shape(source.as_raw(), source.width(), source.height()) {
        return Ok(png.to_vec());
    }
    encode_png(&mac_frame(&source))
}

fn decode_raster(bytes: &[u8], name: &str) -> Result<image::DynamicImage, String> {
    let extension = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let format = match extension.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::WebP),
        "bmp" => Some(ImageFormat::Bmp),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    };
    let mut reader = match format {
        Some(format) => ImageReader::with_format(Cursor::new(bytes), format),
        None => ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| error.to_string())?,
    };
    reader.limits(image_limits());
    reader.decode().map_err(|error| error.to_string())
}

fn decode_png(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(image_limits());
    reader.decode().map_err(|error| error.to_string())
}

fn image_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits
}

fn render_svg(bytes: &[u8]) -> Result<RgbaImage, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err("gzip-compressed SVG is not supported".into());
    }
    if bytes.len() > MAX_UPLOAD_INPUT_BYTES {
        return Err("SVG input exceeds size limit".into());
    }
    let svg = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let document = resvg::usvg::roxmltree::Document::parse_with_options(
        svg,
        resvg::usvg::roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_SVG_XML_NODES,
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    if document.descendants().any(|node| {
        node.is_element()
            && (matches!(node.tag_name().name(), "image" | "feImage")
                || node.attributes().any(|attribute| {
                    attribute.name() == "href" && !attribute.value().trim().starts_with('#')
                }))
    }) {
        return Err("SVG image resources and external hrefs are not supported".into());
    }

    let rejected_resource = Arc::new(AtomicBool::new(false));
    let rejected_data = Arc::clone(&rejected_resource);
    let rejected_string = Arc::clone(&rejected_resource);
    let mut options = resvg::usvg::Options::default();
    options.resources_dir = None;
    options.image_href_resolver = resvg::usvg::ImageHrefResolver {
        resolve_data: Box::new(
            move |_: &str, _: Arc<Vec<u8>>, _: &resvg::usvg::Options<'_>| {
                rejected_data.store(true, Ordering::Relaxed);
                None
            },
        ),
        resolve_string: Box::new(move |_: &str, _: &resvg::usvg::Options<'_>| {
            rejected_string.store(true, Ordering::Relaxed);
            None
        }),
    };
    let tree = resvg::usvg::Tree::from_str(svg, &options).map_err(|error| error.to_string())?;
    if rejected_resource.load(Ordering::Relaxed) {
        return Err("SVG image resources and external hrefs are not supported".into());
    }
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return Err("invalid SVG size".into());
    }
    let scale = (APP_ICON_CUSTOM_SIZE as f32 * 2.0) / longest;
    let width = (size.width() * scale)
        .round()
        .clamp(1.0, (APP_ICON_CUSTOM_SIZE * 2) as f32) as u32;
    let height = (size.height() * scale)
        .round()
        .clamp(1.0, (APP_ICON_CUSTOM_SIZE * 2) as f32) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| "SVG allocation failed".to_string())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut pixels = pixmap.data().to_vec();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u16;
        for channel in &mut pixel[..3] {
            *channel = if alpha == 0 {
                0
            } else {
                ((*channel as u16 * 255 + alpha / 2) / alpha).min(255) as u8
            };
        }
    }
    RgbaImage::from_raw(width, height, pixels).ok_or_else(|| "invalid SVG pixels".into())
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(cursor.into_inner())
}

fn over(dst: &mut Rgba<u8>, src: [f64; 4]) {
    let source_alpha = src[3];
    let destination_alpha = dst[3] as f64 / 255.0;
    let alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if alpha <= 0.0 {
        *dst = Rgba([0, 0, 0, 0]);
        return;
    }
    let mut output = [0u8; 4];
    for i in 0..3 {
        let channel =
            (src[i] * source_alpha + dst[i] as f64 * destination_alpha * (1.0 - source_alpha))
                / alpha;
        output[i] = channel.round().clamp(0.0, 255.0) as u8;
    }
    output[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    *dst = Rgba(output);
}

fn mac_frame(source: &RgbaImage) -> RgbaImage {
    let size = source.width();
    let side = (size as f64 * (1.0 - 2.0 * MAC_INSET)).round() as u32;
    let x0 = ((size - side) / 2) as f64;
    let radius = size as f64 * MAC_RADIUS;
    let dy = size as f64 * 10.0 / 1024.0;
    let mut shadow = GrayImage::new(size, size);
    for (x, y, pixel) in shadow.enumerate_pixels_mut() {
        *pixel = Luma([
            (rounded_square_coverage(x, y, x0, x0 + dy, side as f64, radius) * 255.0).round()
                as u8,
        ]);
    }
    let shadow = imageops::blur(&shadow, size as f32 * 12.0 / 1024.0);
    let mut output = RgbaImage::new(size, size);
    for (x, y, pixel) in output.enumerate_pixels_mut() {
        *pixel = Rgba([0, 0, 0, (shadow.get_pixel(x, y)[0] as f64 * 0.3).round() as u8]);
    }
    let body = imageops::resize(source, side, side, imageops::FilterType::Lanczos3);
    for (x, y, pixel) in body.enumerate_pixels() {
        let (x, y) = (x + x0 as u32, y + x0 as u32);
        let coverage = rounded_square_coverage(x, y, x0, x0, side as f64, radius);
        if coverage <= 0.0 {
            continue;
        }
        let alpha = pixel[3] as f64 / 255.0 * coverage;
        over(
            output.get_pixel_mut(x, y),
            [pixel[0] as f64, pixel[1] as f64, pixel[2] as f64, alpha],
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn rejects_external_svg_image_hrefs() {
        for href in [
            "missing-falcon-icon-resource.png",
            "file:///nonexistent-falcon-icon-resource.png",
            "https://example.invalid/icon.png",
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image href="{href}" width="8" height="8"/></svg>"#
            );
            assert_eq!(
                render_svg(svg.as_bytes()).unwrap_err(),
                "SVG image resources and external hrefs are not supported"
            );
        }
    }

    #[test]
    fn rejects_oversized_embedded_svg_image() {
        let png = encode_png(&RgbaImage::new(4097, 1)).unwrap();
        let data = STANDARD.encode(png);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image href="data:image/png;base64,{data}" width="8" height="8"/></svg>"#
        );
        assert_eq!(
            render_svg(svg.as_bytes()).unwrap_err(),
            "SVG image resources and external hrefs are not supported"
        );
    }

    #[test]
    fn rejects_external_use_hrefs() {
        for href in [
            "href=\"external.svg#shape\"",
            "xlink:href=\"external.svg#shape\"",
        ] {
            let svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><defs><path id="shape" d="M0 0H10V10H0Z"/></defs><use {href}/></svg>"##
            );
            assert_eq!(
                render_svg(svg.as_bytes()).unwrap_err(),
                "SVG image resources and external hrefs are not supported"
            );
        }
    }

    #[test]
    fn renders_ordinary_path_svg() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><path d="M0 0H10V10H0Z" fill="#f00"/></svg>"##;
        let image = render_svg(svg).unwrap();
        assert_eq!(
            image.get_pixel(image.width() / 2, image.height() / 2).0,
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn renders_local_fragment_use() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><defs><path id="shape" d="M0 0H10V10H0Z" fill="#f00"/></defs><use xlink:href="#shape"/></svg>"##;
        let image = render_svg(svg).unwrap();
        assert_eq!(
            image.get_pixel(image.width() / 2, image.height() / 2).0,
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn rejects_gzip_svg() {
        assert_eq!(
            render_svg(&[0x1f, 0x8b, 0x08, 0x00]).unwrap_err(),
            "gzip-compressed SVG is not supported"
        );
    }

    #[test]
    fn rejects_dtd_and_custom_entities() {
        let svg = br##"<!DOCTYPE svg [<!ENTITY paint "#f00">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><path d="M0 0H10V10H0Z" fill="&paint;"/></svg>"##;
        assert!(render_svg(svg).is_err());
    }

    #[test]
    fn rejects_svg_node_limit() {
        let groups = "<g/>".repeat(MAX_SVG_XML_NODES as usize);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{groups}</svg>"#
        );
        assert!(render_svg(svg.as_bytes()).is_err());
    }
}
