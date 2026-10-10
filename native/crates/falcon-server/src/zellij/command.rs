//! 拼 zellij 命令行的纯函数（argv 与 env，零 I/O）。移植自 `packages/server/src/zellij/command.ts`（S1 待移植）。

/// zellij 在宿主机上用到的路径。[`super::host::HostLayout`] `Deref` 到它
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZellijPaths {
    /// 二进制完整路径
    pub bin: String,
    /// ~/.falcon/zellij/config/config.kdl —— 见 CONFIG_BODY
    pub config_file: String,
    /// ~/.falcon/zellij/sock
    pub socket_dir: String,
    /// ~/.falcon/zellij/config —— 保持为空目录即可保证零配置文件运行
    pub config_dir: String,
    /// ~/.falcon/zellij/data
    pub data_dir: String,
    /// ~/.falcon/zellij/cache —— 仅 Linux 生效（XDG_CACHE_HOME）
    pub cache_dir: String,
    /// ~/.falcon/zellij/layouts/falcon.kdl —— 见 LAYOUT_BODY
    pub layout_file: String,
    /// ~/.falcon/zellij/config/scroll.kdl —— 带滚动位置插件的会话用的配置，见 scroll_config_body
    pub scroll_config_file: String,
    /// ~/.falcon/zellij/plugins/falcon-scroll.wasm —— 滚动位置插件（ADR 0019）
    pub scroll_plugin_file: String,
}
