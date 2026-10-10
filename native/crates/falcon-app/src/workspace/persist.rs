//! 工作区落盘：web 的 `falcon.workspace`，形状逐字一致，按服务端配置各存一份
//! （`<数据目录>/servers/<配置 id>/workspace.json`）。读入清洗与写出规则都在 falcon-core
//! （与 web 同一份逻辑）：只存终端窗口，pending 与文件 / 差异窗口活不过重启。
//!
//! 浏览器版就存在 localStorage 的 `falcon.workspace` 里——与 web 同一个键，老用户的排布原样接上。

use falcon_core::workspace::{PersistedWorkspace, WorkspaceState, load_workspace, serialize_workspace};

use crate::profiles::ServerProfile;

#[cfg(target_family = "wasm")]
const KEY: &str = "falcon.workspace";

#[cfg(target_family = "wasm")]
fn read(_profile: &ServerProfile) -> Option<String> {
    crate::prefs::local_storage()?.get_item(KEY).ok().flatten()
}

#[cfg(target_family = "wasm")]
fn write(_profile: &ServerProfile, json: &str) {
    if let Some(storage) = crate::prefs::local_storage()
        && let Err(err) = storage.set_item(KEY, json)
    {
        log::warn!("工作区写入 localStorage 失败：{err:?}");
    }
}

#[cfg(not(target_family = "wasm"))]
fn path(profile: &ServerProfile) -> std::path::PathBuf {
    profile.state_dir().join("workspace.json")
}

#[cfg(not(target_family = "wasm"))]
fn read(profile: &ServerProfile) -> Option<String> {
    std::fs::read_to_string(path(profile)).ok()
}

#[cfg(not(target_family = "wasm"))]
fn write(profile: &ServerProfile, json: &str) {
    if let Err(err) = crate::prefs::write_atomic(&path(profile), json.as_bytes()) {
        log::warn!("工作区写盘失败：{err}");
    }
}

pub fn load(profile: &ServerProfile) -> PersistedWorkspace {
    load_workspace(read(profile).as_deref())
}

pub fn save(profile: &ServerProfile, state: &WorkspaceState) {
    let json = serialize_workspace(&state.persisted());
    // 内容相同跳过（web 的"只在松手时写、内容相同跳过"）
    if read(profile).is_some_and(|old| old == json) {
        return;
    }
    write(profile, &json);
}
