//! 锁定版本的 meegle CLI（ADR 0010）。原来经 pnpm 装 `@lark-project/meegle` 拿各平台的
//! 静态二进制；仓库里不再有 Node，这里直接从 npm 仓库下同一个 tarball、按锁定的 sha512
//! 校验后解出本平台那个。
//!
//! - `cargo xtask server`（发布构建）把它编进服务端（embed-meegle），首次用到时释放到
//!   `<dataDir>/bin/meegle-<哈希>`；
//! - 开发构建的服务端找不到编进去的那份时，按 `native/.cache/meegle/bin/meegle-<平台>` 兜底
//!   （falcon-server 的 meegle/bin.rs），所以开发时跑一次 `cargo xtask meegle` 就行。
//!
//! 升级：改下面的 VERSION 与 INTEGRITY（npm 仓库里该版本的 `dist.integrity`），
//! 再按 meegle/command.rs 顶部的 CLI 约定核对一遍输出格式。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use sha2::Digest as _;

use crate::util::{download, host_platform, native, root};

/// 锁定版本
pub const VERSION: &str = "1.0.23";
/// 该版本 tarball 的 sha512（npm 的 dist.integrity）
const INTEGRITY: &str = "sha512-RDaz27RZaCHOxQcBbzt2cNlq5SPCktNPqQhTGvg2iVn116UbwQ7n79mKYKywokFnbsaoSu8kfbiIGIxQDnFsog==";

fn tarball_url() -> String {
    format!("https://registry.npmjs.org/@lark-project/meegle/-/meegle-{VERSION}.tgz")
}

fn cache_dir() -> PathBuf {
    native().join(".cache").join("meegle")
}

/// 包里本平台二进制的名字（与 npm 包 bin/ 下一致）：`meegle-darwin-arm64`、`meegle-win32-x64.exe`
pub fn bin_name(platform: &str) -> String {
    let exe = if platform.starts_with("win32") { ".exe" } else { "" };
    format!("meegle-{platform}{exe}")
}

pub fn run(args: &[String]) -> Result<()> {
    let mut platform = host_platform();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => platform = it.next().context("--target 后面缺平台名")?.clone(),
            other => bail!("未知参数：{other}"),
        }
    }
    let bin = ensure(&platform)?;
    println!("meegle {VERSION}（{platform}）：{}", bin.strip_prefix(root()).unwrap_or(&bin).display());
    Ok(())
}

/// 确保 `native/.cache/meegle/bin/<bin_name>` 是锁定版本，返回它的路径
pub fn ensure(platform: &str) -> Result<PathBuf> {
    let bin_dir = cache_dir().join("bin");
    let dest = bin_dir.join(bin_name(platform));
    let stamp = bin_dir.join(format!("{}.version", bin_name(platform)));
    if dest.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|v| v.trim() == VERSION) {
        return Ok(dest);
    }

    let tgz = cache_dir().join(format!("meegle-{VERSION}.tgz"));
    if !tgz.is_file() || verify(&tgz).is_err() {
        download(&tarball_url(), &tgz)?;
    }
    verify(&tgz)?;
    extract(&tgz, &format!("package/bin/{}", bin_name(platform)), &dest)
        .with_context(|| format!("@lark-project/meegle {VERSION} 里没有 {} 的二进制", platform))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(&stamp, VERSION)?;
    Ok(dest)
}

/// 按锁定的 sha512 校验 tarball
fn verify(tgz: &Path) -> Result<()> {
    let data = std::fs::read(tgz)?;
    let got = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&data)));
    if got != INTEGRITY {
        bail!("{} 校验不过：期望 {INTEGRITY}，实际 {got}", tgz.display());
    }
    Ok(())
}

/// 从 tar.gz 里解出一个文件
fn extract(tgz: &Path, entry: &str, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(tgz)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for item in archive.entries()? {
        let mut item = item?;
        if item.path()?.to_string_lossy() == entry {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // 先写临时文件再改名：正在跑的开发服务端可能正 spawn 着旧的那个
            let tmp = dest.with_extension("part");
            item.unpack(&tmp)?;
            std::fs::rename(&tmp, dest)?;
            return Ok(());
        }
    }
    bail!("tarball 里没有 {entry}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_names_match_the_npm_package_layout() {
        assert_eq!(bin_name("darwin-arm64"), "meegle-darwin-arm64");
        assert_eq!(bin_name("win32-x64"), "meegle-win32-x64.exe");
    }
}
