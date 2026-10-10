//! 移植自 `packages/server/src/meegle/bin.ts`（含 `command.test.ts` 里测它的那条用例）。
//!
//! meegle 可执行文件的定位。
//!
//! CLI 随服务内置：锁定版本的 `@lark-project/meegle` npm 包里带着六个平台的静态二进制
//! （bin/meegle-<platform>-<arch>[.exe]），我们直接 spawn 本平台那一个，不经它的 meegle.js
//! 包装——省一个 node 进程，也躲开包装脚本里的更新提示逻辑。版本与 sha512 锁在
//! `native/xtask/src/meegle.rs`，`cargo xtask meegle` 从 npm 仓库直接取 tarball 解出来（不经 Node）。
//!
//! 解析顺序：
//! 1. FALCON_MEEGLE_BIN —— 显式指定（文件得真的在：Node 版 SEA 把它设进自己的环境，
//!    从旧版 falcon 终端里起的进程会继承一个早已清掉的 runtime 路径）；
//! 2. 编进二进制的那份（`embed-meegle` feature，发布构建）—— 首次用时释放到
//!    `<dataDir>/bin/meegle-<内容哈希>`；
//! 3. `native/.cache/meegle/bin/` 里本平台的二进制 —— `cargo xtask meegle` 取下来的锁定版本
//!    （开发构建）；
//! 4. PATH 上的 `meegle` —— 用户自己 npm -g 装的，兜底。
//!
//! # 与 TS 的差别
//!
//! 第 3 步 Node 版用 `require.resolve` 从 server 包出发找依赖。Rust 二进制没有模块解析，
//! 这里退成**编译期的仓库位置**：`<本 crate>/../../.cache/meegle`（`CARGO_MANIFEST_DIR`
//! 推出来的；没取过就落到第 4 步）。只在从源码树跑的开发构建上有用；发布产物走第 2 步
//! （`cargo xtask server` 开 embed-meegle）。
//!
//! 第 4 步不在这里查 PATH：返回裸名 `meegle`，由 spawn 时按**子进程环境**（登录环境）里的
//! PATH 找——与 Node 的 spawn 同一口径（Rust 的 `Command` 在显式给了 PATH 时也按新 PATH 找）。

use std::path::{Path, PathBuf};

/// 显式指定 meegle 可执行文件的环境变量
pub const MEEGLE_BIN_ENV: &str = "FALCON_MEEGLE_BIN";

/// 与 @lark-project/meegle 的 bin/ 目录命名一致。`platform` / `arch` 是 Node 的叫法
/// （`darwin` / `linux` / `win32`，`arm64` / `x64`），缺省为本进程
pub fn bundled_bin_name(platform: Option<&str>, arch: Option<&str>) -> String {
    let platform = platform.unwrap_or(node_platform());
    let arch = arch.unwrap_or(node_arch());
    format!("meegle-{platform}-{arch}{}", if platform == "win32" { ".exe" } else { "" })
}

/// 开发态的取用位置（见文件头"与 TS 的差别"），布局同 npm 包：`<目录>/bin/<bundled_bin_name>`。
/// 目录不存在也照列，由调用方判断
pub fn bundled_package_dirs() -> Vec<PathBuf> {
    vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/meegle")]
}

/// `env` 读环境变量（生产上是 `|k| std::env::var(k).ok()`）
pub fn resolve_meegle_bin(env: impl Fn(&str) -> Option<String>) -> String {
    resolve_with(env(MEEGLE_BIN_ENV), &bundled_package_dirs())
}

/// 服务进程用的完整解析：显式指定 > 编进二进制的那份 > 依赖包 > PATH
pub fn resolve_for_server(data_dir: &Path) -> String {
    let explicit = std::env::var(MEEGLE_BIN_ENV).ok().filter(|b| !b.is_empty());
    if let Some(bin) = &explicit {
        if Path::new(bin).is_file() {
            return bin.clone();
        }
        log::warn!("{MEEGLE_BIN_ENV} 指向的文件不存在，忽略：{bin}");
    }
    if let Some(bin) = embedded::extract(data_dir) {
        return bin.to_string_lossy().into_owned();
    }
    resolve_with(None, &bundled_package_dirs())
}

/// 编进二进制的 meegle CLI（发布构建）。构建时 `FALCON_EMBED_MEEGLE_BIN` 指向本平台那个二进制
pub mod embedded {
    use std::path::{Path, PathBuf};

    #[cfg(feature = "embed-meegle")]
    static BYTES: &[u8] = include_bytes!(env!("FALCON_EMBED_MEEGLE_BIN"));

    /// 释放到 `<dataDir>/bin/meegle-<内容哈希前 12 位>` 并返回路径；没编进来时 None。
    /// 名字带哈希：升级后内容变了自然换一个文件，不去覆盖可能正在跑的旧版本
    pub fn extract(data_dir: &Path) -> Option<PathBuf> {
        #[cfg(feature = "embed-meegle")]
        {
            use sha2::Digest as _;
            let tag = hex::encode(sha2::Sha256::digest(BYTES));
            let dir = data_dir.join("bin");
            let dest = dir.join(format!("meegle-{}{}", &tag[..12], std::env::consts::EXE_SUFFIX));
            if dest.metadata().is_ok_and(|m| m.len() == BYTES.len() as u64) {
                return Some(dest);
            }
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(&dir)?;
                // 先写临时文件再改名：两个进程同时释放也不会看到半截文件
                let tmp = dir.join(format!(".meegle-{}.tmp-{}", &tag[..12], std::process::id()));
                std::fs::write(&tmp, BYTES)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
                }
                std::fs::rename(&tmp, &dest)
            };
            return match write() {
                Ok(()) => Some(dest),
                Err(e) => {
                    log::warn!("释放内置的 meegle CLI 失败（{}）：{e}", dest.display());
                    None
                }
            };
        }
        #[cfg(not(feature = "embed-meegle"))]
        {
            let _ = data_dir;
            None
        }
    }
}

fn resolve_with(explicit: Option<String>, package_dirs: &[PathBuf]) -> String {
    // if (env.FALCON_MEEGLE_BIN)：空串不算
    if let Some(bin) = explicit.filter(|b| !b.is_empty()) {
        return bin;
    }
    let name = bundled_bin_name(None, None);
    for dir in package_dirs {
        // 规整成真实路径（日志里看得懂、`..` 不留在命令行里）；目录不在（没取过）就落到 PATH
        let Ok(pkg) = dir.canonicalize() else { continue };
        let bin = pkg.join("bin").join(&name);
        if bin.exists() {
            return bin.to_string_lossy().into_owned();
        }
    }
    "meegle".into()
}

/// Node 的 `process.platform`
fn node_platform() -> &'static str {
    crate::sessions::login_env::node_platform()
}

/// Node 的 `process.arch`
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// bundledBinName 与 npm 包的 bin/ 命名一致
    #[test]
    fn bundled_bin_name_matches_npm_package_layout() {
        assert_eq!(bundled_bin_name(Some("darwin"), Some("arm64")), "meegle-darwin-arm64");
        assert_eq!(bundled_bin_name(Some("win32"), Some("x64")), "meegle-win32-x64.exe");
        // 显式指定优先于一切
        let env = |k: &str| (k == MEEGLE_BIN_ENV).then(|| "/x/meegle".to_string());
        assert_eq!(resolve_meegle_bin(env), "/x/meegle");
    }

    /// 依赖装着：解析到包里本平台的二进制（TS 断言的是真装着的依赖；这里在临时目录里
    /// 摆一个同样布局的包，不依赖 worktree 里有没有 node_modules）
    #[test]
    fn resolves_the_bundled_binary_then_falls_back_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("node_modules/@lark-project/meegle");
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        let missing = dir.path().join("nope");
        // 包在、二进制不在：落到 PATH
        assert_eq!(resolve_with(None, &[missing.clone(), pkg.clone()]), "meegle");
        std::fs::write(pkg.join("bin").join(bundled_bin_name(None, None)), b"").unwrap();
        let got = resolve_with(None, &[missing, pkg]);
        assert!(got.contains("@lark-project/meegle/bin/meegle-"), "{got}");
        // 空串的 FALCON_MEEGLE_BIN 不算显式指定
        assert_eq!(resolve_with(Some(String::new()), &[]), "meegle");
        assert_eq!(resolve_with(Some("/y/meegle".into()), &[]), "/y/meegle");
    }
}
