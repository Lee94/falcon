//! 服务端发布单文件与浏览器版客户端的构建（原 scripts/build-server.mjs）。
//!
//! `cargo xtask server` 出 `release/falcon-v<版本>-<平台>`，`cargo xtask pkg` 把它放进
//! Falcon.app 的 Resources，`falcon service install` 再把它拷到 `<dataDir>/bin/falcon`。
//! 不需要首次运行的解压步骤：
//! - 浏览器版客户端（build-web.sh 的产物，rust-embed）与 Zellij 滚动插件直接从二进制里服务；
//! - 锁定版本的 meegle CLI 编进二进制，首次用到时释放到 `<dataDir>/bin/meegle-<内容哈希>`。
//!
//! 浏览器版要 rustup 的 wasm32-unknown-unknown target 与同版本的 wasm-bindgen-cli（见 build-web.sh）。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::util::{self, host_platform, native, root, run_env, version};

/// 平台名 → Rust 目标三元组。Windows 不做本机服务（服务本身依赖 Zellij 与 POSIX shell）
const TRIPLES: [(&str, &str); 4] = [
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x64", "x86_64-apple-darwin"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
];

/// 浏览器版客户端，产物 native/target-wasm/dist（开发构建的服务端缺省就托管它）
pub fn build_web() -> Result<()> {
    util::run(native().join("scripts/build-web.sh"), std::iter::empty::<&str>(), Some(&native()))
}

pub fn run(args: &[String]) -> Result<()> {
    let mut target = host_platform();
    let mut skip_web = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = it.next().context("--target 后面缺平台名")?.clone(),
            "--skip-web" => skip_web = true,
            other => bail!("未知参数：{other}"),
        }
    }
    build(&target, skip_web).map(|_| ())
}

/// 构建并返回产物路径
pub fn build(target: &str, skip_web: bool) -> Result<PathBuf> {
    let Some((_, triple)) = TRIPLES.iter().find(|(p, _)| *p == target) else {
        bail!("不支持的目标：{target}（可选：{}）", TRIPLES.map(|(p, _)| p).join(", "));
    };
    let cross = target != host_platform();

    // ---- 1. 浏览器版客户端（GPUI 编到 wasm）----
    let web_dist = native().join("target-wasm/dist");
    if !skip_web {
        println!("== 构建浏览器版（native/scripts/build-web.sh） ==");
        build_web()?;
    }
    if !web_dist.join("index.html").is_file() {
        bail!("native/target-wasm/dist 缺少 index.html，先跑 cargo xtask web");
    }

    // ---- 2. 本平台的 meegle CLI ----
    let meegle_bin = crate::meegle::ensure(target)?;

    // ---- 3. cargo ----
    // 发布产物不带调试信息（工作区的 release profile 为原生客户端留着 debuginfo）。改 profile 会让
    // 依赖全部重编，所以单独一个 target 目录，不跟原生客户端的 release 缓存互相冲掉
    let target_dir = native().join("target-server");
    let mut cargo_args: Vec<String> = [
        "build",
        "--release",
        "-p",
        "falcon-server",
        "--features",
        "embed-web,embed-meegle",
        "--config",
        "profile.release.debug=false",
        "--config",
        "profile.release.strip=\"symbols\"",
    ]
    .map(str::to_string)
    .to_vec();
    if cross {
        // rustup 的工具链排在前面：PATH 上的 Homebrew rustc 没有交叉目标的标准库
        cargo_args.extend(["--target".to_string(), triple.to_string()]);
    }
    println!("== cargo {} ==", cargo_args.join(" "));
    run_env(
        "cargo",
        &cargo_args,
        Some(&native()),
        &[
            ("FALCON_EMBED_WEB_DIR", web_dist.as_os_str()),
            ("FALCON_EMBED_MEEGLE_BIN", meegle_bin.as_os_str()),
            ("CARGO_TARGET_DIR", target_dir.as_os_str()),
        ],
    )?;

    let built = if cross { target_dir.join(triple) } else { target_dir.clone() }.join("release/falcon-server");
    let release = root().join("release");
    std::fs::create_dir_all(&release)?;
    let out = release.join(format!("falcon-v{}-{target}", version()));
    // 先删再拷：覆盖一个正在跑的同名文件时，macOS 会因改了已映射的签名文件把它 SIGKILL
    let _ = std::fs::remove_file(&out);
    std::fs::copy(&built, &out).with_context(|| format!("拷贝 {} 失败", built.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))?;
    }
    let mb = std::fs::metadata(&out)?.len() as f64 / 1024.0 / 1024.0;
    println!("== 完成：{}（{mb:.1} MB）==", out.strip_prefix(root()).unwrap_or(&out).display());
    Ok(out)
}
