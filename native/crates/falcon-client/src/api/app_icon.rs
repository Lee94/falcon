//! 应用图标（ADR 0018）：服务端级的选择、自定义图标的上传 / 删除 / 取图。

use std::future::Future;

use bytes::Bytes;
use falcon_proto::AppIconState;
use reqwest::Method;
use serde::Serialize;

use crate::client::{FalconClient, Req, ReqBody, json_body};
use crate::error::{ApiError, ApiResult};

impl FalconClient {
    /// `GET /api/app-icon`
    pub fn app_icon(&self) -> impl Future<Output = ApiResult<AppIconState>> + Send + 'static {
        self.get("/api/app-icon".to_owned())
    }

    /// `PUT /api/app-icon`：选内置图标，或选回已上传的自定义图标（`"custom"`）。
    /// 未知 id、还没传过图就选自定义是 400。
    pub fn set_app_icon(&self, selected: &str) -> impl Future<Output = ApiResult<AppIconState>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            selected: &'a str,
        }
        self.json(Method::PUT, "/api/app-icon".to_owned(), json_body(&Body { selected }))
    }

    /// `PUT /api/app-icon/custom`：上传规整好的正方形 PNG（服务端不解码，只验 PNG 头与边长），
    /// 上传即选中。
    pub fn upload_app_icon(&self, png: impl Into<Bytes>) -> impl Future<Output = ApiResult<AppIconState>> + Send + 'static {
        let body = ReqBody::Raw { data: png.into(), content_type: "image/png".to_owned() };
        self.call(move |inner| async move {
            inner.fetch_json(&Req::new(Method::PUT, "/api/app-icon/custom".to_owned(), body)).await
        })
    }

    /// `DELETE /api/app-icon/custom`：正选着它就回默认图标
    pub fn remove_custom_app_icon(&self) -> impl Future<Output = ApiResult<AppIconState>> + Send + 'static {
        self.bare(Method::DELETE, "/api/app-icon/custom".to_owned())
    }

    /// `GET /api/app-icon/custom.png?v=`：自定义图标的 PNG 字节
    pub fn custom_app_icon(&self, version: &str) -> impl Future<Output = ApiResult<Bytes>> + Send + 'static {
        let path = custom_icon_path(version);
        self.call(move |inner| async move {
            let resp = inner.execute(&Req::new(Method::GET, path, ReqBody::Empty)).await?;
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map_err(|e| ApiError::network(&e))?;
            if !(200..300).contains(&status) {
                return Err(ApiError::http(status, &bytes));
            }
            Ok(bytes)
        })
    }
}

/// 与 shared 的 `customIconUrl` 同形（falcon-client 不依赖 falcon-core，就地拼）
fn custom_icon_path(version: &str) -> String {
    format!("/api/app-icon/custom.png?v={}", super::seg(version))
}
