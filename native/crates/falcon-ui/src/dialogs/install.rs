//! Zellij 授权 / 安装（web 的 `components/ZellijInstallModal.tsx`，ADR 0001）。
//!
//! SSH 项目第一次在某台远端主机上建会话前：先问一次授权（会往用户的服务器写可执行文件，
//! 授权按 host + port + username 记），同意后经 `/ws/install/:projectId` 看宿主机自己下载、解压、
//! 验证的进度；瞬时故障后端自动重试 3 次，稳定的环境事实（noexec、缺 curl）直接给重试按钮。
//! 拒绝则直接建非持久会话。`then_create` 带着开场 agent：装完（或拒绝 / 跳过）后接着把会话建出来
//! （web 的 `finish(create)`）；为 None 时（命令面板「为某主机启用持久会话」）只装不建。
//!
//! 授权是"允许 falcon 往这台服务器写可执行文件"的决定，该问；安装是随后的执行过程，只报进度。
//! 失败态里能直接改下载地址再重试——后端刚刚已经用旧地址自动试过 3 次了，不给改地址的话
//! 那个「重试」按钮几乎注定再失败一轮。

use falcon_client::{HostAuthorizationPatch, InstallEvent, InstallSocket};
use falcon_core::reason::reason_text;
use falcon_proto::{
    HostZellijStatus, InstallServerMessage, NonDurableReason, SessionAgent, SessionState, ZellijInstallStage,
    can_retry_install,
};
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::Select;
use gpui_kit::component::{Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClipboardItem, Context, Entity, IntoElement, Render, ScrollHandle, Task, Window, div,
};
use rust_i18n::t;

use crate::dialogs::project_form::kit::{self, OptState, Opt, Options, Tone, Tr, disclosure, error_line, field, note};
use crate::theme::{Ui, radius};
use crate::toasts::ToastExt;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

/// Zellij 官方发行的 target 三元组，与后端 version.ts 保持一致
const TARGETS: [&str; 5] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

/// 只有这几种失败换个下载源才有用；其余的显示输入框只会误导
const URL_FIXABLE: [NonDurableReason; 3] =
    [NonDurableReason::DownloadFailed, NonDurableReason::ExtractFailed, NonDurableReason::ProbeFailed];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ask,
    Installing,
    Failed,
}

pub struct InstallView {
    ws: Entity<Workspace>,
    project_id: String,
    host: String,
    then_create: Option<Option<SessionAgent>>,
    phase: Phase,
    status: HostZellijStatus,
    /// 下载地址：授权页与失败页共用同一个值（web 同一个 state）
    base_url: Entity<InputState>,
    saved_url: String,
    stage: ZellijInstallStage,
    commands: Vec<String>,
    failure: Option<NonDurableReason>,
    detail: Option<String>,
    /// 后端本轮自动重试到第几次；>1 时说明"还在转"是因为在重试，而不是卡住了
    attempt: u32,
    /// 这一轮后端已经报过结果：之后通道的断开只是收尾，不能拿它盖掉真正的结果
    settled: bool,
    show_detail: bool,
    show_manual: bool,
    copied: bool,
    target: Entity<OptState>,
    log_scroll: ScrollHandle,
    /// 读安装通道的任务。丢掉 = 关掉 socket = 服务端取消这次安装
    install: Option<Task<()>>,
    /// 授权 / 改下载源的请求失败（web 没接这个错，这里就地显示）
    error: Option<String>,
    finished: bool,
}

pub fn open(
    ws: &Entity<Workspace>,
    project_id: &str,
    then_create: Option<Option<SessionAgent>>,
    window: &mut Window,
    cx: &mut App,
) {
    let request = ws.read(cx).client.host_status(project_id);
    let (ws, pid) = (ws.clone(), project_id.to_string());
    window
        .spawn(cx, async move |cx| {
            let status = request.await;
            cx.update(|window, cx| match status {
                Ok(status) => open_view(ws, pid, then_create, status, window, cx),
                // 查不到主机状态：照常建会话，由后端判定持久性（web 的 skip("verify-failed")）
                Err(err) => ws.update(cx, |w, cx| {
                    w.handle_error(&err, cx);
                    if let Some(agent) = then_create {
                        w.create_session_now(&pid, agent, None, cx);
                    }
                }),
            })
            .ok();
        })
        .detach();
}

fn open_view(
    ws: Entity<Workspace>,
    pid: String,
    then_create: Option<Option<SessionAgent>>,
    status: HostZellijStatus,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(project) = ws.read(cx).project(&pid).cloned() else { return };
    let host = project.ssh.as_ref().map(|s| s.host.clone()).unwrap_or(project.name.clone());
    let view = cx.new(|cx| InstallView::new(ws, pid, host, then_create, status, window, cx));
    let v = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let (title, installing) = {
            let s = v.read(cx);
            let title = if s.phase == Phase::Failed {
                t!("zellij.failTitle", host = s.host.clone())
            } else {
                t!("zellij.enableTitle", host = s.host.clone())
            };
            (title.to_string(), s.phase == Phase::Installing)
        };
        let (ok, cancel) = (v.clone(), v.clone());
        dialog
            .title(title)
            .w(zpx(576.))
            // 安装进行中锁住遮罩（web 的 lockOverlay 只在 installing 时开）
            .overlay_closable(!installing)
            .close_button(!installing)
            .child(v.clone())
            .on_ok(move |_, window, cx| {
                ok.update(cx, |this, cx| this.primary(window, cx));
                false
            })
            // 关掉 = 跳过：本次不装，回到用户本来想干的事
            .on_cancel(move |_, window, cx| {
                cancel.update(cx, |this, cx| this.finish(true, false, window, cx));
                true
            })
    });
}

impl InstallView {
    fn new(
        ws: Entity<Workspace>,
        project_id: String,
        host: String,
        then_create: Option<Option<SessionAgent>>,
        status: HostZellijStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = status.base_url.clone().unwrap_or_default();
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(status.default_base_url.clone())
                .default_value(url.clone())
        });
        let target = cx.new(|cx| {
            let opts = TARGETS.iter().map(|t| Opt::new(*t, *t)).collect();
            kit::new_select(Options::flat(opts), TARGETS[0], window, cx)
        });
        cx.observe(&base_url, |_, _, cx| cx.notify()).detach();
        cx.observe(&target, |_, _, cx| cx.notify()).detach();
        let authorized = status.authorized == Some(true);
        let mut this = Self {
            ws,
            project_id,
            host,
            then_create,
            phase: Phase::Ask,
            status,
            base_url,
            saved_url: url,
            stage: ZellijInstallStage::Probing,
            commands: Vec::new(),
            failure: None,
            detail: None,
            attempt: 1,
            settled: false,
            show_detail: false,
            show_manual: false,
            copied: false,
            target,
            log_scroll: ScrollHandle::new(),
            install: None,
            error: None,
            finished: false,
        };
        // 已授权（装过但版本不对 / 没装完）就直接进安装，不再打扰用户
        if authorized {
            this.start_install(window, cx);
        }
        this
    }

    fn url(&self, cx: &App) -> String {
        self.base_url.read(cx).value().trim().to_string()
    }

    /// 装完 / 放弃之后回到用户本来想干的事：从「新建终端」进来的就继续建会话
    fn finish(&mut self, create: bool, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.finished {
            return;
        }
        self.finished = true;
        // 还在装就是取消：丢掉任务 = 关掉通道，服务端据此 abort
        self.install = None;
        if close {
            window.close_dialog(cx);
        }
        if create && let Some(agent) = self.then_create {
            let pid = self.project_id.clone();
            self.ws.update(cx, |w, cx| w.create_session_now(&pid, agent, None, cx));
        }
    }

    fn start_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.install = None;
        self.phase = Phase::Installing;
        self.stage = ZellijInstallStage::Probing;
        self.attempt = 1;
        self.failure = None;
        self.detail = None;
        self.error = None;
        self.settled = false;
        self.commands.clear();
        let client = self.ws.read(cx).client.clone();
        let mut socket = InstallSocket::open(&client, &self.project_id);
        self.install = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(ev) = socket.next().await {
                let alive = this.update_in(cx, |this, window, cx| this.on_event(ev, window, cx)).is_ok();
                if !alive {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn on_event(&mut self, ev: InstallEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            InstallEvent::Message(InstallServerMessage::Stage { stage, attempt, command }) => {
                self.stage = stage;
                self.attempt = attempt;
                if let Some(cmd) = command
                    && self.commands.last() != Some(&cmd)
                {
                    self.commands.push(cmd);
                    self.log_scroll.scroll_to_bottom();
                }
            }
            InstallEvent::Message(InstallServerMessage::Done) => {
                self.settled = true;
                self.on_installed(window, cx);
            }
            InstallEvent::Message(InstallServerMessage::Failed { reason, detail, attempts }) => {
                self.settled = true;
                self.failure = Some(reason);
                self.detail = detail;
                self.attempt = attempts.unwrap_or(1);
                self.phase = Phase::Failed;
            }
            InstallEvent::Message(InstallServerMessage::Unknown) => {}
            // 连不上后端：这条通道自己就断了，跟远端装没装成无关，同样给重试按钮
            InstallEvent::ChannelError(message) if !self.settled => {
                self.settled = true;
                self.failure = Some(NonDurableReason::ProbeFailed);
                self.detail = Some(format!("{}\n{message}", t!("zellij.channelError")));
                self.phase = Phase::Failed;
            }
            InstallEvent::Unauthorized if !self.settled => {
                self.settled = true;
                self.failure = Some(NonDurableReason::ProbeFailed);
                self.detail = Some(t!("zellij.channelError").to_string());
                self.phase = Phase::Failed;
            }
            _ => {}
        }
        cx.notify();
    }

    /// 装好之后已经起来的会话**不会**变持久——它们是裸 shell，外面没有 Zellij 包着。
    /// 不说清楚的话用户会以为问题解决了，下次断线才发现工作没保住。
    fn on_installed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pid = self.project_id.clone();
        let live = self
            .ws
            .read(cx)
            .sessions
            .iter()
            .filter(|s| s.project_id == pid && s.state != SessionState::Dead)
            .count();
        let will_create = self.then_create.is_some();
        // 任务正读着通道、这次更新就在它里面：放它自己跑完，别在自己脚下丢掉
        if let Some(task) = self.install.take() {
            task.detach();
        }
        self.finish(true, true, window, cx);
        let title = t!("zellij.readyTitle", host = self.host.clone()).to_string();
        let mut n = Notification::success(title.clone());
        if live > 0 {
            n = n.title(title).message(t!("zellij.readyBody", n = live).to_string()).autohide(false);
        }
        if !will_create {
            let ws = self.ws.clone();
            n = n.action(move |_, _, cx| {
                let (ws, pid) = (ws.clone(), pid.clone());
                let note = cx.entity();
                Button::new("zellij-ready-new")
                    .small()
                    .primary()
                    .label(t!("sidebar.newTerminal").to_string())
                    .on_click(move |_, window, cx| {
                        ws.update(cx, |w, cx| w.new_terminal(&pid, None, None, cx));
                        note.update(cx, |n, cx| n.dismiss(window, cx));
                    })
            });
        }
        window.push_toast(n, cx);
    }

    fn allow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url(cx);
        let patch = HostAuthorizationPatch { authorized: Some(true), base_url: Some(url.clone()) };
        let request = self.ws.read(cx).client.set_host_authorization(&self.project_id, &patch);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.saved_url = url;
                    this.start_install(window, cx);
                }
                Err(err) => {
                    this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    this.error = Some(err.message.clone());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 显式拒绝会写库；措辞里把可逆性说出来，反悔的入口在命令面板
    fn deny(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let patch = HostAuthorizationPatch { authorized: Some(false), base_url: None };
        let request = self.ws.read(cx).client.set_host_authorization(&self.project_id, &patch);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => this.ws.update(cx, |w, cx| {
                        w.toast(
                            ToastKind::Warning,
                            t!("zellij.skippedTitle").to_string(),
                            Some(t!("zellij.skippedBody").to_string()),
                            cx,
                        )
                    }),
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                this.finish(true, true, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url(cx);
        if url == self.saved_url.trim() {
            self.start_install(window, cx);
            return;
        }
        // 换地址要先落库：后端据此作废旧的安装记录，重试才不会用回老源
        let patch = HostAuthorizationPatch { authorized: None, base_url: Some(url.clone()) };
        let request = self.ws.read(cx).client.set_host_authorization(&self.project_id, &patch);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.saved_url = url;
                    this.start_install(window, cx);
                }
                Err(err) => {
                    this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    this.error = Some(err.message.clone());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 回车：当前阶段的主按钮（web 里它带 data-autofocus）
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Ask => self.allow(window, cx),
            Phase::Failed if can_retry_install(self.failure) => self.retry(window, cx),
            _ => {}
        }
    }

    fn manual_cmd(&self, cx: &App) -> String {
        let version = &self.status.required_version;
        let url = self.url(cx);
        let base = if url.is_empty() { self.status.default_base_url.clone() } else { url };
        let base = base.trim_end_matches('/');
        let target = kit::selected(&self.target, cx).unwrap_or_else(|| TARGETS[0].to_string());
        let windows = target == "x86_64-pc-windows-msvc";
        let asset = format!("zellij-no-web-{target}.{}", if windows { "zip" } else { "tar.gz" });
        let asset_url = format!("{base}/v{version}/{asset}");
        if windows {
            [
                "New-Item -ItemType Directory -Force ~\\.falcon\\bin | Out-Null".to_string(),
                format!("curl.exe -fsSL \"{asset_url}\" -o \"$env:TEMP\\zellij.zip\""),
                "tar.exe -xf \"$env:TEMP\\zellij.zip\" -C ~\\.falcon\\bin".to_string(),
                format!("Move-Item -Force ~\\.falcon\\bin\\zellij.exe ~\\.falcon\\bin\\zellij-{version}.exe"),
            ]
            .join("\n")
        } else {
            [
                "mkdir -p ~/.falcon/bin".to_string(),
                format!("curl -L \"{asset_url}\" | tar xz -C ~/.falcon/bin"),
                format!("mv ~/.falcon/bin/zellij ~/.falcon/bin/zellij-{version}"),
                format!("chmod +x ~/.falcon/bin/zellij-{version}"),
            ]
            .join("\n")
        }
    }

    fn copy_manual(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.manual_cmd(cx)));
        self.copied = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1600)).await;
            this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---------------- 渲染 ----------------

    /// 安装过程中远端实际跑过的命令。最新一条是正在执行或刚失败的那条
    fn command_log(&self, running: bool, cx: &App) -> gpui_kit::Div {
        let ui = Ui::global(cx).clone();
        let n = self.commands.len();
        let mut log = div()
            .id("zellij-log")
            .max_h(zpx(208.))
            .overflow_y_scroll()
            .track_scroll(&self.log_scroll)
            .rounded(radius::MD)
            .bg(ui.app)
            .px_3()
            .py_2()
            .text_size(zpx(11.5))
            .flex()
            .flex_col()
            .gap_2();
        for (i, cmd) in self.commands.iter().enumerate() {
            let last = i + 1 == n;
            let mut line = format!("$ {cmd}");
            if last && running {
                line.push_str(" …");
            }
            log = log.child(div().text_color(if last { ui.foreground } else { ui.muted_foreground }).child(line));
        }
        div().flex().flex_col().gap(zpx(6.)).child(note(t!("zellij.commandLog").to_string(), cx)).child(log)
    }

    fn code_block(code: String, cx: &App) -> gpui_kit::Div {
        let ui = Ui::global(cx);
        div()
            .rounded(radius::MD)
            .bg(ui.app)
            .px_3()
            .py(zpx(10.))
            .text_size(zpx(11.5))
            .text_color(ui.muted_foreground)
            .child(code)
    }

    fn render_ask(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let ui = Ui::global(cx).clone();
        let body = t!("zellij.enableBody", host = self.host.clone(), version = self.status.required_version.clone());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().line_height(zpx(22.)).text_color(ui.muted_foreground).child(body.to_string()))
            // 不做完整性校验，自定义地址等于把执行权交给该地址的控制者
            .child(field(
                Some(t!("zellij.sourceLabel").to_string()),
                Input::new(&self.base_url).small(),
                Some((t!("zellij.sourceWarning").to_string(), Tone::Warn)),
                cx,
            ))
            .child(note(t!("zellij.authNote").to_string(), cx))
            .when_some(self.error.clone(), |d, e| d.child(error_line(e, cx)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("zellij-deny")
                            .outline()
                            .label(t!("zellij.authDeny").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.deny(window, cx))),
                    )
                    .child(
                        Button::new("zellij-allow")
                            .primary()
                            .label(t!("zellij.authAllow").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.allow(window, cx))),
                    ),
            )
    }

    fn render_installing(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let ui = Ui::global(cx).clone();
        // 宿主机自己下载，后端拿不到字节数，因此只有阶段没有百分比
        let stage = match self.stage {
            ZellijInstallStage::Unknown => t!("native.forms.installing").to_string(),
            s => {
                let key = format!("zellij.stage_{}", s.as_str());
                t!(&key).to_string()
            }
        };
        let mut d = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().text_color(ui.muted_foreground).child(stage));
        if self.attempt > 1 {
            d = d.child(note(t!("zellij.autoRetrying", n = self.attempt).to_string(), cx));
        }
        if !self.commands.is_empty() {
            d = d.child(self.command_log(true, cx));
        }
        d.child(
            div().flex().justify_end().child(
                Button::new("zellij-cancel")
                    .outline()
                    .label(t!("zellij.installCancel").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.finish(true, true, window, cx))),
            ),
        )
    }

    fn render_failed(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let ui = Ui::global(cx).clone();
        let mut reason = div()
            .flex()
            .flex_col()
            .child(div().text_size(zpx(13.)).child(reason_text(&Tr, self.failure)));
        if self.attempt > 1 {
            reason = reason.child(note(t!("zellij.autoRetried", n = self.attempt).to_string(), cx).mt(zpx(2.)));
        }
        let mut d = div().flex().flex_col().gap_3().child(
            div()
                .flex()
                .gap(zpx(10.))
                .rounded(radius::MD)
                .bg(ui.warning.opacity(0.1))
                .px_3()
                .py(zpx(10.))
                .child(crate::ui::icon(IconName::TriangleAlert).size(zpx(16.)).flex_none().text_color(ui.warning))
                .child(reason),
        );
        if !self.commands.is_empty() {
            d = d.child(self.command_log(false, cx));
        }
        if self.failure.is_some_and(|f| URL_FIXABLE.contains(&f)) {
            d = d.child(field(Some(t!("zellij.sourceLabel").to_string()), Input::new(&self.base_url).small(), None, cx));
        }
        if let Some(detail) = self.detail.clone() {
            d = d.child(disclosure(
                "zellij-detail",
                self.show_detail,
                t!("zellij.detailToggle").to_string(),
                cx.listener(|this, _, _, cx| {
                    this.show_detail = !this.show_detail;
                    cx.notify();
                }),
                Self::code_block(detail, cx),
                cx,
            ));
        }
        let manual = {
            let cmd = self.manual_cmd(cx);
            let copy_label = if self.copied { t!("zellij.copied") } else { t!("zellij.copy") };
            let copy = Button::new("zellij-copy")
                .xsmall()
                .secondary()
                .label(copy_label.to_string())
                .on_click(cx.listener(|this, _, _, cx| this.copy_manual(cx)));
            let recheck = Button::new("zellij-recheck")
                .xsmall()
                .link()
                .label(t!("zellij.recheck").to_string())
                .on_click(cx.listener(|this, _, window, cx| this.start_install(window, cx)));
            let target = field(Some(t!("zellij.manualTarget").to_string()), Select::new(&self.target).small().w_full(), None, cx).mb_2();
            let block = Self::code_block(cmd, cx)
                .relative()
                .pr(zpx(64.))
                .child(div().absolute().top_2().right_2().child(copy))
                .child(div().mt_2().flex().child(recheck));
            div().flex().flex_col().child(target).child(block)
        };
        d = d.child(disclosure(
            "zellij-manual",
            self.show_manual,
            t!("zellij.manualToggle").to_string(),
            cx.listener(|this, _, _, cx| {
                this.show_manual = !this.show_manual;
                cx.notify();
            }),
            manual,
            cx,
        ));
        if let Some(e) = self.error.clone() {
            d = d.child(error_line(e, cx));
        }
        let edited = self.url(cx) != self.saved_url.trim();
        let mut footer = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("zellij-skip")
                    .ghost()
                    .label(t!("zellij.installSkip").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.finish(true, true, window, cx))),
            )
            .child(div().flex_1());
        if can_retry_install(self.failure) {
            let label = if edited { t!("zellij.installRetryNewUrl") } else { t!("zellij.installRetry") };
            footer = footer.child(
                Button::new("zellij-retry")
                    .primary()
                    .label(label.to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx))),
            );
        }
        d.child(footer)
    }
}

impl Render for InstallView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.phase {
            Phase::Ask => self.render_ask(cx),
            Phase::Installing => self.render_installing(cx),
            Phase::Failed => self.render_failed(cx),
        }
    }
}
