//! 服务端升级：每个网关地址 / 节点各持有一份状态，关闭对话框不取消升级或重启后的轮询。
use std::collections::HashMap;
use std::time::Duration;

use falcon_client::FalconClient;
use falcon_proto::{ServerUpdateCheck, ServerUpdateTask, SshHost};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Global, IntoElement, Render, Window, div};
use rust_i18n::t;

use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

#[derive(Default)]
struct Updates(HashMap<(String, Option<String>), Entity<ServerUpdate>>);
impl Global for Updates {}

pub fn open(ws: &Entity<Workspace>, host: Option<&SshHost>, window: &mut Window, cx: &mut App) {
    let w = ws.read(cx);
    let client = w.client.clone();
    let host_id = host.map(|h| h.id.clone());
    let label = match host {
        Some(h) => format!("{} · {}@{}:{}", h.name, h.username, h.host, h.port),
        None if w.profile.name.trim().is_empty() => w.profile.url.clone(),
        None => format!("{} · {}", w.profile.name, w.profile.url),
    };
    let key = (client.base_url().to_owned(), host_id.clone());
    if !cx.has_global::<Updates>() {
        cx.set_global(Updates::default());
    }
    let existing = cx.global::<Updates>().0.get(&key).cloned();
    let view = existing.unwrap_or_else(|| {
        let view = cx.new(|_| ServerUpdate {
            client,
            host_id,
            ws: ws.downgrade(),
            label: label.clone(),
            check: None,
            task: None,
            checking: false,
            running: false,
            reconnecting: false,
            error: None,
        });
        cx.global_mut::<Updates>().0.insert(key, view.clone());
        view
    });
    view.update(cx, |v, cx| {
        v.label = label;
        v.ws = ws.downgrade();
        v.refresh(cx);
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("serverUpdate.title").to_string())
            .w(zpx(520.))
            .on_ok(|_, _, _| false)
            .child(view.clone())
    });
}

// 界面按协议字符串解释阶段，不依赖协议枚举的具体变体；未来阶段按进行中处理。
struct TaskState {
    phase: String,
    target_version: Option<String>,
    error: Option<String>,
}
impl From<ServerUpdateTask> for TaskState {
    fn from(task: ServerUpdateTask) -> Self {
        let phase = serde_json::to_value(task.phase)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        Self {
            phase,
            target_version: task.target_version,
            error: task.error,
        }
    }
}

struct ServerUpdate {
    client: FalconClient,
    ws: gpui_kit::WeakEntity<Workspace>,
    host_id: Option<String>,
    label: String,
    check: Option<ServerUpdateCheck>,
    task: Option<TaskState>,
    checking: bool,
    running: bool,
    reconnecting: bool,
    error: Option<String>,
}

fn active(phase: &str) -> bool {
    !matches!(phase, "idle" | "succeeded" | "failed")
}

impl ServerUpdate {
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.checking || self.running {
            return;
        }
        self.checking = true;
        self.error = None;
        let (client, host) = (self.client.clone(), self.host_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            // 先查任务，接续其他客户端或关闭窗口前已经发起的升级；查询失败时不开放安装。
            let status = client.server_update_status(host.as_deref()).await;
            let check = client.check_server_update(host.as_deref()).await;
            let _ = this.update(cx, |v, cx| {
                v.checking = false;
                match status {
                    Ok(task) => {
                        let task = TaskState::from(task);
                        let running = active(&task.phase);
                        v.task = Some(task);
                        match check {
                            Ok(check) => {
                                v.error = check.error.clone();
                                v.check = Some(check);
                            }
                            Err(err) => {
                                v.error = Some(err.message);
                                v.check = None;
                            }
                        }
                        if running {
                            v.running = true;
                            v.poll(cx);
                        }
                    }
                    Err(err) => {
                        v.error = Some(err.message);
                        v.check = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn install(&mut self, version: String, cx: &mut Context<Self>) {
        if self.checking
            || self.running
            || !self.check.as_ref().is_some_and(|c| {
                c.available && c.error.is_none() && c.latest_version.as_deref() == Some(version.as_str())
            })
        {
            return;
        }
        self.running = true;
        self.error = None;
        self.reconnecting = false;
        let (client, host) = (self.client.clone(), self.host_id.clone());
        // 立即固定目标并禁用按钮。请求若在重启时丢失应答，只查询状态，绝不自动重发 POST。
        self.task = Some(TaskState {
            phase: "downloading".into(),
            target_version: Some(version.clone()),
            error: None,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.start_server_update(host.as_deref(), &version).await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(task) => {
                        v.task = Some(task.into());
                    }
                    Err(err)
                        if err
                            .status
                            .is_some_and(|s| (400..500).contains(&s) && s != 409) =>
                    {
                        v.running = false;
                        v.error = Some(err.message);
                        v.check = None;
                        v.task = None;
                        cx.notify();
                        return;
                    }
                    Err(err) => {
                        v.reconnecting = true;
                        v.error = Some(err.message);
                    }
                }
                v.poll(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let (client, host) = (self.client.clone(), self.host_id.clone());
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let status = client
                    .server_update_status(host.as_deref())
                    .await
                    .map(TaskState::from);
                let terminal = status
                    .as_ref()
                    .is_ok_and(|t| matches!(t.phase.as_str(), "succeeded" | "failed"));
                let check = if terminal {
                    Some(client.check_server_update(host.as_deref()).await)
                } else {
                    None
                };
                let keep_polling = this
                    .update(cx, |v, cx| {
                        match status {
                            Ok(task) => {
                                v.reconnecting = false;
                                v.error = None;
                                // 没有任务不能当作成功，也不能永远禁用操作；先停止并要求重新检查，绝不自动重发。
                                if task.phase == "idle" {
                                    v.running = false;
                                    v.task = None;
                                    v.check = None;
                                    v.error = Some(t!("serverUpdate.missingTask").to_string());
                                } else {
                                    v.task = Some(task);
                                }
                                if terminal {
                                    v.running = false;
                                    v.check = None;
                                    if let Some(check) = check {
                                        match check {
                                            Ok(check) => {
                                                v.error = check.error.clone();
                                                v.check = Some(check);
                                            }
                                            Err(err) => v.error = Some(err.message),
                                        }
                                    }
                                    if let Some(ws) = v.ws.upgrade() {
                                        let task =
                                            v.task.as_ref().expect("terminal status has a task");
                                        let succeeded = task.phase == "succeeded";
                                        let version = task
                                            .target_version
                                            .clone()
                                            .unwrap_or_else(|| "—".into());
                                        let message = t!(
                                            if succeeded {
                                                "serverUpdate.phase_succeeded"
                                            } else {
                                                "serverUpdate.phase_failed"
                                            },
                                            version = version
                                        )
                                        .to_string();
                                        let body = format!(
                                            "{}\n{}",
                                            v.label,
                                            task.error.as_deref().unwrap_or(&message)
                                        );
                                        ws.update(cx, |w, cx| {
                                            if v.host_id.is_none()
                                                && let Some(check) = &v.check
                                                && let Some(system) = w.system.as_mut()
                                            {
                                                system.version = check.current_version.clone();
                                            }
                                            // Workspace 的通知事件统一交由 ToastExt 绘制，设置对话框不会盖住它。
                                            w.toast(
                                                if succeeded {
                                                    ToastKind::Success
                                                } else {
                                                    ToastKind::Danger
                                                },
                                                message,
                                                Some(body),
                                                cx,
                                            );
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                            Err(err) => {
                                v.reconnecting = true;
                                v.error = Some(err.message);
                            }
                        }
                        cx.notify();
                        v.running
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for ServerUpdate {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .text_sm()
            .child(div().text_color(ui.foreground).child(self.label.clone()))
            .child(
                div()
                    .text_color(ui.muted_foreground)
                    .child(t!("serverUpdate.intro").to_string()),
            );
        if let Some(check) = &self.check {
            body = body.child(
                div().child(
                    t!(
                        "serverUpdate.versions",
                        current = check.current_version.clone(),
                        latest = check.latest_version.clone().unwrap_or_else(|| "—".into()),
                        platform = check.platform.clone()
                    )
                    .to_string(),
                ),
            );
        }
        if let Some(task) = &self.task {
            let version = task.target_version.clone().unwrap_or_else(|| "—".into());
            body = body.child(
                div()
                    .text_color(if task.phase == "failed" {
                        ui.destructive
                    } else {
                        ui.muted_foreground
                    })
                    .child(
                        t!(
                            format!(
                                "serverUpdate.phase_{}",
                                if matches!(
                                    task.phase.as_str(),
                                    "idle"
                                        | "downloading"
                                        | "verifying"
                                        | "installing"
                                        | "restarting"
                                        | "succeeded"
                                        | "failed"
                                ) {
                                    task.phase.as_str()
                                } else {
                                    "unknown"
                                }
                            ),
                            version = version
                        )
                        .to_string(),
                    ),
            );
            if let Some(error) = &task.error {
                body = body.child(div().text_color(ui.destructive).child(error.clone()));
            }
        }
        if self.reconnecting {
            body = body.child(
                div()
                    .text_color(ui.muted_foreground)
                    .child(t!("serverUpdate.reconnecting").to_string()),
            );
        }
        if let Some(error) = &self.error {
            body = body.child(div().text_color(ui.destructive).child(error.clone()));
        }
        let available = self
            .check
            .as_ref()
            .filter(|c| c.available && c.error.is_none())
            .and_then(|c| c.latest_version.clone());
        if !self.running
            && let Some(version) = available
        {
            let label = self.label.clone();
            body = body.child(
                Button::new("server-update-install")
                    .primary()
                    .small()
                    .disabled(self.checking)
                    .label(t!("serverUpdate.install", version = version.clone()).to_string())
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let entity = cx.entity();
                        let version = version.clone();
                        super::confirm(
                            super::ConfirmOpts {
                                title: t!("serverUpdate.title").to_string(),
                                body: t!(
                                    "serverUpdate.confirm",
                                    target = label.clone(),
                                    version = version.clone()
                                )
                                .to_string(),
                                confirm_label: t!(
                                    "serverUpdate.install",
                                    version = version.clone()
                                )
                                .to_string(),
                                ..Default::default()
                            },
                            move |_, cx| entity.update(cx, |v, cx| v.install(version.clone(), cx)),
                            window,
                            cx,
                        );
                    })),
            );
        }
        body.child(
            Button::new("server-update-check")
                .outline()
                .small()
                .disabled(self.checking || self.running)
                .label(
                    if self.checking {
                        t!("serverUpdate.checking")
                    } else {
                        t!("serverUpdate.check")
                    }
                    .to_string(),
                )
                .on_click(cx.listener(|v, _, _, cx| v.refresh(cx))),
        )
    }
}
