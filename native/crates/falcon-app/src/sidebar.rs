//! 侧栏：服务器 → 文件夹 → 检出 → 会话（web 的 `components/Sidebar.tsx`）。
//!
//! - 顶上不设标题栏：树本身就是内容，新建项目走各服务器行 hover 的 ＋ / 右键 / 命令面板；
//! - 行的 hover / 选中是内缩的圆角块（左右留内边距，贴着岛边会切掉圆角）；
//! - 会话行默认收起：一个检出能挂五六个终端，全摊开就占满侧栏；检出行尾巴上摆计数徽标 +
//!   最重的异常状态，折叠着也看得出出没出事。

use falcon_core::meegle_drag::MeegleWorkItemDragPayload;
use falcon_core::project_tree::{FolderGroup, ServerGroup, ServerKind, checkout_label, folder_key, group_servers};
use falcon_proto::{Project, SessionState, SessionWithProject};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::{Sizable, tooltip::Tooltip};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, Pixels, Render, SharedString, Window, div,
};
use rust_i18n::t;

use crate::labels::{local_word, session_label};
use crate::menus::{self, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::{Mark, host_bar, icon, status_mark};
use crate::workspace::{ActiveView, PendingSession, ProjectChanges, ToastKind, Workspace};
use crate::zoom::zpx;

const ROW_H: f32 = 30.;
const SESSION_ROW_H: f32 = 28.;

fn indent(depth: usize) -> Pixels {
    zpx(4. + depth as f32 * 12.)
}

pub struct Sidebar {
    ws: Entity<Workspace>,
}

impl Sidebar {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
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
        let servers: Vec<ServerGroup> = group_servers(&ws.projects, &ws.hosts, &local_word(), ws.state.show_archived)
            .into_iter()
            .filter(|s| s.kind != ServerKind::Local || !s.folders.is_empty() || ws.hosts.is_empty())
            .collect();
        let empty = ws.projects.is_empty() && ws.hosts.is_empty();

        let mut tree = div()
            .id("sidebar-tree")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(zpx(6.))
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
            .h(zpx(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(zpx(6.))
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

        let menu_items = match (&server.kind, &server.host) {
            (ServerKind::Host, Some(host)) => Some(menus::host_menu_items(&self.ws, host)),
            (ServerKind::Local, _) => Some(menus::server_menu_items(&self.ws, true)),
            _ => None,
        };
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

        let mut node = div().mb(zpx(2.)).flex().flex_col().child(row);
        if expanded {
            if server.folders.is_empty() {
                node = node.child(
                    div()
                        .py_1()
                        .pr_2()
                        .pl(zpx(34.))
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(t!("sidebar.emptyServer").to_string()),
                );
            } else {
                for folder in server.folders {
                    node = node.child(self.folder_node(folder, cx));
                }
            }
        }
        node.into_any_element()
    }

    fn folder_node(&self, folder: FolderGroup, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.ws.read(cx);
        let project = folder.project.clone();
        let open = ws.state.collapsed.get(&folder_key(&project.id)).copied() != Some(true);
        let head = ws.heads.get(&project.id).cloned();
        let member_names: Vec<String> = project
            .multi
            .as_ref()
            .map(|m| m.repos.iter().map(|r| basename(&r.dir)).collect())
            .unwrap_or_default();
        let selected = ws.state.selected_project_id.clone();
        let changes = ws.changes.clone();

        let ws_toggle = self.ws.clone();
        let ws_select = self.ws.clone();
        let fkey = folder_key(&project.id);
        let pid = project.id.clone();
        let title = if project.multi.is_some() {
            member_names.join(" · ")
        } else {
            project.working_dir.clone().unwrap_or_else(|| project.name.clone())
        };
        let items = menus::project_menu_items(&self.ws, &project, cx);
        let row = tree_row(TreeRowOpts {
            id: format!("fld-{}", project.id),
            depth: 1,
            expanded: Some(open),
            icon: if project.multi.is_some() { IconName::Folders } else { IconName::Folder },
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
            meegle_drop: project.worktree.is_none().then(|| (self.ws.clone(), project.id.clone())),
        }, cx);

        let mut node = div().flex().flex_col().child(row);
        if open {
            let label = if project.multi.is_some() {
                t!("multi.repoCount", n = member_names.len()).to_string()
            } else {
                checkout_label(&project, head.as_ref())
            };
            let meta = project
                .multi
                .as_ref()
                .map(|_| member_names.iter().take(3).cloned().collect::<Vec<_>>().join(" · "));
            node = node.child(self.checkout_node(&project, label, meta, changes.get(&project.id).cloned(), selected.as_deref() == Some(project.id.as_str()), cx));
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
        let source_name = project
            .worktree
            .as_ref()
            .and_then(|wt| ws.project(&wt.source_project_id))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        // 存档的附属项目：置灰、显示删除倒计时、点击不再选中（要恢复走右键菜单）
        let archived_at = project.worktree.as_ref().and_then(|w| w.archived_at);
        let archived_note = archived_at.map(|at| {
            let deadline = at + falcon_proto::WORKTREE_ARCHIVE_TTL_MS;
            let now = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let days_left = ((deadline - now) as f64 / 86_400_000.0).ceil() as i64;
            if days_left >= 1 {
                t!("worktree.archivedMetaDays", n = days_left).to_string()
            } else {
                t!("worktree.archivedMetaSoon").to_string()
            }
        });
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

        let icon_name = if archived_at.is_some() {
            IconName::Archive
        } else if project.multi.is_some() && project.worktree.is_none() {
            IconName::Folders
        } else {
            IconName::GitBranch
        };
        let pid = project.id.clone();
        let (ws1, ws2, ws3) = (self.ws.clone(), self.ws.clone(), self.ws.clone());
        let pid_toggle = pid.clone();
        let pid_badge = pid.clone();
        let pid_select = pid.clone();
        let ctx_items = menus::project_menu_items(&self.ws, project, cx);
        let new_items = menus::new_session_items(&self.ws, &project.id, None);
        let badge = expandable.then(|| {
            session_count_badge(
                format!("badge-{pid_badge}"),
                count,
                worst,
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
            selected,
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
            meegle_drop: None,
        }, cx);

        let mut node = div().flex().flex_col().child(row);
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
        let items = menus::session_menu_items(&self.ws, session, cx);
        let ctx = items.clone();
        let ws_open = self.ws.clone();
        let sid = session.id.clone();
        let icon_name = if session.agent.is_some() { IconName::Bot } else { IconName::SquareTerminal };
        let tip: SharedString = format!("{label} · {}", t!("sidebar.projectContext")).into();
        let mut row = div()
            .id(SharedString::from(format!("sess-{}", session.id)))
            .group("sess-row")
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
            .child(icon(icon_name).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().pl_1().child(label));
        // 运行中不摆状态记号，异常才值得占位置
        if session.state != SessionState::Active {
            row = row.child(status_mark(Mark::from(session.state), zpx(12.), cx));
        }
        row = row.child(
            div().opacity(0.).group_hover("sess-row", |s| s.opacity(1.)).child(
                Button::new(SharedString::from(format!("sess-menu-{}", session.id)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
            ),
        );
        row.context_menu(move |menu, _, _| to_popup(menu, ctx.clone()))
            .into_any_element()
    }
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
    expanded: bool,
    on_toggle: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let label: SharedString = t!("sidebar.sessionCount", n = count).to_string().into();
    let mark = match worst {
        Some(m) => status_mark(m, zpx(12.), cx),
        None => icon(IconName::SquareTerminal).size(zpx(12.)).into_any_element(),
    };
    div()
        .id(SharedString::from(id))
        .flex_none()
        .h(zpx(20.))
        .flex()
        .items_center()
        .gap(zpx(2.))
        .px(zpx(2.))
        .rounded(zpx(4.))
        .text_size(zpx(11.))
        .text_color(if expanded { ui.foreground } else { ui.muted_foreground })
        .hover(|s| s.text_color(ui.foreground))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
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
    /// 源项目行：接住从飞书项目面板拖来的工作项，以它的 Key 预填派生表单
    meegle_drop: Option<(Entity<Workspace>, String)>,
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
    row = match (o.expanded, o.on_toggle) {
        (Some(expanded), Some(toggle)) => row.child(chevron(format!("{}-toggle", o.id), expanded, cx, toggle)),
        _ => row.child(div().flex_none().size(zpx(18.))),
    };
    row = row
        .child(icon(o.icon).size(zpx(14.)).text_color(ui.muted_foreground))
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
    if let Some((ws, source_id)) = o.meegle_drop {
        // 落点只认类型化的载荷（web 是自定义 dataTransfer 类型）；拖到上面时整行换底色
        let over = ui.accent;
        row = row
            .drag_over::<MeegleWorkItemDragPayload>(move |s, _, _, _| s.bg(over))
            .on_drop(move |payload: &MeegleWorkItemDragPayload, window, cx| {
                open_worktree_from_meegle(ws.clone(), source_id.clone(), payload.clone(), window, cx)
            });
    }
    match o.context {
        Some(items) => row.context_menu(move |menu, _, _| to_popup(menu, items.clone())).into_any_element(),
        None => row.into_any_element(),
    }
}

/// 飞书项目工作项拖到源项目上（web `Sidebar.tsx` 的 `openWorktreeFromMeegle`）：取详情拿到带
/// 前缀的 Key，预填派生表单的名字与分支（new-branch 模式，基点是源项目的默认基点或 HEAD）。
/// 什么都不直接建——表单等用户确认
fn open_worktree_from_meegle(
    ws: Entity<Workspace>,
    source_id: String,
    payload: MeegleWorkItemDragPayload,
    window: &mut Window,
    cx: &mut App,
) {
    let client = ws.read(cx).client.clone();
    let handle = window.window_handle();
    let fetch = client.meegle_work_item(&payload.space_key, &payload.id, false);
    // 命中服务端缓存时详情几十毫秒就回来，立刻提示只是闪一下；慢了才值得交代在等什么
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let (done_hint, ws_hint) = (done.clone(), ws.clone());
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
        if !done_hint.get() {
            cx.update(|cx| ws_hint.update(cx, |w, cx| w.toast(ToastKind::Info, t!("meegle.dropKeyLoading").to_string(), None, cx)));
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        let result = fetch.await;
        done.set(true);
        cx.update(|cx| {
            let key = match &result {
                Ok(detail) => falcon_core::meegle_key::meegle_display_key(detail),
                Err(err) => {
                    ws.update(cx, |w, cx| {
                        w.handle_error(err, cx);
                        w.toast(ToastKind::Warning, t!("meegle.dropKeyFallback").to_string(), None, cx);
                    });
                    payload.id.clone()
                }
            };
            let start_point = ws
                .read(cx)
                .project(&source_id)
                .and_then(|p| p.default_worktree_branch.clone())
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "HEAD".to_string());
            let preset = crate::dialogs::worktree_form::Preset {
                branch: Some(key.clone()),
                name: Some(key),
                start_point: Some(start_point),
            };
            let _ = handle.update(cx, |_, window, cx| {
                crate::dialogs::worktree_form::open(&ws, &source_id, preset, window, cx)
            });
        });
    })
    .detach();
}
