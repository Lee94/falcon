//! 宿主机节点的公开身份与连接状态。节点令牌不属于公开协议。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub node_id: String,
    pub version: String,
    pub protocol_version: u32,
    pub platform: String,
    pub arch: String,
    pub data_dir: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub node_id: Option<String>,
    pub host_id: String,
    pub name: String,
    pub version: Option<String>,
    pub online: bool,
    pub error: Option<String>,
}
