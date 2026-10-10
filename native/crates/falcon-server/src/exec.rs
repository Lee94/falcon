//! 在宿主机上执行一条命令行的结果形状。移植自 `packages/server/src/zellij/install.ts` 的 `ExecResult`。
//!
//! **铁律：非零退出码是正常返回值，绝不当错误**。判错一律显式查 `code`；只有执行本身
//! 起不来（spawn 失败、链路断了）才算链路故障——那种情况 `code` 是 `None`，stderr 里是原因。
//! 执行器（本地 shell / SSH exec）在 S3 落地。

/// 一次执行的结果。stdout / stderr 是整段收完后一次按 UTF-8 解码的（逐块解码会切坏多字节字符）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}
