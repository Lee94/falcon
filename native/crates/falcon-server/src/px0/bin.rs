//! 移植自 `packages/server/src/px0/bin.ts`。TS 没有它的单测；文件末的用例是 Rust 侧补的
//! （下载换成注入的假实现，不碰网络）。
//!
//! px0 二进制的下载与校验（ADR 0017 决定四）。
//!
//! 所有平台的资产都由后端本机下载，落在 `<dataDir>/bin/<资产名>`：本地项目直接跑它，
//! SSH 项目再经 stdin 推到远端。不让远端自己下载——内网主机常常出不了网。
//!
//! 每个资产的 sha256 钉死在 command.rs（[`px0_sha256`]），下载后与已在的文件都要对上
//! 才用。后端所在机器下不了 GitHub 时，用户自己把同名文件放进去也行（照样校验）。
//!
//! 本机可用 FALCON_PX0_BIN 指定自己的 px0，不校验版本——用户有意覆盖。
//!
//! # 与 TS 的差别
//!
//! - TS 函数的 `env = process.env` 参数在这里是显式的 `env_bin: Option<&str>`
//!   （FALCON_PX0_BIN 的值，调用方读环境）；下载器是注入的 [`DownloadFn`]（生产上是
//!   [`http_download_fn`]，fetch 的等价物），测试换成假的。
//! - 并发合并的键是**目标路径**（数据目录 + 资产名），TS 只按资产名：同一进程里开两个
//!   数据目录（测试会）时，按资产名合并会把一个目录的结果交给另一个。
//! - 整个下载 / 校验是 `Send` 的，进程内共用一份在飞表；px0 实例管理跑在 LocalSet 上，
//!   照样能等它。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use sha2::{Digest, Sha256};

use super::command::{Px0Target, local_px0_target, px0_asset_name, px0_download_url, px0_sha256};

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// 本机指定自己的 px0（不校验版本）
pub const PX0_BIN_ENV: &str = "FALCON_PX0_BIN";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Px0BinError(pub String);

/// 下载一个 URL 的全部字节。失败的文案由实现给（要能直接给用户看）
pub type DownloadFn = Arc<dyn Fn(String) -> BoxFuture<'static, Result<Vec<u8>, Px0BinError>> + Send + Sync>;

type Ensure = Shared<BoxFuture<'static, Result<PathBuf, Px0BinError>>>;

/// 同一资产的并发请求合并成一次下载；失败后清掉，下次再试
static INFLIGHT: LazyLock<Mutex<HashMap<PathBuf, Ensure>>> = LazyLock::new(Mutex::default);
/// 本进程里已经校验过的文件：12 MB 算一次 sha256 虽然只要几十毫秒，也不必每次打开都算
static VERIFIED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn asset_path(data_dir: &Path, asset: &str) -> PathBuf {
    data_dir.join("bin").join(asset)
}

/// 本机已经有这个资产的文件（还没校验）。只用来决定入口页上写「下载」还是「启动」。
/// `env_bin` 是 FALCON_PX0_BIN（远端目标传 None：本机的覆盖不算远端的资产）
pub fn px0_asset_present(data_dir: &Path, target: Px0Target, env_bin: Option<&str>) -> bool {
    asset_path(data_dir, &px0_asset_name(target, None)).exists() || env_bin.is_some_and(|b| !b.is_empty())
}

/// 拿到某个平台资产在本机的路径（已校验）。没有就下载。
pub async fn ensure_px0_asset(data_dir: &Path, target: Px0Target, download: &DownloadFn) -> Result<PathBuf, Px0BinError> {
    let asset = px0_asset_name(target, None);
    let Some(expected) = px0_sha256(&asset) else {
        return Err(Px0BinError(format!("px0 没有 {asset} 这个资产")));
    };
    ensure_merged(data_dir, &asset, expected, download).await
}

/// 本机跑的 px0：FALCON_PX0_BIN（`env_bin`）优先，否则本平台的锁定资产
pub async fn ensure_local_px0(data_dir: &Path, env_bin: Option<&str>, download: &DownloadFn) -> Result<PathBuf, Px0BinError> {
    if let Some(bin) = env_bin.filter(|b| !b.is_empty()) {
        if !Path::new(bin).exists() {
            return Err(Px0BinError(format!("FALCON_PX0_BIN 指向的文件不存在：{bin}")));
        }
        return Ok(PathBuf::from(bin));
    }
    let Some(target) = local_px0_target(None, None) else {
        let platform = crate::sessions::login_env::node_platform();
        return Err(Px0BinError(format!("px0 没有 {platform}/{} 的构建", std::env::consts::ARCH)));
    };
    ensure_px0_asset(data_dir, target, download).await
}

/// 同一目标的并发请求并成一次 [`ensure_now`]
async fn ensure_merged(data_dir: &Path, asset: &str, expected: &str, download: &DownloadFn) -> Result<PathBuf, Px0BinError> {
    let dest = asset_path(data_dir, asset);
    let running = {
        let mut inflight = lock(&INFLIGHT);
        match inflight.get(&dest) {
            Some(running) => running.clone(),
            None => {
                let (data_dir, asset, expected, download) =
                    (data_dir.to_path_buf(), asset.to_string(), expected.to_string(), download.clone());
                let key = dest.clone();
                let run: Ensure = async move {
                    let result = ensure_now(&data_dir, &asset, &expected, &download).await;
                    // finally：清掉自己
                    lock(&INFLIGHT).remove(&key);
                    result
                }
                .boxed()
                .shared();
                inflight.insert(dest, run.clone());
                run
            }
        }
    };
    running.await
}

async fn ensure_now(data_dir: &Path, asset: &str, expected: &str, download: &DownloadFn) -> Result<PathBuf, Px0BinError> {
    let dest = asset_path(data_dir, asset);
    if lock(&VERIFIED).contains(&dest) && dest.exists() {
        return Ok(dest);
    }
    if dest.exists() && sha256_file(&dest).await.as_deref() == Some(expected) {
        ensure_executable(&dest)?;
        lock(&VERIFIED).insert(dest.clone());
        return Ok(dest);
    }

    let io = |what: &str, e: std::io::Error| Px0BinError(format!("{what}失败：{e}（{}）", dest.display()));
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| io("建 px0 目录", e))?;
    }
    let partial = PathBuf::from(format!("{}.partial", dest.display()));
    let result = async {
        let buf = download(px0_download_url(asset, None, None)).await?;
        let got = hex::encode(Sha256::digest(&buf));
        if got != expected {
            return Err(Px0BinError(format!("px0 下载内容的 sha256 对不上（{asset}）：得到 {got}")));
        }
        write_executable(&partial, &buf).await.map_err(|e| io("写 px0", e))?;
        tokio::fs::rename(&partial, &dest).await.map_err(|e| io("放置 px0", e))
    }
    .await;
    // finally：不留半截文件（改名成功后它已经不在了）
    let _ = tokio::fs::remove_file(&partial).await;
    result?;
    ensure_executable(&dest)?;
    lock(&VERIFIED).insert(dest.clone());
    Ok(dest)
}

/// 写文件并给 0755（TS 的 `writeFileSync(partial, buf, { mode: 0o755 })`）
async fn write_executable(path: &Path, buf: &[u8]) -> std::io::Result<()> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o755);
    use tokio::io::AsyncWriteExt as _;
    let mut file = opts.open(path).await?;
    file.write_all(buf).await?;
    file.flush().await
}

fn ensure_executable(file: &Path) -> Result<(), Px0BinError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Px0BinError(format!("给 px0 加执行权限失败：{e}（{}）", file.display())))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// 文件的 sha256（hex）。读不了按"对不上"处理、去重新下载（TS 是把读错误直接抛出去）：
/// 下载后改名会覆盖掉它，真有权限问题时写 / 改名那一步会报出原因
async fn sha256_file(file: &Path) -> Option<String> {
    let file = file.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut hash = Sha256::new();
        let mut f = std::fs::File::open(file).ok()?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        Some(hex::encode(hash.finalize()))
    })
    .await
    .ok()
    .flatten()
}

/// 生产上的下载器：GitHub release，跟随跳转（资产实际在 objects.githubusercontent.com），
/// 180s 超时。TLS 同 falcon-client：rustls（ring）+ 系统信任库
pub fn http_download_fn() -> DownloadFn {
    Arc::new(|url: String| Box::pin(download(url)))
}

async fn download(url: String) -> Result<Vec<u8>, Px0BinError> {
    let client = http_client().map_err(|e| Px0BinError(format!("下载 px0 失败：{e}（{url}）")))?;
    let res = client.get(&url).send().await.map_err(|e| Px0BinError(format!("下载 px0 失败：{}（{url}）", why(&e))))?;
    if !res.status().is_success() {
        return Err(Px0BinError(format!("下载 px0 失败：HTTP {}（{url}）", res.status().as_u16())));
    }
    let buf = res.bytes().await.map_err(|e| Px0BinError(format!("下载 px0 失败：{}（{url}）", why(&e))))?;
    if buf.is_empty() {
        return Err(Px0BinError("下载 px0 失败：空响应".into()));
    }
    Ok(buf.to_vec())
}

/// reqwest 的错误：超时报"下载超时"（TS 里 AbortController 触发时的文案），其余带上原因链
fn why(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "下载超时".into();
    }
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        msg.push_str(&format!("：{s}"));
        source = s.source();
    }
    msg
}

fn http_client() -> Result<reqwest::Client, String> {
    static CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
        use rustls_platform_verifier::BuilderVerifierExt as _;
        // 显式指定 ring：rustls 的进程级默认 provider 没人装过时 builder() 会 panic
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_platform_verifier())
            .map_err(|e| format!("TLS 初始化失败：{e}"))?
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .timeout(DOWNLOAD_TIMEOUT)
            .user_agent("falcon")
            .build()
            .map_err(|e| format!("建 HTTP 客户端失败：{e}"))
    });
    CLIENT.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::px0::command::{Px0Arch, Px0Os};

    fn fake_download(bytes: Result<Vec<u8>, &'static str>, calls: Arc<AtomicUsize>) -> DownloadFn {
        Arc::new(move |_url| {
            calls.fetch_add(1, Ordering::SeqCst);
            let bytes = bytes.clone();
            Box::pin(async move {
                // 让并发的调用方有机会并进来
                tokio::time::sleep(Duration::from_millis(50)).await;
                bytes.map_err(|e| Px0BinError(e.to_string()))
            })
        })
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn asset_path_and_presence() {
        let dir = tempfile::tempdir().unwrap();
        let target = Px0Target { os: Px0Os::Linux, arch: Px0Arch::Amd64 };
        assert_eq!(asset_path(dir.path(), "px0-0.1.16-linux-amd64"), dir.path().join("bin/px0-0.1.16-linux-amd64"));
        assert!(!px0_asset_present(dir.path(), target, None));
        // 本机的覆盖算"有"（入口页写「启动」而不是「下载」）
        assert!(px0_asset_present(dir.path(), target, Some("/x/px0")));
        assert!(!px0_asset_present(dir.path(), target, Some("")));
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(asset_path(dir.path(), "px0-0.1.16-linux-amd64"), b"x").unwrap();
        assert!(px0_asset_present(dir.path(), target, None));
    }

    #[tokio::test]
    async fn downloads_verifies_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"px0 binary".to_vec();
        let calls = Arc::new(AtomicUsize::new(0));
        let dl = fake_download(Ok(body.clone()), calls.clone());
        let expected = sha(&body);
        // 并发的两次并成一次下载
        let (a, b) = tokio::join!(
            ensure_merged(dir.path(), "asset-a", &expected, &dl),
            ensure_merged(dir.path(), "asset-a", &expected, &dl)
        );
        let path = a.unwrap();
        assert_eq!(b.unwrap(), path);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(!PathBuf::from(format!("{}.partial", path.display())).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        }
        // 已校验过：不再下
        ensure_merged(dir.path(), "asset-a", &expected, &dl).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_existing_file_with_the_right_hash_is_used_and_a_wrong_one_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"the real px0".to_vec();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        // 用户自己放进去的、哈希对得上的文件：直接用
        std::fs::write(asset_path(dir.path(), "asset-ok"), &body).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let dl = fake_download(Ok(body.clone()), calls.clone());
        ensure_now(dir.path(), "asset-ok", &sha(&body), &dl).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // 哈希对不上的旧文件：重新下载换掉
        std::fs::write(asset_path(dir.path(), "asset-stale"), b"tampered").unwrap();
        let path = ensure_now(dir.path(), "asset-stale", &sha(&body), &dl).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(path).unwrap(), body);
    }

    #[tokio::test]
    async fn a_mismatched_download_is_rejected_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let dl = fake_download(Ok(b"evil".to_vec()), calls.clone());
        let err = ensure_merged(dir.path(), "asset-x", &sha(b"good"), &dl).await.unwrap_err();
        assert_eq!(err.0, format!("px0 下载内容的 sha256 对不上（asset-x）：得到 {}", sha(b"evil")));
        assert!(!asset_path(dir.path(), "asset-x").exists());
        assert!(!dir.path().join("bin/asset-x.partial").exists());
        // 失败不留在飞表里：下次再试
        ensure_merged(dir.path(), "asset-x", &sha(b"good"), &dl).await.unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // 下载器自己的错误原样往上报
        let dl = fake_download(Err("下载 px0 失败：HTTP 404（u）"), Arc::default());
        let err = ensure_merged(dir.path(), "asset-y", &sha(b"good"), &dl).await.unwrap_err();
        assert_eq!(err.0, "下载 px0 失败：HTTP 404（u）");
    }

    #[tokio::test]
    async fn local_override_and_unknown_assets() {
        let dir = tempfile::tempdir().unwrap();
        let dl = fake_download(Err("不该下载"), Arc::default());
        let err = ensure_local_px0(dir.path(), Some("/nonexistent/falcon-test/px0"), &dl).await.unwrap_err();
        assert_eq!(err.0, "FALCON_PX0_BIN 指向的文件不存在：/nonexistent/falcon-test/px0");
        let mine = dir.path().join("my-px0");
        std::fs::write(&mine, b"").unwrap();
        assert_eq!(ensure_local_px0(dir.path(), Some(mine.to_str().unwrap()), &dl).await.unwrap(), mine);
        // 钉死的哈希表里每个支持的平台都有
        for os in [Px0Os::Linux, Px0Os::Darwin, Px0Os::Windows] {
            for arch in [Px0Arch::Amd64, Px0Arch::Arm64] {
                assert!(px0_sha256(&px0_asset_name(Px0Target { os, arch }, None)).is_some());
            }
        }
        // 锁定版本的资产在表里、本地没有：走下载（这里假下载器报错）
        let err = ensure_px0_asset(dir.path(), Px0Target { os: Px0Os::Linux, arch: Px0Arch::Amd64 }, &dl).await;
        assert_eq!(err.unwrap_err().0, "不该下载");
    }
}
