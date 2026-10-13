//! SSH 项目建会话前，要不要先弹「为这台主机启用持久会话」（ADR 0020 决定四 / 七）。
//!
//! 二进制由后端推到宿主机、按版本命名：宿主机上的版本与这个 falcon 要的对不上时，建会话那一下
//! 后端会先推新版本——REST 请求要卡上几秒到几十秒，看不到进度。所以版本不符也走安装对话框
//! （已授权的直接开始部署、不再问），进度经 `/ws/install` 看得见。

use falcon_proto::HostInstallStatus;

/// 还没问过授权，或者授权了但宿主机上没有这个 falcon 要的那一版守护进程
pub fn needs_host_setup(status: &HostInstallStatus) -> bool {
    match status.authorized {
        None => true,
        Some(true) => status.installed_version.as_deref() != Some(status.required_version.as_str()),
        // 拒绝过的不再打扰，直接建非持久会话；反悔的入口在命令面板
        Some(false) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(authorized: Option<bool>, installed: Option<&str>) -> HostInstallStatus {
        HostInstallStatus {
            authorized,
            installed_version: installed.map(str::to_string),
            verified_durable: None,
            required_version: "0.3.0".into(),
        }
    }

    #[test]
    fn asks_once_and_redeploys_on_version_change() {
        assert!(needs_host_setup(&status(None, None)));
        assert!(needs_host_setup(&status(None, Some("0.3.0"))), "授权没问过，版本对也得先问");
        assert!(needs_host_setup(&status(Some(true), None)));
        assert!(needs_host_setup(&status(Some(true), Some("0.2.9"))), "版本不符：看着进度推新版");
        assert!(!needs_host_setup(&status(Some(true), Some("0.3.0"))));
        assert!(!needs_host_setup(&status(Some(false), None)), "拒绝过的不再打扰");
    }
}
