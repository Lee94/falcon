//! 凭据静态加密与访问密码哈希。移植自 `packages/server/src/crypto.ts`。
//!
//! **与 Node 版字节兼容是硬要求**：同一个数据目录要能被新旧两版轮流打开（附录 A"数据兼容"），
//! 老库里的主机密码、访问密码哈希必须原样可用。两处容易踩的：
//! - 密文存成 `base64(iv12 ‖ tag16 ‖ ct)`，而 aes-gcm crate 的 `encrypt` 给的是 `ct ‖ tag`，要重排；
//! - scrypt 用 Node `scryptSync` 的默认参数：N = 16384（log_n = 14）、r = 8、p = 1，输出 32 字节。
//!
//! 测试里的密文与哈希是 Node 按 TS 原样算出来的（见 tests），Rust 必须能解开 / 验过。

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, bail};
use base64::Engine as _;
use subtle::ConstantTimeEq;

const IV_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// 凭据静态加密：AES-256-GCM，密钥为后端数据目录下的 secret.key。
/// 保护级别：防数据库文件单独泄露，不防整机被攻破。
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    /// 打开（没有就生成）`<data_dir>/secret.key`。长度不是 32 字节说明文件坏了，拒绝启动——
    /// 拿一把错的钥匙继续跑，库里所有凭据都会解不开
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let key_path = data_dir.join("secret.key");
        if !key_path.exists() {
            let key: [u8; 32] = rand::random();
            write_private(&key_path, &key).with_context(|| format!("写 {} 失败", key_path.display()))?;
        }
        let key = std::fs::read(&key_path).with_context(|| format!("读 {} 失败", key_path.display()))?;
        Self::from_key(&key)
    }

    pub fn from_key(key: &[u8]) -> anyhow::Result<Self> {
        if key.len() != 32 {
            bail!("secret.key 已损坏（长度 {}，应为 32 字节）", key.len());
        }
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("secret.key 长度不对"))?;
        Ok(Self { cipher })
    }

    pub fn encrypt(&self, plaintext: &str) -> String {
        let iv: [u8; IV_LEN] = rand::random();
        let sealed = self
            .cipher
            .encrypt(&Nonce::from(iv), plaintext.as_bytes())
            .expect("AES-GCM 加密不会失败（明文远小于上限）");
        // crate 给的是 ct ‖ tag；落库格式是 iv ‖ tag ‖ ct
        let (ct, tag) = sealed.split_at(sealed.len() - TAG_LEN);
        let mut out = Vec::with_capacity(IV_LEN + TAG_LEN + ct.len());
        out.extend_from_slice(&iv);
        out.extend_from_slice(tag);
        out.extend_from_slice(ct);
        base64::engine::general_purpose::STANDARD.encode(out)
    }

    pub fn decrypt(&self, payload: &str) -> anyhow::Result<String> {
        let buf = base64::engine::general_purpose::STANDARD.decode(payload.trim()).context("凭据不是合法的 base64")?;
        if buf.len() < IV_LEN + TAG_LEN {
            bail!("凭据密文太短（{} 字节）", buf.len());
        }
        let (iv, rest) = buf.split_at(IV_LEN);
        let (tag, ct) = rest.split_at(TAG_LEN);
        let mut sealed = Vec::with_capacity(ct.len() + TAG_LEN);
        sealed.extend_from_slice(ct);
        sealed.extend_from_slice(tag);
        let nonce = Nonce::try_from(iv).map_err(|_| anyhow::anyhow!("iv 长度不对"))?;
        let plain = self
            .cipher
            .decrypt(&nonce, sealed.as_slice())
            .map_err(|_| anyhow::anyhow!("凭据解密失败（secret.key 换过，或数据被改过）"))?;
        String::from_utf8(plain).context("凭据解密结果不是 UTF-8")
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

// ---- 访问密码哈希（scrypt） ----

fn scrypt32(password: &str, salt: &[u8]) -> [u8; 32] {
    // Node scryptSync 的默认参数：N=16384、r=8、p=1
    let params = scrypt::Params::new(14, 8, 1).expect("scrypt 参数合法");
    let mut out = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut out).expect("输出长度 32 合法");
    out
}

/// `salthex:hashhex`，与 Node 版同一格式
pub fn hash_password(password: &str) -> String {
    let salt: [u8; 16] = rand::random();
    format!("{}:{}", hex::encode(salt), hex::encode(scrypt32(password, &salt)))
}

/// 存的哈希格式坏了（缺冒号、不是 hex、长度不对）一律当不匹配。Node 版在长度不等时
/// `timingSafeEqual` 会抛错、变成 500；这里收成 false，对用户来说都是"密码不对"
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split(':');
    let (Some(salt_hex), Some(hash_hex)) = (parts.next(), parts.next()) else {
        return false;
    };
    if salt_hex.is_empty() || hash_hex.is_empty() {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (hex::decode(salt_hex), hex::decode(hash_hex)) else {
        return false;
    };
    let actual = scrypt32(password, &salt);
    expected.len() == actual.len() && bool::from(actual.ct_eq(&expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node 用同一把钥匙、按 crypto.ts 原样算出来的（iv 随机）
    const NODE_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const NODE_ASCII: &str = "xDRzBQf+cu7JBoDA++6bbE9SGxPB05kgwhGTEPNqsR8LZmQ=";
    const NODE_CJK: &str = "L+ol7uGoDp6Fuq4bsFzLMAhmIWHlGIG0YhIrdogBBC5Gt25/67ou10qmoAEGrFGtTf4=";
    const NODE_EMPTY: &str = "ZVD7UOxuZw0oQjmHcIx9HZ/vs9E1+QlBK2t4xA==";
    /// Node `hashPassword("s3cret-密码")`
    const NODE_HASH: &str =
        "659067c7efa6420e918d69e2338dfd7f:8fe2d8705642f13c6d9773b6197708dae0a76835621a7b965c2235bf02bcf1a9";

    fn node_box() -> SecretBox {
        SecretBox::from_key(&hex::decode(NODE_KEY).unwrap()).unwrap()
    }

    #[test]
    fn decrypts_what_node_encrypted() {
        let b = node_box();
        assert_eq!(b.decrypt(NODE_ASCII).unwrap(), "hunter2");
        assert_eq!(b.decrypt(NODE_CJK).unwrap(), "密码🔑 with spaces");
        assert_eq!(b.decrypt(NODE_EMPTY).unwrap(), "");
    }

    #[test]
    fn round_trips_in_node_layout() {
        let b = node_box();
        let enc = b.encrypt("中文 secret");
        let raw = base64::engine::general_purpose::STANDARD.decode(&enc).unwrap();
        assert_eq!(raw.len(), IV_LEN + TAG_LEN + "中文 secret".len());
        assert_eq!(b.decrypt(&enc).unwrap(), "中文 secret");
        // 同一明文两次加密 iv 不同
        assert_ne!(b.encrypt("x"), b.encrypt("x"));
    }

    #[test]
    fn rejects_tampered_or_wrong_key() {
        let mut raw = base64::engine::general_purpose::STANDARD.decode(NODE_ASCII).unwrap();
        *raw.last_mut().unwrap() ^= 1;
        let tampered = base64::engine::general_purpose::STANDARD.encode(raw);
        assert!(node_box().decrypt(&tampered).is_err());
        let other = SecretBox::from_key(&[7u8; 32]).unwrap();
        assert!(other.decrypt(NODE_ASCII).is_err());
        assert!(node_box().decrypt("AAAA").is_err());
        assert!(SecretBox::from_key(&[0u8; 31]).is_err());
    }

    #[test]
    fn verifies_node_password_hash() {
        assert!(verify_password("s3cret-密码", NODE_HASH));
        assert!(!verify_password("s3cret", NODE_HASH));
    }

    #[test]
    fn hash_round_trip_and_malformed() {
        let h = hash_password("pw");
        assert!(verify_password("pw", &h));
        assert!(!verify_password("pw2", &h));
        assert!(!verify_password("pw", "nocolon"));
        assert!(!verify_password("pw", ":abc"));
        assert!(!verify_password("pw", "zz:zz"));
        assert!(!verify_password("pw", "00:00"));
    }

    #[test]
    fn key_file_created_once_with_private_mode() {
        let dir = tempfile::tempdir().unwrap();
        let a = SecretBox::open(dir.path()).unwrap();
        let enc = a.encrypt("v");
        let b = SecretBox::open(dir.path()).unwrap();
        assert_eq!(b.decrypt(&enc).unwrap(), "v");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("secret.key")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
