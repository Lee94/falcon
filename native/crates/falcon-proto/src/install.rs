//! 会话守护进程的部署与宿主机持久能力（ADR 0020；ADR 0001 定下的持久会话语义照旧）。
//!
//! 部署走专门的 WS 通道（`/ws/install/:projectId`，消息见 [`crate::ws`]）：要往宿主机推一个
//! 二进制、可能耗时数秒到数十秒，不适合塞进创建会话的 REST 请求里。二进制由后端推过去
//! （不让宿主机自己下载），所以阶段只有探测、上传、校验。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// 部署阶段。
    ///
    /// Rust 侧加了 `Unknown` 兜底：阶段只用于显示，新服务端多一个阶段时老客户端
    /// 显示个笼统的"安装中"就行，不该让整条进度消息丢掉（Zellij 时代的 downloading /
    /// extracting 在新服务端上不再出现，老客户端照样认得）。
    pub enum InstallStage open {
        Probing => "probing",
        Uploading => "uploading",
        Verifying => "verifying",
    }
}

wire_enum! {
    /// 部署失败的原因。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，服务端会随新的环境故障加值。
    pub enum InstallFailure open {
        /// 宿主机的系统 / 架构没有守护进程的构建
        ArchUnsupported => "arch-unsupported",
        DirNotWritable => "dir-not-writable",
        /// 探测宿主机失败：命令根本没跑通（SSH 抖动），或输出认不出是哪种系统
        ProbeFailed => "probe-failed",
        /// 推二进制的半路断了（SSH 通道断开、磁盘满）
        UploadFailed => "upload-failed",
        /// 推上去了跑不起来：noexec 挂载、或 Windows ARM 的 x64 模拟不可用
        VerifyFailed => "verify-failed",
        Cancelled => "cancelled",
        /// 这个 falcon 构建里没有宿主机要的那份守护进程（开发构建没跑 `cargo xtask sessiond`）
        SessiondUnavailable => "sessiond-unavailable",
    }
}

/// 后端会自动重试的失败：都属于"再来一次可能就好了"的瞬时故障——SSH 抖动、
/// 通道半路断开。verify-failed 不在其中：推上去的字节数核对过了，跑不起来是 noexec 挂载或
/// 架构不兼容，重试只是把同一个错误再犯两遍。目录不可写 / 架构无构建同理，都是稳定的
/// 环境事实，得先改环境。
pub const AUTO_RETRY_FAILURES: &[InstallFailure] = &[InstallFailure::ProbeFailed, InstallFailure::UploadFailed];

pub fn is_auto_retryable(reason: InstallFailure) -> bool {
    AUTO_RETRY_FAILURES.contains(&reason)
}

/// 手动重试是否有意义。
///
/// 比自动重试宽得多：自动重试问的是"同样的环境再来一次能不能成"，手动重试问的是
/// "用户改了 noexec 挂载 / 连上了 VPN 之后能不能成"——除了架构没有构建、Windows Job Object、
/// falcon 构建里缺守护进程这几个用户在宿主机上改不掉的事实，其余都值得给按钮。
/// `Unknown` 也给按钮：认不出的原因，按"可能改得掉"处理。
pub fn can_retry_install(reason: Option<NonDurableReason>) -> bool {
    !matches!(
        reason,
        Some(NonDurableReason::ArchUnsupported | NonDurableReason::WindowsJobObject | NonDurableReason::SessiondUnavailable)
    )
}

wire_enum! {
    /// 会话为什么不持久。UI 据此给出具体说明，而不是笼统的"非持久"——
    /// 用户需要知道该去查什么。
    ///
    /// 摊平成一个枚举（serde 的 untagged 套 `Unknown` 兜底会有歧义），与
    /// [`InstallFailure`] 之间用 `From` / [`NonDurableReason::as_install_failure`] 互转。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类；而且它挂在每条 Session 上，一个新值
    /// 不该让整张会话列表解不出来（Zellij 时代留在库里的 no-tar 之类读出来就是 Unknown）。
    pub enum NonDurableReason open {
        ArchUnsupported => "arch-unsupported",
        DirNotWritable => "dir-not-writable",
        ProbeFailed => "probe-failed",
        UploadFailed => "upload-failed",
        VerifyFailed => "verify-failed",
        Cancelled => "cancelled",
        SessiondUnavailable => "sessiond-unavailable",
        /// 用户拒绝 falcon 往该主机上放东西
        NotAuthorized => "not-authorized",
        /// 后端进程处于 Windows Job Object 中，而且既不允许脱离、WMI 也拉不起守护进程：
        /// 后端一退出会话就被连坐杀掉。与其静默失效，不如诚实标注非持久。
        WindowsJobObject => "windows-job-object",
    }
}

impl From<InstallFailure> for NonDurableReason {
    fn from(f: InstallFailure) -> Self {
        match f {
            InstallFailure::ArchUnsupported => NonDurableReason::ArchUnsupported,
            InstallFailure::DirNotWritable => NonDurableReason::DirNotWritable,
            InstallFailure::ProbeFailed => NonDurableReason::ProbeFailed,
            InstallFailure::UploadFailed => NonDurableReason::UploadFailed,
            InstallFailure::VerifyFailed => NonDurableReason::VerifyFailed,
            InstallFailure::Cancelled => NonDurableReason::Cancelled,
            InstallFailure::SessiondUnavailable => NonDurableReason::SessiondUnavailable,
            InstallFailure::Unknown => NonDurableReason::Unknown,
        }
    }
}

impl NonDurableReason {
    /// 属于部署本身失败的那部分原因；not-authorized / windows-job-object / Unknown 为 `None`。
    pub fn as_install_failure(self) -> Option<InstallFailure> {
        InstallFailure::from_wire(self.as_str())
    }
}

/// 某台宿主机上的守护进程状态（`GET /api/projects/:id/host`，仅 SSH 项目）。
/// 授权按主机记（host+port+username），不按项目：同一台机器上的第二个项目
/// 不该再问一遍，二进制本来就已经在那里了。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostInstallStatus {
    /// null = 还没问过用户
    #[serde(default)]
    pub authorized: Option<bool>,
    /// 宿主机上已经有的守护进程版本
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Windows 远端的真实断线验证结果；null = 未验证
    #[serde(default)]
    pub verified_durable: Option<bool>,
    /// 这个 falcon 要推的守护进程版本
    pub required_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn retry_rules() {
        assert!(is_auto_retryable(InstallFailure::UploadFailed));
        assert!(!is_auto_retryable(InstallFailure::VerifyFailed));
        assert!(!is_auto_retryable(InstallFailure::Cancelled));
        assert!(can_retry_install(None));
        assert!(can_retry_install(Some(NonDurableReason::DirNotWritable)));
        assert!(can_retry_install(Some(NonDurableReason::Unknown)));
        assert!(!can_retry_install(Some(NonDurableReason::ArchUnsupported)));
        assert!(!can_retry_install(Some(NonDurableReason::WindowsJobObject)));
        assert!(!can_retry_install(Some(NonDurableReason::SessiondUnavailable)));
    }

    #[test]
    fn non_durable_is_superset_of_install_failure() {
        for f in InstallFailure::ALL {
            let n = NonDurableReason::from(*f);
            assert_eq!(n.as_str(), f.as_str());
            assert_eq!(n.as_install_failure(), Some(*f));
        }
        assert_eq!(NonDurableReason::ALL.len(), InstallFailure::ALL.len() + 2);
        assert_eq!(NonDurableReason::NotAuthorized.as_install_failure(), None);
        for n in NonDurableReason::ALL {
            let json = format!("\"{}\"", n.as_str());
            assert_eq!(roundtrip::<NonDurableReason>(&json), *n);
        }
        // Zellij 时代留在库里的原因读出来是 Unknown，不让整张列表解不出来
        assert_eq!(serde_json::from_str::<NonDurableReason>("\"no-tar\"").unwrap(), NonDurableReason::Unknown);
    }

    #[test]
    fn stage_literals() {
        for s in InstallStage::ALL {
            let json = format!("\"{}\"", s.as_str());
            assert_eq!(roundtrip::<InstallStage>(&json), *s);
        }
    }

    #[test]
    fn host_status() {
        let h = roundtrip::<HostInstallStatus>(r#"{"authorized":null,"verifiedDurable":null,"requiredVersion":"0.3.0"}"#);
        assert_eq!(h.authorized, None);
        let h = roundtrip::<HostInstallStatus>(
            r#"{"authorized":true,"installedVersion":"0.3.0","verifiedDurable":true,"requiredVersion":"0.3.0"}"#,
        );
        assert_eq!(h.verified_durable, Some(true));
    }
}
