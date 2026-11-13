//! 当前连接的 Falcon 服务端升级协议；节点网关可以把同一接口转发给目标节点。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpdateCheck {
    pub current_version: String,
    #[serde(default)]
    pub latest_version: Option<String>,
    pub available: bool,
    pub platform: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerUpdatePhase {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Installing,
    Restarting,
    Succeeded,
    Failed,
    #[serde(other)]
    Unknown,
}

impl ServerUpdatePhase {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Downloading | Self::Verifying | Self::Installing | Self::Restarting)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpdateTask {
    #[serde(default)]
    pub phase: ServerUpdatePhase,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpdateInput {
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_contract_uses_camel_case_and_nullable_fields() {
        let task = ServerUpdateTask { phase: ServerUpdatePhase::Restarting, target_version: Some("1.2.3".into()), error: None };
        assert_eq!(serde_json::to_value(task).unwrap(), serde_json::json!({"phase":"restarting","targetVersion":"1.2.3","error":null}));
        assert_eq!(serde_json::from_str::<ServerUpdateTask>("{}").unwrap(), ServerUpdateTask::default());
        assert_eq!(serde_json::from_str::<ServerUpdateTask>(r#"{"phase":"future"}"#).unwrap().phase, ServerUpdatePhase::Unknown);
    }
}
