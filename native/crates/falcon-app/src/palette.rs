//! 命令面板（⌘⇧P，web 的 `components/CommandPalette.tsx`）与通用的 [`Picker`]。
//!
//! 前缀语法：`@` 会话、`#` 项目、`>` 命令；不带前缀三组一起搜。前缀语义是这个产品自己的，
//! 不交给模糊打分（它会把 `@` / `#` / `>` 当普通字符去匹配）——筛选就是"标签 + 附注 + 关键字
//! 里包含查询串"，与 web 一致。所有功能都能纯键盘走完。

use std::rc::Rc;

use falcon_proto::{SESSION_AGENTS, SessionState};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, Hsla, IntoElement, Render,
    ScrollHandle, SharedString, Window, div,
};
use rust_i18n::t;

use crate::labels::session_label;
use crate::theme::{Ui, radius};
use crate::ui::{chord, icon};
use crate::workspace::{ActiveView, RightPanelId, Workspace};
use crate::zoom::zpx;

pub type Run = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct PickItem {
    /// 参与筛选的额外关键字
    pub key: String,
    pub label: String,
    pub meta: Option<String>,
    pub icon: IconName,
    pub tone: Option<Hsla>,
    pub run: Run,
}

#[derive(Clone)]
pub struct PickGroup {
    pub label: String,
    pub items: Vec<PickItem>,
}

pub type Source = Rc<dyn Fn(&str, &App) -> Vec<PickGroup>>;

/// 输入框 + 分组列表，↑↓ 选、回车执行、Esc 关（对话框自己接）
pub struct Picker {
    input: Entity<InputState>,
    source: Source,
    groups: Vec<PickGroup>,
    selected: usize,
    scroll: ScrollHandle,
    empty_text: String,
}

impl Picker {
    pub fn new(placeholder: String, empty_text: String, source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                this.refresh(cx);
            }
        })
        .detach();
        input.update(cx, |i, cx| i.focus(window, cx));
        let mut this = Self {
            input,
            source,
            groups: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            empty_text,
        };
        this.refresh(cx);
        this
    }

    /// 数据源变了（例如文件索引拉回来了）时由外面调
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.groups = (self.source)(&query, cx);
        self.selected = 0;
        cx.notify();
    }

    fn flat_len(&self) -> usize {
        self.groups.iter().map(|g| g.items.len()).sum()
    }

    fn item_at(&self, mut index: usize) -> Option<&PickItem> {
        for g in &self.groups {
            if index < g.items.len() {
                return g.items.get(index);
            }
            index -= g.items.len();
        }
        None
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.flat_len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.item_at(index).cloned() else {
            return;
        };
        window.close_dialog(cx);
        (item.run)(window, cx);
    }
}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let mut list = div()
            .id("picker-list")
            .max_h(zpx(360.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(zpx(2.));
        let mut index = 0usize;
        if self.groups.is_empty() {
            list = list.child(
                div()
                    .py_6()
                    .flex()
                    .justify_center()
                    .text_xs()
                    .text_color(ui.muted_foreground)
                    .child(self.empty_text.clone()),
            );
        }
        for g in &self.groups {
            list = list.child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(g.label.clone()),
            );
            for item in &g.items {
                let i = index;
                let selected = i == self.selected;
                let mut row = div()
                    .id(SharedString::from(format!("pick-{i}")))
                    .h(zpx(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(radius::SM)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| this.confirm(i, window, cx)))
                    .child(icon(item.icon.clone()).size(zpx(14.)).text_color(item.tone.unwrap_or(ui.muted_foreground)))
                    .child(div().flex_1().min_w_0().truncate().child(item.label.clone()));
                if let Some(meta) = &item.meta {
                    row = row.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(meta.clone()));
                }
                row = if selected { row.bg(ui.tint).text_color(ui.tint_foreground) } else { row.hover(|s| s.bg(ui.muted)) };
                list = list.child(row);
                index += 1;
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            // ↑↓ / 回车在输入框的上下文里各自绑了动作（MoveUp / MoveDown / Enter），GPUI 先分派
            // 动作、没人要才轮到 key_down——key_down 监听永远等不到，只能在捕获阶段截动作。
            // 回车不截的话还会一路冒到对话框的确认上，直接把面板关掉
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                this.move_selection(1, cx);
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                this.move_selection(-1, cx);
            }))
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                cx.stop_propagation();
                this.confirm(this.selected, window, cx);
            }))
            .child(Input::new(&self.input))
            .child(list)
    }
}

pub fn open_picker(placeholder: String, empty_text: String, source: Source, window: &mut Window, cx: &mut App) -> Entity<Picker> {
    let picker = cx.new(|cx| Picker::new(placeholder, empty_text, source, window, cx));
    let view = picker.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.w(zpx(580.)).close_button(false).margin_top(zpx(80.)).child(view.clone())
    });
    // 打开对话框会把焦点交给对话框自己的容器（Root::open_dialog），输入框得在那之后再要一次，
    // 否则敲的字落回原先聚焦的终端里
    let input = picker.read(cx).input.clone();
    input.update(cx, |i, cx| i.focus(window, cx));
    picker
}

// ---------------- 命令面板 ----------------

pub fn open(ws: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let ws = ws.clone();
    let source: Source = Rc::new(move |query, cx| build(&ws, query, cx));
    open_picker(t!("palette.placeholder").to_string(), t!("palette.empty").to_string(), source, window, cx);
}

fn run(f: impl Fn(&mut Window, &mut App) + 'static) -> Run {
    Rc::new(f)
}

fn build(ws_entity: &Entity<Workspace>, query: &str, cx: &App) -> Vec<PickGroup> {
    let ui = Ui::global(cx).clone();
    let ws = ws_entity.read(cx);
    let local = crate::labels::local_word();
    let host_label = |pid: &str| falcon_core::host_color::host_label(ws.project(pid), &local);

    let session_items: Vec<PickItem> = ws
        .sessions
        .iter()
        .map(|s| {
            let (ic, tone) = match s.state {
                SessionState::Unverified => (IconName::CircleAlert, ui.warning),
                SessionState::Dead => (IconName::CircleX, ui.destructive),
                _ => (IconName::CircleDot, ui.success),
            };
            let name = session_label(s, &ws.projects);
            let host = host_label(&s.project_id);
            let (w, id) = (ws_entity.clone(), s.id.clone());
            PickItem {
                key: format!("@{name} {} {host}", s.project_name),
                label: name,
                meta: Some(format!("{} · {host}", s.project_name)),
                icon: ic,
                tone: Some(tone),
                run: run(move |_, cx| w.update(cx, |w, cx| w.open_session(&id, cx))),
            }
        })
        .collect();

    // 存档的项目不能开终端（到期会连目录一起删），不进面板。每个项目一条普通终端 + 每家 CLI
    // 一条：面板是"敲名字就开"的地方，让人先开终端再输命令就白搭了
    let mut project_items: Vec<PickItem> = Vec::new();
    for p in ws.projects.iter().filter(|p| p.worktree.as_ref().is_none_or(|w| w.archived_at.is_none())) {
        let host = host_label(&p.id);
        {
            let (w, pid) = (ws_entity.clone(), p.id.clone());
            project_items.push(PickItem {
                key: format!("#{} {}", p.name, t!("sidebar.newTerminal")),
                label: t!("palette.newTerminalIn", name = p.name.clone()).to_string(),
                meta: Some(host.clone()),
                icon: IconName::SquareTerminal,
                tone: None,
                run: run(move |_, cx| w.update(cx, |w, cx| w.new_terminal(&pid, None, None, cx))),
            });
        }
        for agent in SESSION_AGENTS {
            let agent_name = t!(format!("agent.{}", agent.as_str())).to_string();
            let (w, pid, a) = (ws_entity.clone(), p.id.clone(), *agent);
            project_items.push(PickItem {
                key: format!("#{} {agent_name}", p.name),
                label: t!("palette.newAgentIn", agent = agent_name, name = p.name.clone()).to_string(),
                meta: Some(host.clone()),
                icon: IconName::Bot,
                tone: None,
                run: run(move |_, cx| w.update(cx, |w, cx| w.new_terminal(&pid, Some(a), None, cx))),
            });
        }
    }
    {
        let w = ws_entity.clone();
        project_items.push(PickItem {
            key: format!("#{}", t!("palette.newProject")),
            label: t!("palette.newProject").to_string(),
            meta: None,
            icon: IconName::Plus,
            tone: None,
            run: run(move |window, cx| crate::dialogs::project_form::open(&w, None, Default::default(), window, cx)),
        });
    }

    let mut actions: Vec<PickItem> = Vec::new();
    for s in ws.sessions.iter().filter(|s| s.state == SessionState::Unverified) {
        let name = session_label(s, &ws.projects);
        let (w, id) = (ws_entity.clone(), s.id.clone());
        actions.push(PickItem {
            key: format!(">{} {name}", t!("session.reattach")),
            label: t!("palette.reattachOne", name = name).to_string(),
            meta: Some(chord("reattach").to_string()),
            icon: IconName::CircleAlert,
            tone: Some(ui.warning),
            run: run(move |_, cx| crate::menus::reattach(&w, &id, cx)),
        });
    }
    for p in ws
        .projects
        .iter()
        .filter(|p| p.project_type == falcon_proto::ProjectType::Ssh && p.worktree.as_ref().is_none_or(|w| w.archived_at.is_none()))
    {
        let host = p.ssh.as_ref().map(|s| s.host.clone()).unwrap_or_else(|| p.name.clone());
        let (w, pid) = (ws_entity.clone(), p.id.clone());
        actions.push(PickItem {
            key: format!(">{} {}", t!("palette.enableDurable", host = host.clone()), p.name),
            label: t!("palette.enableDurable", host = host).to_string(),
            meta: Some(p.name.clone()),
            icon: IconName::ShieldCheck,
            tone: None,
            run: run(move |window, cx| crate::dialogs::install::open(&w, &pid, None, window, cx)),
        });
    }
    if let ActiveView::Terminal { session_id: id } = &ws.state.active {
        if let Some(current) = ws.session(id).filter(|s| s.state != SessionState::Dead).cloned() {
            let w = ws_entity.clone();
            actions.push(PickItem {
                key: format!(">{}", t!("palette.terminateCurrent")),
                label: t!("palette.terminateCurrent").to_string(),
                meta: None,
                icon: IconName::CircleX,
                tone: Some(ui.destructive),
                run: run(move |window, cx| crate::menus::terminate(&w, &current, window, cx)),
            });
        }
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{} {}", t!("palette.zoomOn"), t!("palette.zoomOff")),
            label: if ws.state.term_zoomed { t!("palette.zoomOff") } else { t!("palette.zoomOn") }.to_string(),
            meta: None,
            icon: if ws.state.term_zoomed { IconName::Minimize2 } else { IconName::Maximize2 },
            tone: None,
            run: run(move |_, cx| w.update(cx, |w, cx| w.toggle_term_zoom(cx))),
        });
    }
    {
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.goToFile")),
            label: t!("palette.goToFile").to_string(),
            meta: Some(chord("quickOpen").to_string()),
            icon: IconName::FileText,
            tone: None,
            run: run(move |window, cx| crate::dialogs::quick_open::open(&w, window, cx)),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.overview")),
            label: t!("palette.overview").to_string(),
            meta: Some(chord("overview").to_string()),
            icon: IconName::LayoutDashboard,
            tone: None,
            run: run(move |_, cx| w.update(cx, |w, cx| w.show_overview(cx))),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.openSettings")),
            label: t!("palette.openSettings").to_string(),
            meta: None,
            icon: IconName::Settings,
            tone: None,
            run: run(move |window, cx| crate::dialogs::settings::open(&w, Default::default(), window, cx)),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{} {}", t!("palette.toggleSidebarOn"), t!("palette.toggleSidebarOff")),
            label: if ws.state.sidebar_open { t!("palette.toggleSidebarOn") } else { t!("palette.toggleSidebarOff") }.to_string(),
            meta: Some(chord("toggleSidebar").to_string()),
            icon: IconName::PanelLeft,
            tone: None,
            run: run(move |_, cx| w.update(cx, |w, cx| w.toggle_sidebar(cx))),
        });
    }
    for (panel, ic, on, off, ch) in [
        (RightPanelId::Files, IconName::Folder, "palette.toggleFilesOn", "palette.toggleFilesOff", "toggleFilesPanel"),
        (RightPanelId::Changes, IconName::FileDiff, "palette.toggleChangesOn", "palette.toggleChangesOff", "toggleChangesPanel"),
        (RightPanelId::Git, IconName::GitBranch, "palette.toggleGitOn", "palette.toggleGitOff", "toggleGitPanel"),
        (RightPanelId::Meegle, IconName::ListTodo, "palette.toggleMeegleOn", "palette.toggleMeegleOff", "toggleMeeglePanel"),
    ] {
        let showing = ws.state.right_open && ws.state.right_panel == panel;
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{} {}", t!(on), t!(off)),
            label: if showing { t!(on) } else { t!(off) }.to_string(),
            meta: Some(chord(ch).to_string()),
            icon: ic,
            tone: None,
            run: run(move |_, cx| w.update(cx, |w, cx| w.toggle_right_panel(panel, cx))),
        });
    }
    for (pref, key) in [
        (falcon_theme::ThemePref::System, "theme.system"),
        (falcon_theme::ThemePref::Light, "theme.light"),
        (falcon_theme::ThemePref::Dark, "theme.dark"),
    ] {
        if crate::theme::current_mode_pref(cx) == pref {
            continue;
        }
        actions.push(PickItem {
            key: format!(">{} {} theme", t!("theme.label"), t!(key)),
            label: t!("palette.theme", name = t!(key)).to_string(),
            meta: None,
            icon: match pref {
                falcon_theme::ThemePref::Light => IconName::Sun,
                falcon_theme::ThemePref::Dark => IconName::Moon,
                _ => IconName::SunMoon,
            },
            tone: None,
            run: run(move |_, cx| crate::theme::update_settings(cx, |s| s.mode = pref)),
        });
    }
    {
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{} theme ghostty", t!("palette.pickTheme")),
            label: t!("palette.pickTheme").to_string(),
            meta: None,
            icon: IconName::Palette,
            tone: None,
            run: run(move |window, cx| crate::dialogs::settings::open(&w, crate::dialogs::settings::Tab::Appearance, window, cx)),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.openRelays")),
            label: t!("palette.openRelays").to_string(),
            meta: Some(chord("openRelays").to_string()),
            icon: IconName::ArrowLeftRight,
            tone: None,
            run: run(move |window, cx| crate::dialogs::settings::open(&w, crate::dialogs::settings::Tab::Relays, window, cx)),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.setPassword")),
            label: t!("palette.setPassword").to_string(),
            meta: None,
            icon: IconName::Settings,
            tone: None,
            run: run(move |window, cx| crate::dialogs::settings::open(&w, crate::dialogs::settings::Tab::Account, window, cx)),
        });
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.addHost")),
            label: t!("palette.addHost").to_string(),
            meta: None,
            icon: IconName::Plus,
            tone: None,
            run: run(move |window, cx| crate::dialogs::host_form::open(&w, None, None, window, cx)),
        });
        actions.push(PickItem {
            key: format!(">{}", t!("native.menu.connect")),
            label: t!("native.menu.connect").to_string(),
            meta: None,
            icon: IconName::Server,
            tone: None,
            run: run(|_, cx| crate::dialogs::connect::open_connect_window(cx)),
        });
    }
    if ws.auth.as_ref().is_some_and(|a| a.required && a.authenticated) {
        let w = ws_entity.clone();
        actions.push(PickItem {
            key: format!(">{}", t!("palette.logout")),
            label: t!("palette.logout").to_string(),
            meta: None,
            icon: IconName::X,
            tone: None,
            run: run(move |_, cx| w.update(cx, |w, cx| w.logout(cx))),
        });
    }

    let groups = [
        ('@', t!("palette.groupSessions").to_string(), session_items),
        ('#', t!("palette.groupProjects").to_string(), project_items),
        ('>', t!("palette.groupActions").to_string(), actions),
    ];
    let prefix = query.chars().next();
    let needle = query.trim_start_matches(['>', '@', '#']).trim().to_lowercase();
    groups
        .into_iter()
        .filter(|(p, _, _)| match prefix {
            Some(c @ ('@' | '#' | '>')) => *p == c,
            _ => true,
        })
        .map(|(_, label, items)| PickGroup {
            label,
            items: items
                .into_iter()
                .filter(|i| {
                    needle.is_empty()
                        || format!("{} {} {}", i.label, i.meta.clone().unwrap_or_default(), i.key)
                            .to_lowercase()
                            .contains(&needle)
                })
                .collect(),
        })
        .filter(|g| !g.items.is_empty())
        .collect()
}
