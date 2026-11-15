use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use falcon_core::app_icon::{
    APP_ICON_CUSTOM_SIZE, CUSTOM, DEFAULT_APP_ICON, MAC_INSET, MAC_RADIUS, contain_rect,
    has_own_shape, rounded_square_coverage,
};
#[cfg(not(target_arch = "wasm32"))]
use falcon_client::FalconClient;
use falcon_proto::AppIconState;
use image::{GrayImage, ImageFormat, ImageReader, Limits, Luma, Rgba, RgbaImage, imageops};
use nsh_core::element::ImageSrc;
use nsh_core::prelude::{Owner, Signal, effect, signal, spawn};

use crate::store::WorkspaceStore;

macro_rules! builtin {
    ($($id:literal),* $(,)?) => {
        [$(($id, include_bytes!(concat!("../../assets/app-icons/", $id, ".png")) as &[u8])),*]
    };
}

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

static BUILTIN_SOURCES: LazyLock<Vec<(&'static str, String)>> = LazyLock::new(|| {
    BUILTIN
        .iter()
        .map(|(id, png)| (*id, format!("data:image/png;base64,{}", STANDARD.encode(png))))
        .collect()
});

const MAX_NORMALIZED_PNG_BYTES: usize = 2 * 1024 * 1024;
const MAX_UPLOAD_INPUT_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct CustomIcon {
    pub version: String,
    pub png: Vec<u8>,
    pub dock_png: Vec<u8>,
    pub data_uri: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RequestToken {
    epoch: u64,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IconAction {
    Upload,
    Select(String),
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IconOperation {
    token: RequestToken,
    action: IconAction,
}

#[derive(Clone)]
pub struct AppIconController {
    pub selected: Signal<String>,
    pub custom_version: Signal<Option<String>>,
    pub custom_icon: Signal<Option<CustomIcon>>,
    pub busy: Signal<bool>,
    pub ready: Signal<bool>,
    pending: Signal<Option<IconOperation>>,
    epoch: Rc<Cell<u64>>,
    generation: Rc<Cell<u64>>,
}

impl AppIconController {
    pub fn new() -> Self {
        Self {
            selected: signal(DEFAULT_APP_ICON.to_string()),
            custom_version: signal(None),
            custom_icon: signal(None),
            busy: signal(false),
            ready: signal(false),
            pending: signal(None),
            epoch: Rc::new(Cell::new(0)),
            generation: Rc::new(Cell::new(0)),
        }
    }

    pub(crate) fn begin_server_epoch(&self) -> RequestToken {
        let epoch = self.epoch.get().wrapping_add(1);
        let generation = self.generation.get().wrapping_add(1);
        self.epoch.set(epoch);
        self.generation.set(generation);
        self.selected.set(DEFAULT_APP_ICON.to_string());
        self.custom_version.set(None);
        self.custom_icon.set(None);
        self.busy.set(false);
        self.ready.set(false);
        self.pending.set(None);
        if let Some(png) = builtin_png(DEFAULT_APP_ICON) {
            falcon_platform::shared().set_app_icon(png);
        }
        RequestToken { epoch, generation }
    }

    pub fn request_upload(&self) {
        self.request(IconAction::Upload);
    }

    pub fn request_select(&self, id: &str) {
        self.request(IconAction::Select(id.to_string()));
    }

    pub fn request_remove(&self) {
        self.request(IconAction::Remove);
    }

    fn request(&self, action: IconAction) {
        if self.busy.get_untracked() || !self.ready.get_untracked() {
            return;
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let token = RequestToken {
            epoch: self.epoch.get(),
            generation,
        };
        self.busy.set(true);
        self.pending.set(Some(IconOperation { token, action }));
    }

    fn is_current(&self, token: RequestToken) -> bool {
        self.epoch.get() == token.epoch && self.generation.get() == token.generation
    }

    fn finish(&self, token: RequestToken) {
        if self.is_current(token) {
            self.busy.set(false);
        }
    }

    fn set_server_state(&self, state: &AppIconState, custom_icon: Option<CustomIcon>) {
        self.selected.set(selected_id(&state.selected).to_string());
        self.custom_version.set(state.custom.clone());
        self.custom_icon.set(custom_icon);
        self.ready.set(true);
    }

    fn finish_initial_load(&self, token: RequestToken) {
        if self.is_current(token) {
            self.ready.set(true);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct ApiTarget {
    client: FalconClient,
}

#[cfg(target_arch = "wasm32")]
struct ApiTarget {
    base: String,
}

fn api_target(store: &WorkspaceStore) -> Result<ApiTarget, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(ApiTarget {
            client: store.client().ok_or_else(|| "server client unavailable".to_string())?,
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let base = store.server_url.get_untracked().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err("server URL unavailable".into());
        }
        Ok(ApiTarget { base })
    }
}

pub fn install_operations(store: WorkspaceStore) {
    let root_owner = Owner::current();
    let controller = store.appearance.app_icon.clone();
    let pending = controller.pending;
    effect(move || {
        let Some(operation) = pending.get() else {
            return;
        };
        pending.set(None);
        let store = store.clone();
        let controller = controller.clone();
        root_owner.run(move || {
            spawn(async move {
                execute_operation(store, controller, operation).await;
            });
        });
    });
}

pub(crate) async fn load_initial(store: WorkspaceStore, token: RequestToken) {
    let controller = store.appearance.app_icon.clone();
    if !controller.is_current(token) {
        return;
    }
    let api = match api_target(&store) {
        Ok(api) => api,
        Err(_) => {
            initial_load_failed(&store, &controller, token);
            return;
        }
    };
    let state = match api_state(&api).await {
        Ok(state) => state,
        Err(_) => {
            initial_load_failed(&store, &controller, token);
            return;
        }
    };
    if !controller.is_current(token) {
        return;
    }

    let selected = selected_id(&state.selected);
    let custom_icon = match state.custom.as_deref() {
        Some(version) => match api_custom_icon(&api, version).await {
            Ok(png) => match prepare_custom_icon(version.to_string(), png).await {
                Ok(icon) => Some(icon),
                Err(_) if selected == CUSTOM => {
                    initial_load_failed(&store, &controller, token);
                    return;
                }
                Err(_) => None,
            },
            Err(_) if selected == CUSTOM => {
                initial_load_failed(&store, &controller, token);
                return;
            }
            Err(_) => None,
        },
        None if selected == CUSTOM => {
            initial_load_failed(&store, &controller, token);
            return;
        }
        None => None,
    };
    if !controller.is_current(token) {
        return;
    }

    let icon_png = if selected == CUSTOM {
        let Some(icon) = custom_icon.as_ref() else {
            initial_load_failed(&store, &controller, token);
            return;
        };
        icon.dock_png.clone()
    } else {
        builtin_png(selected)
            .unwrap_or_else(|| builtin_png(DEFAULT_APP_ICON).unwrap())
            .to_vec()
    };
    controller.set_server_state(&state, custom_icon);
    falcon_platform::shared().set_app_icon(&icon_png);
}

fn initial_load_failed(store: &WorkspaceStore, controller: &AppIconController, token: RequestToken) {
    if controller.is_current(token) {
        controller.finish_initial_load(token);
        store.show_toast(crate::i18n::t!("appIcon.loadFailed"));
    }
}

pub fn builtin_png(id: &str) -> Option<&'static [u8]> {
    BUILTIN.iter().find(|(name, _)| *name == id).map(|(_, png)| *png)
}

pub fn builtin_source(id: &str) -> ImageSrc {
    BUILTIN_SOURCES
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or(&BUILTIN_SOURCES[0])
        .1
        .clone()
        .into()
}

pub fn custom_source(icon: &CustomIcon) -> ImageSrc {
    icon.data_uri.clone().into()
}

pub fn selected_source(controller: &AppIconController) -> ImageSrc {
    let selected = controller.selected.get();
    if selected == CUSTOM
        && let Some(icon) = controller.custom_icon.get()
    {
        return custom_source(&icon);
    }
    builtin_source(&selected)
}

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

fn custom_icon(version: &str, png: Vec<u8>) -> Result<CustomIcon, String> {
    let dock_png = dock_png(&png)?;
    Ok(CustomIcon {
        version: version.to_string(),
        data_uri: format!("data:image/png;base64,{}", STANDARD.encode(&png)),
        png,
        dock_png,
    })
}

async fn prepare_custom_icon(version: String, png: Vec<u8>) -> Result<CustomIcon, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        nsh_core::prelude::blocking(move || custom_icon(&version, png))
            .await
            .map_err(|error| error.to_string())?
    }
    #[cfg(target_arch = "wasm32")]
    {
        custom_icon(&version, png)
    }
}

fn selected_id(id: &str) -> &str {
    if id == CUSTOM || falcon_core::app_icon::is_builtin(id) {
        id
    } else {
        DEFAULT_APP_ICON
    }
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
            allow_dtd: true,
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
    let width = (size.width() * scale).round().clamp(1.0, (APP_ICON_CUSTOM_SIZE * 2) as f32) as u32;
    let height = (size.height() * scale).round().clamp(1.0, (APP_ICON_CUSTOM_SIZE * 2) as f32) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| "SVG allocation failed".to_string())?;
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

async fn execute_operation(
    store: WorkspaceStore,
    controller: AppIconController,
    operation: IconOperation,
) {
    let token = operation.token;
    if !controller.is_current(token) {
        return;
    }
    let result = match api_target(&store) {
        Ok(api) if controller.is_current(token) => match operation.action {
            IconAction::Upload => upload(&api, controller.clone(), token).await,
            IconAction::Select(id) => select(&api, controller.clone(), token, &id).await,
            IconAction::Remove => remove(&api, controller.clone(), token).await,
        },
        Ok(_) => return,
        Err(_) => Err(crate::i18n::t!("appIcon.saveFailed").to_string()),
    };
    if !controller.is_current(token) {
        return;
    }
    if let Err(message) = result {
        store.show_toast(message);
    }
    controller.finish(token);
}

async fn upload(
    api: &ApiTarget,
    controller: AppIconController,
    token: RequestToken,
) -> Result<(), String> {
    let picked = nsh_core::prelude::pick_files(nsh_core::picker::FilePickRequest::new(false, 1)).await;
    if !controller.is_current(token) {
        return Ok(());
    }
    let Some(file) = picked.into_iter().next() else {
        return Ok(());
    };
    let bytes = file.data.bytes().to_vec();
    if bytes.len() > MAX_UPLOAD_INPUT_BYTES {
        return Err(crate::i18n::t!("appIcon.decodeFailed").to_string());
    }
    let name = file.name;
    let mime = file.mime;
    #[cfg(not(target_arch = "wasm32"))]
    let normalized = nsh_core::prelude::blocking(move || normalize_upload(&bytes, &name, &mime))
        .await
        .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?
        .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?;
    #[cfg(target_arch = "wasm32")]
    let normalized = normalize_upload(&bytes, &name, &mime)
        .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?;
    if !controller.is_current(token) {
        return Ok(());
    }
    let mut icon = prepare_custom_icon(String::new(), normalized.clone())
        .await
        .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?;
    if !controller.is_current(token) {
        return Ok(());
    }
    let response = api_upload(api, normalized).await
        .map_err(|_| crate::i18n::t!("appIcon.uploadFailed").to_string())?;
    if !controller.is_current(token) {
        return Ok(());
    }
    let version = response
        .custom
        .as_deref()
        .ok_or_else(|| crate::i18n::t!("appIcon.uploadFailed").to_string())?;
    icon.version = version.to_string();
    let selected = selected_id(&response.selected);
    let platform_png = if selected == CUSTOM {
        icon.dock_png.clone()
    } else {
        builtin_png(selected)
            .unwrap_or_else(|| builtin_png(DEFAULT_APP_ICON).unwrap())
            .to_vec()
    };
    controller.set_server_state(&response, Some(icon));
    falcon_platform::shared().set_app_icon(&platform_png);
    Ok(())
}

async fn select(
    api: &ApiTarget,
    controller: AppIconController,
    token: RequestToken,
    id: &str,
) -> Result<(), String> {
    let id = selected_id(id);
    if !controller.is_current(token) {
        return Ok(());
    }
    let response = api_select(api, id)
        .await
        .map_err(|_| crate::i18n::t!("appIcon.saveFailed").to_string())?;
    if !controller.is_current(token) {
        return Ok(());
    }
    let chosen = selected_id(&response.selected);
    let custom_icon = if chosen == CUSTOM {
        let version = response
            .custom
            .as_deref()
            .ok_or_else(|| crate::i18n::t!("appIcon.saveFailed").to_string())?;
        match controller.custom_icon.get_untracked() {
            Some(icon) if icon.version == version => Some(icon),
            _ => {
                if !controller.is_current(token) {
                    return Ok(());
                }
                let png = api_custom_icon(api, version)
                    .await
                    .map_err(|_| crate::i18n::t!("appIcon.saveFailed").to_string())?;
                Some(prepare_custom_icon(version.to_string(), png).await
                    .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?)
            }
        }
    } else {
        match (&response.custom, controller.custom_version.get_untracked()) {
            (Some(version), Some(current)) if version == &current => {
                controller.custom_icon.get_untracked()
            }
            _ => None,
        }
    };
    if !controller.is_current(token) {
        return Ok(());
    }
    let platform_png = if chosen == CUSTOM {
        custom_icon.as_ref().map(|icon| icon.dock_png.clone())
    } else {
        Some(
            builtin_png(chosen)
                .unwrap_or_else(|| builtin_png(DEFAULT_APP_ICON).unwrap())
                .to_vec(),
        )
    };
    controller.set_server_state(&response, custom_icon);
    if let Some(png) = platform_png {
        falcon_platform::shared().set_app_icon(&png);
    }
    Ok(())
}

async fn remove(
    api: &ApiTarget,
    controller: AppIconController,
    token: RequestToken,
) -> Result<(), String> {
    if !controller.is_current(token) {
        return Ok(());
    }
    let response = api_remove(api)
        .await
        .map_err(|_| crate::i18n::t!("appIcon.removeFailed").to_string())?;
    if !controller.is_current(token) {
        return Ok(());
    }
    let selected = selected_id(&response.selected);
    let custom_icon = if selected == CUSTOM {
        let version = response
            .custom
            .as_deref()
            .ok_or_else(|| crate::i18n::t!("appIcon.removeFailed").to_string())?;
        let png = api_custom_icon(api, version)
            .await
            .map_err(|_| crate::i18n::t!("appIcon.removeFailed").to_string())?;
        Some(prepare_custom_icon(version.to_string(), png).await
            .map_err(|_| crate::i18n::t!("appIcon.decodeFailed").to_string())?)
    } else {
        None
    };
    if !controller.is_current(token) {
        return Ok(());
    }
    let png = if selected == CUSTOM {
        custom_icon.as_ref().unwrap().dock_png.clone()
    } else {
        builtin_png(selected)
            .unwrap_or_else(|| builtin_png(DEFAULT_APP_ICON).unwrap())
            .to_vec()
    };
    controller.set_server_state(&response, custom_icon);
    falcon_platform::shared().set_app_icon(&png);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn api_state(api: &ApiTarget) -> Result<AppIconState, String> {
    api.client
        .app_icon()
        .await
        .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn api_state(api: &ApiTarget) -> Result<AppIconState, String> {
    let body = web_request(api, "GET", "/api/app-icon", None, None).await?;
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
async fn api_custom_icon(api: &ApiTarget, version: &str) -> Result<Vec<u8>, String> {
    api.client
        .custom_app_icon(version)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn api_custom_icon(api: &ApiTarget, version: &str) -> Result<Vec<u8>, String> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("v", version)
        .finish();
    web_request(
        api,
        "GET",
        &format!("/api/app-icon/custom.png?{query}"),
        None,
        None,
    )
    .await
}

#[cfg(not(target_arch = "wasm32"))]
async fn api_select(api: &ApiTarget, id: &str) -> Result<AppIconState, String> {
    api.client
        .set_app_icon(id)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn api_select(api: &ApiTarget, id: &str) -> Result<AppIconState, String> {
    let body = serde_json::to_vec(&serde_json::json!({ "selected": id }))
        .map_err(|error| error.to_string())?;
    let response = web_request(api, "PUT", "/api/app-icon", Some(body), Some("application/json"))
        .await?;
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
async fn api_upload(api: &ApiTarget, png: Vec<u8>) -> Result<AppIconState, String> {
    api.client
        .upload_app_icon(png)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn api_upload(api: &ApiTarget, png: Vec<u8>) -> Result<AppIconState, String> {
    let response = web_request(
        api,
        "PUT",
        "/api/app-icon/custom",
        Some(png),
        Some("image/png"),
    )
    .await?;
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
async fn api_remove(api: &ApiTarget) -> Result<AppIconState, String> {
    api.client
        .remove_custom_app_icon()
        .await
        .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn api_remove(api: &ApiTarget) -> Result<AppIconState, String> {
    let response = web_request(api, "DELETE", "/api/app-icon/custom", None, None).await?;
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn web_request(
    api: &ApiTarget,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    content_type: Option<&str>,
) -> Result<Vec<u8>, String> {
    use nsh_core::prelude::{HttpBody, HttpRequest, fetch};

    let url = if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{}{path}", api.base)
    };
    let mut request = HttpRequest::new(method, url);
    if let Some(content_type) = content_type {
        request
            .headers
            .push(("Content-Type".to_string(), content_type.to_string()));
    }
    if let Some(body) = body {
        request.body = HttpBody::Bytes(body);
    }
    let response = fetch(request).await.map_err(|error| error.to_string())?;
    if !response.ok() {
        return Err("server request failed".into());
    }
    Ok(response.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

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
}
