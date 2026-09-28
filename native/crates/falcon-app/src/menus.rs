//! 会话 / 项目 / 主机上的动作，以及溢出菜单的内容：web `lib/useActions.ts` 的原生版。
//! 侧栏的 ＋ / ⋯ / 右键、总览、命令面板、窗口标题栏共用同一份，避免同一个操作各写一遍。
//!
//! 菜单项是数据（[`MenuItemSpec`]），由 [`to_popup`] 画成 gpui-component 的 PopupMenu，
//! 命令面板也直接消费这份数据。

use std::rc::Rc;

use falcon_proto::{Project, SESSION_AGENTS, SessionAgent, SessionState, SessionWithProject, SshHost};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Entity, Window, div};
use rust_i18n::t;

use crate::dialogs::{self, ConfirmItem, ConfirmOpts};
use crate::labels::{idle_text, session_label};
use crate::workspace::{ToastKind, Workspace};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct MenuItemSpec {
    pub label: String,
    pub kbd: Option<&'static str>,
    pub disabled: bool,
    pub checked: Option<bool>,
    pub danger: bool,
    /// 这一项之前画分隔线
    pub separated: bool,
    pub on_select: Handler,
}

impl MenuItemSpec {
    pub fn new(label: impl Into<String>, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self {
            label: label.into(),
            kbd: None,
            disabled: false,
            checked: None,
            danger: false,
            separated: false,
            on_select: Rc::new(f),
        }
    }
    pub fn sep(mut self) -> Self {
        self.separated = true;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn kbd(mut self, k: &'static str) -> Self {
        self.kbd = Some(k);
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn checked(mut self, c: bool) -> Self {
        self.checked = Some(c);
        self
    }
}

pub fn to_popup(mut menu: PopupMenu, items: Vec<MenuItemSpec>) -> PopupMenu {
    for (i, item) in items.into_iter().enumerate() {
        if item.separated && i > 0 {
            menu = menu.separator();
        }
        let handler = item.on_select.clone();
        // PopupMenuItem 的纯文字项既没有危险色也不会把快捷键右对齐，自己画：
        // 危险项（终止、删除）用 destructive 色，与 web 的 Menu 一致
        let (label, kbd, danger) = (item.label.clone(), item.kbd, item.danger);
        let mut entry = PopupMenuItem::element(move |_, cx| {
            let ui = crate::theme::Ui::global(cx);
            div()
                .flex()
                .items_center()
                .gap_4()
                .w_full()
                .child(div().flex_1().when(danger, |d| d.text_color(ui.destructive)).child(label.clone()))
                .when_some(kbd, |d, k| d.child(div().flex_none().text_xs().text_color(ui.muted_foreground).child(k)))
        })
        .disabled(item.disabled)
            .on_click(move |_, window, cx| handler(window, cx));
        if let Some(c) = item.checked {
            entry = entry.checked(c);
        }
        menu = menu.item(entry);
    }
    menu
}

fn toast(ws: &Entity<Workspace>, kind: ToastKind, title: String, body: Option<String>, cx: &mut App) {
    ws.update(cx, |ws, cx| ws.toast(kind, title, body, cx));
}

fn fail(ws: &Entity<Workspace>, err: &falcon_client::ApiError, cx: &mut App) {
    ws.update(cx, |w, cx| {
        w.handle_error(err, cx);
        w.toast(ToastKind::Danger, t!("toast.failed").to_string(), Some(err.to_string()), cx);
    });
}

// ---------------- 会话 ----------------

pub fn reattach(ws: &Entity<Workspace>, session_id: &str, cx: &mut App) {
    let client = ws.read(cx).client.clone();
    let ws = ws.clone();
    let id = session_id.to_string();
    cx.spawn(async move |cx| {
        let result = client.reattach_session(&id).await;
        cx.update(|cx| {
            match result {
                Ok(row) => {
                    ws.update(cx, |w, cx| w.apply_session_state(&row.id, row.state, row.dead_reason, cx));
                    if row.state != SessionState::Active {
                        toast(&ws, ToastKind::Danger, t!("toast.failed").to_string(), Some(t!("session.attachFailed").to_string()), cx);
                    } else {
                        toast(&ws, ToastKind::Success, t!("toast.reattached").to_string(), Some(t!("toast.reattachedBody").to_string()), cx);
                    }
                }
                Err(err) => fail(&ws, &err, cx),
            }
            ws.update(cx, |w, cx| w.refresh_sessions(cx));
        });
    })
    .detach();
}

pub fn terminate(ws: &Entity<Workspace>, session: &SessionWithProject, window: &mut Window, cx: &mut App) {
    let label = session_label(session, &ws.read(cx).projects);
    let ws2 = ws.clone();
    let id = session.id.clone();
    let label2 = label.clone();
    dialogs::confirm(
        ConfirmOpts {
            title: t!("session.terminateTitle", name = label).to_string(),
            body: t!("session.terminateBody").to_string(),
            confirm_label: t!("session.terminateConfirm").to_string(),
            danger: true,
            ..Default::default()
        },
        move |_, cx| ws2.update(cx, |w, cx| w.terminate(&id, &label2, cx)),
        window,
        cx,
    );
}

pub fn terminate_many(ws: &Entity<Workspace>, ids: Vec<String>, window: &mut Window, cx: &mut App) {
    let w = ws.read(cx);
    let picked: Vec<SessionWithProject> = w.sessions.iter().filter(|s| ids.contains(&s.id)).cloned().collect();
    let list = picked
        .iter()
        .map(|s| ConfirmItem {
            name: session_label(s, &w.projects),
            state: Some(s.state),
            meta: Some(idle_text(s.last_active_at)),
        })
        .collect();
    let n = picked.len();
    let ws2 = ws.clone();
    dialogs::confirm(
        ConfirmOpts {
            title: t!("session.terminateManyTitle", n = n).to_string(),
            body: t!("session.terminateManyBody").to_string(),
            list,
            confirm_label: t!("session.terminateManyConfirm").to_string(),
            danger: true,
            ..Default::default()
        },
        move |_, cx| {
            let client = ws2.read(cx).client.clone();
            let ws3 = ws2.clone();
            let ids: Vec<String> = picked.iter().map(|s| s.id.clone()).collect();
            ws2.update(cx, |w, cx| {
                for id in &ids {
                    w.drop_tab(id, cx);
                }
            });
            cx.spawn(async move |cx| {
                for id in &ids {
                    let _ = client.terminate_session(id).await;
                }
                cx.update(|cx| {
                    toast(&ws3, ToastKind::Danger, t!("toast.terminatedMany", n = n).to_string(), None, cx);
                    // 总览里的勾选跟着清掉（web 的 setSelected([])），否则剩下的行还勾着
                    ws3.update(cx, |w, cx| {
                        w.selected.clear();
                        w.refresh_sessions(cx);
                    });
                });
            })
            .detach();
        },
        window,
        cx,
    );
}

/// 清除只删一条记录，没有副作用——不和终止用同一套确认心智
pub fn clear_dead(ws: &Entity<Workspace>, session_id: &str, cx: &mut App) {
    ws.update(cx, |w, cx| w.clear_dead(session_id, cx));
}

pub fn clear_all_dead(ws: &Entity<Workspace>, cx: &mut App) {
    let dead: Vec<String> = ws
        .read(cx)
        .sessions
        .iter()
        .filter(|s| s.state == SessionState::Dead)
        .map(|s| s.id.clone())
        .collect();
    if dead.is_empty() {
        toast(ws, ToastKind::Info, t!("overview.noDead").to_string(), None, cx);
        return;
    }
    let client = ws.read(cx).client.clone();
    let ws = ws.clone();
    let n = dead.len();
    cx.spawn(async move |cx| {
        for id in &dead {
            let _ = client.clear_session(id).await;
        }
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                for id in &dead {
                    w.drop_tab(id, cx);
                }
                w.refresh_sessions(cx);
            });
            toast(&ws, ToastKind::Info, t!("overview.clearedDead", n = n).to_string(), Some(t!("overview.clearedDeadBody").to_string()), cx);
        });
    })
    .detach();
}

pub fn copy_conn(ws: &Entity<Workspace>, project: &Project, cx: &mut App) {
    let w = ws.read(cx);
    let text = falcon_core::host_color::conn_label(Some(project), w.system.as_ref(), &crate::labels::local_word());
    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    toast(ws, ToastKind::Info, t!("toast.copied").to_string(), Some(text), cx);
}

/// 新建会话的菜单：普通终端 + 各家 CLI。after 给了就插在那扇窗口所在列的右边
pub fn new_session_items(ws: &Entity<Workspace>, project_id: &str, after: Option<String>) -> Vec<MenuItemSpec> {
    let mut items = Vec::new();
    {
        let ws = ws.clone();
        let pid = project_id.to_string();
        let after = after.clone();
        items.push(
            MenuItemSpec::new(t!("agent.terminal").to_string(), move |_, cx| {
                ws.update(cx, |w, cx| w.new_terminal(&pid, None, after.clone(), cx))
            })
            .kbd(crate::ui::chord("newTerminal")),
        );
    }
    for (i, agent) in SESSION_AGENTS.iter().enumerate() {
        let ws = ws.clone();
        let pid = project_id.to_string();
        let after = after.clone();
        let agent: SessionAgent = *agent;
        let mut item = MenuItemSpec::new(t!(format!("agent.{}", agent.as_str())).to_string(), move |_, cx| {
            ws.update(cx, |w, cx| w.new_terminal(&pid, Some(agent), after.clone(), cx))
        });
        if i == 0 {
            item = item.sep();
        }
        items.push(item);
    }
    items
}

pub fn session_menu_items(ws: &Entity<Workspace>, session: &SessionWithProject, cx: &App) -> Vec<MenuItemSpec> {
    let w = ws.read(cx);
    let project = w.project(&session.project_id).cloned();
    let mut items = Vec::new();
    let id = session.id.clone();
    if session.state == SessionState::Unverified {
        let ws = ws.clone();
        let id = id.clone();
        items.push(
            MenuItemSpec::new(t!("session.reattachMenu").to_string(), move |_, cx| reattach(&ws, &id, cx))
                .kbd(crate::ui::chord("reattach")),
        );
    }
    if session.state != SessionState::Dead {
        let ws = ws.clone();
        let id = id.clone();
        items.push(MenuItemSpec::new(t!("session.open").to_string(), move |_, cx| {
            ws.update(cx, |w, cx| w.open_session(&id, cx))
        }));
    }
    if let Some(project) = project {
        {
            let ws = ws.clone();
            let p = project.clone();
            items.push(MenuItemSpec::new(t!("session.copyConn").to_string(), move |_, cx| copy_conn(&ws, &p, cx)));
        }
        let ws = ws.clone();
        let pid = project.id.clone();
        items.push(MenuItemSpec::new(t!("session.duplicate").to_string(), move |_, cx| {
            ws.update(cx, |w, cx| w.new_terminal(&pid, None, None, cx))
        }));
    }
    if session.state != SessionState::Dead {
        let ws = ws.clone();
        let id = id.clone();
        items.push(
            MenuItemSpec::new(t!("session.rename").to_string(), move |window, cx| {
                dialogs::rename::open(&ws, &id, window, cx)
            })
            .sep(),
        );
    }
    if session.state == SessionState::Dead {
        let ws = ws.clone();
        let id = id.clone();
        items.push(MenuItemSpec::new(t!("session.clearRecord").to_string(), move |_, cx| clear_dead(&ws, &id, cx)).sep());
    } else {
        let ws = ws.clone();
        let s = session.clone();
        items.push(
            MenuItemSpec::new(t!("session.terminate").to_string(), move |window, cx| terminate(&ws, &s, window, cx))
                .sep()
                .danger(),
        );
    }
    items
}

// ---------------- 项目 ----------------

pub fn project_menu_items(ws: &Entity<Workspace>, project: &Project, cx: &App) -> Vec<MenuItemSpec> {
    let w = ws.read(cx);
    // 已存档：只剩恢复与删除。其余动作（开终端、编辑）都以"项目还在服役"为前提
    if project.worktree.as_ref().is_some_and(|wt| wt.archived_at.is_some()) {
        let (ws1, p1) = (ws.clone(), project.clone());
        let (ws2, p2) = (ws.clone(), project.clone());
        return vec![
            MenuItemSpec::new(t!("worktree.restore").to_string(), move |_, cx| restore_worktree_project(&ws1, &p1, cx)),
            MenuItemSpec::new(t!("worktree.deleteNow").to_string(), move |window, cx| {
                delete_worktree_project(&ws2, &p2, window, cx)
            })
            .sep()
            .danger(),
        ];
    }
    let mut items = new_session_items(ws, &project.id, None);
    // px0 审阅（ADR 0017）：px0 跑在服务端那边，这里只把入口交给系统浏览器——不嵌进
    // WebView，那得把登录 cookie 塞给它。浏览器没登录过时服务端会送去登录再回来。
    // 没有工作目录（多仓库容器、未填目录的 SSH 项目）就没有东西可看，不给入口
    if project.working_dir.as_deref().is_some_and(|d| !d.trim().is_empty()) {
        let url = format!(
            "{}{}",
            w.client.base_url().trim_end_matches('/'),
            falcon_core::px0::px0_base_path(&project.id)
        );
        items.push(MenuItemSpec::new(t!("project.px0Review").to_string(), move |_, cx| cx.open_url(&url)).sep());
    }
    {
        let ws = ws.clone();
        let pid = project.id.clone();
        items.push(
            MenuItemSpec::new(t!("project.filterInOverview").to_string(), move |_, cx| {
                ws.update(cx, |w, cx| {
                    w.overview_project = Some(pid.clone());
                    w.overview_filter = None;
                    w.show_overview(cx);
                })
            })
            .sep(),
        );
    }
    {
        let ws = ws.clone();
        let p = project.clone();
        items.push(
            MenuItemSpec::new(t!("project.edit").to_string(), move |window, cx| {
                dialogs::project_form::open(&ws, Some(p.clone()), Default::default(), window, cx)
            })
            .sep(),
        );
    }
    // 附属项目不能再派生：树永远只有两级，删除级联也就不必递归
    if project.worktree.is_none() {
        {
            let ws = ws.clone();
            let pid = project.id.clone();
            let label = if project.multi.is_some() {
                t!("multi.derive")
            } else {
                t!("project.derive")
            };
            items.push(MenuItemSpec::new(label.to_string(), move |window, cx| {
                dialogs::worktree_form::open(&ws, &pid, Default::default(), window, cx)
            }));
        }
        // 开关是全局的，但入口挂在有存档的源项目上——不然藏起来的东西无处发现
        let archived = w
            .projects
            .iter()
            .filter(|p| {
                p.worktree
                    .as_ref()
                    .is_some_and(|wt| wt.source_project_id == project.id && wt.archived_at.is_some())
            })
            .count();
        if archived > 0 || w.state.show_archived {
            let ws = ws.clone();
            items.push(
                MenuItemSpec::new(t!("worktree.showArchived", n = archived).to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.toggle_show_archived(cx))
                })
                .checked(w.state.show_archived),
            );
        }
    } else {
        let ws = ws.clone();
        let p = project.clone();
        items.push(
            MenuItemSpec::new(t!("worktree.archive").to_string(), move |window, cx| {
                archive_worktree_project(&ws, &p, window, cx)
            })
            .sep(),
        );
    }
    {
        let ws = ws.clone();
        let p = project.clone();
        let label = if project.worktree.is_some() {
            t!("worktree.deleteConfirm")
        } else {
            t!("project.delete")
        };
        items.push(
            MenuItemSpec::new(label.to_string(), move |window, cx| {
                if p.worktree.is_some() {
                    delete_worktree_project(&ws, &p, window, cx)
                } else {
                    delete_project(&ws, &p, window, cx)
                }
            })
            .sep()
            .danger(),
        );
    }
    items
}

fn report_leftovers(ws: &Entity<Workspace>, warnings: Option<Vec<String>>, cx: &mut App) {
    if let Some(w) = warnings.filter(|w| !w.is_empty()) {
        toast(ws, ToastKind::Warning, t!("worktree.leftoverTitle").to_string(), Some(w.join("\n")), cx);
    }
}

fn finish_delete(ws: &Entity<Workspace>, cx: &mut App) {
    ws.update(cx, |w, cx| {
        w.refresh_projects(cx);
        w.refresh_sessions(cx);
        w.refresh_hosts(cx);
    });
}

pub fn delete_project(ws: &Entity<Workspace>, project: &Project, window: &mut Window, cx: &mut App) {
    let w = ws.read(cx);
    // 附属项目连坐：会话数与确认框都要把它们算进去
    let kids: Vec<Project> = w
        .projects
        .iter()
        .filter(|p| p.worktree.as_ref().is_some_and(|wt| wt.source_project_id == project.id))
        .cloned()
        .collect();
    let mut doomed: Vec<&str> = kids.iter().map(|p| p.id.as_str()).collect();
    doomed.push(&project.id);
    let live: Vec<&SessionWithProject> = w
        .sessions
        .iter()
        .filter(|s| doomed.contains(&s.project_id.as_str()) && s.state != SessionState::Dead)
        .collect();
    let mut list: Vec<ConfirmItem> = kids
        .iter()
        .map(|p| ConfirmItem {
            name: p.working_dir.clone().unwrap_or_else(|| p.name.clone()),
            state: None,
            meta: p.worktree.as_ref().map(|wt| wt.branch.clone()),
        })
        .collect();
    list.extend(live.iter().map(|s| ConfirmItem {
        name: session_label(s, &w.projects),
        state: Some(s.state),
        meta: Some(idle_text(s.last_active_at)),
    }));
    let mut foot = Vec::new();
    if !kids.is_empty() {
        foot.push(t!("worktree.cascadeFootnote", n = kids.len()).to_string());
    }
    if project.project_type == falcon_proto::ProjectType::Ssh {
        foot.push(t!("project.deleteFootnote").to_string());
    }
    let n_live = live.len();
    let body = if n_live > 0 {
        t!("project.deleteBody", n = n_live).to_string()
    } else {
        t!("project.deleteBodyEmpty").to_string()
    };
    let ws2 = ws.clone();
    let p = project.clone();
    dialogs::confirm(
        ConfirmOpts {
            title: t!("project.deleteTitle", name = project.name.clone()).to_string(),
            body,
            list,
            footnote: (!foot.is_empty()).then(|| foot.join(" ")),
            confirm_label: t!("project.deleteConfirm").to_string(),
            danger: true,
        },
        move |_, cx| {
            let client = ws2.read(cx).client.clone();
            let ws3 = ws2.clone();
            let p = p.clone();
            cx.spawn(async move |cx| {
                let result = client.delete_project(&p.id, true).await;
                cx.update(|cx| {
                    match result {
                        Ok(res) => {
                            toast(&ws3, ToastKind::Danger, t!("project.deleted", name = p.name.clone()).to_string(), Some(t!("project.deletedBody", n = n_live).to_string()), cx);
                            report_leftovers(&ws3, res.warnings, cx);
                        }
                        Err(err) => fail(&ws3, &err, cx),
                    }
                    finish_delete(&ws3, cx);
                });
            })
            .detach();
        },
        window,
        cx,
    );
}

/// 删除附属项目：先问一次工作区状态，把会丢的东西摆到台面上。预检失败**不阻断**删除，
/// 只是确认框改口说"无法确认"——把"读不到"和"是干净的"混成一个答案才是真会让人丢东西
pub fn delete_worktree_project(ws: &Entity<Workspace>, project: &Project, window: &mut Window, cx: &mut App) {
    let client = ws.read(cx).client.clone();
    let ws = ws.clone();
    let project = project.clone();
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        let status = client.worktree_status(&project.id).await.ok();
        let _ = window_handle.update(cx, |_, window, cx| {
            let w = ws.read(cx);
            let live: Vec<&SessionWithProject> = w
                .sessions
                .iter()
                .filter(|s| s.project_id == project.id && s.state != SessionState::Dead)
                .collect();
            let dir = project.working_dir.clone().unwrap_or_default();
            let branch = project.worktree.as_ref().map(|wt| wt.branch.clone()).unwrap_or_default();
            let clean = status
                .as_ref()
                .is_some_and(|st| st.error.is_none() && st.dirty_count == 0 && st.ignored_count == 0);
            let mut body = vec![if clean {
                t!("worktree.deleteBodyClean", dir = dir.clone(), branch = branch.clone()).to_string()
            } else {
                t!("worktree.deleteBodyDirty", dir = dir.clone(), branch = branch.clone()).to_string()
            }];
            if !live.is_empty() {
                body.push(t!("worktree.deleteBodySessions", n = live.len()).to_string());
            }
            let mut list: Vec<ConfirmItem> = live
                .iter()
                .map(|s| ConfirmItem {
                    name: session_label(s, &w.projects),
                    state: Some(s.state),
                    meta: Some(idle_text(s.last_active_at)),
                })
                .collect();
            if let Some(st) = &status {
                list.extend(st.dirty_sample.iter().map(|f| ConfirmItem {
                    name: f.clone(),
                    state: None,
                    meta: Some(t!("worktree.dirtyFile").to_string()),
                }));
            }
            let mut foot = Vec::new();
            if let Some(m) = &project.multi {
                foot.push(t!("multi.deleteFootnote", n = m.repos.len()).to_string());
            }
            match &status {
                None => foot.push(t!("worktree.deleteStatusUnknown").to_string()),
                Some(st) => {
                    if st.error.is_some() {
                        foot.push(t!("worktree.deleteStatusUnknown").to_string());
                    }
                    if !st.present {
                        foot.push(t!("worktree.deleteMissing").to_string());
                    }
                    if st.dirty_count as usize > st.dirty_sample.len() {
                        foot.push(t!("worktree.deleteDirtyMore", n = st.dirty_count as usize - st.dirty_sample.len()).to_string());
                    }
                    // 被忽略的文件必须单说：.env、本地数据库会跟着一起消失
                    if st.ignored_count > 0 {
                        foot.push(t!("worktree.deleteDirtyIgnored", n = st.ignored_count).to_string());
                    }
                    if st.ahead.unwrap_or(0) > 0 {
                        foot.push(t!("worktree.deleteAhead", n = st.ahead.unwrap_or(0)).to_string());
                    }
                }
            }
            let ws2 = ws.clone();
            let p = project.clone();
            dialogs::confirm(
                ConfirmOpts {
                    title: t!("worktree.deleteTitle", name = project.name.clone()).to_string(),
                    body: body.join(" "),
                    list,
                    footnote: (!foot.is_empty()).then(|| foot.join(" · ")),
                    confirm_label: t!("worktree.deleteConfirm").to_string(),
                    danger: true,
                },
                move |_, cx| {
                    let client = ws2.read(cx).client.clone();
                    let ws3 = ws2.clone();
                    let p = p.clone();
                    cx.spawn(async move |cx| {
                        let result = client.delete_project(&p.id, true).await;
                        cx.update(|cx| {
                            match result {
                                Ok(res) => {
                                    toast(&ws3, ToastKind::Danger, t!("worktree.deleted", name = p.name.clone()).to_string(), p.working_dir.clone(), cx);
                                    report_leftovers(&ws3, res.warnings, cx);
                                }
                                Err(err) => fail(&ws3, &err, cx),
                            }
                            finish_delete(&ws3, cx);
                        });
                    })
                    .detach();
                },
                window,
                cx,
            );
        });
    })
    .detach();
}

/// 存档附属项目：隐藏而不是删除。目录与分支原样保留，到期后端自动清理
pub fn archive_worktree_project(ws: &Entity<Workspace>, project: &Project, window: &mut Window, cx: &mut App) {
    let w = ws.read(cx);
    let live: Vec<&SessionWithProject> = w
        .sessions
        .iter()
        .filter(|s| s.project_id == project.id && s.state != SessionState::Dead)
        .collect();
    let days = falcon_proto::WORKTREE_ARCHIVE_TTL_DAYS;
    let mut body = vec![t!(
        "worktree.archiveBody",
        days = days,
        branch = project.worktree.as_ref().map(|wt| wt.branch.clone()).unwrap_or_default()
    )
    .to_string()];
    if !live.is_empty() {
        body.push(t!("worktree.deleteBodySessions", n = live.len()).to_string());
    }
    let list = live
        .iter()
        .map(|s| ConfirmItem {
            name: session_label(s, &w.projects),
            state: Some(s.state),
            meta: Some(idle_text(s.last_active_at)),
        })
        .collect();
    let ws2 = ws.clone();
    let p = project.clone();
    dialogs::confirm(
        ConfirmOpts {
            title: t!("worktree.archiveTitle", name = project.name.clone()).to_string(),
            body: body.join(" "),
            list,
            footnote: Some(t!("worktree.archiveFootnote", dir = project.working_dir.clone().unwrap_or_default()).to_string()),
            confirm_label: t!("worktree.archiveConfirm").to_string(),
            danger: false,
        },
        move |_, cx| {
            let client = ws2.read(cx).client.clone();
            let ws3 = ws2.clone();
            let p = p.clone();
            cx.spawn(async move |cx| {
                let result = client.archive_project(&p.id).await;
                cx.update(|cx| {
                    match result {
                        Ok(_) => {
                            // 项目从侧栏消失，选中态落回源项目，别让主区停在一个看不见的项目上
                            ws3.update(cx, |w, cx| {
                                if w.state.selected_project_id.as_deref() == Some(p.id.as_str()) {
                                    let src = p.worktree.as_ref().map(|wt| wt.source_project_id.clone());
                                    match src.filter(|s| w.projects.iter().any(|x| &x.id == s)) {
                                        Some(s) => w.select_project(&s, cx),
                                        None => w.show_overview(cx),
                                    }
                                }
                            });
                            toast(&ws3, ToastKind::Info, t!("worktree.archivedToast", name = p.name.clone()).to_string(), Some(t!("worktree.archivedToastBody", days = days).to_string()), cx);
                        }
                        Err(err) => fail(&ws3, &err, cx),
                    }
                    finish_delete(&ws3, cx);
                });
            })
            .detach();
        },
        window,
        cx,
    );
}

/// 恢复只是清掉存档标记，没有副作用——不需要确认框
pub fn restore_worktree_project(ws: &Entity<Workspace>, project: &Project, cx: &mut App) {
    let client = ws.read(cx).client.clone();
    let ws = ws.clone();
    let p = project.clone();
    cx.spawn(async move |cx| {
        let result = client.restore_project(&p.id).await;
        cx.update(|cx| {
            match result {
                Ok(_) => toast(&ws, ToastKind::Success, t!("worktree.restored", name = p.name.clone()).to_string(), None, cx),
                Err(err) => fail(&ws, &err, cx),
            }
            ws.update(cx, |w, cx| w.refresh_projects(cx));
        });
    })
    .detach();
}

// ---------------- 主机 / 服务器行 ----------------

pub fn delete_host(ws: &Entity<Workspace>, host: &SshHost, window: &mut Window, cx: &mut App) {
    if host.project_count > 0 {
        toast(ws, ToastKind::Warning, t!("host.deleteInUse", n = host.project_count).to_string(), None, cx);
        return;
    }
    let ws2 = ws.clone();
    let h = host.clone();
    dialogs::confirm(
        ConfirmOpts {
            title: t!("host.deleteTitle", name = host.name.clone()).to_string(),
            body: t!("host.deleteBody").to_string(),
            confirm_label: t!("host.delete").to_string(),
            danger: true,
            ..Default::default()
        },
        move |_, cx| {
            let client = ws2.read(cx).client.clone();
            let ws3 = ws2.clone();
            let h = h.clone();
            cx.spawn(async move |cx| {
                let result = client.delete_host(&h.id).await;
                cx.update(|cx| {
                    match result {
                        Ok(_) => toast(&ws3, ToastKind::Success, t!("host.deleted", name = h.name.clone()).to_string(), None, cx),
                        Err(err) => fail(&ws3, &err, cx),
                    }
                    ws3.update(cx, |w, cx| w.refresh_hosts(cx));
                });
            })
            .detach();
        },
        window,
        cx,
    );
}

pub fn host_menu_items(ws: &Entity<Workspace>, host: &SshHost) -> Vec<MenuItemSpec> {
    use dialogs::project_form::Preset;
    let (ws1, h1) = (ws.clone(), host.id.clone());
    let (ws2, h2) = (ws.clone(), host.id.clone());
    let (ws3, h3) = (ws.clone(), host.clone());
    let (ws4, h4) = (ws.clone(), host.clone());
    vec![
        MenuItemSpec::new(t!("host.addProject").to_string(), move |window, cx| {
            dialogs::project_form::open(&ws1, None, Preset::ssh(Some(h1.clone()), false), window, cx)
        }),
        // 多仓库项目的成员必须同宿主机，所以入口挂在主机上：位置随主机定死
        MenuItemSpec::new(t!("multi.newProject").to_string(), move |window, cx| {
            dialogs::project_form::open(&ws2, None, Preset::ssh(Some(h2.clone()), true), window, cx)
        }),
        MenuItemSpec::new(t!("host.edit").to_string(), move |window, cx| {
            dialogs::host_form::open(&ws3, Some(h3.clone()), None, window, cx)
        })
        .sep(),
        MenuItemSpec::new(t!("host.delete").to_string(), move |window, cx| delete_host(&ws4, &h4, window, cx))
            .sep()
            .danger(),
    ]
}

/// 侧栏第一层：本机 / 未绑定主机的存量 SSH
pub fn server_menu_items(ws: &Entity<Workspace>, local: bool) -> Vec<MenuItemSpec> {
    use dialogs::project_form::Preset;
    let ws1 = ws.clone();
    let mut items = vec![MenuItemSpec::new(t!("sidebar.newProject").to_string(), move |window, cx| {
        let preset = if local { Preset::local(false) } else { Preset::ssh(None, false) };
        dialogs::project_form::open(&ws1, None, preset, window, cx)
    })];
    // legacy（未绑定主机的存量 SSH）不给：多仓库 v1 不支持手写 ssh 字段
    if local {
        let ws2 = ws.clone();
        items.push(MenuItemSpec::new(t!("multi.newProject").to_string(), move |window, cx| {
            dialogs::project_form::open(&ws2, None, Preset::local(true), window, cx)
        }));
    }
    items
}
