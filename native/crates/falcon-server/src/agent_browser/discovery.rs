//! 被动发现 agent-browser 守护进程（ADR 0029 决定四）。
//!
//! agent-browser 的内部文件布局解析只放在本文件一处，并用固定样例文件做单元测试；
//! 绝不调用 `agent-browser` CLI（避免顺手拉起守护进程）。

use std::collections::HashMap;
use std::path::Path;

use falcon_proto::MIN_AGENT_BROWSER_VERSION;

use super::{BrowserKey, is_valid_browser_session_name, is_valid_session_id, policy_dir};

/// 发现结果：某个终端会话的一个 agent-browser stream 端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredBrowser {
    pub key: BrowserKey,
    pub port: u16,
    pub pid: u32,
    pub version: (u32, u32, u32),
}

/// 扫描阶段被拒（版本过低）的条目，供调用方按实例与 pid 去重打一次日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedBrowser {
    pub key: BrowserKey,
    pub pid: u32,
    pub raw_version: String,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub active: HashMap<BrowserKey, DiscoveredBrowser>,
    pub rejected: Vec<RejectedBrowser>,
}

/// 扫描 `<dataDir>/agent-browser/run/`，找出所有满足条件（有 `.stream`、`.pid` 存活、`.version` 达标）
/// 且能关联到 Falcon 终端的浏览器实例。
pub fn scan_run_dir(run_dir: &Path) -> ScanReport {
    scan_run_dir_with_probe(run_dir, pid_alive)
}

fn scan_run_dir_with_probe(run_dir: &Path, is_alive: impl Fn(u32) -> bool) -> ScanReport {
    let mut report = ScanReport::default();
    let Ok(entries) = std::fs::read_dir(run_dir) else { return report };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = name.strip_suffix(".stream") else { continue };
        if !is_valid_browser_session_name(stem) {
            continue;
        }
        let Ok(stream_raw) = std::fs::read_to_string(&path) else { continue };
        let Some(port) = parse_port(&stream_raw) else { continue };

        let pid_path = run_dir.join(format!("{stem}.pid"));
        let Ok(pid_raw) = std::fs::read_to_string(&pid_path) else { continue };
        let Some(pid) = parse_pid(&pid_raw) else { continue };
        if !is_alive(pid) {
            continue;
        }

        // 命名实例通过继承的 policy 归属终端。没有该元数据的默认实例仍沿用原命名规则。
        let session_id = owner_from_policy_config(run_dir, stem).or_else(|| {
            stem.strip_prefix("falcon-").filter(|id| is_valid_session_id(id)).map(str::to_owned)
        });
        let Some(key) = session_id.and_then(|id| BrowserKey::new(&id, stem)) else { continue };

        let version_path = run_dir.join(format!("{stem}.version"));
        let version_raw = std::fs::read_to_string(&version_path).unwrap_or_default();
        let Some(version) = parse_semver(&version_raw) else {
            report.rejected.push(RejectedBrowser { key, pid, raw_version: version_raw.trim().to_string() });
            continue;
        };
        if version < MIN_AGENT_BROWSER_VERSION {
            report.rejected.push(RejectedBrowser { key, pid, raw_version: version_raw.trim().to_string() });
            continue;
        }
        report.active.insert(key.clone(), DiscoveredBrowser { key, port, pid, version });
    }
    report
}

/// 自定义 browser session 仍继承 `<dataDir>/agent-browser/policy/<sessionId>.json`。
/// 只接受与注入路径完全相等的绝对 / 相对词法路径，不能仅凭任意 JSON 里的文件名认领实例。
fn owner_from_policy_config(run_dir: &Path, browser_session: &str) -> Option<String> {
    let stored = super::policy::read_policy_setting(&run_dir.join(format!("{browser_session}.config")))?;
    let stored_path = Path::new(&stored);
    let file_name = stored_path.file_name()?.to_str()?;
    let session_id = file_name.strip_suffix(".json")?;
    if !is_valid_session_id(session_id) {
        return None;
    }
    let agent_browser_root = run_dir.parent()?;
    let expected = policy_dir(agent_browser_root.parent()?).join(format!("{session_id}.json"));
    (stored_path == expected).then(|| session_id.to_string())
}

/// 解析 `.stream` 文件里的端口号：兼容纯数字与 JSON `{"port": 12345}`。
pub fn parse_port(raw: &str) -> Option<u16> {
    let trimmed = raw.trim();
    if let Ok(port) = trimmed.parse::<u16>() {
        return (port > 0).then_some(port);
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let port = val.get("port")?.as_u64()?;
    u16::try_from(port).ok().filter(|&p| p > 0)
}

/// 解析 `.pid` 文件：纯整数或 JSON `{"pid": 123}`。
pub fn parse_pid(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if let Ok(pid) = trimmed.parse::<u32>() {
        return (pid > 0).then_some(pid);
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = val.get("pid")?.as_u64()?;
    u32::try_from(pid).ok().filter(|&p| p > 0)
}

/// 解析 `.version` 文件：`0.43.0`、`v0.43`、`0.43.1-beta.1` 或 JSON `{"version":"0.43"}`。
pub fn parse_semver(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.starts_with('{') {
        serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()?
            .get("version")?
            .as_str()?
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    };
    let s = candidate.strip_prefix('v').or_else(|| candidate.strip_prefix('V')).unwrap_or(&candidate);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    if pid <= 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(handle) };
    alive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_pid_and_semver_formats() {
        assert_eq!(parse_port("51234\n"), Some(51234));
        assert_eq!(parse_port(r#"{"port": 51234}"#), Some(51234));
        assert_eq!(parse_port("0"), None);

        assert_eq!(parse_pid("9912\n"), Some(9912));
        assert_eq!(parse_pid(r#"{"pid": 9912}"#), Some(9912));

        assert_eq!(parse_semver("0.43.0"), Some((0, 43, 0)));
        assert_eq!(parse_semver("v0.43"), Some((0, 43, 0)));
        assert_eq!(parse_semver("0.44.2-rc.1\n"), Some((0, 44, 2)));
        assert_eq!(parse_semver(r#"{"version":"v0.43.1"}"#), Some((0, 43, 1)));
        assert_eq!(parse_semver(""), None);
    }

    #[test]
    fn scans_default_and_policy_owned_named_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agent-browser");
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let write = |stem: &str, port: u16, pid: u32, version: &str| {
            std::fs::write(run.join(format!("{stem}.stream")), format!("{port}\n")).unwrap();
            std::fs::write(run.join(format!("{stem}.pid")), format!("{pid}\n")).unwrap();
            std::fs::write(run.join(format!("{stem}.version")), format!("{version}\n")).unwrap();
        };
        write("falcon-sess-ok", 43123, 1001, "0.43.0");
        write("falcon-sess-old", 43124, 1002, "0.42.9");
        write("falcon-sess-dead", 43125, 9999, "0.45.0");
        write("other", 43126, 1001, "0.45.0");

        // 同一个终端可以有第二个、由脚本自行命名的浏览器。
        write("vinci-viewer", 43127, 1003, "0.43.0");
        let policy = root.join("policy").join("sess-ok.json");
        std::fs::write(
            run.join("vinci-viewer.config"),
            serde_json::json!({"action_policy": policy.to_string_lossy()}).to_string(),
        )
        .unwrap();
        // policy 不属于 Falcon 注入目录的实例不能认领。
        write("unowned", 43128, 1004, "0.43.0");
        std::fs::write(run.join("unowned.config"), r#"{"action_policy":"/other/sess-ok.json"}"#).unwrap();

        let report = scan_run_dir_with_probe(&run, |pid| pid != 9999);
        assert_eq!(report.active.len(), 2);
        let default_key = BrowserKey::for_request("sess-ok", None).unwrap();
        assert_eq!(report.active.get(&default_key).unwrap().port, 43123);
        let named_key = BrowserKey::new("sess-ok", "vinci-viewer").unwrap();
        let named = report.active.get(&named_key).unwrap();
        assert_eq!(named.port, 43127);
        assert_eq!(named.pid, 1003);
        assert_eq!(named.version, (0, 43, 0));

        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].key.session_id, "sess-old");
        assert_eq!(report.rejected[0].raw_version, "0.42.9");
    }
}
