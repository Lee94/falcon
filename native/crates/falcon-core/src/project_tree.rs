//! 「机器 → 环境 → 线程」与旧项目共用的分组视图模型。对应旧 React 版的 `lib/projectTree.ts`。
//! 桌面侧栏默认的线程视图不分组，列表来自 [`filter_and_sort_threads`]（默认口径见 [`thread_list`]）；
//! 按机器分组的树只在侧栏的兼容视图里出现。
//!
//! 侧栏与切换面板共用：两边必须看到同一棵树，分组规则只能有一份。纯函数，零 I/O。
//! 这里的"服务器"是 SSH Host 分组（本机 / 已保存主机 / 没绑主机的存量 SSH），
//! 不是 falcon 服务端。

use std::collections::{HashMap, HashSet};

use falcon_proto::{ProgramState, Project, ProjectType, SessionState, SessionWithProject, SshHost};
use serde::{Deserialize, Serialize};

use crate::host_color::{HostBar, host_bar_from_ssh, ssh_bar, ssh_conn};

/// 源项目工作区当前 HEAD，侧栏在没有 worktree 时用分支名代表默认仓库
/// （React 版 `store.ts` 里的 `ProjectHead`）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectHead {
    pub branch: Option<String>,
    pub sha: Option<String>,
}

/// 第一层的种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Local,
    /// 已保存的主机
    Host,
    /// 没有绑定主机的存量 SSH 项目，按 user@host:port 聚成一组
    Legacy,
}

impl ServerKind {
    /// React 版里的字面量（`"local" | "host" | "legacy"`）
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::Local => "local",
            ServerKind::Host => "host",
            ServerKind::Legacy => "legacy",
        }
    }
}

/// 第一层：本机、已保存主机、以及没有绑定主机的存量 SSH
#[derive(Debug, Clone, PartialEq)]
pub struct ServerGroup {
    /// `s:local` / `s:host:<id>` / `s:legacy:<conn>`，展开状态的键
    pub key: String,
    pub kind: ServerKind,
    pub name: String,
    pub conn: Option<String>,
    /// 身份色条；React 版是 CSS 字符串，这里是结构化的（见 [`crate::host_color`]）
    pub bar: Option<HostBar>,
    pub host: Option<SshHost>,
    /// 无项目的独立会话分组 id（`standalone:local` / `standalone:host:<id>`）；存量 SSH 组没有
    pub standalone_id: Option<String>,
    pub folders: Vec<FolderGroup>,
}

/// 第二层：一个文件夹项目。
///
/// 有附属 worktree 的普通源项目，第三层是当前检出与各附属项目。没有附属 worktree 的普通源项目、
/// 多仓库项目（含批量派生出来的）自己就是这一层：`worktrees` 为空，会话直接挂在项目行上，
/// 派生产物按全局顺序另占一行，不嵌在源项目下面。
#[derive(Debug, Clone, PartialEq)]
pub struct FolderGroup {
    pub project: Project,
    pub worktrees: Vec<Project>,
}

/// 侧栏上独占一行的项目：普通源项目，以及多仓库项目。
///
/// 旧多仓库派生行与源项目同级；线程始终挂在所属环境下。
fn is_sidebar_folder(p: &Project) -> bool {
    !p.is_thread() && (p.worktree.is_none() || p.multi.is_some())
}

pub fn folder_key(project_id: &str) -> String {
    format!("p:{project_id}")
}

/// 检出行的标签。
///
/// 多仓库容器没有单一 HEAD，返回空串。侧栏不再给多仓库项目画检出行。
/// `??` 语义照抄：探到的分支名是空串也原样用，不往下落。派生行（含多仓库派生）用分支名。
pub fn checkout_label(project: &Project, head: Option<&ProjectHead>) -> String {
    if project.is_environment() || project.is_thread() { return project.name.clone(); }
    if let Some(wt) = &project.worktree {
        return wt.branch.clone();
    }
    if project.multi.is_some() {
        return String::new();
    }
    head.and_then(|h| h.branch.clone().or_else(|| h.sha.clone())).unwrap_or_else(|| project.name.clone())
}

/// JS 真值：`p.hostId` 是空串时当没有
fn truthy(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// 存档的附属项目：`p.worktree?.archivedAt` 为真（0 在 JS 里是假）
fn archived(p: &Project) -> bool {
    p.worktree.as_ref().and_then(|w| w.archived_at).is_some_and(|t| t != 0)
}

/// 存量 SSH 项目的分组键：有连接配置按 user@host:port，没有就是 `"ssh"`
fn legacy_conn(p: &Project) -> String {
    p.ssh.as_ref().map(ssh_conn).unwrap_or_else(|| "ssh".to_string())
}

pub fn group_servers(projects: &[Project], hosts: &[SshHost], local_name: &str, show_archived: bool) -> Vec<ServerGroup> {
    // 存档的附属项目默认不占侧栏；开关一开，单仓库的回到源项目下面，多仓库的回到自己那一行
    let shown: Vec<&Project> = projects.iter().filter(|p| show_archived || !archived(p)).collect();
    let sources: Vec<&Project> = shown.iter().copied().filter(|p| is_sidebar_folder(p)).collect();
    let source_ids: HashSet<&str> = sources.iter().map(|p| p.id.as_str()).collect();
    let mut kids_by_source: HashMap<&str, Vec<&Project>> = HashMap::new();
    let mut orphans: Vec<&Project> = Vec::new();
    for p in &shown {
        // 多仓库派生是同级文件夹，不挂到源项目下面
        if is_sidebar_folder(p) {
            continue;
        }
        // sourceProjectId 为空串的附属项目在 TS 里既不是源、也不挂在谁下面，整个不出现
        let Some(src) = p.thread_environment_id.as_deref()
            .or_else(|| p.worktree.as_ref().map(|w| w.source_project_id.as_str()))
            .filter(|s| !s.is_empty()) else {
            continue;
        };
        if source_ids.contains(src) {
            kids_by_source.entry(src).or_default().push(p);
        } else if !p.is_thread() {
            orphans.push(p);
        }
    }

    let folders_of = |matches: &dyn Fn(&Project) -> bool| -> Vec<FolderGroup> {
        let mut out: Vec<FolderGroup> = sources
            .iter()
            .filter(|p| matches(p))
            .map(|p| FolderGroup {
                project: (*p).clone(),
                worktrees: kids_by_source.get(p.id.as_str()).map(|v| v.iter().map(|w| (*w).clone()).collect()).unwrap_or_default(),
            })
            .collect();
        out.extend(orphans.iter().filter(|p| matches(p)).map(|p| FolderGroup { project: (*p).clone(), worktrees: Vec::new() }));
        out
    };

    let mut servers = vec![ServerGroup {
        key: "s:local".into(),
        kind: ServerKind::Local,
        name: local_name.to_string(),
        conn: None,
        bar: None,
        host: None,
        standalone_id: Some(falcon_proto::STANDALONE_LOCAL.to_string()),
        folders: folders_of(&|p| p.project_type == ProjectType::Local),
    }];

    for host in hosts {
        servers.push(ServerGroup {
            key: format!("s:host:{}", host.id),
            kind: ServerKind::Host,
            name: host.name.clone(),
            conn: Some(ssh_conn(host)),
            bar: Some(host_bar_from_ssh(host)),
            host: Some(host.clone()),
            standalone_id: Some(falcon_proto::standalone_for_host(&host.id)),
            folders: folders_of(&|p| p.project_type == ProjectType::Ssh && p.host_id.as_deref() == Some(host.id.as_str())),
        });
    }

    let mut seen: HashSet<String> = HashSet::new();
    for p in &shown {
        if p.project_type != ProjectType::Ssh || truthy(&p.host_id).is_some() {
            continue;
        }
        let conn = legacy_conn(p);
        if !seen.insert(conn.clone()) {
            continue;
        }
        servers.push(ServerGroup {
            key: format!("s:legacy:{conn}"),
            kind: ServerKind::Legacy,
            name: p.ssh.as_ref().map(|s| s.host.clone()).unwrap_or_else(|| conn.clone()),
            conn: Some(conn.clone()),
            bar: ssh_bar(Some(p)),
            host: None,
            standalone_id: None,
            folders: folders_of(&|x| x.project_type == ProjectType::Ssh && truthy(&x.host_id).is_none() && legacy_conn(x) == conn),
        });
    }

    servers
}

/// 侧栏线程列表的排序方式（随工作区持久化，见 `workspace::PersistedWorkspace`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadSort {
    /// 最近创建（默认）：按 `created_at` 倒序，同一毫秒保留服务端顺序
    #[default]
    CreatedDesc,
    /// 最近活跃：按线程最近活动时间倒序（取会话最新活跃时间，无会话退回创建时间）
    LastActiveDesc,
    /// 状态优先：需关注 → 运行中 → 活跃中 → 闲置，同档按最近活跃倒序
    StatusPriority,
    /// 名称 A → Z（忽略大小写）
    NameAsc,
    /// 名称 Z → A（忽略大小写）
    NameDesc,
    /// 最早创建：按 `created_at` 正序
    CreatedAsc,
}

impl ThreadSort {
    pub const ALL: &'static [Self] = &[
        Self::CreatedDesc,
        Self::LastActiveDesc,
        Self::StatusPriority,
        Self::NameAsc,
        Self::NameDesc,
        Self::CreatedAsc,
    ];

    pub fn is_default(&self) -> bool {
        *self == Self::CreatedDesc
    }

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::CreatedDesc => "created_desc",
            Self::LastActiveDesc => "last_active_desc",
            Self::StatusPriority => "status_priority",
            Self::NameAsc => "name_asc",
            Self::NameDesc => "name_desc",
            Self::CreatedAsc => "created_asc",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "created_desc" => Self::CreatedDesc,
            "last_active_desc" => Self::LastActiveDesc,
            "status_priority" => Self::StatusPriority,
            "name_asc" => Self::NameAsc,
            "name_desc" => Self::NameDesc,
            "created_asc" => Self::CreatedAsc,
            _ => return None,
        })
    }
}

impl Serialize for ThreadSort {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for ThreadSort {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&s).unwrap_or_default())
    }
}

/// 侧栏线程列表的状态过滤条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadStatusFilter {
    #[default]
    All,
    /// 活跃中：有活跃终端会话、创建中会话或打开的 Claude 对话窗口
    Active,
    /// 运行中：有程序处于 working、创建中会话，或 Claude 对话正在处理
    Working,
    /// 需关注：有程序处于 blocked / 未读 done 或 error、会话为 unverified / dead、
    /// 创建失败，或 Claude 对话等待权限审批
    NeedsAttention,
    /// 有代码改动：工作区存在未提交的新增或删除行
    HasChanges,
}

impl ThreadStatusFilter {
    pub const ALL: &'static [Self] = &[
        Self::All,
        Self::Active,
        Self::Working,
        Self::NeedsAttention,
        Self::HasChanges,
    ];
}

/// 侧栏线程列表的机器过滤条件。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThreadMachineFilter {
    #[default]
    All,
    /// 仅本机线程
    Local,
    /// 指定已保存的远端主机
    Host(String),
}

/// 侧栏线程列表的过滤条件（各项之间为与关系）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThreadFilter {
    /// 关键词查询：按空白分词，每个词均须命中线程名称或运行态补充检索文本（忽略大小写）
    pub query: String,
    pub status: ThreadStatusFilter,
    pub machine: ThreadMachineFilter,
    /// 指定所属环境 ID
    pub environment_id: Option<String>,
    /// 是否包含已存档的旧工作树线程（与 [`group_servers`] 的 `show_archived` 口径一致）
    pub show_archived: bool,
}

impl ThreadFilter {
    /// 是否启用了状态、机器或环境等结构化过滤（不含搜索词与 `show_archived`）
    pub fn has_structured_filter(&self) -> bool {
        self.status != ThreadStatusFilter::All
            || self.machine != ThreadMachineFilter::All
            || self.environment_id.is_some()
    }

    /// 是否有任何收窄列表的条件生效（结构化过滤或非空搜索词）
    pub fn is_active(&self) -> bool {
        self.has_structured_filter() || !self.query.trim().is_empty()
    }

    /// 清除搜索词与结构化过滤条件，保留 `show_archived` 偏好
    pub fn clear(&mut self) {
        self.query.clear();
        self.status = ThreadStatusFilter::All;
        self.machine = ThreadMachineFilter::All;
        self.environment_id = None;
    }
}

/// 单个线程参与过滤与排序的运行态摘要（由调用方从实时状态汇总，纯值、零 UI 依赖）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadRuntime {
    pub has_active: bool,
    pub is_working: bool,
    pub needs_attention: bool,
    pub has_changes: bool,
    /// 线程下会话的最近活动时间（unix 毫秒）；无会话时为 `None`，排序时回退到 `thread.created_at`
    pub last_active_at: Option<i64>,
    /// 参与关键词搜索的额外文本（所属环境名、所属机器名、会话名称 / 自动标题 / Agent 名等）
    pub search_terms: Vec<String>,
}

impl ThreadRuntime {
    /// 从属于该线程的会话切片汇总基础运行态与会话搜索词；调用方可继续叠加 pending、对话窗口、
    /// 工作区改动和环境 / 机器名称。
    pub fn from_sessions<'a>(sessions: impl IntoIterator<Item = &'a SessionWithProject>) -> Self {
        let mut rt = Self::default();
        for s in sessions {
            let activity = s
                .program_status
                .as_ref()
                .map(|p| s.last_active_at.max(p.updated_at))
                .unwrap_or(s.last_active_at);
            rt.last_active_at = Some(rt.last_active_at.map_or(activity, |cur| cur.max(activity)));
            match s.state {
                SessionState::Active => {
                    rt.has_active = true;
                    if let Some(ps) = &s.program_status {
                        match ps.state {
                            ProgramState::Working => rt.is_working = true,
                            ProgramState::Blocked | ProgramState::Error | ProgramState::Done => {
                                rt.needs_attention = true;
                            }
                            ProgramState::Idle | ProgramState::Unknown => {}
                        }
                    }
                }
                SessionState::Unverified | SessionState::Dead | SessionState::Unknown => {
                    rt.needs_attention = true;
                }
            }
            if !s.name.is_empty() {
                rt.search_terms.push(s.name.clone());
            }
            if let Some(title) = s.title.as_deref().filter(|t| !t.is_empty()) {
                rt.search_terms.push(title.to_string());
            }
            if let Some(agent) = s.agent {
                rt.search_terms.push(agent.as_str().to_string());
            }
        }
        rt
    }

    /// `ThreadSort::StatusPriority` 的优先级档位（数值越小排越前）：
    /// 0 = 需关注、1 = 运行中、2 = 活跃中、3 = 闲置
    pub fn status_rank(&self) -> u8 {
        if self.needs_attention {
            0
        } else if self.is_working {
            1
        } else if self.has_active {
            2
        } else {
            3
        }
    }
}

/// 判断项目（环境或线程）是否落在指定的机器过滤条件内。
pub fn project_matches_machine(project: &Project, machine: &ThreadMachineFilter) -> bool {
    match machine {
        ThreadMachineFilter::All => true,
        ThreadMachineFilter::Local => project.project_type == ProjectType::Local,
        ThreadMachineFilter::Host(id) => {
            project.project_type == ProjectType::Ssh && truthy(&project.host_id) == Some(id.as_str())
        }
    }
}

/// 已存档旧工作树线程的数量，供线程视图的过滤菜单标注「显示已存档（N）」。
pub fn archived_thread_count(projects: &[Project]) -> usize {
    projects.iter().filter(|p| p.is_thread() && archived(p)).count()
}

/// 侧栏线程视图的一级列表：平铺全部线程，默认按创建时间倒序排列。
///
/// 机器与环境是创建线程时的选项，侧栏不再用它们分层；所属环境由调用方按
/// [`thread_environment`] 查。创建时间相同的保留服务端顺序。存档的旧工作树线程默认不列出，
/// 与 [`group_servers`] 的 `show_archived` 口径一致。需要过滤或非默认排序时使用
/// [`filter_and_sort_threads`]。
pub fn thread_list(projects: &[Project], show_archived: bool) -> Vec<Project> {
    let filter = ThreadFilter { show_archived, ..Default::default() };
    filter_and_sort_threads(projects, &filter, ThreadSort::CreatedDesc, |_| ThreadRuntime::default())
}

/// 按过滤条件和排序规则计算侧栏线程列表。
///
/// 排序全部采用稳定排序：比较键相同时保留服务端返回的相对顺序。
/// `runtime_of` 仅对通过基础过滤（类型、存档、机器、环境）的线程各调用一次。
pub fn filter_and_sort_threads(
    projects: &[Project],
    filter: &ThreadFilter,
    sort: ThreadSort,
    mut runtime_of: impl FnMut(&Project) -> ThreadRuntime,
) -> Vec<Project> {
    let tokens: Vec<String> = filter.query.split_whitespace().map(|w| w.to_lowercase()).collect();

    struct Entry {
        project: Project,
        lower_name: String,
        effective_last_active: i64,
        status_rank: u8,
    }

    let mut entries: Vec<Entry> = Vec::new();
    for p in projects {
        if !p.is_thread() {
            continue;
        }
        if !filter.show_archived && archived(p) {
            continue;
        }
        if !project_matches_machine(p, &filter.machine) {
            continue;
        }
        if let Some(env_id) = filter.environment_id.as_deref()
            && thread_environment(p) != Some(env_id)
        {
            continue;
        }

        let rt = runtime_of(p);
        let status_ok = match filter.status {
            ThreadStatusFilter::All => true,
            ThreadStatusFilter::Active => rt.has_active,
            ThreadStatusFilter::Working => rt.is_working,
            ThreadStatusFilter::NeedsAttention => rt.needs_attention,
            ThreadStatusFilter::HasChanges => rt.has_changes,
        };
        if !status_ok {
            continue;
        }

        let lower_name = p.name.to_lowercase();
        if !tokens.is_empty() {
            let lower_terms: Vec<String> = rt.search_terms.iter().map(|s| s.to_lowercase()).collect();
            let matched = tokens
                .iter()
                .all(|tok| lower_name.contains(tok) || lower_terms.iter().any(|term| term.contains(tok)));
            if !matched {
                continue;
            }
        }

        let effective_last_active = thread_last_active(p, &rt);
        let status_rank = rt.status_rank();
        entries.push(Entry {
            project: p.clone(),
            lower_name,
            effective_last_active,
            status_rank,
        });
    }

    // sort_by 是稳定排序：所有比较键相等时保留服务端给的相对顺序
    match sort {
        ThreadSort::CreatedDesc => entries.sort_by(|a, b| b.project.created_at.cmp(&a.project.created_at)),
        ThreadSort::CreatedAsc => entries.sort_by(|a, b| a.project.created_at.cmp(&b.project.created_at)),
        ThreadSort::LastActiveDesc => entries.sort_by(|a, b| {
            b.effective_last_active
                .cmp(&a.effective_last_active)
                .then_with(|| b.project.created_at.cmp(&a.project.created_at))
        }),
        ThreadSort::StatusPriority => entries.sort_by(|a, b| {
            a.status_rank
                .cmp(&b.status_rank)
                .then_with(|| b.effective_last_active.cmp(&a.effective_last_active))
                .then_with(|| b.project.created_at.cmp(&a.project.created_at))
        }),
        ThreadSort::NameAsc => entries.sort_by(|a, b| {
            a.lower_name
                .cmp(&b.lower_name)
                .then_with(|| b.project.created_at.cmp(&a.project.created_at))
        }),
        ThreadSort::NameDesc => entries.sort_by(|a, b| {
            b.lower_name
                .cmp(&a.lower_name)
                .then_with(|| b.project.created_at.cmp(&a.project.created_at))
        }),
    }

    entries.into_iter().map(|e| e.project).collect()
}

/// 线程的最近活动时间：会话的最近活跃时间与创建时间取晚的（没有会话就是创建时间）
pub fn thread_last_active(thread: &Project, rt: &ThreadRuntime) -> i64 {
    rt.last_active_at.map_or(thread.created_at, |t| t.max(thread.created_at))
}

/// 线程行尾巴上写的时间、分段也按它分：按创建时间排的看创建时间，其余看最近活动
pub fn thread_sort_time(sort: ThreadSort, thread: &Project, rt: &ThreadRuntime) -> i64 {
    match sort {
        ThreadSort::CreatedDesc | ThreadSort::CreatedAsc => thread.created_at,
        _ => thread_last_active(thread, rt),
    }
}

/// 线程列表的分段（照 Claude 桌面版的「今天 / 昨天 / 过去 7 天」）。按时间排时按本地日期分，
/// 状态优先时按 [`ThreadRuntime::status_rank`] 的四档分，按名称排不分段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSection {
    Today,
    Yesterday,
    Last7Days,
    Last30Days,
    Older,
    NeedsAttention,
    Working,
    Active,
    Idle,
}

/// 时间戳落在哪个日期段。按本地日历日比，不按「距今 24 小时」：昨晚 11 点的线程今早就算昨天。
/// `offset_of(秒)` 是那一刻本地相对 UTC 的偏移（秒），逐个时间戳取，夏令时两边才都对；
/// 比现在还晚的（别的机器时钟快）算今天
pub fn day_section(ts_ms: i64, now_ms: i64, offset_of: impl Fn(i64) -> i64) -> ThreadSection {
    let day = |ms: i64| (ms.div_euclid(1000) + offset_of(ms.div_euclid(1000))).div_euclid(86_400);
    match day(now_ms) - day(ts_ms) {
        ..=0 => ThreadSection::Today,
        1 => ThreadSection::Yesterday,
        2..=7 => ThreadSection::Last7Days,
        8..=30 => ThreadSection::Last30Days,
        _ => ThreadSection::Older,
    }
}

/// 已排好序的线程列表从哪几行开始换段：`(段, 起始下标)`，相邻同段合并。`keys` 与列表逐行对应，
/// 是 `(thread_sort_time, status_rank)`。按名称排返回空——名称排序下分段只会把字母切碎
pub fn thread_sections(
    sort: ThreadSort,
    keys: &[(i64, u8)],
    now_ms: i64,
    offset_of: impl Fn(i64) -> i64,
) -> Vec<(ThreadSection, usize)> {
    let section_of = |&(time, rank): &(i64, u8)| match sort {
        ThreadSort::NameAsc | ThreadSort::NameDesc => None,
        ThreadSort::StatusPriority => Some(match rank {
            0 => ThreadSection::NeedsAttention,
            1 => ThreadSection::Working,
            2 => ThreadSection::Active,
            _ => ThreadSection::Idle,
        }),
        _ => Some(day_section(time, now_ms, &offset_of)),
    };
    let mut out: Vec<(ThreadSection, usize)> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let Some(section) = section_of(key) else { return Vec::new() };
        if out.last().is_none_or(|(last, _)| *last != section) {
            out.push((section, i));
        }
    }
    out
}

/// 线程名称的字数上限（按字符）。侧栏一行放得下，又能看出是哪件事
pub const THREAD_NAME_MAX_CHARS: usize = 40;

/// 新建线程不单独填名称：取第一条消息的第一行非空文字，超长截断并补省略号。消息为空时
/// `None`，由调用方给一个默认名（服务端要求名称非空）。名称之后可在编辑里改
pub fn thread_name_from_prompt(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    // 多个连续空白收成一个，免得从代码块里粘进来的缩进占满一行
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= THREAD_NAME_MAX_CHARS {
        return Some(line);
    }
    let cut: String = line.chars().take(THREAD_NAME_MAX_CHARS - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// 线程所属环境的 ID。新线程记在 `thread_environment_id`，旧工作树线程记在 `worktree.sourceProjectId`
pub fn thread_environment(thread: &Project) -> Option<&str> {
    thread
        .thread_environment_id
        .as_deref()
        .or_else(|| thread.worktree.as_ref().map(|w| w.source_project_id.as_str()))
        .filter(|id| !id.is_empty())
}

/// 侧栏拖拽排序：把 `id` 挪到 `target` 的前面（`after = false`）或后面，其余的相对顺序不动。
///
/// 顺序是全局一条（服务端 `projects.sort_order`），侧栏按服务器分组时保留相对顺序，所以在
/// 全局列表里挪到 target 旁边，组内看就是挪到了 target 旁边；单仓库附属项目挂在源项目下面、不看全局
/// 位置，跟着走不用管。多仓库派生自己占一行，排序跟普通项目一样。服务端落库与客户端的乐观更新都走这一个函数，两边挪出来的才一样。
///
/// 两个都得在、且不是同一个，否则 None；挪完和原来一样（本来就挨着）也是 None——调用方据此
/// 省掉一次请求
pub fn move_in_order<T: Clone>(items: &[T], key: impl Fn(&T) -> &str, id: &str, target: &str, after: bool) -> Option<Vec<T>> {
    if id == target {
        return None;
    }
    let from = items.iter().position(|x| key(x) == id)?;
    let mut out = items.to_vec();
    let moved = out.remove(from);
    let at = out.iter().position(|x| key(x) == target)? + usize::from(after);
    out.insert(at, moved);
    let unchanged = out.iter().zip(items).all(|(a, b)| key(a) == key(b));
    (!unchanged).then_some(out)
}

/// 侧栏拖拽的落点：组里第 `from` 个放到第 `at` 个的前面 / 后面等于没动（放到自己身上、紧挨着的
/// 上一个后面、紧挨着的下一个前面）。这些落点不画提示线、也不接——与 [`move_in_order`] 回 None
/// 的情形对得上，落点那边只有组内下标，不用再拿整份列表算一遍
pub fn lands_in_place(from: usize, at: usize, after: bool) -> bool {
    from == at || if after { from == at + 1 } else { from + 1 == at }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(json: &str) -> Project {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn day_section_uses_local_calendar_days() {
        const H: i64 = 3_600_000;
        const D: i64 = 24 * H;
        // 东八区 2026-11-27 09:00 = UTC 01:00
        let now = 1_795_741_200_000_i64;
        let cst = |_| 8 * 3600;
        assert_eq!(day_section(now, now, cst), ThreadSection::Today);
        assert_eq!(day_section(now + H, now, cst), ThreadSection::Today);
        // 本地 00:30 还是今天，前一晚 23:30 已是昨天（只差一小时）
        assert_eq!(day_section(now - 8 * H - 30 * 60_000, now, cst), ThreadSection::Today);
        assert_eq!(day_section(now - 9 * H - 30 * 60_000, now, cst), ThreadSection::Yesterday);
        assert_eq!(day_section(now - 2 * D, now, cst), ThreadSection::Last7Days);
        assert_eq!(day_section(now - 7 * D, now, cst), ThreadSection::Last7Days);
        assert_eq!(day_section(now - 8 * D, now, cst), ThreadSection::Last30Days);
        assert_eq!(day_section(now - 31 * D, now, cst), ThreadSection::Older);
        // 同一刻按 UTC 算是 01:00，两小时前就跨进了前一天：日期段跟着时区走
        assert_eq!(day_section(now - 2 * H, now, |_| 0), ThreadSection::Yesterday);
    }

    #[test]
    fn thread_sections_merge_runs_and_skip_name_sorts() {
        const D: i64 = 86_400_000;
        let now = 100 * D + D / 2;
        let utc = |_| 0;
        let keys = [(now, 3), (now - 1, 3), (now - D, 0), (now - 3 * D, 1), (now - 40 * D, 2)];
        assert_eq!(
            thread_sections(ThreadSort::CreatedDesc, &keys, now, utc),
            vec![
                (ThreadSection::Today, 0),
                (ThreadSection::Yesterday, 2),
                (ThreadSection::Last7Days, 3),
                (ThreadSection::Older, 4),
            ]
        );
        let ranks = [(0, 0), (0, 0), (0, 1), (0, 3)];
        assert_eq!(
            thread_sections(ThreadSort::StatusPriority, &ranks, now, utc),
            vec![(ThreadSection::NeedsAttention, 0), (ThreadSection::Working, 2), (ThreadSection::Idle, 3)]
        );
        assert!(thread_sections(ThreadSort::NameAsc, &keys, now, utc).is_empty());
        assert!(thread_sections(ThreadSort::CreatedDesc, &[], now, utc).is_empty());
    }

    #[test]
    fn move_in_order_places_beside_target() {
        let ids = ["a", "b", "c", "d"];
        let mv = |id, target, after| move_in_order(&ids, |s| s, id, target, after);
        assert_eq!(mv("d", "a", false).as_deref(), Some(&["d", "a", "b", "c"][..]));
        assert_eq!(mv("a", "c", true).as_deref(), Some(&["b", "c", "a", "d"][..]));
        assert_eq!(mv("a", "c", false).as_deref(), Some(&["b", "a", "c", "d"][..]));
        assert_eq!(mv("c", "a", true).as_deref(), Some(&["a", "c", "b", "d"][..]));
        // 本来就挨着：放到下一个的前面 / 上一个的后面都是原样
        assert_eq!(mv("a", "b", false), None);
        assert_eq!(mv("b", "a", true), None);
        assert_eq!(mv("a", "a", true), None);
        assert_eq!(mv("x", "a", true), None);
        assert_eq!(mv("a", "x", true), None);

        // 落点那边的判定与 move_in_order 是否回 None 逐一对得上
        for from in 0..ids.len() {
            for at in 0..ids.len() {
                for after in [false, true] {
                    assert_eq!(lands_in_place(from, at, after), mv(ids[from], ids[at], after).is_none(), "{from} → {at} {after}");
                }
            }
        }
    }

    #[test]
    fn moved_folder_keeps_its_worktrees_in_the_group() {
        let projects = vec![
            p(r#"{"id":"s1","name":"s1","type":"local","createdAt":0}"#),
            p(r#"{"id":"w1","name":"w1","type":"local","createdAt":0,
                  "worktree":{"sourceProjectId":"s1","branch":"x","repoDir":"/r","createdByFalcon":true}}"#),
            p(r#"{"id":"h","name":"h","type":"ssh","hostId":"h1","createdAt":0}"#),
            p(r#"{"id":"s2","name":"s2","type":"local","createdAt":0}"#),
        ];
        let moved = move_in_order(&projects, |p| p.id.as_str(), "s2", "s1", false).unwrap();
        let local = &group_servers(&moved, &[], "本机", false)[0];
        let ids: Vec<&str> = local.folders.iter().map(|f| f.project.id.as_str()).collect();
        assert_eq!(ids, ["s2", "s1"]);
        assert_eq!(local.folders[1].worktrees[0].id, "w1");
    }

    #[test]
    fn hosts_and_legacy_servers_follow_the_local_group() {
        let hosts: Vec<SshHost> = serde_json::from_str(
            r#"[{"id":"h1","name":"dev","host":"10.0.0.2","port":22,"username":"root","authMethod":"agent",
                 "hasSecret":false,"projectCount":1,"createdAt":0},
                {"id":"h2","name":"idle","host":"10.0.0.3","port":22,"username":"root","authMethod":"agent",
                 "hasSecret":false,"projectCount":0,"createdAt":0}]"#,
        )
        .unwrap();
        let ssh = r#""ssh":{"host":"old.box","port":2222,"username":"u","authMethod":"key","hasSecret":false}"#;
        let projects = vec![
            p(r#"{"id":"a","name":"a","type":"ssh","hostId":"h1","createdAt":0}"#),
            p(&format!(r#"{{"id":"b","name":"b","type":"ssh",{ssh},"createdAt":0}}"#)),
            p(&format!(r#"{{"id":"c","name":"c","type":"ssh","hostId":"",{ssh},"createdAt":0}}"#)),
            p(r#"{"id":"d","name":"d","type":"ssh","createdAt":0}"#),
            p(r#"{"id":"e","name":"e","type":"local","createdAt":0}"#),
        ];
        let servers = group_servers(&projects, &hosts, "本机", false);
        let keys: Vec<&str> = servers.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["s:local", "s:host:h1", "s:host:h2", "s:legacy:u@old.box:2222", "s:legacy:ssh"]);
        let ids = |s: &ServerGroup| s.folders.iter().map(|f| f.project.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&servers[0]), ["e"]);
        assert_eq!(ids(&servers[1]), ["a"]);
        assert!(servers[2].folders.is_empty());
        // hostId 为空串在 JS 里是假：与没有 hostId 的 b 归到同一组存量 SSH
        assert_eq!(ids(&servers[3]), ["b", "c"]);
        assert_eq!(servers[3].name, "old.box");
        assert_eq!(servers[4].name, "ssh");
        assert_eq!(servers[4].bar, Some(HostBar::Neutral));
        assert_eq!(servers[1].conn.as_deref(), Some("root@10.0.0.2:22"));
        assert!(matches!(servers[1].bar, Some(HostBar::Hue(_))));
        assert_eq!(servers[0].standalone_id.as_deref(), Some("standalone:local"));
        assert_eq!(servers[1].standalone_id.as_deref(), Some("standalone:host:h1"));
        assert_eq!(servers[2].standalone_id.as_deref(), Some("standalone:host:h2"));
        assert_eq!(servers[3].standalone_id, None);
        assert_eq!(servers[4].standalone_id, None);
    }

    #[test]
    fn archived_worktrees_hide_until_asked_for() {
        let projects = vec![
            p(r#"{"id":"s","name":"s","type":"local","createdAt":0}"#),
            p(r#"{"id":"w","name":"w","type":"local","createdAt":0,
                  "worktree":{"sourceProjectId":"s","branch":"x","repoDir":"/r","createdByFalcon":true,"archivedAt":5}}"#),
        ];
        assert!(group_servers(&projects, &[], "本机", false)[0].folders[0].worktrees.is_empty());
        assert_eq!(group_servers(&projects, &[], "本机", true)[0].folders[0].worktrees.len(), 1);
        assert_eq!(folder_key("s"), "p:s");
    }

    #[test]
    fn thread_is_nested_under_environment_even_with_multiple_repos() {
        let env = p(r#"{"id":"env","name":"env","type":"local","createdAt":0,"environmentContext":"","multi":{"repos":[{"dir":"/a"},{"dir":"/b"}]}}"#);
        let task = p(r#"{"id":"thread","name":"task","type":"local","createdAt":1,"environmentContext":"notes",
            "multi":{"repos":[{"dir":"/threads/x/projects/a"},{"dir":"/threads/x/projects/b"}]},
            "worktree":{"sourceProjectId":"env","branch":"falcon/thread-x","repoDir":"","createdByFalcon":true}}"#);
        let servers = group_servers(&[task, env], &[], "本机", false);
        let folders = &servers[0].folders;
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].project.id, "env");
        assert_eq!(checkout_label(&folders[0].worktrees[0], None), "task");
    }

    #[test]
    fn thread_list_is_flat_and_newest_first() {
        let projects = vec![
            p(r#"{"id":"env","name":"env","type":"local","createdAt":0,"environmentContext":""}"#),
            p(r#"{"id":"remote-env","name":"remote","type":"ssh","hostId":"h1","createdAt":0,"environmentContext":""}"#),
            p(r#"{"id":"t1","name":"t1","type":"local","createdAt":10,"environmentContext":"","threadEnvironmentId":"env"}"#),
            p(r#"{"id":"t2","name":"t2","type":"ssh","hostId":"h1","createdAt":30,"environmentContext":"","threadEnvironmentId":"remote-env"}"#),
            p(r#"{"id":"t3","name":"t3","type":"local","createdAt":30,"environmentContext":"","threadEnvironmentId":"env"}"#),
            p(r#"{"id":"old","name":"old","type":"local","createdAt":20,"environmentContext":"",
                  "worktree":{"sourceProjectId":"env","branch":"falcon/thread-old","repoDir":"/r","createdByFalcon":true}}"#),
            p(r#"{"id":"gone","name":"gone","type":"local","createdAt":40,"environmentContext":"",
                  "worktree":{"sourceProjectId":"env","branch":"falcon/thread-gone","repoDir":"/r","createdByFalcon":true,"archivedAt":5}}"#),
            p(r#"{"id":"plain","name":"plain","type":"local","createdAt":50}"#),
        ];
        let ids = |show_archived| thread_list(&projects, show_archived).into_iter().map(|t| t.id).collect::<Vec<_>>();
        // 跨机器混排；同一时间的 t2、t3 保留服务端顺序；环境和普通项目不进列表
        assert_eq!(ids(false), ["t2", "t3", "old", "t1"]);
        assert_eq!(ids(true), ["gone", "t2", "t3", "old", "t1"]);
        assert_eq!(thread_environment(&projects[2]), Some("env"));
        assert_eq!(thread_environment(&projects[5]), Some("env"));
        assert_eq!(thread_environment(&projects[0]), None);
    }

    #[test]
    fn thread_name_is_the_first_line_of_the_message() {
        assert_eq!(thread_name_from_prompt("\n  修复登录超时  \n再补上测试").as_deref(), Some("修复登录超时"));
        assert_eq!(thread_name_from_prompt("fix   the\tlogin"), Some("fix the login".into()));
        assert_eq!(thread_name_from_prompt(" \n\t "), None);
        let long = "很".repeat(THREAD_NAME_MAX_CHARS + 5);
        let name = thread_name_from_prompt(&long).unwrap();
        assert_eq!(name.chars().count(), THREAD_NAME_MAX_CHARS);
        assert!(name.ends_with('…'));
        let exact = "a".repeat(THREAD_NAME_MAX_CHARS);
        assert_eq!(thread_name_from_prompt(&exact), Some(exact.clone()));
    }

    #[test]
    fn derived_multi_project_is_a_sibling_folder() {
        let projects = vec![
            p(r#"{"id":"s","name":"s","type":"local","createdAt":0}"#),
            p(r#"{"id":"w","name":"w","type":"local","createdAt":0,
                  "worktree":{"sourceProjectId":"s","branch":"x","repoDir":"/r","createdByFalcon":true}}"#),
            p(r#"{"id":"c","name":"组合","type":"local","createdAt":0,"multi":{"repos":[{"dir":"/a/web"}]}}"#),
            p(r#"{"id":"d","name":"feat-x","type":"local","createdAt":0,
                  "multi":{"repos":[{"dir":"/a/组合-feat-x/web","repoDir":"/a/web"}]},
                  "worktree":{"sourceProjectId":"c","branch":"feat/x","repoDir":"","createdByFalcon":true}}"#),
            p(r#"{"id":"h","name":"hidden","type":"local","createdAt":0,
                  "multi":{"repos":[{"dir":"/a/组合-old/web","repoDir":"/a/web"}]},
                  "worktree":{"sourceProjectId":"c","branch":"old","repoDir":"","createdByFalcon":true,"archivedAt":5}}"#),
        ];
        let folders = &group_servers(&projects, &[], "本机", false)[0].folders;
        let ids: Vec<&str> = folders.iter().map(|f| f.project.id.as_str()).collect();
        assert_eq!(ids, ["s", "c", "d"]);
        assert_eq!(folders[0].worktrees[0].id, "w");
        assert!(folders[1].worktrees.is_empty());
        assert!(folders[2].worktrees.is_empty());
        let shown = &group_servers(&projects, &[], "本机", true)[0].folders;
        let shown_ids: Vec<&str> = shown.iter().map(|f| f.project.id.as_str()).collect();
        assert_eq!(shown_ids, ["s", "c", "d", "h"]);
        assert!(shown.iter().all(|f| f.project.id == "s" || f.worktrees.is_empty()));
    }

    fn sess(json: &str) -> SessionWithProject {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn thread_sort_wire_roundtrip_and_unknown_fallback() {
        for &sort in ThreadSort::ALL {
            let wire = sort.as_wire();
            assert_eq!(ThreadSort::from_wire(wire), Some(sort));
            let json = serde_json::to_string(&sort).unwrap();
            assert_eq!(serde_json::from_str::<ThreadSort>(&json).unwrap(), sort);
        }
        assert!(ThreadSort::CreatedDesc.is_default());
        assert!(!ThreadSort::LastActiveDesc.is_default());
        assert_eq!(ThreadSort::from_wire("unknown_sort"), None);
        assert_eq!(serde_json::from_str::<ThreadSort>(r#""unknown_sort""#).unwrap(), ThreadSort::CreatedDesc);
    }

    #[test]
    fn thread_runtime_aggregates_sessions_and_ranks() {
        let s_idle = sess(
            r#"{"id":"s1","projectId":"t1","name":"shell","state":"active","durable":true,
                "createdAt":10,"lastActiveAt":100,"projectName":"t1","projectType":"local"}"#,
        );
        let s_working = sess(
            r#"{"id":"s2","projectId":"t1","name":"","title":"cargo test","agent":"claude","state":"active","durable":true,
                "programStatus":{"state":"working","updatedAt":250},"createdAt":10,"lastActiveAt":200,
                "projectName":"t1","projectType":"local"}"#,
        );
        let s_blocked = sess(
            r#"{"id":"s3","projectId":"t1","name":"","state":"active","durable":true,
                "programStatus":{"state":"blocked","kind":"permission","updatedAt":300},"createdAt":10,"lastActiveAt":150,
                "projectName":"t1","projectType":"local"}"#,
        );
        let s_dead = sess(
            r#"{"id":"s4","projectId":"t2","name":"","state":"dead","durable":false,
                "createdAt":10,"lastActiveAt":50,"projectName":"t2","projectType":"local"}"#,
        );

        let empty = ThreadRuntime::from_sessions(std::iter::empty());
        assert_eq!(empty.status_rank(), 3);
        assert_eq!(empty.last_active_at, None);

        let active = ThreadRuntime::from_sessions([&s_idle]);
        assert!(active.has_active && !active.is_working && !active.needs_attention);
        assert_eq!(active.status_rank(), 2);
        assert_eq!(active.last_active_at, Some(100));

        let working = ThreadRuntime::from_sessions([&s_idle, &s_working]);
        assert!(working.has_active && working.is_working && !working.needs_attention);
        assert_eq!(working.status_rank(), 1);
        assert_eq!(working.last_active_at, Some(250));
        assert!(working.search_terms.contains(&"cargo test".to_string()));
        assert!(working.search_terms.contains(&"claude".to_string()));

        let blocked = ThreadRuntime::from_sessions([&s_working, &s_blocked]);
        assert!(blocked.needs_attention);
        assert_eq!(blocked.status_rank(), 0);
        assert_eq!(blocked.last_active_at, Some(300));

        let dead = ThreadRuntime::from_sessions([&s_dead]);
        assert!(!dead.has_active && dead.needs_attention);
        assert_eq!(dead.status_rank(), 0);
    }

    #[test]
    fn filter_threads_by_query_machine_env_status_and_archived() {
        let projects = vec![
            p(r#"{"id":"env-a","name":"mojito","type":"local","createdAt":0,"environmentContext":""}"#),
            p(r#"{"id":"env-b","name":"backend","type":"ssh","hostId":"h1","createdAt":0,"environmentContext":""}"#),
            p(r#"{"id":"t1","name":"修复登录超时","type":"local","createdAt":10,"environmentContext":"","threadEnvironmentId":"env-a"}"#),
            p(r#"{"id":"t2","name":"API 性能压测","type":"ssh","hostId":"h1","createdAt":20,"environmentContext":"","threadEnvironmentId":"env-b"}"#),
            p(r#"{"id":"t3","name":"重构侧栏过滤","type":"local","createdAt":30,"environmentContext":"","threadEnvironmentId":"env-a"}"#),
            p(r#"{"id":"t4","name":"旧工作树任务","type":"local","createdAt":40,"environmentContext":"",
                  "worktree":{"sourceProjectId":"env-a","branch":"falcon/old","repoDir":"/r","createdByFalcon":true,"archivedAt":5}}"#),
        ];

        assert_eq!(archived_thread_count(&projects), 1);

        let rt = |t: &Project| match t.id.as_str() {
            "t1" => ThreadRuntime {
                has_active: true,
                needs_attention: true,
                last_active_at: Some(500),
                search_terms: vec!["mojito".into(), "本机".into(), "claude".into()],
                ..Default::default()
            },
            "t2" => ThreadRuntime {
                has_active: true,
                is_working: true,
                has_changes: true,
                last_active_at: Some(900),
                search_terms: vec!["backend".into(), "work".into()],
                ..Default::default()
            },
            "t3" => ThreadRuntime {
                has_changes: true,
                last_active_at: None,
                search_terms: vec!["mojito".into(), "本机".into()],
                ..Default::default()
            },
            _ => ThreadRuntime::default(),
        };

        let run = |filter: &ThreadFilter, sort: ThreadSort| -> Vec<String> {
            filter_and_sort_threads(&projects, filter, sort, rt).into_iter().map(|t| t.id).collect()
        };

        // 关键词：忽略大小写、多词 AND、可跨线程名 / 环境名 / 机器名 / 会话词命中
        let q = |query: &str| ThreadFilter { query: query.into(), ..Default::default() };
        assert_eq!(run(&q("api work"), ThreadSort::CreatedDesc), ["t2"]);
        assert_eq!(run(&q("MOJITO claude"), ThreadSort::CreatedDesc), ["t1"]);
        assert_eq!(run(&q("不存在"), ThreadSort::CreatedDesc), Vec::<String>::new());

        // 按机器过滤
        let local_only = ThreadFilter { machine: ThreadMachineFilter::Local, ..Default::default() };
        assert_eq!(run(&local_only, ThreadSort::CreatedDesc), ["t3", "t1"]);
        let host_only = ThreadFilter { machine: ThreadMachineFilter::Host("h1".into()), ..Default::default() };
        assert_eq!(run(&host_only, ThreadSort::CreatedDesc), ["t2"]);

        // 按环境过滤（含旧工作树存档开关）
        let env_a = ThreadFilter { environment_id: Some("env-a".into()), ..Default::default() };
        assert_eq!(run(&env_a, ThreadSort::CreatedDesc), ["t3", "t1"]);
        let env_a_archived = ThreadFilter { show_archived: true, ..env_a };
        assert_eq!(run(&env_a_archived, ThreadSort::CreatedDesc), ["t4", "t3", "t1"]);

        // 按状态过滤
        let st = |status| ThreadFilter { status, ..Default::default() };
        assert_eq!(run(&st(ThreadStatusFilter::Active), ThreadSort::CreatedDesc), ["t2", "t1"]);
        assert_eq!(run(&st(ThreadStatusFilter::Working), ThreadSort::CreatedDesc), ["t2"]);
        assert_eq!(run(&st(ThreadStatusFilter::NeedsAttention), ThreadSort::CreatedDesc), ["t1"]);
        assert_eq!(run(&st(ThreadStatusFilter::HasChanges), ThreadSort::CreatedDesc), ["t3", "t2"]);

        // 组合过滤与清除
        let mut combined = ThreadFilter {
            query: "mojito".into(),
            status: ThreadStatusFilter::HasChanges,
            machine: ThreadMachineFilter::Local,
            environment_id: Some("env-a".into()),
            show_archived: true,
        };
        assert!(combined.has_structured_filter());
        assert!(combined.is_active());
        assert_eq!(run(&combined, ThreadSort::CreatedDesc), ["t3"]);
        combined.clear();
        assert!(!combined.has_structured_filter());
        assert!(!combined.is_active());
        assert!(combined.show_archived, "清除过滤应保留 show_archived 偏好");
    }

    #[test]
    fn sort_threads_by_all_modes() {
        let projects = vec![
            p(r#"{"id":"t1","name":"Beta","type":"local","createdAt":10,"environmentContext":"","threadEnvironmentId":"env"}"#),
            p(r#"{"id":"t2","name":"alpha","type":"local","createdAt":30,"environmentContext":"","threadEnvironmentId":"env"}"#),
            p(r#"{"id":"t3","name":"Gamma","type":"local","createdAt":20,"environmentContext":"","threadEnvironmentId":"env"}"#),
            p(r#"{"id":"t4","name":"delta","type":"local","createdAt":40,"environmentContext":"","threadEnvironmentId":"env"}"#),
        ];

        // t1: 需关注，最近活跃 100
        // t2: 活跃中，最近活跃 400
        // t3: 运行中，最近活跃 300
        // t4: 闲置（无会话，回退到 created_at=40）
        let rt = |t: &Project| match t.id.as_str() {
            "t1" => ThreadRuntime { has_active: true, needs_attention: true, last_active_at: Some(100), ..Default::default() },
            "t2" => ThreadRuntime { has_active: true, last_active_at: Some(400), ..Default::default() },
            "t3" => ThreadRuntime { has_active: true, is_working: true, last_active_at: Some(300), ..Default::default() },
            _ => ThreadRuntime::default(),
        };

        let ids = |sort| {
            filter_and_sort_threads(&projects, &ThreadFilter::default(), sort, rt)
                .into_iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ids(ThreadSort::CreatedDesc), ["t4", "t2", "t3", "t1"]);
        assert_eq!(ids(ThreadSort::CreatedAsc), ["t1", "t3", "t2", "t4"]);
        assert_eq!(ids(ThreadSort::LastActiveDesc), ["t2", "t3", "t1", "t4"]);
        assert_eq!(ids(ThreadSort::StatusPriority), ["t1", "t3", "t2", "t4"]);
        assert_eq!(ids(ThreadSort::NameAsc), ["t2", "t1", "t4", "t3"]);
        assert_eq!(ids(ThreadSort::NameDesc), ["t3", "t4", "t1", "t2"]);
    }
}
