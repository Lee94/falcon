//! 侧栏默认是线程视图：顶上固定一行「新建线程」与线程过滤 / 排序条，下面平铺所有线程，默认按
//! 创建时间倒序排列（[`filter_and_sort_threads`]）。机器与环境是新建线程对话框里的选项，侧栏不
//! 单独给它们开入口，只在线程视图提供按关键词 / 状态 / 机器 / 环境过滤和常见排序。
//!
//! 线程列表照 Claude 桌面版 / Orca 的样子画：
//! - 按时间排时分「今天 / 昨天 / 过去 7 天 / 过去 30 天 / 更早」几段，状态优先时按状态分段，
//!   按名称排不分段（[`thread_sections`]）；
//! - 线程行两行：上面是状态记号 + 名称 + 时间，下面淡色写环境（远端再带机器）、改动行数与会话数。
//!   行首那一格只画整条线程最要紧的一个状态（[`ThreadGlyph`]），一列里扫一眼就知道该点哪条；
//! - 时间在悬停时让位给 ＋（新建会话）与「…」（线程菜单），行宽不变、名称不跳。
//!
//! 底栏的切换按钮打开按机器分组的兼容视图：机器 → 环境 → 线程，外加旧项目与独立终端。
//! 编辑 / 删除环境、在机器上开独立终端、管理旧项目都在这一视图。
//! 多仓库项目与没有附属 worktree 的单仓库项目不往下挂检出：项目行自己承接会话。
//! 单仓库有附属 worktree 时才显示各条检出，批量派生出来的多仓库项目与源项目同级。
//!
//! - 兼容视图里新建环境与旧项目走各机器行 hover 的 ＋ / 右键 / 命令面板；
//! - 行的 hover / 选中是内缩的圆角块（左右留内边距，贴着岛边会切掉圆角）；
//! - 会话行默认收起：一个检出能挂五六个终端，全摊开就占满侧栏；检出行尾巴上摆计数徽标 +
//!   最重的异常状态，折叠着也看得出出没出事——包括有程序卡在 blocked 等你（OSC 7501，ADR 0020）；
//! - 文件夹行可以拖着在同一台机器的组里排序，顺序存在服务端，各个客户端看到的一样。
//!   单仓库附属项目跟着源项目走，不单独拖；多仓库派生自己占一行，可以拖；
//! - 本机分组一直留着。不单独列「无项目」分组，独立会话直接挂在所属机器下面，开终端走机器行的菜单。

use falcon_core::program_status::{RowReading, RowStatus};
use falcon_core::project_tree::{
    FolderGroup, ServerGroup, ServerKind, ThreadFilter, ThreadMachineFilter, ThreadRuntime, ThreadSection, ThreadSort,
    ThreadStatusFilter, archived_thread_count, checkout_label, filter_and_sort_threads, folder_key, group_servers,
    lands_in_place, project_matches_machine, thread_environment, thread_list, thread_sections, thread_sort_time,
};
use falcon_proto::{Project, ProjectType, SessionState, SessionWithProject};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::{Disableable, Sizable, tooltip::Tooltip};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, KeyDownEvent, Pixels, Render, SharedString,
    Subscription, WeakEntity, Window, div, relative,
};
use rust_i18n::t;

use crate::labels::{age_text, local_word, session_label};
use crate::menus::{self, MenuItemSpec, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::{DragLabel, Mark, host_bar, icon, needs_you_mark, spinner, status_mark};
use crate::workspace::{ActiveView, PendingSession, ProjectChanges, Workspace};
use crate::zoom::zpx;

const ROW_H: f32 = 30.;
const SESSION_ROW_H: f32 = 28.;
/// 树里相邻两行之间的缝。选中的项目（检出行）与当前会话各自高亮、又常常上下挨着（会话行就摊在它的
/// 检出下面），没有缝两块圆角底色会粘成一条；每一层的 flex_col 都要带上，跨层的相邻行才同样有缝
const ROW_GAP: f32 = 2.;
/// 过滤菜单里机器 / 环境超过这个数量时改用子菜单，免得下拉菜单过长
const FILTER_INLINE_MACHINES: usize = 5;
const FILTER_INLINE_ENVS: usize = 6;

fn indent(depth: usize) -> Pixels {
    zpx(4. + depth as f32 * 12.)
}

/// 侧栏里拖着的源项目。只在同一个服务器分组里挪：项目属于哪台机器是定死的，拖到别的组里放下
/// 也只是全局顺序里换了个邻居、组内看不出变化，不如不接。`index` 是它在组里的位置，落点据此
/// 认出"放回原处"（[`lands_in_place`]）
#[derive(Clone)]
struct ProjectDrag {
    id: String,
    group: String,
    index: usize,
}

pub struct Sidebar {
    /// 按机器分组的兼容视图；默认是平铺的线程视图
    by_machine: bool,
    /// 线程视图的关键词搜索框（仅当前窗口内存态，不持久化）
    query_input: Entity<InputState>,
    /// 线程视图的结构化过滤条件（状态 / 机器 / 环境）；渲染时合入搜索词与 `ws.state.show_archived`
    filter: ThreadFilter,
    terminal_positions: std::collections::HashMap<String, (usize, usize, usize)>,
    ws: Entity<Workspace>,
    _subs: Vec<Subscription>,
}

impl Sidebar {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("sidebar.filterPlaceholder").to_string()));
        let subs = vec![
            cx.observe(&ws, |this, _, cx| {
                this.reconcile_filter(cx);
                cx.notify();
            }),
            cx.subscribe_in(&query_input, window, |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        // 会话行尾巴上的时长按分钟走，列表没变时工作区不通知（轮询结果一样就跳过）：自己隔一会儿
        // 重画一次，不然「刚刚」能一直挂着。侧栏是缓存视图，半分钟一次重画不值一提
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(30)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            ws,
            by_machine: false,
            query_input,
            filter: ThreadFilter::default(),
            terminal_positions: Default::default(),
            _subs: subs,
        }
    }

    /// 已被删除的主机不再保留在机器过滤里，免得列表一直被无效条件筛空。
    /// 节点只是暂时离线时主机仍在 `ws.hosts` 里，不改动用户筛选。
    fn reconcile_filter(&mut self, cx: &Context<Self>) {
        let w = self.ws.read(cx);
        if !w.inventory_loaded() {
            return;
        }
        if let ThreadMachineFilter::Host(id) = &self.filter.machine
            && !w.hosts.iter().any(|h| &h.id == id)
        {
            self.filter.machine = ThreadMachineFilter::All;
            if let Some(env_id) = &self.filter.environment_id
                && w.project(env_id).is_none()
            {
                self.filter.environment_id = None;
            }
        }
    }

    fn effective_filter(&self, cx: &App) -> ThreadFilter {
        let mut f = self.filter.clone();
        f.query = self.query_input.read(cx).value().to_string();
        f.show_archived = self.ws.read(cx).state.show_archived;
        f
    }

    fn set_status_filter(&mut self, status: ThreadStatusFilter, cx: &mut Context<Self>) {
        if self.filter.status != status {
            self.filter.status = status;
            cx.notify();
        }
    }

    fn set_machine_filter(&mut self, machine: ThreadMachineFilter, cx: &mut Context<Self>) {
        if self.filter.machine == machine {
            return;
        }
        self.filter.machine = machine;
        // 切换机器时，若当前环境不属于新选中的机器则一并清掉，避免出现必然为空的组合
        if let Some(env_id) = self.filter.environment_id.as_deref() {
            let matches = self
                .ws
                .read(cx)
                .project(env_id)
                .is_some_and(|env| project_matches_machine(env, &self.filter.machine));
            if !matches {
                self.filter.environment_id = None;
            }
        }
        cx.notify();
    }

    fn set_env_filter(&mut self, env_id: Option<String>, cx: &mut Context<Self>) {
        if self.filter.environment_id != env_id {
            self.filter.environment_id = env_id;
            cx.notify();
        }
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.clear();
        if !self.query_input.read(cx).value().is_empty() {
            self.query_input.update(cx, |i, cx| i.set_value("", window, cx));
        }
        cx.notify();
    }

    fn on_query_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            cx.stop_propagation();
            if !self.query_input.read(cx).value().is_empty() {
                self.query_input.update(cx, |i, cx| i.set_value("", window, cx));
                cx.notify();
            } else {
                window.blur(cx);
            }
        }
    }

    fn thread_runtime(&self, thread: &Project, cx: &App) -> ThreadRuntime {
        let ws = self.ws.read(cx);
        let mut rt = ThreadRuntime::from_sessions(ws.sessions.iter().filter(|s| s.project_id == thread.id));
        for pending in ws.state.pending.iter().filter(|p| p.project_id == thread.id) {
            if pending.error.is_some() {
                rt.needs_attention = true;
            } else {
                rt.has_active = true;
                rt.is_working = true;
            }
        }
        if ws.state.chats.iter().any(|id| id == &thread.id) {
            rt.has_active = true;
            if let Some(chat) = ws.chats.get(&thread.id) {
                let chat = chat.read(cx);
                if chat.is_busy() {
                    rt.is_working = true;
                }
                if chat.has_pending_permission() {
                    rt.needs_attention = true;
                }
            }
        }
        rt.has_changes = ws.changes.get(&thread.id).is_some_and(|c| c.added > 0 || c.deleted > 0);
        if let Some(env_name) = thread_environment(thread).and_then(|id| ws.project(id)).map(|p| p.name.clone()) {
            rt.search_terms.push(env_name);
        }
        let machine = thread_machine(ws, thread);
        if !machine.is_empty() {
            rt.search_terms.push(machine);
        }
        rt
    }
}

/// 折叠着也得看得出出没出事：取最重的那个状态，全是 active 就不摆记号
fn worst_state(sessions: &[&SessionWithProject], pending: &[&PendingSession]) -> Option<Mark> {
    if sessions.iter().any(|s| s.state == SessionState::Dead) || pending.iter().any(|p| p.error.is_some()) {
        return Some(Mark::Dead);
    }
    if sessions.iter().any(|s| s.state == SessionState::Unverified) {
        return Some(Mark::Unverified);
    }
    if !pending.is_empty() {
        return Some(Mark::Creating);
    }
    None
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let (columns, show_archived, sort, projects_empty, hosts_empty) = {
            let ws = self.ws.read(cx);
            (
                ws.state.columns.clone(),
                ws.state.show_archived,
                ws.state.thread_sort,
                ws.projects.is_empty(),
                ws.hosts.is_empty(),
            )
        };
        self.terminal_positions = falcon_core::layout::terminal_positions(&columns);
        let filter = self.effective_filter(cx);

        // 线程视图只要平铺的线程（按过滤与排序规则计算）；兼容视图里本机分组一律画出来，
        // 独立会话直接列在机器下面，见 [`Sidebar::server_node`]。
        // 运行态在过滤时算一遍就留着：分段与行尾时间都用它，不再逐行重算
        let mut runtimes: std::collections::HashMap<String, ThreadRuntime> = Default::default();
        let (threads, total_threads, servers) = if self.by_machine {
            let ws = self.ws.read(cx);
            (Vec::new(), 0, group_servers(&ws.projects, &ws.hosts, &local_word(), show_archived))
        } else {
            let projects = self.ws.read(cx).projects.clone();
            let total = thread_list(&projects, show_archived).len();
            let filtered = filter_and_sort_threads(&projects, &filter, sort, |t| {
                let rt = self.thread_runtime(t, cx);
                runtimes.insert(t.id.clone(), rt.clone());
                rt
            });
            (filtered, total, Vec::new())
        };
        let empty = if self.by_machine { projects_empty && hosts_empty } else { threads.is_empty() };
        let filtered_out = !self.by_machine && threads.is_empty() && total_threads > 0 && filter.is_active();

        // 「新建线程」与线程过滤 / 排序条不跟着树滚动：列表再长也一直在顶上
        let mut header = div().flex_none().px(zpx(8.)).pt(zpx(8.)).child(new_thread_row(&self.ws, cx));
        if !self.by_machine {
            header = header.child(self.render_thread_controls(&filter, sort, threads.len(), total_threads, cx));
        }

        let mut tree = div()
            .id("sidebar-tree")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(zpx(8.))
            .pt(zpx(ROW_GAP))
            .pb(zpx(8.))
            .flex()
            .flex_col();
        if empty {
            if filtered_out {
                tree = tree.child(
                    div()
                        .px_2()
                        .pt(zpx(8.))
                        .pb(zpx(10.))
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(zpx(6.))
                        .child(div().text_xs().text_color(ui.muted_foreground).child(t!("sidebar.filterEmpty").to_string()))
                        .child(
                            Button::new("sidebar-filter-empty-clear")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RotateCcw)
                                .label(t!("sidebar.filterClear").to_string())
                                .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx))),
                        ),
                );
            } else {
                let text = if self.by_machine { t!("sidebar.empty") } else { t!("sidebar.emptyThreads") };
                tree = tree.child(
                    div()
                        .px_2()
                        .pt(zpx(6.))
                        .pb(zpx(10.))
                        .text_xs()
                        .text_color(ui.muted_foreground)
                        .child(text.to_string()),
                );
            }
        }
        if self.by_machine {
            for server in servers {
                tree = tree.child(self.server_node(server, cx));
            }
        } else {
            let now = now_ms();
            let none = ThreadRuntime::default();
            let rt_of = |t: &Project| runtimes.get(&t.id).unwrap_or(&none);
            let keys: Vec<(i64, u8)> =
                threads.iter().map(|t| (thread_sort_time(sort, t, rt_of(t)), rt_of(t).status_rank())).collect();
            let sections = thread_sections(sort, &keys, now, crate::local_time::utc_offset_at);
            let mut list = div().flex().flex_col().gap(zpx(ROW_GAP));
            let mut next_section = sections.iter().peekable();
            for (i, thread) in threads.iter().enumerate() {
                if let Some((section, _)) = next_section.next_if(|(_, start)| *start == i) {
                    list = list.child(section_header(*section, i == 0, cx));
                }
                let age = (now - keys[i].0).max(0) as u64;
                list = list.child(self.thread_node(thread, age, cx));
            }
            tree = tree.child(list);
        }

        let ws_handle = self.ws.clone();
        let ws_handle2 = self.ws.clone();
        let (view_icon, view_tip) = if self.by_machine {
            (IconName::MessageCircle, t!("sidebar.threadView"))
        } else {
            (IconName::Server, t!("sidebar.machineView"))
        };
        let bottom = div()
            .h(zpx(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(zpx(8.))
            .child(
                Button::new("sidebar-settings")
                    .ghost()
                    .small()
                    .icon(IconName::Settings)
                    .label(t!("sidebar.settings").to_string())
                    .on_click(move |_, window, cx| {
                        crate::dialogs::settings::open(&ws_handle, Default::default(), window, cx)
                    }),
            )
            .child(Button::new("sidebar-view").ghost().small().icon(view_icon)
                .tooltip(view_tip.to_string()).on_click(cx.listener(|this, _, _, cx| {
                    this.by_machine = !this.by_machine;
                    cx.notify();
                })))
            .child(div().flex_1())
            .child(
                Button::new("sidebar-collapse")
                    .ghost()
                    .small()
                    .icon(IconName::PanelLeft)
                    .tooltip(format!("{} · {}", t!("sidebar.collapse"), crate::ui::chord("toggleSidebar")))
                    .on_click(move |_, _, cx| ws_handle2.update(cx, |w, cx| w.toggle_sidebar(cx))),
            );

        crate::ui::island(cx)
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .child(header)
            .child(tree)
            .child(bottom)
    }
}

impl Sidebar {
    /// 线程视图顶部的过滤与排序控件：搜索框 + 过滤菜单 + 排序菜单；启用了状态 / 机器 / 环境
    /// 过滤时，在下方紧跟着展示可一键取消的过滤芯片与命中计数。仅输入关键词时不另起一行，
    /// 输入框自带清除按钮，避免打字时列表上下跳动。
    fn render_thread_controls(
        &self,
        filter: &ThreadFilter,
        sort: ThreadSort,
        shown: usize,
        total: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let me = cx.entity().downgrade();
        let ws = self.ws.clone();

        let filter_active = filter.has_structured_filter();
        let filter_tip = if filter_active {
            let summary = self.filter_summary(filter, cx).join(" · ");
            t!("sidebar.filterActiveTooltip", summary = summary).to_string()
        } else {
            t!("sidebar.filterTooltip").to_string()
        };
        let me_filter = me.clone();
        let ws_filter = ws.clone();
        let mut filter_btn = Button::new("sidebar-thread-filter")
            .ghost()
            .xsmall()
            .icon(IconName::ListFilter)
            .tooltip(filter_tip);
        filter_btn = if filter_active {
            filter_btn.bg(ui.tint).text_color(ui.tint_foreground)
        } else {
            filter_btn.text_color(ui.muted_foreground)
        };
        let filter_btn = filter_btn.dropdown_menu(move |menu, window, cx| {
            build_filter_menu(menu, &me_filter, &ws_filter, window, cx)
        });

        let sort_active = !sort.is_default();
        let sort_tip = t!("sidebar.sortTooltip", name = sort_label(sort)).to_string();
        let ws_sort = ws.clone();
        let mut sort_btn = Button::new("sidebar-thread-sort")
            .ghost()
            .xsmall()
            .icon(IconName::ArrowUpDown)
            .tooltip(sort_tip);
        sort_btn = if sort_active {
            sort_btn.bg(ui.tint).text_color(ui.tint_foreground)
        } else {
            sort_btn.text_color(ui.muted_foreground)
        };
        let sort_btn = sort_btn.dropdown_menu(move |menu, _, cx| build_sort_menu(menu, &ws_sort, cx));

        let bar = div()
            .mt(zpx(4.))
            .flex()
            .items_center()
            .gap(zpx(4.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .capture_key_down(cx.listener(Self::on_query_key))
                    .child(
                        Input::new(&self.query_input)
                            .xsmall()
                            .cleanable(true)
                            .prefix(icon(IconName::Search).size(zpx(12.)).text_color(ui.muted_foreground)),
                    ),
            )
            .child(filter_btn)
            .child(sort_btn);

        let mut box_el = div().pb(zpx(4.)).flex().flex_col().child(bar);
        if filter_active {
            let mut chips = div().mt(zpx(6.)).flex().flex_wrap().items_center().gap(zpx(4.));
            if filter.status != ThreadStatusFilter::All {
                chips = chips.child(filter_chip(
                    "chip-status",
                    status_filter_label(filter.status),
                    cx.listener(|this, _, _, cx| this.set_status_filter(ThreadStatusFilter::All, cx)),
                    cx,
                ));
            }
            if filter.machine != ThreadMachineFilter::All {
                let label = machine_filter_label(&filter.machine, self.ws.read(cx));
                chips = chips.child(filter_chip(
                    "chip-machine",
                    label,
                    cx.listener(|this, _, _, cx| this.set_machine_filter(ThreadMachineFilter::All, cx)),
                    cx,
                ));
            }
            if let Some(env_id) = filter.environment_id.as_deref() {
                let label = env_filter_label(env_id, self.ws.read(cx));
                chips = chips.child(filter_chip(
                    "chip-env",
                    label,
                    cx.listener(|this, _, _, cx| this.set_env_filter(None, cx)),
                    cx,
                ));
            }
            chips = chips.child(
                div()
                    .ml(zpx(2.))
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(t!("sidebar.filterCount", shown = shown, total = total).to_string()),
            );
            let active_count = usize::from(filter.status != ThreadStatusFilter::All)
                + usize::from(filter.machine != ThreadMachineFilter::All)
                + usize::from(filter.environment_id.is_some())
                + usize::from(!filter.query.trim().is_empty());
            if active_count > 1 {
                chips = chips.child(
                    div()
                        .id("chip-clear-all")
                        .px(zpx(4.))
                        .h(zpx(20.))
                        .flex()
                        .items_center()
                        .rounded(radius::SM)
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.bg(ui.muted).text_color(ui.foreground))
                        .child(t!("sidebar.filterClear").to_string())
                        .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx))),
                );
            }
            box_el = box_el.child(chips);
        }
        box_el.into_any_element()
    }

    fn filter_summary(&self, filter: &ThreadFilter, cx: &App) -> Vec<String> {
        let ws = self.ws.read(cx);
        let mut parts = Vec::new();
        if filter.status != ThreadStatusFilter::All {
            parts.push(status_filter_label(filter.status));
        }
        if filter.machine != ThreadMachineFilter::All {
            parts.push(machine_filter_label(&filter.machine, ws));
        }
        if let Some(env_id) = filter.environment_id.as_deref() {
            parts.push(env_filter_label(env_id, ws));
        }
        parts
    }

    /// 线程视图的一行：两行式的线程行，会话摊在它下面（会话图标与线程名称对齐）。`age`：行尾写的
    /// 时间距今多久（毫秒），口径见 [`thread_sort_time`]
    fn thread_node(&self, thread: &Project, age: u64, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let mine: Vec<&SessionWithProject> = ws.sessions.iter().filter(|s| s.project_id == thread.id).collect();
        let mine_pending: Vec<&PendingSession> = ws.state.pending.iter().filter(|p| p.project_id == thread.id).collect();
        let sessions_open = ws.state.sessions_open.get(&thread.id).copied().unwrap_or(false);
        // 活动会话摊在线程下面时只强调最具体的那一行；收起后才由线程行承担选中态
        let active_child_visible = sessions_open
            && match &ws.state.active {
                ActiveView::Terminal { session_id } => mine.iter().any(|s| &s.id == session_id),
                _ => false,
            };
        let archived_at = thread.worktree.as_ref().and_then(|w| w.archived_at);
        let selected = archived_at.is_none()
            && !active_child_visible
            && ws.state.selected_project_id.as_deref() == Some(thread.id.as_str());
        let count = mine.len() + mine_pending.len();
        // 存档的线程开不了会话，也就没有会话行可摊
        let expandable = archived_at.is_none() && count > 0 && thread.environment_context.is_none();
        let glyph = self.thread_glyph(thread, &mine, &mine_pending, cx);
        let changes = if archived_at.is_some() { None } else { ws.changes.get(&thread.id).cloned() };
        let tooltip: SharedString = format!("{} · {}", thread_tooltip(ws, thread), t!("sidebar.projectContext")).into();

        // 第二行：环境，远端线程再带上机器（同名环境在不同机器上很常见）；存档的写删除倒计时
        let env = thread_environment(thread).and_then(|id| ws.project(id)).map(|p| p.name.clone());
        let machine = (thread.project_type == ProjectType::Ssh).then(|| thread_machine(ws, thread));
        let subtitle = match archived_at {
            Some(at) => archive_countdown(at),
            None => [env, machine].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
        };

        let id = thread.id.clone();
        let group: SharedString = format!("th-{id}").into();
        let menu_items = menus::project_menu_items(&self.ws, thread, cx);
        let ctx_items = menu_items.clone();
        let new_items = archived_at.is_none().then(|| menus::new_session_items(&self.ws, &thread.id, None, cx));
        let (fg, sub) = if selected {
            (ui.tint_foreground, ui.tint_foreground.opacity(0.7))
        } else if archived_at.is_some() {
            (ui.muted_foreground, ui.muted_foreground)
        } else {
            (ui.foreground, ui.muted_foreground)
        };

        // 行尾：平时写时间，悬停时同一格换成 ＋ 与「…」。按钮叠在时间上（绝对定位、靠右），格子
        // 给够两个按钮的宽度，名称的截断位置不随悬停变
        let mut actions = div().flex().items_center();
        if let Some(items) = new_items {
            let tip = if items.len() == 1 {
                items[0].label.clone()
            } else {
                format!("{} · {}", t!("sidebar.newTerminal"), crate::ui::chord("newTerminal"))
            };
            actions = actions.child(
                Button::new(SharedString::from(format!("th-new-{id}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(tip)
                    .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
            );
        }
        actions = actions.child(
            Button::new(SharedString::from(format!("th-menu-{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .dropdown_menu(move |menu, _, _| to_popup(menu, menu_items.clone())),
        );
        let trailing = div()
            .relative()
            .flex_none()
            .min_w(zpx(44.))
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .child(
                div()
                    .text_size(zpx(11.))
                    .text_color(sub)
                    .group_hover(group.clone(), |s| s.opacity(0.))
                    .child(age_text(age)),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(-zpx(4.))
                    .flex()
                    .items_center()
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .child(actions),
            );

        let title = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(fg)
            // 跑完了还没看的线程名称加粗，与会话行的「未读」同一个口径
            .when(glyph == ThreadGlyph::Unread, |d| d.font_weight(FontWeight::SEMIBOLD))
            .child(thread.name.clone());
        let line1 = div()
            .h(zpx(18.))
            .flex()
            .items_center()
            .gap(zpx(4.))
            .child(glyph_cell(glyph, &id, selected, cx))
            .child(title)
            .child(trailing);

        let mut meta = div().flex_none().flex().items_center().gap(zpx(6.)).text_size(zpx(11.));
        if let Some(badge) = changes.as_ref().and_then(|c| change_badge(c, cx)) {
            meta = meta.child(badge);
        }
        if expandable {
            let ws_toggle = self.ws.clone();
            let pid = id.clone();
            let label: SharedString = t!("sidebar.sessionCount", n = count).to_string().into();
            meta = meta.child(
                div()
                    .id(SharedString::from(format!("th-sessions-{id}")))
                    .flex()
                    .items_center()
                    .gap(zpx(3.))
                    .px(zpx(3.))
                    .rounded(zpx(4.))
                    .text_color(if sessions_open { fg } else { sub })
                    .hover(|s| s.text_color(fg))
                    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                    .child(icon(IconName::SquareTerminal).size(zpx(11.)))
                    .child(count.to_string())
                    .child(crate::ui::disclosure_chevron(
                        SharedString::from(format!("th-sessions-chev-{id}")),
                        sessions_open,
                        zpx(10.),
                    ))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        ws_toggle.update(cx, |w, cx| w.toggle_sessions(&pid, cx))
                    }),
            );
        }
        let line2 = div()
            .h(zpx(16.))
            .flex()
            .items_center()
            .gap(zpx(4.))
            .child(div().flex_none().w(zpx(GLYPH_W)))
            .child(div().flex_1().min_w_0().truncate().text_size(zpx(11.)).text_color(sub).child(subtitle))
            .child(meta);

        let mut row = div()
            .id(SharedString::from(format!("th-{id}")))
            .group(group)
            .flex()
            .flex_col()
            .gap(zpx(1.))
            .py(zpx(5.))
            .pl(zpx(THREAD_PAD_L))
            .pr(zpx(8.))
            .rounded(radius::SM)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(line1)
            .child(line2);
        row = if selected { row.bg(ui.tint) } else { row.hover(|s| s.bg(ui.muted)) };
        if archived_at.is_none() {
            let ws_select = self.ws.clone();
            let pid = id.clone();
            // 线程里开着什么（对话、终端、Agent）都在它的画布上，选中就是切过去
            row = row
                .cursor_pointer()
                .on_click(move |_, _, cx| ws_select.update(cx, |w, cx| w.select_project(&pid, cx)));
        }
        let row = row.context_menu(move |menu, _, _| to_popup(menu, ctx_items.clone()));
        let row = if selected {
            crate::ui::selected_fade(format!("sel-th-{id}"), row, ui.tint).into_any_element()
        } else {
            row.into_any_element()
        };

        let mut node = div().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expandable && sessions_open {
            node = node.children(self.terminal_rows(mine.into_iter(), mine_pending.into_iter(), 0, cx));
        }
        node.into_any_element()
    }

    /// 线程行首那一格：整条线程（会话、创建中的会话、Claude 对话）里最要紧的那个状态
    fn thread_glyph(
        &self,
        thread: &Project,
        sessions: &[&SessionWithProject],
        pending: &[&PendingSession],
        cx: &App,
    ) -> ThreadGlyph {
        let ws = self.ws.read(cx);
        let mut glyph = ThreadGlyph::None;
        let mut bump = |g: ThreadGlyph| glyph = glyph.max(g);
        for s in sessions {
            bump(match s.state {
                SessionState::Active => match ws.row_status(s).status {
                    RowStatus::Working => ThreadGlyph::Working,
                    RowStatus::NeedsYou => ThreadGlyph::NeedsYou,
                    RowStatus::Unread { error: true } => ThreadGlyph::Error,
                    RowStatus::Unread { error: false } => ThreadGlyph::Unread,
                    RowStatus::Read { .. } | RowStatus::Idle | RowStatus::Inactive => ThreadGlyph::Live,
                },
                SessionState::Dead => ThreadGlyph::Error,
                SessionState::Unverified | SessionState::Unknown => ThreadGlyph::Unverified,
            });
        }
        for p in pending {
            bump(if p.error.is_some() { ThreadGlyph::Error } else { ThreadGlyph::Working });
        }
        if ws.state.chats.iter().any(|id| id == &thread.id) {
            bump(ThreadGlyph::Live);
            if let Some(chat) = ws.chats.get(&thread.id) {
                let chat = chat.read(cx);
                if chat.is_busy() {
                    bump(ThreadGlyph::Working);
                }
                if chat.has_pending_permission() {
                    bump(ThreadGlyph::NeedsYou);
                }
            }
        }
        glyph
    }

    /// 兼容视图的机器分组
    fn server_node(&self, server: ServerGroup, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let unavailable = server.host.as_ref().and_then(|host| ws.node_status(&host.id)).filter(|node| !node.online);
        let expanded = unavailable.is_none() && !ws.state.collapsed.get(&server.key).copied().unwrap_or(false);
        let key = server.key.clone();
        let ws_toggle = self.ws.clone();
        let icon_name = if server.kind == ServerKind::Local { IconName::Monitor } else { IconName::Server };

        let standalone_id = server.standalone_id.clone();
        let mut menu_items = match (&server.kind, &server.host) {
            (ServerKind::Host, Some(host)) => Some(menus::host_menu_items(&self.ws, host)),
            (ServerKind::Local, _) => Some(menus::server_menu_items(&self.ws, true)),
            _ => None,
        };
        if unavailable.is_none()
            && let Some(id) = standalone_id.as_deref()
            && let Some(items) = menu_items.take()
        {
            menu_items = Some(menus::with_machine_terminal(&self.ws, id, items, cx));
        }
        let ctx_items = menu_items
            .clone()
            .unwrap_or_else(|| menus::server_menu_items(&self.ws, false));

        let ws_new = self.ws.clone();
        let preset = match (&server.kind, &server.host) {
            (ServerKind::Host, Some(h)) => crate::dialogs::project_form::Preset::ssh(Some(h.id.clone()), true),
            (ServerKind::Local, _) => crate::dialogs::project_form::Preset::local(true),
            _ => crate::dialogs::project_form::Preset::ssh(None, true),
        };
        let can_create = unavailable.is_none() && server.kind != ServerKind::Legacy;
        let offline = unavailable.is_some_and(|node| node.error.is_some());
        let status = unavailable.map(|_| {
            if offline { t!("sidebar.hostOffline") } else { t!("sidebar.hostConnecting") }.to_string()
        });
        let mut title = server.conn.clone().unwrap_or_else(|| server.name.clone());
        if let Some(status) = &status {
            title.push_str(&format!(" · {status}"));
            if let Some(error) = unavailable.and_then(|node| node.error.as_deref()) {
                title.push_str(&format!("\n{error}"));
            }
        }
        let title: SharedString = title.into();

        let mut row = div()
            .id(SharedString::from(format!("srv-{}", server.key)))
            .group("srv-row")
            .h(zpx(ROW_H))
            .flex()
            .items_center()
            .gap_1()
            .pl(zpx(4.))
            .pr(zpx(6.))
            .rounded(radius::SM)
            .hover(|s| s.bg(ui.muted))
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            .when(unavailable.is_none(), |row| {
                row.child(chevron(format!("srv-toggle-{key}"), expanded, cx, move |cx| {
                    ws_toggle.update(cx, |w, cx| w.toggle_collapsed(&key, cx))
                }))
            })
            .when(unavailable.is_some(), |row| row.child(div().w(zpx(18.)).flex_none()))
            .child(host_bar(server.bar, zpx(16.), cx))
            .child(icon(icon_name).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().pl_1().font_weight(FontWeight::MEDIUM).child(server.name.clone()))
            .when_some(status, |row, label| {
                row.child(div().flex_none().text_xs()
                    .text_color(if offline { ui.warning } else { ui.muted_foreground })
                    .child(label))
            })
            .child(
                div().opacity(0.).group_hover("srv-row", |s| s.opacity(1.)).child(
                    Button::new(SharedString::from(format!("srv-new-{}", server.key)))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .disabled(!can_create)
                        .tooltip(t!("env.create").to_string())
                        .on_click(move |_, window, cx| {
                            crate::dialogs::project_form::open(&ws_new, None, preset.clone(), window, cx)
                        }),
                ),
            );
        if let Some(items) = menu_items {
            row = row.child(
                Button::new(SharedString::from(format!("srv-menu-{}", server.key)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
            );
        }
        let row = row.context_menu(move |menu, _, _| to_popup(menu, ctx_items.clone()));

        let mut node = div().mb(zpx(ROW_GAP)).flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expanded {
            let mut has_standalone = false;
            if let Some(id) = standalone_id.as_deref() {
                let ws = self.ws.read(cx);
                let rows = self.terminal_rows(
                    ws.sessions.iter().filter(|s| s.project_id == id),
                    ws.state.pending.iter().filter(|p| p.project_id == id),
                    1,
                    cx,
                );
                has_standalone = !rows.is_empty();
                node = node.children(rows);
            }
            if server.folders.is_empty() {
                if !has_standalone {
                    node = node.child(
                        div()
                            .py_1()
                            .pr_2()
                            .pl(zpx(34.))
                            .text_size(zpx(11.))
                            .text_color(ui.muted_foreground)
                            .child(t!("sidebar.emptyServer").to_string()),
                    );
                }
            } else {
                let group = server.key.clone();
                for (index, folder) in server.folders.into_iter().enumerate() {
                    node = node.child(self.folder_node(folder, &group, index, cx));
                }
            }
        }
        node.into_any_element()
    }

    /// `group` / `index`：所在服务器分组的键与组内下标，拖拽排序用
    fn folder_node(&self, folder: FolderGroup, group: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let env = folder.project.is_environment();
        if !env && (folder.project.multi.is_some() || folder.worktrees.is_empty()) {
            return self.flat_folder_node(folder, group, index, cx);
        }
        let ws = self.ws.read(cx);
        let project = folder.project.clone();
        // 只有源项目能拖、能当落点。孤儿附属项目（源项目被藏起来了）也占一个文件夹位，但它们
        // 永远排在组的末尾（`group_servers`），挪了也没用
        let sortable = project.worktree.is_none();
        let open = ws.state.collapsed.get(&folder_key(&project.id)).copied() != Some(true);
        let head = ws.heads.get(&project.id).cloned();
        let selected = ws.state.selected_project_id.clone();
        let changes = ws.changes.clone();

        let ws_toggle = self.ws.clone();
        let ws_select = self.ws.clone();
        let fkey = folder_key(&project.id);
        let pid = project.id.clone();
        let member_dirs: Vec<&str> = project
            .multi
            .as_ref()
            .map(|m| m.repos.iter().map(|r| r.dir.as_str()).collect())
            .unwrap_or_default();
        let title = if !member_dirs.is_empty() {
            member_dirs.join(" · ")
        } else {
            project.working_dir.clone().unwrap_or_else(|| project.name.clone())
        };
        let meta = (!member_dirs.is_empty()).then(|| t!("env.dirCount", n = member_dirs.len()).to_string());
        let icon = if !member_dirs.is_empty() { IconName::Folders } else { IconName::Folder };
        let items = menus::project_menu_items(&self.ws, &project, cx);
        let new_items = env.then(|| {
            let ws = self.ws.clone();
            let id = project.id.clone();
            vec![menus::MenuItemSpec::new(t!("env.thread").to_string(), move |window, cx| {
                crate::dialogs::new_thread::open(&ws, &id, window, cx)
            })]
        });
        let edit = project.clone();
        let row = tree_row(TreeRowOpts {
            id: format!("fld-{}", project.id),
            depth: 1,
            expanded: Some(open),
            icon,
            label: project.name.clone(),
            meta,
            changes: None,
            title,
            muted: false,
            selected: false,
            on_toggle: Some(Box::new(move |cx| ws_toggle.update(cx, |w, cx| w.toggle_collapsed(&fkey, cx)))),
            on_select: Some(Box::new(move |window, cx| {
                if env {
                    crate::dialogs::project_form::open(&ws_select, Some(edit.clone()), Default::default(), window, cx)
                } else {
                    ws_select.update(cx, |w, cx| w.select_project(&pid, cx))
                }
            })),
            menu: Some(items.clone()),
            context: Some(items),
            on_new: new_items,
            trailing: None,
            drag: sortable.then(|| ProjectDrag { id: project.id.clone(), group: group.to_string(), index }),
        }, cx);

        let mut node = div().relative().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if open {
            if !env {
                let label = checkout_label(&project, head.as_ref());
                node = node.child(self.checkout_node(
                    &project,
                    label,
                    None,
                    changes.get(&project.id).cloned(),
                    selected.as_deref() == Some(project.id.as_str()),
                    2,
                    cx,
                ));
            }
            for wt in &folder.worktrees {
                node = node.child(self.checkout_node(
                    wt,
                    checkout_label(wt, None),
                    None,
                    changes.get(&wt.id).cloned(),
                    selected.as_deref() == Some(wt.id.as_str()),
                    2,
                    cx,
                ));
            }
        }
        if sortable {
            for after in [false, true] {
                node = node.child(drop_zone(&self.ws, &project.id, group, index, after, cx));
            }
        }
        node.into_any_element()
    }

    /// 不需要检出这一层的项目：多仓库项目，以及没有附属 worktree 的单仓库项目。
    /// 会话直接挂在项目行上；有附属 worktree 的单仓库项目才保留「项目 → 检出 → 会话」。
    /// 批量派生的多仓库项目与源项目同级（分组在 [`group_servers`]）。存档的仍置灰，恢复走右键。
    fn flat_folder_node(&self, folder: FolderGroup, group: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.ws.read(cx);
        let project = folder.project;
        let is_multi = project.multi.is_some();
        let archived_at = project.worktree.as_ref().and_then(|w| w.archived_at);
        let member_names: Vec<String> = project
            .multi
            .as_ref()
            .map(|m| m.repos.iter().map(|r| basename(&r.dir)).collect())
            .unwrap_or_default();
        let mine: Vec<&SessionWithProject> = ws.sessions.iter().filter(|s| s.project_id == project.id).collect();
        let mine_pending: Vec<&PendingSession> = ws.state.pending.iter().filter(|p| p.project_id == project.id).collect();
        let sessions_open = ws.state.sessions_open.get(&project.id).copied().unwrap_or(false);
        let active_child_visible = sessions_open
            && match &ws.state.active {
                ActiveView::Terminal { session_id } => mine.iter().any(|s| &s.id == session_id),
                _ => false,
            };
        let source_name = project
            .worktree
            .as_ref()
            .and_then(|wt| ws.project(&wt.source_project_id))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let title = if project.worktree.is_some() {
            let mut parts = vec![t!("worktree.derivedFrom", name = source_name).to_string()];
            if let Some(dir) = &project.working_dir {
                parts.push(dir.clone());
            }
            parts.join(" · ")
        } else if is_multi {
            let names = member_names.join(" · ");
            if names.is_empty() {
                project.name.clone()
            } else {
                names
            }
        } else {
            project.working_dir.clone().unwrap_or_else(|| project.name.clone())
        };
        let count = mine.len() + mine_pending.len();
        let expandable = archived_at.is_none() && count > 0 && project.environment_context.is_none();
        let worst = worst_state(&mine, &mine_pending);
        let blocked = mine
            .iter()
            .any(|s| s.program_status.as_ref().is_some_and(|p| p.state == falcon_proto::ProgramState::Blocked));
        let selected = archived_at.is_none()
            && !active_child_visible
            && ws.state.selected_project_id.as_deref() == Some(project.id.as_str());
        let meta = archived_at
            .map(archive_countdown)
            .or_else(|| (!member_names.is_empty()).then(|| t!("multi.repoCount", n = member_names.len()).to_string()));
        let changes = if archived_at.is_some() { None } else { ws.changes.get(&project.id).cloned() };

        let pid = project.id.clone();
        let (ws1, ws2, ws3) = (self.ws.clone(), self.ws.clone(), self.ws.clone());
        let pid_toggle = pid.clone();
        let pid_badge = pid.clone();
        let pid_select = pid.clone();
        let durable_thread = project.thread_environment_id.is_some();
        let items = menus::project_menu_items(&self.ws, &project, cx);
        let new_items = menus::new_session_items(&self.ws, &project.id, None, cx);
        let badge = expandable.then(|| {
            session_count_badge(
                format!("badge-{pid_badge}"),
                count,
                worst,
                blocked && !sessions_open,
                sessions_open,
                move |cx| ws2.update(cx, |w, cx| w.toggle_sessions(&pid_badge, cx)),
                cx,
            )
        });
        let row = tree_row(
            TreeRowOpts {
                id: format!("fld-{pid}"),
                depth: 1,
                expanded: expandable.then_some(sessions_open),
                icon: if archived_at.is_some() {
                    IconName::Archive
                } else if durable_thread {
                    IconName::MessageCircle
                } else if is_multi {
                    IconName::Folders
                } else {
                    IconName::Folder
                },
                label: project.name.clone(),
                meta,
                changes,
                title,
                muted: archived_at.is_some(),
                selected,
                on_toggle: expandable.then(|| -> Box<dyn Fn(&mut App)> {
                    Box::new(move |cx| ws1.update(cx, |w, cx| w.toggle_sessions(&pid_toggle, cx)))
                }),
                on_select: archived_at.is_none().then(|| -> Box<dyn Fn(&mut Window, &mut App)> {
                    // 线程里开着什么（对话、终端、Agent）都在它的画布上，选中就是切过去
                    Box::new(move |_, cx| ws3.update(cx, |w, cx| w.select_project(&pid_select, cx)))
                }),
                menu: Some(items.clone()),
                context: Some(items),
                on_new: archived_at.is_none().then_some(new_items),
                trailing: badge,
                drag: archived_at.is_none().then(|| ProjectDrag { id: project.id.clone(), group: group.to_string(), index }),
            },
            cx,
        );

        let mut node = div().relative().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expandable && sessions_open {
            node = node.children(self.terminal_rows(mine.into_iter(), mine_pending.into_iter(), 2, cx));
        }
        if archived_at.is_none() {
            for after in [false, true] {
                node = node.child(drop_zone(&self.ws, &project.id, group, index, after, cx));
            }
        }
        node.into_any_element()
    }

    /// 检出行，也画线程行。`depth`：兼容视图里挂在文件夹下面是 2，线程视图里线程自己占第一层是 0；
    /// 会话行比它深一层
    #[allow(clippy::too_many_arguments)]
    fn checkout_node(
        &self,
        project: &Project,
        label: String,
        meta: Option<String>,
        changes: Option<ProjectChanges>,
        selected: bool,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ws = self.ws.read(cx);
        let mine: Vec<&SessionWithProject> = ws.sessions.iter().filter(|s| s.project_id == project.id).collect();
        let mine_pending: Vec<&PendingSession> = ws.state.pending.iter().filter(|p| p.project_id == project.id).collect();
        let sessions_open = ws.state.sessions_open.get(&project.id).copied().unwrap_or(false);
        // 活动会话摊在检出下面时只强调最具体的那一行；检出收起后才由检出行承担选中态。
        let active_child_visible = sessions_open
            && match &ws.state.active {
                ActiveView::Terminal { session_id } => mine.iter().any(|s| &s.id == session_id),
                _ => false,
            };
        let source_name = project
            .worktree
            .as_ref()
            .and_then(|wt| ws.project(&wt.source_project_id))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        // 存档的附属项目：置灰、显示删除倒计时、点击不再选中（要恢复走右键菜单）
        let archived_at = project.worktree.as_ref().and_then(|w| w.archived_at);
        let archived_note = archived_at.map(archive_countdown);
        let title = if project.is_thread() {
            thread_tooltip(ws, project)
        } else if project.worktree.is_some() {
            let mut parts = vec![t!("worktree.derivedFrom", name = source_name).to_string()];
            if let Some(dir) = &project.working_dir {
                parts.push(dir.clone());
            }
            parts.join(" · ")
        } else {
            project.working_dir.clone().unwrap_or_else(|| project.name.clone())
        };
        let count = mine.len() + mine_pending.len();
        // 存档的检出开不了会话，也就没有会话行可摊
        let expandable = archived_at.is_none() && count > 0 && project.environment_context.is_none();
        let worst = worst_state(&mine, &mine_pending);
        // 收着的检出里有程序在等人：计数徽标上挂「在等你」的记号（与会话行同一个），不用摊开也看得见
        let blocked = mine
            .iter()
            .any(|s| s.program_status.as_ref().is_some_and(|p| p.state == falcon_proto::ProgramState::Blocked));

        let durable_thread = project.thread_environment_id.is_some();
        let icon_name = if archived_at.is_some() {
            IconName::Archive
        } else if durable_thread {
            IconName::MessageCircle
        } else {
            IconName::GitBranch
        };
        let pid = project.id.clone();
        let (ws1, ws2, ws3) = (self.ws.clone(), self.ws.clone(), self.ws.clone());
        let pid_toggle = pid.clone();
        let pid_badge = pid.clone();
        let pid_select = pid.clone();
        let ctx_items = menus::project_menu_items(&self.ws, project, cx);
        let new_items = menus::new_session_items(&self.ws, &project.id, None, cx);
        let badge = expandable.then(|| {
            session_count_badge(
                format!("badge-{pid_badge}"),
                count,
                worst,
                blocked && !sessions_open,
                sessions_open,
                move |cx| ws2.update(cx, |w, cx| w.toggle_sessions(&pid_badge, cx)),
                cx,
            )
        });
        let row = tree_row(TreeRowOpts {
            id: format!("co-{pid}"),
            depth,
            expanded: expandable.then_some(sessions_open),
            icon: icon_name,
            label,
            meta: archived_note.or(meta),
            changes: if archived_at.is_some() { None } else { changes },
            title,
            muted: archived_at.is_some(),
            selected: selected && !active_child_visible,
            on_toggle: expandable.then(|| -> Box<dyn Fn(&mut App)> {
                Box::new(move |cx| ws1.update(cx, |w, cx| w.toggle_sessions(&pid_toggle, cx)))
            }),
            on_select: archived_at.is_none().then(|| -> Box<dyn Fn(&mut Window, &mut App)> {
                // 线程里开着什么（对话、终端、Agent）都在它的画布上，选中就是切过去
                Box::new(move |_, cx| ws3.update(cx, |w, cx| w.select_project(&pid_select, cx)))
            }),
            menu: None,
            context: Some(ctx_items),
            on_new: archived_at.is_none().then_some(new_items),
            trailing: badge,
            drag: None,
        }, cx);

        let mut node = div().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expandable && sessions_open {
            node = node.children(self.terminal_rows(mine.into_iter(), mine_pending.into_iter(), depth + 1, cx));
        }
        node.into_any_element()
    }

    fn terminal_rows<'a>(
        &self,
        sessions: impl Iterator<Item = &'a SessionWithProject>,
        pending: impl Iterator<Item = &'a PendingSession>,
        depth: usize,
        cx: &App,
    ) -> Vec<AnyElement> {
        let ws = self.ws.read(cx);
        let active = match &ws.state.active {
            ActiveView::Terminal { session_id } => Some(session_id.as_str()),
            _ => None,
        };
        let mut rows: Vec<_> = sessions.map(|s| {
            (s.id.as_str(), self.session_row(s, depth, active == Some(s.id.as_str()), cx))
        }).chain(pending.map(|p| (p.id.as_str(), pending_row(p, depth, cx)))).collect();
        rows.sort_by_key(|(id, _)| self.terminal_positions.get(*id).copied().unwrap_or((usize::MAX, 0, 0)));
        rows.into_iter().map(|(_, row)| row).collect()
    }

    fn session_row(&self, session: &SessionWithProject, depth: usize, selected: bool, cx: &App) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let label = session_label(session, &ws.projects);
        // 菜单只走右键：行尾那一格留给状态，悬停再冒一个「…」会把时长挤得一跳一跳
        let ctx = menus::session_menu_items(&self.ws, session, cx);
        // 会话本身不在（待接回 / 已丢失）时尾巴上摆会话的记号，程序状态那时没意义
        let reading = (session.state == SessionState::Active).then(|| ws.row_status(session));
        let status = reading.map(|r| r.status);
        let ws_open = self.ws.clone();
        let sid = session.id.clone();
        let tip: SharedString = format!("{label} · {}", t!("sidebar.projectContext")).into();
        let mut row = div()
            .id(SharedString::from(format!("sess-{}", session.id)))
            .h(zpx(SESSION_ROW_H))
            .flex()
            .items_center()
            .gap_1()
            .pl(indent(depth))
            .pr(zpx(6.))
            .rounded(radius::SM)
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(move |_, _, cx| ws_open.update(cx, |w, cx| w.open_session(&sid, cx)));
        row = if selected {
            row.bg(ui.tint).text_color(ui.tint_foreground)
        } else {
            row.hover(|s| s.bg(ui.muted))
        };
        row = row
            .child(div().flex_none().size(zpx(18.)))
            .child(
                crate::session_icon::session_icon(session.agent)
                    .size(zpx(14.))
                    .text_color(if selected { ui.tint_foreground } else { ui.muted_foreground }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .pl_1()
                    // 跑完了还没看的名字加粗（像未读邮件）；很久没碰的名字淡下去
                    .when(matches!(status, Some(RowStatus::Unread { .. })), |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(status == Some(RowStatus::Inactive) && !selected, |d| d.text_color(ui.muted_foreground))
                    .child(label),
            );
        row = row.child(match reading {
            Some(reading) => status_cell(session, reading, selected, cx),
            None => status_mark(Mark::from(session.state), zpx(12.), cx),
        });
        let row = row.context_menu(move |menu, _, _| to_popup(menu, ctx.clone()));
        if selected {
            crate::ui::selected_fade(format!("sel-sess-{}", session.id), row, ui.tint).into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

/// 会话行尾巴：时长 + 状态记号，五档——转圈（在跑）/ 琥珀色的圈（在等你）/ 实心勾（跑完了还没看）/
/// 淡的勾（看过了）/ 月亮（很久没碰）；什么都没报的只写时长。只有「在等你」上色，其余跟着字色走：
/// 一列灰白里一点琥珀色，扫一眼就知道该去哪儿。程序给的那半句、进度、要的是什么都在悬停提示里。
///
/// 程序状态的摘要由服务端算好随列表来，开着窗口的会话由 WS 实时改写；时长见
/// `program_status::StateClock::row`（本客户端没亲眼看到这一轮开始的 working 不写时长）
fn status_cell(session: &SessionWithProject, reading: RowReading, selected: bool, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    // 选中的行是 tint 底：记号与时长跟着换成 tint 上的字色
    let (fg, bg, sub) = if selected {
        (ui.tint_foreground, ui.tint, ui.tint_foreground.opacity(0.7))
    } else {
        (ui.foreground, ui.background, ui.muted_foreground)
    };
    let mark = |name: IconName, size: f32, color| icon(name).size(zpx(size)).text_color(color).into_any_element();
    let glyph = match reading.status {
        RowStatus::Working => Some(div().text_color(fg).child(spinner("ps-spin", IconName::LoaderCircle, zpx(13.))).into_any_element()),
        RowStatus::NeedsYou => Some(needs_you_mark(zpx(13.), cx)),
        // 实心圆里镂一个勾：比「看过了」那个淡勾重得多，没看的一眼分得出来
        RowStatus::Unread { error: false } => Some(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(zpx(13.))
                .rounded_full()
                .bg(fg)
                .child(icon(IconName::Check).size(zpx(9.)).text_color(bg))
                .into_any_element(),
        ),
        RowStatus::Unread { error: true } => Some(mark(IconName::TriangleAlert, 13., ui.destructive)),
        RowStatus::Read { error } => Some(mark(if error { IconName::TriangleAlert } else { IconName::Check }, 12., sub)),
        RowStatus::Inactive => Some(mark(IconName::Moon, 12., sub)),
        RowStatus::Idle => None,
    };
    let tip = status_tip(session, reading);
    div()
        .id(SharedString::from(format!("sess-ps-{}", session.id)))
        .flex()
        .flex_none()
        .items_center()
        .gap(zpx(6.))
        .text_size(zpx(11.))
        .text_color(sub)
        .when_some(reading.age, |d, ms| d.child(age_text(ms)))
        // 记号占一个定宽的格子：有没有记号、记号多大，时长都对得齐
        .child(div().flex().flex_none().items_center().justify_center().size(zpx(14.)).children(glyph))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .into_any_element()
}

/// 会话行状态那一格的悬停提示：短标签（· 进度）· 程序那半句（已消毒、截断过）
fn status_tip(session: &SessionWithProject, reading: RowReading) -> SharedString {
    use falcon_core::program_status::badge;
    let age = reading.age.map(age_text).unwrap_or_default();
    let finished = |error: bool| t!(if error { "programStatus.error" } else { "programStatus.done" }).to_string();
    match reading.status {
        RowStatus::Working | RowStatus::NeedsYou | RowStatus::Unread { .. } => {
            let Some(b) = session.program_status.as_ref().and_then(badge) else { return SharedString::default() };
            let mut parts = vec![t!(b.label_key).to_string()];
            parts.extend(b.progress.map(|n| t!("programStatus.progress", n = n).to_string()));
            parts.extend(b.detail);
            parts.join(" · ")
        }
        RowStatus::Read { error } => format!("{} · {}", finished(error), t!("sidebar.statusSeen")),
        RowStatus::Idle => t!("sidebar.statusIdle", t = age).to_string(),
        RowStatus::Inactive => t!("sidebar.statusInactive", t = age).to_string(),
    }
    .into()
}

/// 线程行的左内边距与行首状态格的宽度。会话行摊在线程下面时深度取 0：它的图标列
/// （`indent(0)` + 18px 占位 + 行内间距）正好落在线程名称那一列
const THREAD_PAD_L: f32 = 4.;
const GLYPH_W: f32 = 18.;

/// 线程行首的状态记号，按要紧程度从低到高排（取最大的那个）。只有「在等你」与「出错」上色，
/// 其余跟着字色走，与会话行尾巴上的记号同一套口径（[`status_cell`]）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ThreadGlyph {
    /// 什么都没开着
    None,
    /// 有活着的会话或对话，但都闲着：一颗淡的小圆点
    Live,
    /// 有程序跑完了还没看：实心的强调色圆点（像未读）
    Unread,
    /// 在跑：转圈
    Working,
    /// 会话待接回
    Unverified,
    /// 会话丢了、创建失败或程序报错
    Error,
    /// 有程序卡着等人（OSC 7501 blocked、Claude 对话等审批）
    NeedsYou,
}

fn glyph_cell(glyph: ThreadGlyph, id: &str, selected: bool, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let fg = if selected { ui.tint_foreground } else { ui.foreground };
    let dot = |size: f32, color| div().size(zpx(size)).rounded_full().bg(color).into_any_element();
    let inner = match glyph {
        ThreadGlyph::None => None,
        ThreadGlyph::Live => Some(dot(5., if selected { ui.tint_foreground.opacity(0.5) } else { ui.muted_foreground.opacity(0.6) })),
        ThreadGlyph::Unread => Some(dot(7., if selected { ui.tint_foreground } else { ui.tint_strong })),
        ThreadGlyph::Working => Some(
            div().text_color(fg).child(spinner(format!("th-spin-{id}"), IconName::LoaderCircle, zpx(12.))).into_any_element(),
        ),
        ThreadGlyph::Unverified => Some(status_mark(Mark::Unverified, zpx(12.), cx)),
        ThreadGlyph::Error => Some(icon(IconName::TriangleAlert).size(zpx(12.)).text_color(ui.destructive).into_any_element()),
        ThreadGlyph::NeedsYou => Some(needs_you_mark(zpx(12.), cx)),
    };
    div()
        .flex_none()
        .w(zpx(GLYPH_W))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .children(inner)
        .into_any_element()
}

/// 线程列表的段标题（今天 / 昨天 / …，状态优先时是四档状态）
fn section_header(section: ThreadSection, first: bool, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let key = match section {
        ThreadSection::Today => "sidebar.sectionToday",
        ThreadSection::Yesterday => "sidebar.sectionYesterday",
        ThreadSection::Last7Days => "sidebar.sectionLast7Days",
        ThreadSection::Last30Days => "sidebar.sectionLast30Days",
        ThreadSection::Older => "sidebar.sectionOlder",
        ThreadSection::NeedsAttention => "sidebar.filterStatusNeedsAttention",
        ThreadSection::Working => "sidebar.filterStatusWorking",
        ThreadSection::Active => "sidebar.filterStatusActive",
        ThreadSection::Idle => "sidebar.sectionIdle",
    };
    div()
        .flex_none()
        .pt(zpx(if first { 4. } else { 10. }))
        .pb(zpx(2.))
        .pl(zpx(THREAD_PAD_L + 6.))
        .text_size(zpx(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ui.muted_foreground)
        .child(t!(key).to_string())
        .into_any_element()
}

/// 线程所在机器的名字：本机写「本机」，远端优先用已保存主机的名字
fn thread_machine(ws: &Workspace, thread: &Project) -> String {
    match thread.project_type {
        ProjectType::Local => local_word(),
        ProjectType::Ssh => thread
            .host_id
            .as_ref()
            .and_then(|id| ws.hosts.iter().find(|h| &h.id == id))
            .map(|h| h.name.clone())
            .or_else(|| thread.ssh.as_ref().map(|s| s.host.clone()))
            .unwrap_or_default(),
    }
}

/// 线程行的悬停提示：「机器 / 环境 · 工作目录」（多目录线程列出各成员目录）
fn thread_tooltip(ws: &Workspace, thread: &Project) -> String {
    let machine = thread_machine(ws, thread);
    let env = thread_environment(thread).and_then(|id| ws.project(id)).map(|p| p.name.clone());
    let mut parts = vec![match env {
        Some(env) => format!("{machine} / {env}"),
        None => machine,
    }];
    if let Some(dir) = &thread.working_dir {
        parts.push(dir.clone());
    } else if let Some(multi) = &thread.multi {
        parts.extend(multi.repos.iter().map(|r| r.dir.clone()));
    }
    parts.join(" · ")
}

fn now_ms() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn pending_row(p: &PendingSession, depth: usize, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    div()
        .h(zpx(SESSION_ROW_H))
        .flex()
        .items_center()
        .gap(zpx(6.))
        .pl(indent(depth))
        .pr(zpx(6.))
        .text_color(ui.muted_foreground)
        .child(div().flex_none().size(zpx(18.)))
        .child(status_mark(if p.error.is_some() { Mark::Dead } else { Mark::Creating }, zpx(12.), cx))
        .child(div().flex_1().min_w_0().truncate().pl_1().child(t!("tab.creating").to_string()))
        .into_any_element()
}

/// 侧栏唯一的新建入口。画成与线程行同一套行高、缩进与图标列，＋ 正好对齐下面各行的图标；
/// 机器、环境与第一个动作在新建线程的输入框里选（[`crate::dialogs::new_thread::open`]）
fn new_thread_row(ws: &Entity<Workspace>, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let ws = ws.clone();
    div()
        .id("sidebar-new-thread")
        .h(zpx(ROW_H))
        .flex()
        .items_center()
        .gap_1()
        .pl(indent(0))
        .pr(zpx(6.))
        .rounded(radius::SM)
        .cursor_pointer()
        .hover(|s| s.bg(ui.muted))
        .child(div().flex_none().size(zpx(18.)))
        .child(icon(IconName::Plus).size(zpx(14.)).text_color(ui.foreground))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .pl_1()
                .font_weight(FontWeight::MEDIUM)
                .child(t!("env.thread").to_string()),
        )
        .tooltip(move |window, cx| {
            Tooltip::new(format!("{} · {}", t!("env.thread"), crate::ui::chord("newThread"))).build(window, cx)
        })
        .on_click(move |_, window, cx| crate::dialogs::new_thread::open(&ws, "", window, cx))
        .into_any_element()
}

/// 存档附属项目行上的删除倒计时。0 在调用方已经被当成「没存档」。
fn archive_countdown(archived_at: i64) -> String {
    let deadline = archived_at + falcon_proto::WORKTREE_ARCHIVE_TTL_MS;
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let days_left = ((deadline - now) as f64 / 86_400_000.0).ceil() as i64;
    if days_left >= 1 {
        t!("worktree.archivedMetaDays", n = days_left).to_string()
    } else {
        t!("worktree.archivedMetaSoon").to_string()
    }
}

fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

fn chevron(id: String, expanded: bool, cx: &App, on_toggle: impl Fn(&mut App) + 'static) -> AnyElement {
    let ui = Ui::global(cx);
    let chev_id = format!("{id}-chev");
    div()
        .id(SharedString::from(id))
        .flex_none()
        .size(zpx(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(4.))
        .text_color(ui.muted_foreground)
        .hover(|s| s.text_color(ui.foreground))
        .active(|s| s.opacity(0.7))
        .child(crate::ui::disclosure_chevron(SharedString::from(chev_id), expanded, zpx(12.)))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            on_toggle(cx)
        })
        .into_any_element()
}

/// 检出行尾巴上的会话计数。点它 = 摊开 / 收起会话行（行首箭头只有 12px 见方，这里给个够大的落点）
fn session_count_badge(
    id: String,
    count: usize,
    worst: Option<Mark>,
    blocked: bool,
    expanded: bool,
    on_toggle: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let mut label = t!("sidebar.sessionCount", n = count).to_string();
    if blocked {
        label = format!("{label} · {}", t!("programStatus.someoneBlocked"));
    }
    let label: SharedString = label.into();
    let mark = match worst {
        Some(m) => status_mark(m, zpx(12.), cx),
        None => icon(IconName::SquareTerminal).size(zpx(12.)).into_any_element(),
    };
    div()
        .id(SharedString::from(id))
        .flex_none()
        .h(zpx(22.))
        .flex()
        .items_center()
        .gap(zpx(3.))
        .px(zpx(4.))
        .rounded(zpx(6.))
        .text_size(zpx(11.))
        .text_color(if expanded { ui.foreground } else { ui.muted_foreground })
        .hover(|s| s.text_color(ui.foreground))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when(blocked, |d| d.child(needs_you_mark(zpx(12.), cx)))
        .child(mark)
        .child(count.to_string())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            on_toggle(cx)
        })
        .into_any_element()
}

fn change_badge(changes: &ProjectChanges, cx: &App) -> Option<AnyElement> {
    if changes.added == 0 && changes.deleted == 0 {
        return None;
    }
    let ui = Ui::global(cx);
    let mut badge = div().flex_none().flex().gap_1().text_size(zpx(11.));
    if changes.added > 0 {
        badge = badge.child(div().text_color(ui.success).child(format!("+{}", changes.added)));
    }
    if changes.deleted > 0 {
        badge = badge.child(div().text_color(ui.destructive).child(format!("-{}", changes.deleted)));
    }
    Some(badge.into_any_element())
}

struct TreeRowOpts {
    id: String,
    depth: usize,
    expanded: Option<bool>,
    icon: IconName,
    label: String,
    meta: Option<String>,
    changes: Option<ProjectChanges>,
    title: String,
    muted: bool,
    selected: bool,
    on_toggle: Option<Box<dyn Fn(&mut App)>>,
    on_select: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    menu: Option<Vec<menus::MenuItemSpec>>,
    context: Option<Vec<menus::MenuItemSpec>>,
    /// 行尾的 ＋：新建会话的菜单（普通终端 / 各家 CLI）
    on_new: Option<Vec<menus::MenuItemSpec>>,
    /// 摆在 +N −M 左边的附加徽标（检出行的会话计数）
    trailing: Option<AnyElement>,
    /// 源项目行：可拖着排序（落点是 [`drop_zone`]，盖在整个文件夹节点上）
    drag: Option<ProjectDrag>,
}

fn tree_row(o: TreeRowOpts, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let group: SharedString = format!("row-{}", o.id).into();
    let title: SharedString = if o.context.is_some() {
        format!("{} · {}", o.title, t!("sidebar.projectContext")).into()
    } else {
        o.title.clone().into()
    };
    let mut row = div()
        .id(SharedString::from(o.id.clone()))
        .group(group.clone())
        .h(zpx(ROW_H))
        .flex()
        .items_center()
        .gap_1()
        .pl(indent(o.depth))
        .pr(zpx(6.))
        .rounded(radius::SM)
        .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx));
    row = if o.selected {
        row.bg(ui.tint).text_color(ui.tint_foreground)
    } else {
        row.hover(|s| s.bg(ui.muted))
    };
    if let Some(select) = o.on_select {
        row = row.cursor_pointer().on_click(move |_, window, cx| select(window, cx));
    }
    if let Some(drag) = o.drag {
        // 按下挪过几像素才算拖，点一下照旧是选中
        let label: SharedString = o.label.clone().into();
        row = row.on_drag(drag, move |_, _, _, cx| DragLabel::new(label.clone(), cx));
    }
    row = match (o.expanded, o.on_toggle) {
        (Some(expanded), Some(toggle)) => row.child(chevron(format!("{}-toggle", o.id), expanded, cx, toggle)),
        _ => row.child(div().flex_none().size(zpx(18.))),
    };
    row = row
        .child(
            icon(o.icon)
                .size(zpx(14.))
                .text_color(if o.selected { ui.tint_foreground } else { ui.muted_foreground }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .pl_1()
                .font_weight(FontWeight::MEDIUM)
                .when(o.muted, |d| d.text_color(ui.muted_foreground))
                .child(o.label),
        );
    if let Some(meta) = o.meta {
        row = row.child(
            div()
                .max_w(zpx(96.))
                .truncate()
                .text_size(zpx(11.))
                .text_color(ui.muted_foreground)
                .child(meta),
        );
    }
    if let Some(items) = o.on_new {
        let label = if items.len() == 1 { items[0].label.clone() } else {
            format!("{} · {}", t!("sidebar.newTerminal"), crate::ui::chord("newTerminal"))
        };
        row = row.child(
            div().opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)).child(
                Button::new(SharedString::from(format!("{}-new", o.id)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(label)
                    .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
            ),
        );
    }
    if let Some(trailing) = o.trailing {
        row = row.child(trailing);
    }
    if let Some(changes) = o.changes.as_ref().and_then(|c| change_badge(c, cx)) {
        row = row.child(changes);
    }
    if let Some(items) = o.menu {
        row = row.child(
            Button::new(SharedString::from(format!("{}-menu", o.id)))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
        );
    }
    match o.context {
        Some(items) => {
            let row = row.context_menu(move |menu, _, _| to_popup(menu, items.clone()));
            if o.selected {
                crate::ui::selected_fade(format!("sel-{}", o.id), row, ui.tint).into_any_element()
            } else {
                row.into_any_element()
            }
        }
        None => {
            if o.selected {
                crate::ui::selected_fade(format!("sel-{}", o.id), row, ui.tint).into_any_element()
            } else {
                row.into_any_element()
            }
        }
    }
}

/// 拖项目排序的落点：盖在整个文件夹节点（连同摊开的检出 / 会话行）上的上下两半，上半 = 放到它
/// 前面、下半 = 放到它后面，压着的那半在自己那条边上画一道线。按节点分而不是按行分：摊开的文件夹，
/// "放到它后面"的线得画在它最后一行下面，画在文件夹行下面像是要塞进它里面。
///
/// 两半各往外探出一道行缝，线就正好落在缝里、不压着相邻行的高亮；相邻两个节点探进同一道缝的两半
/// 语义相同（A 的后面 = B 的前面），同时亮也是同一个位置。落点层没有点击监听、hitbox 不是 opaque
/// 的，底下的行照常点击、悬停（落下只认 [`ProjectDrag`] 这个类型）
fn drop_zone(ws: &Entity<Workspace>, target: &str, group: &str, index: usize, after: bool, cx: &App) -> AnyElement {
    let line = Ui::global(cx).tint_strong;
    let (ws, target, group) = (ws.clone(), target.to_string(), group.to_string());
    // 拖着别的组的项目、或放下去等于没动：不画线也不接。can_drop 为假时 GPUI 连 drag_over 的样式
    // 都不套
    let zone = div()
        .absolute()
        .left_0()
        .right_0()
        .can_drop(move |dragged, _, _| {
            dragged
                .downcast_ref::<ProjectDrag>()
                .is_some_and(|d| d.group == group && !lands_in_place(d.index, index, after))
        })
        .on_drop(move |d: &ProjectDrag, _, cx| ws.update(cx, |w, cx| w.move_project(&d.id, &target, after, cx)));
    let zone = if after {
        zone.top(relative(0.5))
            .bottom(-zpx(ROW_GAP))
            .drag_over::<ProjectDrag>(move |s, _, _, _| s.border_b(zpx(ROW_GAP)).border_color(line))
    } else {
        zone.top(-zpx(ROW_GAP))
            .bottom(relative(0.5))
            .drag_over::<ProjectDrag>(move |s, _, _, _| s.border_t(zpx(ROW_GAP)).border_color(line))
    };
    zone.into_any_element()
}

// ---------------- 线程视图：过滤与排序辅助 ----------------

fn filter_chip(
    id: &'static str,
    label: String,
    on_remove: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let ui = Ui::global(cx);
    div()
        .id(id)
        .h(zpx(20.))
        .max_w(zpx(132.))
        .px(zpx(6.))
        .flex()
        .items_center()
        .gap(zpx(4.))
        .rounded(radius::SM)
        .bg(ui.tint)
        .text_color(ui.tint_foreground)
        .text_size(zpx(11.))
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .child(div().min_w_0().truncate().child(label))
        .child(icon(IconName::X).size(zpx(10.)).flex_none())
        .on_click(on_remove)
        .into_any_element()
}

fn status_filter_label(status: ThreadStatusFilter) -> String {
    match status {
        ThreadStatusFilter::All => t!("sidebar.filterStatusAll"),
        ThreadStatusFilter::Active => t!("sidebar.filterStatusActive"),
        ThreadStatusFilter::Working => t!("sidebar.filterStatusWorking"),
        ThreadStatusFilter::NeedsAttention => t!("sidebar.filterStatusNeedsAttention"),
        ThreadStatusFilter::HasChanges => t!("sidebar.filterStatusHasChanges"),
    }
    .to_string()
}

fn status_filter_icon(status: ThreadStatusFilter) -> Option<IconName> {
    match status {
        ThreadStatusFilter::All => None,
        ThreadStatusFilter::Active => Some(IconName::CircleDot),
        ThreadStatusFilter::Working => Some(IconName::LoaderCircle),
        ThreadStatusFilter::NeedsAttention => Some(IconName::CircleAlert),
        ThreadStatusFilter::HasChanges => Some(IconName::FileDiff),
    }
}

fn machine_filter_label(machine: &ThreadMachineFilter, ws: &Workspace) -> String {
    match machine {
        ThreadMachineFilter::All => t!("sidebar.filterMachineAll").to_string(),
        ThreadMachineFilter::Local => local_word(),
        ThreadMachineFilter::Host(id) => ws
            .hosts
            .iter()
            .find(|h| &h.id == id)
            .map(|h| h.name.clone())
            .unwrap_or_else(|| id.clone()),
    }
}

fn env_filter_label(env_id: &str, ws: &Workspace) -> String {
    ws.project(env_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| t!("sidebar.filterEnvUnknown").to_string())
}

fn sort_label(sort: ThreadSort) -> String {
    match sort {
        ThreadSort::CreatedDesc => t!("sidebar.sortCreatedDesc"),
        ThreadSort::LastActiveDesc => t!("sidebar.sortLastActiveDesc"),
        ThreadSort::StatusPriority => t!("sidebar.sortStatusPriority"),
        ThreadSort::NameAsc => t!("sidebar.sortNameAsc"),
        ThreadSort::NameDesc => t!("sidebar.sortNameDesc"),
        ThreadSort::CreatedAsc => t!("sidebar.sortCreatedAsc"),
    }
    .to_string()
}

fn sort_icon(sort: ThreadSort) -> IconName {
    match sort {
        ThreadSort::CreatedDesc => IconName::ArrowDownWideNarrow,
        ThreadSort::LastActiveDesc => IconName::Activity,
        ThreadSort::StatusPriority => IconName::CircleAlert,
        ThreadSort::NameAsc => IconName::ArrowDownAZ,
        ThreadSort::NameDesc => IconName::ArrowDownZA,
        ThreadSort::CreatedAsc => IconName::ArrowUpNarrowWide,
    }
}

fn build_sort_menu(menu: PopupMenu, ws: &Entity<Workspace>, cx: &App) -> PopupMenu {
    let current = ws.read(cx).state.thread_sort;
    let items: Vec<MenuItemSpec> = ThreadSort::ALL
        .iter()
        .copied()
        .map(|sort| {
            let ws = ws.clone();
            let item = MenuItemSpec::new(sort_label(sort), move |_, cx| {
                ws.update(cx, |w, cx| w.set_thread_sort(sort, cx));
            })
            .icon(icon(sort_icon(sort)))
            .checked(sort == current);
            // 最常用的三种排序（最近创建 / 最近活跃 / 状态优先）与名称、正序之间画分隔线
            if sort == ThreadSort::NameAsc { item.sep() } else { item }
        })
        .collect();
    to_popup(menu.label(t!("sidebar.sortLabel").to_string()), items)
}

fn build_filter_menu(
    mut menu: PopupMenu,
    sidebar: &WeakEntity<Sidebar>,
    ws: &Entity<Workspace>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(sb) = sidebar.upgrade() else {
        return menu;
    };
    let current = sb.read(cx).effective_filter(cx);

    // 1. 状态过滤
    let status_items: Vec<MenuItemSpec> = ThreadStatusFilter::ALL
        .iter()
        .copied()
        .map(|status| {
            let sidebar = sidebar.clone();
            let mut item = MenuItemSpec::new(status_filter_label(status), move |_, cx| {
                sidebar.update(cx, |this, cx| this.set_status_filter(status, cx)).ok();
            })
            .checked(current.status == status);
            if let Some(glyph) = status_filter_icon(status) {
                item = item.icon(icon(glyph));
            }
            item
        })
        .collect();
    menu = to_popup(menu.label(t!("sidebar.filterStatusLabel").to_string()), status_items);

    // 2. 机器过滤（存在远端主机或已选中机器条件时展示）
    let (hosts, envs, archived_count, show_archived) = {
        let w = ws.read(cx);
        let hosts = w.hosts.clone();
        let envs: Vec<(String, String, Option<String>)> = w
            .projects
            .iter()
            .filter(|p| p.is_environment() && project_matches_machine(p, &current.machine))
            .map(|p| {
                let host_name = match p.project_type {
                    ProjectType::Local => (!w.hosts.is_empty()).then(local_word),
                    ProjectType::Ssh => p
                        .host_id
                        .as_ref()
                        .and_then(|id| w.hosts.iter().find(|h| &h.id == id))
                        .map(|h| h.name.clone()),
                };
                (p.id.clone(), p.name.clone(), host_name)
            })
            .collect();
        (hosts, envs, archived_thread_count(&w.projects), w.state.show_archived)
    };

    if !hosts.is_empty() || current.machine != ThreadMachineFilter::All {
        let mut machine_items = Vec::with_capacity(hosts.len() + 2);
        {
            let sidebar = sidebar.clone();
            machine_items.push(
                MenuItemSpec::new(t!("sidebar.filterMachineAll").to_string(), move |_, cx| {
                    sidebar.update(cx, |this, cx| this.set_machine_filter(ThreadMachineFilter::All, cx)).ok();
                })
                .checked(current.machine == ThreadMachineFilter::All),
            );
        }
        {
            let sidebar = sidebar.clone();
            machine_items.push(
                MenuItemSpec::new(local_word(), move |_, cx| {
                    sidebar.update(cx, |this, cx| this.set_machine_filter(ThreadMachineFilter::Local, cx)).ok();
                })
                .icon(icon(IconName::Monitor))
                .checked(current.machine == ThreadMachineFilter::Local),
            );
        }
        for host in &hosts {
            let sidebar = sidebar.clone();
            let id = host.id.clone();
            let checked = matches!(&current.machine, ThreadMachineFilter::Host(cur) if cur == &id);
            machine_items.push(
                MenuItemSpec::new(host.name.clone(), move |_, cx| {
                    sidebar
                        .update(cx, |this, cx| this.set_machine_filter(ThreadMachineFilter::Host(id.clone()), cx))
                        .ok();
                })
                .icon(icon(IconName::Server))
                .checked(checked),
            );
        }

        menu = menu.separator();
        if machine_items.len() <= FILTER_INLINE_MACHINES {
            menu = to_popup(menu.label(t!("sidebar.filterMachineLabel").to_string()), machine_items);
        } else {
            let label = t!("sidebar.filterMachineSubmenu", name = machine_filter_label(&current.machine, ws.read(cx))).to_string();
            menu = menu.submenu_with_icon(Some(icon(IconName::Server)), label, window, cx, move |sub, _, _| {
                to_popup(sub, machine_items.clone())
            });
        }
    }

    // 3. 环境过滤（按当前选中的机器收敛候选环境）
    if !envs.is_empty() || current.environment_id.is_some() {
        let show_machine_suffix = current.machine == ThreadMachineFilter::All && !hosts.is_empty();
        let mut env_items = Vec::with_capacity(envs.len() + 1);
        {
            let sidebar = sidebar.clone();
            env_items.push(
                MenuItemSpec::new(t!("sidebar.filterEnvAll").to_string(), move |_, cx| {
                    sidebar.update(cx, |this, cx| this.set_env_filter(None, cx)).ok();
                })
                .checked(current.environment_id.is_none()),
            );
        }
        for (id, name, host_name) in envs {
            let label = match (show_machine_suffix, host_name) {
                (true, Some(host)) => format!("{name} · {host}"),
                _ => name,
            };
            let sidebar = sidebar.clone();
            let checked = current.environment_id.as_deref() == Some(id.as_str());
            env_items.push(
                MenuItemSpec::new(label, move |_, cx| {
                    sidebar.update(cx, |this, cx| this.set_env_filter(Some(id.clone()), cx)).ok();
                })
                .icon(icon(IconName::Folder))
                .checked(checked),
            );
        }

        menu = menu.separator();
        if env_items.len() <= FILTER_INLINE_ENVS {
            menu = to_popup(menu.label(t!("sidebar.filterEnvLabel").to_string()), env_items);
        } else {
            let cur_name = current
                .environment_id
                .as_deref()
                .map(|id| env_filter_label(id, ws.read(cx)))
                .unwrap_or_else(|| t!("sidebar.filterEnvAll").to_string());
            let label = t!("sidebar.filterEnvSubmenu", name = cur_name).to_string();
            menu = menu.submenu_with_icon(Some(icon(IconName::Folder)), label, window, cx, move |sub, _, _| {
                to_popup(sub, env_items.clone())
            });
        }
    }

    // 4. 已存档的旧工作树线程开关
    if archived_count > 0 || show_archived {
        let ws = ws.clone();
        menu = menu.separator();
        menu = to_popup(
            menu,
            vec![
                MenuItemSpec::new(t!("worktree.showArchived", n = archived_count).to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.toggle_show_archived(cx));
                })
                .icon(icon(IconName::Archive))
                .checked(show_archived),
            ],
        );
    }

    // 5. 一键清除过滤
    if current.is_active() {
        let sidebar = sidebar.clone();
        menu = menu.separator();
        menu = to_popup(
            menu,
            vec![
                MenuItemSpec::new(t!("sidebar.filterClear").to_string(), move |window, cx| {
                    sidebar.update(cx, |this, cx| this.clear_filters(window, cx)).ok();
                })
                .icon(icon(IconName::RotateCcw)),
            ],
        );
    }

    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_filter_and_sort_i18n_keys_exist() {
        let locale: serde_json::Value = serde_json::from_str(include_str!("../locales/zh-CN.json")).unwrap();
        let has = |key: &str| {
            key.split('.')
                .try_fold(&locale, |v, part| v.get(part))
                .is_some_and(serde_json::Value::is_string)
        };
        for key in [
            "sidebar.filterPlaceholder",
            "sidebar.filterTooltip",
            "sidebar.filterActiveTooltip",
            "sidebar.filterStatusLabel",
            "sidebar.filterStatusAll",
            "sidebar.filterStatusActive",
            "sidebar.filterStatusWorking",
            "sidebar.filterStatusNeedsAttention",
            "sidebar.filterStatusHasChanges",
            "sidebar.filterMachineLabel",
            "sidebar.filterMachineAll",
            "sidebar.filterMachineSubmenu",
            "sidebar.filterEnvLabel",
            "sidebar.filterEnvAll",
            "sidebar.filterEnvSubmenu",
            "sidebar.filterEnvUnknown",
            "sidebar.filterClear",
            "sidebar.filterEmpty",
            "sidebar.filterCount",
            "sidebar.sortLabel",
            "sidebar.sortTooltip",
            "sidebar.sortCreatedDesc",
            "sidebar.sortCreatedAsc",
            "sidebar.sortLastActiveDesc",
            "sidebar.sortStatusPriority",
            "sidebar.sortNameAsc",
            "sidebar.sortNameDesc",
            "sidebar.sectionToday",
            "sidebar.sectionYesterday",
            "sidebar.sectionLast7Days",
            "sidebar.sectionLast30Days",
            "sidebar.sectionOlder",
            "sidebar.sectionIdle",
        ] {
            assert!(has(key), "缺文案 key：{key}");
        }
        for &status in ThreadStatusFilter::ALL {
            assert!(!status_filter_label(status).is_empty());
        }
        for &sort in ThreadSort::ALL {
            assert!(!sort_label(sort).is_empty());
        }
    }
}
