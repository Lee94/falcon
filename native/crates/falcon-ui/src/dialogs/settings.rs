//! 设置（旧 React 版的 `components/SettingsModal.tsx`）：外观（明暗模式、窗口底透明效果、浅 / 深两个主题槽位、
//! 应用图标、终端字体 / 字号 / 行高 / 光标）、通知（程序跑完时的提示音）、远端主机、中转（端口转发与公网发布，按机器管理，ADR 0016）、插件（右侧栏插件的
//! 开关与插件目录，ADR 0025）、关于（含应用更新，ADR 0026）。
//!
//! 设置是覆盖层，不是一页：几乎铺满窗口的对话框，左侧按模块切（顶上一个搜索框按名字 /
//! 分组 / 关键字筛模块），右侧是当前模块的项，各自滚动。
//!
//! 各模块是独立的 Entity（输入框、滑块、主题选择器都有状态），打开时一次建好，关掉对话框
//! 一起释放——主题选择器在释放时把预览复原（theme_picker.rs）。例外是中转页：它开着就每 3s
//! 轮询，所以只在切到它时建、切走就丢（沿用 React 版：切 tab 即卸载）。

mod app_icon;
mod appearance;
mod hosts;
mod plugins;
mod relays;
mod widgets;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Context, Entity, FontWeight, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Appearance,
    Notifications,
    Hosts,
    Relays,
    /// 右侧栏插件（ADR 0025）
    Plugins,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    General,
    Connection,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Appearance,
        Tab::Notifications,
        Tab::Plugins,
        Tab::About,
        Tab::Hosts,
        Tab::Relays,
    ];

    fn id(self) -> &'static str {
        match self {
            Tab::Appearance => "appearance",
            Tab::Notifications => "notifications",
            Tab::Hosts => "hosts",
            Tab::Relays => "relays",
            Tab::Plugins => "plugins",
            Tab::About => "about",
        }
    }

    fn group(self) -> Group {
        match self {
            Tab::Hosts | Tab::Relays => Group::Connection,
            _ => Group::General,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Tab::Appearance => IconName::Palette,
            Tab::Notifications => IconName::Bell,
            Tab::Hosts => IconName::Server,
            Tab::Relays => IconName::ArrowLeftRight,
            Tab::Plugins => IconName::Puzzle,
            Tab::About => IconName::Info,
        }
    }

    fn label(self) -> String {
        t!(format!("settings.tab_{}", self.id())).to_string()
    }

    /// 按 id 找页（自动化脚本用）；空串是外观
    #[cfg_attr(not(feature = "automation"), allow(dead_code))]
    pub fn from_id(id: &str) -> Option<Self> {
        if id.is_empty() {
            return Some(Tab::Appearance);
        }
        Tab::ALL.into_iter().find(|t| t.id() == id)
    }
}

impl Group {
    fn label(self) -> String {
        match self {
            Group::General => t!("settings.group_general"),
            Group::Connection => t!("settings.group_connection"),
        }
        .to_string()
    }
}

/// 打开设置，停在 `tab`。已经开着别的对话框（命令面板等）时叠在上面。
pub fn open(ws: &Entity<Workspace>, tab: Tab, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| SettingsView::new(ws.clone(), tab, window, cx));
    let search = view.read(cx).search.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        // 不再铺满窗口：上限 1024×680，小窗口上下左右至少留 32 边距，垂直居中
        let vp = window.viewport_size();
        let w = (vp.width - zpx(64.)).min(zpx(1024.));
        let h = (vp.height - zpx(64.)).min(zpx(680.));
        dialog
            .w(w)
            .h(h)
            .margin_top((vp.height - h) * 0.5)
            .p_0()
            .entrance(DialogEntrance::FadeSlide)
            // 单行输入框里的回车会冒泡成对话框的"确定"：设置没有确定这回事，别让它把整个设置关掉
            .on_ok(|_, _, _| false)
            .child(div().h(h - zpx(2.)).child(view.clone()))
    });
    // 对话框打开时会把焦点收到自己身上：等它收完再交给搜索框（React 版的 data-autofocus）
    window.defer(cx, move |window, cx| search.update(cx, |i, cx| i.focus(window, cx)));
}

struct SettingsView {
    ws: Entity<Workspace>,
    tab: Tab,
    search: Entity<InputState>,
    appearance: Entity<appearance::AppearancePane>,
    hosts: Entity<hosts::HostsPane>,
    /// 只在停在中转页时存在（见模块注释）
    relays: Option<Entity<relays::RelaysPane>>,
    lan_access: Option<falcon_platform::LanAccessState>,
    lan_busy: bool,
}

impl SettingsView {
    fn new(ws: Entity<Workspace>, tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("settings.search").to_string()));
        cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        let updates = crate::app_update::observe(cx);
        cx.observe(&updates, |_, _, cx| cx.notify()).detach();
        // 插件目录是用户手动改的：每次打开设置都重新扫一遍，插件页看到的就是现状
        ws.update(cx, |w, cx| w.refresh_plugins(cx));
        if ws.read(cx).profile.is_loopback()
            && let Some(svc) = falcon_platform::get(cx).local_service()
        {
            cx.spawn(async move |this, cx| {
                let state = cx.background_executor().spawn(async move { svc.lan_access() }).await;
                let _ = this.update(cx, |this, cx| {
                    this.lan_access = state;
                    cx.notify();
                });
            })
            .detach();
        }
        let appearance = cx.new(|cx| appearance::AppearancePane::new(ws.clone(), window, cx));
        let hosts = cx.new(|cx| hosts::HostsPane::new(ws.clone(), cx));
        Self { ws, tab, search, appearance, hosts, relays: None, lan_access: None, lan_busy: false }
    }

    /// 按名字 / 分组名 / 关键字（`settings.tab_*_keys`）筛模块
    fn visible(&self, cx: &App) -> Vec<Tab> {
        let needle = self.search.read(cx).value().trim().to_lowercase();
        Tab::ALL
            .into_iter()
            .filter(|tab| {
                needle.is_empty()
                    || tab.label().to_lowercase().contains(&needle)
                    || tab.group().label().to_lowercase().contains(&needle)
                    || t!(format!("settings.tab_{}_keys", tab.id())).to_lowercase().contains(&needle)
            })
            .collect()
    }

    fn render_nav(&self, visible: &[Tab], active: Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let mut list = div().id("settings-tabs").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if visible.is_empty() {
            list = list.child(
                div().px_2().py_3().text_xs().text_color(ui.muted_foreground).child(t!("settings.empty").to_string()),
            );
        }
        for group in [Group::General, Group::Connection] {
            let items: Vec<Tab> = visible.iter().copied().filter(|t| t.group() == group).collect();
            if items.is_empty() {
                continue;
            }
            let mut block = div().mb_3().flex().flex_col().child(
                div().px_2().pt_1().pb(zpx(6.)).text_size(zpx(11.)).text_color(ui.muted_foreground).child(group.label()),
            );
            for tab in items {
                let on = tab == active;
                let fg = ui.foreground;
                let hover_bg = ui.background.opacity(0.7);
                let b = div()
                    .id(SharedString::from(format!("settings-tab-{}", tab.id())))
                    .h(zpx(32.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded(radius::SM)
                    .text_size(zpx(13.))
                    .cursor_pointer()
                    .map(|d| {
                        if on {
                            d.bg(ui.tint).text_color(ui.tint_foreground).font_weight(FontWeight::MEDIUM)
                        } else {
                            d.text_color(ui.muted_foreground).hover(move |s| s.bg(hover_bg).text_color(fg))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
                    .child(icon(tab.icon()).size(zpx(14.)))
                    .child(tab.label());
                let b = if on {
                    crate::ui::selected_fade(format!("settings-sel-{}", tab.id()), b, ui.tint).into_any_element()
                } else {
                    b.into_any_element()
                };
                block = block.child(b);
            }
            list = list.child(block);
        }
        div()
            .w(zpx(224.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(ui.app.opacity(0.6))
            .px_3()
            .py_4()
            .child(
                div().mb_4().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground)),
                ),
            )
            .child(list)
    }

    /// 程序跑完时的提示音（ADR 0020 决定六）。开关是本机偏好，不跟服务端走：同一台服务端在
    /// 办公室的电脑上想响、在会议室投屏的那台上不想响
    fn render_notifications(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = crate::chime::enabled(cx);
        let toggle = Switch::new("finish-sound")
            .checked(on)
            .on_click(cx.listener(move |_, _, _, cx| {
                crate::chime::set_enabled(!on, cx);
                cx.notify();
            }));
        let preview = Button::new("finish-sound-preview")
            .outline()
            .small()
            .icon(IconName::Volume2)
            .label(t!("settings.finishSoundPreview").to_string())
            .on_click(|_, _, cx| crate::chime::play(cx));
        widgets::section(
            t!("settings.soundTitle").to_string(),
            None,
            widgets::rows(
                vec![
                    (t!("settings.finishSound").to_string(), None, toggle.into_any_element()),
                    (
                        t!("settings.finishSoundPreviewLabel").to_string(),
                        None,
                        preview.into_any_element(),
                    ),
                ],
                false,
                cx,
            ),
            cx,
        )
    }

    fn render_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let mono = |text: String| {
            div().font_family(crate::fonts::BERKELEY).text_sm().text_color(ui.muted_foreground).child(text)
        };
        let dash = "—".to_string();
        let update_phase = crate::app_update::phase(cx);
        let update_hint = crate::app_update::status_hint(&update_phase);
        let update_label = crate::app_update::button_label(&update_phase);
        let update_enabled = crate::app_update::button_enabled(&update_phase);
        let check_update = Button::new("check-update")
            .outline()
            .small()
            .disabled(!update_enabled)
            .label(update_label)
            .on_click(|_, window, cx| crate::app_update::on_about_action(window, cx));
        let ws = self.ws.clone();
        let restart = Button::new("restart-server")
            .outline()
            .small()
            .icon(IconName::RotateCw)
            .label(t!("settings.restartServer").to_string())
            .on_click(move |_, window, cx| {
                let ws = ws.clone();
                crate::dialogs::confirm(
                    crate::dialogs::ConfirmOpts {
                        title: t!("settings.restartConfirmTitle").to_string(),
                        body: t!("settings.restartConfirmBody").to_string(),
                        confirm_label: t!("settings.restartServer").to_string(),
                        danger: true,
                        ..Default::default()
                    },
                    move |_, cx| {
                        ws.update(cx, |w, cx| w.restart_server(cx));
                    },
                    window,
                    cx,
                );
            });
        let mut server_rows = Vec::new();
        if w.profile.is_loopback()
            && let Some(lan) = &self.lan_access
        {
            let enabled = lan.enabled;
            let hint = if !enabled {
                t!("native.settings.lanAccessHintOff", port = lan.port).to_string()
            } else if lan.urls.is_empty() {
                t!("native.settings.lanAccessHintOnNoIp", port = lan.port).to_string()
            } else {
                t!("native.settings.lanAccessHintOn", urls = lan.urls.join(" / ")).to_string()
            };
            let first_url = lan.urls.first().cloned();
            let copy_ws = self.ws.clone();
            let toggle = Switch::new("lan-access-switch")
                .checked(enabled)
                .disabled(self.lan_busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_lan_access(!enabled, cx);
                }));
            let mut right = div().flex().items_center().gap_2();
            if enabled && let Some(url_to_copy) = first_url {
                right = right.child(
                    Button::new("copy-lan-url")
                        .outline()
                        .small()
                        .icon(IconName::Copy)
                        .label(t!("native.settings.lanAccessCopy").to_string())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url_to_copy.clone()));
                            copy_ws.update(cx, |w, cx| {
                                w.toast(ToastKind::Success, t!("plugin.copied").to_string(), None, cx)
                            });
                        }),
                );
            }
            right = right.child(toggle);
            server_rows.push((t!("native.settings.lanAccess").to_string(), Some(hint), right.into_any_element()));
        }
        let ws_update = self.ws.clone();
        server_rows.push((t!("serverUpdate.title").to_string(), Some(w.profile.url.clone()),
            Button::new("server-update-open").outline().small().label(t!("serverUpdate.open").to_string())
                .on_click(move |_, window, cx| crate::dialogs::server_update::open(&ws_update, None, window, cx)).into_any_element()));
        server_rows.push((t!("settings.restartServer").to_string(), None, restart.into_any_element()));

        // React 版这里是"安装应用"（PWA）一行，PWA 已删；换成客户端版本与服务端地址——
        // 原生客户端能连任意一台 falcon 服务端，"版本"指的是连着的那台服务端的
        div()
            .child(widgets::section(
                t!("settings.aboutTitle").to_string(),
                None,
                widgets::rows(
                    vec![
                        (
                            t!("overview.version").to_string(),
                            None,
                            mono(w.system.as_ref().map(|s| format!("v{}", s.version)).unwrap_or_else(|| dash.clone())).into_any_element(),
                        ),
                        (
                            t!("overview.platform").to_string(),
                            None,
                            mono(w.system.as_ref().map(|s| s.platform.clone()).unwrap_or_else(|| dash.clone())).into_any_element(),
                        ),
                        (t!("native.settings.serverUrl").to_string(), None, mono(w.profile.url.clone()).into_any_element()),
                        (
                            t!("native.settings.clientVersion").to_string(),
                            None,
                            mono(format!("v{}", env!("CARGO_PKG_VERSION"))).into_any_element(),
                        ),
                    ],
                    false,
                    cx,
                ),
                cx,
            ))
            .child(widgets::section(
                t!("native.update.title").to_string(),
                Some(t!("native.update.intro").to_string()),
                widgets::rows(
                    vec![(t!("native.update.check").to_string(), update_hint, check_update.into_any_element())],
                    false,
                    cx,
                ),
                cx,
            ))
            .child(widgets::section(
                t!("settings.serverTitle").to_string(),
                None,
                widgets::rows(server_rows, false, cx),
                cx,
            ))
    }

    fn toggle_lan_access(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.lan_busy {
            return;
        }
        let Some(svc) = falcon_platform::get(cx).local_service() else {
            return;
        };
        self.lan_busy = true;
        cx.notify();
        let ws = self.ws.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { svc.set_lan_access(enabled) }).await;
            let _ = this.update(cx, |this, cx| {
                this.lan_busy = false;
                match res {
                    Ok(state) => {
                        let msg = if state.enabled {
                            t!("native.settings.lanAccessEnabled").to_string()
                        } else {
                            t!("native.settings.lanAccessDisabled").to_string()
                        };
                        this.lan_access = Some(state);
                        ws.update(cx, |w, cx| w.toast(ToastKind::Success, msg, None, cx));
                    }
                    Err(err) => {
                        ws.update(cx, |w, cx| {
                            w.toast(ToastKind::Danger, t!("native.settings.lanAccessFailed").to_string(), Some(err), cx)
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl SettingsView {
    fn relays_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<relays::RelaysPane> {
        if let Some(pane) = &self.relays {
            return pane.clone();
        }
        let ws = self.ws.clone();
        let pane = cx.new(|cx| relays::RelaysPane::new(ws, window, cx));
        // 没有主机时的「添加远端主机」：切到远端主机页
        cx.subscribe(&pane, |this, _, ev: &relays::RelaysEvent, cx| match ev {
            relays::RelaysEvent::GoHosts => {
                this.tab = Tab::Hosts;
                cx.notify();
            }
        })
        .detach();
        self.relays = Some(pane.clone());
        pane
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.visible(cx);
        // 筛掉了当前模块就落到第一个还看得见的
        let active = if visible.contains(&self.tab) { self.tab } else { visible.first().copied().unwrap_or(self.tab) };
        if active != Tab::Relays {
            self.relays = None;
        }
        let pane = match active {
            Tab::Appearance => self.appearance.clone().into_any_element(),
            Tab::Notifications => self.render_notifications(cx).into_any_element(),
            Tab::Hosts => self.hosts.clone().into_any_element(),
            Tab::Relays => self.relays_pane(window, cx).into_any_element(),
            Tab::Plugins => plugins::render(&self.ws, cx).into_any_element(),
            Tab::About => self.render_about(cx).into_any_element(),
        };
        let ui = Ui::global(cx).clone();
        div()
            .size_full()
            .flex()
            .rounded(radius::LG)
            .overflow_hidden()
            // 对话框的面在组件库里取 theme.tokens.background，theme.rs 目前只同步了 colors，
            // 换了主题它还停在组件库的默认深 / 浅底上；自己铺一层语义底色，别露出两种底
            .bg(ui.background)
            .child(self.render_nav(&visible, active, cx))
            .child(
                div()
                    .id("settings-pane")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .pl_8()
                    .pr_12()
                    .pt_8()
                    .pb_10()
                    .child(pane),
            )
    }
}
