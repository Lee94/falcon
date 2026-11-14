//! NSH 六端统一客户端开发、打包、检查与基准任务（ADR 0028）。

use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::util;

/// `cargo xtask tui [参数...]`：在当前终端运行 `falcon-app --tui`。
pub fn tui(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut cargo_args: Vec<String> = vec![
        "run".into(),
        "-p".into(),
        "falcon-app".into(),
        "--".into(),
        "--tui".into(),
    ];
    cargo_args.extend(args.iter().cloned());
    util::run("cargo", &cargo_args, Some(&native))
}

/// `cargo xtask web [--skip-cargo]`：构建纯 Rust WASM 站点（`nsh-platform-web`）并生成 `release/web/` 与归档包。
pub fn web(args: &[String]) -> Result<()> {
    let mut skip_cargo = false;
    for a in args {
        match a.as_str() {
            "--skip-cargo" => skip_cargo = true,
            other => bail!("未知参数：{other}"),
        }
    }

    let root = util::root();
    let native = util::native();
    let rel = |p: &Path| p.strip_prefix(&root).unwrap_or(p).display().to_string();

    if !skip_cargo {
        println!("== cargo build --release -p falcon-app --lib --target wasm32-unknown-unknown ==");
        let clang = util::which("clang");
        let llvm_ar = util::which("llvm-ar");
        let mut wasm_env: Vec<(&str, &std::ffi::OsStr)> = Vec::new();
        if std::env::var_os("CC_wasm32_unknown_unknown").is_none()
            && let Some(ref cc) = clang
        {
            wasm_env.push(("CC_wasm32_unknown_unknown", cc.as_os_str()));
        }
        if std::env::var_os("AR_wasm32_unknown_unknown").is_none()
            && let Some(ref ar) = llvm_ar
        {
            wasm_env.push(("AR_wasm32_unknown_unknown", ar.as_os_str()));
        }
        util::run_env(
            "cargo",
            [
                "build",
                "--release",
                "-p",
                "falcon-app",
                "--lib",
                "--target",
                "wasm32-unknown-unknown",
            ],
            Some(&native),
            &wasm_env,
        )?;
    }

    let wasm_src = native.join("target/wasm32-unknown-unknown/release/falcon_app.wasm");
    if !wasm_src.is_file() {
        bail!("找不到 {}，请先安装 rustup target wasm32-unknown-unknown 并去掉 --skip-cargo", rel(&wasm_src));
    }
    let wasm_bytes = std::fs::read(&wasm_src)?;
    let hash8 = &hex::encode(Sha256::digest(&wasm_bytes))[..8];
    let wasm_name = format!("falcon.{hash8}.wasm");

    let nsh_web_dir = util::cargo_package_dir("nsh-platform-web")?;
    let web_runtime = nsh_web_dir.join("web");

    let release_dir = root.join("release");
    let out_dir = release_dir.join("web");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(out_dir.join("browser"))?;

    std::fs::copy(web_runtime.join("nsh.js"), out_dir.join("nsh.js"))
        .context("复制 nsh-platform-web/web/nsh.js 失败")?;
    copy_dir_recursive(&web_runtime.join("browser"), &out_dir.join("browser"))?;
    copy_dir_recursive(
        &native.join("crates/falcon-nsh/assets"),
        &out_dir.join("assets"),
    )?;
    std::fs::copy(
        native.join("crates/falcon-app/assets/icon-512.png"),
        out_dir.join("favicon.png"),
    )?;
    std::fs::write(out_dir.join(&wasm_name), &wasm_bytes)?;
    std::fs::write(out_dir.join("index.html"), web_index_html(&wasm_name))?;

    let ver = util::version();
    let archive_path = release_dir.join(format!("Falcon-v{ver}-web.tar.gz"));
    archive_dir_tar_gz(&out_dir, &archive_path)?;

    let mb = std::fs::metadata(&archive_path)?.len() as f64 / 1024.0 / 1024.0;
    println!("✓ {} & {}（{mb:.2} MB）", rel(&out_dir), rel(&archive_path));
    Ok(())
}

/// `cargo xtask linux-desktop [--skip-cargo] [--target <平台>]`：构建 Linux GTK4 原生桌面安装包（`.tar.gz` + `.deb`）。
pub fn linux_desktop(args: &[String]) -> Result<()> {
    let mut skip_cargo = false;
    let mut target = util::host_platform();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--skip-cargo" => skip_cargo = true,
            "--target" => target = it.next().context("--target 后面缺平台名")?.clone(),
            other => bail!("未知参数：{other}"),
        }
    }
    if !cfg!(target_os = "linux") {
        bail!("Linux 桌面端安装包需要在 Linux 上构建（依赖 libgtk-4-dev）");
    }
    if !target.starts_with("linux-") {
        bail!("linux-desktop 只支持 linux-* 目标：{target}");
    }

    let root = util::root();
    let native = util::native();
    let rel = |p: &Path| p.strip_prefix(&root).unwrap_or(p).display().to_string();

    if !skip_cargo {
        println!("== cargo build --release -p falcon-app --features linux-desktop ==");
        util::run(
            "cargo",
            [
                "build",
                "--release",
                "-p",
                "falcon-app",
                "--features",
                "linux-desktop",
            ],
            Some(&native),
        )?;
    }

    let bin_path = native.join("target/release/falcon-app");
    if !bin_path.is_file() {
        bail!("找不到 {}，请去掉 --skip-cargo 重试", rel(&bin_path));
    }
    let bin_bytes = std::fs::read(&bin_path)?;
    let icon_bytes = std::fs::read(native.join("crates/falcon-app/assets/icon-512.png"))?;
    let desktop_entry = linux_desktop_entry();

    let ver = util::version();
    let release_dir = root.join("release");
    std::fs::create_dir_all(&release_dir)?;

    let tar_out = release_dir.join(format!("Falcon-v{ver}-{target}.tar.gz"));
    let tar_bytes = crate::linux::tar_gz(&[
        ("falcon-app/bin/falcon-app", &bin_bytes, 0o755),
        (
            "falcon-app/share/applications/falcon.desktop",
            desktop_entry.as_bytes(),
            0o644,
        ),
        (
            "falcon-app/share/icons/hicolor/512x512/apps/falcon.png",
            &icon_bytes,
            0o644,
        ),
    ])?;
    std::fs::write(&tar_out, tar_bytes)?;

    let deb_arch = crate::linux::DEB_ARCH
        .iter()
        .find(|(p, _)| *p == target)
        .map(|(_, a)| *a)
        .with_context(|| format!("没有 {target} 对应的 deb 架构名"))?;
    let size_kb = ((bin_bytes.len() + icon_bytes.len()) / 1024).max(1);
    let control = format!(
        "Package: falcon-app\nVersion: {ver}\nArchitecture: {deb_arch}\nMaintainer: Falcon\nInstalled-Size: {size_kb}\nSection: devel\nPriority: optional\nDepends: libc6, libgtk-4-1, libwebkitgtk-6.0-4, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0\nDescription: Falcon Native Shell client\n Falcon 跨端统一客户端（GTK 4 原生桌面版 + 终端 TUI）。\n"
    );
    let control_tar = crate::linux::tar_gz(&[("control", control.as_bytes(), 0o644)])?;
    let data_tar = crate::linux::tar_gz(&[
        ("./usr/bin/falcon-app", &bin_bytes, 0o755),
        (
            "./usr/share/applications/falcon.desktop",
            desktop_entry.as_bytes(),
            0o644,
        ),
        (
            "./usr/share/icons/hicolor/512x512/apps/falcon.png",
            &icon_bytes,
            0o644,
        ),
    ])?;
    let deb_bytes = crate::linux::ar(&[
        ("debian-binary", b"2.0\n".as_slice()),
        ("control.tar.gz", &control_tar),
        ("data.tar.gz", &data_tar),
    ]);
    let deb_out = release_dir.join(format!("Falcon-v{ver}-{target}.deb"));
    std::fs::write(&deb_out, deb_bytes)?;

    let mb = std::fs::metadata(&deb_out)?.len() as f64 / 1024.0 / 1024.0;
    println!("✓ {} & {}（{mb:.1} MB）", rel(&tar_out), rel(&deb_out));
    Ok(())
}

/// `cargo xtask nsh-check`：局部编译检查 `falcon-nsh` 与 `falcon-app`（含测试目标，共用 release 依赖缓存）。
pub fn check(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut check_nsh = vec![
        "check".to_string(),
        "--release".to_string(),
        "-p".to_string(),
        "falcon-nsh".to_string(),
        "--tests".to_string(),
    ];
    check_nsh.extend(args.iter().cloned());
    util::run("cargo", &check_nsh, Some(&native))?;

    let mut check_app = vec![
        "check".to_string(),
        "--release".to_string(),
        "-p".to_string(),
        "falcon-app".to_string(),
        "--tests".to_string(),
    ];
    check_app.extend(args.iter().cloned());
    util::run("cargo", &check_app, Some(&native))
}

/// `cargo xtask nsh-bench`：在 `--release` 下运行 `FalconTerminal` 30 万行高吞吐帧时延门禁基准。
pub fn bench(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut bench_args = vec![
        "test".to_string(),
        "--release".to_string(),
        "-p".to_string(),
        "falcon-nsh".to_string(),
        "phase0_gate_benchmark_300k_mixed_cjk_box_ansi_under_4ms_p95".to_string(),
        "--".to_string(),
        "--nocapture".to_string(),
    ];
    bench_args.extend(args.iter().cloned());
    util::run("cargo", &bench_args, Some(&native))
}

pub(crate) fn web_index_html(wasm_filename: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
  <title>Falcon</title>
  <link rel="icon" type="image/png" href="./favicon.png" />
  <style>
    html, body, #app {{
      margin: 0;
      padding: 0;
      width: 100%;
      height: 100%;
      overflow: hidden;
      background: #0f0f12;
      -webkit-font-smoothing: antialiased;
    }}
    #app {{
      position: fixed;
      inset: 0;
    }}
  </style>
</head>
<body>
  <div id="app"></div>
  <script type="module">
    import {{ createPlayer }} from "./nsh.js";
    const player = await createPlayer(document.getElementById("app"), {{
      dialogs: "viewport",
      assetBase: new URL("./", location.href).href,
      wasm: "./{wasm_filename}",
    }});
    player.runRust();
  </script>
</body>
</html>
"#
    )
}

pub(crate) fn linux_desktop_entry() -> &'static str {
    "[Desktop Entry]
Name=Falcon
Comment=Falcon Workspace & Terminal
Exec=/usr/bin/falcon-app %u
Icon=falcon
Terminal=false
Type=Application
Categories=Development;TerminalEmulator;
MimeType=x-scheme-handler/falcon;
StartupWMClass=com.falcon.app
"
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let out = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &out)?;
        } else {
            std::fs::copy(&path, &out)?;
        }
    }
    Ok(())
}

fn archive_dir_tar_gz(dir: &Path, out_tar_gz: &Path) -> Result<()> {
    let file = std::fs::File::create(out_tar_gz)?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut builder = tar::Builder::new(enc);
    builder.append_dir_all("web", dir)?;
    builder.into_inner()?.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_index_html_boots_pure_rust_player() {
        let html = web_index_html("falcon.1a2b3c4d.wasm");
        assert!(html.contains(r#"import { createPlayer } from "./nsh.js";"#));
        assert!(html.contains(r#"wasm: "./falcon.1a2b3c4d.wasm""#));
        assert!(html.contains("player.runRust();"));
    }

    #[test]
    fn linux_desktop_entry_registers_falcon_scheme() {
        let desktop = linux_desktop_entry();
        assert!(desktop.contains("Exec=/usr/bin/falcon-app %u"));
        assert!(desktop.contains("MimeType=x-scheme-handler/falcon;"));
    }
}
