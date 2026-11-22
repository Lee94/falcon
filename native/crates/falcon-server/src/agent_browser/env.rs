//! 终端会话启动时的环境变量注入（ADR 0029 决定三）。

use std::path::Path;

use super::policy::{policy_path, write_allow};
use super::{browser_session_name, is_valid_session_id, run_dir};

pub const ENV_SESSION: &str = "AGENT_BROWSER_SESSION";
pub const ENV_SOCKET_DIR: &str = "AGENT_BROWSER_SOCKET_DIR";
pub const ENV_ACTION_POLICY: &str = "AGENT_BROWSER_ACTION_POLICY";
pub const ENV_STREAM_QUALITY: &str = "AGENT_BROWSER_STREAM_QUALITY";
pub const ENV_STREAM_MAX_WIDTH: &str = "AGENT_BROWSER_STREAM_MAX_WIDTH";

pub const DEFAULT_STREAM_QUALITY: &str = "60";
pub const DEFAULT_STREAM_MAX_WIDTH: &str = "1280";

/// Unix domain socket 路径长度上限（留 1 字节给结尾 NUL）：macOS 104 字节，Linux 108 字节。
#[cfg(target_os = "macos")]
const UNIX_SOCKET_MAX_BYTES: usize = 103;
#[cfg(all(unix, not(target_os = "macos")))]
const UNIX_SOCKET_MAX_BYTES: usize = 107;
#[cfg(not(unix))]
const UNIX_SOCKET_MAX_BYTES: usize = usize::MAX;

/// 注入结果：标明是否启用以及 policy 锁是否有效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectOutcome {
    pub enabled: bool,
    pub policy_enforced: bool,
}

/// 准备目录与默认 allow policy 文件，并把未由用户设置的环境变量写入 `env`。
///
/// - Unix socket 路径超过平台上限时不注入这一组变量，记一条日志；
/// - 登录环境里已存在的同名非空变量不覆盖；
/// - 会话启动前就写好 allow policy，避免环境变量指向不存在的文件导致 agent 命令失败。
pub fn inject_session_env(data_dir: &Path, session_id: &str, env: &mut Vec<(String, String)>) -> InjectOutcome {
    if !is_valid_session_id(session_id) {
        return InjectOutcome { enabled: false, policy_enforced: false };
    }
    let session_name = browser_session_name(session_id);
    let run = run_dir(data_dir);
    let sock = run.join(format!("{session_name}.sock"));
    let sock_len = sock.as_os_str().as_encoded_bytes().len();
    if sock_len > UNIX_SOCKET_MAX_BYTES {
        log::warn!(
            "agent-browser socket 路径过长（{sock_len} > {UNIX_SOCKET_MAX_BYTES} 字节），禁用该会话的浏览器接入：{}",
            sock.display()
        );
        return InjectOutcome { enabled: false, policy_enforced: false };
    }

    if let Err(e) = std::fs::create_dir_all(&run) {
        log::warn!("创建 agent-browser run 目录失败：{e}");
        return InjectOutcome { enabled: false, policy_enforced: false };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700));
    }

    let policy = policy_path(data_dir, session_id);
    let user_has_policy = has_non_empty_env(env, ENV_ACTION_POLICY);
    let mut policy_enforced = !user_has_policy;
    if !user_has_policy {
        if let Err(e) = write_allow(data_dir, session_id) {
            log::warn!("写入默认 agent-browser policy 失败（{session_id}）：{e}");
            policy_enforced = false;
        }
    }

    let pairs = [
        (ENV_SESSION, session_name),
        (ENV_SOCKET_DIR, run.to_string_lossy().into_owned()),
        (ENV_ACTION_POLICY, policy.to_string_lossy().into_owned()),
        (ENV_STREAM_QUALITY, DEFAULT_STREAM_QUALITY.to_string()),
        (ENV_STREAM_MAX_WIDTH, DEFAULT_STREAM_MAX_WIDTH.to_string()),
    ];
    for (k, v) in pairs {
        if k == ENV_ACTION_POLICY && !policy_enforced {
            continue;
        }
        if !has_non_empty_env(env, k) {
            set_env(env, k, v);
        }
    }

    InjectOutcome { enabled: true, policy_enforced }
}

fn has_non_empty_env(env: &[(String, String)], key: &str) -> bool {
    env.iter().any(|(k, v)| k == key && !v.is_empty())
}

fn set_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some((_, slot)) = env.iter_mut().find(|(k, _)| k == key) {
        *slot = value;
    } else {
        env.push((key.to_string(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_browser::policy::ALLOW_POLICY_JSON;

    #[test]
    fn injects_all_vars_and_creates_allow_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = vec![("PATH".into(), "/usr/bin".into())];
        let out = inject_session_env(dir.path(), "abc-123", &mut env);
        assert_eq!(out, InjectOutcome { enabled: true, policy_enforced: true });

        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str());
        assert_eq!(get(ENV_SESSION), Some("falcon-abc-123"));
        assert_eq!(get(ENV_STREAM_QUALITY), Some("60"));
        assert_eq!(get(ENV_STREAM_MAX_WIDTH), Some("1280"));
        let policy = get(ENV_ACTION_POLICY).unwrap();
        assert_eq!(std::fs::read_to_string(policy).unwrap(), ALLOW_POLICY_JSON);
    }

    #[test]
    fn does_not_overwrite_user_env_vars() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = vec![
            (ENV_ACTION_POLICY.into(), "/custom/policy.json".into()),
            (ENV_STREAM_QUALITY.into(), "90".into()),
        ];
        let out = inject_session_env(dir.path(), "abc-123", &mut env);
        assert_eq!(out, InjectOutcome { enabled: true, policy_enforced: false });
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str());
        assert_eq!(get(ENV_ACTION_POLICY), Some("/custom/policy.json"));
        assert_eq!(get(ENV_STREAM_QUALITY), Some("90"));
    }

    #[cfg(unix)]
    #[test]
    fn skips_injection_when_socket_path_exceeds_limit() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a".repeat(90));
        let mut env = Vec::new();
        let out = inject_session_env(&deep, "12345678-1234-1234-1234-1234567890ab", &mut env);
        assert_eq!(out, InjectOutcome { enabled: false, policy_enforced: false });
        assert!(env.is_empty());
    }
}
