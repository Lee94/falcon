//! 各平台的守护进程二进制从哪来（设计文档 10.5）：
//!
//! 1. `FALCON_SESSIOND_DIR`：一个目录，里面按目标三元组分子目录放 `falcon-sessiond[.exe]`
//!    （即 `native/target-sessiond` 的布局：`<目录>/<三元组>/release/falcon-sessiond`），
//!    或直接是 `<目录>/<三元组>/falcon-sessiond`；
//! 2. 编进发布版的那份（`embed-sessiond` feature，4 个目标各一份 gzip，由 `main.rs` 启动时
//!    经 [`register_embedded`] 登记）；
//! 3. 开发构建：`native/target-sessiond/<三元组>/release/falcon-sessiond`（`cargo xtask sessiond` 的产物）；
//! 4. 都没有：这个构建里没有那个目标的守护进程，会话标非持久（`sessiond-unavailable`）。
//!
//! Mac 上的后端也会连 Linux arm64 的远端，所以发布版把 4 个目标全编进去。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::Digest as _;

use super::command::{SessiondTarget, binary_name};

pub const SESSIOND_DIR_ENV: &str = "FALCON_SESSIOND_DIR";

/// 一个目标的守护进程：字节 + 按内容起的文件名
#[derive(Clone)]
pub struct SessiondBinary {
    pub target: SessiondTarget,
    pub bytes: Arc<Vec<u8>>,
    /// sha256 的前 8 位 hex
    pub hash8: String,
}

impl SessiondBinary {
    pub fn file_name(&self) -> String {
        binary_name(self.target, &self.hash8)
    }
}

impl std::fmt::Debug for SessiondBinary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessiondBinary({}, {} 字节, {})", self.target.triple(), self.bytes.len(), self.hash8)
    }
}

fn exe_name(target: SessiondTarget) -> &'static str {
    if target.is_windows() { "falcon-sessiond.exe" } else { "falcon-sessiond" }
}

/// 开发构建的产物目录（`native/target-sessiond`）
fn dev_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-sessiond")
}

fn candidates(dir: &Path, target: SessiondTarget) -> [PathBuf; 2] {
    let t = target.triple();
    [dir.join(t).join("release").join(exe_name(target)), dir.join(t).join(exe_name(target))]
}

/// 进程内缓存：字节读一次、哈希算一次
static CACHE: Mutex<Vec<SessiondBinary>> = Mutex::new(Vec::new());

/// 某个目标的守护进程；这个构建里没有就是 None
pub fn binary_for(target: SessiondTarget) -> Option<SessiondBinary> {
    if let Some(b) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|b| b.target == target) {
        return Some(b.clone());
    }
    let bytes = load(target)?;
    let hash8 = hex::encode(sha2::Sha256::digest(&bytes))[..8].to_string();
    let b = SessiondBinary { target, bytes: Arc::new(bytes), hash8 };
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).push(b.clone());
    Some(b)
}

fn load(target: SessiondTarget) -> Option<Vec<u8>> {
    if let Some(dir) = std::env::var_os(SESSIOND_DIR_ENV).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        for p in candidates(&dir, target) {
            if let Ok(b) = std::fs::read(&p) {
                return Some(b);
            }
        }
        log::warn!("{SESSIOND_DIR_ENV} 里没有 {} 的守护进程（{}）", target.triple(), dir.display());
    }
    if let Some(b) = embedded::bytes(target) {
        return Some(b);
    }
    for p in candidates(&dev_dir(), target) {
        if let Ok(b) = std::fs::read(&p) {
            return Some(b);
        }
    }
    None
}

/// 编进二进制的守护进程：给出某个目标的 gzip（`include_bytes!` 的结果）
#[cfg(feature = "embed-sessiond")]
pub type EmbeddedArchive = fn(SessiondTarget) -> &'static [u8];

#[cfg(feature = "embed-sessiond")]
static EMBEDDED: std::sync::OnceLock<EmbeddedArchive> = std::sync::OnceLock::new();

/// 发布构建的可执行入口（`main.rs`）在启动最早处登记内嵌的守护进程；重复登记保留第一次的。
///
/// `include_bytes!` 刻意写在 `main.rs` 而不是库里：CI 先用占位文件把整个工作区编完（与守护进程的
/// 构建并行），守护进程产物到了再换上真文件重编。include 在库里时换文件要把 6 万行的库整个重编
/// （macOS / Windows runner 上 3–4 分钟，正好压在发版关键路径上）；放在 bin target 里只重编
/// `main.rs` 再链接。`cargo xtask server` 出包前核对产物里确实原样带着这 4 份 gzip。
#[cfg(feature = "embed-sessiond")]
pub fn register_embedded(gz: EmbeddedArchive) {
    let _ = EMBEDDED.set(gz);
}

/// 编进二进制的守护进程（发布构建）。构建时 `FALCON_EMBED_SESSIOND_DIR` 指向放
/// `<三元组>.gz` 的目录（`cargo xtask server` 准备好），字节由 `main.rs` 登记
mod embedded {
    use super::SessiondTarget;

    pub fn bytes(target: SessiondTarget) -> Option<Vec<u8>> {
        #[cfg(feature = "embed-sessiond")]
        {
            use std::io::Read as _;
            // 库的单元测试没有 main.rs，开了特性也没人登记，照常落到开发构建的目录
            let Some(gz) = super::EMBEDDED.get() else {
                if !cfg!(test) {
                    log::warn!("发布构建没有登记内嵌的守护进程（main.rs 没调 register_embedded？）");
                }
                return None;
            };
            let mut out = Vec::new();
            return match flate2::read::GzDecoder::new(gz(target)).read_to_end(&mut out) {
                Ok(_) => Some(out),
                Err(e) => {
                    log::warn!("解不开内置的守护进程（{}）：{e}", target.triple());
                    None
                }
            };
        }
        #[cfg(not(feature = "embed-sessiond"))]
        {
            let _ = target;
            None
        }
    }
}

/// 本机：把二进制落到 `<dataDir>/bin/<按内容起的名字>`，返回路径。已在（大小对得上）就不写。
///
/// 本机也拷一份而不是直接跑 `target-sessiond` 里的：开发时重编会原地覆盖那个文件，
/// 正在跑的 holder 拉 VT 进程时就找不到自己的二进制了（见 command::binary_name）
pub fn install_local(data_dir: &Path, bin: &SessiondBinary) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("bin");
    let dest = dir.join(bin.file_name());
    if dest.metadata().is_ok_and(|m| m.len() == bin.bytes.len() as u64) {
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir)?;
    // 先写临时文件再改名：两个进程同时释放也不会看到半截文件
    let tmp = dir.join(format!(".{}.tmp-{}", bin.file_name(), std::process::id()));
    std::fs::write(&tmp, bin.bytes.as_slice())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_dir_takes_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let t = SessiondTarget::X86_64Linux;
        let p = dir.path().join(t.triple());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("falcon-sessiond"), b"abc").unwrap();
        assert!(candidates(dir.path(), t).iter().any(|c| c.is_file()));
    }

    #[test]
    fn local_install_is_content_addressed_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let b = SessiondBinary { target: SessiondTarget::Aarch64Mac, bytes: Arc::new(b"#!/bin/sh\n".to_vec()), hash8: "deadbeef".into() };
        let p = install_local(dir.path(), &b).unwrap();
        assert!(p.ends_with(b.file_name()));
        assert_eq!(std::fs::read(&p).unwrap(), b"#!/bin/sh\n");
        assert_eq!(install_local(dir.path(), &b).unwrap(), p);
    }
}
