//! cloudflared 可执行文件的定位与按需下载。移植自 `packages/server/src/cloudflared/bin.ts`
//! （含 `bin.test.ts`）。
//!
//! 只在 falcon 后端本机跑（见 ADR 0014），所以二进制也只落在本机
//! `<dataDir>/bin/cloudflared`，不往远端宿主机装。
//!
//! 解析顺序：
//! 1. FALCON_CLOUDFLARED_BIN —— 显式指定，不校验版本（用户有意覆盖）；
//! 2. `<dataDir>/bin/cloudflared` 且 `--version` 对得上锁定版本；
//! 3. 从 GitHub release 下载锁定版本（darwin 是 tgz，linux 是裸二进制）；
//! 4. PATH 上的 `cloudflared` —— 下载失败时的兜底，版本可能对不上。
//!
//! 不做完整性校验：走默认 GitHub 源时 HTTPS 已保证来源。Zellij 同一套权衡。
//!
//! # 与 TS 的差别
//!
//! - 下载交给本机的 curl（没有再试 wget），不是进程内的 HTTP 客户端：Rust 服务端没有带 TLS 的
//!   HTTP 客户端，不值得为一个一次性的 20MB 下载拖进一整套 TLS 栈。本地 Zellij 安装也是这么下的
//!   （`sessions/local.rs` 的 `local_downloader`）。macOS / 主流 Linux 发行版 / Windows 10 1803+
//!   都自带 curl；都没有时照 TS 的规矩退到 PATH 上的 cloudflared。
//! - 环境变量由调用方传进来（TS 的 `env` 参数缺省是 `process.env`）：Rust 2024 里改进程环境是
//!   unsafe，单测不该去碰它。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::Shared;

use super::command::{CLOUDFLARED_VERSION, CloudflaredAssetKind, cloudflared_asset, download_url, parse_cloudflared_version};
use crate::exec::{LocalBoxFuture, local_exec};

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const VERSION_TIMEOUT: Duration = Duration::from_secs(8);
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);

/// `platform` 是 Node 的叫法（`win32` / `darwin` / `linux`），缺省为本进程
pub fn bundled_bin_name(platform: Option<&str>) -> &'static str {
    let windows = match platform {
        Some(p) => p == "win32",
        None => cfg!(windows),
    };
    if windows { "cloudflared.exe" } else { "cloudflared" }
}

pub fn install_path(data_dir: &Path, platform: Option<&str>) -> PathBuf {
    data_dir.join("bin").join(bundled_bin_name(platform))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudflaredBinFailure {
    ArchUnsupported,
    DownloadFailed,
    ExtractFailed,
    VerifyFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CloudflaredBinError {
    pub reason: CloudflaredBinFailure,
    pub message: String,
}

impl CloudflaredBinError {
    fn new(reason: CloudflaredBinFailure, message: impl Into<String>) -> Self {
        CloudflaredBinError { reason, message: message.into() }
    }
}

type Inflight = Shared<LocalBoxFuture<'static, Result<String, CloudflaredBinError>>>;

thread_local! {
    /// 在途的那一次（TS 模块级的 `inflight`）。管理器跑在单线程的 LocalSet 上，一个线程一份就是一份
    static INFLIGHT: RefCell<Option<Inflight>> = const { RefCell::new(None) };
}

/// 拿到一个能跑的 cloudflared 路径。并发调用合并成一次下载。
/// 失败后清掉 inflight，下次再试。
///
/// `env_bin` 是 FALCON_CLOUDFLARED_BIN 的值（空串当没设，同 TS 的真值判断）。
pub async fn ensure_cloudflared(data_dir: &Path, env_bin: Option<&str>) -> Result<String, CloudflaredBinError> {
    let fut = INFLIGHT.with(|cell| {
        if let Some(f) = cell.borrow().clone() {
            return f;
        }
        let data_dir = data_dir.to_path_buf();
        let env_bin = env_bin.filter(|s| !s.is_empty()).map(str::to_string);
        let f: LocalBoxFuture<'static, _> = Box::pin(async move {
            let r = ensure_now(&data_dir, env_bin.as_deref()).await;
            INFLIGHT.with(|c| c.borrow_mut().take());
            r
        });
        let shared = f.shared();
        *cell.borrow_mut() = Some(shared.clone());
        shared
    });
    fut.await
}

async fn ensure_now(data_dir: &Path, env_bin: Option<&str>) -> Result<String, CloudflaredBinError> {
    if let Some(p) = env_bin {
        if !Path::new(p).exists() {
            return Err(CloudflaredBinError::new(
                CloudflaredBinFailure::VerifyFailed,
                format!("FALCON_CLOUDFLARED_BIN 指向的文件不存在：{p}"),
            ));
        }
        return Ok(p.to_string());
    }

    let dest = install_path(data_dir, None);
    if dest.exists() && version_of(&dest).await.as_deref() == Some(CLOUDFLARED_VERSION) {
        return Ok(dest.to_string_lossy().into_owned());
    }

    match download_locked(&dest).await {
        Ok(()) => Ok(dest.to_string_lossy().into_owned()),
        Err(err) => match which_cloudflared().await {
            Some(path_bin) => {
                log::warn!("cloudflared 下载失败（{}），退到 PATH 上的 {path_bin}", err.message);
                Ok(path_bin)
            }
            None => Err(err),
        },
    }
}

/// 下载用的临时目录，drop 时连同内容删掉（TS 的 `finally { fs.rmSync(tmpRoot, …) }`）
struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("falcon-cloudflared-{}", &crate::askpass::hub::uuid_v4()[..8]));
        std::fs::create_dir_all(&dir)?;
        Ok(TmpDir(dir))
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn download_locked(dest: &Path) -> Result<(), CloudflaredBinError> {
    use CloudflaredBinFailure::*;
    let Some(asset) = cloudflared_asset(None, None) else {
        return Err(CloudflaredBinError::new(ArchUnsupported, "该架构没有官方 cloudflared 构建"));
    };
    let url = download_url(&asset, None, None);
    let tmp = TmpDir::new()
        .map_err(|e| CloudflaredBinError::new(DownloadFailed, format!("下载 cloudflared 失败：建临时目录失败：{e}")))?;
    let download_to = tmp.0.join(asset.name);
    fetch_to_file(&url, &download_to).await?;

    let mut binary = download_to.clone();
    if asset.kind == CloudflaredAssetKind::Tgz {
        if let Err(msg) = extract_tgz(&download_to, &tmp.0).await {
            return Err(CloudflaredBinError::new(ExtractFailed, format!("解压 cloudflared 失败：{msg}")));
        }
        binary = find_extracted_binary(&tmp.0)
            .ok_or_else(|| CloudflaredBinError::new(ExtractFailed, "压缩包里没有 cloudflared 二进制"))?;
    }

    let io_err = |what: &str, e: std::io::Error| CloudflaredBinError::new(VerifyFailed, format!("{what}：{e}"));
    make_executable(&binary).map_err(|e| io_err("给 cloudflared 加执行权限失败", e))?;
    let ver = version_of(&binary).await;
    if ver.as_deref() != Some(CLOUDFLARED_VERSION) {
        return Err(CloudflaredBinError::new(
            VerifyFailed,
            match ver {
                Some(v) => format!("cloudflared 版本是 {v}，期望 {CLOUDFLARED_VERSION}"),
                None => "cloudflared 无法执行（可能是 noexec 挂载或架构不兼容）".into(),
            },
        ));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err("建 bin 目录失败", e))?;
    }
    let mut partial = dest.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    std::fs::copy(&binary, &partial).map_err(|e| io_err("安装 cloudflared 失败", e))?;
    make_executable(&partial).map_err(|e| io_err("给 cloudflared 加执行权限失败", e))?;
    std::fs::rename(&partial, dest).map_err(|e| io_err("安装 cloudflared 失败", e))?;
    Ok(())
}

fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

async fn extract_tgz(archive: &Path, into: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("tar");
    cmd.arg("-xzf").arg(archive).arg("-C").arg(into);
    let out = run(cmd, EXTRACT_TIMEOUT).await?;
    if out.status.success() { Ok(()) } else { Err(failure_detail(&out)) }
}

pub fn find_extracted_binary(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name();
        let is_file = e.file_type().is_ok_and(|t| t.is_file());
        if is_file && (name == "cloudflared" || name == "cloudflared.exe") {
            return Some(dir.join(name));
        }
    }
    None
}

/// 下载到 `dest`。见模块注释「与 TS 的差别」：交给本机 curl，总时长同 TS 的 120s。
async fn fetch_to_file(url: &str, dest: &Path) -> Result<(), CloudflaredBinError> {
    let fail = |msg: String| CloudflaredBinError::new(CloudflaredBinFailure::DownloadFailed, msg);
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut cmd = tokio::process::Command::new(curl);
    // -f：HTTP 错误码当失败（TS 的 `!res.ok`）；-L：跟随 GitHub 到 objects 存储的跳转
    cmd.args(["-fsSL", "--connect-timeout", "20", "--max-time", "120", "-A", "falcon", "-o"]).arg(dest).arg(url);
    let out = match run(cmd, DOWNLOAD_TIMEOUT + Duration::from_secs(5)).await {
        Ok(out) => out,
        Err(msg) => return Err(fail(format!("下载 cloudflared 失败：{msg}"))),
    };
    if !out.status.success() {
        // curl 28 = 超时；22 = HTTP 错误码，照 TS 写成「HTTP 404」
        let detail = match out.status.code() {
            Some(28) => "下载超时".to_string(),
            Some(22) => http_status_of(&String::from_utf8_lossy(&out.stderr))
                .map(|s| format!("HTTP {s}"))
                .unwrap_or_else(|| failure_detail(&out)),
            _ => failure_detail(&out),
        };
        return Err(fail(format!("下载 cloudflared 失败：{detail}")));
    }
    if std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(fail("下载 cloudflared 失败: 空响应".into()));
    }
    Ok(())
}

/// curl -f 的 `curl: (22) The requested URL returned error: 404` 里的状态码
fn http_status_of(stderr: &str) -> Option<&str> {
    let rest = &stderr[stderr.find("returned error: ")? + "returned error: ".len()..];
    let code = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    (!code.is_empty()).then_some(code)
}

/// 跑一条命令收输出，超时就杀（kill_on_drop）。起不来（没装）是 Err
async fn run(mut cmd: tokio::process::Command, limit: Duration) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    hide_window(&mut cmd);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("超时".into()),
    }
}

fn failure_detail(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr.lines().map(str::trim).rfind(|l| !l.is_empty());
    match (line, out.status.code()) {
        (Some(l), _) => l.to_string(),
        (None, Some(code)) => format!("退出码 {code}"),
        (None, None) => "被信号终止".into(),
    }
}

/// windowsHide：别闪出一个黑色控制台窗口
pub(crate) fn hide_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(not(windows))]
    let _ = cmd;
}

/// `<bin> --version` 报的版本；跑不起来 / 8s 没回 / 认不出都是 None
pub async fn version_of(bin: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version");
    let out = run(cmd, VERSION_TIMEOUT).await.ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    parse_cloudflared_version(&text)
}

async fn which_cloudflared() -> Option<String> {
    // `command` 是 shell 内置，不能直接 spawn；跟 localExec 一样走 shell。
    let cmd = if cfg!(windows) { "where cloudflared" } else { "command -v cloudflared" };
    let res = local_exec(cmd, None).await;
    if !res.ok() {
        return None;
    }
    let line = res.stdout.lines().map(str::trim).find(|l| !l.is_empty());
    Some(line.unwrap_or("cloudflared").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // describe("installPath")

    #[test]
    fn puts_the_binary_under_data_dir_bin_with_exe_on_windows() {
        assert_eq!(install_path(Path::new("/data"), Some("darwin")), Path::new("/data").join("bin").join("cloudflared"));
        assert_eq!(bundled_bin_name(Some("win32")), "cloudflared.exe");
        assert_eq!(bundled_bin_name(Some("linux")), "cloudflared");
    }

    // 以下不在 TS 测试里

    #[test]
    fn http_status_is_read_from_curl_stderr() {
        assert_eq!(http_status_of("curl: (22) The requested URL returned error: 404\n"), Some("404"));
        assert_eq!(http_status_of("curl: (6) Could not resolve host"), None);
    }

    #[test]
    fn finds_the_extracted_binary_by_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_extracted_binary(dir.path()), None);
        std::fs::create_dir(dir.path().join("cloudflared.d")).unwrap();
        std::fs::write(dir.path().join("README"), "x").unwrap();
        assert_eq!(find_extracted_binary(dir.path()), None);
        std::fs::write(dir.path().join("cloudflared"), "x").unwrap();
        assert_eq!(find_extracted_binary(dir.path()), Some(dir.path().join("cloudflared")));
    }

    #[cfg(unix)]
    fn fake_bin(dir: &Path, name: &str, version: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho 'cloudflared version {version} (built 2026-10-01-1200 UTC)'\n"))
            .unwrap();
        make_executable(&path).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn version_of_parses_the_banner_and_tolerates_junk() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(dir.path(), "cf", "2026.10.0");
        assert_eq!(version_of(&bin).await.as_deref(), Some("2026.10.0"));
        assert_eq!(version_of(&dir.path().join("missing")).await, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn env_override_wins_without_a_version_check() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let bin = fake_bin(dir.path(), "custom", "1999.1.1");
                let got = ensure_cloudflared(dir.path(), Some(bin.to_str().unwrap())).await;
                assert_eq!(got.as_deref(), Ok(bin.to_str().unwrap()));
                let missing = dir.path().join("nope");
                let err = ensure_cloudflared(dir.path(), Some(missing.to_str().unwrap())).await.unwrap_err();
                assert_eq!(err.reason, CloudflaredBinFailure::VerifyFailed);
                assert_eq!(err.message, format!("FALCON_CLOUDFLARED_BIN 指向的文件不存在：{}", missing.display()));
            })
            .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installed_binary_with_the_locked_version_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        fake_bin(&dir.path().join("bin"), "cloudflared", CLOUDFLARED_VERSION);
        // 版本对得上就不下载（这个用例在没网的机器上也必须过）
        let got = ensure_cloudflared(dir.path(), None).await.unwrap();
        assert_eq!(got, install_path(dir.path(), None).to_string_lossy());
    }
}
