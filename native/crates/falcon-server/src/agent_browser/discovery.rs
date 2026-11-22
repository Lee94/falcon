//! 被动发现 agent-browser 守护进程（ADR 0029 决定四）。
//!
//! agent-browser 的内部文件布局解析只放在本文件一处，并用固定样例文件做单元测试；
//! 绝不调用 `agent-browser` CLI（避免顺手拉起守护进程）。

use std::collections::HashMap;
use std::path::Path;

use falcon_proto::MIN_AGENT_BROWSER_VERSION;

use super::is_valid_session_id;

/// 发现结果：某个终端会话当前存活的 agent-browser stream 端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredBrowser {
    pub session_id: String,
    pub port: u16,
    pub pid: u32,
    pub version: (u32, u32, u32),
}

/// 扫描阶段被拒（版本过低）的条目，供调用方按 (session_id, pid) 去重打一次日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedBrowser {
    pub session_id: String,
    pub pid: u32,
    pub raw_version: String,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub active: HashMap<String, DiscoveredBrowser>,
    pub rejected: Vec<RejectedBrowser>,
}

/// 扫描 `<dataDir>/agent-browser/run/`，找出所有满足条件（有 `.stream`、`.pid` 存活、`.version` 达标）的浏览器会话。
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
        let Some(session_id) = stem.strip_prefix("falcon-") else { continue };
        if !is_valid_session_id(session_id) {
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

        let version_path = run_dir.join(format!("{stem}.version"));
        let version_raw = std::fs::read_to_string(&version_path).unwrap_or_default();
        let Some(version) = parse_semver(&version_raw) else {
            report.rejected.push(RejectedBrowser {
                session_id: session_id.to_string(),
                pid,
                raw_version: version_raw.trim().to_string(),
            });
            continue;
        };
        if version < MIN_AGENT_BROWSER_VERSION {
            report.rejected.push(RejectedBrowser {
                session_id: session_id.to_string(),
                pid,
                raw_version: version_raw.trim().to_string(),
            });
            continue;
        }
        report.active.insert(
            session_id.to_string(),
            DiscoveredBrowser {
                session_id: session_id.to_string(),
                port,
                pid,
                version,
            },
        );
    }
    report
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

/// 解析 `.version` 文件：`0.43.0`、`v0.43`、`0.43.1-beta.1` 或 JSON `{"version":"0.43.0"}`。
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
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn pid_alive(pid: u32) -> bool {
    pid > 0
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
    fn scans_fixture_files_with_version_and_pid_checks() {
        let dir = tempfile::tempdir().unwrap();
        // 正常会话
        std::fs::write(dir.path().join("falcon-sess-ok.stream"), "43123\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-ok.pid"), "1001\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-ok.version"), "0.43.0\n").unwrap();
        // 版本过低
        std::fs::write(dir.path().join("falcon-sess-old.stream"), "43124\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-old.pid"), "1002\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-old.version"), "0.42.9\n").unwrap();
        // 进程已死
        std::fs::write(dir.path().join("falcon-sess-dead.stream"), "43125\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-dead.pid"), "9999\n").unwrap();
        std::fs::write(dir.path().join("falcon-sess-dead.version"), "0.45.0\n").unwrap();
        // 非 falcon 会话
        std::fs::write(dir.path().join("other.stream"), "43126\n").unwrap();
        std::fs::write(dir.path().join("other.pid"), "1001\n").unwrap();
        std::fs::write(dir.path().join("other.version"), "0.45.0\n").unwrap();

        let report = scan_run_dir_with_probe(dir.path(), |pid| pid != 9999);
        assert_eq!(report.active.len(), 1);
        let ok = report.active.get("sess-ok").unwrap();
        assert_eq!(ok.port, 43123);
        assert_eq!(ok.pid, 1001);
        assert_eq!(ok.version, (0, 43, 0));

        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].session_id, "sess-old");
        assert_eq!(report.rejected[0].raw_version, "0.42.9");
    }
}
