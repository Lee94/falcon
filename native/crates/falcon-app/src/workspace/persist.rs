//! 工作区落盘：web 的 `falcon.workspace`，形状逐字一致，按服务端配置各存一份
//! （`<数据目录>/servers/<配置 id>/workspace.json`）。读入清洗与写出规则都在 falcon-core
//! （与 web 同一份逻辑）：只存终端窗口，pending 与文件 / 差异窗口活不过重启。

use falcon_core::workspace::{PersistedWorkspace, WorkspaceState, load_workspace, serialize_workspace};

use crate::prefs::write_atomic;
use crate::profiles::ServerProfile;

fn path(profile: &ServerProfile) -> std::path::PathBuf {
    profile.state_dir().join("workspace.json")
}

pub fn load(profile: &ServerProfile) -> PersistedWorkspace {
    let raw = std::fs::read_to_string(path(profile)).ok();
    load_workspace(raw.as_deref())
}

pub fn save(profile: &ServerProfile, state: &WorkspaceState) {
    let json = serialize_workspace(&state.persisted());
    let target = path(profile);
    // 内容相同跳过（web 的"只在松手时写、内容相同跳过"）
    if std::fs::read_to_string(&target).is_ok_and(|old| old == json) {
        return;
    }
    if let Err(err) = write_atomic(&target, json.as_bytes()) {
        log::warn!("工作区写盘失败：{err}");
    }
}
