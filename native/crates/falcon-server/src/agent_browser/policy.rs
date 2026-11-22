//! Action policy 接管锁（ADR 0029 决定五）。
//!
//! 守护进程每执行一条命令前都会重读 policy 文件，而 stream 注入的输入直走 CDP。
//! 原子写（临时文件 + rename）确保守护进程不会读到半截 JSON。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{is_valid_session_id, policy_dir, run_dir};

pub const ALLOW_POLICY_JSON: &str = r#"{"default":"allow"}"#;
pub const TAKEOVER_POLICY_JSON: &str = r#"{"default":"deny","allow":["snapshot","get","read","wait"]}"#;

/// 接管期间允许 agent 执行的只读 / 等待类动作。
pub const TAKEOVER_ALLOWED_ACTIONS: &[&str] = &["snapshot", "get", "read", "wait"];

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn policy_path(data_dir: &Path, session_id: &str) -> PathBuf {
    policy_dir(data_dir).join(format!("{session_id}.json"))
}

pub fn write_allow(data_dir: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    write_policy_file(data_dir, session_id, ALLOW_POLICY_JSON)
}

pub fn write_takeover(data_dir: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    write_policy_file(data_dir, session_id, TAKEOVER_POLICY_JSON)
}

fn write_policy_file(data_dir: &Path, session_id: &str, body: &str) -> std::io::Result<PathBuf> {
    if !is_valid_session_id(session_id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "非法会话 id"));
    }
    let dir = policy_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let target = policy_path(data_dir, session_id);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{session_id}.json.tmp.{}.{seq}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        f.write_all(body.as_bytes())?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// 服务端启动或关闭时把所有遗留的 policy 文件重置为 allow，避免留下卡死的锁。
pub fn reset_all_to_allow(data_dir: &Path) {
    let dir = policy_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if let Some(session_id) = name.strip_suffix(".json")
            && is_valid_session_id(session_id)
        {
            if let Err(e) = write_allow(data_dir, session_id) {
                log::warn!("重置浏览器 policy 失败（{session_id}）：{e}");
            }
        }
    }
}

pub fn remove_policy(data_dir: &Path, session_id: &str) {
    if !is_valid_session_id(session_id) {
        return;
    }
    let _ = std::fs::remove_file(policy_path(data_dir, session_id));
}

pub(super) fn read_policy_setting(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("action_policy")
        .or_else(|| value.get("actionPolicy"))
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

/// 当前守护进程的 `.config` 和重启时沿用的 `.settings` 都可能指定另一份 policy。
pub(super) fn settings_overrides_browser_policy(data_dir: &Path, session_id: &str, browser_session: &str) -> bool {
    let run = run_dir(data_dir);
    let expected = policy_path(data_dir, session_id);
    ["config", "settings"].iter().any(|ext| {
        read_policy_setting(&run.join(format!("{browser_session}.{ext}")))
            .is_some_and(|path| Path::new(&path) != expected.as_path())
    })
}

pub fn is_allowed_during_takeover(action: &str) -> bool {
    TAKEOVER_ALLOWED_ACTIONS.iter().any(|a| a.eq_ignore_ascii_case(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_reset_and_remove_policy() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_takeover(dir.path(), "sess-1").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), TAKEOVER_POLICY_JSON);
        reset_all_to_allow(dir.path());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), ALLOW_POLICY_JSON);
        remove_policy(dir.path(), "sess-1");
        assert!(!p.exists());
    }

    #[test]
    fn detects_settings_override() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_dir(dir.path());
        std::fs::create_dir_all(&run).unwrap();
        let expected = policy_path(dir.path(), "s1");
        std::fs::write(
            run.join("falcon-s1.settings"),
            serde_json::json!({ "actionPolicy": expected.to_string_lossy() }).to_string(),
        )
        .unwrap();
        assert!(!settings_overrides_browser_policy(dir.path(), "s1", "falcon-s1"));

        std::fs::write(
            run.join("falcon-s1.settings"),
            serde_json::json!({ "actionPolicy": "/other/policy.json" }).to_string(),
        )
        .unwrap();
        assert!(settings_overrides_browser_policy(dir.path(), "s1", "falcon-s1"));
    }
}
