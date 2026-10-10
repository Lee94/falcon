//! 构建期：Windows 上把应用图标与版本信息编进 exe 的资源段。字体在 falcon-ui 的 build.rs。

fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        windows_resources(&manifest, &out);
    }
}

/// Windows：把应用图标与版本信息编进 exe 的资源段。资源只能编进最终的可执行文件，所以在这个
/// bin crate 里做（`cargo xtask win` 也从这里的 OUT_DIR 取同一份 .ico 给安装包用）。
///
/// GPUI 建窗口时按资源 ID 1 取图标（gpui-pre-windows `load_icon`），没有就是系统默认的空白
/// 窗口图标；资源管理器、任务栏、安装包的快捷方式也都读这一份。winresource 的 `set_icon`
/// 恰好用 ID 1。
///
/// 图形用浏览器版的圆角方块版（`cargo xtask icons` 的产物 native/web/icons、已进仓库）：Windows 没有 macOS
/// 那种留边 + 投影的版式约定，满版圆角在任务栏里与别的 App 一样大。构建机上不必有 rsvg-convert，
/// 缩放在这里用 image 做。运行时换图标（ADR 0018）在 Windows 上没接，跟的是这里的默认图标。
#[cfg(windows)]
fn windows_resources(manifest: &std::path::Path, out: &std::path::Path) {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::imageops::FilterType;

    // 与 falcon_core::app_icon::DEFAULT_APP_ICON 同一个
    let src = manifest.join("../../web/icons/emberwing/icon-512.png");
    println!("cargo:rerun-if-changed={}", src.display());
    let img = image::open(&src)
        .unwrap_or_else(|e| panic!("读不到 {}：{e}。先在 native/ 下 cargo xtask icons", src.display()))
        .into_rgba8();
    // 资源管理器各档视图与高 DPI 任务栏要的尺寸；256 那档 Windows 只认 PNG 编码，IcoFrame::as_png 统一这么存
    let frames: Vec<IcoFrame> = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&s| {
            let scaled = image::imageops::resize(&img, s, s, FilterType::Lanczos3);
            IcoFrame::as_png(scaled.as_raw(), s, s, image::ExtendedColorType::Rgba8).unwrap()
        })
        .collect();
    let ico = out.join("falcon.ico");
    IcoEncoder::new(std::fs::File::create(&ico).unwrap()).encode_images(&frames).unwrap();

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set("ProductName", "Falcon")
        .set("FileDescription", "Falcon")
        .set("OriginalFilename", "Falcon.exe");
    res.compile().expect("编译 Windows 资源失败（rc.exe 来自 Windows SDK，随 VS 生成工具装）");
}
