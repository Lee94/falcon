//! "连接到 falcon 服务端"：管理服务端配置、打开对应的窗口（设计 §4.7）。
//!
//! 独立的小窗口而不是某个服务端窗口里的对话框：它管的是"开哪些窗口"，不属于任何一台服务端。
//! 一台服务端一个窗口，已经开着的就把它拉到前面，不重复开。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Root, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, IntoElement, Render, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, point, px, size,
};
use rust_i18n::t;

use crate::profiles::{Profiles, ServerProfile, new_profile_id};
use crate::theme::Ui;
use crate::zoom::zpx;

pub fn open_connect_window(cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(t!("native.menu.connect").to_string().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(560.), px(520.)), cx))),
        ..Default::default()
    };
    let _ = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ConnectView::new(window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
}

/// 窗口是否已经为这台服务端开着；开着就激活它
fn activate_existing(profile_id: &str, cx: &mut App) -> bool {
    for handle in cx.windows() {
        let Some(root) = handle.downcast::<Root>() else {
            continue;
        };
        let matches = root
            .read(cx)
            .ok()
            .and_then(|r| r.view().clone().downcast::<crate::window::ServerWindow>().ok())
            .is_some_and(|v| v.read(cx).ws.read(cx).profile.id == profile_id);
        if matches {
            let _ = root.update(cx, |_, window, _| window.activate_window());
            return true;
        }
    }
    false
}

pub fn open_profile(profile: ServerProfile, cx: &mut App) {
    if !activate_existing(&profile.id, cx) {
        crate::window::open_server_window(profile, cx);
    }
}

pub struct ConnectView {
    name: Entity<InputState>,
    url: Entity<InputState>,
    error: Option<String>,
}

impl ConnectView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("native.connect.namePlaceholder").to_string()));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://falcon.example.com"));
        Self { name, url, error: None }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().trim_end_matches('/').to_string();
        let parsed = url::Url::parse(&url);
        let ok = parsed.as_ref().is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
        if !ok {
            self.error = Some(t!("native.connect.badUrl").to_string());
            cx.notify();
            return;
        }
        let mut profile = ServerProfile {
            id: new_profile_id(),
            name: self.name.read(cx).value().trim().to_string(),
            url,
            plaintext_ok: false,
        };
        self.error = None;
        cx.notify();
        if !profile.is_insecure() {
            Self::save_and_open(profile, cx);
            return;
        }
        // 非回环的 http://：密码与终端内容裸奔，问过一次、记在配置上（设计 §4.7）
        profile.plaintext_ok = true;
        let body = t!("native.connect.plaintextBody", url = profile.url.as_str()).to_string();
        crate::dialogs::confirm(
            crate::dialogs::ConfirmOpts {
                title: t!("native.connect.plaintextTitle").to_string(),
                body,
                confirm_label: t!("native.connect.plaintextConfirm").to_string(),
                danger: true,
                ..Default::default()
            },
            move |_, cx| Self::save_and_open(profile.clone(), cx),
            window,
            cx,
        );
    }

    fn save_and_open(profile: ServerProfile, cx: &mut App) {
        cx.global_mut::<Profiles>().upsert(profile.clone());
        // 确认框的回调拿不到本视图，改全局后让列表重画
        cx.refresh_windows();
        open_profile(profile, cx);
    }
}

impl Render for ConnectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let profiles: Vec<ServerProfile> = Profiles::global(cx).all().to_vec();
        let mut list = div().flex().flex_col().gap_1();
        for p in profiles {
            let name = crate::window::display_name(&p, cx);
            let pid = p.id.clone();
            let p_open = p.clone();
            let mut row = div()
                .id(SharedString::from(format!("prof-{}", p.id)))
                .h(zpx(36.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(zpx(8.))
                .hover(|s| s.bg(ui.muted))
                .child(crate::ui::icon(if p.is_local() { IconName::Monitor } else { IconName::Server }).size(zpx(14.)).text_color(ui.muted_foreground))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().truncate().child(name))
                    .child(div().truncate().text_size(zpx(11.)).text_color(ui.muted_foreground).child(p.url.clone())))
                .child(
                    Button::new(SharedString::from(format!("prof-open-{}", p.id)))
                        .small()
                        .outline()
                        .label(t!("native.connect.open").to_string())
                        .on_click(move |_, _, cx| open_profile(p_open.clone(), cx)),
                );
            if !p.is_local() {
                row = row.child(
                    Button::new(SharedString::from(format!("prof-del-{}", p.id)))
                        .small()
                        .ghost()
                        .icon(IconName::Trash)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.global_mut::<Profiles>().remove(&pid);
                            cx.notify();
                        })),
                );
            }
            list = list.child(row);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.app)
            .text_color(ui.foreground)
            .font_family(crate::fonts::BERKELEY)
            .text_size(zpx(13.))
            .pt(zpx(40.))
            .px_4()
            .pb_4()
            .gap_3()
            .child(crate::ui::island(cx).p_3().child(list))
            .child(
                crate::ui::island(cx)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(t!("native.connect.add").to_string()))
                    .child(Input::new(&self.name))
                    .child(Input::new(&self.url))
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("native.connect.hint").to_string()))
                    .when_some(self.error.clone(), |d, e| d.child(div().text_xs().text_color(ui.destructive).child(e)))
                    .child(
                        Button::new("prof-add")
                            .primary()
                            .label(t!("native.connect.connect").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            // 顶上 40 的留白在 macOS 是给红绿灯的；Windows 上在这条里铺拖动区 + 三颗窗口按钮
            .when(falcon_platform::get(cx).info().custom_window_controls(), |d| {
                let strip = zpx(40.);
                d.relative().child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(strip)
                        .flex()
                        .child(crate::window_controls::drag_area(div().id("connect-drag").flex_1().h_full(), cx))
                        .children(crate::window_controls::controls(strip, window, cx)),
                )
            })
            // 对话框与通知层由 Root 的插件挂（gpui-component 0.7 起），这里不画，见 window.rs / toasts.rs
    }
}
