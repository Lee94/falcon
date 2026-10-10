//! Fastify 在进路由之前就替 Node 版做掉的几件事：按 Content-Type 选请求体解析器、
//! 认不出的类型回 415、把没读完的请求体读掉丢弃；外加 querystring 取值。
//!
//! Node 版注册过的解析器：Fastify 自带的 `application/json`、`text/plain`，routes.ts 加的
//! `image/*`（攒成 Buffer，上限 PASTE_IMAGE_MAX_BYTES）与 `application/octet-stream`（原样
//! 交出流）。这几种以外的 Content-Type、或者有请求体却没有 Content-Type，Fastify 直接
//! 回 415，路由根本不会跑。

use axum::body::Body;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING};
use axum::http::{HeaderMap, StatusCode, Uri};
use futures::{Stream, StreamExt as _};
use serde_json::json;

use super::error::ApiError;

/// 请求体会被 Fastify 交给哪个解析器
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    /// 没有 Content-Type，也没有请求体：直接进路由，`req.body` 是 undefined
    None,
    Json,
    Text,
    /// `image/*`：攒成 Buffer
    Image,
    /// 原样交出流（上传）
    OctetStream,
    /// 没有对应的解析器：Fastify 回 415
    Unsupported,
}

/// Fastify 的 isEmptyBody 取反：带 Transfer-Encoding，或 Content-Length 不是 0
fn has_body(headers: &HeaderMap) -> bool {
    headers.contains_key(TRANSFER_ENCODING) || headers.get(CONTENT_LENGTH).is_some_and(|v| v.as_bytes() != b"0")
}

pub fn media_of(headers: &HeaderMap) -> Media {
    let Some(ct) = headers.get(CONTENT_TYPE) else {
        return if has_body(headers) { Media::Unsupported } else { Media::None };
    };
    let Ok(ct) = ct.to_str() else { return Media::Unsupported };
    let essence = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match essence.as_str() {
        "application/octet-stream" => Media::OctetStream,
        "application/json" => Media::Json,
        "text/plain" => Media::Text,
        e if e.starts_with("image/") && e.len() > "image/".len() => Media::Image,
        _ => Media::Unsupported,
    }
}

/// Fastify 的 FST_ERR_CTP_INVALID_MEDIA_TYPE
pub fn unsupported_media_type() -> ApiError {
    ApiError {
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
        body: json!({
            "statusCode": 415,
            "code": "FST_ERR_CTP_INVALID_MEDIA_TYPE",
            "error": "Unsupported Media Type",
            "message": "Unsupported Media Type",
        }),
    }
}

/// Fastify 的 FST_ERR_CTP_BODY_TOO_LARGE
pub fn body_too_large() -> ApiError {
    ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        body: json!({
            "statusCode": 413,
            "code": "FST_ERR_CTP_BODY_TOO_LARGE",
            "error": "Payload Too Large",
            "message": "Request body is too large",
        }),
    }
}

/// 提前回了错误、请求体却没读完时，把剩下的读掉丢弃（Node 的 http 服务器就是这么做的）。
/// 不读的话 hyper 回完响应就断连接，浏览器还在发请求体，看到的是网络错误而不是那个 409 / 4xx
pub fn drain_in_background<S, T, E>(mut stream: S)
where
    S: Stream<Item = Result<T, E>> + Send + Unpin + 'static,
{
    tokio::spawn(async move { while let Some(Ok(_)) = stream.next().await {} });
}

/// 同 [`drain_in_background`]，拿的是还没拆开的请求体
pub fn drain_body(body: Body) {
    drain_in_background(body.into_data_stream());
}

/// querystring 里某个键的值（重复的键取第一个；Fastify 会给数组，Node 版随后在 `.trim` /
/// `.split` 上抛 TypeError，这里不复刻那条路）。`?path` 与 `?path=` 都是空串
pub fn query_param(uri: &Uri, key: &str) -> Option<String> {
    let query = uri.query()?;
    url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn media_follows_fastify_parsers() {
        assert_eq!(media_of(&headers(&[])), Media::None);
        assert_eq!(media_of(&headers(&[("content-length", "0")])), Media::None);
        assert_eq!(media_of(&headers(&[("content-length", "3")])), Media::Unsupported);
        assert_eq!(media_of(&headers(&[("transfer-encoding", "chunked")])), Media::Unsupported);
        assert_eq!(media_of(&headers(&[("content-type", "Application/Octet-Stream")])), Media::OctetStream);
        assert_eq!(media_of(&headers(&[("content-type", "application/json; charset=utf-8")])), Media::Json);
        assert_eq!(media_of(&headers(&[("content-type", "text/plain")])), Media::Text);
        assert_eq!(media_of(&headers(&[("content-type", "image/png")])), Media::Image);
        assert_eq!(media_of(&headers(&[("content-type", "multipart/form-data; boundary=x")])), Media::Unsupported);
    }

    #[test]
    fn query_first_value_and_bare_keys() {
        let uri: Uri = "/x?path=a%2Fb+c&path=z&flag&name=".parse().unwrap();
        assert_eq!(query_param(&uri, "path").as_deref(), Some("a/b c"));
        assert_eq!(query_param(&uri, "flag").as_deref(), Some(""));
        assert_eq!(query_param(&uri, "name").as_deref(), Some(""));
        assert_eq!(query_param(&uri, "nope"), None);
        assert_eq!(query_param(&"/x".parse().unwrap(), "path"), None);
    }
}
