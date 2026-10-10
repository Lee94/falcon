//! 工作区落盘：web 的 `falcon.workspace`，形状逐字一致，按服务端配置各存一份。读入清洗与写出
//! 规则都在 falcon-core（与 web 同一份逻辑）：只存终端窗口，pending 与文件 / 差异窗口活不过重启。
//!
//! 存哪儿归平台（[`falcon_platform::Platform::load_workspace`]）：原生是
//! `<数据目录>/servers/<配置 id>/workspace.json`，浏览器是 localStorage 的 `falcon.workspace`——
//! 与 web 同一个键，老用户的排布原样接上。

use falcon_core::workspace::{PersistedWorkspace, WorkspaceState, load_workspace, serialize_workspace};
use falcon_platform::Platform;

use crate::profiles::ServerProfile;

pub fn load(platform: &dyn Platform, profile: &ServerProfile) -> PersistedWorkspace {
    load_workspace(platform.load_workspace(&profile.id).as_deref())
}

pub fn save(platform: &dyn Platform, profile: &ServerProfile, state: &WorkspaceState) {
    let json = serialize_workspace(&state.persisted());
    // 内容相同跳过（web 的"只在松手时写、内容相同跳过"）
    if platform.load_workspace(&profile.id).is_some_and(|old| old == json) {
        return;
    }
    platform.store_workspace(&profile.id, &json);
}
