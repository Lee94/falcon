//! 已保存的远端主机（SSH Host，`/api/hosts`）。

use std::future::Future;

use falcon_proto::{OkResponse, SshHost, SshHostInput, SshProbeResult};
use reqwest::Method;
use serde::Serialize;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;

impl FalconClient {
    pub fn list_hosts(&self) -> impl Future<Output = ApiResult<Vec<SshHost>>> + Send + 'static {
        self.get("/api/hosts".to_owned())
    }

    /// 同名主机是 409。
    pub fn create_host(&self, input: &SshHostInput) -> impl Future<Output = ApiResult<SshHost>> + Send + 'static {
        self.json(Method::POST, "/api/hosts".to_owned(), json_body(input))
    }

    /// 连接配置会刷回引用它的项目；`secret` 留空沿用已保存的。
    pub fn update_host(
        &self,
        id: &str,
        input: &SshHostInput,
    ) -> impl Future<Output = ApiResult<SshHost>> + Send + 'static {
        self.json(Method::PUT, format!("/api/hosts/{}", seg(id)), json_body(input))
    }

    /// 还有项目在用这台主机时是 409。
    pub fn delete_host(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::DELETE, format!("/api/hosts/{}", seg(id)))
    }

    /// `POST /api/hosts/:id/test`：试连一台已保存主机。连不上是 200 + `ok: false`。
    pub fn test_host(&self, id: &str) -> impl Future<Output = ApiResult<SshProbeResult>> + Send + 'static {
        self.bare(Method::POST, format!("/api/hosts/{}/test", seg(id)))
    }

    /// `POST /api/hosts/test`：试连表单里还没保存（或正在改）的凭据。编辑已有主机时
    /// 带 `host_id`，空的 secret / keyPath 沿用已保存的。
    pub fn test_host_draft(
        &self,
        input: &SshHostInput,
        host_id: Option<&str>,
    ) -> impl Future<Output = ApiResult<SshProbeResult>> + Send + 'static {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Draft<'a> {
            #[serde(flatten)]
            input: &'a SshHostInput,
            #[serde(skip_serializing_if = "Option::is_none")]
            host_id: Option<&'a str>,
        }
        self.json(Method::POST, "/api/hosts/test".to_owned(), json_body(&Draft { input, host_id }))
    }
}
