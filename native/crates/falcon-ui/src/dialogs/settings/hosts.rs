//! 远端主机：已保存的 SSH 连接配置列表 + 检测连接 / 编辑 / 删除 / 添加。web 的 HostsPane +
//! HostList。表单本身在 `dialogs::host_form`，删除（含"被项目引用时拒绝"）在 `menus::delete_host`。

use falcon_core::host_color::ssh_conn;
use falcon_proto::{HostKind, SshAuthMethod, SshHost, SshProbeResult};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, FontWeight, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use super::widgets::section;
use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

fn auth_label(method: SshAuthMethod) -> String {
    match method {
        SshAuthMethod::Key => t!("project.authKey"),
        SshAuthMethod::Password => t!("project.authPassword"),
        _ => t!("project.authAgent"),
    }
    .to_string()
}

pub struct HostsPane {
    ws: Entity<Workspace>,
    /// 正在检测的那台（按钮换成"正在连接…"并禁用）
    testing: Option<String>,
}

impl HostsPane {
    pub fn new(ws: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        // 打开设置时顺手刷一遍：别的窗口 / web 端刚加的主机、引用计数都要是新的
        ws.update(cx, |w, cx| w.refresh_hosts(cx));
        Self { ws, testing: None }
    }

    fn test(&mut self, host: &SshHost, cx: &mut Context<Self>) {
        self.testing = Some(host.id.clone());
        cx.notify();
        let client = self.ws.read(cx).client.clone();
        let id = host.id.clone();
        cx.spawn(async move |this, cx| {
            let result = client.test_host(&id).await;
            this.update(cx, |this, cx| {
                this.testing = None;
                let (kind, title, body) = match result {
                    Ok(SshProbeResult::Reachable { kind, home }) => {
                        let kind = match kind {
                            HostKind::Windows => t!("host.testKind_windows"),
                            _ => t!("host.testKind_posix"),
                        };
                        (ToastKind::Success, t!("host.testOk"), t!("host.testOkBody", kind = kind, home = home).to_string())
                    }
                    Ok(SshProbeResult::Failed { error }) => (ToastKind::Danger, t!("host.testFail"), error),
                    Err(err) => (ToastKind::Danger, t!("host.testFail"), err.to_string()),
                };
                this.ws.update(cx, |w, cx| w.toast(kind, title.to_string(), Some(body), cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for HostsPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let hosts = self.ws.read(cx).hosts.clone();
        let ws = self.ws.clone();

        let body = if hosts.is_empty() {
            crate::ui::sunken(cx)
                .mb_3()
                .px_4()
                .py_6()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(t!("host.empty").to_string())
                .into_any_element()
        } else {
            // 列宽：名称 / 连接 / 认证三列按比例，操作列按内容
            let head = |text: String| div().flex_1().min_w_0().truncate().child(text);
            let mut table = crate::ui::sunken(cx).mb_3().flex().flex_col().px_2().child(
                div()
                    .h(zpx(40.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .border_b_1()
                    .border_color(ui.border)
                    .child(head(t!("host.name").to_string()))
                    .child(head(t!("host.conn").to_string()))
                    .child(head(t!("host.auth").to_string()))
                    .child(div().w(zpx(224.))),
            );
            let n = hosts.len();
            for (i, host) in hosts.into_iter().enumerate() {
                let testing = self.testing.as_deref() == Some(host.id.as_str());
                let used = if host.project_count > 0 {
                    t!("host.usedBy", n = host.project_count).to_string()
                } else {
                    t!("host.unused").to_string()
                };
                let (h_test, h_edit, h_del) = (host.clone(), host.clone(), host.clone());
                let (ws_edit, ws_del) = (ws.clone(), ws.clone());
                table = table.child(
                    div()
                        .id(SharedString::from(format!("host-row-{}", host.id)))
                        .min_h(zpx(44.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .rounded_md()
                        .when(i + 1 < n, |d| d.border_b_1().border_color(ui.border.opacity(0.5)))
                        .hover(|s| s.bg(ui.background.opacity(0.7)))
                        .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(host.name.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::fonts::BERKELEY)
                                .text_xs()
                                .text_color(ui.muted_foreground)
                                .child(ssh_conn(&host)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .flex()
                                .text_xs()
                                .text_color(ui.muted_foreground)
                                .child(auth_label(host.auth_method))
                                .child(div().text_color(ui.muted_foreground.opacity(0.4)).child(" · "))
                                .child(used),
                        )
                        .child(
                            div()
                                .w(zpx(224.))
                                .flex()
                                .justify_end()
                                .gap(zpx(6.))
                                .child(
                                    Button::new(SharedString::from(format!("host-test-{}", host.id)))
                                        .ghost()
                                        .small()
                                        .disabled(testing)
                                        .label(if testing { t!("host.testing") } else { t!("host.test") }.to_string())
                                        .on_click(cx.listener(move |this, _, _, cx| this.test(&h_test, cx))),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("host-edit-{}", host.id)))
                                        .ghost()
                                        .small()
                                        .label(t!("host.edit").to_string())
                                        .on_click(move |_, window, cx| {
                                            crate::dialogs::host_form::open(&ws_edit, Some(h_edit.clone()), None, window, cx)
                                        }),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("host-del-{}", host.id)))
                                        .ghost()
                                        .small()
                                        .label(t!("host.delete").to_string())
                                        .on_click(move |_, window, cx| crate::menus::delete_host(&ws_del, &h_del, window, cx)),
                                ),
                        ),
                );
            }
            table.into_any_element()
        };
        let ws_add = ws.clone();
        let add = Button::new("host-add")
            .outline()
            .small()
            .label(t!("host.add").to_string())
            .on_click(move |_, window, cx| crate::dialogs::host_form::open(&ws_add, None, None, window, cx));
        section(
            t!("host.title").to_string(),
            Some(t!("host.hint").to_string()),
            vec![body, div().flex().child(add).into_any_element()],
            cx,
        )
    }
}
