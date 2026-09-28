//! Zellij 安装与宿主机持久能力（ADR 0001）。
//!
//! 安装走专门的 WS 通道（`/ws/install/:projectId`，消息见 [`crate::ws`]）：安装是
//! 主机级操作、可能耗时数十秒，不适合塞进创建会话的 REST 请求里。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// 安装阶段。宿主机自己下载，后端拿不到字节数，因此只有阶段没有百分比。
    ///
    /// Rust 侧加了 `Unknown` 兜底：阶段只用于显示，新服务端多一个阶段时老客户端
    /// 显示个笼统的"安装中"就行，不该让整条进度消息丢掉。
    pub enum ZellijInstallStage open {
        Probing => "probing",
        Downloading => "downloading",
        Extracting => "extracting",
        Verifying => "verifying",
    }
}

wire_enum! {
    /// Zellij 安装失败的原因。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，服务端会随新的环境故障加值。
    pub enum ZellijInstallFailure open {
        ArchUnsupported => "arch-unsupported",
        NoDownloader => "no-downloader",
        NoTar => "no-tar",
        DirNotWritable => "dir-not-writable",
        /// 探测宿主机失败：命令根本没跑通（SSH 抖动），或输出认不出是哪种系统
        ProbeFailed => "probe-failed",
        DownloadFailed => "download-failed",
        ExtractFailed => "extract-failed",
        /// 跑不起来：noexec 挂载、或 Windows ARM 的 x64 模拟不可用
        VerifyFailed => "verify-failed",
        Cancelled => "cancelled",
    }
}

/// 后端会自动重试的失败：都属于"再来一次可能就好了"的瞬时故障——
/// 网络抖动、镜像 5xx、SSH 通道半路断开。
///
/// verify-failed 不在其中：能走到验证说明包已完整解压（tar/zip 自带校验，
/// 截断的包在解压阶段就报错了），此时跑不起来的原因是 noexec 挂载或架构不兼容，
/// 重试只是把同一个错误再犯两遍，白白多下 14 MB。缺 curl / 缺 tar /
/// 目录不可写 / 架构无构建同理，都是稳定的环境事实，得先改环境。
pub const AUTO_RETRY_FAILURES: &[ZellijInstallFailure] = &[
    ZellijInstallFailure::ProbeFailed,
    ZellijInstallFailure::DownloadFailed,
    ZellijInstallFailure::ExtractFailed,
];

/// TS 的 `isAutoRetryable`。
pub fn is_auto_retryable(reason: ZellijInstallFailure) -> bool {
    AUTO_RETRY_FAILURES.contains(&reason)
}

/// 手动重试是否有意义（TS 的 `canRetryInstall`）。
///
/// 比自动重试宽得多：自动重试问的是"同样的环境再来一次能不能成"，手动重试问的是
/// "用户去远端装了 curl / 改了 noexec 挂载 / 连上了 VPN 之后能不能成"——
/// 除了架构没有官方构建和 Windows Job Object 这两个改不掉的事实，其余都值得给按钮。
/// `Unknown` 也给按钮：认不出的原因，按"可能改得掉"处理。
pub fn can_retry_install(reason: Option<NonDurableReason>) -> bool {
    !matches!(
        reason,
        Some(NonDurableReason::ArchUnsupported | NonDurableReason::WindowsJobObject)
    )
}

wire_enum! {
    /// 会话为什么不持久。UI 据此给出具体说明，而不是笼统的"非持久"——
    /// 用户需要知道该去查什么。
    ///
    /// TS 里是 `ZellijInstallFailure | "not-authorized" | "windows-job-object"`；
    /// Rust 侧摊平成一个枚举（serde 的 untagged 套 `Unknown` 兜底会有歧义），与
    /// [`ZellijInstallFailure`] 之间用 `From` / [`NonDurableReason::as_install_failure`] 互转。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类；而且它挂在每条 Session 上，一个新值
    /// 不该让整张会话列表解不出来。
    pub enum NonDurableReason open {
        ArchUnsupported => "arch-unsupported",
        NoDownloader => "no-downloader",
        NoTar => "no-tar",
        DirNotWritable => "dir-not-writable",
        /// 探测宿主机失败：命令根本没跑通（SSH 抖动），或输出认不出是哪种系统
        ProbeFailed => "probe-failed",
        DownloadFailed => "download-failed",
        ExtractFailed => "extract-failed",
        /// 跑不起来：noexec 挂载、或 Windows ARM 的 x64 模拟不可用
        VerifyFailed => "verify-failed",
        Cancelled => "cancelled",
        /// 用户拒绝在该主机安装 Zellij
        NotAuthorized => "not-authorized",
        /// 后端进程处于 Windows Job Object 中，Zellij server 会被连坐杀掉。
        /// 与其静默失效，不如诚实标注非持久。
        WindowsJobObject => "windows-job-object",
    }
}

impl From<ZellijInstallFailure> for NonDurableReason {
    fn from(f: ZellijInstallFailure) -> Self {
        match f {
            ZellijInstallFailure::ArchUnsupported => NonDurableReason::ArchUnsupported,
            ZellijInstallFailure::NoDownloader => NonDurableReason::NoDownloader,
            ZellijInstallFailure::NoTar => NonDurableReason::NoTar,
            ZellijInstallFailure::DirNotWritable => NonDurableReason::DirNotWritable,
            ZellijInstallFailure::ProbeFailed => NonDurableReason::ProbeFailed,
            ZellijInstallFailure::DownloadFailed => NonDurableReason::DownloadFailed,
            ZellijInstallFailure::ExtractFailed => NonDurableReason::ExtractFailed,
            ZellijInstallFailure::VerifyFailed => NonDurableReason::VerifyFailed,
            ZellijInstallFailure::Cancelled => NonDurableReason::Cancelled,
            ZellijInstallFailure::Unknown => NonDurableReason::Unknown,
        }
    }
}

impl NonDurableReason {
    /// 属于安装本身失败的那部分原因；not-authorized / windows-job-object / Unknown 为 `None`。
    pub fn as_install_failure(self) -> Option<ZellijInstallFailure> {
        ZellijInstallFailure::from_wire(self.as_str())
    }
}

/// 某台宿主机上的 Zellij 状态（`GET /api/projects/:id/host`，仅 SSH 项目）。
/// 授权按主机记（host+port+username），不按项目：同一台机器上的第二个项目
/// 不该再问一遍，二进制本来就已经装好了。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostZellijStatus {
    /// null = 还没问过用户
    #[serde(default)]
    pub authorized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// 该主机的下载源；未设置时用官方地址
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Windows 远端的真实断线验证结果；null = 未验证
    #[serde(default)]
    pub verified_durable: Option<bool>,
    /// 官方默认下载源，供 UI 显示占位
    pub default_base_url: String,
    /// falcon 锁定的 Zellij 版本
    pub required_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn retry_rules() {
        assert!(is_auto_retryable(ZellijInstallFailure::DownloadFailed));
        assert!(!is_auto_retryable(ZellijInstallFailure::VerifyFailed));
        assert!(!is_auto_retryable(ZellijInstallFailure::Cancelled));
        assert!(can_retry_install(None));
        assert!(can_retry_install(Some(NonDurableReason::NoTar)));
        assert!(can_retry_install(Some(NonDurableReason::Unknown)));
        assert!(!can_retry_install(Some(NonDurableReason::ArchUnsupported)));
        assert!(!can_retry_install(Some(NonDurableReason::WindowsJobObject)));
    }

    #[test]
    fn non_durable_is_superset_of_install_failure() {
        // 每个安装失败原因都能无损转成 NonDurableReason，且字面量一致
        for f in ZellijInstallFailure::ALL {
            let n = NonDurableReason::from(*f);
            assert_eq!(n.as_str(), f.as_str());
            assert_eq!(n.as_install_failure(), Some(*f));
        }
        assert_eq!(NonDurableReason::ALL.len(), ZellijInstallFailure::ALL.len() + 2);
        assert_eq!(NonDurableReason::NotAuthorized.as_install_failure(), None);
        for n in NonDurableReason::ALL {
            let json = format!("\"{}\"", n.as_str());
            assert_eq!(roundtrip::<NonDurableReason>(&json), *n);
        }
    }

    #[test]
    fn stage_literals() {
        for s in ZellijInstallStage::ALL {
            let json = format!("\"{}\"", s.as_str());
            assert_eq!(roundtrip::<ZellijInstallStage>(&json), *s);
        }
    }

    #[test]
    fn host_status() {
        let h = roundtrip::<HostZellijStatus>(
            r#"{"authorized":null,"verifiedDurable":null,
                "defaultBaseUrl":"https://github.com/zellij-org/zellij/releases/download","requiredVersion":"0.44.0"}"#,
        );
        assert_eq!(h.authorized, None);
        let h = roundtrip::<HostZellijStatus>(
            r#"{"authorized":true,"installedVersion":"0.44.0","baseUrl":"https://mirror.example/zellij",
                "verifiedDurable":true,"defaultBaseUrl":"https://github.com/zellij-org/zellij/releases/download",
                "requiredVersion":"0.44.0"}"#,
        );
        assert_eq!(h.verified_durable, Some(true));
    }
}
