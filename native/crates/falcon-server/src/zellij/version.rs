//! Zellij 版本锁定与发行产物映射。移植自 `packages/server/src/zellij/version.ts`。
//!
//! 锁定版本而非跟随 latest：测试矩阵封闭、可回归，升级是一次有意的发版行为。
//! 更新版本请用 `cargo xtask zellij-update <version>`（native/xtask/src/zellij.rs），不要手改这里的常量。
//!
//! 不做完整性校验（无哈希常量）——走默认 GitHub 源时 HTTPS 已保证来源与完整性；
//! 配置了自定义 base URL 的用户会在 UI 上看到明确警告。

pub const ZELLIJ_VERSION: &str = "0.45.1";

/// 官方发行地址；每台主机可在 zellij_hosts 表中覆盖
pub const DEFAULT_BASE_URL: &str = "https://github.com/zellij-org/zellij/releases/download";

/// 用 no-web 变体：不需要 Zellij 自带的 web server，
/// 省约 8 MiB 体积，也少一个我们不需要的监听端口。
const VARIANT: &str = "no-web";

/// Zellij 官方发行的 target 三元组。没有 linux-gnu、freebsd、aarch64-windows。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZellijTarget {
    X86_64LinuxMusl,
    Aarch64LinuxMusl,
    X86_64AppleDarwin,
    Aarch64AppleDarwin,
    X86_64WindowsMsvc,
}

impl ZellijTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            ZellijTarget::X86_64LinuxMusl => "x86_64-unknown-linux-musl",
            ZellijTarget::Aarch64LinuxMusl => "aarch64-unknown-linux-musl",
            ZellijTarget::X86_64AppleDarwin => "x86_64-apple-darwin",
            ZellijTarget::Aarch64AppleDarwin => "aarch64-apple-darwin",
            ZellijTarget::X86_64WindowsMsvc => "x86_64-pc-windows-msvc",
        }
    }

    pub fn is_windows(self) -> bool {
        self == ZellijTarget::X86_64WindowsMsvc
    }

    /// Windows 为 zip，其余为 tar.gz
    pub fn archive(self) -> &'static str {
        if self.is_windows() { "zip" } else { "tar.gz" }
    }
}

impl std::fmt::Display for ZellijTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 发行包文件名，如 zellij-no-web-x86_64-unknown-linux-musl.tar.gz
pub fn asset_name(target: ZellijTarget) -> String {
    format!("zellij-{VARIANT}-{}.{}", target.as_str(), target.archive())
}

pub fn download_url(target: ZellijTarget, base_url: Option<&str>) -> String {
    let base = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
    format!("{base}/v{ZELLIJ_VERSION}/{}", asset_name(target))
}

/// `uname -sm` 输出 → target。无匹配返回 None（调用方降级为非持久并标注架构不支持）。
pub fn target_from_uname(uname: &str) -> Option<ZellijTarget> {
    let mut it = uname.split_whitespace();
    let (os, machine) = (it.next()?, it.next()?);
    let arm = machine == "aarch64" || machine == "arm64";
    let x64 = machine == "x86_64" || machine == "amd64";
    match os {
        "Linux" if x64 => Some(ZellijTarget::X86_64LinuxMusl),
        "Linux" if arm => Some(ZellijTarget::Aarch64LinuxMusl),
        "Darwin" if x64 => Some(ZellijTarget::X86_64AppleDarwin),
        "Darwin" if arm => Some(ZellijTarget::Aarch64AppleDarwin),
        _ => None,
    }
}

/// Windows 的 %PROCESSOR_ARCHITECTURE% → target。
/// ARM64 也映射到 x86_64：Zellij 没有 Windows ARM64 产物（PR #5090/#5258 提了四个月未合），
/// 只能靠 Win11 的 x64 模拟跑，行不行由随后的 --version 握手裁决。
pub fn target_from_windows_arch(arch: &str) -> Option<ZellijTarget> {
    match arch.trim().to_uppercase().as_str() {
        "AMD64" | "ARM64" | "X86_64" => Some(ZellijTarget::X86_64WindowsMsvc),
        _ => None,
    }
}

/// 后端所在机器（本地会话）的 target
pub fn local_target() -> Option<ZellijTarget> {
    if cfg!(target_os = "windows") {
        // x64 与 arm64 都用 x64 产物，理由同 target_from_windows_arch
        return (cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"))
            .then_some(ZellijTarget::X86_64WindowsMsvc);
    }
    if cfg!(target_os = "linux") {
        if cfg!(target_arch = "x86_64") {
            return Some(ZellijTarget::X86_64LinuxMusl);
        }
        if cfg!(target_arch = "aarch64") {
            return Some(ZellijTarget::Aarch64LinuxMusl);
        }
        return None;
    }
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            return Some(ZellijTarget::X86_64AppleDarwin);
        }
        if cfg!(target_arch = "aarch64") {
            return Some(ZellijTarget::Aarch64AppleDarwin);
        }
    }
    None
}

/// 二进制文件名（不含目录）
pub fn binary_name(target: ZellijTarget) -> String {
    if target.is_windows() {
        format!("zellij-{ZELLIJ_VERSION}.exe")
    } else {
        format!("zellij-{ZELLIJ_VERSION}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_and_url() {
        assert_eq!(asset_name(ZellijTarget::X86_64LinuxMusl), "zellij-no-web-x86_64-unknown-linux-musl.tar.gz");
        assert_eq!(asset_name(ZellijTarget::X86_64WindowsMsvc), "zellij-no-web-x86_64-pc-windows-msvc.zip");
        assert_eq!(
            download_url(ZellijTarget::Aarch64AppleDarwin, Some("https://mirror.example/z/")),
            format!("https://mirror.example/z/v{ZELLIJ_VERSION}/zellij-no-web-aarch64-apple-darwin.tar.gz")
        );
    }

    #[test]
    fn uname_mapping() {
        assert_eq!(target_from_uname("Linux x86_64\n"), Some(ZellijTarget::X86_64LinuxMusl));
        assert_eq!(target_from_uname("Linux aarch64"), Some(ZellijTarget::Aarch64LinuxMusl));
        assert_eq!(target_from_uname("Darwin arm64"), Some(ZellijTarget::Aarch64AppleDarwin));
        assert_eq!(target_from_uname("FreeBSD amd64"), None);
        assert_eq!(target_from_uname("Linux armv7l"), None);
        assert_eq!(target_from_uname("Linux"), None);
        assert_eq!(target_from_windows_arch(" arm64 "), Some(ZellijTarget::X86_64WindowsMsvc));
        assert_eq!(target_from_windows_arch("x86"), None);
    }

    #[test]
    fn binary_names() {
        assert_eq!(binary_name(ZellijTarget::X86_64LinuxMusl), format!("zellij-{ZELLIJ_VERSION}"));
        assert_eq!(binary_name(ZellijTarget::X86_64WindowsMsvc), format!("zellij-{ZELLIJ_VERSION}.exe"));
    }
}
