//! 中转：SSH 端口转发（Port Forward）与公网发布（Public Share，ADR 0014 / 0016）。
//!
//! 中转挂机器不挂项目：转发挂在已保存的 SSH Host 上，发布挂在本机或 SSH Host 上。
//! 规则持久化，运行时状态是此刻的事实：连不上、端口占用写在每条规则的
//! `state` / `error` 里，列表本身不因某条失败而报错。
//!
//! 同端口的规则可以建多条、同时只有一条生效：带 enabled 的写入会让服务端顺手停掉同端口
//! 的其它规则，写完要重拉 [`FalconClient::list_relays`]，不能只替换这一行。

use std::future::Future;

use falcon_proto::{
    OkResponse, PortForward, PortForwardInput, PortForwardPatch, PublicShare, PublicShareInput,
    PublicSharePatch, RelayList,
};
use reqwest::Method;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;

impl FalconClient {
    /// 本机与所有 SSH Host 的转发与发布，一次拉全。公网 URL 是运行时事实（进程一重启
    /// 就会变），轮询这个列表拿。
    pub fn list_relays(&self) -> impl Future<Output = ApiResult<RelayList>> + Send + 'static {
        self.get("/api/relays".to_string())
    }

    /// 主机不存在是 404。同端口已有规则不报错：新建的这条若 enabled，旧的会被停掉。
    pub fn create_forward(&self, input: &PortForwardInput) -> impl Future<Output = ApiResult<PortForward>> + Send + 'static {
        self.json(Method::POST, "/api/forwards".to_string(), json_body(input))
    }

    /// 只带要改的字段（开关按钮只发 `{ enabled }`）。
    pub fn update_forward(
        &self,
        forward_id: &str,
        patch: &PortForwardPatch,
    ) -> impl Future<Output = ApiResult<PortForward>> + Send + 'static {
        self.json(Method::PATCH, format!("/api/forwards/{}", seg(forward_id)), json_body(patch))
    }

    pub fn delete_forward(&self, forward_id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::DELETE, format!("/api/forwards/{}", seg(forward_id)))
    }

    /// 立刻返回（状态 starting）：第一次要下载 cloudflared，服务端丢到后台去做。
    /// `host_id` 缺省 = 本机。
    pub fn create_share(&self, input: &PublicShareInput) -> impl Future<Output = ApiResult<PublicShare>> + Send + 'static {
        self.json(Method::POST, "/api/shares".to_string(), json_body(input))
    }

    pub fn update_share(
        &self,
        share_id: &str,
        patch: &PublicSharePatch,
    ) -> impl Future<Output = ApiResult<PublicShare>> + Send + 'static {
        self.json(Method::PATCH, format!("/api/shares/{}", seg(share_id)), json_body(patch))
    }

    pub fn delete_share(&self, share_id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::DELETE, format!("/api/shares/{}", seg(share_id)))
    }
}
