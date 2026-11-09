//! 侧栏：服务器 → 文件夹 → 检出 → 会话（旧 React 版的 `components/Sidebar.tsx`）。
//!
//! 多仓库项目不再往下挂检出：项目行自己承接会话，批量派生出来的项目与源项目同级。
//! 单仓库的附属项目仍缩进在源项目的检出下面。
//!
//! - 顶上不设标题栏：树本身就是内容，新建项目走各服务器行 hover 的 ＋ / 右键 / 命令面板；
//! - 行的 hover / 选中是内缩的圆角块（左右留内边距，贴着岛边会切掉圆角）；
//! - 会话行默认收起：一个检出能挂五六个终端，全摊开就占满侧栏；检出行尾巴上摆计数徽标 +
//!   最重的异常状态，折叠着也看得出出没出事——包括有程序卡在 blocked 等你（OSC 7501，ADR 0020）；
//! - 文件夹行可以拖着在同一台机器的组里排序，顺序存在服务端，各个客户端看到的一样。
//!   单仓库附属项目跟着源项目走，不单独拖；多仓库派生自己占一行，可以拖；
//! - 本机分组一直留着。每台机器的「无项目」行不占空位：已有独立会话、正在创建，或当前选中它时才画。
//!   没画出来时，在这台机器上开终端走机器行的菜单。

use falcon_core::program_status::{RowReading, RowStatus};
use falcon_core::project_tree::{
    FolderGroup, ServerGroup, ServerKind, checkout_label, folder_key, group_servers, lands_in_place,
};
use falcon_proto::{Project, SessionState, SessionWithProject};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::{Sizable, tooltip::Tooltip};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, Pixels, Render, SharedString, Window, div, relative,
};
use rust_i18n::t;

use crate::labels::{age_text, local_word, session_label};
use crate::menus::{self, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::{DragLabel, Mark, host_bar, icon, needs_you_mark, spinner, status_mark};
use crate::workspace::{ActiveView, PendingSession, ProjectChanges, Workspace};
use crate::zoom::zpx;

const ROW_H: f32 = 30.;
const SESSION_ROW_H: f32 = 28.;
/// 树里相邻两行之间的缝。选中的项目（检出行）与当前会话各自高亮、又常常上下挨着（会话行就摊在它的
/// 检出下面），没有缝两块圆角底色会粘成一条；每一层的 flex_col 都要带上，跨层的相邻行才同样有缝
const ROW_GAP: f32 = 2.;

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
    ws: Entity<Workspace>,
}

impl Sidebar {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
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
        Self { ws }
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
        let ws = self.ws.read(cx);
        // 本机分组一律画出来。空的「无项目」不跟着画，见 [`Sidebar::server_node`]。
        let servers: Vec<ServerGroup> = group_servers(&ws.projects, &ws.hosts, &local_word(), ws.state.show_archived);
        let empty = ws.projects.is_empty() && ws.hosts.is_empty();

        let mut tree = div()
            .id("sidebar-tree")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(zpx(8.))
            .flex()
            .flex_col();
        if empty {
            tree = tree.child(
                div()
                    .px_2()
                    .pt(zpx(6.))
                    .pb(zpx(10.))
                    .text_xs()
                    .text_color(ui.muted_foreground)
                    .child(t!("sidebar.empty").to_string()),
            );
        }
        for server in servers {
            tree = tree.child(self.server_node(server, cx));
        }

        let ws_handle = self.ws.clone();
        let ws_handle2 = self.ws.clone();
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
            .child(tree)
            .child(bottom)
    }
}

impl Sidebar {
    fn server_node(&self, server: ServerGroup, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let expanded = !ws.state.collapsed.get(&server.key).copied().unwrap_or(false);
        let key = server.key.clone();
        let ws_toggle = self.ws.clone();
        let icon_name = if server.kind == ServerKind::Local { IconName::Monitor } else { IconName::Server };

        // 空的「无项目」不占一行。有会话、正在创建，或当前选中它（主区还停在这上面）才画。
        // 没画出来时，开终端的入口接到机器行菜单前面。
        let standalone_id = server.standalone_id.clone();
        let show_standalone = standalone_id.as_deref().is_some_and(|id| {
            ws.sessions.iter().any(|s| s.project_id == id)
                || ws.state.pending.iter().any(|p| p.project_id == id)
                || ws.state.selected_project_id.as_deref() == Some(id)
        });
        let mut menu_items = match (&server.kind, &server.host) {
            (ServerKind::Host, Some(host)) => Some(menus::host_menu_items(&self.ws, host)),
            (ServerKind::Local, _) => Some(menus::server_menu_items(&self.ws, true)),
            _ => None,
        };
        if let Some(id) = standalone_id.as_deref().filter(|_| !show_standalone)
            && let Some(items) = menu_items.take()
        {
            menu_items = Some(menus::with_machine_terminal(&self.ws, id, items, cx));
        }
        let ctx_items = menu_items
            .clone()
            .unwrap_or_else(|| menus::server_menu_items(&self.ws, false));

        let ws_new = self.ws.clone();
        let preset = match (&server.kind, &server.host) {
            (ServerKind::Host, Some(h)) => crate::dialogs::project_form::Preset::ssh(Some(h.id.clone()), false),
            (ServerKind::Local, _) => crate::dialogs::project_form::Preset::local(false),
            _ => crate::dialogs::project_form::Preset::ssh(None, false),
        };
        let title: SharedString = server.conn.clone().unwrap_or_else(|| server.name.clone()).into();

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
            .child(chevron(format!("srv-toggle-{key}"), expanded, cx, move |cx| {
                ws_toggle.update(cx, |w, cx| w.toggle_collapsed(&key, cx))
            }))
            .child(host_bar(server.bar, zpx(16.), cx))
            .child(icon(icon_name).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().pl_1().font_weight(FontWeight::MEDIUM).child(server.name.clone()))
            .child(
                div().opacity(0.).group_hover("srv-row", |s| s.opacity(1.)).child(
                    Button::new(SharedString::from(format!("srv-new-{}", server.key)))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip(t!("sidebar.newProject").to_string())
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
            if show_standalone && let Some(id) = &standalone_id {
                node = node.child(self.standalone_node(id, cx));
            }
            if server.folders.is_empty() {
                if !show_standalone {
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

    /// 无项目的独立会话分组行：不绑目录，点 ＋ 新建终端。调用方只在有内容或正选中它时画。
    fn standalone_node(&self, standalone_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.ws.read(cx);
        let selected = ws.state.selected_project_id.as_deref() == Some(standalone_id);
        let mine: Vec<&SessionWithProject> = ws.sessions.iter().filter(|s| s.project_id == standalone_id).collect();
        let mine_pending: Vec<&PendingSession> = ws.state.pending.iter().filter(|p| p.project_id == standalone_id).collect();
        let sessions_open = ws.state.sessions_open.get(standalone_id).copied().unwrap_or(false);
        let count = mine.len() + mine_pending.len();
        let expandable = count > 0;
        let worst = worst_state(&mine, &mine_pending);
        let blocked = mine
            .iter()
            .any(|s| s.program_status.as_ref().is_some_and(|p| p.state == falcon_proto::ProgramState::Blocked));

        let pid = standalone_id.to_string();
        let (ws1, ws2, ws3, ws4) = (self.ws.clone(), self.ws.clone(), self.ws.clone(), self.ws.clone());
        let pid_toggle = pid.clone();
        let pid_badge = pid.clone();
        let pid_select = pid.clone();
        let pid_filter = pid.clone();
        let new_items = menus::new_session_items(&self.ws, standalone_id, None, cx);
        let mut ctx_items = new_items.clone();
        ctx_items.push(
            menus::MenuItemSpec::new(t!("project.filterInOverview").to_string(), move |_, cx| {
                ws4.update(cx, |w, cx| {
                    w.overview_project = Some(pid_filter.clone());
                    w.selected.clear();
                    w.show_overview(cx);
                })
            })
            .sep(),
        );
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
                id: format!("std-{pid}"),
                depth: 1,
                expanded: expandable.then_some(sessions_open),
                icon: IconName::SquareTerminal,
                label: t!("sidebar.standalone").to_string(),
                meta: None,
                changes: None,
                title: t!("sidebar.standaloneHint").to_string(),
                muted: false,
                selected,
                on_toggle: expandable.then(|| -> Box<dyn Fn(&mut App)> {
                    Box::new(move |cx| ws1.update(cx, |w, cx| w.toggle_sessions(&pid_toggle, cx)))
                }),
                on_select: Some(Box::new(move |cx| ws3.update(cx, |w, cx| w.select_project(&pid_select, cx)))),
                menu: None,
                context: Some(ctx_items),
                on_new: Some(new_items),
                trailing: badge,
                drag: None,
            },
            cx,
        );

        let mut node = div().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expandable && sessions_open {
            let active = match &ws.state.active {
                ActiveView::Terminal { session_id: id } => Some(id.clone()),
                _ => None,
            };
            let rows: Vec<AnyElement> = mine
                .iter()
                .map(|s| self.session_row(s, 2, active.as_deref() == Some(s.id.as_str()), cx))
                .collect();
            node = node.children(rows);
            for p in mine_pending {
                node = node.child(pending_row(p, 2, cx));
            }
        }
        node.into_any_element()
    }

    /// `group` / `index`：所在服务器分组的键与组内下标，拖拽排序用
    fn folder_node(&self, folder: FolderGroup, group: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        if folder.project.multi.is_some() {
            return self.multi_folder_node(folder, group, index, cx);
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
        let title = project.working_dir.clone().unwrap_or_else(|| project.name.clone());
        let items = menus::project_menu_items(&self.ws, &project, cx);
        let row = tree_row(TreeRowOpts {
            id: format!("fld-{}", project.id),
            depth: 1,
            expanded: Some(open),
            icon: IconName::Folder,
            label: project.name.clone(),
            meta: None,
            changes: None,
            title,
            muted: false,
            selected: false,
            on_toggle: Some(Box::new(move |cx| ws_toggle.update(cx, |w, cx| w.toggle_collapsed(&fkey, cx)))),
            on_select: Some(Box::new(move |cx| ws_select.update(cx, |w, cx| w.select_project(&pid, cx)))),
            menu: Some(items.clone()),
            context: Some(items),
            on_new: None,
            trailing: None,
            drag: sortable.then(|| ProjectDrag { id: project.id.clone(), group: group.to_string(), index }),
        }, cx);

        let mut node = div().relative().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if open {
            let label = checkout_label(&project, head.as_ref());
            node = node.child(self.checkout_node(
                &project,
                label,
                None,
                changes.get(&project.id).cloned(),
                selected.as_deref() == Some(project.id.as_str()),
                cx,
            ));
            for wt in &folder.worktrees {
                node = node.child(self.checkout_node(
                    wt,
                    checkout_label(wt, None),
                    None,
                    changes.get(&wt.id).cloned(),
                    selected.as_deref() == Some(wt.id.as_str()),
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

    /// 多仓库项目：没有检出这一层。会话直接挂在项目行上；批量派生出来的也是这样一行，
    /// 与源项目同级（分组在 [`group_servers`]）。存档的仍置灰，恢复走右键。
    fn multi_folder_node(&self, folder: FolderGroup, group: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.ws.read(cx);
        let project = folder.project;
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
        } else {
            let names = member_names.join(" · ");
            if names.is_empty() {
                project.name.clone()
            } else {
                names
            }
        };
        let count = mine.len() + mine_pending.len();
        let expandable = archived_at.is_none() && count > 0;
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
                icon: if archived_at.is_some() { IconName::Archive } else { IconName::Folders },
                label: project.name.clone(),
                meta,
                changes,
                title,
                muted: archived_at.is_some(),
                selected,
                on_toggle: expandable.then(|| -> Box<dyn Fn(&mut App)> {
                    Box::new(move |cx| ws1.update(cx, |w, cx| w.toggle_sessions(&pid_toggle, cx)))
                }),
                on_select: archived_at.is_none().then(|| -> Box<dyn Fn(&mut App)> {
                    Box::new(move |cx| ws3.update(cx, |w, cx| w.select_project(&pid_select, cx)))
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
            let active = match &ws.state.active {
                ActiveView::Terminal { session_id: id } => Some(id.clone()),
                _ => None,
            };
            for s in &mine {
                node = node.child(self.session_row(s, 2, active.as_deref() == Some(s.id.as_str()), cx));
            }
            for p in mine_pending {
                node = node.child(pending_row(p, 2, cx));
            }
        }
        if archived_at.is_none() {
            for after in [false, true] {
                node = node.child(drop_zone(&self.ws, &project.id, group, index, after, cx));
            }
        }
        node.into_any_element()
    }

    fn checkout_node(
        &self,
        project: &Project,
        label: String,
        meta: Option<String>,
        changes: Option<ProjectChanges>,
        selected: bool,
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
        let title = if project.worktree.is_some() {
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
        let expandable = archived_at.is_none() && count > 0;
        let worst = worst_state(&mine, &mine_pending);
        // 收着的检出里有程序在等人：计数徽标上挂「在等你」的记号（与会话行同一个），不用摊开也看得见
        let blocked = mine
            .iter()
            .any(|s| s.program_status.as_ref().is_some_and(|p| p.state == falcon_proto::ProgramState::Blocked));

        let icon_name = if archived_at.is_some() { IconName::Archive } else { IconName::GitBranch };
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
            depth: 2,
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
            on_select: archived_at.is_none().then(|| -> Box<dyn Fn(&mut App)> {
                Box::new(move |cx| ws3.update(cx, |w, cx| w.select_project(&pid_select, cx)))
            }),
            menu: None,
            context: Some(ctx_items),
            on_new: archived_at.is_none().then_some(new_items),
            trailing: badge,
            drag: None,
        }, cx);

        let mut node = div().flex().flex_col().gap(zpx(ROW_GAP)).child(row);
        if expandable && sessions_open {
            let active = match &ws.state.active {
                ActiveView::Terminal { session_id: id } => Some(id.clone()),
                _ => None,
            };
            let rows: Vec<AnyElement> = mine
                .iter()
                .map(|s| self.session_row(s, 3, active.as_deref() == Some(s.id.as_str()), cx))
                .collect();
            node = node.children(rows);
            for p in mine_pending {
                node = node.child(pending_row(p, 3, cx));
            }
        }
        node.into_any_element()
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
    div()
        .id(SharedString::from(id))
        .flex_none()
        .size(zpx(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(4.))
        .text_color(ui.muted_foreground)
        .child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }).size(zpx(12.)))
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
    on_select: Option<Box<dyn Fn(&mut App)>>,
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
        row = row.cursor_pointer().on_click(move |_, _, cx| select(cx));
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
        row = row.child(
            div().opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)).child(
                Button::new(SharedString::from(format!("{}-new", o.id)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(format!("{} · {}", t!("sidebar.newTerminal"), crate::ui::chord("newTerminal")))
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
