//! 一台 falcon 服务端的窗口：浮动岛骨架（ADR 0011）——窗口底铺 `app`，侧栏 / 主区 / 右面板
//! 是浮在上面的圆角岛，之间只有一道 6px 的缝；右侧活动栏不成岛，图标直接落在窗口底上。
//!
//! 顶上一条 36px 的标题条是原生独有的：放 macOS 的红绿灯（Windows 上是自绘的三颗窗口按钮）、写明这是哪台 falcon 服务端
//! （"身份先于内容"——远处的服务端要让人一眼认出来，别把命令敲错机器），也是拖窗口的地方。

use gpui_kit::component::Root;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, FocusHandle, IntoElement, Render, Subscription, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, point, px, size,
};
use rust_i18n::t;

use crate::actions::*;
use crate::canvas::Canvas;
use crate::dialogs;
use crate::login::LoginView;
use crate::palette;
use crate::profiles::ServerProfile;
use crate::rightbar::{PanelHost, RightBar};
use crate::sidebar::Sidebar;
use crate::theme::{GAP, Ui};
use crate::workspace::{ActiveView, AuthPhase, RightPanelId, Workspace, WorkspaceEvent};
use crate::zoom::zpx;

const TITLE_STRIP: f32 = 36.;

pub fn open_server_window(profile: ServerProfile, cx: &mut App) {
    let title = window_title(&profile);
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx))),
        window_min_size: Some(size(px(820.), px(480.))),
        ..Default::default()
    };
    let mut ws_out = None;
    let result = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ServerWindow::new(profile, window, cx));
        ws_out = Some(view.read(cx).ws.clone());
        cx.new(|cx| Root::new(view, window, cx))
    });
    match result {
        Ok(handle) => {
            crate::snapshot::schedule(handle.into(), cx);
            #[cfg(feature = "automation")]
            if let Some(ws) = ws_out {
                crate::automation::start(handle.into(), ws, cx);
            }
            #[cfg(not(feature = "automation"))]
            let _ = ws_out;
            remember_open_windows(cx);
        }
        Err(err) => log::error!("开窗口失败：{err:#}"),
    }
}

fn window_title(profile: &ServerProfile) -> String {
    if profile.is_local() {
        "Falcon".to_string()
    } else {
        format!("Falcon — {}", display_name(profile))
    }
}

pub fn display_name(profile: &ServerProfile) -> String {
    if profile.is_local() {
        t!("native.server.local").to_string()
    } else if profile.name.trim().is_empty() {
        profile.url.clone()
    } else {
        profile.name.clone()
    }
}

/// 下次启动原样打开这些窗口
fn remember_open_windows(cx: &mut App) {
    let ids: Vec<String> = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<Root>())
        .filter_map(|w| {
            w.read(cx)
                .ok()
                .and_then(|root| root.view().clone().downcast::<ServerWindow>().ok())
                .map(|v| v.read(cx).ws.read(cx).profile.id.clone())
        })
        .collect();
    cx.global_mut::<crate::profiles::Profiles>().set_open(ids);
}

pub struct ServerWindow {
    pub ws: Entity<Workspace>,
    sidebar: Entity<Sidebar>,
    canvas: Entity<Canvas>,
    overview: Entity<dialogs::overview::SessionOverview>,
    rightbar: Entity<RightBar>,
    panels: Entity<PanelHost>,
    login: Entity<LoginView>,
    focus: FocusHandle,
    /// 拖侧栏 / 右面板宽度中（web 的 ResizableSlot）
    panel_resize: Option<PanelDrag>,
    _subs: Vec<Subscription>,
}

struct PanelDrag {
    edge: falcon_core::panel_width::ResizeEdge,
    start_x: f32,
    start_width: f64,
}

impl ServerWindow {
    pub fn new(profile: ServerProfile, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local = profile.is_local();
        let url = profile.url.clone();
        let ws = cx.new(|cx| Workspace::new(profile, cx));
        let sidebar = cx.new(|cx| Sidebar::new(ws.clone(), window, cx));
        let canvas = cx.new(|cx| Canvas::new(ws.clone(), window, cx));
        let overview = cx.new(|cx| dialogs::overview::SessionOverview::new(ws.clone(), window, cx));
        let rightbar = cx.new(|cx| RightBar::new(ws.clone(), window, cx));
        let panels = cx.new(|cx| PanelHost::new(ws.clone(), window, cx));
        let login = cx.new(|cx| LoginView::new(ws.clone(), window, cx));

        let mut subs = vec![
            cx.observe(&ws, |_, _, cx| cx.notify()),
            cx.subscribe_in(&ws, window, Self::on_workspace_event),
            // 系统切明暗：跟随系统时重新派生主题
            cx.observe_window_appearance(window, |_, window, cx| crate::theme::on_window_appearance(window, cx)),
        ];
        subs.push(cx.on_release(|_, cx| remember_open_windows(cx)));

        // 本机服务先确认起来了再初始化（SEA 首次启动要解压 runtime）；远处的直接连
        if local {
            let ws2 = ws.clone();
            cx.spawn(async move |_, cx| {
                let status = cx
                    .background_executor()
                    .spawn(async move { crate::local_service::ensure_running(&url) })
                    .await;
                if let crate::local_service::ServiceStatus::Unreachable(err) = &status {
                    log::warn!("本机服务没起来：{err}");
                }
                cx.update(|cx| ws2.update(cx, |w, cx| w.init(cx)));
            })
            .detach();
        } else {
            ws.update(cx, |w, cx| w.init(cx));
        }

        Self {
            ws,
            sidebar,
            canvas,
            overview,
            rightbar,
            panels,
            login,
            focus: cx.focus_handle(),
            panel_resize: None,
            _subs: subs,
        }
    }

    fn on_workspace_event(&mut self, _: &Entity<Workspace>, ev: &WorkspaceEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            WorkspaceEvent::Toast(toast) => dialogs::show_toast(toast, window, cx),
            WorkspaceEvent::Askpass => dialogs::askpass::open_head(&self.ws, window, cx),
            WorkspaceEvent::FocusPane(key) => {
                let key = key.clone();
                self.canvas.update(cx, |c, cx| c.focus_pane(&key, window, cx));
            }
            WorkspaceEvent::NeedsInstall { project_id, agent } => {
                dialogs::install::open(&self.ws, project_id, Some(*agent), window, cx)
            }
            WorkspaceEvent::Confirm(spec) => {
                let ws = self.ws.clone();
                let f = spec.on_confirm.clone();
                dialogs::confirm(
                    dialogs::ConfirmOpts {
                        title: spec.title.clone(),
                        body: spec.body.clone(),
                        list: Vec::new(),
                        footnote: spec.footnote.clone(),
                        confirm_label: spec.confirm_label.clone(),
                        danger: spec.danger,
                    },
                    move |_, cx| ws.update(cx, |w, cx| f(w, cx)),
                    window,
                    cx,
                );
            }
        }
    }

    fn active_session(&self, cx: &App) -> Option<String> {
        match &self.ws.read(cx).state.active {
            ActiveView::Terminal { session_id: id } => Some(id.clone()),
            _ => None,
        }
    }

    fn render_title_strip(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let profile = &ws.profile;
        let name = display_name(profile);
        // 文字区就是拖动区；Windows 上右边再贴最小化 / 最大化 / 关闭（window_controls.rs），
        // 两者是兄弟而不是嵌套，按钮的命中区不会被拖动区盖住
        let mut strip = div()
            .id("title-strip-drag")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap_2()
            // macOS 留出红绿灯占的位置
            .pl(px(if cfg!(target_os = "macos") { 84. } else { 12. }))
            .pr_3()
            .text_xs()
            .text_color(ui.muted_foreground)
            .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).text_color(ui.foreground).child(name));
        if !profile.is_local() {
            strip = strip.child(div().child(profile.url.clone()));
            if profile.is_insecure() {
                strip = strip.child(div().text_color(ui.warning).child(t!("native.server.plaintext").to_string()));
            }
        }
        div()
            .id("title-strip")
            .h(px(TITLE_STRIP))
            .flex_none()
            .flex()
            .child(crate::window_controls::drag_area(strip))
            .children(crate::window_controls::controls(px(TITLE_STRIP), window, cx))
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use falcon_core::panel_width::ResizeEdge;
        // 窄窗口临时收起侧栏，不写回偏好（web 同一条规则：只影响显示，显式开合会解除它）
        let narrow = window.viewport_size().width < zpx(1024.);
        if self.ws.read(cx).state.sidebar_auto_hidden != narrow {
            self.ws.update(cx, |w, _| w.state.set_sidebar_auto_hidden(narrow));
        }
        let ws = self.ws.read(cx);
        let sidebar_w = zpx(ws.state.sidebar_width as f32);
        let right_w = zpx(ws.state.right_width as f32);
        let show_sidebar = ws.state.sidebar_visible();
        let show_right = ws.state.right_open;
        let overview = ws.state.active == ActiveView::Overview;

        // 侧栏、右侧面板、图标栏走缓存视图：没被 notify 就复用上一帧的布局与绘制。终端刷屏时
        // 只有终端视图在变，不缓存的话整棵树每帧重排——实测 7 行的侧栏每帧就要 3ms 多，
        // 开着历史面板再加近 3ms。它们都订阅了工作区（变了会 notify），主题切换走
        // refresh_windows（无视缓存），所以不会画旧。画布不缓存：终端就在它里面
        let full = || gpui_kit::StyleRefinement::default().size_full();
        let mut main = div().flex().flex_1().min_w_0().min_h_0().pl(GAP).pb(GAP);
        if show_sidebar {
            main = main
                .child(div().flex_none().w(sidebar_w).h_full().child(self.sidebar.clone().cached(full())))
                .child(self.panel_handle("sidebar-resize", ResizeEdge::Right, cx));
        }
        main = main.child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.canvas.clone())
                .when(overview, |d| d.child(div().absolute().inset_0().child(self.overview.clone()))),
        );
        if show_right {
            main = main
                .child(self.panel_handle("right-resize", ResizeEdge::Left, cx))
                .child(div().flex_none().w(right_w).h_full().child(self.panels.clone().cached(full())));
        } else {
            main = main.child(div().flex_none().w(GAP));
        }
        main.child(self.rightbar.clone())
    }

    /// 岛与岛之间那道缝兼作拖宽把手（缝本身就是 GAP 宽，不另占位置）
    fn panel_handle(&self, id: &'static str, edge: falcon_core::panel_width::ResizeEdge, cx: &mut Context<Self>) -> impl IntoElement {
        use falcon_core::panel_width::ResizeEdge;
        let ui = Ui::global(cx).clone();
        let active = self.panel_resize.as_ref().is_some_and(|d| d.edge == edge);
        div()
            .id(id)
            .flex_none()
            .w(GAP)
            .h_full()
            .flex()
            .justify_center()
            .cursor(gpui_kit::CursorStyle::ResizeLeftRight)
            .child(
                div()
                    .w(zpx(2.))
                    .h_full()
                    .my(zpx(12.))
                    .rounded_full()
                    .when(active, |d| d.bg(ui.primary)),
            )
            .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener(move |this, e: &gpui_kit::MouseDownEvent, _, cx| {
                let ws = this.ws.read(cx);
                if e.click_count >= 2 {
                    // 双击回默认宽度
                    this.ws.update(cx, |w, cx| {
                        match edge {
                            ResizeEdge::Right => w.state.set_sidebar_width(falcon_core::panel_width::PANEL_WIDTH_DEFAULT),
                            ResizeEdge::Left => w.state.set_right_width(falcon_core::panel_width::PANEL_WIDTH_DEFAULT),
                        };
                        w.persist();
                        cx.notify();
                    });
                    return;
                }
                let start_width = match edge {
                    ResizeEdge::Right => ws.state.sidebar_width,
                    ResizeEdge::Left => ws.state.right_width,
                };
                this.panel_resize = Some(PanelDrag { edge, start_x: f32::from(e.position.x), start_width });
                cx.stop_propagation();
                cx.notify();
            }))
    }

    fn on_panel_drag(&mut self, e: &gpui_kit::MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        use falcon_core::panel_width::{PanelResize, ResizeEdge, resize_panel_width};
        let Some(d) = &self.panel_resize else {
            return;
        };
        if e.pressed_button != Some(gpui_kit::MouseButton::Left) {
            self.panel_resize = None;
            self.ws.read(cx).persist();
            cx.notify();
            return;
        }
        // 存下来的宽度是缩放前的逻辑像素（渲染时再乘倍数），指针位移要先折回去
        let z = crate::zoom::zoom() as f64;
        let (start_x, x) = (d.start_x as f64 / z, f32::from(e.position.x) as f64 / z);
        let width = resize_panel_width(&PanelResize::new(d.start_width, start_x, x, d.edge));
        let edge = d.edge;
        self.ws.update(cx, |w, cx| {
            let changed = match edge {
                ResizeEdge::Right => w.state.set_sidebar_width(width),
                ResizeEdge::Left => w.state.set_right_width(width),
            };
            if changed {
                cx.notify();
            }
        });
    }

    fn on_panel_drag_end(&mut self, _: &gpui_kit::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.panel_resize.take().is_some() {
            // 拖的途中只改内存，松手才落盘
            self.ws.read(cx).persist();
            cx.notify();
        }
    }
}

impl Render for ServerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let phase = self.ws.read(cx).auth_phase.clone();
        // 缺的终端视图在窗口上下文里补建（Workspace 自己拿不到 Window）
        if phase == AuthPhase::Ready {
            self.ws.update(cx, |w, cx| w.ensure_terminal_views(window, cx));
        }

        let body = match phase {
            AuthPhase::Ready => self.render_main(window, cx).into_any_element(),
            AuthPhase::NeedLogin => div().flex_1().child(self.login.clone()).into_any_element(),
            AuthPhase::Checking => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(ui.muted_foreground)
                .child(t!("native.server.connecting").to_string())
                .into_any_element(),
            AuthPhase::Unreachable(err) => {
                let ws = self.ws.clone();
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .child(t!("native.server.unreachable").to_string())
                    .child(div().text_xs().text_color(ui.muted_foreground).child(err))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("retry-connect")
                                    .primary()
                                    .label(t!("native.server.retry").to_string())
                                    .on_click(move |_, _, cx| ws.update(cx, |w, cx| w.init(cx))),
                            )
                            // Windows 上 set_menus 不出原生菜单栏，又没有本机服务（首次启动"本机"必然连不上），
                            // 不在这里给入口就只能干等重试
                            .child(
                                Button::new("connect-other")
                                    .label(t!("native.menu.connect").to_string())
                                    .on_click(|_, _, cx| crate::dialogs::connect::open_connect_window(cx)),
                            ),
                    )
                    .into_any_element()
            }
        };

        // 焦点落空（刚开窗、关掉最后一扇终端、对话框关闭后没人接）时收回到窗口根上：
        // GPUI 按焦点路径派发动作，没有焦点时 ⌘, / ⌘0 这些挂在根上的动作一个都收不到
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }

        div()
            .id("server-window")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.app)
            .text_color(ui.foreground)
            .font_family(crate::fonts::BERKELEY)
            .text_size(zpx(13.))
            .on_mouse_move(cx.listener(Self::on_panel_drag))
            .on_mouse_up(gpui_kit::MouseButton::Left, cx.listener(Self::on_panel_drag_end))
            .on_action(cx.listener(|this, _: &Palette, window, cx| palette::open(&this.ws, window, cx)))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| dialogs::quick_open::open(&this.ws, window, cx)))
            .on_action(cx.listener(|this, _: &NewTerminal, _, cx| {
                if let Some(pid) = this.ws.read(cx).current_project_id() {
                    this.ws.update(cx, |w, cx| w.new_terminal(&pid, None, None, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                let active = this.ws.read(cx).state.active.clone();
                this.ws.update(cx, |w, cx| match active {
                    ActiveView::Terminal { session_id: id } => w.close_tab(&id, cx),
                    ActiveView::File { .. } => w.close_file(cx),
                    ActiveView::Diff => w.close_diff(cx),
                    _ => {}
                });
            }))
            .on_action(cx.listener(|this, _: &DetachTab, _, cx| {
                if let Some(id) = this.active_session(cx) {
                    this.ws.update(cx, |w, cx| w.detach_tab(&id, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Reattach, _, cx| {
                if let Some(id) = this.active_session(cx) {
                    let unverified = this
                        .ws
                        .read(cx)
                        .session(&id)
                        .is_some_and(|s| s.state == falcon_proto::SessionState::Unverified);
                    if unverified {
                        crate::menus::reattach(&this.ws, &id, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.ws.update(cx, |w, cx| w.toggle_sidebar(cx))))
            .on_action(cx.listener(|this, _: &ToggleFilesPanel, _, cx| this.ws.update(cx, |w, cx| w.toggle_right_panel(RightPanelId::Files, cx))))
            .on_action(cx.listener(|this, _: &ToggleChangesPanel, _, cx| this.ws.update(cx, |w, cx| w.toggle_right_panel(RightPanelId::Changes, cx))))
            .on_action(cx.listener(|this, _: &ToggleGitPanel, _, cx| this.ws.update(cx, |w, cx| w.toggle_right_panel(RightPanelId::Git, cx))))
            .on_action(cx.listener(|this, _: &OpenRelays, window, cx| {
                dialogs::settings::open(&this.ws, dialogs::settings::Tab::Relays, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleMeeglePanel, _, cx| this.ws.update(cx, |w, cx| w.toggle_right_panel(RightPanelId::Meegle, cx))))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.ws.update(cx, |w, cx| w.toggle_term_zoom(cx))))
            .on_action(cx.listener(|this, _: &Overview, _, cx| this.ws.update(cx, |w, cx| w.show_overview(cx))))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.ws.update(cx, |w, cx| w.cycle_tab(1, cx))))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| this.ws.update(cx, |w, cx| w.cycle_tab(-1, cx))))
            .on_action(cx.listener(|this, _: &NextCanvas, _, cx| this.ws.update(cx, |w, cx| w.step_canvas(1, cx))))
            .on_action(cx.listener(|this, _: &PrevCanvas, _, cx| this.ws.update(cx, |w, cx| w.step_canvas(-1, cx))))
            .on_action(cx.listener(|this, _: &Tab1, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(0, cx))))
            .on_action(cx.listener(|this, _: &Tab2, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(1, cx))))
            .on_action(cx.listener(|this, _: &Tab3, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(2, cx))))
            .on_action(cx.listener(|this, _: &Tab4, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(3, cx))))
            .on_action(cx.listener(|this, _: &Tab5, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(4, cx))))
            .on_action(cx.listener(|this, _: &Tab6, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(5, cx))))
            .on_action(cx.listener(|this, _: &Tab7, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(6, cx))))
            .on_action(cx.listener(|this, _: &Tab8, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(7, cx))))
            .on_action(cx.listener(|this, _: &Tab9, _, cx| this.ws.update(cx, |w, cx| w.focus_tab_at(8, cx))))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| dialogs::settings::open(&this.ws, Default::default(), window, cx)))
            .on_action(cx.listener(|this, _: &NewProject, window, cx| dialogs::project_form::open(&this.ws, None, Default::default(), window, cx)))
            .child(self.render_title_strip(window, cx))
            .child(body)
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(
                // 通知必须压在对话框上面（设置里点「检测连接」的结果就是在对话框开着时弹的）。
                // 按绘制顺序排在对话框层后面并不够——实测仍被设置对话框盖住，推迟到最后画才稳
                Root::render_notification_layer(window, cx).map(|layer| gpui_kit::deferred(layer).with_priority(100)),
            )
    }
}
