//! 工作区的动作。状态转换一律调 [`falcon_core::workspace::WorkspaceState`]（store.ts 同名动作的
//! 移植），这里只补 app 层的事：发请求、落盘、通知视图、把焦点交给对应窗口。
//!
//! Detach / Terminate 的区分照 CONTEXT.md：手动关终端窗口 = Terminate（前台有程序在跑先确认）；
//! Shift+关闭 = Detach；离开 / 退出 App 永远不是 Terminate。

use std::rc::Rc;
use std::time::Duration;

use falcon_core::layout::DropSpot;
use falcon_core::pane_key::{self, PaneItem};
use falcon_proto::{GitFileChange, SessionAgent, SessionState};
use gpui_kit::Context;
use rust_i18n::t;

use super::{
    ActiveView, DiffCommit, DiffTabTarget, FileTabTarget, RightPanelId, ToastKind, Workspace,
    WorkspaceEvent, is_pending_id,
};

/// 要用户确认的动作（窗口收到后开确认框，确认了再回调）
pub struct ConfirmSpec {
    pub title: String,
    pub body: String,
    pub footnote: Option<String>,
    pub confirm_label: String,
    pub danger: bool,
    pub on_confirm: Rc<dyn Fn(&mut Workspace, &mut Context<Workspace>)>,
}

impl Workspace {
    /// 动作之后的统一收尾：落盘 + 让焦点跟着 active 走 + 重画
    fn commit(&mut self, focus: bool, cx: &mut Context<Self>) {
        self.sync_terminals();
        self.persist();
        if focus && let Some(key) = falcon_core::workspace::active_key(&self.state.active) {
            cx.emit(WorkspaceEvent::FocusPane(key));
        }
        cx.notify();
    }

    // ---------------- 选择与焦点 ----------------

    pub fn open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.state.open_session(session_id, &self.sessions);
        self.commit(true, cx);
    }

    /// 把输入焦点交给某扇窗口（点标题栏 / 点进画布）
    pub fn focus_pane(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.state.focus_pane(key) {
            self.persist();
            cx.notify();
        }
    }

    pub fn select_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        self.state.select_project(project_id, &self.projects, &self.sessions);
        self.commit(true, cx);
    }

    pub fn show_overview(&mut self, cx: &mut Context<Self>) {
        self.state.show_overview();
        self.commit(false, cx);
    }

    pub fn focus_tab_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.state.focus_tab_at(index, &self.sessions) {
            self.commit(true, cx);
        }
    }

    pub fn cycle_tab(&mut self, delta: i64, cx: &mut Context<Self>) {
        if self.state.cycle_tab(delta, &self.sessions) {
            self.commit(true, cx);
        }
    }

    // ---------------- 文件 / 差异窗口 ----------------

    pub fn open_file(&mut self, project_id: &str, path: &str, cx: &mut Context<Self>) {
        self.state.open_file(project_id, path, &self.sessions);
        self.commit(true, cx);
    }

    pub fn close_file(&mut self, cx: &mut Context<Self>) {
        self.state.close_file(None, &self.sessions);
        self.commit(false, cx);
    }

    pub fn open_diff(
        &mut self,
        project_id: &str,
        file: GitFileChange,
        commit: Option<DiffCommit>,
        repo: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.state.open_diff(DiffTabTarget {
            project_id: project_id.to_string(),
            file,
            commit,
            repo,
        }, &self.sessions);
        self.commit(true, cx);
    }

    pub fn close_diff(&mut self, cx: &mut Context<Self>) {
        self.state.close_diff(&self.sessions);
        self.commit(false, cx);
    }

    pub fn file_tab(&self) -> Option<&FileTabTarget> {
        self.state.file_tab.as_ref()
    }

    // ---------------- 关窗口：Terminate / Detach ----------------

    /// 只把窗口摘掉，不碰会话
    pub fn drop_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.drop_tab(id, &self.sessions);
        self.commit(false, cx);
    }

    /// 手动关终端窗口 = Terminate。前台真有程序在跑时先确认一次；侦测失败或 2s 超时按空闲
    /// 处理——它是一道保险，自己坏了不能挡住关闭（SSH 上探测要过一次网络往返）。
    pub fn close_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        let session = if is_pending_id(id) { None } else { self.session(id).cloned() };
        // pending 还没有后端 id；dead 会话没什么可杀的，记录留着等用户自己清
        let Some(session) = session.filter(|s| s.state != SessionState::Dead) else {
            self.drop_tab(id, cx);
            return;
        };
        let client = self.client.clone();
        let sid = session.id.clone();
        let label = crate::labels::session_label(&session, &self.projects);
        cx.spawn(async move |this, cx| {
            let probe = futures::FutureExt::fuse(client.session_foreground(&sid));
            let timeout = futures::FutureExt::fuse(cx.background_executor().timer(Duration::from_secs(2)));
            futures::pin_mut!(probe, timeout);
            let fg = futures::select_biased! {
                fg = probe => fg.ok(),
                _ = timeout => None,
            };
            this.update(cx, |this, cx| {
                let busy = fg.as_ref().is_some_and(|f| f.busy);
                if !busy {
                    this.terminate(&sid, &label, cx);
                    return;
                }
                let command = fg.and_then(|f| f.command).unwrap_or_default();
                let (sid2, label2) = (sid.clone(), label.clone());
                cx.emit(WorkspaceEvent::Confirm(ConfirmSpec {
                    title: t!("tab.busyTitle", name = label).to_string(),
                    body: t!("tab.busyBody", command = command).to_string(),
                    footnote: Some(t!("tab.busyFootnote").to_string()),
                    confirm_label: t!("session.terminateConfirm").to_string(),
                    danger: true,
                    on_confirm: Rc::new(move |ws, cx| ws.terminate(&sid2, &label2, cx)),
                }));
            })
            .ok();
        })
        .detach();
    }

    pub fn terminate(&mut self, id: &str, label: &str, cx: &mut Context<Self>) {
        self.drop_tab(id, cx);
        let client = self.client.clone();
        let (id, label) = (id.to_string(), label.to_string());
        cx.spawn(async move |this, cx| {
            let result = client.terminate_session(&id).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.toast(ToastKind::Danger, t!("toast.terminated", name = label).to_string(), None, cx),
                    Err(err) => {
                        this.handle_error(&err, cx);
                        this.toast(ToastKind::Danger, t!("toast.failed").to_string(), Some(err.to_string()), cx);
                    }
                }
                this.refresh_sessions(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Detach：只收起窗口，会话留在后台继续跑（Shift+关闭）
    pub fn detach_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        let session = if is_pending_id(id) { None } else { self.session(id).cloned() };
        self.drop_tab(id, cx);
        if let Some(s) = session.filter(|s| s.state != SessionState::Dead) {
            let label = crate::labels::session_label(&s, &self.projects);
            self.toast(
                ToastKind::Info,
                t!("toast.detachTitle", name = label).to_string(),
                Some(t!("toast.detachBody").to_string()),
                cx,
            );
        }
    }

    /// 关掉一组窗口（关闭其他 / 左 / 右）。文件和差异立刻收起；终端走 Terminate
    pub fn close_pane_keys(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        for key in keys {
            match pane_key::parse_pane_key(&key) {
                Some(PaneItem::Terminal { id, .. }) => self.close_tab(&id, cx),
                Some(PaneItem::File { .. }) => self.close_file(cx),
                Some(PaneItem::Diff { .. }) => self.close_diff(cx),
                None => {}
            }
        }
    }

    pub fn clear_dead(&mut self, id: &str, cx: &mut Context<Self>) {
        self.drop_tab(id, cx);
        let client = self.client.clone();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = client.clear_session(&id).await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.handle_error(&err, cx);
                }
                this.refresh_sessions(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn rename_session(&mut self, id: &str, name: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = client.rename_session(&id, &name).await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.handle_error(&err, cx);
                }
                this.refresh_sessions(cx);
            })
            .ok();
        })
        .detach();
    }

    // ---------------- 拖拽与尺寸 ----------------

    pub fn move_pane(&mut self, key: &str, spot: DropSpot, cx: &mut Context<Self>) {
        // 落点是在**可见**列上量出来的，state 里先翻成全量坐标再落；拖了等于没拖就不改
        if self.state.move_pane(key, spot, &self.sessions) {
            self.persist();
            cx.notify();
        }
    }

    pub fn toggle_pin_pane(&mut self, key: &str, cx: &mut Context<Self>) {
        self.state.toggle_pin_pane(key);
        self.commit(false, cx);
    }

    /// 把一扇窗口挪到另一块画布（`None` = 新开一块，紧跟在当前画布后面）。焦点跟着它走
    pub fn move_pane_to_canvas(&mut self, key: &str, canvas: Option<&str>, cx: &mut Context<Self>) {
        if self.state.move_pane_to_canvas(key, canvas, &self.sessions) {
            self.commit(true, cx);
        }
    }

    /// 切到某块画布（画布条上点数字）：焦点交给它上次停的那扇窗口
    pub fn show_canvas(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.state.show_canvas(id, &self.sessions) {
            self.commit(true, cx);
        }
    }

    /// 切到左 / 右一块画布（横向手势、快捷键），不回绕
    pub fn step_canvas(&mut self, delta: i64, cx: &mut Context<Self>) {
        if self.state.step_canvas(delta, &self.sessions) {
            self.commit(true, cx);
        }
    }

    /// 画布量到的宽：新列排不排得下按它算。当场就把还没分的列分好，不等通知之后的
    /// observe_self——宽是画布在 prepaint 里报上来的，同一帧接着排版就该用分好的画布
    pub fn set_canvas_width(&mut self, width: f64, cx: &mut Context<Self>) {
        if self.state.set_canvas_width(width) {
            self.settle_canvases(cx);
            cx.notify();
        }
    }

    /// 拖的途中不落盘，松手时由调用方 `persist()`
    pub fn set_column_width(&mut self, id: &str, width: Option<f64>, cx: &mut Context<Self>) {
        self.state.set_column_width(id, width);
        cx.notify();
    }

    pub fn set_pane_height(&mut self, key: &str, height: Option<f64>, cx: &mut Context<Self>) {
        self.state.set_pane_height(key, height);
        cx.notify();
    }

    // ---------------- 侧栏 / 右侧栏 ----------------

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        self.commit(false, cx);
    }

    /// 点同一格再关；点另一格则切过去
    pub fn toggle_right_panel(&mut self, panel: RightPanelId, cx: &mut Context<Self>) {
        self.state.toggle_right_panel(panel);
        self.commit(false, cx);
    }

    pub fn toggle_term_zoom(&mut self, cx: &mut Context<Self>) {
        self.state.toggle_term_zoom();
        self.commit(false, cx);
    }

    pub fn toggle_collapsed(&mut self, key: &str, cx: &mut Context<Self>) {
        self.state.toggle_collapsed(key);
        self.commit(false, cx);
    }

    pub fn toggle_sessions(&mut self, project_id: &str, cx: &mut Context<Self>) {
        self.state.toggle_sessions(project_id);
        self.commit(false, cx);
    }

    pub fn toggle_show_archived(&mut self, cx: &mut Context<Self>) {
        self.state.toggle_show_archived();
        self.commit(false, cx);
    }

    pub fn set_multi_repo(&mut self, project_id: &str, dir: &str, cx: &mut Context<Self>) {
        self.state.set_multi_repo(project_id, dir);
        cx.notify();
    }

    // ---------------- 新建会话 ----------------

    /// 新建终端。SSH 项目首次用时先走授权 + 安装；已拒绝过的主机不再打扰，直接建非持久会话
    pub fn new_terminal(&mut self, project_id: &str, agent: Option<SessionAgent>, after: Option<String>, cx: &mut Context<Self>) {
        let Some(project) = self.project(project_id).cloned() else {
            return;
        };
        if project.project_type == falcon_proto::ProjectType::Ssh {
            let client = self.client.clone();
            let pid = project_id.to_string();
            cx.spawn(async move |this, cx| {
                let status = client.host_status(&pid).await;
                this.update(cx, |this, cx| {
                    let needs_setup = status.as_ref().is_ok_and(|s| {
                        s.authorized.is_none() || (s.authorized == Some(true) && s.installed_version.is_none())
                    });
                    if needs_setup {
                        cx.emit(WorkspaceEvent::NeedsInstall { project_id: pid.clone(), agent });
                    } else {
                        // 查不到主机状态就照常建会话，由后端判定持久性
                        this.create_session_now(&pid, agent, after.clone(), cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        self.create_session_now(project_id, agent, after, cx);
    }

    pub fn create_session_now(&mut self, project_id: &str, agent: Option<SessionAgent>, after: Option<String>, cx: &mut Context<Self>) {
        let pending_id = self.state.begin_pending(project_id, agent, after.as_deref(), &self.sessions);
        self.commit(false, cx);
        self.spawn_create(pending_id, project_id.to_string(), agent, cx);
    }

    pub fn retry_pending(&mut self, pending_id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.state.retry_pending(pending_id) else {
            return;
        };
        cx.notify();
        self.spawn_create(pending_id.to_string(), entry.project_id, entry.agent, cx);
    }

    fn spawn_create(&mut self, pending_id: String, project_id: String, agent: Option<SessionAgent>, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let hint = crate::theme::TerminalLook::global(cx).appearance.clone();
        cx.spawn(async move |this, cx| {
            let request = falcon_proto::CreateSessionRequest {
                name: None,
                agent,
                appearance: hint.appearance,
                background: hint.background.clone(),
                foreground: hint.foreground.clone(),
            };
            let result = client.create_session(&project_id, &request).await;
            let sessions = client.list_sessions().await;
            this.update(cx, |this, cx| match result {
                Ok(session) => {
                    if let Ok(list) = sessions {
                        this.sessions = list;
                    }
                    let was_active = this.state.active == ActiveView::Terminal { session_id: pending_id.clone() };
                    this.state.resolve_pending(&pending_id, &session.id);
                    this.state.apply_sessions(&this.sessions);
                    this.commit(was_active, cx);
                }
                Err(err) => {
                    this.handle_error(&err, cx);
                    this.state.fail_pending(&pending_id, &err.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
