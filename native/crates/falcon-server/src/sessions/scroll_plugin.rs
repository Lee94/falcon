//! 滚动位置插件（ADR 0019）在宿主机上的部署：插件本体 + zellij 的预授权。
//! 移植自 `packages/server/src/sessions/scrollPlugin.ts` 的纯函数部分。
//!
//! 插件是随服务打包的 .wasm（源码 native/zellij-plugin，产物提交在
//! native/crates/falcon-server/assets/），由后端推到宿主机——不让宿主机自己下载，内网主机常常
//! 出不了网（与 px0 同理）。宿主机上的路径固定（`HostLayout.scroll_plugin_file`），旁边
//! 一个 .sha256 记着内容，对得上就不再推。升级时原子替换文件即可：zellij 的插件缓存
//! 只在会话进程内存里，新会话读到新文件，老会话继续用内存里的旧实例（所以插件协议
//! 只能向后兼容地改）。
//!
//! 只在 POSIX 宿主机上做。Windows 远端没有测试机、推二进制也得另走 base64 行协议，
//! 先不支持——那边的会话照旧用 config.kdl，没有滚动条。
//!
//! 部署失败不挡开会话：返回 false，新会话退回老配置（没有滚动条）而已。
//!
//! 插件本体随二进制 `include_bytes!`（rust-unification.md 决定四）：不再像 Node 版那样从
//! `FALCON_ZELLIJ_PLUGIN` / 资产目录找文件——那个环境变量会从父实例漏进会话里，变成陈旧值。

use crate::zellij::command::{HostOs, scroll_permissions_entry};
use crate::zellij::host::quote_posix;
use crate::zellij::version::ZellijTarget;

/// 宿主机的系统，决定 permissions.kdl 在哪（见 `scroll_permissions_file`）
pub fn target_os(target: ZellijTarget) -> Option<HostOs> {
    let t = target.as_str();
    if t.ends_with("-apple-darwin") {
        return Some(HostOs::Darwin);
    }
    if t.contains("-linux-") {
        return Some(HostOs::Linux);
    }
    None
}

// ---------------- 命令构造（远端 POSIX） ----------------

/// 读宿主机上插件旁边的 .sha256；没有就是空输出
pub fn posix_plugin_digest_command(plugin_file: &str) -> String {
    format!("cat {} 2>/dev/null || true", quote_posix(&format!("{plugin_file}.sha256")))
}

/// 从 stdin 收插件本体：先落 .partial 再改名，中途断开不留半截文件；改名之后才写
/// .sha256——摘要文件在，就说明本体是完整的那一份。
pub fn posix_plugin_install_command(plugin_file: &str, sha256: &str) -> String {
    let dir = posix_dirname(plugin_file);
    format!(
        "d={}; f={}; mkdir -p \"$d\" && cat > \"$f.partial\" && mv -f \"$f.partial\" \"$f\" && \
         printf %s {} > \"$f.sha256\"",
        quote_posix(dir),
        quote_posix(plugin_file),
        quote_posix(sha256),
    )
}

/// 往 permissions.kdl 追加预授权条目，已有就不动。macOS 上这个文件与用户自己的 zellij
/// 共用，只能追加；条目前垫一个换行，防止原文件末尾没有换行时粘到上一行。
pub fn posix_permissions_command(perm_file: &str, plugin_file: &str) -> String {
    let entry = scroll_permissions_entry(plugin_file);
    // entry.slice(0, entry.indexOf("\n"))：第一行就是 `"<插件路径>" {`
    let key = entry.split('\n').next().unwrap_or_default();
    let dir = posix_dirname(perm_file);
    format!(
        "f={}; mkdir -p {} && {{ grep -qsF {} \"$f\" || printf '\\n%s' {} >> \"$f\"; }}",
        quote_posix(perm_file),
        quote_posix(dir),
        quote_posix(key),
        quote_posix(&entry),
    )
}

/// TS 的 `p.replace(/\/[^/]*$/, "") || "/"`：去掉最后一个 `/` 及其后的部分，剩空串就是根。
/// 没有 `/` 时原样返回（与 TS 一致；调用方给的都是绝对路径）。
fn posix_dirname(p: &str) -> &str {
    let dir = match p.rfind('/') {
        Some(i) => &p[..i],
        None => p,
    };
    if dir.is_empty() { "/" } else { dir }
}

// ---------------- 部署 ----------------

/// 插件本体与它的 sha256（宿主机上 `.sha256` 文件的内容，对得上就不再推）
pub struct ScrollPluginAsset {
    pub bytes: &'static [u8],
    pub sha256: String,
}

static ASSET: std::sync::LazyLock<ScrollPluginAsset> = std::sync::LazyLock::new(|| {
    let bytes: &'static [u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/falcon-scroll.wasm"));
    use sha2::Digest as _;
    ScrollPluginAsset { bytes, sha256: hex::encode(sha2::Sha256::digest(bytes)) }
});

pub fn scroll_plugin_asset() -> &'static ScrollPluginAsset {
    &ASSET
}

/// 远端：内容不对就经 stdin 推一份，再预写授权。任何一步失败都返回 false
pub async fn ensure_remote_scroll_plugin(
    link: &crate::sessions::ssh::SshLink,
    layout: &crate::zellij::host::HostLayout,
    target: ZellijTarget,
    home: &str,
) -> bool {
    use crate::exec::Exec as _;
    let asset = scroll_plugin_asset();
    let Some(host_os) = target_os(target) else { return false };
    let attempt = async {
        let digest = link.exec(&posix_plugin_digest_command(&layout.scroll_plugin_file), None).await?;
        if digest.stdout.trim() != asset.sha256 {
            let res = link
                .exec_with_input(&posix_plugin_install_command(&layout.scroll_plugin_file, &asset.sha256), asset.bytes)
                .await?;
            if res.code != Some(0) {
                return anyhow::Ok(false);
            }
        }
        let perm_file = crate::zellij::command::scroll_permissions_file(layout, host_os, home);
        let perm = link.exec(&posix_permissions_command(&perm_file, &layout.scroll_plugin_file), None).await?;
        anyhow::Ok(perm.code == Some(0))
    };
    attempt.await.unwrap_or(false)
}

/// 本机：同上，直接读写文件
pub fn ensure_local_scroll_plugin(layout: &crate::zellij::host::HostLayout, target: ZellijTarget, home: &str) -> bool {
    let asset = scroll_plugin_asset();
    let Some(host_os) = target_os(target) else { return false };
    let attempt = || -> std::io::Result<()> {
        let file = std::path::Path::new(&layout.scroll_plugin_file);
        let sha_file = format!("{}.sha256", layout.scroll_plugin_file);
        let current = std::fs::read_to_string(&sha_file).map(|s| s.trim().to_string()).unwrap_or_default();
        if current != asset.sha256 || !file.exists() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let partial = format!("{}.partial", layout.scroll_plugin_file);
            std::fs::write(&partial, asset.bytes)?;
            std::fs::rename(&partial, file)?;
            std::fs::write(&sha_file, &asset.sha256)?;
        }
        let perm_file = crate::zellij::command::scroll_permissions_file(layout, host_os, home);
        let entry = crate::zellij::command::scroll_permissions_entry(&layout.scroll_plugin_file);
        let key = entry.split('\n').next().unwrap_or(&entry);
        let existing = std::fs::read_to_string(&perm_file).unwrap_or_default();
        if !existing.contains(key) {
            if let Some(dir) = std::path::Path::new(&perm_file).parent() {
                std::fs::create_dir_all(dir)?;
            }
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&perm_file)?;
            write!(f, "\n{entry}")?;
        }
        Ok(())
    };
    attempt().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- targetOs ----

    /// 按 zellij target 分出 permissions.kdl 的位置口径
    #[test]
    fn target_os_splits_permissions_location_by_zellij_target() {
        assert_eq!(target_os(ZellijTarget::Aarch64AppleDarwin), Some(HostOs::Darwin));
        assert_eq!(target_os(ZellijTarget::X86_64LinuxMusl), Some(HostOs::Linux));
        assert_eq!(target_os(ZellijTarget::X86_64WindowsMsvc), None);
    }

    // ---- POSIX 部署命令 ----
    // 下面几条真跑一遍生成的 sh 命令：引号嵌套最容易错，看字符串看不出来

    #[cfg(unix)]
    mod posix {
        use super::*;
        use std::io::Write as _;
        use std::path::{Path, PathBuf};
        use std::process::{Command, Stdio};

        fn sh(cmd: &str, input: Option<&str>) -> String {
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            // execFileSync 不给 input 时 stdin 也是一条立刻 EOF 的管道
            let mut stdin = child.stdin.take().unwrap();
            if let Some(input) = input {
                stdin.write_all(input.as_bytes()).unwrap();
            }
            drop(stdin);
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "sh failed: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap()
        }

        fn setup() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::Builder::new().prefix("falcon-scroll-").tempdir().unwrap();
            let plugin = dir.path().join("with space").join("falcon-scroll.wasm");
            (dir, plugin)
        }

        fn s(p: &Path) -> &str {
            p.to_str().unwrap()
        }

        /// 没装过时摘要为空；装完摘要对得上、本体完整、不留 .partial
        #[test]
        fn digest_empty_before_install_then_matches_without_partial() {
            let (_dir, plugin) = setup();
            let plugin = s(&plugin);
            assert_eq!(sh(&posix_plugin_digest_command(plugin), None).trim(), "");
            sh(&posix_plugin_install_command(plugin, "abc123"), Some("wasm-bytes"));
            assert_eq!(std::fs::read_to_string(plugin).unwrap(), "wasm-bytes");
            assert_eq!(sh(&posix_plugin_digest_command(plugin), None).trim(), "abc123");
            assert!(!Path::new(&format!("{plugin}.partial")).exists());
        }

        /// 预授权只追加一次，并保留文件里原有的条目
        #[test]
        fn permissions_appended_once_keeping_existing_entries() {
            let (dir, plugin) = setup();
            let plugin = s(&plugin);
            let perm = dir.path().join("cache").join("zellij").join("permissions.kdl");
            std::fs::create_dir_all(perm.parent().unwrap()).unwrap();
            std::fs::write(&perm, "\"/other.wasm\" {\n    ReadCliPipes\n}").unwrap(); // 末尾故意没有换行
            sh(&posix_permissions_command(s(&perm), plugin), None);
            sh(&posix_permissions_command(s(&perm), plugin), None);
            let text = std::fs::read_to_string(&perm).unwrap();
            assert_eq!(text.matches(&format!("\"{plugin}\" {{")).count(), 1);
            assert!(text.starts_with("\"/other.wasm\" {\n    ReadCliPipes\n}\n"));
        }

        /// 文件不存在时连目录一起建
        #[test]
        fn permissions_creates_missing_dirs() {
            let (dir, plugin) = setup();
            let plugin = s(&plugin);
            let perm = dir.path().join("fresh").join("deep").join("permissions.kdl");
            sh(&posix_permissions_command(s(&perm), plugin), None);
            assert!(
                std::fs::read_to_string(&perm)
                    .unwrap()
                    .contains(&format!("\"{plugin}\" {{\n    ReadApplicationState\n"))
            );
        }
    }

    // ---- 以下不在 scrollPlugin.test.ts 里：逐字节对照 TS 产出 ----

    #[test]
    fn extra_commands_match_ts_byte_for_byte() {
        let p = "/home/u/.falcon/zellij/plugins/falcon-scroll.wasm";
        assert_eq!(
            posix_plugin_digest_command(p),
            "cat '/home/u/.falcon/zellij/plugins/falcon-scroll.wasm.sha256' 2>/dev/null || true"
        );
        assert_eq!(
            posix_plugin_install_command(p, "ab"),
            "d='/home/u/.falcon/zellij/plugins'; f='/home/u/.falcon/zellij/plugins/falcon-scroll.wasm'; \
             mkdir -p \"$d\" && cat > \"$f.partial\" && mv -f \"$f.partial\" \"$f\" && printf %s 'ab' > \"$f.sha256\""
        );
        assert_eq!(
            posix_permissions_command("/c/permissions.kdl", "/p.wasm"),
            "f='/c/permissions.kdl'; mkdir -p '/c' && { grep -qsF '\"/p.wasm\" {' \"$f\" || \
             printf '\\n%s' '\"/p.wasm\" {\n    ReadApplicationState\n    ChangeApplicationState\n    \
             ReadCliPipes\n    ReadPaneContents\n}\n' >> \"$f\"; }"
        );
        assert_eq!(posix_dirname("/x.wasm"), "/");
        assert_eq!(posix_dirname("x.wasm"), "x.wasm");
    }
}
