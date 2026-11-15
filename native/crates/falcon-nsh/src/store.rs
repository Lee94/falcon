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

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::{FalconClient, raw_url};
use falcon_core::agent_mode::{AgentDisplayStyle, AgentModeTransition};
use falcon_core::file_text::{TextShape, for_disk, for_editor};
use falcon_core::layout::{
    ColumnLayout, column, find_pane, insert_column, new_canvas_id, pane_keys, remove_pane,
};
use falcon_core::pane_key::{DIFF_KEY, file_key, parse_pane_key, term_key};
use falcon_core::profiles::{ProfilesFile, ServerProfile};
#[cfg(not(target_arch = "wasm32"))]
use falcon_core::program_status::FinishChimes;
use falcon_core::program_status::detail_text;
use falcon_core::term::TermPref;
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::{
    CreateSessionRequest, FilePreview, ForwardKind, PluginActionInput, PluginRenderInput,
    WorkspaceEntryKind,
};
use falcon_proto::{
    PluginBlock, PluginInfo, PluginKind, PluginList, PluginOption, PluginPackageInfo, PluginScope,
    PluginSeries, PluginStat, PluginTone, PluginView, ProgramState, SessionState,
};
use nsh_core::prelude::{Signal, effect, provide_context, signal, spawn, use_context};
use parking_lot::Mutex;

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
    Profiles,
    NewProject,
    Settings(SettingsTab),
    Askpass {
        id: String,
        prompt: String,
    },
    ConfirmTerminate {
        session_id: String,
        foreground_cmd: String,
    },
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
    pub remote_changed_conflict: bool,
    pub shape: TextShape,
    pub clean_body: String,
    pub current_body: String,
    /// 若为 HTML 预览，仅允许填入 `/api/projects/:id/raw/<token>/<path>` 作用域路径（ADR 0007）。
    pub raw_preview_url: Option<String>,
}

impl OpenFileBuffer {
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
            remote_changed_conflict: false,
            shape,
            clean_body: body.to_string(),
            current_body: body.to_string(),
            raw_preview_url,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.current_body != self.clean_body
    }

    pub fn encoded_for_disk(&self) -> String {
        for_disk(&self.current_body, self.shape)
    }
}

/// Git 工作区变更项与提交图节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeItem {
    pub path: String,
    pub status: String,
    pub staged: bool,
    pub additions: u32,
    pub deletions: u32,
    pub old_text: String,
    pub new_text: String,
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
    pub mobile_tab: Signal<MobileTab>,
    pub tui_passthrough: Signal<bool>,
    pub extra_ctrl_latched: Signal<bool>,
    pub extra_alt_latched: Signal<bool>,

    pub selected_app_icon: Signal<String>,
    pub server_info: Signal<Option<falcon_proto::SystemInfo>>,
    pub finish_sound: Signal<bool>,
    pub file_directory: Signal<String>,
    pub files_loading: Signal<bool>,
    pub file_error: Signal<Option<String>>,
    pub term_prefs: Signal<TermPref>,
    pub agent_views: Signal<HashMap<String, AgentSessionView>>,
    pub file_entries: Signal<Vec<FileEntryItem>>,
    pub open_files: Signal<HashMap<String, OpenFileBuffer>>,
    pub git_loading: Signal<bool>,
    pub git_error: Signal<Option<String>>,
    pub git_diff: Signal<Option<String>>,
    pub git_changes: Signal<Vec<GitChangeItem>>,
    pub git_commits: Signal<Vec<GitCommitItem>>,
    pub selected_git_path: Signal<Option<String>>,
    pub commit_draft: Signal<String>,

    pub plugin_list: Signal<PluginList>,
    pub plugin_states: Signal<HashMap<String, PluginPanelState>>,
    pub toast_banner: Signal<Option<String>>,

    terminals: Rc<SharedTerminals>,
}

impl WorkspaceStore {
    /// 初始化工作区（填充一套结构完备的本地种子状态，并在非离线模式下异步连向 `falcon-server`）。
    pub fn install(opts: &FalconLaunchOptions) -> Self {
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
                status: "M".into(),
                staged: true,
                additions: 148,
                deletions: 0,
                old_text: "// legacy placeholder\n".into(),
                new_text: "pub fn falcon_root() -> nsh_core::element::Root {\n    falcon_root_with_options(FalconLaunchOptions::default())\n}\n".into(),
            },
            GitChangeItem {
                path: "native/crates/falcon-server/src/plugins/nshpkg.rs".into(),
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

        let store = Self {
            profiles: signal(profiles),
            server_url: signal(url.clone()),
            server_connected: signal(opts.offline_demo),
            plaintext_warning: signal(plaintext),
            projects: signal(projects),
            active_project_id: signal("proj-falcon".into()),
            sessions: signal(sessions),
            hosts: signal(hosts),
            relays: signal(relays),
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
            mobile_tab: signal(MobileTab::Workspace),
            tui_passthrough: signal(false),
            extra_ctrl_latched: signal(false),
            extra_alt_latched: signal(false),
            selected_app_icon: signal(falcon_core::app_icon::DEFAULT_APP_ICON.to_string()),
            server_info: signal(None),
            finish_sound: signal(
                crate::theme::load_pref_item("falcon.finishSound").as_deref() != Some("off"),
            ),
            file_directory: signal(String::new()),
            files_loading: signal(false),
            file_error: signal(None),
            term_prefs: signal(falcon_core::term::load_term_pref(
                crate::theme::load_pref_item("falcon.term").as_deref(),
            )),
            agent_views: signal(agent_map),
            file_entries: signal(file_entries),
            open_files: signal(open_files),
            git_loading: signal(false),
            git_error: signal(None),
            git_diff: signal(None),
            git_changes: signal(git_changes),
            git_commits: signal(git_commits),
            selected_git_path: signal(Some("native/crates/falcon-nsh/src/lib.rs".into())),
            commit_draft: signal(String::new()),
            plugin_list: signal(plugin_list),
            plugin_states: signal(plugin_states),
            toast_banner: signal(None),
            terminals,
        };

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
        let Ok(parsed_url) = url.parse() else {
            return;
        };
        self.switch_workspace(url);
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
        spawn(async move {
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

            if let Ok(info) = client.system().await {
                store.server_info.set(Some(info));
            }
            if let Ok(icon) = client.app_icon().await {
                store.selected_app_icon.set(icon.selected);
            }
            let mut proj_paths: HashMap<String, String> = HashMap::new();

            if let Ok(projects) = client.list_projects().await {
                if store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
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
                        store.active_project_id.set(first.id.clone());
                    }
                }
                store.projects.set(mapped);
                store.refresh_project_data();
            }

            if let Ok(sessions) = client.list_sessions().await {
                if store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
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
                let keys: Vec<_> = pane_keys(&columns).into_iter().filter(|key| {
                    matches!(parse_pane_key(key), Some(falcon_core::pane_key::PaneItem::Terminal { id, .. })
                        if mapped_sessions.iter().any(|s| s.id == id))
                }).collect();
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

            if let Ok(hosts) = client.list_hosts().await {
                if store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
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

            if let Ok(relays) = client.list_relays().await {
                if store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                let mut list = Vec::new();
                for f in relays.forwards {
                    let kind_label = match f.kind {
                        ForwardKind::Local => "L → R",
                        ForwardKind::Remote => "R → L",
                    };
                    list.push(RelayRecord {
                        id: f.id,
                        machine_label: f.host_id,
                        kind_label: kind_label.into(),
                        local_port: f.bind_port,
                        target: format!("{}:{}", f.dest_host, f.dest_port),
                        public_url: None,
                        enabled: f.enabled,
                        same_port_conflict: false,
                    });
                }
                for sh in relays.shares {
                    list.push(RelayRecord {
                        id: sh.id,
                        machine_label: sh.host_id.unwrap_or_else(|| "local".into()),
                        kind_label: "Quick Tunnel".into(),
                        local_port: sh.dest_port,
                        target: format!("http://{}:{}", sh.dest_host, sh.dest_port),
                        public_url: sh.public_url,
                        enabled: sh.enabled,
                        same_port_conflict: false,
                    });
                }
                store.relays.set(list);
            }

            if let Ok(agents) = client.local_agents().await {
                store
                    .detected_agents
                    .set(agents.into_iter().map(|a| a.as_str().to_string()).collect());
            }

            if let Ok(plugins) = client.list_plugins().await {
                if store.server_url.get().trim_end_matches('/') != target_url.trim_end_matches('/')
                {
                    return;
                }
                let trust_root = if let Some(root) = plugins.trust_root.clone() {
                    Some(root)
                } else {
                    client.plugin_trust().await.ok().map(|t| t.root_public_key)
                };

                for p in &plugins.plugins {
                    if !p.enabled {
                        continue;
                    }
                    if p.kind == PluginKind::Package {
                        if let (Some(root_b64), Ok(pkg_bytes)) =
                            (trust_root.as_deref(), client.plugin_package(&p.id).await)
                        {
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
                store.plugin_list.set(plugins);
            }
        });
    }

    /// Web (`wasm32`) 端使用浏览器原生 `fetch` 同步 `falcon-server` 状态。
    #[cfg(target_arch = "wasm32")]
    pub fn connect_live_server(&self, url: &str) {
        self.switch_workspace(url);
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
        spawn(async move {
            use nsh_core::prelude::{HttpRequest, fetch};

            if let Ok(resp) = fetch(HttpRequest::get(format!("{base}/api/projects"))).await
                && resp.ok()
                && let Ok(projects) =
                    serde_json::from_slice::<Vec<falcon_proto::Project>>(&resp.body)
            {
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
                        store.active_project_id.set(first.id.clone());
                    }
                }
                store.projects.set(mapped);
            }

            if let Ok(resp) = fetch(HttpRequest::get(format!("{base}/api/sessions"))).await
                && resp.ok()
                && let Ok(sessions) =
                    serde_json::from_slice::<Vec<falcon_proto::SessionWithProject>>(&resp.body)
            {
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
                store.sessions.set(mapped);
            }

            if let Ok(resp) = fetch(HttpRequest::get(format!("{base}/api/plugins"))).await
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

    pub fn select_app_icon(&self, id: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let id = id.to_string();
            spawn(async move {
                match client.set_app_icon(&id).await {
                    Ok(icon) => store.selected_app_icon.set(icon.selected),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
            return;
        }
        self.selected_app_icon.set(id.to_string());
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
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let id = id.to_string();
            store.show_toast(crate::i18n::t!("host.testing"));
            spawn(async move {
                match client.test_host(&id).await {
                    Ok(falcon_proto::SshProbeResult::Reachable { .. }) => {
                        store.show_toast(crate::i18n::t!("host.testOk"))
                    }
                    Ok(falcon_proto::SshProbeResult::Failed { error }) => store.show_toast(error),
                    Err(error) => store.show_toast(error.to_string()),
                }
            });
        }
    }

    pub fn check_app_update(&self) {
        let Some(updater) = falcon_platform::shared().app_updater() else {
            return;
        };
        let store = self.clone();
        self.show_toast(crate::i18n::t!("native.update.checking"));
        spawn(async move {
            let result =
                nsh_core::prelude::blocking(move || updater.check(env!("CARGO_PKG_VERSION"))).await;
            let message = match result {
                Ok(falcon_platform::UpdateCheck::UpToDate { .. }) => {
                    crate::i18n::t!("native.update.upToDate")
                }
                Ok(falcon_platform::UpdateCheck::Available(update)) => {
                    crate::i18n::t!("native.update.available", version = update.version.as_str())
                }
                _ => crate::i18n::t!("native.update.failed"),
            };
            store.show_toast(message);
        });
    }

    pub fn create_project_request(&self, name: &str, path: &str, host: Option<&str>) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let input = falcon_proto::ProjectInput {
                name: name.to_string(),
                project_type: if host.is_some() {
                    falcon_proto::ProjectType::Ssh
                } else {
                    falcon_proto::ProjectType::Local
                },
                working_dir: (!path.is_empty()).then(|| path.to_string()),
                shell: None,
                host_id: host.map(str::to_string),
                ssh: None,
                repos: None,
                default_worktree_branch: None,
            };
            spawn(async move {
                match client.create_project(&input).await {
                    Ok(project) => {
                        let id = project.id.clone();
                        store.projects.update(|list| {
                            list.push(ProjectRecord {
                                source: Some(project.clone()),
                                id: project.id,
                                name: project.name,
                                path: project.working_dir.unwrap_or_default(),
                                host_id: project.host_id,
                                host_name: project.ssh.map(|s| s.host),
                                derived_from: None,
                                branch: None,
                            })
                        });
                        store.active_dialog.set(ActiveDialog::None);
                        store.select_project(&id);
                    }
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
            return;
        }
        let id = format!("proj-{}", self.projects.get().len() + 1);
        self.projects.update(|list| {
            list.push(ProjectRecord {
                id: id.clone(),
                name: name.to_string(),
                path: path.to_string(),
                host_id: host.map(str::to_string),
                host_name: None,
                derived_from: None,
                branch: None,
                source: None,
            })
        });
        self.active_project_id.set(id);
        self.active_dialog.set(ActiveDialog::None);
    }

    pub fn create_ssh_host(&self, input: falcon_proto::SshHostInput) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            spawn(async move {
                match client.create_host(&input).await {
                    Ok(h) => store.hosts.update(|list| {
                        list.push(HostRecord {
                            source: Some(h.clone()),
                            id: h.id,
                            name: h.name,
                            destination: format!("{}@{}:{}", h.username, h.host, h.port),
                            os_label: "SSH".into(),
                            connected: false,
                        })
                    }),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
            return;
        }
        let _ = input;
    }

    pub fn refresh_project_data(&self) {
        self.browse_directory("");
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let project = self.active_project_id.get();
            if project.is_empty() {
                return;
            }
            self.git_loading.set(true);
            self.git_error.set(None);
            spawn(async move {
                let (head, working) = futures::join!(
                    client.repo_info(&project),
                    client.git_working(&project, None)
                );
                if store.active_project_id.get() != project {
                    return;
                }
                if let Ok(head) = head {
                    store.projects.update(|projects| {
                        if let Some(p) = projects
                            .iter_mut()
                            .find(|p| p.id == project && p.derived_from.is_none())
                        {
                            p.branch = head.head_branch.or(head.head_sha);
                        }
                    });
                }
                match working {
                    Ok(working) if working.available => {
                        let changes: Vec<_> = working
                            .files
                            .into_iter()
                            .map(|f| GitChangeItem {
                                path: f.path.clone(),
                                status: if !f.work.trim().is_empty() {
                                    f.work.clone()
                                } else {
                                    f.index.clone()
                                },
                                staged: true,
                                additions: f.added.unwrap_or(0) as u32,
                                deletions: f.deleted.unwrap_or(0) as u32,
                                old_text: String::new(),
                                new_text: String::new(),
                            })
                            .collect();
                        let selected = store
                            .selected_git_path
                            .get()
                            .filter(|path| changes.iter().any(|c| &c.path == path));
                        store.selected_git_path.set(selected);
                        store.git_changes.set(changes);
                    }
                    Ok(working) => {
                        store.git_changes.set(Vec::new());
                        store.git_error.set(Some(
                            working
                                .detail
                                .unwrap_or_else(|| crate::i18n::t!("changes.loadFailed")),
                        ));
                    }
                    Err(e) => store.git_error.set(Some(e.to_string())),
                }
                store.git_loading.set(false);
            });
        }
    }

    pub fn commit_git_changes(&self, amend: bool, push: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let project = self.active_project_id.get();
            let input = falcon_proto::GitCommitInput {
                message: self.commit_draft.get(),
                all: false,
                paths: Some(
                    self.git_changes
                        .get()
                        .into_iter()
                        .filter(|c| c.staged)
                        .map(|c| c.path)
                        .collect(),
                ),
                amend: Some(amend),
                push: Some(push),
            };
            spawn(async move {
                match client.git_commit_changes(&project, &input, None).await {
                    Ok(result) if result.ok => {
                        store.commit_draft.set(String::new());
                        store.refresh_project_data();
                    }
                    Ok(result) => store.show_toast(result.detail),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
        let _ = (amend, push);
    }

    pub fn refresh_relays(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            spawn(async move {
                match client.list_relays().await {
                    Ok(list) => {
                        let mut rules: Vec<_> = list
                            .forwards
                            .into_iter()
                            .map(|f| RelayRecord {
                                id: f.id,
                                machine_label: f.host_id,
                                kind_label: if f.kind == falcon_proto::ForwardKind::Local {
                                    "L → R".into()
                                } else {
                                    "R → L".into()
                                },
                                local_port: f.bind_port,
                                target: format!("{}:{}", f.dest_host, f.dest_port),
                                public_url: None,
                                enabled: f.enabled,
                                same_port_conflict: false,
                            })
                            .collect();
                        rules.extend(list.shares.into_iter().map(|s| RelayRecord {
                            id: s.id,
                            machine_label: s.host_id.unwrap_or_else(|| "local".into()),
                            kind_label: "Quick Tunnel".into(),
                            local_port: s.dest_port,
                            target: format!("http://{}:{}", s.dest_host, s.dest_port),
                            public_url: s.public_url,
                            enabled: s.enabled,
                            same_port_conflict: false,
                        }));
                        store.relays.set(rules);
                    }
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
    }

    pub fn set_relay_enabled(&self, id: &str, share: bool, enabled: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let id = id.to_string();
            spawn(async move {
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
                        .map(|_| ())
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
                        .map(|_| ())
                };
                match result {
                    Ok(()) => store.refresh_relays(),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
            return;
        }
        let _ = share;
        self.relays.update(|rules| {
            if let Some(r) = rules.iter_mut().find(|r| r.id == id) {
                r.enabled = enabled;
            }
        });
    }

    pub fn create_public_share(&self, host: &str, port: u16, name: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let input = falcon_proto::PublicShareInput {
                host_id: (!host.is_empty()).then(|| host.to_string()),
                name: (!name.is_empty()).then(|| name.to_string()),
                dest_host: None,
                dest_port: port,
                enabled: Some(true),
            };
            spawn(async move {
                match client.create_share(&input).await {
                    Ok(_) => store.refresh_relays(),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
        let _ = (host, port, name);
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
        if let Some(client) = self.client() {
            let store = self.clone();
            let input = falcon_proto::PortForwardInput {
                host_id: host.to_string(),
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
            spawn(async move {
                match client.create_forward(&input).await {
                    Ok(_) => store.refresh_relays(),
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
        let _ = (host, remote, bind, destination, port, name);
    }

    /// 切换连接前先保存旧工作区，再读取目标服务端的布局，避免把旧布局写进新配置。
    fn switch_workspace(&self, url: &str) {
        if self.server_url.get().trim_end_matches('/') == url.trim_end_matches('/') {
            return;
        }
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
        self.server_connected.set(false);
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
        self.file_directory.set(path.to_string());
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.terminals.client.lock().clone() {
            let store = self.clone();
            let project = self.active_project_id.get();
            let directory = path.to_string();
            self.files_loading.set(true);
            self.file_error.set(None);
            spawn(async move {
                let result = client.list_files(&project, Some(&directory)).await;
                // 快速切目录或项目时，迟到的旧响应不得覆盖当前面板。
                if store.active_project_id.get() != project
                    || store.file_directory.get() != directory
                {
                    return;
                }
                match result {
                    Ok(listing) => store.file_entries.set(
                        listing
                            .entries
                            .into_iter()
                            .map(|e| FileEntryItem {
                                is_html: e.path.ends_with(".html") || e.path.ends_with(".htm"),
                                name: e.name,
                                path: e.path,
                                is_dir: e.kind == WorkspaceEntryKind::Dir,
                                size_bytes: e.size.unwrap_or(0),
                            })
                            .collect(),
                    ),
                    Err(e) => store.file_error.set(Some(e.to_string())),
                }
                store.files_loading.set(false);
            });
        }
    }

    pub fn select_project(&self, project_id: &str) {
        self.selected_git_path.set(None);
        self.git_diff.set(None);
        self.overview_open.set(false);
        let changed = self.active_project_id.get() != project_id;
        self.active_project_id.set(project_id.to_string());
        if changed {
            self.refresh_project_data();
        }
        // 选项目只切换该项目的画布，不自动接回收起的会话。
        if let Some(s) = self
            .sessions
            .get()
            .iter()
            .find(|s| s.project_id == project_id && self.is_session_attached(&s.id))
        {
            self.focused_pane_key.set(term_key(&s.id));
        } else {
            self.focused_pane_key.set(String::new());
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
        if let Some(s) = self.sessions.get().iter().find(|s| s.id == session_id) {
            self.active_project_id.set(s.project_id.clone());
            self.sessions_open.update(|open| {
                open.insert(s.project_id.clone(), true);
            });
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

    /// 新建一扇当前焦点项目的持久终端窗口并排入当前画布。
    pub fn create_terminal_in_active_project(&self) -> String {
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

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.terminals.client.lock().clone() {
            let store = self.clone();
            let pid = proj_id.clone();
            let ppath = proj_path.clone();
            spawn(async move {
                if let Ok(created) = client
                    .create_session(&pid, &CreateSessionRequest::default())
                    .await
                {
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
                    store.focus_session(&sid);
                }
            });
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
            agent: None,
            program_status: None,
            foreground_cmd: None,
        };
        let sess = TerminalSession::new_offline(&id, 80, 24, true);
        sess.feed_output(format!("\x1b[90m{proj_path}\x1b[0m $ ").as_bytes());
        self.terminals.map.lock().insert(id.clone(), sess);
        self.sessions.update(|v| v.push(rec));
        self.focus_session(&id);
        id
    }

    /// **Detach（收起窗口）**：仅从列式工作区移除该窗口并断开本地 Viewer socket，**绝不结束后台会话**。
    pub fn detach_pane(&self, pane_key: &str) {
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
        let key = term_key(session_id);
        self.detach_pane(&key);
        self.sessions
            .update(|list| list.retain(|s| s.id != session_id));
        self.terminals.map.lock().remove(session_id);
        self.active_dialog.set(ActiveDialog::None);

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.terminals.client.lock().clone() {
            let sid = session_id.to_string();
            spawn(async move {
                let _ = client.terminate_session(&sid).await;
            });
        }
    }

    /// 在工作区打开一扇文件编辑/预览窗口。
    pub fn open_file_pane(&self, project_id: &str, path: &str) {
        let key = file_key(project_id, path);
        let mut map = self.open_files.get();
        if !map.contains_key(&key) {
            let is_html = path.ends_with(".html") || path.ends_with(".htm");
            let fallback_raw = is_html.then(|| {
                format!(
                    "{}/api/projects/{project_id}/raw/scoped_readonly/{path}",
                    self.server_url.get().trim_end_matches('/')
                )
            });
            map.insert(
                key.clone(),
                OpenFileBuffer::from_disk_text(
                    project_id,
                    path,
                    &format!("// {path}\n"),
                    "sha256:initial",
                    fallback_raw,
                ),
            );
            self.open_files.set(map);

            #[cfg(not(target_arch = "wasm32"))]
            if let Some(client) = self.terminals.client.lock().clone() {
                let store = self.clone();
                let pid = project_id.to_string();
                let fpath = path.to_string();
                let fkey = key.clone();
                let base_url = self.server_url.get();
                spawn(async move {
                    if let Ok(wf) = client.read_file(&pid, &fpath).await
                        && let FilePreview::Text { text, version, .. } = wf.preview
                    {
                        let rel = wf.raw_path.as_deref().unwrap_or(&fpath);
                        let scoped_path = raw_url(&wf.raw_base, rel);
                        let full_raw = if is_html {
                            if scoped_path.starts_with("http://")
                                || scoped_path.starts_with("https://")
                            {
                                Some(scoped_path)
                            } else {
                                Some(format!(
                                    "{}/{}",
                                    base_url.trim_end_matches('/'),
                                    scoped_path.trim_start_matches('/')
                                ))
                            }
                        } else {
                            None
                        };
                        let buf = OpenFileBuffer::from_disk_text(
                            &pid,
                            &fpath,
                            &text,
                            version.unwrap_or_default(),
                            full_raw,
                        );
                        store.open_files.update(|m| {
                            m.insert(fkey, buf);
                        });
                    }
                });
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

    /// 在修改面板内预览差异；仅显式展开时才添加工作区窗口。
    pub fn preview_git_path(&self, path: &str) {
        self.selected_git_path.set(Some(path.to_string()));
        self.load_git_diff();
    }

    fn load_git_diff(&self) {
        self.git_diff.set(None);
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(client), Some(path)) = (self.client(), self.selected_git_path.get()) {
            let store = self.clone();
            let project = self.active_project_id.get();
            let base_url = self.server_url.get();
            spawn(async move {
                match client
                    .git_file_diff(&project, &falcon_client::GitFileRef::new(&path), None)
                    .await
                {
                    Ok(diff)
                        if diff.available
                            && store.server_url.get() == base_url
                            && store.active_project_id.get() == project
                            && store.selected_git_path.get().as_deref() == Some(path.as_str()) =>
                    {
                        store.git_diff.set(Some(diff.diff))
                    }
                    Ok(_) => {}
                    Err(e) => store.show_toast(e.to_string()),
                }
            });
        }
    }

    /// 打开工作区级统一 Diff 窗口（`DIFF_KEY = "d"`）。
    pub fn open_diff_pane(&self) {
        self.load_git_diff();
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
        let mut ok = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut pending_save: Option<(String, String, String, Option<String>)> = None;

        self.open_files.update(|map| {
            if let Some(buf) = map.get_mut(file_pane_key) {
                if buf.remote_changed_conflict {
                    return;
                }
                let disk_payload = buf.encoded_for_disk();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let base = (!buf.disk_hash.is_empty() && buf.disk_hash != "sha256:initial")
                        .then(|| buf.disk_hash.clone());
                    pending_save =
                        Some((buf.project_id.clone(), buf.path.clone(), disk_payload, base));
                }
                #[cfg(target_arch = "wasm32")]
                let _ = disk_payload;
                buf.clean_body = buf.current_body.clone();
                ok = true;
            }
        });

        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(client), Some((pid, path, payload, base))) =
            (self.terminals.client.lock().clone(), pending_save)
        {
            let store = self.clone();
            let key = file_pane_key.to_string();
            spawn(async move {
                match client
                    .save_file(&pid, &path, payload.into_bytes(), base.as_deref())
                    .await
                {
                    Ok(saved) => {
                        store.open_files.update(|m| {
                            if let Some(buf) = m.get_mut(&key) {
                                buf.disk_hash = saved.version;
                                buf.remote_changed_conflict = false;
                            }
                        });
                    }
                    Err(err) if err.is_conflict() => {
                        store.open_files.update(|m| {
                            if let Some(buf) = m.get_mut(&key) {
                                buf.remote_changed_conflict = true;
                            }
                        });
                    }
                    Err(_) => {}
                }
            });
        }
        ok
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
            let project_id = Some(self.active_project_id.get());
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
                spawn(async move {
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
                    if store.server_url.get() != request_url {
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
