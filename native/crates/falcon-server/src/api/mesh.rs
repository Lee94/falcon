//! `/api/mesh`：本机两种 mesh 的现状与开关。
//!
//! 密钥不走这里。auth key / setup key 在服务端进程的环境变量里，界面只能看 `configured`。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use falcon_proto::{MeshNetbirdConfig, MeshSetupKey, MeshStatus, MeshToggle};

use super::error::{ApiError, ApiResult};
use super::AppState;
use crate::mesh::Provider;

const NB_URL_KEY: &str = "mesh.netbird.management_url";
/// setup key 的密文。明文只在 MeshHost 的内存里，库里是 SecretBox 的输出。
const NB_SETUP_KEY: &str = "mesh.netbird.setup_key";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/mesh", get(status))
        // 具体路径放在 {provider} 前面，不然 "netbird" 会被当成 provider、"config" 落进 404
        .route("/api/mesh/netbird/config", put(put_netbird))
        .route("/api/mesh/netbird/setup-key", put(put_setup_key))
        .route("/api/mesh/{provider}", post(toggle))
}

fn with_url(state: &AppState) -> MeshStatus {
    let mut st = state.mesh.status();
    st.netbird_management_url = state.mesh.netbird_url();
    st
}

async fn status(State(state): State<AppState>) -> Json<MeshStatus> {
    Json(with_url(&state))
}

async fn toggle(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<MeshToggle>,
) -> ApiResult<Json<MeshStatus>> {
    let Some(provider) = Provider::parse(&provider) else {
        return Err(ApiError::bad_request(format!("不认识的 mesh：{provider}")));
    };
    state.mesh.set_enabled(provider, body.enabled);
    Ok(Json(with_url(&state)))
}

/// 改 NetBird 的管理地址。空串清掉，用官方的。
/// 地址真的变了、并且 sidecar 正在跑：停掉再拉起，让它吃到新地址。没开着就只存。
async fn put_netbird(State(state): State<AppState>, Json(body): Json<MeshNetbirdConfig>) -> ApiResult<Json<MeshStatus>> {
    let url = body.management_url.trim().to_string();
    if !url.is_empty() && !valid_management_url(&url) {
        return Err(ApiError::bad_request("管理地址要是 http:// 或 https:// 开头的 URL"));
    }
    let changed = url != state.mesh.netbird_url();
    let restart = changed && state.mesh.netbird_running();
    state.db.set_setting(NB_URL_KEY, &url);
    state.mesh.set_netbird_url(url);
    if restart {
        // set_enabled(true) 自己会先停掉上一份，两份不会抢同一个状态目录
        state.mesh.set_enabled(Provider::Netbird, true);
    }
    let mut st = with_url(&state);
    st.restarted = restart;
    Ok(Json(st))
}

/// 改 setup key。缺省 = 不改；空串 = 清掉。改了且 sidecar 正在跑，就重新拉起。
async fn put_setup_key(State(state): State<AppState>, Json(body): Json<MeshSetupKey>) -> ApiResult<Json<MeshStatus>> {
    let Some(key) = body.setup_key else {
        return Ok(Json(with_url(&state)));
    };
    let key = key.trim().to_string();
    let restart = state.mesh.netbird_running();
    if key.is_empty() {
        state.db.set_setting(NB_SETUP_KEY, "");
        state.mesh.set_netbird_key(String::new());
    } else {
        state.db.set_setting(NB_SETUP_KEY, &state.secrets.encrypt(&key));
        state.mesh.set_netbird_key(key);
    }
    if restart {
        state.mesh.set_enabled(Provider::Netbird, true);
    }
    let mut st = with_url(&state);
    st.restarted = restart;
    Ok(Json(st))
}

fn valid_management_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if scheme != "https" && scheme != "http" {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !host.contains(' ')
}

/// 启动时把库里的管理地址灌进 MeshHost。main 在建完 AppState 后调一次。
pub fn load_stored(state: &AppState) {
    if let Some(url) = state.db.get_setting(NB_URL_KEY) {
        state.mesh.set_netbird_url(url);
    }
    if let Some(enc) = state.db.get_setting(NB_SETUP_KEY).filter(|s| !s.is_empty()) {
        match state.secrets.decrypt(&enc) {
            Ok(key) => state.mesh.set_netbird_key(key),
            Err(e) => log::warn!("mesh 的 setup key 解不开，当没设：{e}"),
        }
    }
}
