//! 构建期：Windows 上把应用图标与版本信息编进 exe 的资源段；Android 上为 cdylib 声明 16 KB 页大小对齐。字体在 falcon-nsh 的 build.rs。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        // rustc-link-arg-cdylib 只作用于产出 cdylib 的 crate 本身（falcon-app），确保 Android 15+ 16 KB 页设备可加载
        println!("cargo:rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-link-arg-cdylib=-Wl,-z,common-page-size=16384");
    }

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        windows_resources(&manifest, &out);
    }
}

/// Windows：把应用图标与版本信息编进 exe 的资源段。资源只能编进最终的可执行文件，所以在这个
/// bin crate 里做（`cargo xtask win` 也由同一份 `assets/icon-512.png` 生成安装包的 `.ico`）。
///
/// 图形用圆角方块版（`cargo xtask icons` 的产物 assets/icon-512.png、已进仓库）：Windows 没有 macOS
/// 那种留边 + 投影的版式约定，满版圆角在任务栏里与别的 App 一样大。构建机上不必有 rsvg-convert，
/// 缩放在这里用 image 做。
#[cfg(windows)]
fn windows_resources(manifest: &std::path::Path, out: &std::path::Path) {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::imageops::FilterType;

    // 与 falcon_core::app_icon::DEFAULT_APP_ICON 同一个
    let src = manifest.join("assets/icon-512.png");
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
