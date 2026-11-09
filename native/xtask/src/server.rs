//! 服务端发布单文件的构建（原 scripts/build-server.mjs）。
//!
//! `cargo xtask server` 出 `release/falcon-v<版本>-<平台>`，`cargo xtask pkg` 把它放进
//! Falcon.app 的 Resources，`falcon service install` 再把它拷到 `<dataDir>/bin/falcon`。
//! 不需要首次运行的解压步骤：5 个目标的会话守护进程（falcon-sessiond，ADR 0020）gzip 后编进
//! 二进制。Mac 上的后端也要往 Linux arm64 的远端推，用到哪个目标就释放 / 推送哪个
//! （falcon-server 的 sessiond/bin.rs）。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::util::{ host_platform, native, root, run_env, version};

/// 平台名 → Rust 目标三元组。Windows 不做本机服务（服务的打包与托管只做了 macOS / Linux）
const TRIPLES: [(&str, &str); 4] = [
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x64", "x86_64-apple-darwin"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
];

pub fn run(args: &[String]) -> Result<()> {
    let mut target = host_platform();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = it.next().context("--target 后面缺平台名")?.clone(),
            other => bail!("未知参数：{other}"),
        }
    }
    build(&target).map(|_| ())
}

/// 构建并返回产物路径
pub fn build(target: &str) -> Result<PathBuf> {
    let Some((_, triple)) = TRIPLES.iter().find(|(p, _)| *p == target) else {
        bail!("不支持的目标：{target}（可选：{}）", TRIPLES.map(|(p, _)| p).join(", "));
    };
    let cross = target != host_platform();

    // ---- 1. 5 个目标的会话守护进程（要 Zig 与 cargo-zigbuild，见 sessiond.rs）----
    println!("== 构建会话守护进程（cargo xtask sessiond --targets all） ==");
    crate::sessiond::build(&crate::sessiond::TARGETS, false, false)?;
    let embed_dir = native().join("target-sessiond/embed");
    std::fs::create_dir_all(&embed_dir)?;
    for (t, _) in crate::sessiond::TARGETS {
        let bytes = std::fs::read(crate::sessiond::artifact(t))?;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut gz, &bytes)?;
        std::fs::write(embed_dir.join(format!("{t}.gz")), gz.finish()?)?;
    }

    // ---- 2. cargo ----
    // 本机构建直接共用工作区的 native/target（与原生客户端共用同一套 release profile 缓存，
    // 公共依赖只编一遍），拷出发布产物后再用系统 strip 剥离符号；交叉编译产物本来就落在
    // target/<triple>/ 下，不冲掉宿主缓存，继续由 cargo 直接 strip。
    let target_dir = native().join("target");
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
        // rustup 的工具链排在前面：PATH 上的 Homebrew rustc 没有交叉目标的标准库
        cargo_args.extend([
            "--target".to_string(),
            triple.to_string(),
            "--config".to_string(),
            "profile.release.debug=false".to_string(),
            "--config".to_string(),
            "profile.release.strip=\"symbols\"".to_string(),
        ]);
    }
    println!("== cargo {} ==", cargo_args.join(" "));
    run_env(
        "cargo",
        &cargo_args,
        Some(&native()),
        &[("FALCON_EMBED_SESSIOND_DIR", embed_dir.as_os_str())],
    )?;

    let built = if cross { target_dir.join(triple) } else { target_dir }.join("release/falcon-server");
    let release = root().join("release");
    std::fs::create_dir_all(&release)?;
    let out = release.join(format!("falcon-v{}-{target}", version()));
    // 先删再拷：覆盖一个正在跑的同名文件时，macOS 会因改了已映射的签名文件把它 SIGKILL
    let _ = std::fs::remove_file(&out);
    std::fs::copy(&built, &out).with_context(|| format!("拷贝 {} 失败", built.display()))?;
    if !cross {
        crate::util::run("strip", [out.as_os_str()], None)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))?;
    }
    let mb = std::fs::metadata(&out)?.len() as f64 / 1024.0 / 1024.0;
    println!("== 完成：{}（{mb:.1} MB）==", out.strip_prefix(root()).unwrap_or(&out).display());
    Ok(out)
}
