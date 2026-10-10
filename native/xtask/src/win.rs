//! Windows 安装包（Inno Setup 的 setup.exe，原 scripts/build-windows-installer.mjs）：
//! 只装原生客户端（GPUI）。
//!
//! 与 macOS pkg 不同，这里没有捆绑服务端——服务本身依赖 Zellij 与 POSIX shell（见 README），
//! 装好后在客户端里连别处的 falcon 服务。
//!
//! ```text
//! cargo xtask win                 # cargo build --release，再编安装包
//! cargo xtask win --skip-cargo    # 只用已有的 native/target/release/falcon-desktop.exe
//! ```
//!
//! 产物 release/Falcon-v<版本>-win32-x64-setup.exe。没有代码签名证书，别人机器上
//! SmartScreen 会拦，「更多信息 → 仍要运行」即可。
//!
//! 必须在 Windows 上跑：exe 的图标与版本信息由 falcon-desktop 的 build.rs 经 rc.exe 编进去
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
        println!("== cargo build --release -p falcon-desktop ==");
        util::run("cargo", ["build", "--release", "-p", "falcon-desktop"], Some(&native()))?;
    }
    let exe = native().join("target/release/falcon-desktop.exe");
    if !exe.is_file() {
        bail!("找不到 {}，先去掉 --skip-cargo 跑一遍", rel(&exe));
    }

    // 安装包自己的图标用 build.rs 编进 exe 的同一份 .ico（在 OUT_DIR 里，目录名带哈希；
    // 换过构建配置会留下好几个，取最新的那个）
    let build_dir = native().join("target/release/build");
    let icon = std::fs::read_dir(&build_dir)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("falcon-desktop-"))
        .map(|e| e.path().join("out/falcon.ico"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
        .context("target/release/build/falcon-desktop-*/out/ 里没有 falcon.ico，build.rs 没走 Windows 资源那一段？")?;

    // 版本跟原生客户端走（exe 版本信息里也是这个，同一个工作区版本）
    let ver = version();
    let release = root().join("release");
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
