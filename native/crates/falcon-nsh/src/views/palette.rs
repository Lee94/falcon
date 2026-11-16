use std::rc::Rc;

use falcon_core::command_palette::{self, Entry, Group};
use falcon_proto::{CreateSessionRequest, SessionState};
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{ActiveDialog, RightPanelKind, SessionPlacement, SettingsTab, WorkspaceStore};
use crate::terminal::bind_safe_key_when;
use crate::theme::{ThemeController, rgb_to_hsla, use_theme};
use crate::zoom::{ZoomController, zpx};

#[derive(Clone)]
struct Row {
    entry: Entry,
    run: Rc<dyn Fn()>,
}

#[derive(Default)]
pub(crate) struct PaletteKeyState {
    rows: Vec<Row>,
    row_refs: Vec<nsh_core::element::ElementRef>,
}

pub(crate) fn render(
    store: &WorkspaceStore,
    query: Signal<String>,
    selected: Signal<usize>,
    key_state: Rc<std::cell::RefCell<PaletteKeyState>>,
    theme_ctrl: ThemeController,
    zoom_ctrl: Option<ZoomController>,
    theme_picker_open: Signal<bool>,
    theme_slot: Signal<falcon_theme::ThemeMode>,
) -> impl IntoElement {
    let input_ref = element_ref();
    let change_input = input_ref.clone();
    let content_input = input_ref.clone();
    let content_store = store.clone();
    let content_key_state = key_state.clone();
    view()
        .flex_col()
        .h(zpx(460.0))
        .child(
            view().p(zpx(16.0)).child(
                input()
                    .test_id("palette-input")
                    .element_ref(&input_ref)
                    .auto_focus(true)
                    .w_full()
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0))
                    .bg(crate::theme::rgba_to_hsla(use_theme().resolved().ui.input))
                    .text_xs()
                    .placeholder(t!("palette.placeholder"))
                    .on_change(move |value: &str| {
                        query.set(value.to_string());
                        selected.set(0);
                        change_input.focus();
                    }),
            ),
        )
        .child(dynamic(move || {
            render_rows(
                content_store.clone(),
                query,
                selected,
                content_key_state.clone(),
                theme_ctrl.clone(),
                zoom_ctrl.clone(),
                theme_picker_open,
                theme_slot,
                content_input.clone(),
            )
        }))
}

fn render_rows(
    store: WorkspaceStore,
    query: Signal<String>,
    selected: Signal<usize>,
    key_state: Rc<std::cell::RefCell<PaletteKeyState>>,
    theme_ctrl: ThemeController,
    zoom_ctrl: Option<ZoomController>,
    theme_picker_open: Signal<bool>,
    theme_slot: Signal<falcon_theme::ThemeMode>,
    input_ref: nsh_core::element::ElementRef,
) -> impl IntoElement {
    let resolved = use_theme().resolved();
    let ui = &resolved.ui;
    let mut rows = Vec::new();
    for session in store.sessions.get() {
        if session.state == SessionState::Dead {
            continue;
        }
        let label = session.display_title();
        let attached = store.is_session_attached(&session.id);
        let meta = format!(
            "{} · {}",
            if attached {
                t!("palette.sessionOpen")
            } else {
                t!("palette.sessionReattach")
            },
            session.cwd
        );
        let st = store.clone();
        let id = session.id.clone();
        rows.push(Row {
            entry: Entry {
                id: format!("session:{}", session.id),
                label,
                meta: meta.clone(),
                keywords: vec![session.cwd, session.name],
                group: Group::Session,
            },
            run: Rc::new(move || st.focus_palette_session(&id)),
        });
    }
    let server_url = store.server_url.get();
    let server_here = store
        .profiles
        .get()
        .profiles
        .iter()
        .find(|profile| profile.url.trim_end_matches('/') == server_url.trim_end_matches('/'))
        .is_some_and(|profile| profile.is_loopback());
    for project in store.projects.get() {
        let st = store.clone();
        let id = project.id.clone();
        rows.push(Row {
            entry: Entry {
                id: format!("project:{}", project.id),
                label: project.name.clone(),
                meta: project.path.clone(),
                keywords: vec![project.path.clone()],
                group: Group::Project,
            },
            run: Rc::new(move || st.select_project(&id)),
        });
        if let Some(source) = project.source.as_ref()
            && let Some(target) = falcon_core::editor::editor_target(source, server_here)
        {
            for editor in falcon_core::editor::Editor::ALL {
                let Some(url) = falcon_core::editor::editor_url(editor, &target) else {
                    continue;
                };
                let editor_name = editor.name();
                let open_label = t!(
                    "palette.openInEditor",
                    editor = editor_name,
                    name = project.name.as_str()
                );
                let url = url.clone();
                rows.push(Row {
                    entry: Entry {
                        id: format!("editor:{}:{}", project.id, editor_name),
                        label: open_label,
                        meta: project.path.clone(),
                        keywords: vec![editor_name.to_string(), project.path.clone()],
                        group: Group::Project,
                    },
                    run: Rc::new(move || {
                        let _ = open_url(url.clone());
                    }),
                });
            }
        }
    }
    let focused = store.focused_pane_key.get();
    let current_session = falcon_core::pane_key::parse_pane_key(&focused)
        .and_then(|pane| match pane {
            falcon_core::pane_key::PaneItem::Terminal { id, .. } => Some(id),
            _ => None,
        })
        .filter(|id| {
            store
                .sessions
                .get()
                .iter()
                .any(|session| session.id == *id && session.state != SessionState::Dead)
        });
    let can_create = store.can_create_session();
    let mut commands: Vec<(String, String, String, Box<dyn Fn()>)> = vec![
        (
            "new-project".into(),
            t!("palette.newProject"),
            "create project".into(),
            {
                let st = store.clone();
                Box::new(move || st.active_dialog.set(ActiveDialog::NewProject))
            },
        ),
        (
            "quick-open".into(),
            t!("palette.goToFile"),
            "file path".into(),
            {
                let st = store.clone();
                Box::new(move || st.open_quick_open())
            },
        ),
        (
            "overview".into(),
            t!("palette.overview"),
            "windows canvas".into(),
            {
                let st = store.clone();
                Box::new(move || st.overview_open.set(true))
            },
        ),
        (
            "sidebar".into(),
            if store.sidebar_open.get() {
                t!("palette.toggleSidebarOn")
            } else {
                t!("palette.toggleSidebarOff")
            },
            "toggle navigation".into(),
            {
                let st = store.clone();
                Box::new(move || st.sidebar_open.update(|v| *v = !*v))
            },
        ),
        (
            "files".into(),
            if store.right_panel_open.get() && store.right_panel_kind.get() == RightPanelKind::Files
            {
                t!("palette.toggleFilesOn")
            } else {
                t!("palette.toggleFilesOff")
            },
            "toggle files".into(),
            {
                let st = store.clone();
                Box::new(move || st.toggle_right_panel(RightPanelKind::Files))
            },
        ),
        (
            "changes".into(),
            if store.right_panel_open.get() && store.right_panel_kind.get() == RightPanelKind::Git {
                t!("palette.toggleChangesOn")
            } else {
                t!("palette.toggleChangesOff")
            },
            "toggle changes git".into(),
            {
                let st = store.clone();
                Box::new(move || st.toggle_right_panel(RightPanelKind::Git))
            },
        ),
        (
            "settings".into(),
            t!("palette.openSettings"),
            "preferences appearance".into(),
            {
                let st = store.clone();
                Box::new(move || {
                    st.active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Appearance))
                })
            },
        ),
        (
            "relays".into(),
            t!("palette.openRelays"),
            "settings relay".into(),
            {
                let st = store.clone();
                Box::new(move || {
                    st.active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Relays))
                })
            },
        ),
        (
            "profiles".into(),
            t!("palette.profiles"),
            "server profile".into(),
            {
                let st = store.clone();
                Box::new(move || st.active_dialog.set(ActiveDialog::Profiles))
            },
        ),
        (
            "theme-system".into(),
            t!("theme.system"),
            "theme mode".into(),
            {
                let tc = theme_ctrl;
                Box::new(move || tc.set_mode(falcon_theme::ThemePref::System))
            },
        ),
        (
            "theme-light".into(),
            t!("theme.light"),
            "theme mode".into(),
            {
                let tc = theme_ctrl;
                Box::new(move || tc.set_mode(falcon_theme::ThemePref::Light))
            },
        ),
        (
            "theme-dark".into(),
            t!("theme.dark"),
            "theme mode".into(),
            {
                let tc = theme_ctrl;
                Box::new(move || tc.set_mode(falcon_theme::ThemePref::Dark))
            },
        ),
        (
            "theme-picker".into(),
            t!("palette.pickTheme"),
            "choose theme".into(),
            {
                let st = store.clone();
                let picker = theme_picker_open;
                let slot = theme_slot;
                let tc = theme_ctrl;
                Box::new(move || {
                    slot.set(tc.active_slot());
                    picker.set(true);
                    st.active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Appearance));
                })
            },
        ),
    ];
    if let Some(zoom) = zoom_ctrl {
        let zoom_in = zoom;
        commands.push((
            "zoom-in".into(),
            t!("native.settings.uiZoomIn"),
            "ui zoom".into(),
            Box::new(move || zoom_in.step_in()),
        ));
        let zoom_out = zoom;
        commands.push((
            "zoom-out".into(),
            t!("native.settings.uiZoomOut"),
            "ui zoom".into(),
            Box::new(move || zoom_out.step_out()),
        ));
        commands.push((
            "zoom-reset".into(),
            t!("palette.zoomReset"),
            "ui zoom".into(),
            Box::new(move || zoom.reset()),
        ));
    }
    if can_create {
        let st = store.clone();
        commands.push((
            "new-terminal".into(),
            t!("sidebar.newTerminal"),
            "terminal shell".into(),
            Box::new(move || {
                st.create_terminal_in_active_project();
            }),
        ));
    }
    if can_create && !focused.is_empty() {
        let st = store.clone();
        commands.push((
            "split-right".into(),
            t!("tab.newToRight"),
            "split terminal right".into(),
            Box::new(move || {
                st.create_terminal_with_request(
                    CreateSessionRequest::default(),
                    SessionPlacement::Right,
                );
            }),
        ));
        let st = store.clone();
        commands.push((
            "split-down".into(),
            t!("tab.newBelow"),
            "split terminal down".into(),
            Box::new(move || {
                st.create_terminal_with_request(
                    CreateSessionRequest::default(),
                    SessionPlacement::Down,
                );
            }),
        ));
    }
    for agent_name in store
        .detected_agents
        .get()
        .into_iter()
        .filter(|_| can_create)
    {
        let Some(agent) = falcon_proto::SESSION_AGENTS
            .iter()
            .find(|agent| agent.as_str() == agent_name)
            .cloned()
        else {
            continue;
        };
        let project_name = store
            .projects
            .get()
            .into_iter()
            .find(|project| project.id == store.active_project_id.get())
            .map_or_else(String::new, |project| project.name);
        let label = t!(
            "palette.newAgentIn",
            name = project_name.as_str(),
            agent = agent.as_str()
        );
        let st = store.clone();
        let request = CreateSessionRequest {
            agent: Some(agent.clone()),
            ..Default::default()
        };
        commands.push((
            format!("agent:{}", agent.as_str()),
            label,
            format!("{} agent cli", project_name),
            Box::new(move || {
                st.create_terminal_with_request(request.clone(), SessionPlacement::Focus);
            }),
        ));
    }
    // 「为某主机启用持久会话」（GPUI palette.enableDurable）：只走授权 + 部署，不建会话。
    // 部署对话框由 install 流程单元补齐，落地前经 request_enable_durable 占位入口给说明。
    for project in store.projects.get().into_iter().filter(|project| {
        project.source.as_ref().is_some_and(|source| {
            source.project_type == falcon_proto::ProjectType::Ssh
                && source
                    .worktree
                    .as_ref()
                    .is_none_or(|worktree| worktree.archived_at.is_none())
        })
    }) {
        let host = project
            .host_name
            .clone()
            .unwrap_or_else(|| project.name.clone());
        let st = store.clone();
        let pid = project.id.clone();
        commands.push((
            format!("enable-durable:{}", project.id),
            t!("palette.enableDurable", host = host.clone()),
            format!("durable session {}", project.name),
            Box::new(move || st.request_enable_durable(&pid)),
        ));
    }
    for plugin in store
        .plugin_list
        .get()
        .plugins
        .into_iter()
        .filter(|plugin| plugin.enabled && plugin.error.is_none())
    {
        let st = store.clone();
        let plugin_id = plugin.id.clone();
        let action_plugin_id = plugin_id.clone();
        let plugin_name = plugin.name.clone();
        let is_open = store.right_panel_open.get()
            && store.right_panel_kind.get() == RightPanelKind::Plugin(plugin_id.clone());
        commands.push((
            format!("plugin-toggle:{}", plugin_id),
            if is_open {
                t!("palette.togglePluginOn", name = plugin_name.as_str())
            } else {
                t!("palette.togglePluginOff", name = plugin_name.as_str())
            },
            format!("plugin {}", plugin_name),
            Box::new(move || {
                st.toggle_right_panel(RightPanelKind::Plugin(action_plugin_id.clone()))
            }),
        ));
    }
    if let Some(id) = current_session.as_ref() {
        let st = store.clone();
        let key = focused.clone();
        rows.push(Row {
            entry: Entry {
                id: "command:detach-current".into(),
                label: t!("native.nsh.DetachWindow"),
                meta: id.clone(),
                keywords: vec!["detach current pane".into()],
                group: Group::Command,
            },
            run: Rc::new(move || st.detach_pane(&key)),
        });
        let st = store.clone();
        let session_id = id.clone();
        rows.push(Row {
            entry: Entry {
                id: "command:terminate-current".into(),
                label: t!("session.terminate"),
                meta: id.clone(),
                keywords: vec!["terminate current session".into()],
                group: Group::Command,
            },
            run: Rc::new(move || st.request_terminate_session(&session_id)),
        });
        let st = store.clone();
        commands.push((
            "maximize".into(),
            if store.term_zoomed.get() {
                t!("palette.zoomOff")
            } else {
                t!("palette.zoomOn")
            },
            "maximize current pane".into(),
            Box::new(move || st.term_zoomed.set(!st.term_zoomed.get())),
        ));
    }
    let can_quick_open = store.can_open_quick_open();
    commands.retain(|(id, _, _, _)| id != "quick-open" || can_quick_open);
    for (id, label, keywords, run) in commands {
        rows.push(Row {
            entry: Entry {
                id: format!("command:{id}"),
                label,
                meta: String::new(),
                keywords: vec![keywords],
                group: Group::Command,
            },
            run: Rc::from(run),
        });
    }
    let entries: Vec<_> = rows.iter().map(|row| row.entry.clone()).collect();
    let filtered = command_palette::filter(&entries, &query.get());
    let next = selected.get().min(filtered.len().saturating_sub(1));
    selected.set(next);
    let key_rows = rows;
    {
        let mut state = key_state.borrow_mut();
        state.rows = key_rows.clone();
        state.row_refs.clear();
    }
    let selected_down = selected;
    let down_state = key_state.clone();
    let down_query = query;
    let down_input = input_ref.clone();
    bind_safe_key_when("down", false, nsh_core::keymap::When::Always, move || {
        let entries: Vec<_> = down_state
            .borrow()
            .rows
            .iter()
            .map(|row| row.entry.clone())
            .collect();
        let count = command_palette::filter(&entries, &down_query.get()).len();
        selected_down.update(|v| *v = command_palette::move_selection(*v, count, 1));
        let next = selected_down.get();
        if let Some(row) = down_state.borrow().row_refs.get(next).cloned() {
            row.reveal(Default::default());
        }
        down_input.focus();
    });
    let selected_up = selected;
    let up_state = key_state.clone();
    let up_query = query;
    let up_input = input_ref.clone();
    bind_safe_key_when("up", false, nsh_core::keymap::When::Always, move || {
        let entries: Vec<_> = up_state
            .borrow()
            .rows
            .iter()
            .map(|row| row.entry.clone())
            .collect();
        let count = command_palette::filter(&entries, &up_query.get()).len();
        selected_up.update(|v| *v = command_palette::move_selection(*v, count, -1));
        let next = selected_up.get();
        if let Some(row) = up_state.borrow().row_refs.get(next).cloned() {
            row.reveal(Default::default());
        }
        up_input.focus();
    });
    let selected_enter = selected;
    let st_enter = store.clone();
    let enter_state = key_state.clone();
    let enter_query = query;
    bind_safe_key_when("enter", false, nsh_core::keymap::When::Always, move || {
        let run = {
            let state = enter_state.borrow();
            let entries: Vec<_> = state.rows.iter().map(|row| row.entry.clone()).collect();
            let filtered = command_palette::filter(&entries, &enter_query.get());
            let Some(row_idx) = filtered.get(selected_enter.get()).copied() else {
                return;
            };
            let Some(row) = state.rows.get(row_idx) else {
                return;
            };
            row.run.clone()
        };
        if st_enter.active_dialog.get() != ActiveDialog::Palette {
            return;
        }
        st_enter.active_dialog.set(ActiveDialog::None);
        run();
    });
    let mut list = view().flex_col().gap(zpx(3.0)).p(zpx(10.0));
    if filtered.is_empty() {
        list = list.child(
            text(t!("palette.empty"))
                .test_id("palette-empty")
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    let mut previous = None;
    let mut row_refs = Vec::with_capacity(filtered.len());
    for (position, index) in filtered.iter().copied().enumerate() {
        let row = &key_rows[index];
        if previous != Some(row.entry.group) {
            previous = Some(row.entry.group);
            let title = match previous {
                Some(Group::Session) => t!("palette.groupSessions"),
                Some(Group::Project) => t!("palette.groupProjects"),
                _ => t!("palette.groupActions"),
            };
            list = list.child(
                text(title)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        let run = row.run.clone();
        let st = store.clone();
        let is_selected = position == next;
        let test_id = row.entry.id.strip_prefix("project:").map_or_else(
            || format!("palette-entry-{}", row.entry.id),
            |id| format!("palette-project-{id}"),
        );
        let test_id = if row.entry.group == Group::Command && is_selected {
            format!("{test_id}-selected")
        } else {
            test_id
        };
        let row_ref = element_ref();
        let mut item = view()
            .test_id(&test_id)
            .element_ref(&row_ref)
            .flex_row()
            .items_center()
            .justify_between()
            .px(zpx(10.0))
            .py(zpx(6.0))
            .rounded(zpx(6.0))
            .cursor_pointer()
            .on_press(move || {
                st.active_dialog.set(ActiveDialog::None);
                run();
            })
            .child(
                text(row.entry.label.clone())
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground)),
            );
        if !row.entry.meta.is_empty() {
            item = item.child(
                text(row.entry.meta.clone())
                    .text_xs()
                    .font_family(FONT_FAMILY_MONO)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        list = list.child(if is_selected {
            item.bg(rgb_to_hsla(ui.card))
        } else {
            item
        });
        row_refs.push(row_ref);
    }
    key_state.borrow_mut().row_refs = row_refs.clone();
    scroll().flex_1().child(list)
}
