//! Zellij 相关的两件事：
//!
//! - `cargo xtask zellij-update <版本|--latest>`（原 scripts/update-zellij.mjs）：只改
//!   falcon-server/src/zellij/version.rs 里的版本常量，并校验该 tag 下我们需要的五个 target
//!   产物都存在——不校验哈希，因为二进制由宿主机自己下载、后端不经手（ADR 0001「供应链与授权」）。
//!   升级版本是一次有意的发版行为：改完请跑一遍真实远端的手工验证再发。
//! - `cargo xtask zellij-plugin`（原 scripts/build-zellij-plugin.mjs）：编译滚动位置插件
//!   （native/zellij-plugin，ADR 0019），产物拷到 falcon-server/assets/falcon-scroll.wasm——这份
//!   是提交进仓库的，服务端编译时 include_bytes! 进二进制，平时的构建、发布与 CI 都直接用它，
//!   只有改了插件或升了 zellij 才需要跑。
//!
//! 插件要 rustup 管的工具链带 wasm32-wasip1 target（Homebrew 的 rust 没有这个 target）：
//!
//! ```text
//! brew install rustup        # keg-only，不会盖住 PATH 里的 Homebrew rust
//! rustup toolchain install stable --profile minimal --target wasm32-wasip1
//! ```
//!
//! 实测坑：直接调工具链里的 cargo 时它会去 PATH 上找 rustc（找到 Homebrew 那个，报
//! "can't find crate for core"），rust-lld 也找不到 libLLVM.dylib——平时是 rustup 的
//! 代理替你设好的。这里显式把 RUSTC、PATH 和动态库路径都指进工具链。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::{self, native, root};

const TARGETS: [&str; 5] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

const VERSION_PREFIX: &str = "pub const ZELLIJ_VERSION: &str = \"";

fn version_file() -> PathBuf {
    native().join("crates/falcon-server/src/zellij/version.rs")
}

/// version.rs 里锁定的版本
fn locked_version() -> Result<String> {
    let src = std::fs::read_to_string(version_file())?;
    quoted_after(&src, VERSION_PREFIX).context("version.rs 里找不到 ZELLIJ_VERSION")
}

/// `prefix` 之后、下一个 `"` 之前的那段
fn quoted_after(src: &str, prefix: &str) -> Option<String> {
    let rest = &src[src.find(prefix)? + prefix.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

fn github(path: &str) -> Result<serde_json::Value> {
    let url = format!("https://api.github.com/repos/zellij-org/zellij/{path}");
    let body = util::output(
        "curl",
        ["-fsSL", "--retry", "3", "-A", "falcon-xtask", "-H", "accept: application/vnd.github+json", url.as_str()],
        None,
    )
    .with_context(|| format!("请求 {url} 失败"))?;
    Ok(serde_json::from_str(&body)?)
}

pub fn update(args: &[String]) -> Result<()> {
    let version = match args {
        [v] if v != "--latest" => v.trim_start_matches('v').to_string(),
        [v] if v == "--latest" => github("releases/latest")?["tag_name"]
            .as_str()
            .context("releases/latest 没有 tag_name")?
            .trim_start_matches('v')
            .to_string(),
        _ => bail!("用法：cargo xtask zellij-update <版本|--latest>"),
    };
    println!("目标版本：v{version}");

    let release = github(&format!("releases/tags/v{version}")).with_context(|| format!("找不到 release v{version}"))?;
    let names: Vec<&str> = release["assets"]
        .as_array()
        .context("release 没有 assets")?
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    // 我们用 no-web 变体：省约 8 MiB，也少一个不需要的监听端口
    let missing: Vec<&str> = TARGETS
        .into_iter()
        .filter(|t| {
            let ext = if t.contains("windows") { "zip" } else { "tar.gz" };
            !names.contains(&format!("zellij-no-web-{t}.{ext}").as_str())
        })
        .collect();
    if !missing.is_empty() {
        bail!("v{version} 缺少这些 target 的 no-web 产物：\n  {}", missing.join("\n  "));
    }

    let file = version_file();
    let src = std::fs::read_to_string(&file)?;
    let old = locked_version()?;
    if old == version {
        println!("版本未变化，无需修改。");
        return Ok(());
    }
    let next = src.replacen(&format!("{VERSION_PREFIX}{old}\";"), &format!("{VERSION_PREFIX}{version}\";"), 1);
    std::fs::write(&file, next)?;
    println!("已更新 {}", file.strip_prefix(root()).unwrap_or(&file).display());
    println!("\n提醒：Zellij 的 CLIENT_SERVER_CONTRACT_VERSION 若在此版本改变，宿主机上已有的会话将无法接回。发版前请确认 CHANGELOG。");
    println!(
        "\n滚动位置插件（ADR 0019）的 zellij-tile 要同步改成 ={version}（native/zellij-plugin/Cargo.toml），再跑 cargo xtask zellij-plugin。"
    );
    Ok(())
}

pub fn build_plugin(args: &[String]) -> Result<()> {
    if let Some(a) = args.first() {
        bail!("未知参数：{a}");
    }
    let krate = native().join("zellij-plugin");
    let out = native().join("crates/falcon-server/assets/falcon-scroll.wasm");

    // 插件 API 跟着 zellij 版本走，两边必须同步升级
    let zellij_version = locked_version()?;
    let tile_version = quoted_after(&std::fs::read_to_string(krate.join("Cargo.toml"))?, "zellij-tile = \"=")
        .context("native/zellij-plugin/Cargo.toml 里找不到 zellij-tile = \"=…\"")?;
    if tile_version != zellij_version {
        bail!("zellij-tile（{tile_version}）与锁定的 zellij（{zellij_version}）版本不一致，先改 native/zellij-plugin/Cargo.toml");
    }

    let rustup = find_rustup().context("找不到 rustup（见 native/xtask/src/zellij.rs 顶部的安装说明）")?;
    let rustc = PathBuf::from(util::output(&rustup, ["which", "--toolchain", "stable", "rustc"], None)?.trim());
    let toolchain = rustc.parent().and_then(Path::parent).context("rustc 路径不对")?.to_path_buf();
    if !toolchain.join("lib/rustlib/wasm32-wasip1").is_dir() {
        bail!(
            "工具链 {} 没有 wasm32-wasip1 target：\n  {} target add wasm32-wasip1 --toolchain stable",
            toolchain.display(),
            rustup.display()
        );
    }

    let lib = toolchain.join("lib");
    let path = std::env::join_paths(
        std::iter::once(toolchain.join("bin")).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )?;
    let env: [(&str, OsString); 4] = [
        ("PATH", path),
        ("RUSTC", rustc.into_os_string()),
        ("DYLD_FALLBACK_LIBRARY_PATH", lib.clone().into_os_string()),
        ("LD_LIBRARY_PATH", lib.into_os_string()),
    ];
    let env: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    util::run_env(toolchain.join("bin/cargo"), ["build", "--release"], Some(&krate), &env)?;

    std::fs::copy(krate.join("target/wasm32-wasip1/release/falcon-scroll.wasm"), &out)?;
    println!(
        "已更新 {}（{} 字节）",
        out.strip_prefix(root()).unwrap_or(&out).display(),
        std::fs::metadata(&out)?.len()
    );
    Ok(())
}

fn find_rustup() -> Option<PathBuf> {
    util::which("rustup").or_else(|| {
        ["/opt/homebrew/opt/rustup/bin/rustup", "/usr/local/opt/rustup/bin/rustup"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_locked_version_and_tile_pin() {
        assert_eq!(quoted_after("x\npub const ZELLIJ_VERSION: &str = \"0.45.1\";\n", VERSION_PREFIX).as_deref(), Some("0.45.1"));
        assert_eq!(quoted_after("zellij-tile = \"=0.45.1\"\n", "zellij-tile = \"=").as_deref(), Some("0.45.1"));
        assert!(locked_version().is_ok());
    }
}
