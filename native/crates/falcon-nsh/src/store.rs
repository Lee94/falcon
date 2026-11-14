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

use falcon_client::FalconClient;
use falcon_core::agent_mode::{AgentDisplayStyle, AgentModeTransition};
use falcon_core::file_text::{TextShape, for_disk, for_editor};
use falcon_core::layout::{
    ColumnLayout, column, find_pane, insert_column, new_canvas_id, pane_keys, remove_pane,
};
use falcon_core::pane_key::{DIFF_KEY, file_key, parse_pane_key, term_key};
use falcon_core::profiles::{ProfilesFile, ServerProfile};
use falcon_core::term::TermPref;
use falcon_proto::{
    PluginActionInput, PluginBlock, PluginInfo, PluginKind, PluginList, PluginOption,
    PluginPackageInfo, PluginRenderInput, PluginScope, PluginSeries, PluginStat, PluginTone,
    PluginView, ProgramState, SessionState,
};
use nsh_core::prelude::{Signal, provide_context, signal, spawn, use_context};
use parking_lot::Mutex;

use crate::terminal::TerminalSession;
use crate::theme::use_theme;

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
    Askpass { id: String, prompt: String },
    ConfirmTerminate { session_id: String, foreground_cmd: String },
}

/// 设置模态框页签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Appearance,
    Terminal,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
        "zsh".to_string()
    }
}

/// 远端主机记录（SSH 连接配置）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRecord {
    pub id: String,
    pub name: String,
    pub destination: String,
    pub os_label: String,
    pub connected: bool,
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
    client: Mutex<Option<FalconClient>>,
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

    pub term_prefs: Signal<TermPref>,
    pub agent_views: Signal<HashMap<String, AgentSessionView>>,
    pub file_entries: Signal<Vec<FileEntryItem>>,
    pub open_files: Signal<HashMap<String, OpenFileBuffer>>,
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
        let url = opts
            .server_url
            .clone()
            .unwrap_or_else(|| falcon_platform::shared().local_server_url());
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
            },
            ProjectRecord {
                id: "proj-falcon-nsh".into(),
                name: "falcon · nsh-client".into(),
                path: "/workspace/falcon.worktrees/nsh-client".into(),
                host_id: None,
                host_name: None,
                derived_from: Some("falcon".into()),
                branch: Some("feat/nsh-unified-client".into()),
            },
            ProjectRecord {
                id: "proj-edge-node".into(),
                name: "edge-gateway".into(),
                path: "/srv/edge-gateway".into(),
                host_id: Some("host-prod-01".into()),
                host_name: Some("build-box-01".into()),
                derived_from: None,
                branch: Some("release".into()),
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
                        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
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
            client: Mutex::new(None),
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

        let mut profiles = ProfilesFile::load_with_local(None, &url);
        if let Some(custom) = &opts.server_url {
            profiles.upsert(ServerProfile {
                id: "active".into(),
                name: crate::i18n::t!("demo.currentServer"),
                url: custom.clone(),
                plaintext_ok: true,
            });
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
            sidebar_open: signal(true),
            sidebar_width: signal(252.0),
            right_panel_open: signal(true),
            right_panel_width: signal(320.0),
            right_panel_kind: signal(RightPanelKind::Plugin("sysmon".into())),
            active_dialog: signal(ActiveDialog::None),
            mobile_tab: signal(MobileTab::Workspace),
            tui_passthrough: signal(false),
            extra_ctrl_latched: signal(false),
            extra_alt_latched: signal(false),
            term_prefs: signal(TermPref::default()),
            agent_views: signal(agent_map),
            file_entries: signal(file_entries),
            open_files: signal(open_files),
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
            store.connect_live_server(&url);
        }

        provide_context(store.clone());
        store
    }

    /// 异步尝试连接真实 `falcon-server`，拉取项目、会话、远端主机与插件列表；连不上时保持可交互不崩。
    pub fn connect_live_server(&self, url: &str) {
        let Ok(parsed_url) = url.parse() else {
            return;
        };
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

        let store = self.clone();
        spawn(async move {
            if let Ok(projects) = client.list_projects().await {
                store.server_connected.set(true);
                if !projects.is_empty() {
                    let mapped: Vec<ProjectRecord> = projects
                        .into_iter()
                        .map(|p| {
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
                            }
                        })
                        .collect();
                    if let Some(first) = mapped.first() {
                        store.active_project_id.set(first.id.clone());
                    }
                    store.projects.set(mapped);
                }
            }
            if let Ok(plugins) = client.list_plugins().await {
                store.plugin_list.set(plugins);
            }
        });
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
        let key = term_key(session_id);
        let cols = self.columns.get();
        if find_pane(&cols, &key).is_none() {
            let cid = self.active_canvas_id.get();
            let next = insert_column(&cols, &key, cols.len(), Some(&cid));
            self.columns.set(next);
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
        let proj_path = self
            .projects
            .get()
            .into_iter()
            .find(|p| p.id == proj_id)
            .map_or_else(|| "/workspace".to_string(), |p| p.path);
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
        if let Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) =
            parse_pane_key(pane_key)
        {
            if let Some(sess) = self.terminals.map.lock().get(&id) {
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
        self.sessions.update(|list| list.retain(|s| s.id != session_id));
        self.terminals.map.lock().remove(session_id);
        self.active_dialog.set(ActiveDialog::None);

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
            let raw_url = is_html.then(|| {
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
                    raw_url,
                ),
            );
            self.open_files.set(map);
        }
        let cols = self.columns.get();
        if find_pane(&cols, &key).is_none() {
            let cid = self.active_canvas_id.get();
            let next = insert_column(&cols, &key, cols.len(), Some(&cid));
            self.columns.set(next);
        }
        self.focused_pane_key.set(key);
    }

    /// 打开工作区级统一 Diff 窗口（`DIFF_KEY = "d"`）。
    pub fn open_diff_pane(&self) {
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
        self.open_files.update(|map| {
            if let Some(buf) = map.get_mut(file_pane_key) {
                if buf.remote_changed_conflict {
                    return;
                }
                let _disk_payload = buf.encoded_for_disk();
                buf.clean_body = buf.current_body.clone();
                ok = true;
            }
        });
        ok
    }

    /// 触发插件渲染或按钮动作（在线走 `FalconClient`，离线走本地参数写回）。
    pub fn dispatch_plugin_action(
        &self,
        plugin_id: &str,
        action_id: Option<&str>,
        value: Option<String>,
    ) {
        let project_id = Some(self.active_project_id.get());
        let params = self
            .plugin_states
            .get()
            .get(plugin_id)
            .map(|s| s.params.clone())
            .unwrap_or_default();

        if let Some(client) = self.terminals.client.lock().clone() {
            let pid = plugin_id.to_string();
            let act = action_id.map(str::to_owned);
            let store = self.clone();
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
                if let Ok(view) = res {
                    store.plugin_states.update(|map| {
                        let entry = map.entry(pid.clone()).or_insert_with(|| PluginPanelState {
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
