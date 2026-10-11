//! 工作区状态：持久化的 `falcon.workspace` 形状与读入清洗，以及旧 React 版 `store.ts`
//! 里与工作区相关的纯函数 selector 和 reducer 式的状态迁移。
//!
//! React 版的 store 是一个 zustand store，工作区的状态迁移写在各个 action 的 `set(...)`
//! 里，与 API 调用、toast、i18n 混在一起。这里只取纯的那一半：[`WorkspaceState`] 上的
//! 方法照 React 版对应 action 里的 `set` 逐字段移植；发请求、弹提示、问"要不要关"这些
//! 仍归 app 层（对应的 React 版 action 名写在每个方法的注释里）。会话 / 项目列表是服务端
//! 数据，由调用方按参数传进来。
//!
//! 落盘：React 版在部分 action 之后调 `persist()`，这里的方法注释里标了「此后落盘」，
//! app 照做即可（只在松手 / 关窗时写、内容相同跳过，设计文档 §4.9）。

use std::collections::{BTreeMap, HashSet};

use falcon_proto::{GitFileChange, Project, SessionAgent, SessionState, SessionWithProject, DeadReason};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::{js_num, js_truthy};
use crate::layout::{
    CANVAS_GAP_PX, CanvasGroup, ColumnLayout, DropSpot, PaneAt, PaneLayout, apply_drop, assign_canvases, canvas_end,
    canvas_groups, canvas_index, column, find_pane, insert_column, insert_pane, is_pinned, move_to_canvas,
    order_by_canvas, pane_keys, pin_pane, remove_pane, replace_pane, resolve_spot, set_column_basis, set_pane_basis,
    sync_columns, unpin_all, visible_columns,
};
use crate::pane_key::{DIFF_KEY, PaneItem, file_key, parse_pane_key, term_key};
use crate::panel_width::{PANEL_WIDTH_DEFAULT, clamp_panel_width_default, parse_panel_width};

pub const PENDING_PREFIX: &str = "pending:";

/// 还没拿到后端 id 的会话 id（`pending:<序号>`）
pub fn is_pending_id(id: &str) -> bool {
    id.starts_with(PENDING_PREFIX)
}

/// 主区此刻的焦点
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum ActiveView {
    Overview,
    Terminal { session_id: String },
    /// 选中了项目但还没有可显示的终端
    Project,
    /// Git 面板点开的文件差异（见 [`WorkspaceState::diff_tab`]）
    Diff,
    /// 文件查看窗口；path 一并带着，好让排布 key 认得出是哪个文件
    File { project_id: String, path: String },
}

/// 右侧栏打开的是哪一格。加面板时在这里加一个值，持久化形状不用改。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum RightPanelId {
    #[default]
    Git,
    Changes,
    Files,
    Meegle,
}

impl RightPanelId {
    /// 持久化的布局可能来自旧版本，认不出的面板名一律退回默认（由调用方处理 `None`）
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "git" => RightPanelId::Git,
            "changes" => RightPanelId::Changes,
            // 旧版本的 "forward"（转发面板）已挪进设置的「中转」页（ADR 0016），落到默认
            "files" => RightPanelId::Files,
            "meegle" => RightPanelId::Meegle,
            _ => return None,
        })
    }
}

/// 差异窗口看的是哪次提交（从 History 的提交详情点进来时才有）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
}

/// 差异查看窗口的目标。单例：再点别的文件就地替换内容，像编辑器的预览标签——
/// 每个文件各开一扇只会让人在一排窗口里找不到终端。不持久化：刷新后工作区的 diff
/// 早就变了，恢复一个过期视图没有意义。
#[derive(Debug, Clone, PartialEq)]
pub struct DiffTabTarget {
    pub project_id: String,
    pub file: GitFileChange,
    /// 带上就取那一次提交的改动；不带就是「修改」面板点进来的，看工作区现状
    pub commit: Option<DiffCommit>,
    /// 多仓库项目：diff 属于哪个成员仓库（成员 dir 原文）
    pub repo: Option<String>,
}

/// 文件查看窗口的目标。与差异一样是**单例预览**：再点一个文件，是同一扇窗口换了内容
/// （[`replace_pane`]），不是新开一扇。不持久化：刷新后工作区的文件可能已经变了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTabTarget {
    pub project_id: String,
    /// 工作目录相对路径，一律 `/` 分隔
    pub path: String,
}

impl FileTabTarget {
    pub fn key(&self) -> String {
        file_key(&self.project_id, &self.path)
    }
}

pub fn same_file(a: &FileTabTarget, b: &FileTabTarget) -> bool {
    a.project_id == b.project_id && a.path == b.path
}

/// 还没拿到后端 id 的会话：窗口立刻出现并显示"正在建立会话…"，而不是等 REST 返回
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSession {
    pub id: String,
    pub project_id: String,
    /// 这扇窗口开出来会跑什么 CLI；`None` = 普通 shell
    pub agent: Option<SessionAgent>,
    pub error: Option<String>,
}

// ---------------- 持久化形状 ----------------

/// 持久化的一列。列 id 不存——重建时现生成即可，它只在一次运行内当视图的 key 用；
/// 文件 / 差异窗口也不存（重开后它们本来就消失），落盘的只有终端。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PersistedColumn {
    #[serde(with = "js_num::opt")]
    pub basis: Option<f64>,
    pub panes: Vec<PaneLayout>,
    /// 固定在最右（见 [`ColumnLayout::pinned`]）。只可能是最后一列。落盘时恒写这个字段（照 React 版）
    #[serde(default)]
    pub pinned: bool,
    /// 所在画布（见 [`ColumnLayout::canvas`]）。旧版本没有，读回来按还没分处理；固定列不记
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<String>,
}

/// `falcon.workspace` 的形状（字段名、嵌套、顺序照 React 版 `JSON.stringify` 的输出：浏览器版沿用
/// 同一个 localStorage 键，老用户的排布原样读回）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedWorkspace {
    pub tabs: Vec<String>,
    /// 主区排布：列 → 窗口。与 tabs 对账后才使用（见 [`sync_columns`]）
    pub columns: Vec<PersistedColumn>,
    pub active: ActiveView,
    pub sidebar_open: bool,
    pub right_open: bool,
    pub right_panel: RightPanelId,
    pub collapsed: BTreeMap<String, bool>,
    /// 哪些检出把会话行摊开了（key 是 projectId）。与 collapsed 反着来：**默认收起**，
    /// 只记"展开过的"。会话行是树里最长的一段，默认摊开会把侧栏挤满，而"开着哪些会话"
    /// 平时看检出行上的计数徽标就够了。
    pub sessions_open: BTreeMap<String, bool>,
    pub selected_project_id: Option<String>,
    /// 侧栏是否显示已存档的附属项目。默认藏起来，存档就是为了少占地方
    pub show_archived: bool,
    #[serde(with = "js_num")]
    pub sidebar_width: f64,
    #[serde(with = "js_num")]
    pub right_width: f64,
    /// 终端画布只显示当前活动的那一列（标题栏的最大化）
    pub term_zoomed: bool,
}

impl Default for PersistedWorkspace {
    fn default() -> Self {
        PersistedWorkspace {
            tabs: Vec::new(),
            columns: Vec::new(),
            active: ActiveView::Overview,
            sidebar_open: true,
            right_open: false,
            right_panel: RightPanelId::Git,
            collapsed: BTreeMap::new(),
            sessions_open: BTreeMap::new(),
            selected_project_id: None,
            show_archived: false,
            sidebar_width: PANEL_WIDTH_DEFAULT,
            right_width: PANEL_WIDTH_DEFAULT,
            term_zoomed: false,
        }
    }
}

/// 落盘的排布可能来自旧版本或被手改过：认不出的形状一律丢掉，宁可退回"每个会话一列"
pub fn parse_persisted_columns(raw: Option<&Value>) -> Vec<PersistedColumn> {
    let Some(Value::Array(cols)) = raw else { return Vec::new() };
    let mut out: Vec<PersistedColumn> = Vec::new();
    for col in cols {
        let Some(col) = col.as_object() else { continue };
        let Some(Value::Array(panes)) = col.get("panes") else { continue };
        let mut kept: Vec<PaneLayout> = Vec::new();
        for pane in panes {
            let Some(pane) = pane.as_object() else { continue };
            // pending id 活不过重启；文件 / 差异也不落盘，这里一并挡住
            let Some(key) = pane.get("key").and_then(Value::as_str) else { continue };
            if !key.starts_with("t:") || is_pending_id(&key[2..]) {
                continue;
            }
            kept.push(PaneLayout { key: key.to_string(), basis: pane.get("basis").and_then(Value::as_f64) });
        }
        if !kept.is_empty() {
            out.push(PersistedColumn {
                basis: col.get("basis").and_then(Value::as_f64),
                panes: kept,
                pinned: col.get("pinned") == Some(&Value::Bool(true)),
                canvas: col.get("canvas").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string),
            });
        }
    }
    // 固定列必须是最后一列：中间那些（上一次落盘时后面还有别的列，重建时被丢掉了些）
    // 一律降级成普通列，免得插新列的夹取（pin_edge）把整片右边都封死
    let last = out.len().saturating_sub(1);
    for (i, c) in out.iter_mut().enumerate() {
        if i < last {
            c.pinned = false;
        }
    }
    out
}

/// `Record<string, boolean>`：React 版不校验值的类型、用时按真假读，这里读入时就按 JS 真值转
fn bool_record(v: Option<&Value>) -> BTreeMap<String, bool> {
    match v {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), js_truthy(v))).collect(),
        _ => BTreeMap::new(),
    }
}

/// 读工作区：`raw` 是存储里的原文（键 `falcon.workspace`，由平台取：浏览器版在 falcon-web 的
/// localStorage，原生在 falcon-desktop 的数据目录；改名前的 `mojito.workspace` 不再认）。
/// 没有、坏 JSON、形状离谱到 React 版会抛异常（`tabs` 不是数组、整个是 `null`）都退回默认。
pub fn load_workspace(raw: Option<&str>) -> PersistedWorkspace {
    let fallback = PersistedWorkspace::default();
    let Some(raw) = raw.filter(|s| !s.is_empty()) else { return fallback };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else { return fallback };
    if parsed.is_null() {
        return fallback;
    }
    let get = |k: &str| parsed.as_object().and_then(|o| o.get(k));
    // 持久化的 tab 里绝不该混进上一次的 pending id。`(parsed.tabs ?? []).filter(...)`：
    // tabs 是别的类型时 React 版的 `.filter` 直接抛，整份退回默认
    let tabs = match get("tabs") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            items.iter().filter_map(Value::as_str).filter(|t| !is_pending_id(t)).map(str::to_string).collect()
        }
        Some(_) => return fallback,
    };
    let active = {
        let a = get("active").and_then(Value::as_object);
        let kind = a.and_then(|a| a.get("kind")).and_then(Value::as_str);
        match (kind, a.and_then(|a| a.get("sessionId")).and_then(Value::as_str)) {
            (Some("terminal"), Some(id)) => ActiveView::Terminal { session_id: id.to_string() },
            (Some("project"), _) => ActiveView::Project,
            _ => ActiveView::Overview,
        }
    };
    PersistedWorkspace {
        tabs,
        columns: parse_persisted_columns(get("columns")),
        active,
        sidebar_open: get("sidebarOpen") != Some(&Value::Bool(false)),
        right_open: get("rightOpen") == Some(&Value::Bool(true)),
        right_panel: get("rightPanel").and_then(Value::as_str).and_then(RightPanelId::from_wire).unwrap_or_default(),
        collapsed: bool_record(get("collapsed")),
        sessions_open: bool_record(get("sessionsOpen")),
        selected_project_id: get("selectedProjectId").and_then(Value::as_str).map(str::to_string),
        show_archived: get("showArchived") == Some(&Value::Bool(true)),
        sidebar_width: parse_panel_width(get("sidebarWidth")),
        right_width: parse_panel_width(get("rightWidth")),
        term_zoomed: get("termZoomed") == Some(&Value::Bool(true)),
    }
}

/// 写回存储的 JSON
pub fn serialize_workspace(ws: &PersistedWorkspace) -> String {
    serde_json::to_string(ws).unwrap_or_default()
}

/// 读回来的排布 → 内存里的列：列 id 现生成，再与 tabs 对账一遍（没排布的会话各自接一列）
pub fn restore_columns(ws: &PersistedWorkspace) -> Vec<ColumnLayout> {
    let columns: Vec<ColumnLayout> = ws
        .columns
        .iter()
        .map(|c| ColumnLayout {
            basis: c.basis,
            panes: c.panes.clone(),
            pinned: c.pinned,
            canvas: c.canvas.clone(),
            ..column(Vec::<String>::new(), None)
        })
        .collect();
    let live: Vec<String> = ws.tabs.iter().map(|t| term_key(t)).collect();
    sync_columns(&columns, &live)
}

// ---------------- selector ----------------

/// 窗口（会话 / pending）属于哪个项目
pub fn tab_project_id<'a>(tab_id: &str, sessions: &'a [SessionWithProject], pending: &'a [PendingSession]) -> Option<&'a str> {
    if is_pending_id(tab_id) {
        return pending.iter().find(|p| p.id == tab_id).map(|p| p.project_id.as_str());
    }
    sessions.iter().find(|s| s.id == tab_id).map(|s| s.project_id.as_str())
}

/// 当前活动视图对应的 key；总览 / 项目空页没有窗口
pub fn active_key(active: &ActiveView) -> Option<String> {
    match active {
        ActiveView::Terminal { session_id } => Some(term_key(session_id)),
        ActiveView::File { project_id, path } => Some(file_key(project_id, path)),
        ActiveView::Diff => Some(DIFF_KEY.to_string()),
        ActiveView::Overview | ActiveView::Project => None,
    }
}

/// key → 视图。切窗口的快捷键按排布顺序（从左到右、列内从上到下）走
pub fn pane_view(key: &str) -> Option<ActiveView> {
    Some(match parse_pane_key(key)? {
        PaneItem::Terminal { id, .. } => ActiveView::Terminal { session_id: id },
        PaneItem::File { project_id, path, .. } => ActiveView::File { project_id, path },
        PaneItem::Diff { .. } => ActiveView::Diff,
    })
}

/// 多仓库项目在 Git / 修改面板里当前看的成员仓库；没选过（或选的成员已被编辑掉）就
/// 退回第一个成员。非多仓库项目给 `None`——调用方原样把它传给 api 的 repo 参数即可。
pub fn select_multi_repo_dir(multi_repo: &BTreeMap<String, String>, project: Option<&Project>) -> Option<String> {
    let project = project?;
    let multi = project.multi.as_ref()?;
    if let Some(picked) = multi_repo.get(&project.id).filter(|p| !p.is_empty())
        && multi.repos.iter().any(|m| &m.dir == picked) {
            return Some(picked.clone());
        }
    multi.repos.first().map(|m| m.dir.clone())
}

/// WS 推过来的状态立刻写进列表（React 版 `applySessionState`），不等 5s 轮询——否则接回后
/// 仍显示「待接回」。不是 dead 就清掉 deadReason。
pub fn apply_session_state(sessions: &mut [SessionWithProject], id: &str, state: SessionState, dead_reason: Option<DeadReason>) {
    for s in sessions.iter_mut().filter(|s| s.id == id) {
        s.session.state = state;
        s.session.dead_reason = if state == SessionState::Dead { dead_reason.or(s.session.dead_reason) } else { None };
    }
}

/// 自动标题（前台命令）变了就地更新（React 版 `applySessionTitle`）
pub fn apply_session_title(sessions: &mut [SessionWithProject], id: &str, title: Option<&str>) {
    for s in sessions.iter_mut().filter(|s| s.id == id) {
        s.session.title = title.map(str::to_string);
    }
}

// ---------------- 状态 ----------------

/// React 版 store 里工作区那一部分的状态（不含服务端数据与弹层开关）
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceState {
    /// 打开的终端窗口（手动关掉 = 结束会话），可能含 pending id
    pub tabs: Vec<String>,
    /// 主区排布：列 → 窗口（终端 / 文件 / 差异混排）。含**所有**项目的窗口，画之前按
    /// 当前项目过滤（[`WorkspaceState::layout_columns`]）；终端那部分持久化。
    pub columns: Vec<ColumnLayout>,
    pub active: ActiveView,
    pub pending: Vec<PendingSession>,
    pub diff_tab: Option<DiffTabTarget>,
    pub file_tab: Option<FileTabTarget>,
    /// 用户的侧栏偏好（持久化）
    pub sidebar_open: bool,
    /// 窄窗口临时隐藏，不写回偏好——不然开一次窄窗口就把用户的设置改了
    pub sidebar_auto_hidden: bool,
    /// 左右栏宽度（持久化）。拖的时候只改内存，松手才写回
    pub sidebar_width: f64,
    pub right_width: f64,
    /// 用户的右侧栏偏好（持久化）。默认关：第一次打开不该把终端挤窄
    pub right_open: bool,
    /// 终端画布的最大化（持久化）：只显示当前活动的那一列。是画布级开关而不是某一列的
    /// 属性：跟着 active 走，切窗口仍只看一列
    pub term_zoomed: bool,
    pub right_panel: RightPanelId,
    pub collapsed: BTreeMap<String, bool>,
    pub sessions_open: BTreeMap<String, bool>,
    pub show_archived: bool,
    /// 多仓库项目在 Git / 修改面板里当前看的成员（projectId → 成员 dir）。不持久化；
    /// 两个面板 + 差异窗口必须看同一个成员，所以放这里不放面板自己
    pub multi_repo: BTreeMap<String, String>,
    /// 侧栏当前选中的项目；主区只显示它下面的窗口。`None` = 在总览
    pub selected_project_id: Option<String>,
    /// 画布宽（画布量出来报上来的，不持久化）。新列排不排得下要按它算；
    /// 0 = 还没量出来，这时不分画布（见 [`WorkspaceState::settle_canvases`]）。
    pub canvas_width: f64,
    /// 上一次显示的画布（不持久化）。平时就是活动窗口所在的那块，用不着它；活动窗口在
    /// 固定列里时（每块画布都有它）靠它知道该显示哪块。由 settle_canvases 跟着 active 更新。
    pub canvas_id: Option<String>,
    /// 每块画布上一次停在哪扇窗口（canvas id → pane key，不持久化）：切回来时把焦点还给它
    pub canvas_focus: BTreeMap<String, String>,
    pending_seq: u64,
}

/// [`WorkspaceState::settle_canvases`] 改了什么
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    /// 分了画布：排布变了，要落盘
    pub columns: bool,
    /// 有任何改动（含当前画布 / 每块的焦点记忆）：要重画
    pub changed: bool,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::from_persisted(&PersistedWorkspace::default())
    }
}

impl WorkspaceState {
    /// 从读回来的工作区起一份状态（React 版 store 的初值）
    pub fn from_persisted(ws: &PersistedWorkspace) -> Self {
        WorkspaceState {
            tabs: ws.tabs.clone(),
            columns: restore_columns(ws),
            active: ws.active.clone(),
            pending: Vec::new(),
            diff_tab: None,
            file_tab: None,
            sidebar_open: ws.sidebar_open,
            sidebar_auto_hidden: false,
            sidebar_width: ws.sidebar_width,
            right_width: ws.right_width,
            right_open: ws.right_open,
            term_zoomed: ws.term_zoomed,
            right_panel: ws.right_panel,
            collapsed: ws.collapsed.clone(),
            sessions_open: ws.sessions_open.clone(),
            show_archived: ws.show_archived,
            multi_repo: BTreeMap::new(),
            selected_project_id: ws.selected_project_id.clone(),
            canvas_width: 0.0,
            canvas_id: None,
            canvas_focus: BTreeMap::new(),
            pending_seq: 0,
        }
    }

    /// 要落盘的那份（React 版的 `persist()`）：只落终端，pending 与两个查看窗口都活不过重启
    pub fn persisted(&self) -> PersistedWorkspace {
        let columns = self
            .columns
            .iter()
            .filter_map(|c| {
                let panes: Vec<PaneLayout> =
                    c.panes.iter().filter(|p| p.key.starts_with("t:") && !is_pending_id(&p.key[2..])).cloned().collect();
                // 固定列在每块画布上都有，它的画布不记
                let canvas = if c.pinned { None } else { c.canvas.clone() };
                (!panes.is_empty()).then_some(PersistedColumn { basis: c.basis, panes, pinned: c.pinned, canvas })
            })
            .collect();
        let active = match &self.active {
            ActiveView::Diff | ActiveView::File { .. } => self.idle_view(),
            ActiveView::Terminal { session_id } if is_pending_id(session_id) => self.idle_view(),
            other => other.clone(),
        };
        PersistedWorkspace {
            tabs: self.tabs.iter().filter(|t| !is_pending_id(t)).cloned().collect(),
            columns,
            active,
            sidebar_open: self.sidebar_open,
            right_open: self.right_open,
            right_panel: self.right_panel,
            collapsed: self.collapsed.clone(),
            sessions_open: self.sessions_open.clone(),
            selected_project_id: self.selected_project_id.clone(),
            show_archived: self.show_archived,
            sidebar_width: self.sidebar_width,
            right_width: self.right_width,
            term_zoomed: self.term_zoomed,
        }
    }

    /// 没有窗口可聚焦时停在哪：选了项目是项目空页，否则总览
    fn idle_view(&self) -> ActiveView {
        if self.selected_project_id.is_some() { ActiveView::Project } else { ActiveView::Overview }
    }

    // ---- selector ----

    /// 侧栏此刻是否真的显示：用户偏好与窄窗口临时隐藏的合成结果
    pub fn sidebar_visible(&self) -> bool {
        self.sidebar_open && !self.sidebar_auto_hidden
    }

    /// 文件窗口在当前项目下是否可见（选了别的项目就藏起来，排布原样留着）
    pub fn file_visible(&self) -> bool {
        match (&self.file_tab, &self.selected_project_id) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(f), Some(sel)) => &f.project_id == sel,
        }
    }

    /// 差异窗口同理
    pub fn diff_visible(&self) -> bool {
        match (&self.diff_tab, &self.selected_project_id) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(d), Some(sel)) => &d.project_id == sel,
        }
    }

    /// 侧栏选中项目时，主区只显示这个项目下的会话
    pub fn visible_tabs(&self, sessions: &[SessionWithProject]) -> Vec<String> {
        let Some(sel) = &self.selected_project_id else { return self.tabs.clone() };
        self.tabs.iter().filter(|id| tab_project_id(id, sessions, &self.pending) == Some(sel.as_str())).cloned().collect()
    }

    /// 工作区里**所有**窗口的 key（含别的项目的）。排布与数据源的对账认它：不在这里面
    /// 的窗口一律从列里摘掉，在这里面却没排布的各自接一列。
    pub fn live_pane_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.tabs.iter().map(|t| term_key(t)).collect();
        if let Some(f) = &self.file_tab {
            keys.push(f.key());
        }
        if self.diff_tab.is_some() {
            keys.push(DIFF_KEY.to_string());
        }
        keys
    }

    /// 当前项目下该画出来的窗口 key
    pub fn visible_pane_keys(&self, sessions: &[SessionWithProject]) -> HashSet<String> {
        let mut keys: HashSet<String> = self.visible_tabs(sessions).iter().map(|t| term_key(t)).collect();
        if self.file_visible()
            && let Some(f) = &self.file_tab {
                keys.insert(f.key());
            }
        if self.diff_visible() {
            keys.insert(DIFF_KEY.to_string());
        }
        keys
    }

    /// 画布此刻要画的列（别的项目的窗口过滤掉，空列不占位），按画布一块接一块排好、
    /// 固定列在最后——切窗口的快捷键（⌘1…9、上一个 / 下一个）就按这个顺序走。
    pub fn layout_columns(&self, sessions: &[SessionWithProject]) -> Vec<ColumnLayout> {
        let visible = self.visible_pane_keys(sessions);
        order_by_canvas(&visible_columns(&self.columns, |k| visible.contains(k)))
    }

    /// 当前视图里的画布（每块的列已含接在最右的固定列）
    pub fn view_canvases(&self, sessions: &[SessionWithProject]) -> Vec<CanvasGroup> {
        canvas_groups(&self.layout_columns(sessions))
    }

    /// 正在显示的那块画布的 id：活动窗口所在的那块；活动窗口在固定列里就是上一次显示的
    /// 那块（`canvas_id`）。当前视图一扇窗口都没有时 `None`。
    pub fn current_canvas(&self, sessions: &[SessionWithProject]) -> Option<String> {
        let groups = self.view_canvases(sessions);
        if groups.is_empty() {
            return None;
        }
        let key = active_key(&self.active);
        let at = canvas_index(&groups, key.as_deref(), self.canvas_id.as_deref());
        Some(groups[at].id.clone())
    }

    /// 新开的一列插在排布的哪个下标：`after` 那扇窗口所在列的右边；没给 `after`（或它在
    /// 固定列里——固定列在排布里永远是最后一列，接在它后面就跑到最后一块画布去了）就接在
    /// 当前画布的最后一列后面。先往眼前这块里放，排不下 [`assign_canvases`] 会紧跟着它
    /// 另起一块。当前视图里一扇窗口都没有就接到最右。
    fn new_column_at(&self, after: Option<&str>, sessions: &[SessionWithProject]) -> usize {
        if let Some(at) = after.and_then(|k| find_pane(&self.columns, k))
            && !self.columns[at.col].pinned
        {
            return at.col + 1;
        }
        self.current_canvas(sessions).and_then(|c| canvas_end(&self.columns, &c)).unwrap_or(self.columns.len())
    }

    /// 按排布顺序可以切到的视图（切窗口的快捷键走这个）
    pub fn view_tabs(&self, sessions: &[SessionWithProject]) -> Vec<ActiveView> {
        pane_keys(&self.layout_columns(sessions)).iter().filter_map(|k| pane_view(k)).collect()
    }

    /// 右侧各面板跟谁走：侧栏选中的项目优先，否则当前窗口所属项目。
    /// 总览且没选项目时为 `None`——不要退回第一个项目，免得打开面板看到别人的仓库。
    ///
    /// 两个查看窗口（差异 / 文件）也要认：从文件面板点开一个文件，active 就从终端变成
    /// 文件，这时若不看文件窗口的 projectId，面板会当场塌回"选一个项目"。
    pub fn focus_project_id(&self, sessions: &[SessionWithProject]) -> Option<String> {
        if let Some(sel) = &self.selected_project_id {
            return Some(sel.clone());
        }
        match &self.active {
            ActiveView::Terminal { session_id } => self
                .pending
                .iter()
                .find(|p| &p.id == session_id)
                .map(|p| p.project_id.clone())
                .or_else(|| sessions.iter().find(|s| &s.id == session_id).map(|s| s.project_id.clone())),
            ActiveView::File { project_id, .. } => Some(project_id.clone()),
            ActiveView::Diff => self.diff_tab.as_ref().map(|d| d.project_id.clone()),
            ActiveView::Overview | ActiveView::Project => None,
        }
    }

    /// 关掉一个不代表会话的窗口（差异 / 文件）之后落到哪。
    /// 顺序：最近的可见终端 → 还开着的文件窗口 → 差异窗口 → 项目空页 → 总览。
    fn fallback_active(&self, sessions: &[SessionWithProject]) -> ActiveView {
        if let Some(last) = self.visible_tabs(sessions).last() {
            return ActiveView::Terminal { session_id: last.clone() };
        }
        if self.file_visible()
            && let Some(f) = &self.file_tab {
                return ActiveView::File { project_id: f.project_id.clone(), path: f.path.clone() };
            }
        if self.diff_visible() {
            return ActiveView::Diff;
        }
        self.idle_view()
    }

    /// 文件 / 差异窗口该落在哪一列。
    ///
    /// 两者共用同一座"查看列"：已经开着另一个查看窗口就落到它下面，否则在当前活动窗口
    /// 的右边另起一列。终端是工作区的主角，查看类窗口不该把它挤到看不见的地方去——
    /// 当前画布排不下就另起一块画布（[`assign_canvases`]），已在场的终端不会被挤窄。
    fn place_view_pane(&self, key: &str, sessions: &[SessionWithProject]) -> Vec<ColumnLayout> {
        if find_pane(&self.columns, key).is_some() {
            return self.columns.clone();
        }
        let sibling_key = if key == DIFF_KEY {
            self.file_tab.as_ref().map(FileTabTarget::key)
        } else {
            self.diff_tab.as_ref().map(|_| DIFF_KEY.to_string())
        };
        if let Some(sibling) = sibling_key.and_then(|k| find_pane(&self.columns, &k)) {
            return insert_pane(&self.columns, key, PaneAt { col: sibling.col, index: sibling.index + 1 });
        }
        let at = self.new_column_at(active_key(&self.active).as_deref(), sessions);
        insert_column(&self.columns, key, at, None)
    }

    fn resync(&mut self) {
        self.columns = sync_columns(&self.columns, &self.live_pane_keys());
    }

    // ---- reducer：会话 ----

    /// 服务端的会话列表变了之后的收敛（React 版 `refreshSessions` 里的 `set`）。
    /// dead 会话仍在列表里，因此恢复出来的窗口不会被静默丢弃，只是显示为已丢失。
    /// 此后落盘。
    pub fn apply_sessions(&mut self, sessions: &[SessionWithProject]) {
        let alive: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        let keep = |id: &str| is_pending_id(id) || alive.contains(id);
        self.tabs.retain(|t| keep(t));
        self.resync();
        if let ActiveView::Terminal { session_id } = &self.active
            && !keep(session_id) {
                self.active = self.idle_view();
            }
    }

    /// 服务端的项目列表变了之后的收敛（React 版 `refreshProjects` 里的 `set`）：被存档的项目
    /// 不能保持选中——它已从侧栏消失，主区不能停在一个看不见的项目上。
    pub fn apply_projects(&mut self, projects: &[Project]) {
        let still = self.selected_project_id.as_ref().is_some_and(|sel| {
            projects.iter().any(|p| &p.id == sel && p.worktree.as_ref().and_then(|w| w.archived_at).is_none_or(|t| t == 0))
        });
        if !still {
            self.selected_project_id = None;
            if self.active == ActiveView::Project {
                self.active = ActiveView::Overview;
            }
        }
    }

    /// 打开一个已有会话（React 版 `openSession`）：还没排布过就自己接一列在最右；刚打开的
    /// 会话要在侧栏里看得见，会话行默认收着，这里替用户摊开它那个检出。此后落盘。
    pub fn open_session(&mut self, session_id: &str, sessions: &[SessionWithProject]) {
        let project_id = tab_project_id(session_id, sessions, &self.pending)
            .map(str::to_string)
            .or_else(|| self.selected_project_id.clone());
        if !self.tabs.iter().any(|t| t == session_id) {
            self.tabs.push(session_id.to_string());
        }
        self.resync();
        self.active = ActiveView::Terminal { session_id: session_id.to_string() };
        if let Some(pid) = project_id {
            self.sessions_open.insert(pid.clone(), true);
            self.selected_project_id = Some(pid);
        }
    }

    /// 新建会话的第一步（React 版 `createSessionNow` 发请求之前的 `set`）：先摆一扇 pending
    /// 窗口。默认独占一列接在当前画布的最右（排不下另起一块，见 [`assign_canvases`]）；
    /// `after` 给了就插在那扇窗口所在列的右边。开在别的项目里就接到最右——当前画布是
    /// 这个项目的，与它无关。新开的终端不能建在一个收着的检出里，顺手摊开。返回 pending id。
    pub fn begin_pending(
        &mut self,
        project_id: &str,
        agent: Option<SessionAgent>,
        after: Option<&str>,
        sessions: &[SessionWithProject],
    ) -> String {
        // 位置按加 pending 之前的状态算（React 版在同一个 set 里用的也是旧状态）
        let at = if self.selected_project_id.as_deref() == Some(project_id) || after.is_some() {
            self.new_column_at(after, sessions)
        } else {
            self.columns.len()
        };
        self.pending_seq += 1;
        let pending_id = format!("{PENDING_PREFIX}{}", self.pending_seq);
        self.pending.push(PendingSession { id: pending_id.clone(), project_id: project_id.to_string(), agent, error: None });
        self.tabs.push(pending_id.clone());
        self.columns = insert_column(&self.columns, &term_key(&pending_id), at, None);
        self.active = ActiveView::Terminal { session_id: pending_id.clone() };
        self.selected_project_id = Some(project_id.to_string());
        self.sessions_open.insert(project_id.to_string(), true);
        pending_id
    }

    /// 会话建好了：pending 换成真 id，窗口原地不动（React 版 `createSessionNow` / `retryPending`
    /// 成功后的 `set`）。此后落盘。
    pub fn resolve_pending(&mut self, pending_id: &str, session_id: &str) {
        self.pending.retain(|p| p.id != pending_id);
        for t in &mut self.tabs {
            if t == pending_id {
                *t = session_id.to_string();
            }
        }
        self.columns = replace_pane(&self.columns, &term_key(pending_id), &term_key(session_id));
        if matches!(&self.active, ActiveView::Terminal { session_id: s } if s == pending_id) {
            self.active = ActiveView::Terminal { session_id: session_id.to_string() };
        }
    }

    /// 建会话失败：错误挂在 pending 窗口上，等用户重试或关掉
    pub fn fail_pending(&mut self, pending_id: &str, error: &str) {
        for p in self.pending.iter_mut().filter(|p| p.id == pending_id) {
            p.error = Some(error.to_string());
        }
    }

    /// 重试前清掉错误（React 版 `retryPending` 的第一个 `set`）；返回要重建的那条，没有就是 `None`
    pub fn retry_pending(&mut self, pending_id: &str) -> Option<PendingSession> {
        let entry = self.pending.iter_mut().find(|p| p.id == pending_id)?;
        entry.error = None;
        Some(entry.clone())
    }

    /// 只把窗口摘掉，不碰会话（React 版 `dropTab`）：Terminate / Detach / 清除已丢失记录之后
    /// 都走这里。此后落盘。
    pub fn drop_tab(&mut self, id: &str, sessions: &[SessionWithProject]) {
        self.tabs.retain(|t| t != id);
        self.pending.retain(|p| p.id != id);
        self.columns = remove_pane(&self.columns, &term_key(id));
        if matches!(&self.active, ActiveView::Terminal { session_id } if session_id == id) {
            self.active = self.fallback_active(sessions);
        }
    }

    // ---- reducer：查看窗口 ----

    /// 在差异窗口里打开一个文件，就地替换上一个（React 版 `openDiff`）
    pub fn open_diff(&mut self, target: DiffTabTarget, sessions: &[SessionWithProject]) {
        self.columns = self.place_view_pane(DIFF_KEY, sessions);
        self.diff_tab = Some(target);
        self.active = ActiveView::Diff;
    }

    /// React 版 `closeDiff`
    pub fn close_diff(&mut self, sessions: &[SessionWithProject]) {
        self.diff_tab = None;
        self.columns = remove_pane(&self.columns, DIFF_KEY);
        if self.active == ActiveView::Diff {
            self.active = self.fallback_active(sessions);
        }
    }

    /// 打开工作目录里的一个文件（React 版 `openFile`）：已经开着一个文件时是同一扇窗口换内容，
    /// 位置与高度都不动
    pub fn open_file(&mut self, project_id: &str, path: &str, sessions: &[SessionWithProject]) {
        let next = FileTabTarget { project_id: project_id.to_string(), path: path.to_string() };
        let key = next.key();
        self.columns = match &self.file_tab {
            Some(prev) if !same_file(prev, &next) && find_pane(&self.columns, &prev.key()).is_some() => {
                replace_pane(&self.columns, &prev.key(), &key)
            }
            _ => self.place_view_pane(&key, sessions),
        };
        self.file_tab = Some(next);
        self.active = ActiveView::File { project_id: project_id.to_string(), path: path.to_string() };
    }

    /// 关掉文件窗口（React 版 `closeFile`）；`target` 缺省是正开着的那个。指名要关的不是正开着
    /// 的那个文件（比如改名后回收旧路径）时什么也不做。
    pub fn close_file(&mut self, target: Option<&FileTabTarget>, sessions: &[SessionWithProject]) {
        let Some(current) = self.file_tab.clone() else { return };
        let closing = target.cloned().unwrap_or_else(|| current.clone());
        if !same_file(&current, &closing) {
            return;
        }
        let was_active = matches!(&self.active, ActiveView::File { project_id, path } if *project_id == closing.project_id && *path == closing.path);
        self.file_tab = None;
        self.columns = remove_pane(&self.columns, &closing.key());
        if was_active {
            self.active = self.fallback_active(sessions);
        }
    }

    // ---- reducer：焦点与导航 ----

    /// 把焦点交给某扇窗口（点标题栏 / 点进画布，React 版 `focusPane`）。返回有没有变；变了要落盘。
    pub fn focus_pane(&mut self, key: &str) -> bool {
        match pane_view(key) {
            Some(view) if view != self.active => {
                self.active = view;
                true
            }
            _ => false,
        }
    }

    /// 侧栏点一个项目（React 版 `selectProject`）：它名下还没开窗口的会话按最近活跃从旧到新
    /// 补进来；焦点留在它名下的当前终端，否则最新的 pending，否则最近活跃的会话，
    /// 都没有就是项目空页。项目不存在时什么也不做。此后落盘。
    pub fn select_project(&mut self, project_id: &str, projects: &[Project], sessions: &[SessionWithProject]) {
        if !projects.iter().any(|p| p.id == project_id) {
            return;
        }
        let mut extra: Vec<&SessionWithProject> =
            sessions.iter().filter(|s| s.project_id == project_id && !self.tabs.contains(&s.id)).collect();
        extra.sort_by_key(|s| s.last_active_at);
        self.tabs.extend(extra.into_iter().map(|s| s.id.clone()));
        let mine = |id: &str| tab_project_id(id, sessions, &self.pending) == Some(project_id);
        let keep_active = matches!(&self.active, ActiveView::Terminal { session_id } if self.tabs.contains(session_id) && mine(session_id));
        if !keep_active {
            let pending_last = self.pending.iter().rfind(|p| p.project_id == project_id);
            // 最近活跃的那个；并列时取列表里靠前的（与 JS 的稳定排序后取 [0] 一致）
            let mut newest: Option<&SessionWithProject> = None;
            for s in sessions.iter().filter(|s| s.project_id == project_id) {
                if newest.is_none_or(|n| s.last_active_at > n.last_active_at) {
                    newest = Some(s);
                }
            }
            self.active = match (pending_last, newest) {
                (Some(p), _) => ActiveView::Terminal { session_id: p.id.clone() },
                (None, Some(s)) => ActiveView::Terminal { session_id: s.id.clone() },
                (None, None) => ActiveView::Project,
            };
        }
        self.selected_project_id = Some(project_id.to_string());
        self.resync();
    }

    /// 回到总览（React 版 `showOverview`）。此后落盘。
    pub fn show_overview(&mut self) {
        self.active = ActiveView::Overview;
        self.selected_project_id = None;
    }

    /// 切到第 index 扇（从 0 起，按排布顺序；React 版 `focusTabAt`）。返回有没有切；切了要落盘。
    pub fn focus_tab_at(&mut self, index: usize, sessions: &[SessionWithProject]) -> bool {
        match self.view_tabs(sessions).into_iter().nth(index) {
            Some(view) => {
                self.active = view;
                true
            }
            None => false,
        }
    }

    /// 前后切窗口（React 版 `cycleTab`）。当前焦点不在排布里时，往后切落到第一扇、往前切落到
    /// 最后一扇。返回有没有切；切了要落盘。
    ///
    /// 已知出入：|delta| 大于窗口数时 JS 的 `%` 会得出负下标（什么都不切、active 变成
    /// undefined），这里按欧几里得取模照样落到一扇上。实际只传 ±1。
    pub fn cycle_tab(&mut self, delta: i64, sessions: &[SessionWithProject]) -> bool {
        let items = self.view_tabs(sessions);
        if items.is_empty() {
            return false;
        }
        let len = items.len() as i64;
        let from = match items.iter().position(|v| *v == self.active) {
            Some(i) => i as i64,
            None if delta > 0 => -1,
            None => 0,
        };
        let next = (from + delta + len).rem_euclid(len) as usize;
        self.active = items[next].clone();
        true
    }

    // ---- reducer：排布 ----

    /// 拖拽松手：把一扇窗口挪到落点（React 版 `movePane`）。落点是在**可见**列上量出来的，
    /// 先翻成全量坐标再落。返回有没有动——拖了等于没拖就不改状态，省掉一轮终端重新量
    /// 尺寸；动了要落盘。
    pub fn move_pane(&mut self, key: &str, spot: DropSpot, sessions: &[SessionWithProject]) -> bool {
        let visible = self.layout_columns(sessions);
        match apply_drop(&self.columns, key, resolve_spot(&self.columns, &visible, spot)) {
            Some(columns) => {
                self.columns = columns;
                true
            }
            None => false,
        }
    }

    /// 固定 / 取消固定「这扇窗口所在的那一列」在最右（React 版 `togglePinPane`）。此后落盘。
    pub fn toggle_pin_pane(&mut self, key: &str) {
        self.columns = if is_pinned(&self.columns, key) { unpin_all(&self.columns) } else { pin_pane(&self.columns, key) };
    }

    /// 把一扇窗口挪到另一块画布（React 版 `movePaneToCanvas`）：独占一列接在那块最右；`None` =
    /// 新开一块，紧跟在当前画布后面。焦点跟着它走，画布也就切过去了。返回有没有动；动了
    /// 要落盘。
    pub fn move_pane_to_canvas(&mut self, key: &str, canvas: Option<&str>, sessions: &[SessionWithProject]) -> bool {
        let current = self.current_canvas(sessions);
        let Some(columns) = move_to_canvas(&self.columns, key, canvas, current.as_deref()) else { return false };
        self.columns = columns;
        if let Some(view) = pane_view(key) {
            self.active = view;
        }
        true
    }

    /// 切到某块画布（React 版 `showCanvas`）：焦点交给它上次停的那扇窗口；活动窗口在固定列里
    /// 就只换画布、焦点不动。返回有没有变；焦点变了要落盘（focusPane 里）。
    pub fn show_canvas(&mut self, id: &str, sessions: &[SessionWithProject]) -> bool {
        let groups = self.view_canvases(sessions);
        let Some(group) = groups.iter().find(|g| g.id == id) else { return false };
        let shown = |k: &str| group.columns.iter().any(|c| !c.pinned && c.panes.iter().any(|p| p.key == k));
        let key = active_key(&self.active);
        if key.as_deref().is_some_and(shown) {
            return false;
        }
        // 输入停在固定列里：它在每块画布上都有，只换画布、焦点不动
        if let Some(key) = &key
            && is_pinned(&self.columns, key)
            && group.columns.iter().any(|c| c.pinned)
        {
            if self.canvas_id.as_deref() == Some(id) {
                return false;
            }
            self.canvas_id = Some(id.to_string());
            return true;
        }
        let remembered = self.canvas_focus.get(id).filter(|k| shown(k)).cloned();
        let target = remembered.or_else(|| {
            group.columns.iter().find(|c| !c.pinned).and_then(|c| c.panes.first()).map(|p| p.key.clone())
        });
        target.is_some_and(|k| self.focus_pane(&k))
    }

    /// 切到左 / 右一块画布，不回绕（React 版 `stepCanvas`）。画布条、横向手势、快捷键都走它
    pub fn step_canvas(&mut self, delta: i64, sessions: &[SessionWithProject]) -> bool {
        let groups = self.view_canvases(sessions);
        let key = active_key(&self.active);
        let at = canvas_index(&groups, key.as_deref(), self.canvas_id.as_deref()) as i64 + delta;
        match usize::try_from(at).ok().and_then(|i| groups.get(i)) {
            Some(next) => {
                let id = next.id.clone();
                self.show_canvas(&id, sessions)
            }
            None => false,
        }
    }

    /// 画布量到的宽。返回有没有变
    pub fn set_canvas_width(&mut self, width: f64) -> bool {
        if self.canvas_width == width {
            return false;
        }
        self.canvas_width = width;
        true
    }

    /// 画布的收尾（React 版挂在 store 订阅上的 `settleCanvases`）——每次会改 columns / active /
    /// 可见性的变更之后都要过一遍，跟 [`sync_columns`] 一样不能漏：
    ///
    /// 1. 还没分画布的可见列分好（[`assign_canvases`]）：新开的终端、对账补进来的会话、取消
    ///    固定的列、旧版本落盘的排布。"当前画布排不下就自动新开一块"就发生在这一步。
    /// 2. 记下正在显示哪块（`canvas_id`）、每块上次停在哪扇窗口（`canvas_focus`）。
    ///
    /// app 层在工作区每次通知之后统一调它（observe_self），而不是在每个动作里各调一遍。
    pub fn settle_canvases(&mut self, sessions: &[SessionWithProject]) -> Settled {
        let mut out = Settled::default();
        let visible = self.visible_pane_keys(sessions);
        if let Some(columns) = assign_canvases(&self.columns, |k| visible.contains(k), self.canvas_width, CANVAS_GAP_PX) {
            self.columns = columns;
            out.columns = true;
            out.changed = true;
        }
        let Some(key) = active_key(&self.active) else { return out };
        let group = self
            .view_canvases(sessions)
            .into_iter()
            .find(|g| g.columns.iter().any(|c| !c.pinned && c.panes.iter().any(|p| p.key == key)));
        let Some(group) = group else { return out };
        if self.canvas_id.as_deref() != Some(group.id.as_str()) {
            self.canvas_id = Some(group.id.clone());
            out.changed = true;
        }
        if self.canvas_focus.get(&group.id) != Some(&key) {
            // 顺手清掉已经没有了的画布
            let live: HashSet<&str> = self.columns.iter().filter_map(|c| c.canvas.as_deref()).collect();
            self.canvas_focus.retain(|id, _| live.contains(id.as_str()));
            self.canvas_focus.insert(group.id, key);
            out.changed = true;
        }
        out
    }

    /// 拖列间的缝：只钉左边那一列的宽度，右边跟着让。拖的途中不落盘
    pub fn set_column_width(&mut self, id: &str, width: Option<f64>) {
        self.columns = set_column_basis(&self.columns, id, width);
    }

    /// 拖列内的缝：只钉上面那扇窗口的高度
    pub fn set_pane_height(&mut self, key: &str, height: Option<f64>) {
        self.columns = set_pane_basis(&self.columns, key, height);
    }

    // ---- reducer：侧栏与面板 ----

    /// 显式开合永远以"现在看到的样子"为准，并解除窄窗口的临时隐藏。此后落盘。
    pub fn toggle_sidebar(&mut self) {
        let visible = self.sidebar_visible();
        self.sidebar_open = !visible;
        self.sidebar_auto_hidden = false;
    }

    pub fn set_sidebar_auto_hidden(&mut self, hidden: bool) {
        self.sidebar_auto_hidden = hidden;
    }

    /// 拖的途中只改内存，松手再落盘。返回有没有变
    pub fn set_sidebar_width(&mut self, width: f64) -> bool {
        let next = clamp_panel_width_default(width);
        let changed = self.sidebar_width != next;
        self.sidebar_width = next;
        changed
    }

    pub fn set_right_width(&mut self, width: f64) -> bool {
        let next = clamp_panel_width_default(width);
        let changed = self.right_width != next;
        self.right_width = next;
        changed
    }

    /// 终端画布：只看当前一列 ⇄ 多列并排。此后落盘。
    pub fn toggle_term_zoom(&mut self) {
        self.term_zoomed = !self.term_zoomed;
    }

    /// 点同一格再关；点另一格则切过去（React 版 `toggleRightPanel`，缺省面板是 git）。此后落盘。
    pub fn toggle_right_panel(&mut self, id: RightPanelId) {
        if self.right_open && self.right_panel == id {
            self.right_open = false;
        } else {
            self.right_open = true;
            self.right_panel = id;
        }
    }

    /// 摊开 / 收起某个检出下的会话行。此后落盘。
    pub fn toggle_sessions(&mut self, project_id: &str) {
        let now = self.sessions_open.get(project_id).copied().unwrap_or(false);
        self.sessions_open.insert(project_id.to_string(), !now);
    }

    /// 折叠 / 展开侧栏树的某一层（服务器 / 文件夹 / worktree）。此后落盘。
    pub fn toggle_collapsed(&mut self, key: &str) {
        let now = self.collapsed.get(key).copied().unwrap_or(false);
        self.collapsed.insert(key.to_string(), !now);
    }

    /// 此后落盘。
    pub fn toggle_show_archived(&mut self) {
        self.show_archived = !self.show_archived;
    }

    /// 多仓库项目：切换 Git / 修改面板正在看的成员仓库
    pub fn set_multi_repo(&mut self, project_id: &str, dir: &str) {
        self.multi_repo.insert(project_id.to_string(), dir.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::pane_keys;
    use serde_json::json;

    fn session(id: &str, project: &str, last_active: i64) -> SessionWithProject {
        serde_json::from_value(json!({
            "id": id, "projectId": project, "name": "", "state": "active", "durable": true,
            "createdAt": 0, "lastActiveAt": last_active, "projectName": project, "projectType": "local"
        }))
        .unwrap()
    }

    fn project(id: &str) -> Project {
        serde_json::from_value(json!({ "id": id, "name": id, "type": "local", "createdAt": 0 })).unwrap()
    }

    fn shape(cols: &[ColumnLayout]) -> Vec<Vec<String>> {
        cols.iter().map(|c| c.panes.iter().map(|p| p.key.clone()).collect()).collect()
    }

    #[test]
    fn load_falls_back_on_missing_or_broken_input() {
        let d = PersistedWorkspace::default();
        assert_eq!(load_workspace(None), d);
        assert_eq!(load_workspace(Some("")), d);
        assert_eq!(load_workspace(Some("{oops")), d);
        assert_eq!(load_workspace(Some("null")), d);
        // tabs 不是数组：React 版的 `.filter` 抛异常，整份退回默认（连 sidebarOpen 也不认）
        assert_eq!(load_workspace(Some(r#"{"tabs":"s1","sidebarOpen":false}"#)), d);
        assert_eq!(load_workspace(Some("5")), d);
    }

    #[test]
    fn load_sanitizes_every_field() {
        let raw = json!({
            "tabs": ["s1", "pending:3", 7, "s2"],
            "columns": [
                { "basis": 320, "pinned": true, "panes": [{ "key": "t:s1", "basis": 200 }, { "key": "t:pending:3" }] },
                { "basis": "wide", "panes": [{ "key": "f:p:a.ts" }, { "key": "d" }] },
                { "panes": [{ "key": "t:s2", "basis": "x" }], "pinned": true },
                "junk",
            ],
            "active": { "kind": "file", "projectId": "p", "path": "a" },
            "sidebarOpen": 0,
            "rightOpen": "yes",
            "rightPanel": "meegle",
            "collapsed": { "s:local": true, "p:x": 0 },
            "selectedProjectId": 5,
            "sidebarWidth": 1000,
            "rightWidth": "300",
            "termZoomed": true,
            "themeId": "legacy"
        })
        .to_string();
        let ws = load_workspace(Some(&raw));
        assert_eq!(ws.tabs, ["s1", "s2"]);
        assert_eq!(ws.columns.len(), 2);
        // 第一列后面还有列：固定降级
        assert!(!ws.columns[0].pinned && ws.columns[1].pinned);
        assert_eq!(ws.columns[0].basis, Some(320.0));
        assert_eq!(ws.columns[0].panes, [PaneLayout { key: "t:s1".into(), basis: Some(200.0) }]);
        assert_eq!(ws.columns[1].basis, None);
        assert_eq!(ws.columns[1].panes[0].basis, None);
        assert_eq!(ws.active, ActiveView::Overview);
        assert!(ws.sidebar_open, "只有 false 才算关");
        assert!(!ws.right_open, "只有 true 才算开");
        assert_eq!(ws.right_panel, RightPanelId::Meegle);
        assert_eq!(ws.collapsed.get("s:local"), Some(&true));
        assert_eq!(ws.collapsed.get("p:x"), Some(&false));
        assert_eq!(ws.selected_project_id, None);
        assert_eq!(ws.sidebar_width, 420.0);
        assert_eq!(ws.right_width, PANEL_WIDTH_DEFAULT);
        assert!(ws.term_zoomed);
        assert_eq!(load_workspace(Some(r#"{"active":{"kind":"terminal","sessionId":"s9"}}"#)).active, ActiveView::Terminal {
            session_id: "s9".into()
        });
        assert_eq!(load_workspace(Some(r#"{"active":{"kind":"project"}}"#)).active, ActiveView::Project);
        assert_eq!(load_workspace(Some(r#"{"rightPanel":"nope"}"#)).right_panel, RightPanelId::Git);
        // 转发面板挪进了设置（ADR 0016）：旧布局里存的 "forward" 退回默认
        assert_eq!(load_workspace(Some(r#"{"rightPanel":"forward"}"#)).right_panel, RightPanelId::Git);
    }

    #[test]
    fn persisted_json_matches_the_web_shape() {
        let mut ws = WorkspaceState::from_persisted(&load_workspace(Some(r#"{"tabs":["s1"]}"#)));
        ws.selected_project_id = Some("p".into());
        ws.open_file("p", "a.ts", &[]);
        ws.collapsed.insert("s:local".into(), true);
        let out = serialize_workspace(&ws.persisted());
        assert_eq!(
            out,
            r#"{"tabs":["s1"],"columns":[{"basis":null,"panes":[{"key":"t:s1","basis":null}],"pinned":false}],"active":{"kind":"project"},"sidebarOpen":true,"rightOpen":false,"rightPanel":"git","collapsed":{"s:local":true},"sessionsOpen":{},"selectedProjectId":"p","showArchived":false,"sidebarWidth":260,"rightWidth":260,"termZoomed":false}"#
        );
        // 读回来还是同一份
        assert_eq!(load_workspace(Some(&out)), ws.persisted());
        assert_eq!(
            serde_json::to_string(&ActiveView::Terminal { session_id: "s1".into() }).unwrap(),
            r#"{"kind":"terminal","sessionId":"s1"}"#
        );
    }

    #[test]
    fn restore_reconciles_columns_with_tabs() {
        let ws = load_workspace(Some(
            &json!({ "tabs": ["a", "b", "c"], "columns": [{ "basis": 300, "panes": [{ "key": "t:b" }, { "key": "t:gone" }] }] })
                .to_string(),
        ));
        let cols = restore_columns(&ws);
        assert_eq!(shape(&cols), [vec!["t:b"], vec!["t:a"], vec!["t:c"]]);
        assert_eq!(cols[0].basis, Some(300.0));
    }

    #[test]
    fn pending_sessions_open_resolve_and_persist_without_pending_ids() {
        let sessions = vec![session("s1", "p", 1)];
        let mut ws = WorkspaceState::default();
        ws.open_session("s1", &sessions);
        assert_eq!(ws.selected_project_id.as_deref(), Some("p"));
        assert_eq!(ws.sessions_open.get("p"), Some(&true));
        let pid = ws.begin_pending("p", Some(SessionAgent::Claude), Some("t:s1"), &sessions);
        assert_eq!(pid, "pending:1");
        assert_eq!(shape(&ws.columns), [vec!["t:s1"], vec!["t:pending:1"]]);
        // pending 不落盘，焦点落成项目空页
        let saved = ws.persisted();
        assert_eq!(saved.tabs, ["s1"]);
        assert_eq!(saved.active, ActiveView::Project);
        assert_eq!(saved.columns.len(), 1);
        ws.fail_pending(&pid, "boom");
        assert_eq!(ws.retry_pending(&pid).map(|p| p.error), Some(None));
        ws.resolve_pending(&pid, "s2");
        assert_eq!(ws.tabs, ["s1", "s2"]);
        assert_eq!(shape(&ws.columns), [vec!["t:s1"], vec!["t:s2"]]);
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "s2".into() });
        assert!(ws.pending.is_empty());
    }

    #[test]
    fn view_panes_share_one_column_and_fall_back_when_closed() {
        let sessions = vec![session("s1", "p", 1), session("s2", "p", 2)];
        let mut ws = WorkspaceState::default();
        ws.open_session("s1", &sessions);
        ws.open_session("s2", &sessions);
        ws.focus_pane("t:s1");
        // 文件在活动窗口右边另起一列；差异落到文件下面
        ws.open_file("p", "a.ts", &sessions);
        assert_eq!(shape(&ws.columns), [vec!["t:s1"], vec!["f:p:a.ts"], vec!["t:s2"]]);
        let diff = DiffTabTarget {
            project_id: "p".into(),
            file: GitFileChange { path: "a.ts".into(), orig_path: None, index: "M".into(), work: " ".into() },
            commit: None,
            repo: None,
        };
        ws.open_diff(diff, &sessions);
        assert_eq!(shape(&ws.columns), [vec!["t:s1"], vec!["f:p:a.ts", "d"], vec!["t:s2"]]);
        // 换一个文件是同一扇窗口换内容
        ws.open_file("p", "b.ts", &sessions);
        assert_eq!(shape(&ws.columns)[1], ["f:p:b.ts", "d"]);
        // 关掉正看着的差异：先落到最近的可见终端，不是旁边的文件
        ws.focus_pane("d");
        assert_eq!(ws.active, ActiveView::Diff);
        ws.close_diff(&sessions);
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "s2".into() });
        // 关的不是正开着的那个：什么也不做
        ws.close_file(Some(&FileTabTarget { project_id: "p".into(), path: "a.ts".into() }), &sessions);
        assert!(ws.file_tab.is_some());
        // 没有终端时文件窗口排在差异前面接焦点
        ws.drop_tab("s1", &sessions);
        ws.drop_tab("s2", &sessions);
        assert_eq!(ws.active, ActiveView::File { project_id: "p".into(), path: "b.ts".into() });
        ws.close_file(None, &sessions);
        assert_eq!(ws.active, ActiveView::Project);
        assert!(ws.columns.is_empty());
    }

    #[test]
    fn select_project_adopts_its_sessions_and_picks_a_focus() {
        let projects = vec![project("p"), project("q")];
        let sessions = vec![session("a", "p", 5), session("b", "p", 9), session("c", "q", 1)];
        let mut ws = WorkspaceState::default();
        ws.select_project("p", &projects, &sessions);
        assert_eq!(ws.tabs, ["a", "b"]);
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "b".into() });
        assert_eq!(ws.visible_tabs(&sessions), ["a", "b"]);
        ws.select_project("q", &projects, &sessions);
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "c".into() });
        assert_eq!(pane_keys(&ws.layout_columns(&sessions)), ["t:c"]);
        ws.select_project("missing", &projects, &sessions);
        assert_eq!(ws.selected_project_id.as_deref(), Some("q"));
        let before = ws.clone();
        ws.select_project("r", &[project("r")], &sessions);
        assert_eq!(ws.active, ActiveView::Project);
        assert_ne!(ws, before);
    }

    #[test]
    fn cycle_and_focus_follow_the_visible_layout_order() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("x", "q", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["a", "b", "x"] {
            ws.open_session(id, &sessions);
        }
        ws.select_project("p", &[project("p"), project("q")], &sessions);
        ws.active = ActiveView::Overview;
        assert!(ws.cycle_tab(1, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "a".into() });
        assert!(ws.cycle_tab(-1, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "b".into() });
        assert!(ws.focus_tab_at(0, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "a".into() });
        assert!(!ws.focus_tab_at(5, &sessions));
    }

    #[test]
    fn move_pane_resolves_visible_spots_and_reports_no_ops() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("x", "q", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["x", "a", "b"] {
            ws.open_session(id, &sessions);
        }
        ws.selected_project_id = Some("p".into());
        // 可见的是 [a] [b]；把 b 拖进 a 下面
        assert!(ws.move_pane("t:b", DropSpot::Into { col: 0, index: 1 }, &sessions));
        assert_eq!(shape(&ws.columns), [vec!["t:x"], vec!["t:a", "t:b"]]);
        assert!(!ws.move_pane("t:b", DropSpot::Into { col: 0, index: 1 }, &sessions));
        ws.toggle_pin_pane("t:a");
        assert!(crate::layout::is_pinned(&ws.columns, "t:a"));
        ws.toggle_pin_pane("t:a");
        assert!(!crate::layout::is_pinned(&ws.columns, "t:a"));
    }

    #[test]
    fn session_list_changes_prune_dead_tabs_and_projects_drop_archived_selection() {
        let mut ws = WorkspaceState::default();
        let sessions = vec![session("a", "p", 1), session("b", "p", 2)];
        ws.open_session("a", &sessions);
        ws.open_session("b", &sessions);
        let pid = ws.begin_pending("p", None, None, &sessions);
        ws.apply_sessions(&sessions[..1]);
        assert_eq!(ws.tabs, ["a", pid.as_str()]);
        assert_eq!(pane_keys(&ws.columns), ["t:a", "t:pending:1"]);
        ws.active = ActiveView::Terminal { session_id: "b".into() };
        ws.apply_sessions(&sessions[..1]);
        assert_eq!(ws.active, ActiveView::Project);
        let archived: Project = serde_json::from_value(json!({
            "id": "p", "name": "p", "type": "local", "createdAt": 0,
            "worktree": { "sourceProjectId": "s", "branch": "x", "repoDir": "/r", "createdByFalcon": true, "archivedAt": 9 }
        }))
        .unwrap();
        ws.apply_projects(&[archived]);
        assert_eq!(ws.selected_project_id, None);
        assert_eq!(ws.active, ActiveView::Overview);
    }

    /// 画布按宽并排两列自适应（见 layout 的 COLUMN_FIT_PX）
    const TWO: f64 = crate::layout::COLUMN_FIT_PX * 2.0 + crate::layout::CANVAS_GAP_PX;

    fn canvases(ws: &WorkspaceState, sessions: &[SessionWithProject]) -> Vec<Vec<Vec<String>>> {
        ws.view_canvases(sessions)
            .iter()
            .map(|g| g.columns.iter().map(|c| c.panes.iter().map(|p| p.key.clone()).collect()).collect())
            .collect()
    }

    #[test]
    fn settle_assigns_canvases_once_the_width_is_known() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("c", "p", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["a", "b", "c"] {
            ws.open_session(id, &sessions);
        }
        // 还没量出画布宽：不分，显示上都挤在一块
        assert_eq!(ws.settle_canvases(&sessions), Settled { columns: false, changed: true });
        assert_eq!(canvases(&ws, &sessions).len(), 1);
        assert!(ws.set_canvas_width(TWO));
        let settled = ws.settle_canvases(&sessions);
        assert!(settled.columns && settled.changed);
        assert_eq!(canvases(&ws, &sessions), [vec![vec!["t:a"], vec!["t:b"]], vec![vec!["t:c"]]]);
        // 收敛之后再跑一遍什么也不变
        assert_eq!(ws.settle_canvases(&sessions), Settled::default());
        // 当前画布跟着活动窗口（c 在第二块）
        assert_eq!(ws.current_canvas(&sessions), Some(ws.view_canvases(&sessions)[1].id.clone()));
        // 画布落盘，读回来还是同两块
        let saved = ws.persisted();
        assert!(saved.columns.iter().all(|c| c.canvas.is_some()));
        let back = WorkspaceState::from_persisted(&load_workspace(Some(&serialize_workspace(&saved))));
        assert_eq!(canvases(&back, &sessions), canvases(&ws, &sessions));
    }

    #[test]
    fn new_terminals_fill_the_current_canvas_before_opening_a_new_one() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("c", "p", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["a", "b", "c"] {
            ws.open_session(id, &sessions);
        }
        ws.set_canvas_width(TWO);
        ws.settle_canvases(&sessions);
        // 回到第一块（已满）开一个：另起一块紧跟在它后面，不跑到 c 那块
        ws.focus_pane("t:a");
        ws.settle_canvases(&sessions);
        let pid = ws.begin_pending("p", None, None, &sessions);
        ws.settle_canvases(&sessions);
        let pending = format!("t:{pid}");
        assert_eq!(
            canvases(&ws, &sessions),
            [vec![vec!["t:a".to_string()], vec!["t:b".into()]], vec![vec![pending.clone()]], vec![vec!["t:c".into()]]]
        );
        // 在第三块（只有 c，放得下）开一个：就落在 c 旁边
        ws.focus_pane("t:c");
        ws.settle_canvases(&sessions);
        let pid2 = ws.begin_pending("p", None, None, &sessions);
        ws.settle_canvases(&sessions);
        assert_eq!(canvases(&ws, &sessions)[2], [vec!["t:c".to_string()], vec![format!("t:{pid2}")]]);
    }

    #[test]
    fn show_and_step_canvas_return_focus_to_where_it_was() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("c", "p", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["a", "b", "c"] {
            ws.open_session(id, &sessions);
        }
        ws.set_canvas_width(TWO);
        ws.settle_canvases(&sessions);
        ws.focus_pane("t:b");
        ws.settle_canvases(&sessions);
        assert!(ws.step_canvas(1, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "c".into() });
        ws.settle_canvases(&sessions);
        // 已经在最右一块：不回绕
        assert!(!ws.step_canvas(1, &sessions));
        // 切回第一块：焦点还给上次停的 b，而不是第一扇 a
        assert!(ws.step_canvas(-1, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "b".into() });
        // 点正在显示的那块：什么也不做
        ws.settle_canvases(&sessions);
        let first = ws.view_canvases(&sessions)[0].id.clone();
        assert!(!ws.show_canvas(&first, &sessions));
    }

    #[test]
    fn with_focus_in_the_pinned_column_switching_canvas_keeps_focus() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2), session("x", "p", 3)];
        let mut ws = WorkspaceState::default();
        for id in ["a", "b", "x"] {
            ws.open_session(id, &sessions);
        }
        ws.toggle_pin_pane("t:x");
        ws.set_canvas_width(TWO);
        ws.settle_canvases(&sessions);
        // 固定列也占宽：每块只放一列 + 固定列
        assert_eq!(
            canvases(&ws, &sessions),
            [vec![vec!["t:a"], vec!["t:x"]], vec![vec!["t:b"], vec!["t:x"]]]
        );
        ws.focus_pane("t:x");
        ws.settle_canvases(&sessions);
        let second = ws.view_canvases(&sessions)[1].id.clone();
        assert!(ws.show_canvas(&second, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "x".into() });
        assert_eq!(ws.current_canvas(&sessions).as_deref(), Some(second.as_str()));
    }

    #[test]
    fn move_pane_to_canvas_follows_the_pane() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2)];
        let mut ws = WorkspaceState::default();
        ws.open_session("a", &sessions);
        ws.open_session("b", &sessions);
        ws.set_canvas_width(TWO);
        ws.settle_canvases(&sessions);
        assert_eq!(canvases(&ws, &sessions).len(), 1);
        assert!(ws.move_pane_to_canvas("t:a", None, &sessions));
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "a".into() });
        assert_eq!(canvases(&ws, &sessions), [vec![vec!["t:b"]], vec![vec!["t:a"]]]);
        // 本来就独占一列待在那块上：没动
        let own = ws.view_canvases(&sessions)[1].id.clone();
        assert!(!ws.move_pane_to_canvas("t:a", Some(&own), &sessions));
    }

    #[test]
    fn drop_tab_moves_focus_to_the_nearest_visible_terminal() {
        let sessions = vec![session("a", "p", 1), session("b", "p", 2)];
        let mut ws = WorkspaceState::default();
        ws.open_session("a", &sessions);
        ws.open_session("b", &sessions);
        ws.drop_tab("b", &sessions);
        assert_eq!(ws.active, ActiveView::Terminal { session_id: "a".into() });
        ws.drop_tab("a", &sessions);
        assert_eq!(ws.active, ActiveView::Project);
        assert!(ws.columns.is_empty());
    }

    #[test]
    fn panels_toggles_and_selectors() {
        let mut ws = WorkspaceState::default();
        ws.toggle_right_panel(RightPanelId::Files);
        assert!(ws.right_open && ws.right_panel == RightPanelId::Files);
        ws.toggle_right_panel(RightPanelId::Files);
        assert!(!ws.right_open);
        ws.set_sidebar_auto_hidden(true);
        assert!(!ws.sidebar_visible());
        ws.toggle_sidebar();
        assert!(ws.sidebar_open && ws.sidebar_visible());
        assert!(ws.set_sidebar_width(999.0));
        assert_eq!(ws.sidebar_width, 420.0);
        assert!(!ws.set_sidebar_width(430.0));
        ws.toggle_sessions("p");
        ws.toggle_sessions("p");
        assert_eq!(ws.sessions_open.get("p"), Some(&false));
        assert_eq!(active_key(&ActiveView::Diff).as_deref(), Some("d"));
        assert_eq!(pane_view("f:p:x/y"), Some(ActiveView::File { project_id: "p".into(), path: "x/y".into() }));

        let multi: Project = serde_json::from_value(json!({
            "id": "m", "name": "m", "type": "local", "createdAt": 0,
            "multi": { "repos": [{ "dir": "/a" }, { "dir": "/b" }] }
        }))
        .unwrap();
        let mut picks = BTreeMap::new();
        assert_eq!(select_multi_repo_dir(&picks, Some(&multi)).as_deref(), Some("/a"));
        picks.insert("m".into(), "/b".into());
        assert_eq!(select_multi_repo_dir(&picks, Some(&multi)).as_deref(), Some("/b"));
        picks.insert("m".into(), "/gone".into());
        assert_eq!(select_multi_repo_dir(&picks, Some(&multi)).as_deref(), Some("/a"));
        assert_eq!(select_multi_repo_dir(&picks, Some(&project("p"))), None);
    }

    #[test]
    fn session_state_and_title_updates() {
        let mut sessions = vec![session("a", "p", 0)];
        apply_session_state(&mut sessions, "a", SessionState::Dead, Some(DeadReason::Exited));
        assert_eq!(sessions[0].dead_reason, Some(DeadReason::Exited));
        apply_session_state(&mut sessions, "a", SessionState::Dead, None);
        assert_eq!(sessions[0].dead_reason, Some(DeadReason::Exited));
        apply_session_state(&mut sessions, "a", SessionState::Active, None);
        assert_eq!(sessions[0].dead_reason, None);
        apply_session_title(&mut sessions, "a", Some("pnpm dev"));
        assert_eq!(sessions[0].title.as_deref(), Some("pnpm dev"));
        apply_session_title(&mut sessions, "a", None);
        assert_eq!(sessions[0].title, None);
    }
}
