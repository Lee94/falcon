//! 给 GPUI 挂上 HTTP 客户端：`img("https://…")`（飞书项目的头像、Markdown 里的外链图片）
//! 靠它取字节。GPUI 自己不带实现（Zed 用的是它内部的 reqwest 封装），这里转给
//! falcon-client 的 [`falcon_client::fetch_external`]：同一个网络运行时、同一套 TLS，
//! 不带 falcon 的 cookie，只允许 GET / HEAD，响应体有上限。

use std::sync::Arc;

use futures::future::BoxFuture;
use gpui_kit::http_client::http::{HeaderValue, Request, Response};
use gpui_kit::http_client::{AsyncBody, HttpClient, Url};

struct ExternalHttp;

impl HttpClient for ExternalHttp {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, req: Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
            .collect();
        let fetch = falcon_client::fetch_external(req.method().as_str(), &req.uri().to_string(), headers);
        Box::pin(async move {
            let resp = fetch.await.map_err(anyhow::Error::msg)?;
            let mut builder = Response::builder().status(resp.status);
            for (name, value) in resp.headers {
                builder = builder.header(name, value);
            }
            Ok(builder.body(AsyncBody::from(resp.body))?)
        })
    }
}

pub fn install(cx: &mut gpui_kit::App) {
    cx.set_http_client(Arc::new(ExternalHttp));
}
