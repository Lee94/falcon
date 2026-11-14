//! 原生宿主系统能力实现（非 `wasm32` 目标共用，零 GPUI 依赖）。

pub mod android_migration;
pub mod app_link;
pub mod app_update;
pub mod chime;
pub mod dock;
pub mod local_service;
pub mod menu_identity;
pub mod passwords;
pub mod platform;

use std::sync::{Arc, Once};

static INSTALL_HOST: Once = Once::new();

/// 在真实宿主进程启动时注册 [`platform::HostPlatform`]（幂等）。
pub fn install_once() {
    INSTALL_HOST.call_once(|| {
        falcon_platform::install_shared(Arc::new(platform::HostPlatform::new()));
    });
}
