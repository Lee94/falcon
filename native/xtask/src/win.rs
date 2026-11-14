//! Windows 安装包（Inno Setup 的 setup.exe，原 scripts/build-windows-installer.mjs）：
//! 原生客户端（NSH `falcon-app`）+ 同目录的服务端单文件 falcon-server.exe。
//!
//! 与 macOS pkg 一样把服务端捆进安装包：Inno Setup 装完就用它跑 `falcon service install`
//! 注册登录自启的计划任务（schtasks，免管理员）。没有服务端产物时先 `cargo xtask server`。
//!
//! ```text
//! cargo xtask win                 # cargo build --release，再编安装包
//! cargo xtask win --skip-cargo    # 只用已有的 native/target/release/falcon-app.exe
//! ```
//!
//! 产物 release/Falcon-v<版本>-win32-x64-setup.exe。没有代码签名证书，别人机器上
//! SmartScreen 会拦，「更多信息 → 仍要运行」即可。
//!
//! 必须在 Windows 上跑：exe 的图标与版本信息由 falcon-app 的 build.rs 经 rc.exe 编进去
//! （Windows SDK，随 VS 生成工具装）；安装包要 Inno Setup 6 的 ISCC.exe——
//! `winget install JRSoftware.InnoSetup --scope user`，或用 FALCON_ISCC 指到它。
//! 安装脚本在 native/xtask/assets/windows-installer/Falcon.iss。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::util::{self, host_platform, native, root, version};

pub fn run(args: &[String]) -> Result<()> {
    let mut skip_cargo = false;
    for a in args {
        match a.as_str() {
            "--skip-cargo" => skip_cargo = true,
            other => bail!("未知参数：{other}"),
        }
    }
    if !cfg!(windows) {
        bail!("Windows 安装包只能在 Windows 上打（需要 rc.exe 与 Inno Setup）");
    }
    let rel = |p: &std::path::Path| p.strip_prefix(root()).unwrap_or(p).display().to_string();

    let iscc = find_iscc()
        .context("找不到 Inno Setup 6 的 ISCC.exe：winget install JRSoftware.InnoSetup --scope user，或设 FALCON_ISCC")?;

    if !skip_cargo {
        // crates.io 走 native/.cargo/config.toml 里的镜像配置
        println!("== cargo build --release -p falcon-app ==");
        util::run("cargo", ["build", "--release", "-p", "falcon-app"], Some(&native()))?;
    }
    let exe = native().join("target/release/falcon-app.exe");
    if !exe.is_file() {
        bail!("找不到 {}，先去掉 --skip-cargo 跑一遍", rel(&exe));
    }

    // Cargo 1.100 起调整了内部 build-dir 布局，不再从 target/release/build/*/out/ 捞私有产物；
    // 直接由同一份 assets/icon-512.png 生成安装包用的 .ico（--skip-cargo 下同样可用）
    let icon = native().join("target/release/falcon.ico");
    write_windows_ico(&native().join("crates/falcon-app/assets/icon-512.png"), &icon)?;

    // 版本跟原生客户端走（exe 版本信息里也是这个，同一个工作区版本）
    let ver = version();
    let release = root().join("release");

    // 捆绑的服务端单文件（发布产物，不是 target 里那份——它内嵌了守护进程）。
    // 守护进程不在 Windows 上构建（zig_host 只认 macOS / Linux），本地补建只能
    // --skip-sessiond 复用 native/target-sessiond/embed；CI 的 windows job 就是先拿
    // falcon-sessiond-embed artifact 再 server --skip-sessiond
    let platform = host_platform();
    let server = release.join(format!("falcon-v{ver}-{platform}.exe"));
    if !server.is_file() {
        if skip_cargo {
            bail!("找不到 {}，先 cargo xtask server --skip-sessiond（embed 取自 CI artifact falcon-sessiond-embed）", rel(&server));
        }
        println!("== 没有服务端产物，先 cargo xtask server --skip-sessiond ==");
        crate::server::build_skipping_sessiond(&platform)
            .context("Windows 上编不了会话守护进程：把 CI 的 falcon-sessiond-embed 解到 native/target-sessiond/embed，再跑 cargo xtask server --skip-sessiond")?;
    }
    if !server.is_file() {
        bail!("cargo xtask server 之后仍没有 {}", rel(&server));
    }
    std::fs::create_dir_all(&release)?;
    let base = format!("Falcon-v{ver}-{}-setup", host_platform());
    let iss = native().join("xtask/assets/windows-installer/Falcon.iss");
    println!("== ISCC {} ==", rel(&iss));
    util::run(
        &iscc,
        [
            "/Qp".to_string(),
            format!("/DAppVersion={ver}"),
            format!("/DSourceExe={}", exe.display()),
            format!("/DSourceServer={}", server.display()),
            format!("/DSourceIcon={}", icon.display()),
            format!("/DOutputDir={}", release.display()),
            format!("/DOutputBaseFilename={base}"),
            iss.display().to_string(),
        ],
        Some(&root()),
    )?;

    let out = release.join(format!("{base}.exe"));
    let mb = std::fs::metadata(&out)?.len() as f64 / 1024.0 / 1024.0;
    println!("✓ {}（{mb:.1} MB）", rel(&out));
    Ok(())
}

fn find_iscc() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let inno = |base: PathBuf| base.join("Inno Setup 6").join("ISCC.exe");
    let candidates = [
        env("FALCON_ISCC"),
        env("LOCALAPPDATA").map(|d| inno(d.join("Programs"))),
        env("ProgramFiles(x86)").map(inno),
        env("ProgramFiles").map(inno),
    ];
    if let Some(found) = candidates.into_iter().flatten().find(|p| p.is_file()) {
        return Some(found);
    }
    // PATH 兜底
    util::which("ISCC.exe")
}

fn write_windows_ico(src: &std::path::Path, ico: &std::path::Path) -> Result<()> {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::imageops::FilterType;

    let img = image::open(src)
        .with_context(|| format!("读不到 {}，请先运行 cargo xtask icons", src.display()))?
        .into_rgba8();
    let frames: Vec<IcoFrame> = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&s| {
            let scaled = image::imageops::resize(&img, s, s, FilterType::Lanczos3);
            IcoFrame::as_png(scaled.as_raw(), s, s, image::ExtendedColorType::Rgba8)
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<_>>()?;
    if let Some(parent) = ico.parent() {
        std::fs::create_dir_all(parent)?;
    }
    IcoEncoder::new(std::fs::File::create(ico)?).encode_images(&frames)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_installer_ico_from_app_icon() {
        let out = std::env::temp_dir().join(format!("falcon-xtask-win-test-{}.ico", std::process::id()));
        let src = native().join("crates/falcon-app/assets/icon-512.png");
        write_windows_ico(&src, &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(&bytes[..6], &[0, 0, 1, 0, 9, 0]);
    }
}
