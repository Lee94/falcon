//! Android 客户端内置 Tailscale 节点 JNI 桥接（ADR 0030 决定三 / 六）。
//!
//! 懒加载约定：只有显式调用 [`start`] 时才触发 dlopen Go 动态库。
//! 开关未打开时，`status` / `proxy` / `stop` 都直接返回，不会拉起 Go 运行时；
//! `set_interfaces` 会缓存最新网卡与 DNS，待 `start` 加载库后再第一时间同步给 Go。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use falcon_tailnet::{InterfaceList, LoadError, StartConfig, Status, Tailnet};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

static LOADED_TAILNET: OnceLock<&'static Tailnet> = OnceLock::new();
static LOAD_LOCK: Mutex<()> = Mutex::new(());
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
    /// Kotlin 侧经 `BaseDexClassLoader.findLibrary` 或 APK 条目解析出的确切路径
    /// （`extractNativeLibs=false` 下形如 `/data/app/.../base.apk!/lib/arm64-v8a/libfalcon_tailnet.so`）。
    #[serde(default)]
    library_path: Option<String>,
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

fn load_tailnet(library_path: Option<&str>) -> Result<&'static Tailnet, LoadError> {
    if let Some(&tn) = LOADED_TAILNET.get() {
        return Ok(tn);
    }
    let _guard = LOAD_LOCK.lock();
    if let Some(&tn) = LOADED_TAILNET.get() {
        return Ok(tn);
    }
    let trimmed = library_path.map(str::trim).filter(|s| !s.is_empty());
    let tn = match trimmed {
        // 安卓启用 useLegacyPackaging=false（extractNativeLibs=false）时，.so 留在 APK 内而不解压到
        // nativeLibraryDir；不能先做 Path::is_file() 判断。交给 Tailnet::open 直接 dlopen 确切路径，
        // 若 dlopen 失败则保留原始 dlerror，避免被误归类为 NotFound（「此构建未包含 Tailscale」）。
        Some(path) => open_explicit_path(path)?,
        None => Tailnet::global()?,
    };
    let _ = LOADED_TAILNET.set(tn);
    Ok(tn)
}

fn open_explicit_path(path: &str) -> Result<&'static Tailnet, LoadError> {
    match Tailnet::open(Path::new(path)) {
        Ok(tn) => Ok(tn),
        Err(first_err @ LoadError::Load { .. }) => {
            #[cfg(target_os = "android")]
            {
                // Kotlin 侧已先调用 System.loadLibrary("falcon_tailnet") 将库载入当前 classloader-namespace，
                // 若按 APK 内路径 dlopen 受限于个别系统实现，再按 DT_SONAME 回退一次，仍失败则返回原始错误。
                if let Ok(tn) = Tailnet::open(Path::new("libfalcon_tailnet.so")) {
                    return Ok(tn);
                }
            }
            Err(first_err)
        }
        Err(err) => Err(err),
    }
}

pub fn start(cfg_json: &str) -> String {
    let input: StartInput = match serde_json::from_str(cfg_json) {
        Ok(v) => v,
        Err(e) => return OpResult::err(format!("配置格式错误：{e}"), false),
    };
    let tn = match load_tailnet(input.library_path.as_deref()) {
        Ok(tn) => tn,
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
    let Some(&tn) = LOADED_TAILNET.get() else {
        return serde_json::to_string(&Status::stopped()).unwrap_or_default();
    };
    let st = tn.status().unwrap_or_else(|e| Status {
        state: falcon_tailnet::State::Error,
        error: Some(e.to_string()),
        ..Status::stopped()
    });
    serde_json::to_string(&st).unwrap_or_default()
}

pub fn proxy() -> String {
    let Some(&tn) = LOADED_TAILNET.get() else {
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
    if let Some(&tn) = LOADED_TAILNET.get() {
        tn.stop();
    }
    OpResult::ok()
}

pub fn logout(state_dir: &str) -> String {
    let dir = PathBuf::from(state_dir);
    if let Some(&tn) = LOADED_TAILNET.get() {
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
    let Some(&tn) = LOADED_TAILNET.get() else {
        return OpResult::ok();
    };
    match tn.set_interfaces(&list) {
        Ok(()) => OpResult::ok(),
        Err(err) => OpResult::err(err.to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_library_path_reports_load_error_not_missing() {
        let cfg = serde_json::json!({
            "stateDir": "/tmp/falcon-tailnet-test",
            "hostname": "falcon-test",
            "libraryPath": "/nonexistent/base.apk!/lib/arm64-v8a/libfalcon_tailnet.so"
        })
        .to_string();
        let raw = start(&cfg);
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["ok"], false);
        assert!(
            parsed.get("missing").is_none(),
            "显式传入已探测库路径后，dlopen 失败不应误报为 missing：{raw}"
        );
        let err = parsed["error"].as_str().unwrap_or("");
        assert!(err.contains("libfalcon_tailnet.so"), "应保留具体加载错误：{raw}");
    }

    #[test]
    fn idle_queries_stay_stopped_before_load() {
        let st: Status = serde_json::from_str(&status()).unwrap();
        assert_eq!(st.state, falcon_tailnet::State::Stopped);
        assert!(proxy().is_empty());
        let stop_res: serde_json::Value = serde_json::from_str(&stop()).unwrap();
        assert_eq!(stop_res["ok"], true);
    }
}
