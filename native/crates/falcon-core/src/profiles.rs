//! 服务端配置表（`profiles.json`）的纯模型与清洗规则（ADR 0015 决定六）。
//!
//! - `"local"` 配置永远存在且不可删除，其 `url` 每次启动按平台现算（不信磁盘旧值）；
//! - 工作区状态按配置 ID 分目录存放（`servers/<id>/workspace.json`），避免跨服务端 ID 冲突。

use serde::{Deserialize, Serialize};
use web_time::{SystemTime, UNIX_EPOCH};

pub const LOCAL_PROFILE_ID: &str = "local";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub url: String,
    /// 用户确认过非回环地址走明文 HTTP（只问一次）
    #[serde(default)]
    pub plaintext_ok: bool,
}

impl ServerProfile {
    pub fn local(local_url: impl Into<String>) -> Self {
        Self {
            id: LOCAL_PROFILE_ID.to_owned(),
            name: String::new(),
            url: local_url.into(),
            plaintext_ok: true,
        }
    }

    pub fn is_local(&self) -> bool {
        self.id == LOCAL_PROFILE_ID
    }

    /// 基址是否为回环地址（`127.0.0.1` / `localhost` / `::1`）。
    pub fn is_loopback(&self) -> bool {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .is_some_and(|h| h == "127.0.0.1" || h == "localhost" || h == "::1" || h == "[::1]")
    }

    /// 非回环地址的明文 HTTP 连接。
    pub fn is_insecure(&self) -> bool {
        self.url.starts_with("http://") && !self.is_loopback()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesFile {
    #[serde(default)]
    pub profiles: Vec<ServerProfile>,
    #[serde(default)]
    pub last_connected: Option<String>,
}

impl ProfilesFile {
    /// 从 JSON 解析并用当前平台计算出的 `local_url` 刷新本机条目。
    pub fn load_with_local(raw: Option<&str>, local_url: &str) -> Self {
        let mut file: Self = raw.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
        file.ensure_local(local_url);
        file
    }

    /// 确保索引 0 或已有 `"local"` 条目存在且基址更新为最新的 `local_url`。
    pub fn ensure_local(&mut self, local_url: &str) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == LOCAL_PROFILE_ID) {
            p.url = local_url.to_owned();
            p.plaintext_ok = true;
        } else {
            self.profiles.insert(0, ServerProfile::local(local_url));
        }
    }

    pub fn get(&self, id: &str) -> Option<&ServerProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// 启动时默认打开的配置：上次成功连接且仍存在的配置，否则回退本机。
    pub fn startup_profile(&self) -> ServerProfile {
        self.last_connected
            .as_deref()
            .and_then(|id| self.get(id))
            .or_else(|| self.get(LOCAL_PROFILE_ID))
            .cloned()
            .unwrap_or_else(|| ServerProfile::local("http://127.0.0.1:4923"))
    }

    /// 记录最近连接成功的配置；返回值变化时返回 `true` 提示写盘。
    pub fn mark_connected(&mut self, id: &str) -> bool {
        if self.last_connected.as_deref() == Some(id) {
            return false;
        }
        self.last_connected = Some(id.to_owned());
        true
    }

    pub fn upsert(&mut self, profile: ServerProfile) {
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
    }

    /// 删除非本机配置；若删除了条目返回 `true`。
    pub fn remove(&mut self, id: &str) -> bool {
        if id == LOCAL_PROFILE_ID {
            return false;
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        if self.last_connected.as_deref() == Some(id) {
            self.last_connected = None;
        }
        self.profiles.len() != before
    }
}

pub fn new_profile_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("srv-{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_is_always_refreshed_and_undeletable() {
        let raw = r#"{"profiles":[{"id":"local","name":"","url":"http://127.0.0.1:4000","plaintextOk":true},{"id":"srv-1","name":"Dev","url":"https://dev.example.com"}],"lastConnected":"srv-1"}"#;
        let mut file = ProfilesFile::load_with_local(Some(raw), "http://127.0.0.1:4923");
        assert_eq!(file.get(LOCAL_PROFILE_ID).unwrap().url, "http://127.0.0.1:4923");
        assert_eq!(file.startup_profile().id, "srv-1");
        assert!(!file.remove(LOCAL_PROFILE_ID));
        assert!(file.remove("srv-1"));
        assert_eq!(file.startup_profile().id, LOCAL_PROFILE_ID);
    }

    #[test]
    fn loopback_and_insecure_checks() {
        let local = ServerProfile::local("http://127.0.0.1:4923");
        assert!(local.is_loopback());
        assert!(!local.is_insecure());

        let ipv6 = ServerProfile::local("http://[::1]:4923");
        assert!(ipv6.is_loopback());
        assert!(!ipv6.is_insecure());

        let remote_http = ServerProfile {
            id: "srv-2".into(),
            name: "LAN".into(),
            url: "http://192.168.1.20:4923".into(),
            plaintext_ok: false,
        };
        assert!(!remote_http.is_loopback());
        assert!(remote_http.is_insecure());
    }
}
