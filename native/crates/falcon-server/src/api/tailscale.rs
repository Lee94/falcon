//! 从后端本机的 Tailscale 列出可添加的远端主机。
//!
//! 一律 200。没安装、没在跑、没登录写在 `available` / `reason` 里，跟主机探测同一个口径：
//! 环境事实不是 4xx。

use axum::Json;
use axum::Router;
use axum::routing::get;
use falcon_proto::TailscaleDevices;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tailscale/devices", get(devices))
}

async fn devices() -> Json<TailscaleDevices> {
    Json(crate::tailscale::list_devices().await)
}
