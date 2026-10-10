//! 取第三方地址的字节：界面里的外链图片（飞书项目的头像、Markdown 里的 https 图片）。
//!
//! 与 [`crate::FalconClient`] 刻意分开：
//! - **不带 falcon 的 cookie**，也不走自动重登——对方是任意第三方站点，登录令牌不能出门；
//! - **走系统代理**（reqwest 默认读 `https_proxy` 等环境变量）：外链图片本来就在公网上，
//!   FalconClient 为了不劫持 127.0.0.1 才关掉代理，这里没有那个顾虑；
//! - 响应体设上限：图片不该有几十 MB，防止一个恶意链接把内存吃光。
//!
//! TLS 与 FalconClient 同一套（rustls + 系统信任库）。
//!
//! 浏览器里这条路受 CORS 限制：第三方图片站多半不给跨源读字节，取不到就是取不到。
//! 浏览器版要换成服务端代理（docs/design/rust-unification.md 附录 B），这里只保证编得过、
//! 对给了 CORS 头的站点能用。

use std::sync::OnceLock;

use crate::runtime::{MaybeSend, run};

/// 响应体上限
pub const EXTERNAL_MAX_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ExternalResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

#[cfg(target_family = "wasm")]
fn client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| reqwest::Client::builder().build().map_err(|e| format!("建外链 HTTP 客户端失败：{e}")))
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(not(target_family = "wasm"))]
fn client() -> Result<&'static reqwest::Client, String> {
    use std::time::Duration;

    use crate::runtime::runtime;
    use crate::tls;

    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let tls = tls::client_config()?;
            // hyper 的连接池在 build 时就要一个 tokio 上下文来挂后台任务
            let _guard = runtime().enter();
            reqwest::Client::builder()
                .use_preconfigured_tls((*tls).clone())
                .user_agent(concat!("falcon-native/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .map_err(|e| format!("建外链 HTTP 客户端失败：{e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// GET / HEAD 一个 http(s) 地址，整份取回。其它方法与 scheme 一律拒绝：这条路只给图片用
pub fn fetch_external(
    method: &str,
    url: &str,
    headers: Vec<(String, Vec<u8>)>,
) -> impl Future<Output = Result<ExternalResponse, String>> + MaybeSend + 'static {
    let method = method.to_ascii_uppercase();
    let url = url.to_string();
    async move {
        let method = match method.as_str() {
            "GET" => reqwest::Method::GET,
            "HEAD" => reqwest::Method::HEAD,
            other => return Err(format!("外链只允许 GET / HEAD，收到 {other}")),
        };
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(format!("外链只允许 http(s)：{url}"));
        }
        let client = client()?.clone();
        let fut = async move {
            let mut req = client.request(method, &url);
            for (name, value) in headers {
                req = req.header(name, value);
            }
            #[allow(unused_mut)]
            let mut resp = req.send().await.map_err(|e| e.to_string())?;
            if resp.content_length().is_some_and(|n| n as usize > EXTERNAL_MAX_BYTES) {
                return Err(format!("响应超过 {} MB 上限", EXTERNAL_MAX_BYTES / 1024 / 1024));
            }
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
                .collect();
            #[cfg(not(target_family = "wasm"))]
            let body = {
                let mut body = Vec::new();
                while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                    if body.len() + chunk.len() > EXTERNAL_MAX_BYTES {
                        return Err(format!("响应超过 {} MB 上限", EXTERNAL_MAX_BYTES / 1024 / 1024));
                    }
                    body.extend_from_slice(&chunk);
                }
                body
            };
            // fetch 的响应体只能整份读；上限照样查（读完之后）
            #[cfg(target_family = "wasm")]
            let body = {
                let body = resp.bytes().await.map_err(|e| e.to_string())?;
                if body.len() > EXTERNAL_MAX_BYTES {
                    return Err(format!("响应超过 {} MB 上限", EXTERNAL_MAX_BYTES / 1024 / 1024));
                }
                body.to_vec()
            };
            Ok(ExternalResponse { status, headers, body })
        };
        run(fut).await.map_err(|_| "网络运行时已关闭".to_string())?
    }
}
