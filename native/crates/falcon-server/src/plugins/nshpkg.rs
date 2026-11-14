//! `.nshpkg` 签名沙盒面板包的密钥引导、重签与校验（ADR 0028）。
//!
//! 自托管的 `falcon-server` 实例在 `<数据目录>/plugins/.keys/` 下自动维护两级 Ed25519 密钥：
//! - `root.sec`（`0600`）与 `root.pub`：实例信任根，公钥经 `GET /api/plugins/trust`（以及存在包插件时的
//!   `GET /api/plugins` `trustRoot`）发给已认证客户端；
//! - `release.sec`（`0600`）与 `release.cert.json`：由 `root.sec` 签发的发布密钥证书。
//!
//! 用户放入 `<数据目录>/plugins/<id>.nshpkg` 或 `<数据目录>/plugins/<id>/` 下的 `.nshpkg` 包（可由任意
//! 开发者或构建流水线自签名），服务端先由 `nsh_package::resign` 校验原始包自身签名与文件哈希完好，再用本实例的
//! 发布密钥重签，使连接的六端客户端只需信任当前 `falcon-server` 的根公钥即可在本地 `TrustStore` 中完成
//! 证书链、签名、SHA-256 与构建号防回滚校验。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use falcon_proto::PluginPackageInfo;
use nsh_package::{Certificate, Contents, PublicKey, SecretKey, TrustStore, VerifiedPackage, certify, resign};

/// 单个 `.nshpkg` 文件的大小上限（16 MiB）：右侧栏面板包只含 UI 逻辑与少量静态资源。
pub const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;

/// 证书有效期上限（约 100 年，Unix 秒），避免自托管实例因本地证书过期导致面板停摆。
const CERT_VALIDITY_SECS: u64 = 100 * 365 * 24 * 3600;

struct Keys {
    root_pub: PublicKey,
    release_sec: SecretKey,
    release_cert: Certificate,
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone)]
pub struct SignedPackage {
    pub bytes: Vec<u8>,
    pub meta: PluginPackageInfo,
    pub app_name: String,
}

pub struct PluginKeyring {
    keys_dir: PathBuf,
    keys: Mutex<Option<Keys>>,
    cache: Mutex<HashMap<PathBuf, (FileStamp, Result<SignedPackage, String>)>>,
}

impl PluginKeyring {
    pub fn new(plugins_dir: &Path) -> Self {
        Self {
            keys_dir: plugins_dir.join(".keys"),
            keys: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 获取（并在缺失时自动生成）当前服务端实例的信任根公钥（Base64）。
    pub fn root_public_key_base64(&self) -> Result<String, String> {
        self.with_keys(|k| Ok(k.root_pub.to_base64()))
    }

    /// 用本实例发布密钥直接构建并签名一份 `.nshpkg`（供测试与本地打包工具使用）。
    pub fn build_package(&self, contents: &Contents) -> Result<Vec<u8>, String> {
        self.with_keys(|k| {
            nsh_package::build(contents, &k.release_sec, &k.release_cert).map_err(|e| format!("打包签名失败：{e}"))
        })
    }

    /// 读取磁盘上的 `.nshpkg`，校验完整性并用本实例发布证书重签，按 `(len, mtime)` 缓存结果。
    pub fn load_signed(&self, path: &Path) -> Result<SignedPackage, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("读不了面板包 {}：{e}", path.display()))?;
        if !meta.is_file() {
            return Err("面板包路径不是普通文件".into());
        }
        if meta.len() > MAX_PACKAGE_BYTES {
            return Err(format!("面板包太大了（超过 {} MiB）", MAX_PACKAGE_BYTES >> 20));
        }
        let stamp = FileStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
        };
        if let Some((cached_stamp, cached_res)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(path)
            && *cached_stamp == stamp
        {
            return cached_res.clone();
        }

        let raw = std::fs::read(path).map_err(|e| format!("读取面板包失败：{e}"))?;
        let res = self.resign_bytes(&raw);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf(), (stamp, res.clone()));
        res
    }

    fn resign_bytes(&self, raw: &[u8]) -> Result<SignedPackage, String> {
        self.with_keys(|k| {
            let signed = resign(raw, &k.release_sec, &k.release_cert)
                .map_err(|e| format!(".nshpkg 签名或哈希校验未通过：{e}"))?;
            let verified = verify_with_root(&signed, k.root_pub)?;
            let size = signed.len() as u64;
            Ok(SignedPackage {
                bytes: signed,
                app_name: verified.app.name,
                meta: PluginPackageInfo {
                    app_id: verified.app.id,
                    version: verified.app.version,
                    build: verified.app.build,
                    sha256: verified.sha256,
                    size,
                },
            })
        })
    }

    fn with_keys<T>(&self, f: impl FnOnce(&Keys) -> Result<T, String>) -> Result<T, String> {
        let mut slot = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(load_or_init_keys(&self.keys_dir)?);
        }
        f(slot.as_ref().expect("initialized above"))
    }
}

fn verify_with_root(bytes: &[u8], root_pub: PublicKey) -> Result<VerifiedPackage, String> {
    let mut store = TrustStore::new();
    store.add_root(root_pub);
    store
        .verify(bytes, "")
        .map_err(|e| format!("重签后的 .nshpkg 未通过本机信任根校验：{e}"))
}

fn load_or_init_keys(dir: &Path) -> Result<Keys, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建插件密钥目录失败：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }

    let root_sec_path = dir.join("root.sec");
    let root_pub_path = dir.join("root.pub");
    let rel_sec_path = dir.join("release.sec");
    let rel_cert_path = dir.join("release.cert.json");

    if let (Ok(root_b64), Ok(rel_b64), Ok(cert_json)) = (
        std::fs::read_to_string(&root_sec_path),
        std::fs::read_to_string(&rel_sec_path),
        std::fs::read_to_string(&rel_cert_path),
    ) && let (Ok(root_sec), Ok(release_sec), Ok(release_cert)) = (
        SecretKey::from_base64(&root_b64),
        SecretKey::from_base64(&rel_b64),
        Certificate::from_json(&cert_json),
    ) {
        let root_pub = root_sec.public();
        // 核对现有发布证书确实由当前根密钥签发且未损坏；若不一致则重新签发证书。
        let now = now_secs();
        let valid_cert = refresh_cert_if_needed(&root_sec, &release_sec, &release_cert, &rel_cert_path, now)?;
        return Ok(Keys {
            root_pub,
            release_sec,
            release_cert: valid_cert,
        });
    }

    let root_sec = SecretKey::generate().map_err(|e| format!("生成插件根密钥失败：{e}"))?;
    let release_sec = SecretKey::generate().map_err(|e| format!("生成插件发布密钥失败：{e}"))?;
    let root_pub = root_sec.public();
    let now = now_secs();
    let release_cert = certify(
        &root_sec,
        &release_sec.public(),
        &["*".to_string()],
        now.saturating_sub(3600),
        now.saturating_add(CERT_VALIDITY_SECS),
    );

    write_secret_file(&root_sec_path, root_sec.to_base64().as_bytes())?;
    write_public_file(&root_pub_path, root_pub.to_base64().as_bytes())?;
    write_secret_file(&rel_sec_path, release_sec.to_base64().as_bytes())?;
    write_public_file(&rel_cert_path, release_cert.to_json().as_bytes())?;

    Ok(Keys {
        root_pub,
        release_sec,
        release_cert,
    })
}

fn refresh_cert_if_needed(
    root_sec: &SecretKey,
    release_sec: &SecretKey,
    cert: &Certificate,
    cert_path: &Path,
    now: u64,
) -> Result<Certificate, String> {
    let probe = Contents {
        app: nsh_package::App {
            id: "com.falcon.probe".into(),
            name: "probe".into(),
            version: "0.1.0".into(),
            build: 1,
        },
        capabilities: None,
        source: "export default function Probe() { return null; }\n".into(),
        bytecode: None,
        source_map: None,
        assets: Vec::new(),
        min_runtime: None,
    };
    if let Ok(pkg) = nsh_package::build(&probe, release_sec, cert)
        && verify_with_root(&pkg, root_sec.public()).is_ok()
    {
        return Ok(cert.clone());
    }
    let fresh = certify(
        root_sec,
        &release_sec.public(),
        &["*".to_string()],
        now.saturating_sub(3600),
        now.saturating_add(CERT_VALIDITY_SECS),
    );
    write_public_file(cert_path, fresh.to_json().as_bytes())?;
    Ok(fresh)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("写入密钥 {} 失败：{e}", path.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("写入密钥 {} 失败：{e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, bytes).map_err(|e| format!("写入密钥 {} 失败：{e}", path.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("保存密钥 {} 失败：{e}", path.display()))
}

fn write_public_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("保存 {} 失败：{e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_package::App;

    fn sample_external_package(build: u64) -> Vec<u8> {
        let ext_root = SecretKey::generate().unwrap();
        let ext_rel = SecretKey::generate().unwrap();
        let now = now_secs();
        let ext_cert = certify(
            &ext_root,
            &ext_rel.public(),
            &["com.falcon.plugin.*".into()],
            now.saturating_sub(60),
            now + 3600,
        );
        nsh_package::build(
            &Contents {
                app: App {
                    id: "com.falcon.plugin.demo".into(),
                    name: "示例沙盒面板".into(),
                    version: "0.1.0".into(),
                    build,
                },
                capabilities: None,
                source: "export default function Demo() { return 'ok'; }\n".into(),
                bytecode: None,
                source_map: None,
                assets: vec![("icon.svg".into(), b"<svg/>".to_vec())],
                min_runtime: None,
            },
            &ext_rel,
            &ext_cert,
        )
        .unwrap()
    }

    #[test]
    fn bootstraps_keys_resigns_external_package_and_verifies_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let keyring = PluginKeyring::new(&tmp.path().join("plugins"));
        let root_b64 = keyring.root_public_key_base64().unwrap();
        let root_pub = PublicKey::from_base64(&root_b64).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(tmp.path().join("plugins/.keys/root.sec"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let pkg_path = tmp.path().join("demo.nshpkg");
        std::fs::write(&pkg_path, sample_external_package(2)).unwrap();
        let signed = keyring.load_signed(&pkg_path).unwrap();
        assert_eq!(signed.meta.app_id, "com.falcon.plugin.demo");
        assert_eq!(signed.meta.build, 2);

        // 客户端只信任服务端根公钥，就能验过原本由第三方密钥签名的包
        let store_dir = tmp.path().join("client-trust");
        let mut client_store = TrustStore::open(&store_dir).unwrap();
        client_store.add_root(root_pub);
        let verified = client_store.verify(&signed.bytes, "").unwrap();
        assert_eq!(verified.source, "export default function Demo() { return 'ok'; }\n");
        client_store.accept(&verified).unwrap();

        // 防回滚：构建号更低的旧包被客户端 TrustStore 拒绝
        let older_path = tmp.path().join("older.nshpkg");
        std::fs::write(&older_path, sample_external_package(1)).unwrap();
        let older_signed = keyring.load_signed(&older_path).unwrap();
        assert!(matches!(
            client_store.verify(&older_signed.bytes, ""),
            Err(nsh_package::Error::Rollback { build: 1, accepted: 2, .. })
        ));

        // 篡改单字节：服务端重签阶段即拒绝
        let mut tampered = sample_external_package(3);
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0xff;
        let bad_path = tmp.path().join("tampered.nshpkg");
        std::fs::write(&bad_path, tampered).unwrap();
        assert!(keyring.load_signed(&bad_path).is_err());
    }
}
