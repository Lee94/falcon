//! 后端本机 Tailscale 上的设备（`GET /api/tailscale/devices`），用来添加远端主机。
//!
//! 没安装 / 没在跑 / 没登录是 200 + `available: false`，不会变成 [`ApiError`](crate::error::ApiError)。

use std::future::Future;

use falcon_proto::TailscaleDevices;

use crate::client::FalconClient;
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

impl FalconClient {
    pub fn tailscale_devices(&self) -> impl Future<Output = ApiResult<TailscaleDevices>> + MaybeSend + 'static {
        self.get("/api/tailscale/devices".to_owned())
    }
}
