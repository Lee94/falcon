//! Android 客户端内置 Tailscale 节点 JNI 桥接（ADR 0030 决定三 / 六）。
//!
//! 懒加载约定：只有显式调用 [`start`] 时才触发 `Tailnet::global()`（dlopen Go 动态库）。
//! 开关未打开时，`status` / `proxy` / `stop` 都直接返回，不会拉起 Go 运行时；
//! `set_interfaces` 会缓存最新网卡与 DNS，待 `start` 加载库后再第一时间同步给 Go。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use falcon_tailnet::{InterfaceList, LoadError, StartConfig, Status, Tailnet};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

static LOADED: AtomicBool = AtomicBool::new(false);
static PENDING_INTERFACES: Mutex<Option<InterfaceList>> = Mutex::new(None);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartInput {
    state_dir: String,
    hostname: String,
    #[serde(default)]
    control_url: Option<String>,
    #[serde(default)]
    auth_key: Option<String>,
}

#[derive(Serialize)]
struct OpResult {
    ok: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl OpResult {
    fn ok() -> String {
        serde_json::to_string(&Self { ok: true, missing: false, error: None })
            .unwrap_or_else(|_| "{\"ok\":true}".to_owned())
    }

    fn err(msg: impl Into<String>, missing: bool) -> String {
        serde_json::to_string(&Self { ok: false, missing, error: Some(msg.into()) })
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"序列化失败\"}".to_owned())
    }
}

pub fn start(cfg_json: &str) -> String {
    let input: StartInput = match serde_json::from_str(cfg_json) {
        Ok(v) => v,
        Err(e) => return OpResult::err(format!("配置格式错误：{e}"), false),
    };
    let tn = match Tailnet::global() {
        Ok(tn) => {
            LOADED.store(true, Ordering::Release);
            tn
        }
        Err(LoadError::NotFound { .. }) => {
            return OpResult::err("此构建未包含 Tailscale", true);
        }
        Err(err) => {
            return OpResult::err(err.to_string(), false);
        }
    };

    if let Some(ifaces) = PENDING_INTERFACES.lock().clone() {
        let _ = tn.set_interfaces(&ifaces);
    }

    let cfg = StartConfig {
        state_dir: PathBuf::from(input.state_dir),
        hostname: input.hostname,
        control_url: input.control_url.filter(|s| !s.trim().is_empty()),
        auth_key: input.auth_key.filter(|s| !s.trim().is_empty()),
    };
    match tn.start(&cfg) {
        Ok(()) => OpResult::ok(),
        Err(err) => OpResult::err(err.to_string(), false),
    }
}

pub fn status() -> String {
    if !LOADED.load(Ordering::Acquire) {
        return serde_json::to_string(&Status::stopped()).unwrap_or_default();
    }
    let st = match Tailnet::global() {
        Ok(tn) => tn.status().unwrap_or_else(|e| Status {
            state: falcon_tailnet::State::Error,
            error: Some(e.to_string()),
            ..Status::stopped()
        }),
        Err(e) => Status {
            state: falcon_tailnet::State::Error,
            error: Some(e.to_string()),
            ..Status::stopped()
        },
    };
    serde_json::to_string(&st).unwrap_or_default()
}

pub fn proxy() -> String {
    if !LOADED.load(Ordering::Acquire) {
        return String::new();
    }
    let Ok(tn) = Tailnet::global() else {
        return String::new();
    };
    let Ok(Some(p)) = tn.proxy() else {
        return String::new();
    };
    serde_json::json!({
        "addr": p.addr,
        "user": p.user,
        "pass": p.pass,
    })
    .to_string()
}

pub fn stop() -> String {
    if LOADED.load(Ordering::Acquire)
        && let Ok(tn) = Tailnet::global()
    {
        tn.stop();
    }
    OpResult::ok()
}

pub fn logout(state_dir: &str) -> String {
    let dir = PathBuf::from(state_dir);
    if LOADED.load(Ordering::Acquire)
        && let Ok(tn) = Tailnet::global()
    {
        return match tn.logout(&dir) {
            Ok(()) => OpResult::ok(),
            Err(err) => OpResult::err(err.to_string(), false),
        };
    }
    if dir.exists()
        && let Err(err) = std::fs::remove_dir_all(&dir)
    {
        return OpResult::err(err.to_string(), false);
    }
    OpResult::ok()
}

pub fn set_interfaces(json: &str) -> String {
    let list: InterfaceList = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return OpResult::err(format!("网卡列表格式错误：{e}"), false),
    };
    *PENDING_INTERFACES.lock() = Some(list.clone());
    if !LOADED.load(Ordering::Acquire) {
        return OpResult::ok();
    }
    match Tailnet::global() {
        Ok(tn) => match tn.set_interfaces(&list) {
            Ok(()) => OpResult::ok(),
            Err(err) => OpResult::err(err.to_string(), false),
        },
        Err(_) => OpResult::ok(),
    }
}
