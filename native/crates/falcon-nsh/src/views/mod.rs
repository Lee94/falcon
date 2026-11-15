//! 工作区功能视图集合：插件双轨面板、Coding Agent GUI、文件浏览与编辑、Git 修改与模态框。

pub mod agent;
pub mod dialogs;
pub mod files;
pub mod file_operations;
pub mod git;
mod host_form;
mod tailscale;
pub mod plugins;
mod relays;

pub use agent::{AgentPaneView, agent_pane_view};
pub use dialogs::{ModalLayer, modal_layer};
pub use file_operations::{
    FileOperationLayer, FileOperationsToolbar, file_operation_layer, file_operations_toolbar,
};
pub use files::{FilePaneView, FilesPanel, file_pane_view, files_panel, is_scoped_raw_preview_url};
pub use git::{DiffPaneView, GitPanel, diff_pane_view, git_panel};
pub use plugins::{ClientPluginVerifier, PluginPanel, plugin_panel};
