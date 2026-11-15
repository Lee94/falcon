//! 客户端响应式工作区状态仓库（`WorkspaceStore`）。
//!
//! 严格对齐 `CONTEXT.md` 领域语义与 `falcon-core` 纯逻辑：
//! - **浮动岛列式排布（ADR 0011 / 0012）**：列与多画布状态存放在 `Vec<ColumnLayout>`，几何统一走
//!   `falcon_core::layout::layout_frames`；
//! - **收起窗口（Detach）≠ 终止（Terminate）**：`detach_pane` 仅从工作区排布摘除窗口并关闭该视图的
//!   WebSocket，后台持久会话继续运行、侧栏可随时「接回窗口」；`terminate_session` 才会销毁宿主机进程
//!   （前台仍有进程在跑时先弹确认框，避免误杀）；
//! - **源项目 / 附属项目 / 远端主机 / 中转**：保持与服务端节点模型（ADR 0027）一致的归属关系；
//! - 同时支持离线确定性工作台（供 `nsh-testing` 无头回归测试与首屏展示）与在线 `FalconClient` 实时同步。

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::{FalconClient, raw_url};
use falcon_core::agent_mode::{AgentDisplayStyle, AgentModeTransition};
use falcon_core::file_text::{TextShape, for_disk, for_editor};
use falcon_core::layout::{
    ColumnLayout, column, find_pane, insert_column, insert_pane, new_canvas_id, pane_keys,
    remove_pane,
};
use falcon_core::pane_key::{DIFF_KEY, file_key, parse_pane_key, term_key};
use falcon_core::profiles::{ProfilesFile, ServerProfile};
#[cfg(not(target_arch = "wasm32"))]
use falcon_core::program_status::FinishChimes;
use falcon_core::program_status::detail_text;
use falcon_core::term::TermPref;
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::{
    CreateSessionRequest, PluginActionInput, PluginRenderInput, WorkspaceEntryKind,
};
use falcon_proto::{
    FilePreview, PluginBlock, PluginInfo, PluginKind, PluginList, PluginOption, PluginPackageInfo,
    PluginScope, PluginSeries, PluginStat, PluginTone, PluginView, ProgramState, SessionState,
};
use nsh_core::prelude::{Owner, Signal, effect, provide_context, signal, spawn, use_context};
use parking_lot::Mutex;

#[path = "file_operations.rs"]
mod file_operations;
use file_operations::FileOperations;
pub(crate) use file_operations::{ConfirmationKind, FileJob, JobKind, NameAction};

use crate::terminal::TerminalSession;
#[cfg(not(target_arch = "wasm32"))]
use crate::theme::use_theme;
#[cfg(not(target_arch = "wasm32"))]
use crate::views::ClientPluginVerifier;

/// 右侧栏可切换的功能面板（ADR 0025：插件持久化为 `plugin:<id>`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RightPanelKind {
    Files,
    Git,
    Plugin(String),
}

/// 顶层模态框状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveDialog {
    None,
    Palette,
    QuickOpen,
    Profiles,
    NewProject,
    EditProject(String),
    DeriveProject(String),
    Settings(SettingsTab),
    Askpass {
        id: String,
        prompt: String,
    },
    ConfirmTerminate {
        session_id: String,
        foreground_cmd: String,
    },
    RenameSession {
        session_id: String,
        form_id: u64,
    },
    ConfirmDeleteHost {
        host_id: String,
        host_name: String,
        project_count: u32,
    },
    ConfirmUnsavedFiles {
        action: FileGuardAction,
        file_keys: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileGuardAction {
    Close(String),
    Reload(String),
    SwitchServer(String),
    Quit,
    CloseWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileGuardChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileBufferState {
    Loading,
    Editable,
    ReadOnly,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePreviewKind {
    Text { truncated: bool, has_version: bool },
    Image { mime: String, size: u64 },
    Binary { size: u64 },
    TooLarge { size: u64 },
    Unknown,
}

fn file_response_is_current(
    current_connection: u64,
    response_connection: u64,
    current_buffer: u64,
    response_buffer: u64,
) -> bool {
    current_connection == response_connection && current_buffer == response_buffer
}

/// 设置模态框页签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Appearance,
    Terminal,
    Notifications,
    Hosts,
    Relays,
    Plugins,
    About,
}

/// 移动端顶层导航页。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileTab {
    Projects,
    Workspace,
    Panels,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPlacement {
    Focus,
    Right,
    Down,
}

/// 项目摘要记录（对齐 `CONTEXT.md`：源项目 vs 附属项目、本机宿主机 vs 远端主机）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `None` 表示本机宿主机；`Some(host_id)` 表示 SSH 远端主机。
    pub host_id: Option<String>,
    pub host_name: Option<String>,
    /// 非空表示这是从该源项目 git worktree 派生出的**附属项目**（ADR 0002）。
    pub derived_from: Option<String>,
    pub branch: Option<String>,
    /// 保留服务端的树分组信息（多仓库、存档与 shell），避免界面摘要丢失归属。
    pub source: Option<falcon_proto::Project>,
}

/// 会话摘要记录（对齐 `CONTEXT.md`：默认不起名，显示自动标题；区分持久会话与临时会话）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub auto_title: String,
    pub cwd: String,
    pub durable: bool,
    pub state: SessionState,
    /// 若为 Coding Agent 会话（ADR 0013），记录 CLI 标识（如 `"claude"` / `"codex"`）。
    pub agent: Option<String>,
    /// 守护进程经 OSC 7501 推来的程序运行状态摘要（只画在窗口标题栏胶囊与侧栏徽标上，绝不在终端内加动态行）。
    pub program_status: Option<(ProgramState, String)>,
    /// 前台正在运行的非 shell 命令（用于终止前确认拦截）。
    pub foreground_cmd: Option<String>,
}

impl SessionRecord {
    /// 侧栏与命令面板显示的统一标题（手起名 → 前台命令/自动标题 → Agent 名 → shell）。
    pub fn display_title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        if !self.auto_title.trim().is_empty() {
            return self.auto_title.clone();
        }
        if let Some(ag) = &self.agent {
            return ag.clone();
        }
        falcon_core::session_title::shell_label(None)
    }
}

/// 远端主机记录（SSH 连接配置）。
#[derive(Clone, Debug, PartialEq)]
pub struct HostRecord {
    pub id: String,
    pub name: String,
    pub destination: String,
    pub os_label: String,
    pub connected: bool,
    pub source: Option<falcon_proto::SshHost>,
}

/// 中转规则记录（ADR 0016：端口转发与公网发布按机器挂，同槽位互斥）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayRecord {
    pub id: String,
    pub machine_label: String,
    pub kind_label: String,
    pub local_port: u16,
    pub target: String,
    pub public_url: Option<String>,
    pub enabled: bool,
    pub same_port_conflict: bool,
}

fn relay_record_from_forward(forward: falcon_proto::PortForward) -> RelayRecord {
    RelayRecord {
        id: forward.id,
        machine_label: forward.host_id,
        kind_label: if forward.kind == falcon_proto::ForwardKind::Local {
            "L → R".into()
        } else {
            "R → L".into()
        },
        local_port: forward.bind_port,
        target: format!("{}:{}", forward.dest_host, forward.dest_port),
        public_url: None,
        enabled: forward.enabled,
        same_port_conflict: false,
    }
}

fn relay_record_from_share(share: falcon_proto::PublicShare) -> RelayRecord {
    RelayRecord {
        id: share.id,
        machine_label: share.host_id.unwrap_or_else(|| "local".into()),
        kind_label: "Quick Tunnel".into(),
        local_port: share.dest_port,
        target: format!("http://{}:{}", share.dest_host, share.dest_port),
        public_url: share.public_url,
        enabled: share.enabled,
        same_port_conflict: false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RelayWriteKind {
    Toggle { id: String, share: bool },
    CreateShare { host: String },
    CreateForward { host: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RelayWriteKey {
    origin_url: String,
    kind: RelayWriteKind,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RelayRefreshKey {
    origin_url: String,
    connection_generation: u64,
    request_generation: u64,
}

#[cfg(not(target_arch = "wasm32"))]
struct RelayRefreshQueueState {
    in_flight: bool,
    trailing: Option<(RelayRefreshKey, FalconClient)>,
}

/// Agent 会话图形时间线条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTimelineItem {
    pub id: String,
    pub role: AgentItemRole,
    pub title: String,
    pub body: String,
    pub diff: Option<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentItemRole {
    User,
    Assistant,
    Tool,
    Permission,
}

/// 单个 Agent 会话的交互状态（含 `falcon_core::agent_mode::AgentModeTransition` 状态机）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionView {
    pub session_id: String,
    pub agent_name: String,
    pub model: String,
    pub available_models: Vec<String>,
    pub reasoning_effort: String,
    pub awaiting_permission: Option<String>,
    pub running: bool,
    pub graphical: bool,
    pub style: AgentDisplayStyle,
    pub transition: AgentModeTransition,
    pub items: Vec<AgentTimelineItem>,
}

/// 文件目录项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntryItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub is_html: bool,
}

/// 打开的文件编辑器缓冲（ADR 0023：换行风格保持 + `content_hash` 冲突拦截）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFileBuffer {
    pub project_id: String,
    pub path: String,
    pub disk_hash: String,
    pub state: FileBufferState,
    pub preview_kind: Option<FilePreviewKind>,
    pub load_error: Option<String>,
    pub save_error: Option<String>,
    pub saving: bool,
    pub generation: u64,
    pub edit_revision: u64,
    pub close_after_save: bool,
    pub remote_changed_conflict: bool,
    pub shape: TextShape,
    pub clean_body: String,
    pub current_body: String,
    /// 若为 HTML 预览，仅允许填入 `/api/projects/:id/raw/<token>/<path>` 作用域路径（ADR 0007）。
    pub raw_preview_url: Option<String>,
}

struct PendingFileBatch {
    action: FileGuardAction,
    connection_generation: u64,
    file_keys: Vec<String>,
    expected_generations: HashMap<String, u64>,
    remaining: HashSet<String>,
    failed: bool,
}

impl OpenFileBuffer {
    pub fn loading(
        project_id: impl Into<String>,
        path: impl Into<String>,
        generation: u64,
    ) -> Self {
        let (body, shape) = for_editor("");
        Self {
            project_id: project_id.into(),
            path: path.into(),
            disk_hash: String::new(),
            state: FileBufferState::Loading,
            preview_kind: None,
            load_error: None,
            save_error: None,
            saving: false,
            generation,
            edit_revision: 0,
            close_after_save: false,
            remote_changed_conflict: false,
            shape,
            clean_body: body.to_string(),
            current_body: body.to_string(),
            raw_preview_url: None,
        }
    }

    pub fn from_disk_text(
        project_id: impl Into<String>,
        path: impl Into<String>,
        disk_text: &str,
        hash: impl Into<String>,
        raw_preview_url: Option<String>,
    ) -> Self {
        let (body, shape) = for_editor(disk_text);
        Self {
            project_id: project_id.into(),
            path: path.into(),
            disk_hash: hash.into(),
            state: FileBufferState::Editable,
            preview_kind: Some(FilePreviewKind::Text {
                truncated: false,
                has_version: true,
            }),
            load_error: None,
            save_error: None,
            saving: false,
            generation: 0,
            edit_revision: 0,
            close_after_save: false,
            remote_changed_conflict: false,
            shape,
            clean_body: body.to_string(),
            current_body: body.to_string(),
            raw_preview_url,
        }
    }

    pub fn from_preview(
        project_id: impl Into<String>,
        path: impl Into<String>,
        generation: u64,
        preview: FilePreview,
        raw_preview_url: Option<String>,
    ) -> Self {
        let project_id = project_id.into();
        let path = path.into();
        let mut buffer = Self::loading(project_id, path, generation);
        buffer.raw_preview_url = raw_preview_url;
        match preview {
            FilePreview::Text {
                text,
                truncated,
                version,
                ..
            } => {
                let (body, shape) = for_editor(&text);
                let has_version = version.as_deref().is_some_and(|v| !v.trim().is_empty());
                let editable = !truncated && has_version;
                buffer.state = if editable {
                    FileBufferState::Editable
                } else {
                    FileBufferState::ReadOnly
                };
                buffer.preview_kind = Some(FilePreviewKind::Text {
                    truncated,
                    has_version,
                });
                buffer.disk_hash = if editable {
                    version.unwrap_or_default()
                } else {
                    String::new()
                };
                buffer.clean_body = body.to_string();
                buffer.current_body = body.to_string();
                buffer.shape = shape;
            }
            FilePreview::Image { mime, size } => {
                buffer.state = FileBufferState::ReadOnly;
                buffer.preview_kind = Some(FilePreviewKind::Image { mime, size });
            }
            FilePreview::Binary { size } => {
                buffer.state = FileBufferState::ReadOnly;
                buffer.preview_kind = Some(FilePreviewKind::Binary { size });
            }
            FilePreview::TooLarge { size } => {
                buffer.state = FileBufferState::ReadOnly;
                buffer.preview_kind = Some(FilePreviewKind::TooLarge { size });
            }
            FilePreview::Unknown => {
                buffer.state = FileBufferState::ReadOnly;
                buffer.preview_kind = Some(FilePreviewKind::Unknown);
            }
        }
        buffer
    }

    pub fn fail_load(&mut self, error: impl Into<String>) {
        self.state = FileBufferState::Failed;
        self.load_error = Some(error.into());
        self.current_body.clear();
        self.clean_body.clear();
        self.disk_hash.clear();
        self.preview_kind = None;
        self.raw_preview_url = None;
    }

    pub fn editable(&self) -> bool {
        self.state == FileBufferState::Editable && !self.disk_hash.trim().is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.editable() && self.current_body != self.clean_body
    }

    pub fn save_succeeded(
        &mut self,
        generation: u64,
        submitted_body: String,
        submitted_revision: u64,
        disk_hash: String,
    ) -> bool {
        if self.generation != generation {
            return false;
        }
        self.saving = false;
        self.disk_hash = disk_hash;
        self.clean_body = submitted_body;
        self.remote_changed_conflict = false;
        self.save_error = None;
        let should_close = self.close_after_save && self.edit_revision == submitted_revision;
        self.close_after_save = false;
        should_close
    }

    pub fn save_failed(&mut self, generation: u64, error: String, conflict: bool) -> bool {
        if self.generation != generation {
            return false;
        }
        self.saving = false;
        self.save_error = Some(error);
        self.remote_changed_conflict = conflict;
        self.close_after_save = false;
        true
    }

    pub fn encoded_for_disk(&self) -> String {
        for_disk(&self.current_body, self.shape)
    }
}

/// Git 工作区变更项与提交图节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeItem {
    pub path: String,
    pub orig_path: Option<String>,
    pub index: String,
    pub work: String,
    pub untracked: bool,
    pub status: String,
    pub staged: bool,
    pub additions: u32,
    pub deletions: u32,
    pub old_text: String,
    pub new_text: String,
}

pub fn git_change_is_conflicted(change: &GitChangeItem) -> bool {
    matches!(
        format!("{}{}", change.index.trim(), change.work.trim()).as_str(),
        "UU" | "AA" | "DD" | "AU" | "UA" | "DU" | "UD"
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitRequestContext {
    pub connection_generation: u64,
    pub server_url: String,
    pub project_id: String,
    pub repo: Option<String>,
    pub mapping_generation: crate::project_operations::MappingGeneration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GitConfirmedOperation {
    Restore { paths: Vec<String>, untracked: Vec<String> },
    Abort,
    Take { side: falcon_proto::GitTakeSide, paths: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct GitConfirmation {
    pub context: GitRequestContext,
    pub project_name: String,
    pub operation: GitConfirmedOperation,
    pub paths: Vec<String>,
    pub untracked: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitItem {
    pub sha: String,
    pub lane: usize,
    pub subject: String,
    pub author: String,
    pub age: String,
}

/// 插件面板运行时快照（同时承载声明式 `PluginView` 与 `.nshpkg` 签名校验状态）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginPanelState {
    pub view: Option<PluginView>,
    pub params: BTreeMap<String, String>,
    pub package_status: Option<PackageVerificationState>,
    pub loading: bool,
    pub error: Option<String>,
}

/// `.nshpkg` 签名包校验结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageVerificationState {
    Verified {
        app_id: String,
        version: String,
        build: u64,
        sha256: String,
    },
    Untrusted {
        reason: String,
    },
    RollbackBlocked {
        current: u64,
        incoming: u64,
    },
}

/// 启动参数配置。
#[derive(Clone, Debug, Default)]
pub struct FalconLaunchOptions {
    /// 强制离线演示数据模式（无头测试默认开启，不发真实网络请求）。
    pub offline_demo: bool,
    /// 覆盖初始服务地址。
    pub server_url: Option<String>,
    /// 覆盖首帧主题偏好（供无头测试注入浅色槽位深底主题等场景）。
    pub initial_theme: Option<falcon_theme::ThemeSettings>,
    #[cfg(test)]
    pub test_client_url: Option<String>,
    #[cfg(test)]
    pub test_switch_url: Option<String>,
    #[cfg(test)]
    pub test_live_file_operations: bool,
}

struct SharedTerminals {
    map: Mutex<HashMap<String, Arc<TerminalSession>>>,
    #[cfg(not(target_arch = "wasm32"))]
    client: Mutex<Option<FalconClient>>,
    #[cfg(not(target_arch = "wasm32"))]
    chimes: Mutex<FinishChimes>,
    #[cfg(not(target_arch = "wasm32"))]
    verifier: Mutex<ClientPluginVerifier>,
}

/// 跨组件共享的响应式工作区状态句柄（`Copy`，底层全部由 NSH `Signal` 与 `Rc` 承载）。
#[derive(Clone)]
pub struct WorkspaceStore {
    pub profiles: Signal<ProfilesFile>,
    pub server_url: Signal<String>,
    pub server_connected: Signal<bool>,
    pub plaintext_warning: Signal<bool>,

    pub projects: Signal<Vec<ProjectRecord>>,
    pub active_project_id: Signal<String>,
    pub sessions: Signal<Vec<SessionRecord>>,
    pub hosts: Signal<Vec<HostRecord>>,
    pub relays: Signal<Vec<RelayRecord>>,
    relay_writes: Signal<HashMap<RelayWriteKey, u64>>,
    pub detected_agents: Signal<Vec<String>>,

    pub columns: Signal<Vec<ColumnLayout>>,
    pub active_canvas_id: Signal<String>,
    pub focused_pane_key: Signal<String>,

    pub collapsed: Signal<BTreeMap<String, bool>>,
    pub sessions_open: Signal<BTreeMap<String, bool>>,
    pub overview_open: Signal<bool>,
    pub term_zoomed: Signal<bool>,
    pub sidebar_open: Signal<bool>,
    pub sidebar_width: Signal<f32>,
    pub right_panel_open: Signal<bool>,
    pub right_panel_width: Signal<f32>,
    pub right_panel_kind: Signal<RightPanelKind>,

    pub active_dialog: Signal<ActiveDialog>,
    pub editing_host: Signal<Option<falcon_proto::SshHost>>,
    pub editing_host_generation: Signal<u64>,
    pub host_delete_busy: Signal<Option<(u64, String)>>,
    pub host_delete_error: Signal<Option<String>>,
    pub mobile_tab: Signal<MobileTab>,
    pub tui_passthrough: Signal<bool>,
    pub extra_ctrl_latched: Signal<bool>,
    pub extra_alt_latched: Signal<bool>,

    pub appearance: crate::appearance::AppearanceState,
    pub server_info: Signal<Option<falcon_proto::SystemInfo>>,
    pub finish_sound: Signal<bool>,
    pub file_directory: Signal<String>,
    pub files_loading: Signal<bool>,
    pub file_error: Signal<Option<String>>,
    pub quick_open_paths: Signal<Vec<String>>,
    pub quick_open_truncated: Signal<bool>,
    pub quick_open_loading: Signal<bool>,
    pub quick_open_error: Signal<Option<String>>,
    pub quick_open_project: Signal<String>,
    pub term_prefs: Signal<TermPref>,
    pub agent_views: Signal<HashMap<String, AgentSessionView>>,
    pub file_entries: Signal<Vec<FileEntryItem>>,
    pub(crate) file_operations: FileOperations,
    pub open_files: Signal<HashMap<String, OpenFileBuffer>>,
    pub git_loading: Signal<bool>,
    pub git_error: Signal<Option<String>>,
    pub git_diff: Signal<Option<String>>,
    pub git_diff_loading: Signal<bool>,
    pub git_diff_error: Signal<Option<String>>,
    pub git_diff_truncated: Signal<bool>,
    pub git_changes: Signal<Vec<GitChangeItem>>,
    pub git_commits: Signal<Vec<GitCommitItem>>,
    pub selected_git_path: Signal<Option<String>>,
    pub selected_git_repo: Signal<Option<String>>,
    pub git_conflict: Signal<Option<falcon_proto::GitConflict>>,
    pub git_operation_busy: Signal<Option<GitRequestContext>>,
    pub git_operation_error: Signal<Option<String>>,
    pub git_confirmation: Signal<Option<GitConfirmation>>,
    pub git_diff_split: Signal<bool>,
    pub commit_draft: Signal<String>,
    pub commit_draft_revision: Signal<u64>,

    pub plugin_list: Signal<PluginList>,
    pub plugin_states: Signal<HashMap<String, PluginPanelState>>,
    pub toast_banner: Signal<Option<String>>,

    pub project_operations: Rc<crate::project_operations::ProjectOperations>,
    pub(crate) session_operations: crate::session_operations::SessionOperations,

    terminals: Rc<SharedTerminals>,
    task_owner: Owner,
    offline_demo: bool,
    #[cfg(test)]
    test_live_file_operations: bool,
    connection_generation: Rc<Cell<u64>>,
    connection_request_generation: Rc<Cell<u64>>,
    next_file_generation: Rc<Cell<u64>>,
    directory_request_generation: Rc<Cell<u64>>,
    quick_open_generation: Rc<Cell<u64>>,
    git_working_generation: Rc<Cell<u64>>,
    git_diff_generation: Rc<Cell<u64>>,
    termination_in_flight: Rc<Mutex<HashSet<(u64, String)>>>,
    reattach_in_flight: Rc<Mutex<HashSet<(u64, String)>>>,
    create_session_in_flight: Rc<Mutex<HashSet<(u64, String)>>>,
    host_mutation_in_flight: Rc<Mutex<HashSet<(u64, String)>>>,
    metadata_revision_by_url: Rc<Mutex<HashMap<String, u64>>>,
    git_mutation_in_flight: Rc<Mutex<HashSet<GitRequestContext>>>,
    pending_file_batch: Rc<Mutex<Option<PendingFileBatch>>>,
    file_save_in_flight: Rc<Mutex<HashSet<(String, String, u64)>>>,
    next_file_save_request_id: Rc<Cell<u64>>,
    next_relay_write_token: Rc<Cell<u64>>,
    #[cfg(not(target_arch = "wasm32"))]
    relay_refresh_queue: Rc<Mutex<RelayRefreshQueueState>>,
}

impl WorkspaceStore {
    fn offline_file_operations(&self) -> bool {
        #[cfg(test)]
        {
            self.offline_demo && !self.test_live_file_operations
        }
        #[cfg(not(test))]
        {
            self.offline_demo
        }
    }

    pub(crate) fn spawn_root(&self, future: impl Future<Output = ()> + 'static) {
        self.task_owner.run(move || {
            let _ = spawn(future);
        });
    }

    pub fn open_quick_open(&self) {
        self.active_dialog.set(ActiveDialog::QuickOpen);
        self.load_quick_open_index(&self.active_project_id.get());
    }

    pub fn load_quick_open_index(&self, project_id: &str) {
        let project_id = project_id.to_string();
        if self.quick_open_loading.get() && self.quick_open_project.get() == project_id {
            return;
        }
        let generation = self.quick_open_generation.get().wrapping_add(1);
        self.quick_open_generation.set(generation);
        self.quick_open_project.set(project_id.clone());
        self.quick_open_paths.set(Vec::new());
        self.quick_open_truncated.set(false);
        self.quick_open_loading.set(false);
        self.quick_open_error.set(None);
        if project_id.is_empty() {
            self.quick_open_loading.set(false);
            return;
        }
        let mapping_generation = self.project_operations.mapping_generation(&project_id);
        if !self
            .project_operations
            .mapping_is_current(&project_id, &mapping_generation)
        {
            self.quick_open_error
                .set(Some(crate::i18n::t!("project.mappingChangePending").to_string()));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let connection_generation = self.connection_generation.get();
            let store = self.clone();
            self.quick_open_loading.set(true);
            self.spawn_root(async move {
                let result = client.index_files(&project_id).await;
                if store.connection_generation.get() != connection_generation
                    || store.quick_open_generation.get() != generation
                    || store.active_project_id.get() != project_id
                    || !store
                        .project_operations
                        .mapping_is_current(&project_id, &mapping_generation)
                {
                    return;
                }
                match result {
                    Ok(index) => {
                        store.quick_open_paths.set(index.paths);
                        store.quick_open_truncated.set(index.truncated);
                    }
                    Err(error) => store.quick_open_error.set(Some(error.to_string())),
                }
                store.quick_open_loading.set(false);
            });
            return;
        }
        if self.offline_demo {
            self.quick_open_paths.set(
                self.file_entries
                    .get()
                    .into_iter()
                    .map(|entry| entry.path)
                    .collect(),
            );
            self.quick_open_loading.set(false);
        } else {
            self.quick_open_error
                .set(Some(crate::i18n::t!("files.notConnected")));
            self.quick_open_loading.set(false);
        }
    }

    fn next_connection_generation(&self) -> u64 {
        let generation = self.connection_generation.get().wrapping_add(1);
        self.connection_generation.set(generation);
        generation
    }

    fn next_connection_request_generation(&self) -> u64 {
        let generation = self.connection_request_generation.get().wrapping_add(1);
        self.connection_request_generation.set(generation);
        generation
    }

    fn next_file_generation(&self) -> u64 {
        let generation = self.next_file_generation.get().wrapping_add(1);
        self.next_file_generation.set(generation);
        generation
    }

    fn next_file_save_request_id(&self) -> u64 {
        let request_id = self.next_file_save_request_id.get().wrapping_add(1);
        self.next_file_save_request_id.set(request_id);
        request_id
    }

    fn dirty_file_keys(&self) -> Vec<String> {
        self.open_files
            .get()
            .into_iter()
            .filter_map(|(key, buffer)| (buffer.is_dirty() || buffer.saving).then_some(key))
            .collect()
    }

    pub fn request_close_file(&self, key: &str) {
        let Some(buffer) = self.open_files.get().get(key).cloned() else {
            self.detach_pane_now(key);
            return;
        };
        if buffer.is_dirty() || buffer.saving {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::Close(key.to_string()),
                file_keys: vec![key.to_string()],
            });
        } else {
            self.detach_pane_now(key);
        }
    }

    pub fn request_reload_file(&self, key: &str) {
        let Some(buffer) = self.open_files.get().get(key).cloned() else {
            return;
        };
        if buffer.is_dirty() || buffer.saving {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::Reload(key.to_string()),
                file_keys: vec![key.to_string()],
            });
        } else {
            self.start_file_load(
                &buffer.project_id,
                &buffer.path,
                key,
                self.next_file_generation(),
            );
        }
    }

    pub fn resolve_file_guard(&self, choice: FileGuardChoice) {
        let ActiveDialog::ConfirmUnsavedFiles { action, file_keys } = self.active_dialog.get()
        else {
            return;
        };
        match choice {
            FileGuardChoice::Cancel => {
                drop(self.pending_file_batch.lock().take());
                self.active_dialog.set(ActiveDialog::None);
            }
            FileGuardChoice::Save => match action.clone() {
                FileGuardAction::Close(key) => {
                    self.active_dialog.set(ActiveDialog::None);
                    self.save_file_internal(&key, false, true);
                }
                FileGuardAction::Reload(_) => {}
                FileGuardAction::SwitchServer(_)
                | FileGuardAction::Quit
                | FileGuardAction::CloseWindow => self.start_file_save_batch(action, file_keys),
            },
            FileGuardChoice::Discard => {
                if file_keys.iter().any(|key| {
                    self.open_files
                        .get()
                        .get(key)
                        .is_some_and(|buffer| buffer.saving)
                }) {
                    self.show_toast(crate::i18n::t!("native.nsh.fileSaveInProgress"));
                    return;
                }
                self.active_dialog.set(ActiveDialog::None);
                match action.clone() {
                    FileGuardAction::Close(key) => self.detach_pane_now(&key),
                    FileGuardAction::Reload(key) => {
                        if let Some(buffer) = self.open_files.get().get(&key).cloned() {
                            self.start_file_load(
                                &buffer.project_id,
                                &buffer.path,
                                &key,
                                self.next_file_generation(),
                            );
                        }
                    }
                    FileGuardAction::SwitchServer(_) => self.complete_file_guard_action(action),
                    FileGuardAction::Quit | FileGuardAction::CloseWindow => {
                        for key in file_keys {
                            self.detach_pane_now(&key);
                        }
                        self.complete_file_guard_action(action)
                    }
                }
            }
        }
    }

    fn complete_file_guard_action(&self, action: FileGuardAction) {
        match action {
            FileGuardAction::SwitchServer(url) => {
                #[cfg(not(target_arch = "wasm32"))]
                self.connect_live_server_now(&url);
                #[cfg(target_arch = "wasm32")]
                self.connect_live_server_now(&url);
            }
            FileGuardAction::Quit => {
                #[cfg(not(target_arch = "wasm32"))]
                nsh_core::element::desktop::quit();
            }
            FileGuardAction::CloseWindow => {
                #[cfg(not(target_arch = "wasm32"))]
                nsh_core::element::desktop::main_window().close();
            }
            FileGuardAction::Close(key) => self.detach_pane_now(&key),
            FileGuardAction::Reload(key) => self.request_reload_file(&key),
        }
    }

    fn start_file_save_batch(&self, action: FileGuardAction, file_keys: Vec<String>) {
        if self.pending_file_batch.lock().is_some() {
            self.show_toast(crate::i18n::t!("native.nsh.fileSaveInProgress"));
            return;
        }
        if file_keys.is_empty() {
            self.complete_file_guard_action(action);
            return;
        }
        let connection_generation = self.connection_generation.get();
        *self.pending_file_batch.lock() = Some(PendingFileBatch {
            action,
            connection_generation,
            expected_generations: file_keys
                .iter()
                .map(|key| {
                    let generation = self
                        .open_files
                        .get()
                        .get(key)
                        .map_or(0, |buffer| buffer.generation);
                    (key.clone(), generation)
                })
                .collect(),
            remaining: file_keys.iter().cloned().collect(),
            file_keys: file_keys.clone(),
            failed: false,
        });
        self.active_dialog.set(ActiveDialog::None);
        for key in file_keys {
            let already_saving = self
                .open_files
                .get()
                .get(&key)
                .is_some_and(|buffer| buffer.saving);
            let generation = self
                .pending_file_batch
                .lock()
                .as_ref()
                .and_then(|batch| batch.expected_generations.get(&key).copied())
                .unwrap_or(0);
            if !already_saving && !self.save_file_internal(&key, false, false) {
                self.finish_file_batch_key(&key, generation, false);
            }
        }
    }

    fn finish_file_batch_key(&self, key: &str, generation: u64, success: bool) {
        let completed = {
            let mut pending = self.pending_file_batch.lock();
            let Some(batch) = pending.as_mut() else {
                return;
            };
            if batch.expected_generations.get(key) != Some(&generation) {
                return;
            }
            if !batch.remaining.remove(key) {
                return;
            }
            batch.failed |= !success;
            let done = batch.remaining.is_empty();
            if done { pending.take() } else { None }
        };
        if let Some(batch) = completed {
            if batch.connection_generation != self.connection_generation.get() {
                return;
            }
            let open_files = self.open_files.get();
            let current_dirty: Vec<String> = open_files
                .iter()
                .filter_map(|(key, buffer)| {
                    (buffer.is_dirty() || buffer.saving).then_some(key.clone())
                })
                .collect();
            let generations_unchanged = batch.file_keys.iter().all(|key| {
                open_files.get(key).is_none_or(|buffer| {
                    batch.expected_generations.get(key) == Some(&buffer.generation)
                })
            });
            let all_clean = current_dirty.is_empty() && generations_unchanged;
            if !batch.failed && all_clean {
                self.complete_file_guard_action(batch.action);
            } else if !batch.failed && !current_dirty.is_empty() {
                self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                    action: batch.action,
                    file_keys: current_dirty,
                });
            } else {
                self.show_toast(crate::i18n::t!("native.nsh.fileSaveFailed"));
            }
        }
    }

    pub fn request_quit(&self) {
        let file_keys = self.dirty_file_keys();
        if file_keys.is_empty() {
            #[cfg(not(target_arch = "wasm32"))]
            nsh_core::element::desktop::quit();
        } else {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::Quit,
                file_keys,
            });
        }
    }

    pub fn request_close_window(&self) {
        let file_keys = self.dirty_file_keys();
        if file_keys.is_empty() {
            #[cfg(not(target_arch = "wasm32"))]
            nsh_core::element::desktop::main_window().close();
        } else {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::CloseWindow,
                file_keys,
            });
        }
    }

    /// 初始化工作区（填充一套结构完备的本地种子状态，并在非离线模式下异步连向 `falcon-server`）。
    pub fn install(opts: &FalconLaunchOptions) -> Self {
        let file_task_owner = Owner::current();
        let task_owner = Owner::current();
        let connection_generation = Rc::new(Cell::new(0));
        let connection_request_generation = Rc::new(Cell::new(0));
        let local_url = falcon_platform::shared().local_server_url();
        let persisted_profiles = (!opts.offline_demo)
            .then(|| falcon_platform::shared().load_profiles())
            .flatten();
        let mut profiles = ProfilesFile::load_with_local(persisted_profiles.as_deref(), &local_url);
        let url = opts
            .server_url
            .clone()
            .unwrap_or_else(|| profiles.startup_profile().url);
        if !profiles
            .profiles
            .iter()
            .any(|p| p.url.trim_end_matches('/') == url.trim_end_matches('/'))
        {
            profiles.upsert(ServerProfile {
                id: falcon_core::profiles::new_profile_id(),
                name: url.clone(),
                url: url.clone(),
                plaintext_ok: false,
            });
        }
        let plaintext = ServerProfile {
            id: "probe".into(),
            name: String::new(),
            url: url.clone(),
            plaintext_ok: false,
        }
        .is_insecure();

        let c1 = new_canvas_id();
        let mut initial_col = column([term_key("sess-main"), term_key("sess-agent")], None);
        initial_col.canvas = Some(c1.clone());

        let projects = vec![
            ProjectRecord {
                id: "proj-falcon".into(),
                name: "falcon".into(),
                path: "/workspace/falcon".into(),
                host_id: None,
                host_name: None,
                derived_from: None,
                branch: Some("main".into()),
                source: None,
            },
            ProjectRecord {
                id: "proj-falcon-nsh".into(),
                name: "falcon · nsh-client".into(),
                path: "/workspace/falcon.worktrees/nsh-client".into(),
                host_id: None,
                host_name: None,
                derived_from: Some("proj-falcon".into()),
                branch: Some("feat/nsh-unified-client".into()),
                source: None,
            },
            ProjectRecord {
                id: "proj-edge-node".into(),
                name: "edge-gateway".into(),
                path: "/srv/edge-gateway".into(),
                host_id: Some("host-prod-01".into()),
                host_name: Some("build-box-01".into()),
                derived_from: None,
                branch: Some("release".into()),
                source: None,
            },
        ];

        let sessions = vec![
            SessionRecord {
                id: "sess-main".into(),
                project_id: "proj-falcon".into(),
                name: String::new(),
                auto_title: "cargo check -p falcon-nsh".into(),
                cwd: "/workspace/falcon/native".into(),
                durable: true,
                state: SessionState::Active,
                agent: None,
                program_status: Some((ProgramState::Working, "cargo check · 1.2s".into())),
                foreground_cmd: Some("cargo check -p falcon-nsh".into()),
            },
            SessionRecord {
                id: "sess-agent".into(),
                project_id: "proj-falcon-nsh".into(),
                name: String::new(),
                auto_title: "Claude Code".into(),
                cwd: "/workspace/falcon.worktrees/nsh-client".into(),
                durable: true,
                state: SessionState::Active,
                agent: Some("claude".into()),
                program_status: Some((ProgramState::Done, crate::i18n::t!("demo.refactorDone"))),
                foreground_cmd: None,
            },
        ];

        let hosts = vec![HostRecord {
            id: "host-prod-01".into(),
            name: "build-box-01".into(),
            destination: "dev@10.0.4.12:22".into(),
            os_label: crate::i18n::t!("demo.hostOsReady"),
            connected: true,
            source: None,
        }];

        let relays = vec![
            RelayRecord {
                id: "relay-1".into(),
                machine_label: "build-box-01".into(),
                kind_label: "L → R".into(),
                local_port: 5432,
                target: "127.0.0.1:5432".into(),
                public_url: None,
                enabled: true,
                same_port_conflict: false,
            },
            RelayRecord {
                id: "relay-2".into(),
                machine_label: "local".into(),
                kind_label: "Quick Tunnel".into(),
                local_port: 4923,
                target: "http://127.0.0.1:4923".into(),
                public_url: Some("https://falcon-demo.trycloudflare.com".into()),
                enabled: false,
                same_port_conflict: false,
            },
        ];

        let mut agent_map = HashMap::new();
        agent_map.insert(
            "sess-agent".into(),
            AgentSessionView {
                session_id: "sess-agent".into(),
                agent_name: "Claude Code".into(),
                model: "claude-sonnet-4-5".into(),
                available_models: vec![
                    "claude-sonnet-4-5".into(),
                    "claude-opus-4-6".into(),
                    "codex-mini-latest".into(),
                ],
                reasoning_effort: "high".into(),
                awaiting_permission: None,
                running: false,
                graphical: true,
                style: AgentDisplayStyle::Rich,
                transition: AgentModeTransition::new().connected(false),
                items: vec![
                    AgentTimelineItem {
                        id: "item-1".into(),
                        role: AgentItemRole::User,
                        title: "User".into(),
                        body: crate::i18n::t!("demo.agentPrompt"),
                        diff: None,
                    },
                    AgentTimelineItem {
                        id: "item-2".into(),
                        role: AgentItemRole::Tool,
                        title: "Edit native/crates/falcon-nsh/src/terminal/painter.rs".into(),
                        body: crate::i18n::t!("demo.agentToolBody"),
                        diff: Some((
                            "fn paint_cell_slow() {}\n".into(),
                            "pub fn paint_snapshot(snap: &Snapshot, resolved: &ResolvedTheme) -> Painter {\n    Painter::new()\n}\n".into(),
                        )),
                    },
                    AgentTimelineItem {
                        id: "item-3".into(),
                        role: AgentItemRole::Assistant,
                        title: "Claude Code".into(),
                        body: crate::i18n::t!("demo.agentReply"),
                        diff: None,
                    },
                ],
            },
        );

        let file_entries = vec![
            FileEntryItem {
                name: "Cargo.toml".into(),
                path: "native/Cargo.toml".into(),
                is_dir: false,
                size_bytes: 4120,
                is_html: false,
            },
            FileEntryItem {
                name: "src/lib.rs".into(),
                path: "native/crates/falcon-nsh/src/lib.rs".into(),
                is_dir: false,
                size_bytes: 2890,
                is_html: false,
            },
            FileEntryItem {
                name: "preview/report.html".into(),
                path: "docs/report.html".into(),
                is_dir: false,
                size_bytes: 1540,
                is_html: true,
            },
        ];

        let mut open_files = HashMap::new();
        open_files.insert(
            file_key("proj-falcon", "native/Cargo.toml"),
            OpenFileBuffer::from_disk_text(
                "proj-falcon",
                "native/Cargo.toml",
                "[workspace]\nmembers = [\n  \"crates/falcon-core\",\n  \"crates/falcon-nsh\",\n  \"crates/falcon-app\",\n]\n",
                "sha256:9f86d081884c7d65",
                None,
            ),
        );
        open_files.insert(
            file_key("proj-falcon", "docs/report.html"),
            OpenFileBuffer::from_disk_text(
                "proj-falcon",
                "docs/report.html",
                "<!doctype html><html><body><h1>Falcon Sandbox Preview</h1></body></html>\n",
                "sha256:4b227777d4dd1fc6",
                Some(format!(
                    "{}/api/projects/proj-falcon/raw/tok_scoped_readonly/docs/report.html",
                    url.trim_end_matches('/')
                )),
            ),
        );

        let git_changes = vec![
            GitChangeItem {
                path: "native/crates/falcon-nsh/src/lib.rs".into(),
                orig_path: None,
                index: " ".into(),
                work: "M".into(),
                untracked: false,
                status: "M".into(),
                staged: true,
                additions: 148,
                deletions: 0,
                old_text: "// legacy placeholder\n".into(),
                new_text: "pub fn falcon_root() -> nsh_core::element::Root {\n    falcon_root_with_options(FalconLaunchOptions::default())\n}\n".into(),
            },
            GitChangeItem {
                path: "native/crates/falcon-server/src/plugins/nshpkg.rs".into(),
                orig_path: None,
                index: "?".into(),
                work: "?".into(),
                untracked: true,
                status: "A".into(),
                staged: false,
                additions: 220,
                deletions: 0,
                old_text: String::new(),
                new_text: "pub struct PluginKeyring {\n    root_public_str: String,\n}\n".into(),
            },
        ];

        let git_commits = vec![
            GitCommitItem {
                sha: "b05ffe7".into(),
                lane: 0,
                subject: "feat(nsh): unify six-platform rendering onto Native Shell 0.5".into(),
                author: "Falcon Team".into(),
                age: "2m".into(),
            },
            GitCommitItem {
                sha: "8a41c20".into(),
                lane: 1,
                subject: "feat(plugins): add Ed25519 keyring and .nshpkg resigning".into(),
                author: "Falcon Team".into(),
                age: "18m".into(),
            },
            GitCommitItem {
                sha: "41d9e03".into(),
                lane: 0,
                subject: "refactor(platform): decouple falcon-platform from gpui-kit".into(),
                author: "Falcon Team".into(),
                age: "1h".into(),
            },
        ];

        let plugin_list = PluginList {
            dir: "~/.falcon/plugins".into(),
            plugins: vec![
                PluginInfo {
                    id: "sysmon".into(),
                    name: crate::i18n::t!("demo.sysmonName"),
                    description: Some(crate::i18n::t!("demo.sysmonDesc")),
                    icon: "activity".into(),
                    kind: PluginKind::Builtin,
                    scope: PluginScope::Project,
                    refresh: Some(2.0),
                    enabled: true,
                    error: None,
                    dir: None,
                    package: None,
                },
                PluginInfo {
                    id: "gpu-inspector".into(),
                    name: crate::i18n::t!("demo.gpuPkgName"),
                    description: Some(crate::i18n::t!("demo.gpuPkgDesc")),
                    icon: "cpu".into(),
                    kind: PluginKind::Package,
                    scope: PluginScope::Global,
                    refresh: None,
                    enabled: true,
                    error: None,
                    dir: None,
                    package: Some(PluginPackageInfo {
                        app_id: "dev.falcon.plugin.gpu-inspector".into(),
                        version: "0.5.1".into(),
                        build: 12,
                        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                            .into(),
                        size: 18432,
                    }),
                },
            ],
            trust_root: None,
        };

        let mut plugin_states = HashMap::new();
        plugin_states.insert(
            "sysmon".into(),
            PluginPanelState {
                view: Some(PluginView {
                    title: Some("Linux x86_64 · 8 Cores".into()),
                    refresh: Some(2.0),
                    blocks: vec![
                        PluginBlock::Select {
                            param: "machine".into(),
                            label: Some(crate::i18n::t!("demo.targetMachine")),
                            value: Some("follow".into()),
                            options: vec![
                                PluginOption {
                                    value: "follow".into(),
                                    label: crate::i18n::t!("demo.followCurrentProject"),
                                },
                                PluginOption {
                                    value: "local".into(),
                                    label: crate::i18n::t!("demo.localHost"),
                                },
                            ],
                        },
                        PluginBlock::Stats {
                            items: vec![
                                PluginStat {
                                    label: "CPU".into(),
                                    value: "14.2%".into(),
                                    detail: Some(crate::i18n::t!("demo.cpuDetail")),
                                    tone: Some(PluginTone::Accent),
                                },
                                PluginStat {
                                    label: crate::i18n::t!("demo.memoryLabel"),
                                    value: "6.4 / 16 GB".into(),
                                    detail: Some(crate::i18n::t!("demo.memoryDetail")),
                                    tone: Some(PluginTone::Success),
                                },
                            ],
                        },
                        PluginBlock::Meter {
                            label: crate::i18n::t!("demo.rootDiskLabel"),
                            value: 42.0,
                            max: Some(100.0),
                            text: Some("42%".into()),
                            tone: Some(PluginTone::Default),
                        },
                        PluginBlock::Chart {
                            label: Some(crate::i18n::t!("demo.cpuChartLabel")),
                            text: Some("14.2%".into()),
                            series: vec![PluginSeries {
                                name: Some("total".into()),
                                values: vec![8.0, 11.5, 19.0, 15.2, 12.0, 16.4, 13.8, 14.2],
                                tone: Some(PluginTone::Accent),
                            }],
                            max: Some(100.0),
                            slots: Some(12),
                        },
                    ],
                    toast: None,
                    params: BTreeMap::new(),
                }),
                params: BTreeMap::new(),
                package_status: None,
                loading: false,
                error: None,
            },
        );
        plugin_states.insert(
            "gpu-inspector".into(),
            PluginPanelState {
                view: Some(PluginView {
                    title: Some("NSH Sandbox · Verified".into()),
                    refresh: None,
                    blocks: vec![PluginBlock::Stats {
                        items: vec![
                            PluginStat {
                                label: crate::i18n::t!("demo.frameBudgetLabel"),
                                value: "1.1 ms".into(),
                                detail: Some(crate::i18n::t!("demo.frameBudgetDetail")),
                                tone: Some(PluginTone::Success),
                            },
                            PluginStat {
                                label: crate::i18n::t!("demo.vramLabel"),
                                value: "38 MB".into(),
                                detail: Some(crate::i18n::t!("demo.vramDetail")),
                                tone: Some(PluginTone::Accent),
                            },
                        ],
                    }],
                    toast: None,
                    params: BTreeMap::new(),
                }),
                params: BTreeMap::new(),
                package_status: Some(PackageVerificationState::Verified {
                    app_id: "dev.falcon.plugin.gpu-inspector".into(),
                    version: "0.5.1".into(),
                    build: 12,
                    sha256: "e3b0c44298fc1c14".into(),
                }),
                loading: false,
                error: None,
            },
        );

        let terminals = Rc::new(SharedTerminals {
            map: Mutex::new(HashMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            client: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            chimes: Mutex::new(FinishChimes::new()),
            #[cfg(not(target_arch = "wasm32"))]
            verifier: Mutex::new(ClientPluginVerifier::new_in_memory()),
        });

        // 预热主终端演示输出（含真彩 ANSI + 盒线 + 中文宽字符）
        {
            let t_main = TerminalSession::new_offline("sess-main", 96, 28, true);
            t_main.feed_output(
                b"\x1b[38;2;97;175;239m\xe2\x94\x8c\xe2\x94\x80 Falcon Unified NSH Terminal \xe2\x94\x80\xe2\x94\x90\x1b[0m\r\n\
                  \x1b[32m\xe2\x9c\x94\x1b[0m sessiond durable holder attached (\xe6\x8c\x81\xe4\xb9\x85\xe4\xbc\x9a\xe8\xaf\x9d)\r\n\
                  \x1b[90m/workspace/falcon/native\x1b[0m $ cargo check -p falcon-nsh\r\n\
                  \x1b[1;32m    Finished\x1b[0m `dev` profile [unoptimized + debuginfo] in 0.42s\r\n\
                  \x1b[90m/workspace/falcon/native\x1b[0m $ ",
            );
            let t_agent = TerminalSession::new_offline("sess-agent", 80, 24, true);
            t_agent.feed_output(
                b"\x1b[1;35mClaude Code\x1b[0m \xc2\xb7 feat/nsh-unified-client\r\n\
                  > Ready for structured GUI or full PTY interaction.\r\n",
            );
            let mut guard = terminals.map.lock();
            guard.insert("sess-main".into(), t_main);
            guard.insert("sess-agent".into(), t_agent);
        }

        let server_url = signal(url.clone());
        let store = Self {
            profiles: signal(profiles),
            server_url: server_url.clone(),
            server_connected: signal(opts.offline_demo),
            plaintext_warning: signal(plaintext),
            projects: signal(projects),
            active_project_id: signal("proj-falcon".into()),
            sessions: signal(sessions),
            hosts: signal(hosts),
            relays: signal(relays),
            relay_writes: signal(HashMap::new()),
            detected_agents: signal(vec!["claude".into(), "codex".into()]),
            columns: signal(vec![initial_col]),
            active_canvas_id: signal(c1),
            focused_pane_key: signal(term_key("sess-main")),
            collapsed: signal(BTreeMap::new()),
            sessions_open: signal(BTreeMap::new()),
            overview_open: signal(false),
            term_zoomed: signal(false),
            sidebar_open: signal(true),
            sidebar_width: signal(260.0),
            right_panel_open: signal(true),
            right_panel_width: signal(260.0),
            right_panel_kind: signal(RightPanelKind::Plugin("sysmon".into())),
            active_dialog: signal(ActiveDialog::None),
            editing_host: signal(None),
            editing_host_generation: signal(0),
            host_delete_busy: signal(None),
            host_delete_error: signal(None),
            mobile_tab: signal(MobileTab::Workspace),
            tui_passthrough: signal(false),
            extra_ctrl_latched: signal(false),
            extra_alt_latched: signal(false),
            appearance: crate::appearance::AppearanceState::new(),
            server_info: signal(None),
            finish_sound: signal(
                crate::theme::load_pref_item("falcon.finishSound").as_deref() != Some("off"),
            ),
            file_directory: signal(String::new()),
            files_loading: signal(false),
            file_error: signal(None),
            quick_open_paths: signal(Vec::new()),
            quick_open_truncated: signal(false),
            quick_open_loading: signal(false),
            quick_open_error: signal(None),
            quick_open_project: signal(String::new()),
            term_prefs: signal(falcon_core::term::load_term_pref(
                crate::theme::load_pref_item("falcon.term").as_deref(),
            )),
            agent_views: signal(agent_map),
            file_entries: signal(file_entries),
            file_operations: FileOperations::new(file_task_owner),
            open_files: signal(open_files),
            git_loading: signal(false),
            git_error: signal(None),
            git_diff: signal(None),
            git_diff_loading: signal(false),
            git_diff_error: signal(None),
            git_diff_truncated: signal(false),
            git_changes: signal(git_changes),
            git_commits: signal(git_commits),
            selected_git_path: signal(Some("native/crates/falcon-nsh/src/lib.rs".into())),
            selected_git_repo: signal(None),
            git_conflict: signal(None),
            git_operation_busy: signal(None),
            git_operation_error: signal(None),
            git_confirmation: signal(None),
            git_diff_split: signal(false),
            commit_draft: signal(String::new()),
            commit_draft_revision: signal(0),
            plugin_list: signal(plugin_list),
            plugin_states: signal(plugin_states),
            toast_banner: signal(None),
            project_operations: Rc::new(crate::project_operations::ProjectOperations::new(
                task_owner,
                connection_generation.clone(),
                connection_request_generation.clone(),
                server_url,
            )),
            session_operations: crate::session_operations::SessionOperations::new(
                task_owner,
                server_url,
            ),
            terminals,
            task_owner,
            offline_demo: opts.offline_demo,
            #[cfg(test)]
            test_live_file_operations: opts.test_live_file_operations,
            connection_generation,
            connection_request_generation,
            next_file_generation: Rc::new(Cell::new(0)),
            directory_request_generation: Rc::new(Cell::new(0)),
            quick_open_generation: Rc::new(Cell::new(0)),
            git_working_generation: Rc::new(Cell::new(0)),
            git_diff_generation: Rc::new(Cell::new(0)),
            termination_in_flight: Rc::new(Mutex::new(HashSet::new())),
            reattach_in_flight: Rc::new(Mutex::new(HashSet::new())),
            create_session_in_flight: Rc::new(Mutex::new(HashSet::new())),
            host_mutation_in_flight: Rc::new(Mutex::new(HashSet::new())),
            metadata_revision_by_url: Rc::new(Mutex::new(HashMap::new())),
            git_mutation_in_flight: Rc::new(Mutex::new(HashSet::new())),
            pending_file_batch: Rc::new(Mutex::new(None)),
            file_save_in_flight: Rc::new(Mutex::new(HashSet::new())),
            next_file_save_request_id: Rc::new(Cell::new(0)),
            next_relay_write_token: Rc::new(Cell::new(0)),
            #[cfg(not(target_arch = "wasm32"))]
            relay_refresh_queue: Rc::new(Mutex::new(RelayRefreshQueueState {
                in_flight: false,
                trailing: None,
            })),
        };

        #[cfg(all(test, not(target_arch = "wasm32")))]
        if let Some(url) = opts.test_client_url.as_deref() {
            let parsed = url.parse().expect("test client URL must be valid");
            *store.terminals.client.lock() = Some(FalconClient::new(parsed));
        }

        if !opts.offline_demo {
            store.projects.set(Vec::new());
            store.sessions.set(Vec::new());
            store.hosts.set(Vec::new());
            store.relays.set(Vec::new());
            store.agent_views.set(HashMap::new());
            store.file_entries.set(Vec::new());
            store.open_files.set(HashMap::new());
            store.git_changes.set(Vec::new());
            store.git_commits.set(Vec::new());
            store.selected_git_path.set(None);
            store.plugin_list.set(PluginList {
                dir: String::new(),
                plugins: Vec::new(),
                trust_root: None,
            });
            store.plugin_states.set(HashMap::new());
            store.terminals.map.lock().clear();
            store.active_project_id.set(String::new());
            store.columns.set(Vec::new());
            store.focused_pane_key.set(String::new());
            store.restore_workspace(&url);
            store.connect_live_server(&url);
            let persisted = store.clone();
            effect(move || persisted.persist_workspace());
            let profiles = store.profiles;
            effect(move || {
                if let Ok(json) = serde_json::to_string(&profiles.get()) {
                    let platform = falcon_platform::shared();
                    if platform.load_profiles().as_deref() != Some(json.as_str()) {
                        platform.store_profiles(&json);
                    }
                }
            });
            let prefs = store.term_prefs;
            effect(move || {
                crate::theme::save_pref_item(
                    "falcon.term",
                    Some(&falcon_core::term::serialize_term_pref(&prefs.get())),
                )
            });
        }

        provide_context(store.clone());
        store
    }

    /// 异步尝试连接真实 `falcon-server`，拉取项目、会话、远端主机、中转、文件/Git 与双轨插件；连不上时保持可交互不崩。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_live_server(&self, url: &str) {
        let file_keys = self.dirty_file_keys();
        let same_server = self.server_url.get().trim_end_matches('/') == url.trim_end_matches('/');
        if !same_server && !file_keys.is_empty() {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::SwitchServer(url.to_string()),
                file_keys,
            });
            return;
        }
        self.connect_live_server_now(url);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn connect_live_server_now(&self, url: &str) {
        let Ok(parsed_url) = url.parse() else {
            self.show_toast(crate::i18n::t!("native.nsh.invalidServerUrl"));
            return;
        };
        self.session_operations
            .begin_connection(self.server_connected);
        let icon_token = self.appearance.app_icon.begin_server_epoch();
        self.file_operations.connection_changed(self);
        self.switch_workspace(url);
        let connection_generation = self.connection_generation.get();
        let connection_request_generation = self.next_connection_request_generation();
        let client = FalconClient::new(parsed_url);
        *self.terminals.client.lock() = Some(client.clone());
        self.server_url.set(url.to_string());
        self.plaintext_warning.set(
            ServerProfile {
                id: "probe".into(),
                name: String::new(),
                url: url.to_string(),
                plaintext_ok: false,
            }
            .is_insecure(),
        );

        let target_url = url.to_string();
        let store = self.clone();
        self.spawn_root(async move {
            // 若连接的是本机默认服务端且平台托管了本机服务（如 macOS LaunchAgent），先按需拉起
            let is_local = target_url.trim_end_matches('/')
                == falcon_platform::shared()
                    .local_server_url()
                    .trim_end_matches('/');
            if is_local && falcon_platform::shared().local_service().is_some() {
                let ensure_url = target_url.clone();
                let _ = nsh_core::prelude::blocking(move || {
                    if let Some(svc) = falcon_platform::shared().local_service() {
                        let _ = svc.ensure_running(&ensure_url);
                    }
                })
                .await;
            }

            if let Ok(info) = client.system().await
                && store.connection_generation.get() == connection_generation
                && store.connection_request_generation.get() == connection_request_generation
            {
                store.server_info.set(Some(info));
            }
            if store.connection_generation.get() == connection_generation
                && store.connection_request_generation.get() == connection_request_generation
                && store.server_url.get().trim_end_matches('/')
                    == target_url.trim_end_matches('/')
            {
                let icon_store = store.clone();
                store.spawn_root(async move {
                    crate::appearance::app_icon::load_initial(icon_store, icon_token).await;
                });
            }
            let mut proj_paths: HashMap<String, String> = HashMap::new();

            let mut snapshot = store.project_operations.begin_mapping_snapshot(
                &target_url,
                connection_generation,
                connection_request_generation,
            );
            let mut revision = store.project_operations.project_update_revision(&target_url);
            let projects = loop {
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                let Ok(projects) = client.list_projects().await else {
                    break None;
                };
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                if !store.project_operations.project_update_revision_is_current(&revision) {
                    snapshot = store.project_operations.begin_mapping_snapshot(
                        &target_url,
                        connection_generation,
                        connection_request_generation,
                    );
                    revision = store.project_operations.project_update_revision(&target_url);
                    continue;
                }
                break Some(projects);
            };

            if let Some(projects) = projects {
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                let project_ids = projects
                    .iter()
                    .map(|project| project.id.clone())
                    .collect::<Vec<_>>();
                store.server_connected.set(true);
                let mapped: Vec<ProjectRecord> = projects
                    .into_iter()
                    .map(|p| {
                        let source = Some(p.clone());
                        let path = p.working_dir.unwrap_or_default();
                        proj_paths.insert(p.id.clone(), path.clone());
                        let host_name = p.ssh.as_ref().map(|s| s.host.clone());
                        let (derived_from, branch) = match p.worktree {
                            Some(wt) => (Some(wt.source_project_id), Some(wt.branch)),
                            None => (None, None),
                        };
                        ProjectRecord {
                            id: p.id,
                            name: p.name,
                            path,
                            host_id: p.host_id,
                            host_name,
                            derived_from,
                            branch,
                            source,
                        }
                    })
                    .collect();
                if let Some(first) = mapped.first() {
                    if !mapped.iter().any(|p| p.id == store.active_project_id.get()) {
                        store.set_active_project_id(first.id.clone());
                    }
                }
                store.projects.set(mapped);
                if !store
                    .project_operations
                    .reconcile_mapping_snapshot(&snapshot, &project_ids)
                {
                    return;
                }
                store.refresh_project_data();
            }

            let sessions = loop {
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                let revision = store.metadata_revision(&target_url);
                let Ok(sessions) = client.list_sessions().await else {
                    break None;
                };
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                if store.metadata_revision(&target_url) != revision {
                    continue;
                }
                break Some(sessions);
            };

            if let Some(sessions) = sessions {
                let chimed = store.terminals.chimes.lock().observe_all(
                    sessions
                        .iter()
                        .map(|s| (s.id.as_str(), s.program_status.as_ref())),
                );
                if !chimed.is_empty() && store.finish_sound.get() {
                    falcon_platform::shared().play_chime();
                }

                let mapped_sessions: Vec<SessionRecord> = sessions
                    .into_iter()
                    .map(|s| {
                        let ps = s
                            .program_status
                            .as_ref()
                            .map(|sum| (sum.state, detail_text(sum, 80).unwrap_or_default()));
                        let cwd = proj_paths.get(&s.project_id).cloned().unwrap_or_default();
                        SessionRecord {
                            id: s.id.clone(),
                            project_id: s.project_id.clone(),
                            name: s.name.clone(),
                            auto_title: s.title.clone().unwrap_or_default(),
                            cwd,
                            durable: s.durable,
                            state: s.state,
                            agent: s.agent.as_ref().map(|a| a.as_str().to_string()),
                            program_status: ps,
                            foreground_cmd: s.title.clone(),
                        }
                    })
                    .collect();

                // 只保留实际仍存在的已打开会话；后台会话不因客户端重启自动全部接回。
                let columns = store.columns.get();
                let keys: Vec<_> = pane_keys(&columns)
                    .into_iter()
                    .filter(|key| match parse_pane_key(key) {
                        Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) => {
                            mapped_sessions.iter().any(|session| session.id == id)
                        }
                        Some(_) => true,
                        None => false,
                    })
                    .collect();
                store
                    .columns
                    .set(falcon_core::layout::sync_columns(&columns, &keys));
                if !keys.contains(&store.focused_pane_key.get()) {
                    store
                        .focused_pane_key
                        .set(keys.first().cloned().unwrap_or_default());
                }
                store.sessions.set(mapped_sessions);
            }

            let hosts = loop {
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                let revision = store.metadata_revision(&target_url);
                let Ok(hosts) = client.list_hosts().await else {
                    break None;
                };
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                if store.metadata_revision(&target_url) != revision {
                    continue;
                }
                break Some(hosts);
            };

            if let Some(hosts) = hosts {
                let mapped_hosts: Vec<HostRecord> = hosts
                    .into_iter()
                    .map(|h| HostRecord {
                        source: Some(h.clone()),
                        id: h.id,
                        name: h.name,
                        destination: format!("{}@{}:{}", h.username, h.host, h.port),
                        os_label: "SSH".into(),
                        connected: true,
                    })
                    .collect();
                store.hosts.set(mapped_hosts);
            }

            store.queue_relay_refresh(
                client.clone(),
                &target_url,
                connection_generation,
                connection_request_generation,
            );

            let target_project = store.active_project_id.get();
            if let Ok(agents) = client
                .agents(&falcon_client::ProbeTarget::Project(target_project.clone()))
                .await
            {
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.active_project_id.get() != target_project
                {
                    return;
                }
                store
                    .detected_agents
                    .set(agents.into_iter().map(|a| a.as_str().to_string()).collect());
            }

            if let Ok(plugins) = client.list_plugins().await {
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                let trust_root = if let Some(root) = plugins.trust_root.clone() {
                    Some(root)
                } else {
                    client.plugin_trust().await.ok().map(|t| t.root_public_key)
                };
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                {
                    return;
                }

                for p in &plugins.plugins {
                    if store.connection_generation.get() != connection_generation
                        || store.connection_request_generation.get() != connection_request_generation
                    {
                        return;
                    }
                    if !p.enabled {
                        continue;
                    }
                    if p.kind == PluginKind::Package {
                        if let (Some(root_b64), Ok(pkg_bytes)) =
                            (trust_root.as_deref(), client.plugin_package(&p.id).await)
                        {
                            if store.connection_generation.get() != connection_generation
                                || store.connection_request_generation.get() != connection_request_generation
                            {
                                return;
                            }
                            let expected_app_id = p.package.as_ref().map(|m| m.app_id.as_str());
                            let status = store.terminals.verifier.lock().verify_and_accept(
                                root_b64,
                                expected_app_id,
                                &pkg_bytes,
                            );
                            let pid = p.id.clone();
                            store.plugin_states.update(|map| {
                                let entry = map.entry(pid).or_insert_with(|| PluginPanelState {
                                    view: None,
                                    params: BTreeMap::new(),
                                    package_status: None,
                                    loading: false,
                                    error: None,
                                });
                                entry.package_status = Some(status);
                            });
                        }
                    } else if p.id == "sysmon" {
                        store.dispatch_plugin_action(&p.id, None, None);
                    }
                }
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                {
                    return;
                }
                store.plugin_list.set(plugins);
            }
        });
    }

    /// Web (`wasm32`) 端使用浏览器原生 `fetch` 同步 `falcon-server` 状态。
    #[cfg(target_arch = "wasm32")]
    pub fn connect_live_server(&self, url: &str) {
        let file_keys = self.dirty_file_keys();
        let same_server = self.server_url.get().trim_end_matches('/') == url.trim_end_matches('/');
        if !same_server && !file_keys.is_empty() {
            self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                action: FileGuardAction::SwitchServer(url.to_string()),
                file_keys,
            });
            return;
        }
        self.connect_live_server_now(url);
    }

    #[cfg(target_arch = "wasm32")]
    fn connect_live_server_now(&self, url: &str) {
        self.session_operations
            .begin_connection(self.server_connected);
        let icon_token = self.appearance.app_icon.begin_server_epoch();
        self.file_operations.connection_changed(self);
        self.switch_workspace(url);
        let connection_generation = self.connection_generation.get();
        let connection_request_generation = self.next_connection_request_generation();
        let base = url.trim_end_matches('/').to_string();
        self.server_url.set(url.to_string());
        self.plaintext_warning.set(
            ServerProfile {
                id: "probe".into(),
                name: String::new(),
                url: url.to_string(),
                plaintext_ok: false,
            }
            .is_insecure(),
        );

        let store = self.clone();
        self.spawn_root(async move {
            use nsh_core::prelude::{HttpRequest, fetch};

            if store.connection_generation.get() == connection_generation
                && store.connection_request_generation.get() == connection_request_generation
                && store.server_url.get().trim_end_matches('/') == base
            {
                let icon_store = store.clone();
                store.spawn_root(async move {
                    crate::appearance::app_icon::load_initial(icon_store, icon_token).await;
                });
            }

            let mut snapshot = store.project_operations.begin_mapping_snapshot(
                &base,
                connection_generation,
                connection_request_generation,
            );
            let mut revision = store.project_operations.project_update_revision(&base);
            let projects = loop {
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                let Ok(response) = fetch(HttpRequest::get(format!("{base}/api/projects"))).await
                else {
                    break None;
                };
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                if !response.ok() {
                    break None;
                }
                let Ok(projects) =
                    serde_json::from_slice::<Vec<falcon_proto::Project>>(&response.body)
                else {
                    break None;
                };
                if !store.project_operations.project_update_revision_is_current(&revision) {
                    snapshot = store.project_operations.begin_mapping_snapshot(
                        &base,
                        connection_generation,
                        connection_request_generation,
                    );
                    revision = store.project_operations.project_update_revision(&base);
                    continue;
                }
                break Some(projects);
            };

            if let Some(projects) = projects {
                if !store.project_operations.mapping_snapshot_is_current(&snapshot) {
                    return;
                }
                let project_ids = projects
                    .iter()
                    .map(|project| project.id.clone())
                    .collect::<Vec<_>>();
                store.server_connected.set(true);
                let mapped: Vec<ProjectRecord> = projects
                    .into_iter()
                    .map(|p| {
                        let source = Some(p.clone());
                        let host_name = p.ssh.as_ref().map(|s| s.host.clone());
                        let (derived_from, branch) = match p.worktree {
                            Some(wt) => (Some(wt.source_project_id), Some(wt.branch)),
                            None => (None, None),
                        };
                        ProjectRecord {
                            id: p.id,
                            name: p.name,
                            path: p.working_dir.unwrap_or_default(),
                            host_id: p.host_id,
                            host_name,
                            derived_from,
                            branch,
                            source,
                        }
                    })
                    .collect();
                if let Some(first) = mapped.first() {
                    if !mapped.iter().any(|p| p.id == store.active_project_id.get()) {
                        store.set_active_project_id(first.id.clone());
                    }
                }
                store.projects.set(mapped);
                if !store
                    .project_operations
                    .reconcile_mapping_snapshot(&snapshot, &project_ids)
                {
                    return;
                }
            }

            let sessions = loop {
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != base
                {
                    return;
                }
                let revision = store.metadata_revision(&base);
                let Ok(resp) = fetch(HttpRequest::get(format!("{base}/api/sessions"))).await
                else {
                    break None;
                };
                if store.connection_generation.get() != connection_generation
                    || store.connection_request_generation.get() != connection_request_generation
                    || store.server_url.get().trim_end_matches('/') != base
                {
                    return;
                }
                if !resp.ok() {
                    break None;
                }
                let Ok(sessions) =
                    serde_json::from_slice::<Vec<falcon_proto::SessionWithProject>>(&resp.body)
                else {
                    break None;
                };
                if store.metadata_revision(&base) != revision {
                    continue;
                }
                break Some(sessions);
            };

            if let Some(sessions) = sessions {
                let mapped: Vec<SessionRecord> = sessions
                    .into_iter()
                    .map(|s| SessionRecord {
                        id: s.id.clone(),
                        project_id: s.project_id.clone(),
                        name: s.name.clone(),
                        auto_title: s.title.clone().unwrap_or_default(),
                        cwd: String::new(),
                        durable: s.durable,
                        state: s.state,
                        agent: s.agent.as_ref().map(|a| a.as_str().to_string()),
                        program_status: s
                            .program_status
                            .as_ref()
                            .map(|ps| (ps.state, detail_text(ps, 80).unwrap_or_default())),
                        foreground_cmd: s.title.clone(),
                    })
                    .collect();
                let columns = store.columns.get();
                let keys: Vec<_> = pane_keys(&columns)
                    .into_iter()
                    .filter(|key| match parse_pane_key(key) {
                        Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) => {
                            mapped.iter().any(|session| session.id == id)
                        }
                        Some(_) => true,
                        None => false,
                    })
                    .collect();
                store
                    .columns
                    .set(falcon_core::layout::sync_columns(&columns, &keys));
                if !keys.contains(&store.focused_pane_key.get()) {
                    store
                        .focused_pane_key
                        .set(keys.first().cloned().unwrap_or_default());
                }
                store.sessions.set(mapped);
            }

            if let Ok(resp) = fetch(HttpRequest::get(format!("{base}/api/plugins"))).await
                && store.connection_generation.get() == connection_generation
                && store.connection_request_generation.get() == connection_request_generation
                && resp.ok()
                && let Ok(plugins) = serde_json::from_slice::<PluginList>(&resp.body)
            {
                store.plugin_list.set(plugins);
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn client(&self) -> Option<FalconClient> {
        self.terminals.client.lock().clone()
    }

    /// 读取后端本机 tailnet 里的设备。环境不可用由响应的 `available/reason` 表达，
    /// 这里的 `Err` 只表示 HTTP 请求或响应解码没有完成。
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn tailscale_devices(&self) -> Result<falcon_proto::TailscaleDevices, String> {
        let client = self
            .client()
            .ok_or_else(|| crate::i18n::t!("host.tailscaleFailed").to_string())?;
        client
            .tailscale_devices()
            .await
            .map_err(|error| error.to_string())
    }

    /// Web 端与初始状态同步一样使用浏览器 `fetch`，不经过原生 `FalconClient`。
    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn tailscale_devices(&self) -> Result<falcon_proto::TailscaleDevices, String> {
        use nsh_core::prelude::{HttpRequest, fetch};

        let failed = || crate::i18n::t!("host.tailscaleFailed").to_string();
        let base = self.server_url.get().trim_end_matches('/').to_string();
        let response = fetch(HttpRequest::get(format!("{base}/api/tailscale/devices")))
            .await
            .map_err(|_| failed())?;
        if !response.ok() {
            return Err(failed());
        }
        serde_json::from_slice(&response.body).map_err(|_| failed())
    }

    pub fn reload_plugins(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            spawn(async move {
                match client.list_plugins().await {
                    Ok(list) => store.plugin_list.set(list),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let id = id.to_string();
            spawn(async move {
                match client.set_plugin_enabled(&id, enabled).await {
                    Ok(list) => store.plugin_list.set(list),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
            return;
        }
        self.plugin_list.update(|list| {
            if let Some(p) = list.plugins.iter_mut().find(|p| p.id == id) {
                p.enabled = enabled;
            }
        });
    }

    pub fn test_host_connection(&self, id: &str) {
        // Web 端不在浏览器里探测 SSH 主机
        #[cfg(target_arch = "wasm32")]
        let _ = id;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let id = id.to_string();
            let generation = self.connection_generation.get();
            let request_generation = self.connection_request_generation.get();
            store.show_toast(crate::i18n::t!("host.testing"));
            self.spawn_root(async move {
                let result = client.test_host(&id).await;
                if store.connection_generation.get() == generation
                    && store.connection_request_generation.get() == request_generation
                {
                    match result {
                        Ok(falcon_proto::SshProbeResult::Reachable { .. }) => {
                            store.show_toast(crate::i18n::t!("host.testOk"))
                        }
                        Ok(falcon_proto::SshProbeResult::Failed { error }) => {
                            store.show_toast(error)
                        }
                        Err(error) => store.show_toast(error.to_string()),
                    }
                }
            });
        }
    }

    pub fn check_app_update(&self) {
        // Web 端没有应用内更新，nsh-core 在 wasm32 上也不提供 blocking（阻塞线程池），整个入口只在原生端编
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(updater) = falcon_platform::shared().app_updater() else {
                return;
            };
            let store = self.clone();
            self.show_toast(crate::i18n::t!("native.update.checking"));
            spawn(async move {
                let result =
                    nsh_core::prelude::blocking(move || updater.check(env!("CARGO_PKG_VERSION")))
                        .await;
                let message = match result {
                    Ok(falcon_platform::UpdateCheck::UpToDate { .. }) => {
                        crate::i18n::t!("native.update.upToDate")
                    }
                    Ok(falcon_platform::UpdateCheck::Available(update)) => {
                        crate::i18n::t!(
                            "native.update.available",
                            version = update.version.as_str()
                        )
                    }
                    _ => crate::i18n::t!("native.update.failed"),
                };
                store.show_toast(message);
            });
        }
    }

    pub fn begin_host_edit(&self, host: falcon_proto::SshHost) {
        self.invalidate_host_edit_generation();
        self.editing_host.set(Some(host));
    }

    pub fn invalidate_host_edit_generation(&self) {
        self.editing_host_generation
            .update(|generation| *generation = generation.wrapping_add(1));
    }

    pub fn clear_host_edit_if_current(&self, host_id: &str, generation: u64) -> bool {
        if self.editing_host_generation.get() != generation
            || !self
                .editing_host
                .get()
                .as_ref()
                .is_some_and(|host| host.id == host_id)
        {
            return false;
        }
        self.editing_host.set(None);
        self.editing_host_generation
            .set(generation.wrapping_add(1));
        true
    }

    pub(crate) fn connection_generation(&self) -> u64 {
        self.connection_generation.get()
    }

    pub(crate) fn connection_request_generation(&self) -> u64 {
        self.connection_request_generation.get()
    }

    pub fn save_ssh_host(
        &self,
        host_id: Option<String>,
        editing_generation: Option<u64>,
        input: falcon_proto::SshHostInput,
        on_complete: impl FnOnce(Result<(), String>) + 'static,
    ) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(client) = self.client() else {
                on_complete(Err(crate::i18n::t!("host.notConnected")));
                return false;
            };
            let connection_generation = self.connection_generation.get();
            let operation = host_id
                .as_ref()
                .map_or_else(|| "create".to_string(), |id| format!("host:{id}"));
            let in_flight_key = (connection_generation, operation);
            if !self.host_mutation_in_flight.lock().insert(in_flight_key.clone()) {
                return false;
            }
            let metadata_url = self.server_url.get();
            let store = self.clone();
            self.spawn_root(async move {
                store.bump_metadata_revision(&metadata_url);
                let result = match host_id.as_deref() {
                    Some(id) => client.update_host(id, &input).await,
                    None => client.create_host(&input).await,
                };
                store.bump_metadata_revision(&metadata_url);
                store.host_mutation_in_flight.lock().remove(&in_flight_key);
                if store.connection_generation.get() != connection_generation {
                    on_complete(Err(crate::i18n::t!("host.connectionChanged")));
                    return;
                }
                match result {
                    Ok(host) => {
                        if host_id.as_deref().is_some_and(|id| id != host.id) {
                            on_complete(Err(crate::i18n::t!("host.updateMismatch")));
                            return;
                        }
                        let record = HostRecord {
                            id: host.id.clone(),
                            name: host.name.clone(),
                            destination: format!("{}@{}:{}", host.username, host.host, host.port),
                            os_label: "SSH".into(),
                            connected: false,
                            source: Some(host.clone()),
                        };
                        store.hosts.update(|hosts| {
                            if let Some(existing) = hosts.iter_mut().find(|item| item.id == host.id)
                            {
                                let connected = existing.connected;
                                *existing = record.clone();
                                existing.connected = connected;
                            } else {
                                hosts.push(record);
                            }
                        });
                        if let (Some(host_id), Some(editing_generation)) =
                            (host_id.as_deref(), editing_generation)
                        {
                            store.clear_host_edit_if_current(host_id, editing_generation);
                        }
                        on_complete(Ok(()));
                    }
                    Err(error) => on_complete(Err(error.to_string())),
                }
            });
            true
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (host_id, editing_generation, input, on_complete);
            false
        }
    }

    pub fn delete_ssh_host(&self, host_id: &str) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = host_id;
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let host_id = host_id.to_string();
            let expected_dialog = self.active_dialog.get();
            if !matches!(
                &expected_dialog,
                ActiveDialog::ConfirmDeleteHost { host_id: id, .. } if id == &host_id
            ) {
                return;
            }
            if !self
                .hosts
                .get()
                .iter()
                .any(|host| host.id == host_id && host.source.is_some())
            {
                return;
            }
            let connection_generation = self.connection_generation.get();
            if self
                .host_delete_busy
                .get()
                .is_some_and(|(generation, _)| generation == connection_generation)
            {
                return;
            }
            let Some(client) = self.client() else {
                self.host_delete_error
                    .set(Some(crate::i18n::t!("host.notConnected")));
                return;
            };
            let in_flight_key = (connection_generation, format!("host:{host_id}"));
            if !self.host_mutation_in_flight.lock().insert(in_flight_key.clone()) {
                self.host_delete_error
                    .set(Some(crate::i18n::t!("host.operationBusy")));
                return;
            }
            self.host_delete_busy
                .set(Some((connection_generation, host_id.clone())));
            self.host_delete_error.set(None);
            let metadata_url = self.server_url.get();
            let store = self.clone();
            self.spawn_root(async move {
                store.bump_metadata_revision(&metadata_url);
                let result = client.delete_host(&host_id).await;
                store.bump_metadata_revision(&metadata_url);
                store.host_mutation_in_flight.lock().remove(&in_flight_key);
                if store.connection_generation.get() != connection_generation {
                    return;
                }
                match result {
                    Ok(_) => {
                        store.hosts.update(|hosts| hosts.retain(|host| host.id != host_id));
                        let editing_generation = store.editing_host_generation.get();
                        store.clear_host_edit_if_current(&host_id, editing_generation);
                        if store.active_dialog.get() == expected_dialog {
                            store.active_dialog.set(ActiveDialog::Settings(SettingsTab::Hosts));
                        }
                        if store.host_delete_busy.get()
                            == Some((connection_generation, host_id.clone()))
                        {
                            store.host_delete_busy.set(None);
                        }
                        store.host_delete_error.set(None);
                    }
                    Err(error) => {
                        if store.active_dialog.get() == expected_dialog {
                            store.host_delete_error.set(Some(error.to_string()));
                        } else {
                            store.show_toast(error.to_string());
                        }
                        if store.host_delete_busy.get()
                            == Some((connection_generation, host_id.clone()))
                        {
                            store.host_delete_busy.set(None);
                        }
                    }
                }
            });
        }
    }

    pub fn git_repos_for_active_project(&self) -> Vec<String> {
        let project = self.active_project_id.get();
        self.projects
            .get()
            .into_iter()
            .find(|record| record.id == project)
            .and_then(|record| record.source)
            .and_then(|source| source.multi)
            .map(|multi| multi.repos.into_iter().map(|repo| repo.dir).collect())
            .unwrap_or_default()
    }

    fn current_git_context(&self) -> Option<GitRequestContext> {
        let project_id = self.active_project_id.get();
        if project_id.is_empty() {
            return None;
        }
        let repos = self.git_repos_for_active_project();
        let multi_repo = self.projects.get().iter()
            .find(|project| project.id == project_id)
            .and_then(|project| project.source.as_ref())
            .is_some_and(|project| project.multi.is_some());
        if multi_repo && repos.is_empty() {
            return None;
        }
        let repo = if !multi_repo {
            self.selected_git_repo.set(None);
            None
        } else {
            let selected = self
                .selected_git_repo
                .get()
                .filter(|repo| repos.contains(repo))
                .or_else(|| repos.first().cloned());
            self.selected_git_repo.set(selected.clone());
            selected
        };
        Some(GitRequestContext {
            connection_generation: self.connection_generation.get(),
            server_url: self.server_url.get(),
            project_id,
            repo,
            mapping_generation: self.project_operations.mapping_generation(&self.active_project_id.get()),
        })
    }

    fn git_context_is_current(&self, context: &GitRequestContext) -> bool {
        self.connection_generation.get() == context.connection_generation
            && self.server_url.get() == context.server_url
            && self.active_project_id.get() == context.project_id
            && self.selected_git_repo.get() == context.repo
            && self
                .project_operations
                .mapping_is_current(&context.project_id, &context.mapping_generation)
    }

    pub fn select_git_repo(&self, repo: &str) {
        if !self.git_repos_for_active_project().iter().any(|entry| entry == repo) {
            return;
        }
        if self.selected_git_repo.get().as_deref() == Some(repo) {
            return;
        }
        self.selected_git_repo.set(Some(repo.to_string()));
        self.selected_git_path.set(None);
        self.git_diff.set(None);
        self.git_diff_error.set(None);
        self.git_conflict.set(None);
        self.git_operation_error.set(None);
        self.git_changes.set(Vec::new());
        self.refresh_git_data();
    }

    pub fn set_commit_draft(&self, draft: String) {
        self.commit_draft.set(draft);
        self.commit_draft_revision
            .set(self.commit_draft_revision.get().wrapping_add(1));
    }

    pub fn refresh_project_data(&self) {
        self.browse_directory("");
        self.refresh_git_data();
    }

    pub(crate) fn has_open_file_mapping(&self, project_id: &str) -> bool {
        let server_url = crate::project_operations::canonical_server_url(&self.server_url.get());
        self.open_files
            .get()
            .values()
            .any(|buffer| buffer.project_id == project_id)
            || pane_keys(&self.columns.get()).iter().any(|key| {
                matches!(
                    parse_pane_key(key),
                    Some(falcon_core::pane_key::PaneItem::File { project_id: pane_project, .. })
                        if pane_project == project_id
                )
            })
            || self.file_save_in_flight.lock().iter().any(|(url, id, _)| {
                url == &server_url && id == project_id
            })
            || self.git_mutation_in_flight.lock().iter().any(|context| {
                crate::project_operations::canonical_server_url(&context.server_url)
                    == server_url
                    && context.project_id == project_id
            })
            || self
                .file_operations
                .blocks_project_mapping(&server_url, project_id)
    }

    pub(crate) fn metadata_revision(&self, server_url: &str) -> u64 {
        let url = crate::project_operations::canonical_server_url(server_url);
        self.metadata_revision_by_url
            .lock()
            .get(&url)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn bump_metadata_revision(&self, server_url: &str) {
        let url = crate::project_operations::canonical_server_url(server_url);
        self.metadata_revision_by_url
            .lock()
            .entry(url)
            .and_modify(|revision| {
                *revision = revision.checked_add(1).expect("metadata revision exhausted");
            })
            .or_insert(1);
    }

    pub(crate) fn on_project_mapping_change_started(&self, project_id: &str) {
        if self.active_project_id.get() != project_id {
            return;
        }
        self.directory_request_generation
            .set(self.directory_request_generation.get().wrapping_add(1));
        self.quick_open_generation
            .set(self.quick_open_generation.get().wrapping_add(1));
        self.git_working_generation
            .set(self.git_working_generation.get().wrapping_add(1));
        self.git_diff_generation
            .set(self.git_diff_generation.get().wrapping_add(1));
        self.files_loading.set(false);
        self.file_directory.set(String::new());
        self.file_entries.set(Vec::new());
        self.file_error.set(None);
        self.quick_open_paths.set(Vec::new());
        self.quick_open_loading.set(false);
        self.quick_open_truncated.set(false);
        self.quick_open_error.set(None);
        self.quick_open_project.set(String::new());
        self.git_loading.set(false);
        self.git_error.set(None);
        self.git_changes.set(Vec::new());
        self.git_commits.set(Vec::new());
        self.git_conflict.set(None);
        self.git_diff.set(None);
        self.git_diff_loading.set(false);
        self.git_diff_error.set(None);
        self.git_diff_truncated.set(false);
        self.selected_git_path.set(None);
        self.selected_git_repo.set(None);
        self.file_operations.cancel_confirmation();
        self.file_operations.name_form.set(None);
        self.file_operations.clear_selection();
    }

    pub(crate) fn on_project_saved(&self, project: &falcon_proto::Project, _mapping_changed: bool) {
        let record = ProjectRecord {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.working_dir.clone().unwrap_or_default(),
            host_id: project.host_id.clone(),
            host_name: project.ssh.as_ref().map(|ssh| ssh.host.clone()),
            derived_from: project
                .worktree
                .as_ref()
                .map(|worktree| worktree.source_project_id.clone()),
            branch: project.worktree.as_ref().map(|worktree| worktree.branch.clone()),
            source: Some(project.clone()),
        };
        self.projects.update(|projects| {
            if let Some(existing) = projects.iter_mut().find(|item| item.id == record.id) {
                *existing = record;
            } else {
                projects.push(record);
            }
        });
        if self.active_project_id.get() == project.id {
            self.refresh_project_data();
        }
    }

    pub fn refresh_git_data(&self) {
        let project_id = self.active_project_id.get();
        let mapping_generation = self.project_operations.mapping_generation(&project_id);
        if !self
            .project_operations
            .mapping_is_current(&project_id, &mapping_generation)
        {
            self.git_loading.set(false);
            self.git_error
                .set(Some(crate::i18n::t!("project.mappingChangePending").to_string()));
            return;
        }
        let Some(context) = self.current_git_context() else {
            self.git_loading.set(false);
            self.git_error.set(Some(crate::i18n::t!("git.selectRepoRequired")));
            return;
        };
        self.refresh_git_context(context);
    }

    fn refresh_git_context(&self, context: GitRequestContext) {
        if !self.git_context_is_current(&context) {
            return;
        }
        let generation = self.git_working_generation.get().wrapping_add(1);
        self.git_working_generation.set(generation);
        self.git_loading.set(true);
        self.git_error.set(None);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            self.spawn_root(async move {
                let (head, working) = futures::join!(
                    client.repo_info(&context.project_id),
                    client.git_working(&context.project_id, context.repo.as_deref())
                );
                if store.git_working_generation.get() != generation
                    || !store.git_context_is_current(&context)
                {
                    return;
                }
                if let Ok(head) = head {
                    store.projects.update(|projects| {
                        if let Some(project) = projects
                            .iter_mut()
                            .find(|project| project.id == context.project_id)
                        {
                            project.branch = head.head_branch.or(head.head_sha);
                        }
                    });
                }
                match working {
                    Ok(working) if working.available => {
                        store.git_conflict.set(working.conflict);
                        let previous = store.git_changes.get();
                        let changes: Vec<_> = working.files.into_iter().map(|file| {
                            let path = file.path.clone();
                            let orig_path = file.orig_path.clone();
                            let index = file.index.clone();
                            let work = file.work.clone();
                            let untracked = (index.trim() == "?" && work.trim() == "?")
                                || index.trim() == "??" || work.trim() == "??";
                            let status = format!("{}{}", index.trim(), work.trim());
                            let staged = previous.iter().find(|old| {
                                old.path == path && old.orig_path == orig_path
                            }).map_or(true, |old| old.staged);
                            GitChangeItem {
                                path,
                                orig_path,
                                index,
                                work,
                                untracked,
                                status,
                                staged,
                                additions: file.added.unwrap_or(0) as u32,
                                deletions: file.deleted.unwrap_or(0) as u32,
                                old_text: String::new(),
                                new_text: String::new(),
                            }
                        }).collect();
                        let selected_path = store.selected_git_path.get();
                        let selected = selected_path.as_ref().and_then(|path| {
                            changes.iter().find(|change| &change.path == path).cloned()
                        });
                        store.git_changes.set(changes);
                        if let Some(item) = selected {
                            store.load_git_diff(item);
                        } else {
                            store.selected_git_path.set(None);
                            store.clear_git_diff();
                        }
                    }
                    Ok(working) => {
                        store.git_changes.set(Vec::new());
                        store.git_conflict.set(None);
                        store.selected_git_path.set(None);
                        store.clear_git_diff();
                        store.git_error.set(Some(working.detail.unwrap_or_else(|| {
                            crate::i18n::t!("changes.loadFailed")
                        })));
                    }
                    Err(error) => {
                        store.git_changes.set(Vec::new());
                        store.git_conflict.set(None);
                        store.selected_git_path.set(None);
                        store.clear_git_diff();
                        store.git_error.set(Some(error.to_string()));
                    }
                }
                store.git_loading.set(false);
            });
            return;
        }
        if self.offline_demo {
            self.git_loading.set(false);
            return;
        }
        self.git_loading.set(false);
        self.git_error.set(Some(crate::i18n::t!("files.notConnected")));
    }

    pub fn commit_git_changes(&self, amend: bool, push: bool) {
        let Some(context) = self.current_git_context() else {
            self.git_operation_error.set(Some(crate::i18n::t!("git.selectRepoRequired")));
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let selected = self.git_changes.get().into_iter().filter(|change| change.staged);
            let mut paths = Vec::new();
            for change in selected {
                if let Some(old) = change.orig_path {
                    paths.push(old);
                }
                if !paths.contains(&change.path) {
                    paths.push(change.path);
                }
            }
            if paths.is_empty() && !amend {
                return;
            }
            let message = self.commit_draft.get();
            let draft_revision = self.commit_draft_revision.get();
            let input = falcon_proto::GitCommitInput {
                message: message.clone(),
                all: false,
                paths: Some(paths),
                amend: Some(amend),
                push: Some(push),
            };
            if !self.git_mutation_in_flight.lock().insert(context.clone()) {
                return;
            }
            self.git_operation_busy.set(Some(context.clone()));
            self.git_operation_error.set(None);
            let store = self.clone();
            self.spawn_root(async move {
                let result = client.git_commit_changes(
                    &context.project_id,
                    &input,
                    context.repo.as_deref(),
                ).await;
                store.git_mutation_in_flight.lock().remove(&context);
                if store.git_operation_busy.get().as_ref() == Some(&context) {
                    store.git_operation_busy.set(None);
                }
                if !store.git_context_is_current(&context) {
                    return;
                }
                match result {
                    Ok(result) if result.ok => {
                        store.git_operation_error.set(None);
                        if store.commit_draft_revision.get() == draft_revision
                            && store.commit_draft.get() == message
                        {
                            store.set_commit_draft(String::new());
                        }
                        store.refresh_git_context(context);
                    }
                    Ok(result) => store.git_operation_error.set(Some(result.detail)),
                    Err(error) => store.git_operation_error.set(Some(error.to_string())),
                }
            });
        } else {
            self.git_operation_error.set(Some(crate::i18n::t!("files.notConnected")));
        }
        #[cfg(target_arch = "wasm32")]
        self.git_operation_error.set(Some(crate::i18n::t!("files.notConnected")));
    }

    fn make_git_confirmation(
        &self,
        operation: GitConfirmedOperation,
        paths: Vec<String>,
        untracked: Vec<String>,
    ) {
        let Some(context) = self.current_git_context() else {
            self.git_operation_error.set(Some(crate::i18n::t!("git.selectRepoRequired")));
            return;
        };
        let project_name = self.projects.get().into_iter()
            .find(|project| project.id == context.project_id)
            .map(|project| project.name)
            .unwrap_or_else(|| context.project_id.clone());
        self.git_operation_error.set(None);
        self.git_confirmation.set(Some(GitConfirmation {
            context,
            project_name,
            operation,
            paths,
            untracked,
        }));
    }

    pub fn ask_discard_git_changes(&self, all: bool) {
        let selected: Vec<_> = self.git_changes.get().into_iter()
            .filter(|change| all || change.staged)
            .collect();
        if selected.is_empty() { return; }
        let mut paths = Vec::new();
        let mut untracked = Vec::new();
        let mut display_paths = Vec::new();
        for change in selected {
            if change.untracked {
                if !untracked.contains(&change.path) { untracked.push(change.path.clone()); }
                if !display_paths.contains(&change.path) { display_paths.push(change.path); }
                continue;
            }
            if let Some(old) = change.orig_path {
                if !paths.contains(&old) { paths.push(old.clone()); }
                if !display_paths.contains(&old) { display_paths.push(old); }
            }
            if !paths.contains(&change.path) { paths.push(change.path.clone()); }
            if !display_paths.contains(&change.path) { display_paths.push(change.path); }
        }
        self.make_git_confirmation(
            GitConfirmedOperation::Restore {
                paths: paths.clone(),
                untracked: untracked.clone(),
            },
            display_paths,
            untracked,
        );
    }

    pub fn ask_git_abort(&self) {
        self.make_git_confirmation(GitConfirmedOperation::Abort, Vec::new(), Vec::new());
    }

    pub fn ask_git_take(&self, side: falcon_proto::GitTakeSide) {
        let paths: Vec<_> = self.git_changes.get().into_iter()
            .filter(|change| change.staged && git_change_is_conflicted(change))
            .map(|change| change.path)
            .collect();
        if paths.is_empty() { return; }
        self.make_git_confirmation(
            GitConfirmedOperation::Take { side, paths: paths.clone() },
            paths,
            Vec::new(),
        );
    }

    pub fn cancel_git_confirmation(&self) {
        self.git_confirmation.set(None);
    }

    pub fn confirm_git_operation(&self) {
        let Some(confirmation) = self.git_confirmation.get() else { return; };
        self.git_confirmation.set(None);
        if self.connection_generation.get() != confirmation.context.connection_generation
            || self.server_url.get() != confirmation.context.server_url
        {
            self.git_operation_error.set(Some(crate::i18n::t!("git.contextChanged")));
            return;
        }
        let operation = match confirmation.operation {
            GitConfirmedOperation::Restore { paths, untracked } => {
                falcon_proto::GitOpInput::Restore {
                    paths,
                    untracked: (!untracked.is_empty()).then_some(untracked),
                }
            }
            GitConfirmedOperation::Abort => falcon_proto::GitOpInput::Abort,
            GitConfirmedOperation::Take { side, paths } => {
                falcon_proto::GitOpInput::Take { side, paths }
            }
        };
        self.run_git_operation(confirmation.context, operation);
    }

    pub fn continue_git_operation(&self) {
        let Some(context) = self.current_git_context() else { return; };
        self.run_git_operation(context, falcon_proto::GitOpInput::Continue);
    }

    fn run_git_operation(&self, context: GitRequestContext, operation: falcon_proto::GitOpInput) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            if !self.git_mutation_in_flight.lock().insert(context.clone()) { return; }
            self.git_operation_busy.set(Some(context.clone()));
            self.git_operation_error.set(None);
            let store = self.clone();
            self.spawn_root(async move {
                let result = client.git_op(
                    &context.project_id,
                    &operation,
                    context.repo.as_deref(),
                ).await;
                store.git_mutation_in_flight.lock().remove(&context);
                if store.git_operation_busy.get().as_ref() == Some(&context) {
                    store.git_operation_busy.set(None);
                }
                if !store.git_context_is_current(&context) { return; }
                match result {
                    Ok(result) if result.ok => {
                        store.git_operation_error.set(None);
                        store.refresh_git_context(context);
                    }
                    Ok(result) => store.git_operation_error.set(Some(result.detail)),
                    Err(error) => store.git_operation_error.set(Some(error.to_string())),
                }
            });
        } else {
            self.git_operation_error.set(Some(crate::i18n::t!("files.notConnected")));
        }
        #[cfg(target_arch = "wasm32")]
        self.git_operation_error.set(Some(crate::i18n::t!("files.notConnected")));
    }

    pub fn refresh_relays(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.relay_actions_available() {
                self.show_relay_unavailable();
                return;
            }
            self.queue_current_relay_refresh();
        }
        #[cfg(target_arch = "wasm32")]
        self.show_relay_unavailable();
    }

    pub fn set_relay_enabled(&self, id: &str, share: bool, enabled: bool) {
        let connection_generation = self.connection_generation();
        let request_generation = self.connection_request_generation();
        self.set_relay_enabled_for_context(
            id,
            share,
            enabled,
            connection_generation,
            request_generation,
        );
    }

    pub(crate) fn set_relay_enabled_for_context(
        &self,
        id: &str,
        share: bool,
        enabled: bool,
        connection_generation: u64,
        request_generation: u64,
    ) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let origin_url = crate::project_operations::canonical_server_url(&self.server_url.get());
            if !self.relay_context_is_current(&origin_url, connection_generation, request_generation) {
                return;
            }
            if !self.relay_actions_available() {
                self.show_relay_unavailable();
                return;
            }
            let Some(client) = self.client() else {
                self.show_relay_unavailable();
                return;
            };
            let store = self.clone();
            let id = id.to_string();
            let key = RelayWriteKey {
                origin_url: origin_url.clone(),
                kind: RelayWriteKind::Toggle { id: id.clone(), share },
            };
            let Some(token) = self.begin_relay_write(&key) else {
                return;
            };
            self.spawn_root(async move {
                let result = if share {
                    client
                        .update_share(
                            &id,
                            &falcon_proto::PublicSharePatch {
                                enabled: Some(enabled),
                                ..Default::default()
                            },
                        )
                        .await
                        .map(relay_record_from_share)
                } else {
                    client
                        .update_forward(
                            &id,
                            &falcon_proto::PortForwardPatch {
                                enabled: Some(enabled),
                                ..Default::default()
                            },
                        )
                        .await
                        .map(relay_record_from_forward)
                };
                store.finish_relay_write_result(
                    client,
                    key,
                    token,
                    origin_url,
                    connection_generation,
                    request_generation,
                    result.map_err(|error| error.to_string()),
                );
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (id, share, enabled, connection_generation, request_generation);
            self.show_relay_unavailable();
        }
    }

    pub fn create_public_share(&self, host: &str, port: u16, name: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.relay_actions_available() {
                self.show_relay_unavailable();
                return;
            }
            let Some(client) = self.client() else {
                self.show_relay_unavailable();
                return;
            };
            let origin_url = crate::project_operations::canonical_server_url(&self.server_url.get());
            let connection_generation = self.connection_generation();
            let request_generation = self.connection_request_generation();
            let host = host.to_string();
            let key = RelayWriteKey {
                origin_url: origin_url.clone(),
                kind: RelayWriteKind::CreateShare { host: host.clone() },
            };
            let Some(token) = self.begin_relay_write(&key) else {
                return;
            };
            let store = self.clone();
            let input = falcon_proto::PublicShareInput {
                host_id: (!host.is_empty()).then(|| host.to_string()),
                name: (!name.is_empty()).then(|| name.to_string()),
                dest_host: None,
                dest_port: port,
                enabled: Some(true),
            };
            self.spawn_root(async move {
                let result = client
                    .create_share(&input)
                    .await
                    .map(relay_record_from_share)
                    .map_err(|error| error.to_string());
                store.finish_relay_write_result(
                    client,
                    key,
                    token,
                    origin_url,
                    connection_generation,
                    request_generation,
                    result,
                );
            });
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (host, port, name);
        #[cfg(target_arch = "wasm32")]
        self.show_relay_unavailable();
    }

    pub fn create_port_forward(
        &self,
        host: &str,
        remote: bool,
        bind: u16,
        destination: &str,
        port: u16,
        name: &str,
    ) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.relay_actions_available() {
                self.show_relay_unavailable();
                return;
            }
            let Some(client) = self.client() else {
                self.show_relay_unavailable();
                return;
            };
            let origin_url = crate::project_operations::canonical_server_url(&self.server_url.get());
            let connection_generation = self.connection_generation();
            let request_generation = self.connection_request_generation();
            let host = host.to_string();
            let key = RelayWriteKey {
                origin_url: origin_url.clone(),
                kind: RelayWriteKind::CreateForward { host: host.clone() },
            };
            let Some(token) = self.begin_relay_write(&key) else {
                return;
            };
            let store = self.clone();
            let input = falcon_proto::PortForwardInput {
                host_id: host.clone(),
                name: (!name.is_empty()).then(|| name.to_string()),
                kind: if remote {
                    falcon_proto::ForwardKind::Remote
                } else {
                    falcon_proto::ForwardKind::Local
                },
                bind_host: None,
                bind_port: bind,
                dest_host: (!destination.is_empty()).then(|| destination.to_string()),
                dest_port: port,
                enabled: Some(true),
            };
            self.spawn_root(async move {
                let result = client
                    .create_forward(&input)
                    .await
                    .map(relay_record_from_forward)
                    .map_err(|error| error.to_string());
                store.finish_relay_write_result(
                    client,
                    key,
                    token,
                    origin_url,
                    connection_generation,
                    request_generation,
                    result,
                );
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (host, remote, bind, destination, port, name);
            self.show_relay_unavailable();
        }
    }

    pub(crate) fn relay_toggle_pending(&self, id: &str, share: bool) -> bool {
        self.relay_writes.get().contains_key(&RelayWriteKey {
            origin_url: crate::project_operations::canonical_server_url(&self.server_url.get()),
            kind: RelayWriteKind::Toggle { id: id.to_string(), share },
        })
    }

    pub(crate) fn relay_create_pending(&self, host: &str, share: bool) -> bool {
        self.relay_writes.get().contains_key(&RelayWriteKey {
            origin_url: crate::project_operations::canonical_server_url(&self.server_url.get()),
            kind: if share {
                RelayWriteKind::CreateShare { host: host.to_string() }
            } else {
                RelayWriteKind::CreateForward { host: host.to_string() }
            },
        })
    }

    pub(crate) fn relay_actions_available(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.server_connected.get() && self.client().is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    fn show_relay_unavailable(&self) {
        self.show_toast(crate::i18n::t!("forward.unavailable"));
    }

    fn next_relay_write_token(&self) -> u64 {
        let token = self.next_relay_write_token.get().wrapping_add(1);
        self.next_relay_write_token.set(token);
        token
    }

    fn begin_relay_write(&self, key: &RelayWriteKey) -> Option<u64> {
        let token = self.next_relay_write_token();
        let mut started = false;
        self.relay_writes.update(|pending| {
            if !pending.contains_key(key) {
                pending.insert(key.clone(), token);
                started = true;
            }
        });
        started.then_some(token)
    }

    fn finish_relay_write(&self, key: &RelayWriteKey, token: u64) {
        self.relay_writes.update(|pending| {
            if pending.get(key) == Some(&token) {
                pending.remove(key);
            }
        });
    }

    fn relay_context_is_current(&self, origin_url: &str, connection_generation: u64, request_generation: u64) -> bool {
        self.connection_generation() == connection_generation
            && self.connection_request_generation() == request_generation
            && crate::project_operations::canonical_server_url(&self.server_url.get_untracked()) == origin_url
    }

    fn upsert_relay(&self, record: RelayRecord) {
        self.relays.update(|relays| {
            if let Some(existing) = relays.iter_mut().find(|existing| existing.id == record.id) {
                *existing = record;
            } else {
                relays.push(record);
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn finish_relay_write_result(
        &self,
        client: FalconClient,
        key: RelayWriteKey,
        token: u64,
        origin_url: String,
        connection_generation: u64,
        request_generation: u64,
        result: Result<RelayRecord, String>,
    ) {
        let current = self.relay_context_is_current(&origin_url, connection_generation, request_generation);
        if current {
            match result {
                Ok(record) => {
                    self.upsert_relay(record);
                    self.finish_relay_write(&key, token);
                    self.queue_relay_refresh(client, &origin_url, connection_generation, request_generation);
                }
                Err(error) => {
                    self.finish_relay_write(&key, token);
                    self.show_toast(error);
                }
            }
        } else {
            self.finish_relay_write(&key, token);
            if crate::project_operations::canonical_server_url(&self.server_url.get_untracked()) == origin_url {
                self.queue_current_relay_refresh();
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn queue_current_relay_refresh(&self) {
        if let Some(client) = self.client() {
            let origin_url = crate::project_operations::canonical_server_url(&self.server_url.get_untracked());
            self.queue_relay_refresh(
                client,
                &origin_url,
                self.connection_generation(),
                self.connection_request_generation(),
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn queue_relay_refresh(
        &self,
        client: FalconClient,
        origin_url: &str,
        connection_generation: u64,
        request_generation: u64,
    ) {
        let origin_url = crate::project_operations::canonical_server_url(origin_url);
        if !self.relay_context_is_current(&origin_url, connection_generation, request_generation) {
            return;
        }
        let key = RelayRefreshKey { origin_url, connection_generation, request_generation };
        let mut request = Some((key, client));
        let should_spawn = {
            let mut queue = self.relay_refresh_queue.lock();
            if queue.in_flight {
                queue.trailing = request.take();
                false
            } else {
                queue.in_flight = true;
                true
            }
        };
        if should_spawn {
            let (key, client) = request.expect("new relay refresh request");
            self.spawn_relay_refresh(client, key);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn finish_relay_refresh(&self) -> bool {
        let next = {
            let mut queue = self.relay_refresh_queue.lock();
            if queue.trailing.is_some() {
                queue.trailing.take()
            } else {
                queue.in_flight = false;
                None
            }
        };
        if let Some((key, client)) = next {
            self.spawn_relay_refresh(client, key);
            true
        } else {
            false
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_relay_refresh(&self, client: FalconClient, key: RelayRefreshKey) {
        let store = self.clone();
        self.spawn_root(async move {
            if !store.relay_context_is_current(&key.origin_url, key.connection_generation, key.request_generation) {
                store.finish_relay_refresh();
                return;
            }
            let result = client.list_relays().await;
            if store.finish_relay_refresh() { return; }
            if !store.relay_context_is_current(&key.origin_url, key.connection_generation, key.request_generation) {
                return;
            }
            match result {
                Ok(list) => {
                    let mut rules: Vec<_> = list.forwards.into_iter().map(relay_record_from_forward).collect();
                    rules.extend(list.shares.into_iter().map(relay_record_from_share));
                    store.relays.set(rules);
                }
                Err(error) => store.show_toast(error.to_string()),
            }
        });
    }

    /// 切换连接前先保存旧工作区，再读取目标服务端的布局，避免把旧布局写进新配置。
    fn switch_workspace(&self, url: &str) {
        if self.server_url.get().trim_end_matches('/') == url.trim_end_matches('/') {
            return;
        }
        self.next_connection_generation();
        self.server_connected.set(false);
        self.host_delete_busy.set(None);
        self.host_delete_error.set(None);
        self.invalidate_host_edit_generation();
        self.editing_host.set(None);
        if matches!(self.active_dialog.get(), ActiveDialog::ConfirmDeleteHost { .. }) {
            self.active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Hosts));
        }
        self.quick_open_project.set(String::new());
        self.quick_open_paths.set(Vec::new());
        self.quick_open_truncated.set(false);
        self.quick_open_generation
            .set(self.quick_open_generation.get().wrapping_add(1));
        self.quick_open_loading.set(false);
        self.quick_open_error.set(None);
        self.persist_workspace();
        for session in self.terminals.map.lock().values() {
            session.detach_socket();
        }
        self.terminals.map.lock().clear();
        if !self
            .profiles
            .get()
            .profiles
            .iter()
            .any(|p| p.url.trim_end_matches('/') == url.trim_end_matches('/'))
        {
            self.profiles.update(|profiles| {
                profiles.upsert(ServerProfile {
                    id: falcon_core::profiles::new_profile_id(),
                    name: url.to_string(),
                    url: url.to_string(),
                    plaintext_ok: false,
                })
            });
        }
        self.server_url.set(url.to_string());
        self.restore_workspace(url);
        self.server_info.set(None);
        self.detected_agents.set(Vec::new());
        self.files_loading.set(false);
        self.file_error.set(None);
        self.file_directory.set(String::new());
        self.git_loading.set(false);
        self.git_error.set(None);
        self.git_working_generation.set(self.git_working_generation.get().wrapping_add(1));
        self.git_diff_generation.set(self.git_diff_generation.get().wrapping_add(1));
        self.git_diff_loading.set(false);
        self.git_diff_error.set(None);
        self.git_diff_truncated.set(false);
        self.git_conflict.set(None);
        self.selected_git_repo.set(None);
        self.git_operation_busy.set(None);
        self.git_operation_error.set(None);
        self.git_confirmation.set(None);
        self.projects.set(Vec::new());
        self.sessions.set(Vec::new());
        self.hosts.set(Vec::new());
        self.relays.set(Vec::new());
        self.file_entries.set(Vec::new());
        self.open_files.set(HashMap::new());
        self.git_changes.set(Vec::new());
        self.git_diff.set(None);
        self.agent_views.set(HashMap::new());
        self.plugin_states.set(HashMap::new());
        self.create_session_in_flight.lock().clear();
        self.plugin_list.set(PluginList {
            dir: String::new(),
            plugins: Vec::new(),
            trust_root: None,
        });
    }

    /// 沿用 GPUI 的 profile id 与 JSON 形状，直接读取已安装客户端的工作区。
    fn workspace_profile_id(&self) -> String {
        let url = self.server_url.get();
        self.profiles
            .get()
            .profiles
            .iter()
            .find(|p| p.url.trim_end_matches('/') == url.trim_end_matches('/'))
            .map(|p| p.id.clone())
            .unwrap_or_else(|| "local".into())
    }

    fn restore_workspace(&self, _url: &str) {
        use falcon_core::workspace::{ActiveView, RightPanelId, load_workspace, restore_columns};
        let raw = falcon_platform::shared().load_workspace(&self.workspace_profile_id());
        let saved = load_workspace(raw.as_deref());
        self.columns.set(restore_columns(&saved));
        self.overview_open
            .set(matches!(saved.active, ActiveView::Overview));
        self.focused_pane_key.set(match saved.active {
            ActiveView::Terminal { session_id } => term_key(&session_id),
            _ => String::new(),
        });
        self.active_project_id
            .set(saved.selected_project_id.unwrap_or_default());
        self.sidebar_open.set(saved.sidebar_open);
        self.sidebar_width.set(saved.sidebar_width as f32);
        self.right_panel_open.set(saved.right_open);
        self.right_panel_width.set(saved.right_width as f32);
        self.right_panel_kind.set(match saved.right_panel {
            RightPanelId::Files => RightPanelKind::Files,
            RightPanelId::Changes => RightPanelKind::Git,
            RightPanelId::Plugin(id) => RightPanelKind::Plugin(id),
        });
        self.collapsed.set(saved.collapsed);
        self.sessions_open.set(saved.sessions_open);
        self.term_zoomed.set(saved.term_zoomed);
    }

    fn persist_workspace(&self) {
        use falcon_core::workspace::{
            ActiveView, PersistedColumn, PersistedWorkspace, RightPanelId, serialize_workspace,
        };
        let columns = self.columns.get();
        let focused = self.focused_pane_key.get();
        let selected = self.active_project_id.get();
        let persisted = PersistedWorkspace {
            tabs: pane_keys(&columns)
                .iter()
                .filter_map(|k| match parse_pane_key(k) {
                    Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) => Some(id),
                    _ => None,
                })
                .collect(),
            columns: columns
                .iter()
                .filter_map(|c| {
                    let panes: Vec<_> = c
                        .panes
                        .iter()
                        .filter(|p| p.key.starts_with("t:"))
                        .cloned()
                        .collect();
                    (!panes.is_empty()).then(|| PersistedColumn {
                        basis: c.basis,
                        panes,
                        pinned: c.pinned,
                        canvas: if c.pinned { None } else { c.canvas.clone() },
                    })
                })
                .collect(),
            active: if self.overview_open.get() {
                ActiveView::Overview
            } else {
                match parse_pane_key(&focused) {
                    Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) => {
                        ActiveView::Terminal { session_id: id }
                    }
                    _ if !selected.is_empty() => ActiveView::Project,
                    _ => ActiveView::Overview,
                }
            },
            sidebar_open: self.sidebar_open.get(),
            sidebar_width: self.sidebar_width.get() as f64,
            right_open: self.right_panel_open.get(),
            right_width: self.right_panel_width.get() as f64,
            right_panel: match self.right_panel_kind.get() {
                RightPanelKind::Files => RightPanelId::Files,
                RightPanelKind::Git => RightPanelId::Changes,
                RightPanelKind::Plugin(id) => RightPanelId::Plugin(id),
            },
            collapsed: self.collapsed.get(),
            sessions_open: self.sessions_open.get(),
            selected_project_id: (!selected.is_empty()).then_some(selected),
            term_zoomed: self.term_zoomed.get(),
            ..Default::default()
        };
        let json = serialize_workspace(&persisted);
        let platform = falcon_platform::shared();
        let profile = self.workspace_profile_id();
        if platform.load_workspace(&profile).as_deref() != Some(json.as_str()) {
            platform.store_workspace(&profile, &json);
        }
    }

    pub fn browse_directory(&self, path: &str) {
        let project = self.active_project_id.get();
        let mapping_generation = self.project_operations.mapping_generation(&project);
        if !self
            .project_operations
            .mapping_is_current(&project, &mapping_generation)
        {
            self.files_loading.set(false);
            self.file_entries.set(Vec::new());
            self.file_error
                .set(Some(crate::i18n::t!("project.mappingChangePending").to_string()));
            return;
        }
        let project_path = self
            .projects
            .get()
            .into_iter()
            .find(|record| record.id == project)
            .map_or_else(String::new, |record| record.path);
        let directory = if path.is_empty() && !project_path.is_empty() {
            project_path
        } else {
            path.to_string()
        };
        let scope = self.file_operations.begin_directory_request(self, &directory);
        self.file_error.set(None);
        let request_generation = self.directory_request_generation.get().wrapping_add(1);
        self.directory_request_generation.set(request_generation);
        let connection_generation = self.connection_generation.get();

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            self.files_loading.set(true);
            let operations = self.file_operations.clone();
            let scope = scope.clone();
            self.spawn_root(async move {
                let result = client.list_files(&project, Some(&directory)).await;
                if store.connection_generation.get() != connection_generation
                    || store.directory_request_generation.get() != request_generation
                    || store.active_project_id.get() != project
                    || !operations.scope_is_current(&store, &scope)
                {
                    return;
                }
                match result {
                    Ok(listing) => {
                        let canonical_path = if listing.path.is_empty() {
                            directory.clone()
                        } else {
                            listing.path
                        };
                        let entries: Vec<FileEntryItem> = listing
                            .entries
                            .into_iter()
                            .map(|e| FileEntryItem {
                                is_html: e.path.ends_with(".html") || e.path.ends_with(".htm"),
                                name: e.name,
                                path: e.path,
                                is_dir: e.kind == WorkspaceEntryKind::Dir,
                                size_bytes: e.size.unwrap_or(0),
                            })
                            .collect();
                        if operations.apply_listing(&store, &scope, &canonical_path, &entries) {
                            store.file_directory.set(canonical_path);
                            store.file_entries.set(entries);
                        }
                    }
                    Err(e) => {
                        store.file_error.set(Some(e.to_string()));
                        store.file_entries.set(Vec::new());
                    }
                }
                store.files_loading.set(false);
            });
        } else {
            self.files_loading.set(false);
            self.file_entries.set(Vec::new());
            if !self.offline_demo {
                self.file_error
                    .set(Some(crate::i18n::t!("files.notConnected")));
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.files_loading.set(false);
            self.file_entries.set(Vec::new());
            self.file_error
                .set(Some(crate::i18n::t!("files.notConnected")));
        }
    }

    fn activate_project(&self, project_id: &str) {
        if !self.set_active_project_id(project_id.to_string()) {
            return;
        }
        self.selected_git_path.set(None);
        self.clear_git_diff();
        self.git_conflict.set(None);
        self.git_operation_error.set(None);
        self.refresh_project_data();
        self.refresh_project_agents();
    }

    fn set_active_project_id(&self, project_id: String) -> bool {
        if self.active_project_id.get() == project_id {
            return false;
        }
        self.file_operations.project_changed(self);
        self.active_project_id.set(project_id);
        true
    }

    pub fn select_project(&self, project_id: &str) {
        self.selected_git_path.set(None);
        self.clear_git_diff();
        self.overview_open.set(false);
        self.activate_project(project_id);
        // 选项目只切换该项目的画布，不自动接回收起的会话。
        if let Some(session_id) = self
            .sessions
            .get()
            .iter()
            .find(|s| s.project_id == project_id && self.is_session_attached(&s.id))
            .map(|s| s.id.clone())
        {
            self.focus_session(&session_id);
        } else {
            self.focused_pane_key.set(String::new());
        }
    }

    fn refresh_project_agents(&self) {
        let project = self.active_project_id.get();
        self.detected_agents.set(Vec::new());
        if project.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let connection = self.connection_generation.get();
            self.spawn_root(async move {
                let result = client
                    .agents(&falcon_client::ProbeTarget::Project(project.clone()))
                    .await;
                if store.connection_generation.get() != connection
                    || store.active_project_id.get() != project
                {
                    return;
                }
                match result {
                    Ok(agents) => store.detected_agents.set(
                        agents
                            .into_iter()
                            .map(|agent| agent.as_str().to_string())
                            .collect(),
                    ),
                    Err(_) => store.detected_agents.set(Vec::new()),
                }
            });
        }
    }

    pub fn toggle_right_panel(&self, kind: RightPanelKind) {
        if self.right_panel_open.get() && self.right_panel_kind.get() == kind {
            self.right_panel_open.set(false);
        } else {
            self.right_panel_kind.set(kind);
            self.right_panel_open.set(true);
        }
    }

    pub fn term_prefs(&self) -> TermPref {
        self.term_prefs.get()
    }

    /// 获取或惰性创建某条会话的 `TerminalSession` 实例。
    pub fn ensure_terminal_session(&self, session_id: &str) -> Arc<TerminalSession> {
        if let Some(existing) = self.terminals.map.lock().get(session_id).cloned() {
            return existing;
        }
        let durable = self
            .sessions
            .get()
            .iter()
            .find(|s| s.id == session_id)
            .map_or(true, |s| s.durable);
        let created = TerminalSession::new_offline(session_id.to_string(), 80, 24, durable);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.terminals.client.lock().clone() {
            let resolved = use_theme().resolved();
            created.connect(&client, &resolved, false);
        }
        self.terminals
            .map
            .lock()
            .insert(session_id.to_string(), created.clone());
        created
    }

    /// 判断某条会话此刻是否挂在工作区的任意列上（未挂即处于 Detach 后台运行态）。
    pub fn is_session_attached(&self, session_id: &str) -> bool {
        let key = term_key(session_id);
        find_pane(&self.columns.get(), &key).is_some()
    }

    /// 聚焦指定会话窗口；若该会话处于 Detach 后台态，则自动接回（Reattach）到当前画布最右列。
    pub fn focus_session(&self, session_id: &str) {
        self.overview_open.set(false);
        if let Some(session) = self
            .sessions
            .get()
            .into_iter()
            .find(|session| session.id == session_id)
        {
            self.activate_project(&session.project_id);
            self.sessions_open.update(|open| {
                open.insert(session.project_id, true);
            });
        }
        let key = term_key(session_id);
        let cols = self.columns.get();
        if find_pane(&cols, &key).is_none() {
            let cid = self.active_canvas_id.get();
            let next = insert_column(&cols, &key, cols.len(), Some(&cid));
            self.columns.set(next);
        } else if let Some(at) = find_pane(&cols, &key)
            && let Some(canvas) = cols[at.col].canvas.as_ref()
        {
            self.active_canvas_id.set(canvas.clone());
        }
        self.focused_pane_key.set(key);
        // 查看后清除已完成/错误的程序状态提醒（ADR 0020）
        self.sessions.update(|list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == session_id) {
                if matches!(
                    s.program_status.as_ref().map(|(st, _)| st),
                    Some(ProgramState::Done | ProgramState::Error)
                ) {
                    s.program_status = None;
                }
            }
        });
    }

    pub fn focus_palette_session(&self, session_id: &str) {
        let Some(session) = self.sessions.get().into_iter().find(|s| s.id == session_id) else {
            return;
        };
        match session.state {
            SessionState::Active => self.focus_session(session_id),
            SessionState::Dead => {}
            SessionState::Unverified | SessionState::Unknown => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(client) = self.client() {
                    let generation = self.connection_generation.get();
                    let key = (generation, session_id.to_string());
                    if !self.reattach_in_flight.lock().insert(key.clone()) {
                        return;
                    }
                    let store = self.clone();
                    let id = session_id.to_string();
                    self.spawn_root(async move {
                        match client.reattach_session(&id).await {
                            Ok(updated) if store.connection_generation.get() == generation => {
                                let state = updated.state;
                                store.sessions.update(|sessions| {
                                    if let Some(row) = sessions.iter_mut().find(|row| row.id == id)
                                    {
                                        row.state = state;
                                    }
                                });
                                if state == SessionState::Active {
                                    store.focus_session(&id);
                                }
                            }
                            Err(error) if store.connection_generation.get() == generation => {
                                store.show_toast(error.to_string())
                            }
                            _ => {}
                        }
                        store.reattach_in_flight.lock().remove(&key);
                    });
                } else {
                    self.show_toast(crate::i18n::t!("native.nsh.serverNotConnected"));
                }
            }
        }
    }

    /// 新建一扇当前焦点项目的持久终端窗口并排入当前画布。
    pub fn can_create_session(&self) -> bool {
        !self.active_project_id.get().is_empty()
            && (self.server_connected.get() || self.offline_demo)
    }

    pub fn can_open_quick_open(&self) -> bool {
        !self.active_project_id.get().is_empty()
            && (self.server_connected.get() || self.offline_demo)
    }

    pub fn create_terminal_in_active_project(&self) -> String {
        self.create_terminal_with_request(CreateSessionRequest::default(), SessionPlacement::Focus)
    }

    pub fn create_terminal_with_request(
        &self,
        request: CreateSessionRequest,
        placement: SessionPlacement,
    ) -> String {
        let proj_id = self.active_project_id.get();
        if proj_id.is_empty() {
            self.active_dialog.set(ActiveDialog::NewProject);
            return String::new();
        }
        let proj_path = self
            .projects
            .get()
            .into_iter()
            .find(|p| p.id == proj_id)
            .map_or_else(String::new, |p| p.path);
        let connection_generation = self.connection_generation.get();
        let anchor = self.focused_pane_key.get();
        let canvas = self.active_canvas_id.get();
        #[cfg(not(target_arch = "wasm32"))]
        let metadata_url = self.server_url.get();

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let request_key = (connection_generation, proj_id.clone());
            if !self
                .create_session_in_flight
                .lock()
                .insert(request_key.clone())
            {
                return String::new();
            }
            let store = self.clone();
            let pid = proj_id.clone();
            let ppath = proj_path.clone();
            self.spawn_root(async move {
                store.bump_metadata_revision(&metadata_url);
                let result = client.create_session(&pid, &request).await;
                store.bump_metadata_revision(&metadata_url);
                if store.connection_generation.get() == connection_generation {
                    match result {
                        Ok(created) => {
                            let sid = created.id.clone();
                            let rec = SessionRecord {
                                id: sid.clone(),
                                project_id: created.project_id,
                                name: created.name,
                                auto_title: created.title.unwrap_or_default(),
                                cwd: ppath,
                                durable: created.durable,
                                state: created.state,
                                agent: created.agent.map(|a| a.as_str().to_string()),
                                program_status: None,
                                foreground_cmd: None,
                            };
                            store.sessions.update(|v| v.push(rec));
                            if store.active_project_id.get() == pid {
                                store.place_created_session(&sid, placement, &anchor, &canvas);
                            }
                        }
                        Err(err) => store.show_toast(err.to_string()),
                    }
                }
                store.create_session_in_flight.lock().remove(&request_key);
            });
            return String::new();
        }

        if !self.offline_demo {
            self.show_toast(crate::i18n::t!("native.nsh.serverNotConnected"));
            return String::new();
        }

        let idx = self.sessions.get().len() + 1;
        let id = format!("sess-{idx}");
        let rec = SessionRecord {
            id: id.clone(),
            project_id: proj_id,
            name: String::new(),
            auto_title: String::new(),
            cwd: proj_path.clone(),
            durable: true,
            state: SessionState::Active,
            agent: request
                .agent
                .as_ref()
                .map(|agent| agent.as_str().to_string()),
            program_status: None,
            foreground_cmd: None,
        };
        let sess = TerminalSession::new_offline(&id, 80, 24, true);
        sess.feed_output(format!("\x1b[90m{proj_path}\x1b[0m $ ").as_bytes());
        self.terminals.map.lock().insert(id.clone(), sess);
        self.sessions.update(|v| v.push(rec));
        self.place_created_session(&id, placement, &anchor, &canvas);
        id
    }

    fn place_created_session(
        &self,
        session_id: &str,
        placement: SessionPlacement,
        anchor: &str,
        canvas: &str,
    ) {
        let key = term_key(session_id);
        let columns = self.columns.get();
        match placement {
            SessionPlacement::Focus => self.focus_session(session_id),
            SessionPlacement::Right => {
                let at = find_pane(&columns, anchor);
                let col = at.map_or(columns.len(), |at| at.col + 1);
                let target_canvas = at
                    .and_then(|at| columns[at.col].canvas.as_deref())
                    .unwrap_or(canvas);
                self.columns
                    .set(insert_column(&columns, &key, col, Some(target_canvas)));
                self.active_canvas_id.set(target_canvas.to_string());
                self.focused_pane_key.set(key);
            }
            SessionPlacement::Down => {
                if let Some(at) = find_pane(&columns, anchor) {
                    self.columns.set(insert_pane(
                        &columns,
                        &key,
                        falcon_core::layout::PaneAt {
                            col: at.col,
                            index: at.index + 1,
                        },
                    ));
                } else {
                    self.columns
                        .set(insert_column(&columns, &key, columns.len(), Some(canvas)));
                }
                self.active_canvas_id.set(canvas.to_string());
                self.focused_pane_key.set(key);
            }
        }
    }

    /// **Detach（收起窗口）**：仅从列式工作区移除该窗口并断开本地 Viewer socket，**绝不结束后台会话**。
    pub fn detach_pane(&self, pane_key: &str) {
        if matches!(
            parse_pane_key(pane_key),
            Some(falcon_core::pane_key::PaneItem::File { .. })
        ) {
            self.request_close_file(pane_key);
            return;
        }
        self.detach_pane_now(pane_key);
    }

    fn detach_pane_now(&self, pane_key: &str) {
        if let Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) = parse_pane_key(pane_key)
        {
            // 接回时必须重新建立 Viewer；缓存一个已断开 socket 的实例会让终端永远停在旧画面。
            if let Some(sess) = self.terminals.map.lock().remove(&id) {
                sess.detach_socket();
            }
        }
        let next = remove_pane(&self.columns.get(), pane_key);
        let fallback_focus = pane_keys(&next).into_iter().next().unwrap_or_default();
        self.columns.set(next);
        if self.focused_pane_key.get() == pane_key {
            self.focused_pane_key.set(fallback_focus);
        }
        self.open_files.update(|files| {
            files.remove(pane_key);
        });
    }

    pub fn request_rename_session(&self, session_id: &str) {
        self.session_operations.open_rename(self, session_id);
    }

    pub fn request_clear_session_record(&self, session_id: &str) {
        self.session_operations
            .clear_record(self.clone(), session_id);
    }

    /// 请求 **Terminate（终止会话）**：若探到前台仍有非 shell 进程在跑，先弹出确认对话框（2s 探测护栏语义）。
    pub fn request_terminate_session(&self, session_id: &str) {
        if let Some(rec) = self.sessions.get().into_iter().find(|s| s.id == session_id) {
            if let Some(cmd) = rec.foreground_cmd.filter(|c| !c.trim().is_empty()) {
                self.active_dialog.set(ActiveDialog::ConfirmTerminate {
                    session_id: session_id.to_string(),
                    foreground_cmd: cmd,
                });
                return;
            }
        }
        self.force_terminate_session(session_id);
    }

    /// 确认销毁会话：移除工作区窗口、关闭本地终端实例并向服务端发 `POST /api/sessions/:id/terminate`。
    pub fn force_terminate_session(&self, session_id: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let generation = self.connection_generation.get();
            let sid = session_id.to_string();
            let request_key = (generation, sid.clone());
            if !self
                .termination_in_flight
                .lock()
                .insert(request_key.clone())
            {
                return;
            }
            let Some(client) = self.client() else {
                self.termination_in_flight.lock().remove(&request_key);
                if self.offline_demo {
                    self.finish_termination(&sid);
                } else {
                    self.show_toast(crate::i18n::t!("native.nsh.serverNotConnected"));
                }
                return;
            };
            let metadata_url = self.server_url.get();
            let store = self.clone();
            self.spawn_root(async move {
                store.bump_metadata_revision(&metadata_url);
                let result = client.terminate_session(&sid).await;
                store.bump_metadata_revision(&metadata_url);
                match result {
                    Ok(_) if store.connection_generation.get() == generation => {
                        store.finish_termination(&sid);
                    }
                    Ok(_) => {}
                    Err(err) if store.connection_generation.get() == generation => {
                        store.show_toast(err.to_string());
                    }
                    Err(_) => {}
                }
                store.termination_in_flight.lock().remove(&request_key);
            });
        }

        #[cfg(target_arch = "wasm32")]
        self.show_toast(crate::i18n::t!("native.nsh.serverNotConnected"));
    }

    fn finish_termination(&self, session_id: &str) {
        let key = term_key(session_id);
        self.detach_pane_now(&key);
        self.sessions
            .update(|list| list.retain(|s| s.id != session_id));
        self.terminals.map.lock().remove(session_id);
        if matches!(
            self.active_dialog.get(),
            ActiveDialog::ConfirmTerminate { session_id: active, .. } if active == session_id
        ) {
            self.active_dialog.set(ActiveDialog::None);
        }
    }

    /// 在工作区打开一扇文件编辑/预览窗口。
    pub fn open_file_pane(&self, project_id: &str, path: &str) {
        let mapping_generation = self.project_operations.mapping_generation(project_id);
        if !self
            .project_operations
            .mapping_is_current(project_id, &mapping_generation)
        {
            self.show_toast(crate::i18n::t!("project.mappingChangePending"));
            return;
        }
        if self.file_operations.blocks_file_open(self) {
            if let Some(message) = self.file_operations.file_open_lock_message(self) {
                self.show_toast(message);
            }
            return;
        }
        let key = file_key(project_id, path);
        if !self.open_files.get().contains_key(&key) {
            let generation = self.next_file_generation();
            if self.offline_file_operations() {
                let mut buffer = OpenFileBuffer::from_disk_text(
                    project_id,
                    path,
                    &crate::i18n::t!("native.nsh.offlineDemoFileContent", path = path),
                    "offline-demo",
                    None,
                );
                buffer.generation = generation;
                self.open_files.update(|files| {
                    files.insert(key.clone(), buffer);
                });
            } else {
                self.start_file_load(project_id, path, &key, generation);
            }
        }
        let cols = self.columns.get();
        if find_pane(&cols, &key).is_none() {
            let cid = self.active_canvas_id.get();
            let next = insert_column(&cols, &key, cols.len(), Some(&cid));
            self.columns.set(next);
        }
        self.focused_pane_key.set(key);
    }

    fn start_file_load(&self, project_id: &str, path: &str, key: &str, generation: u64) {
        self.open_files.update(|files| {
            files.insert(
                key.to_string(),
                OpenFileBuffer::loading(project_id, path, generation),
            );
        });
        let connection_generation = self.connection_generation.get();
        let mapping_generation = self.project_operations.mapping_generation(project_id);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let project = project_id.to_string();
            let file_path = path.to_string();
            let file_key = key.to_string();
            let base_url = self.server_url.get();
            self.spawn_root(async move {
                let result = client.read_file(&project, &file_path).await;
                if !file_response_is_current(
                    store.connection_generation.get(),
                    connection_generation,
                    generation,
                    generation,
                ) {
                    return;
                }
                if !store
                    .project_operations
                    .mapping_is_current(&project, &mapping_generation)
                {
                    return;
                }
                store.open_files.update(|files| {
                    let Some(buffer) = files.get(&file_key) else {
                        return;
                    };
                    if !file_response_is_current(
                        store.connection_generation.get(),
                        connection_generation,
                        buffer.generation,
                        generation,
                    ) {
                        return;
                    }
                    match result {
                        Ok(wf) => {
                            let rel = wf.raw_path.as_deref().unwrap_or(&file_path);
                            let raw = raw_url(&wf.raw_base, rel);
                            let raw = (file_path.ends_with(".html") || file_path.ends_with(".htm"))
                                .then(|| {
                                    if raw.starts_with("http://") || raw.starts_with("https://") {
                                        raw
                                    } else {
                                        format!(
                                            "{}/{}",
                                            base_url.trim_end_matches('/'),
                                            raw.trim_start_matches('/')
                                        )
                                    }
                                });
                            files.insert(
                                file_key.clone(),
                                OpenFileBuffer::from_preview(
                                    project.clone(),
                                    file_path.clone(),
                                    generation,
                                    wf.preview,
                                    raw,
                                ),
                            );
                        }
                        Err(err) => {
                            if let Some(buffer) = files.get_mut(&file_key) {
                                buffer.fail_load(err.to_string());
                            }
                        }
                    }
                });
            });
            return;
        }
        self.open_files.update(|files| {
            if let Some(buffer) = files.get_mut(key)
                && buffer.generation == generation
            {
                buffer.fail_load(crate::i18n::t!("files.notConnected"));
            }
        });
    }

    pub fn preview_git_path(&self, path: &str) {
        let Some(item) = self.git_changes.get().into_iter().find(|change| change.path == path) else {
            return;
        };
        self.selected_git_path.set(Some(path.to_string()));
        self.load_git_diff(item);
    }

    fn clear_git_diff(&self) {
        self.git_diff_generation.set(self.git_diff_generation.get().wrapping_add(1));
        self.git_diff.set(None);
        self.git_diff_loading.set(false);
        self.git_diff_error.set(None);
        self.git_diff_truncated.set(false);
    }

    fn load_git_diff(&self, item: GitChangeItem) {
        let Some(context) = self.current_git_context() else { return; };
        let generation = self.git_diff_generation.get().wrapping_add(1);
        self.git_diff_generation.set(generation);
        self.git_diff.set(None);
        self.git_diff_loading.set(true);
        self.git_diff_error.set(None);
        self.git_diff_truncated.set(false);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let file_ref = falcon_client::GitFileRef {
                path: item.path.clone(),
                orig_path: item.orig_path.clone(),
                untracked: item.untracked,
            };
            self.spawn_root(async move {
                let result = client.git_file_diff(
                    &context.project_id,
                    &file_ref,
                    context.repo.as_deref(),
                ).await;
                let current_item_matches = store.git_changes.get().iter().any(|change| {
                    change.path == item.path
                        && change.orig_path == item.orig_path
                        && change.untracked == item.untracked
                });
                if store.git_diff_generation.get() != generation
                    || !store.git_context_is_current(&context)
                    || store.selected_git_path.get().as_deref() != Some(item.path.as_str())
                    || !current_item_matches
                { return; }
                match result {
                    Ok(diff) if diff.available => {
                        store.git_diff.set(Some(diff.diff));
                        store.git_diff_truncated.set(diff.truncated);
                    }
                    Ok(diff) => {
                        store.git_diff_error.set(Some(diff.detail.unwrap_or_else(|| {
                            crate::i18n::t!("git.diffFailed")
                        })));
                    }
                    Err(error) => store.git_diff_error.set(Some(error.to_string())),
                }
                store.git_diff_loading.set(false);
            });
            return;
        }
        if self.offline_demo {
            let body = format!(
                "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n@@ -1,{1} +1,{2} @@\n{3}{4}",
                item.path,
                item.old_text.lines().count(),
                item.new_text.lines().count(),
                item.old_text.lines().map(|line| format!("-{line}\n")).collect::<String>(),
                item.new_text.lines().map(|line| format!("+{line}\n")).collect::<String>(),
            );
            self.git_diff.set(Some(body));
            self.git_diff_loading.set(false);
        } else {
            self.git_diff_error.set(Some(crate::i18n::t!("files.notConnected")));
            self.git_diff_loading.set(false);
        }
    }

    /// 打开工作区级统一 Diff 窗口（`DIFF_KEY = "d"`）。
    pub fn open_diff_pane(&self) {
        if let Some(path) = self.selected_git_path.get() {
            self.preview_git_path(&path);
        }
        let cols = self.columns.get();
        if find_pane(&cols, DIFF_KEY).is_none() {
            let cid = self.active_canvas_id.get();
            let next = insert_column(&cols, DIFF_KEY, cols.len(), Some(&cid));
            self.columns.set(next);
        }
        self.focused_pane_key.set(DIFF_KEY.to_string());
    }

    /// 保存文件修改（ADR 0023：还原原文件 CRLF/LF 与末尾换行，校验 `disk_hash` 冲突）。
    pub fn save_file_buffer(&self, file_pane_key: &str) -> bool {
        self.save_file_internal(file_pane_key, false, false)
    }

    pub fn overwrite_file_buffer(&self, file_pane_key: &str) -> bool {
        self.save_file_internal(file_pane_key, true, false)
    }

    fn save_file_internal(&self, key: &str, overwrite: bool, close_after: bool) -> bool {
        let mut submission = None;
        let mut already_saving = false;
        let mut clean_close = false;
        self.open_files.update(|files| {
            let Some(buffer) = files.get_mut(key) else {
                return;
            };
            if buffer.saving {
                already_saving = true;
                buffer.close_after_save |= close_after;
                return;
            }
            if !buffer.editable() || (!buffer.is_dirty() && !close_after) {
                clean_close = close_after && !buffer.is_dirty();
                return;
            }
            if buffer.remote_changed_conflict && !overwrite {
                return;
            }
            let base = (!overwrite).then(|| buffer.disk_hash.clone());
            submission = Some((
                buffer.project_id.clone(),
                buffer.path.clone(),
                buffer.encoded_for_disk(),
                buffer.current_body.clone(),
                buffer.edit_revision,
                buffer.generation,
                base,
            ));
            buffer.saving = true;
            buffer.save_error = None;
            buffer.close_after_save = close_after;
        });
        if clean_close {
            self.detach_pane_now(key);
            return true;
        }
        if already_saving {
            return close_after;
        }
        let Some((project, path, payload, submitted_body, revision, generation, base)) = submission
        else {
            return false;
        };
        let connection_generation = self.connection_generation.get();
        let mapping_generation = self.project_operations.mapping_generation(&project);

        if self.offline_file_operations() {
            let mut should_close = false;
            self.open_files.update(|files| {
                if let Some(buffer) = files.get_mut(key) {
                    should_close = buffer.save_succeeded(
                        generation,
                        submitted_body,
                        revision,
                        "offline-demo".into(),
                    );
                }
            });
            if should_close {
                self.detach_pane_now(key);
            }
            self.finish_file_batch_key(key, generation, true);
            return true;
        }

        #[cfg(not(target_arch = "wasm32"))]
        let Some(client) = self.client() else {
            self.fail_file_save(key, generation, crate::i18n::t!("files.notConnected"));
            self.finish_file_batch_key(key, generation, false);
            return false;
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            let save_request_key = (
                crate::project_operations::canonical_server_url(&self.server_url.get()),
                project.clone(),
                self.next_file_save_request_id(),
            );
            self.file_save_in_flight
                .lock()
                .insert(save_request_key.clone());
            let store = self.clone();
            let file_key = key.to_string();
            self.spawn_root(async move {
                let result = client
                    .save_file(&project, &path, payload.into_bytes(), base.as_deref())
                    .await;
                if store.connection_generation.get() != connection_generation {
                    store.file_save_in_flight.lock().remove(&save_request_key);
                    store.finish_file_batch_key(&file_key, generation, false);
                    return;
                }
                if !store
                    .project_operations
                    .mapping_is_current(&project, &mapping_generation)
                {
                    store.file_save_in_flight.lock().remove(&save_request_key);
                    store.finish_file_batch_key(&file_key, generation, false);
                    return;
                }
                match result {
                    Ok(saved) => {
                        let mut should_close = false;
                        store.open_files.update(|files| {
                            if let Some(buffer) = files.get_mut(&file_key) {
                                should_close = buffer.save_succeeded(
                                    generation,
                                    submitted_body,
                                    revision,
                                    saved.version,
                                );
                            }
                        });
                        if should_close {
                            store.detach_pane_now(&file_key);
                        }
                        store.finish_file_batch_key(&file_key, generation, true);
                    }
                    Err(err) => {
                        let conflict = err.is_conflict();
                        store.open_files.update(|files| {
                            if let Some(buffer) = files.get_mut(&file_key) {
                                buffer.save_failed(generation, err.to_string(), conflict);
                            }
                        });
                        store.finish_file_batch_key(&file_key, generation, false);
                    }
                }
                store.file_save_in_flight.lock().remove(&save_request_key);
            });
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.fail_file_save(key, generation, crate::i18n::t!("files.notConnected"));
            self.finish_file_batch_key(key, generation, false);
            return false;
        }
        true
    }

    fn fail_file_save(&self, key: &str, generation: u64, error: String) {
        self.open_files.update(|files| {
            if let Some(buffer) = files.get_mut(key) {
                buffer.save_failed(generation, error, false);
            }
        });
    }

    /// 触发插件渲染或按钮动作（在线走 `FalconClient`，离线走本地参数写回）。
    pub fn dispatch_plugin_action(
        &self,
        plugin_id: &str,
        action_id: Option<&str>,
        value: Option<String>,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (plugin_id, action_id, value);
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let request_project_id = self.active_project_id.get();
            let project_id = Some(request_project_id.clone());
            let connection_generation = self.connection_generation.get();
            let connection_request_generation = self.connection_request_generation.get();
            let params = self
                .plugin_states
                .get()
                .get(plugin_id)
                .map(|s| s.params.clone())
                .unwrap_or_default();

            if let Some(client) = self.terminals.client.lock().clone() {
                if action_id.is_none()
                    && self
                        .plugin_states
                        .get()
                        .get(plugin_id)
                        .is_some_and(|s| s.loading)
                {
                    return;
                }
                self.plugin_states.update(|states| {
                    let state =
                        states
                            .entry(plugin_id.to_string())
                            .or_insert_with(|| PluginPanelState {
                                view: None,
                                params: BTreeMap::new(),
                                package_status: None,
                                loading: false,
                                error: None,
                            });
                    state.loading = true;
                });
                let pid = plugin_id.to_string();
                let act = action_id.map(str::to_owned);
                let store = self.clone();
                let request_url = self.server_url.get();
                self.spawn_root(async move {
                    let res = if let Some(action) = act {
                        client
                            .plugin_action(
                                &pid,
                                &PluginActionInput {
                                    project_id,
                                    params,
                                    action,
                                    value,
                                },
                            )
                            .await
                    } else {
                        client
                            .plugin_render(&pid, &PluginRenderInput { project_id, params })
                            .await
                    };
                    if store.connection_generation.get() != connection_generation
                        || store.connection_request_generation.get() != connection_request_generation
                        || store.active_project_id.get() != request_project_id
                        || store.server_url.get() != request_url
                    {
                        return;
                    }
                    let result_error = res.as_ref().err().map(ToString::to_string);
                    store.plugin_states.update(|states| {
                        if let Some(state) = states.get_mut(&pid) {
                            state.loading = false;
                            state.error = result_error;
                        }
                    });
                    if let Ok(view) = res {
                        store.plugin_states.update(|map| {
                            let entry =
                                map.entry(pid.clone()).or_insert_with(|| PluginPanelState {
                                    view: None,
                                    params: BTreeMap::new(),
                                    package_status: None,
                                    loading: false,
                                    error: None,
                                });
                            for (k, v) in &view.params {
                                entry.params.insert(k.clone(), v.clone());
                            }
                            if let Some(toast) = &view.toast {
                                store.toast_banner.set(Some(toast.title.clone()));
                            }
                            entry.view = Some(view);
                            entry.error = None;
                        });
                    }
                });
            }
        }
    }

    pub fn set_tui_passthrough(&self, enabled: bool) {
        self.tui_passthrough.set(enabled);
    }

    /// 合并移动端软键盘上方的 `Ctrl` / `Alt` 锁定修饰键并自动复位。
    pub fn apply_extra_modifiers(&self, raw: &str) -> String {
        let ctrl = self.extra_ctrl_latched.get();
        let alt = self.extra_alt_latched.get();
        if !ctrl && !alt {
            return raw.to_string();
        }
        self.extra_ctrl_latched.set(false);
        self.extra_alt_latched.set(false);

        let mut out = String::new();
        if alt {
            out.push('\x1b');
        }
        if ctrl {
            if let Some(ch) = raw.chars().next() {
                let lower = ch.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    let ctrl_byte = (lower as u8) - b'a' + 1;
                    out.push(ctrl_byte as char);
                    out.push_str(&raw[ch.len_utf8()..]);
                    return out;
                }
            }
        }
        out.push_str(raw);
        out
    }

    pub fn show_toast(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.toast_banner.set(Some(msg));
        let banner = self.toast_banner;
        nsh_core::prelude::set_timeout(Duration::from_secs(3), move || {
            banner.set(None);
        });
    }
}

/// 在组件树任意深度读取 `WorkspaceStore`。
pub fn use_workspace() -> WorkspaceStore {
    use_context::<WorkspaceStore>().expect("WorkspaceStore must be installed by FalconApp root")
}

#[cfg(test)]
mod file_buffer_tests {
    use super::*;

    fn text_preview(truncated: bool, version: Option<&str>) -> FilePreview {
        FilePreview::Text {
            text: "contents\n".into(),
            size: 9,
            truncated,
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn live_file_loads_are_truthful_and_only_versioned_complete_text_is_editable() {
        let editable = OpenFileBuffer::from_preview(
            "project",
            "file.txt",
            1,
            text_preview(false, Some("sha256:disk")),
            None,
        );
        assert!(editable.editable());
        assert_eq!(editable.disk_hash, "sha256:disk");

        for preview in [
            text_preview(true, Some("sha256:truncated")),
            text_preview(false, None),
            FilePreview::Image {
                mime: "image/png".into(),
                size: 17,
            },
            FilePreview::Binary { size: 18 },
            FilePreview::TooLarge { size: 19 },
            FilePreview::Unknown,
        ] {
            let buffer = OpenFileBuffer::from_preview("project", "file", 2, preview, None);
            assert_eq!(buffer.state, FileBufferState::ReadOnly);
            assert!(!buffer.editable());
            assert!(!buffer.is_dirty());
        }

        let mut failed = OpenFileBuffer::loading("project", "file", 3);
        failed.fail_load("request failed");
        assert_eq!(failed.state, FileBufferState::Failed);
        assert!(failed.current_body.is_empty());
        assert_eq!(failed.load_error.as_deref(), Some("request failed"));
    }

    #[test]
    fn save_failure_keeps_dirty_body_and_disk_version_and_edits_during_save_stay_dirty() {
        let mut buffer = OpenFileBuffer::from_disk_text("project", "file", "old", "base", None);
        buffer.current_body = "submitted".into();
        buffer.edit_revision = 1;
        buffer.saving = true;
        assert!(buffer.save_failed(0, "version conflict".into(), true));
        assert!(buffer.is_dirty());
        assert_eq!(buffer.disk_hash, "base");
        assert!(buffer.remote_changed_conflict);

        buffer.saving = true;
        buffer.close_after_save = true;
        buffer.current_body = "newer edit".into();
        buffer.edit_revision = 2;
        let should_close = buffer.save_succeeded(0, "submitted".into(), 1, "next".into());
        assert!(!should_close);
        assert_eq!(buffer.disk_hash, "next");
        assert_eq!(buffer.clean_body, "submitted");
        assert!(buffer.is_dirty());
    }

    #[test]
    fn stale_file_response_cannot_mutate_a_reopened_buffer() {
        let mut buffer = OpenFileBuffer::loading("project", "file", 8);
        assert!(!file_response_is_current(3, 1, 8, 8));
        assert!(!file_response_is_current(3, 3, 8, 7));
        assert!(file_response_is_current(3, 3, 8, 8));
        assert!(!buffer.save_succeeded(7, "stale".into(), 0, "stale-hash".into()));
        assert_eq!(buffer.disk_hash, "");
        assert_eq!(buffer.state, FileBufferState::Loading);
    }
}
