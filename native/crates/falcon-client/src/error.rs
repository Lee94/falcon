//! 错误形状：对齐 web 的 `ApiRequestError`（`packages/web/src/api.ts`）。
//!
//! web 那边：非 2xx 时取响应体里的 `error` 字段当消息（没有就是 `HTTP <status>`），
//! 整个响应体原样挂在 `body` 上给调用方按需收窄（批量派生的 `MultiDeriveError`、
//! 飞书项目 409 的 `reason`）；网络层失败 status 为 0。Rust 侧 status 用 `Option`，
//! 网络层失败是 `None`。
//!
//! **环境事实不是错误**：git / repo / 主机探测这些端点一律 200 + `ok` / `available`
//! 字段，客户端据此渲染说明，它们永远不会变成 `ApiError`（服务端 routes.ts 的规矩）。

use std::fmt;

use falcon_proto::MeegleUnavailableReason;
use serde::Deserialize as _;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub type ApiResult<T> = Result<T, ApiError>;

/// 错误出在哪一层。消息文本是给人看的，app 要换成本地化文案时按这个分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiErrorKind {
    /// 服务端回了非 2xx。`status` 必有值，`message` 是服务端 `error` 字段的原话
    /// （服务端的话本来就是给用户看的中文）。
    Http,
    /// 连不上、连接中途断了、TLS 握手失败等。`status` 为 `None`。
    Network,
    /// 2xx 但响应体不是约定的形状：多半是服务端比客户端新 / 旧，或者反代回了个
    /// HTML 页面。`status` 是实际的 HTTP 码。
    Decode,
    /// 本机文件读写失败（下载落盘、上传读源文件）。`status` 为 `None`。
    Io,
    /// 客户端自己的问题：基址不合法、TLS 初始化失败、网络运行时已关停。
    Internal,
}

/// 一次 REST 调用的失败。
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// HTTP 状态码；网络层 / 本机 I/O 失败时为 `None`
    pub status: Option<u16>,
    /// 一句可以直接展示的话。`Http` 类是服务端 `error` 字段的原文
    pub message: String,
    /// 服务端返回的完整错误体（能解析成 JSON 时），调用方按需收窄
    pub body: Option<Value>,
    pub kind: ApiErrorKind,
}

impl ApiError {
    /// 非 2xx 响应 → 错误。响应体解析不了（反代的 HTML 错误页）时消息退回 `HTTP <码>`。
    pub(crate) fn http(status: u16, body: &[u8]) -> Self {
        let value: Option<Value> = serde_json::from_slice(body).ok();
        let message = value
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {status}"));
        ApiError { status: Some(status), message, body: value, kind: ApiErrorKind::Http }
    }

    pub(crate) fn network(err: &reqwest::Error) -> Self {
        ApiError {
            status: None,
            message: error_chain(err),
            body: None,
            kind: ApiErrorKind::Network,
        }
    }

    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub(crate) fn network_msg(message: impl Into<String>) -> Self {
        ApiError { status: None, message: message.into(), body: None, kind: ApiErrorKind::Network }
    }

    pub(crate) fn decode(status: u16, err: &serde_json::Error, body: &[u8]) -> Self {
        ApiError {
            status: Some(status),
            message: format!("响应格式不符：{err}"),
            body: serde_json::from_slice(body).ok(),
            kind: ApiErrorKind::Decode,
        }
    }

    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub(crate) fn io(context: &str, err: &std::io::Error) -> Self {
        ApiError {
            status: None,
            message: format!("{context}：{err}"),
            body: None,
            kind: ApiErrorKind::Io,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        ApiError { status: None, message: message.into(), body: None, kind: ApiErrorKind::Internal }
    }

    /// HTTP 状态码；网络层失败为 `None`。
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// 401：没登录，或登录态随服务端重启丢了。设置过重登密码时客户端已经自动重登
    /// 过一次，走到这里说明密码不对或者没设——该弹登录框了。
    pub fn is_unauthorized(&self) -> bool {
        self.status == Some(401)
    }

    /// 409：状态冲突。上传撞上同名文件（问过用户再带 overwrite 重发）、派生时分支
    /// 被占用、飞书项目 CLI 没装 / 没登录（配合 [`ApiError::meegle_reason`]）都是它。
    pub fn is_conflict(&self) -> bool {
        self.status == Some(409)
    }

    pub fn is_not_found(&self) -> bool {
        self.status == Some(404)
    }

    /// 连不上 / 断了：请求可能根本没到服务端。
    pub fn is_network(&self) -> bool {
        self.kind == ApiErrorKind::Network
    }

    /// 把错误体收窄成具体类型，如批量派生失败的 [`falcon_proto::MultiDeriveError`]。
    pub fn body_as<T: DeserializeOwned>(&self) -> Option<T> {
        self.body.as_ref().and_then(|v| T::deserialize(v).ok())
    }

    /// 飞书项目接口 409 时响应体里的 `reason`：前端据此切到安装 / 登录提示
    /// （ADR 0010：业务请求撞上 409 就重新拉 `/api/meegle/status`）。
    pub fn meegle_reason(&self) -> Option<MeegleUnavailableReason> {
        if !self.is_conflict() {
            return None;
        }
        let reason = self.body.as_ref()?.get("reason")?;
        MeegleUnavailableReason::deserialize(reason).ok()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// reqwest 的 Display 只有最外一层（"error sending request for url (…)"），真正的
/// 原因（connection refused、证书不受信任）在 source 链里。排查"为什么连不上远处的
/// 服务端"时这一段最有用，全接上。
pub(crate) fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(e) = cur {
        let s = e.to_string();
        if !out.contains(&s) {
            out.push('：');
            out.push_str(&s);
        }
        cur = e.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::MultiDeriveError;

    #[test]
    fn server_error_field_becomes_message() {
        let e = ApiError::http(409, "{\"error\":\"同名文件已存在\"}".as_bytes());
        assert_eq!(e.message, "同名文件已存在");
        assert!(e.is_conflict());
        assert_eq!(e.kind, ApiErrorKind::Http);
        assert!(e.body.is_some());
    }

    #[test]
    fn non_json_body_falls_back_to_status() {
        let e = ApiError::http(502, b"<html>Bad Gateway</html>");
        assert_eq!(e.message, "HTTP 502");
        assert_eq!(e.body, None);
        let e = ApiError::http(500, b"");
        assert_eq!(e.message, "HTTP 500");
    }

    #[test]
    fn body_narrowing() {
        let e = ApiError::http(
            502,
            r#"{"error":"派生失败","member":{"dir":"/w/web","reason":"branch-in-use"},"leftover":["/w/x"]}"#
                .as_bytes(),
        );
        let d: MultiDeriveError = e.body_as().unwrap();
        assert_eq!(d.leftover.unwrap(), vec!["/w/x".to_owned()]);
    }

    #[test]
    fn meegle_reason_only_on_409() {
        let e = ApiError::http(
            409,
            r#"{"error":"未登录","reason":"not-authenticated","code":1}"#.as_bytes(),
        );
        assert_eq!(e.meegle_reason(), Some(MeegleUnavailableReason::NotAuthenticated));
        let e = ApiError::http(502, r#"{"error":"x","reason":"cli-error"}"#.as_bytes());
        assert_eq!(e.meegle_reason(), None);
    }
}
