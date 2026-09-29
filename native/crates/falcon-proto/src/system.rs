//! 认证、系统信息、后端机器上的目录浏览、shell 侦测，以及几个各处通用的响应体。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;
use crate::zellij::NonDurableReason;

wire_enum! {
    /// 宿主机的两类执行环境。服务端把本地 Unix / 本地 Windows / SSH POSIX /
    /// SSH Windows 四种抹平成这两类（`zellij/host.ts`、`git/host.ts`）。
    ///
    /// shared 里没有给它起名字，`ShellsInfo.kind` 与 `SshProbeResult.kind` 各写了
    /// 一遍 `"posix" | "windows"`；Rust 侧合成一个类型。
    pub enum HostKind {
        Posix => "posix",
        Windows => "windows",
    }
}

/// `GET /api/auth/status`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// 当前绑定是否要求认证
    pub required: bool,
    pub authenticated: bool,
    pub password_set: bool,
}

/// `GET /api/system`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    /// 服务端的 `process.platform`（`darwin` / `linux` / `win32`）
    pub platform: String,
    /// 本地会话能否持久；null = 尚未探测（首次创建本地会话时才探测）
    #[serde(default)]
    pub local_durable: Option<bool>,
    /// localDurable=false 时的原因
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_durable_reason: Option<NonDurableReason>,
    pub version: String,
}

/// 目录浏览里的一项，只含文件夹（含指向目录的符号链接）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FsDirEntry {
    pub name: String,
    pub path: String,
}

/// 后端机器（或 SSH 远端）上某一层目录的列表，`GET /api/fs/list`。
///
/// 给本地项目选工作目录用：浏览器拿不到后端的真实路径，`showDirectoryPicker`
/// 也给不出服务端路径，只能后端自己列。原生客户端同理——FolderPicker 列的是
/// **宿主机**上的目录，不能换成本机的系统文件夹选择器。
///
/// `path == ""` 是 Windows 的盘符列表（虚拟层，不是真实目录）；POSIX 没有这一层，
/// 根就是 `/`。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FsListing {
    pub path: String,
    /// 上一级。POSIX 根为 null；Windows 盘符根的上一级是 ""（盘符列表）
    #[serde(default)]
    pub parent: Option<String>,
    pub home: String,
    pub roots: Vec<String>,
    pub entries: Vec<FsDirEntry>,
}

/// `POST /api/fs/validate` 的响应：后端本机上这个路径是不是一个能进的文件夹。
///
/// **shared 里没有这个类型**，照 `routes.ts` 的字面量与 web `api.ts` 的
/// `{ ok: boolean; error?: string }` 补上。不是文件夹 / 不存在同样是 200 + ok:false。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FsValidateResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 宿主机上可用 shell 的侦测结果（项目表单的 shell 选择用），`GET /api/shells`。
///
/// `default` 是不设覆盖时后端实际会用的 shell：POSIX 为探测到的登录 shell，
/// Windows 一律 PowerShell；恒等于 `shells[0]`。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellsInfo {
    pub kind: HostKind,
    pub default: String,
    /// 侦测到的可用 shell 绝对路径，去重后默认项排最前
    pub shells: Vec<String>,
}

/// 4xx / 5xx 的响应体。`error` 是一句可以直接给用户看的话。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
}

/// `{ ok: true }`：登录 / 登出 / 改密码 / 终止会话 / 删规则等写操作的成功响应。
///
/// **shared 里没有这个类型**，是 `routes.ts` 里各处字面量返回的共同形状，这里补上
/// 省得客户端每处手写；协议 fixture 会盯着它。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OkResponse {
    pub ok: bool,
}

/// 等密码的 sudo / SSH askpass 提示：WS 的 `askpass` 消息与
/// `GET /api/askpass/pending`（WS 断开时的兜底轮询）的元素是同一个形状。
///
/// **shared 里没有这个类型**：WS 那边内联在 `ServerMessage` 里，REST 那边是
/// `routes.ts` 里 `map((p) => ({ id, prompt }))` 的字面量。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AskpassPrompt {
    pub id: String,
    pub prompt: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn auth_status() {
        let s = roundtrip::<AuthStatus>(
            r#"{"required":true,"authenticated":false,"passwordSet":true}"#,
        );
        assert!(s.password_set);
    }

    #[test]
    fn system_info_probed_and_unprobed() {
        let s = roundtrip::<SystemInfo>(
            r#"{"platform":"darwin","localDurable":null,"version":"0.9.3"}"#,
        );
        assert_eq!(s.local_durable, None);
        let s = roundtrip::<SystemInfo>(
            r#"{"platform":"win32","localDurable":false,"localDurableReason":"windows-job-object","version":"0.9.3"}"#,
        );
        assert_eq!(s.local_durable_reason, Some(NonDurableReason::WindowsJobObject));
    }

    #[test]
    fn fs_listing_posix_root_and_windows_drives() {
        let root = roundtrip::<FsListing>(
            r#"{"path":"/","parent":null,"home":"/Users/fay","roots":["/"],
                "entries":[{"name":"Users","path":"/Users"},{"name":"opt","path":"/opt"}]}"#,
        );
        assert_eq!(root.parent, None);
        let drives = roundtrip::<FsListing>(
            r#"{"path":"","parent":null,"home":"C:\\Users\\fay","roots":["C:\\","D:\\"],
                "entries":[{"name":"C:","path":"C:\\"}]}"#,
        );
        assert_eq!(drives.roots.len(), 2);
        let sub = roundtrip::<FsListing>(
            r#"{"path":"C:\\","parent":"","home":"C:\\Users\\fay","roots":["C:\\"],"entries":[]}"#,
        );
        assert_eq!(sub.parent.as_deref(), Some(""));
    }

    #[test]
    fn shells_info() {
        let s = roundtrip::<ShellsInfo>(
            r#"{"kind":"posix","default":"/bin/zsh","shells":["/bin/zsh","/bin/bash","/bin/sh"]}"#,
        );
        assert_eq!(s.kind, HostKind::Posix);
        assert_eq!(s.default, s.shells[0]);
    }

    #[test]
    fn small_bodies() {
        roundtrip::<ApiError>(r#"{"error":"未认证"}"#);
        roundtrip::<OkResponse>(r#"{"ok":true}"#);
        roundtrip::<FsValidateResult>(r#"{"ok":true}"#);
        roundtrip::<FsValidateResult>(r#"{"ok":false,"error":"不是文件夹"}"#);
        roundtrip::<Vec<AskpassPrompt>>(r#"[{"id":"a1","prompt":"[sudo] password for fay:"}]"#);
    }
}

/// `GET /api/app-icon`（ADR 0018）：服务端级的应用图标选择。
///
/// `selected` 是 shared `APP_ICON_IDS` 里的一个，或 `"custom"`；认 id 的活在
/// falcon-core 的 `app_icon`，这里按字符串收，服务端加了新图标旧客户端也不至于解不开。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppIconState {
    pub selected: String,
    /// 已上传的自定义图标的版本（内容哈希前缀），None = 没有
    #[serde(default)]
    pub custom: Option<String>,
}
