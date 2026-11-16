//! 服务端发布单文件的构建（原 scripts/build-server.mjs）。
//!
//! `cargo xtask server` 出 `release/falcon-v<版本>-<平台>`，`cargo xtask pkg` 把它放进
//! Falcon.app 的 Resources，`falcon service install` 再把它拷到 `<dataDir>/bin/falcon`。
//! 不需要首次运行的解压步骤：4 个目标的会话守护进程（falcon-sessiond，ADR 0020）gzip 后编进
//! 二进制。Mac 上的后端也要往 Linux arm64 的远端推，用到哪个目标就释放 / 推送哪个
//! （falcon-server 的 sessiond/bin.rs）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::{ host_platform, native, root, run_env, version};

/// 平台名 → Rust 目标三元组。Windows 发布可独立运行的服务端；系统服务托管仍由部署方负责。
const TRIPLES: [(&str, &str); 4] = [
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
    ("win32-x64", "x86_64-pc-windows-msvc"),
];

pub fn run(args: &[String]) -> Result<()> {
    let mut target = host_platform();
    let mut skip_sessiond = false;
    let mut skip_cargo = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = it.next().context("--target 后面缺平台名")?.clone(),
            "--skip-sessiond" => skip_sessiond = true,
            "--skip-cargo" => skip_cargo = true,
            other => bail!("未知参数：{other}"),
        }
    }
    build_opts(&target, skip_sessiond, skip_cargo).map(|_| ())
}

/// 构建并返回产物路径
pub fn build(target: &str) -> Result<PathBuf> {
    build_opts(target, false, false)
}

/// 复用已有 target-sessiond/embed 的构建（Windows 上编不了守护进程——Zig 不在 Windows
/// 构建，embed 得从 macOS / Linux 侧拿）。xtask win 的自动补建走这条路
pub fn build_skipping_sessiond(target: &str) -> Result<PathBuf> {
    build_opts(target, true, false)
}

fn build_opts(target: &str, skip_sessiond: bool, skip_cargo: bool) -> Result<PathBuf> {
    let Some((_, triple)) = TRIPLES.iter().find(|(p, _)| *p == target) else {
        bail!("不支持的目标：{target}（可选：{}）", TRIPLES.map(|(p, _)| p).join(", "));
    };
    let cross = target != host_platform();

    // ---- 1. 4 个目标的会话守护进程（要 Zig 与 cargo-zigbuild，见 sessiond.rs）----
    let embed_dir = native().join("target-sessiond/embed");
    if skip_sessiond {
        // CI 在独立 sessiond job 构建一次四目标守护进程，其他 job 只消费同一次 workflow 的压缩产物。
        for (t, _) in crate::sessiond::TARGETS {
            let path = embed_dir.join(format!("{t}.gz"));
            if !path.is_file() { bail!("--skip-sessiond 缺少 {}", path.display()); }
        }
    } else {
        println!("== 构建会话守护进程（cargo xtask sessiond --targets all） ==");
        crate::sessiond::build(&crate::sessiond::TARGETS, false, false)?;
        crate::sessiond::pack_embed()?;
    }

    // ---- 2. cargo ----
    // 本机构建直接共用工作区的 native/target（或由 CI 外层统一执行一次多包 `cargo build --release` 后传
    // `--skip-cargo`，避免与 falcon-desktop 因 tokio / reqwest 特性并集不同而重复编译整棵公共依赖树）；
    // 交叉编译产物落在 target/<triple>/ 下，不冲掉宿主缓存，继续由 cargo 直接 strip。
    let target_dir = native().join("target");
    if !skip_cargo {
        let mut cargo_args: Vec<String> = [
            "build",
            "--release",
            "-p",
            "falcon-server",
            "--features",
            "embed-sessiond",
        ]
        .map(str::to_string)
        .to_vec();
        if cross {
            cargo_args.extend([
                "--target".to_string(),
                triple.to_string(),
                "--config".to_string(),
                "profile.release.debug=false".to_string(),
                "--config".to_string(),
                "profile.release.strip=\"symbols\"".to_string(),
            ]);
            let tc = crate::util::rustup_toolchain_bin()?;
            let cargo_bin = tc.join("cargo");
            let rustc_bin = tc.join("rustc");
            let dylib_fallback = tc
                .parent()
                .expect("工具链目录")
                .join("lib")
                .into_os_string();
            let env: Vec<(&str, &std::ffi::OsStr)> = vec![
                ("FALCON_EMBED_SESSIOND_DIR", embed_dir.as_os_str()),
                ("RUSTC", rustc_bin.as_os_str()),
                ("DYLD_FALLBACK_LIBRARY_PATH", dylib_fallback.as_os_str()),
            ];
            println!("== cargo {} ==", cargo_args.join(" "));
            run_env(&cargo_bin, &cargo_args, Some(&native()), &env)?;
        } else {
            println!("== cargo {} ==", cargo_args.join(" "));
            run_env(
                "cargo",
                &cargo_args,
                Some(&native()),
                &[("FALCON_EMBED_SESSIOND_DIR", embed_dir.as_os_str())],
            )?;
        }
    }

    let suffix = if target.starts_with("win32-") { ".exe" } else { "" };
    let built = if cross { target_dir.join(triple) } else { target_dir }.join(format!("release/falcon-server{suffix}"));
    if !built.is_file() {
        bail!("找不到 {}，请去掉 --skip-cargo 重试", built.display());
    }
    let release = root().join("release");
    std::fs::create_dir_all(&release)?;
    let out = release.join(format!("falcon-v{}-{target}{suffix}", version()));
    // 先删再拷：覆盖一个正在跑的同名文件时，macOS 会因改了已映射的签名文件把它 SIGKILL
    let _ = std::fs::remove_file(&out);
    std::fs::copy(&built, &out).with_context(|| format!("拷贝 {} 失败", built.display()))?;
    if !cross && !target.starts_with("win32-") {
        crate::util::run("strip", [out.as_os_str()], None)?;
    }
    if target.starts_with("darwin-") && cfg!(target_os = "macos") {
        // strip 后重新签名；独立服务端不会再经过 pkg 的整个应用签名流程。
        crate::util::run("codesign", ["--force".as_ref(), "--sign".as_ref(), "-".as_ref(), out.as_os_str()], None)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = verify_embedded(&out, &embed_dir) {
        // 不合格的产物不留在 release/ 里，免得被当成可发布的文件
        let _ = std::fs::remove_file(&out);
        return Err(e);
    }
    let mb = std::fs::metadata(&out)?.len() as f64 / 1024.0 / 1024.0;
    println!("== 完成：{}（{mb:.1} MB）==", out.strip_prefix(root()).unwrap_or(&out).display());
    Ok(out)
}

/// 出包前核对内嵌的守护进程：embed 目录里每份 gzip 都要解得开、解出来是那个目标的可执行文件，
/// 并且原样出现在服务端产物里。
///
/// CI 先用占位文件编完整个工作区，守护进程产物到了再换上真文件重编（只重编 falcon-server 的
/// main.rs）。这里挡住两种会把坏东西发出去的情况：真文件没换上（占位内容不是 gzip），换上了但
/// cargo 没重编（产物里找不到这份 gzip）。本地 `--skip-cargo` 拿到没开 embed-sessiond 的旧产物
/// 也会在这里报错。
fn verify_embedded(binary: &Path, embed_dir: &Path) -> Result<()> {
    let bin = std::fs::read(binary).with_context(|| format!("读取 {} 失败", binary.display()))?;
    for (t, _) in crate::sessiond::TARGETS {
        let path = embed_dir.join(format!("{t}.gz"));
        let gz = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
        let mut exe = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz.as_slice()), &mut exe)
            .with_context(|| format!("{} 不是有效的 gzip（还是 CI 的占位文件？）", path.display()))?;
        let magic: &[u8] = if t.contains("windows") {
            b"MZ"
        } else if t.contains("apple-darwin") {
            &[0xcf, 0xfa, 0xed, 0xfe]
        } else {
            b"\x7fELF"
        };
        if !exe.starts_with(magic) {
            bail!("{} 解出来不是 {t} 的可执行文件", path.display());
        }
        if !bin.windows(gz.len()).any(|w| w == gz.as_slice()) {
            bail!(
                "{} 里没有 {} 的内容：服务端没有带 embed-sessiond 编，或换了守护进程后没重编",
                binary.display(),
                path.display()
            );
        }
    }
    println!("== 内嵌守护进程核对通过：{} 个目标 ==", crate::sessiond::TARGETS.len());
    Ok(())
}
