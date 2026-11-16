//! 记住的访问密码（按服务端配置 id 分条）存哪儿。
//!
//! **macOS：数据目录里的 `passwords.json`（0600），不用钥匙串。** 安装包是 ad-hoc 签名
//! （`cargo xtask pkg`），钥匙串条目除了授权列表还有一张分区表（partition_id），按读写它的程序
//! 记 `cdhash:<哈希>`；每装一个新版本 cdhash 都变，App 一启动读密码就弹窗要 Mac 登录密码。
//! 换成自签证书也不行：授权列表那一层会稳定（designated requirement 变成"标识 + 证书"），
//! 但只有 Apple 签发的证书（Apple Development / Developer ID）分区才记 `teamid:`，自签的照样
//! 记 cdhash——2026-10 在登录钥匙串上实测过。别拿 `security create-keychain` 建的钥匙串去测：
//! 那是旧格式（version 256），没有分区这一层，两种签名都会"通过"。
//! 代价是同一用户下的进程都读得到这个文件：本机服务本来就是这个用户的完整 shell，远端服务端的
//! 密码与 `~/.ssh` 里的私钥同一档。升级前存在钥匙串里的那条（service `com.falcon.app`）不读也
//! 不删——读、删都会再弹一次授权；用户在登录框重输一次密码就落到文件里了。
//!
//! 其它平台照旧走系统凭据库（`keyring`）：Windows 凭据管理器没有按程序授权这一层，重装不弹。

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("passwords.json")
    }

    fn load(data_dir: &Path) -> BTreeMap<String, String> {
        std::fs::read_to_string(path(data_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn get(data_dir: &Path, profile_id: &str) -> Option<String> {
        load(data_dir).remove(profile_id)
    }

    pub fn set(data_dir: &Path, profile_id: &str, password: Option<&str>) {
        let mut all = load(data_dir);
        let changed = match password {
            Some(p) => all.insert(profile_id.to_string(), p.to_string()).as_deref() != Some(p),
            None => all.remove(profile_id).is_some(),
        };
        if !changed {
            return;
        }
        let file = path(data_dir);
        if let Err(err) = write_private(&file, &serde_json::to_vec_pretty(&all).unwrap_or_default()) {
            log::warn!("访问密码写盘失败（{}）：{err}", file.display());
        }
    }

    /// 同 platform.rs 的 write_atomic（临时文件 + 改名），但临时文件一建出来就是 0600：
    /// 先写后 chmod 会有一段时间是 umask 给的 0644，数据目录本身对其他用户可读
    fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        // mode 只在新建时生效：上次写到一半崩了留下的临时文件权限不一定对，删掉重建
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        drop(f);
        std::fs::rename(&tmp, path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn round_trip_and_file_mode() {
            let dir = std::env::temp_dir().join(format!("falcon-passwords-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);

            assert_eq!(get(&dir, "local"), None);
            set(&dir, "local", Some("hunter22"));
            set(&dir, "remote", Some("s3cret!"));
            assert_eq!(get(&dir, "local").as_deref(), Some("hunter22"));
            assert_eq!(get(&dir, "remote").as_deref(), Some("s3cret!"));
            let mode = std::fs::metadata(path(&dir)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);

            // 删一条不影响别的；删不存在的不报错
            set(&dir, "local", None);
            set(&dir, "local", None);
            assert_eq!(get(&dir, "local"), None);
            assert_eq!(get(&dir, "remote").as_deref(), Some("s3cret!"));

            // 坏文件当作空表，下次写入整份覆盖
            std::fs::write(path(&dir), "not json").unwrap();
            assert_eq!(get(&dir, "remote"), None);
            set(&dir, "remote", Some("again"));
            assert_eq!(get(&dir, "remote").as_deref(), Some("again"));

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::Path;

    const KEYRING_SERVICE: &str = "com.falcon.app";

    fn entry(profile_id: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, profile_id).ok()
    }

    pub fn get(_data_dir: &Path, profile_id: &str) -> Option<String> {
        entry(profile_id)?.get_password().ok()
    }

    pub fn set(_data_dir: &Path, profile_id: &str, password: Option<&str>) {
        let Some(entry) = entry(profile_id) else { return };
        let result = match password {
            Some(p) => entry.set_password(p),
            None => entry.delete_credential(),
        };
        if let Err(err) = result {
            // 没有条目时删除会报 NoEntry，那是正常的
            if password.is_some() {
                log::warn!("凭据库写入失败：{err}");
            }
        }
    }
}

pub use imp::{get, set};
