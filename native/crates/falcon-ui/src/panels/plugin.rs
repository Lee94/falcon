//! 右侧栏的插件面板（ADR 0025）：一个插件一扇，画服务端回来的面板文档（`PluginView`）。
//!
//! 面板本身不认识任何插件：顶栏是插件的图标与名字，正文是文档里的块（`blocks.rs`），点按钮 /
//! 换选项再回服务端要一份新的。内置的系统监控与用户的命令插件走的是同一条路。
//!
//! 重画的节奏照插件的意思（`falcon_core::plugin::next_refresh`：文档里的 refresh 盖过清单），
//! 只在面板真的露着时重画——右侧栏切走时视图不卸载（沿用别的面板），但不该还在后台起插件进程；
//! 窗口看不见（最小化 / 浏览器后台标签页）也停。画失败了留着上一份文档、顶上一条错误，下一轮照常再试：
//! 连不上的那一下不该把整个面板清空。

mod blocks;
mod chart;

use std::collections::BTreeMap;
use std::time::Duration;

use falcon_core::plugin::{next_refresh, sync_params};
use falcon_proto::{PluginAction, PluginActionInput, PluginInfo, PluginRenderInput, PluginScope, PluginTone, PluginView};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClipboardItem, Context, Entity, FontWeight, IntoElement, Render, Task, Window, div};
use rust_i18n::t;
use web_time::Instant;

use super::meegle::widgets::{hint, hint_detail, icon_button};
use crate::dialogs::{self, ConfirmOpts};
use crate::theme::Ui;
use crate::ui::{icon, spinner};
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

/// 轮询任务醒来看一眼"到点了没"的间隔。真正的重画间隔由插件定（≥ 1s）
const TICK: Duration = Duration::from_millis(250);
/// 画失败之后多久再试（插件自己的间隔更长就按它的）
const RETRY: Duration = Duration::from_secs(5);

/// Lucide 图标名（kebab-case）→ 图标。认不出就是拼图块。全表一千多个，按名字线性找一遍不值得每帧做，
/// 记在线程本地的表里（wasm 上没有别的线程）
pub fn plugin_icon(name: &str) -> IconName {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, IconName>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(i) = c.borrow().get(name) {
            return *i;
        }
        let path = format!("icons/{name}.svg");
        let found = IconName::ALL.iter().copied().find(|i| i.path().as_ref() == path).unwrap_or(IconName::Puzzle);
        c.borrow_mut().insert(name.to_string(), found);
        found
    })
}

pub struct PluginPanel {
    ws: Entity<Workspace>,
    id: String,
    view: Option<PluginView>,
    error: Option<String>,
    /// 面板参数（select 的取值、setParam 动作设的值），每次请求都带上，回来的文档里的取值再覆盖它
    params: BTreeMap<String, String>,
    /// 焦点项目：每次请求都带上；插件是 project 范围的，换了就重画
    project: Option<String>,
    visible: bool,
    /// 参数 / 项目每变一次加一：晚到的旧结果丢掉
    seq: u64,
    in_flight: bool,
    /// 手动刷新、头一次画、按钮动作：顶栏转圈。自动重画不转——两秒闪一下只会让人分心
    busy: bool,
    next_due: Option<Instant>,
    _poll: Task<()>,
}

impl PluginPanel {
    pub fn new(ws: Entity<Workspace>, id: String, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |this, _, cx| {
            this.sync(cx);
            cx.notify();
        })
        .detach();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let alive = this.update(cx, |this, cx| {
                    let due = this.next_due.is_some_and(|d| Instant::now() >= d);
                    if this.visible && due && !this.in_flight {
                        this.load(false, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            ws,
            id,
            view: None,
            error: None,
            params: BTreeMap::new(),
            project: None,
            visible: false,
            seq: 0,
            in_flight: false,
            busy: false,
            next_due: None,
            _poll: poll,
        };
        this.sync(cx);
        this
    }

    fn info<'a>(&self, cx: &'a gpui_kit::App) -> Option<&'a PluginInfo> {
        self.ws.read(cx).plugin(&self.id)
    }

    /// 跟着工作区走：露出来时画一次；project 范围的插件换了焦点项目也画一次
    fn sync(&mut self, cx: &mut Context<Self>) {
        let ws = self.ws.read(cx);
        let visible = ws.ready() && ws.window_visible && ws.state.right_open && ws.state.right_panel.is_plugin(&self.id);
        let info = ws.plugin(&self.id);
        let usable = info.is_some_and(|i| i.enabled && i.error.is_none());
        let follows = info.is_some_and(|i| i.scope == PluginScope::Project);
        let project = ws.focus_project_id();
        let moved = project != self.project;
        self.project = project;
        let appeared = visible && !self.visible;
        self.visible = visible;
        if !visible || !usable {
            return;
        }
        if moved && follows {
            self.seq += 1;
            self.in_flight = false;
            self.load(true, cx);
        } else if appeared || self.view.is_none() && self.error.is_none() && !self.in_flight {
            self.load(self.view.is_none(), cx);
        }
    }

    fn input(&self) -> PluginRenderInput {
        PluginRenderInput { project_id: self.project.clone(), params: self.params.clone() }
    }

    /// 画一次。`busy` = 顶栏要不要转圈
    fn load(&mut self, busy: bool, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        self.busy |= busy;
        self.next_due = None;
        let seq = self.seq;
        let fut = self.ws.read(cx).client.plugin_render(&self.id, &self.input());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.in_flight = false;
                this.busy = false;
                match result {
                    Ok(view) => this.apply(view, false, cx),
                    Err(err) => this.failed(err, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 参数变了 / 点了刷新：作废在路上的那次，马上重画
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.seq += 1;
        self.in_flight = false;
        self.load(true, cx);
    }

    fn apply(&mut self, view: PluginView, from_action: bool, cx: &mut Context<Self>) {
        sync_params(&view, &mut self.params);
        if from_action && let Some(toast) = &view.toast {
            let kind = match toast.tone {
                Some(PluginTone::Success) => ToastKind::Success,
                Some(PluginTone::Warning) => ToastKind::Warning,
                Some(PluginTone::Danger) => ToastKind::Danger,
                _ => ToastKind::Info,
            };
            let (title, body) = (toast.title.clone(), toast.body.clone());
            self.ws.update(cx, |w, cx| w.toast(kind, title, body, cx));
        }
        let info = self.info(cx).cloned();
        self.next_due = next_refresh(info.as_ref(), Some(&view)).map(|d| Instant::now() + d);
        self.view = Some(view);
        self.error = None;
    }

    fn failed(&mut self, err: falcon_client::ApiError, cx: &mut Context<Self>) {
        self.error = Some(err.to_string());
        // 409：插件刚被停用 / 清单改坏了；404：插件目录被删了。列表跟着更新，面板据此换成提示
        if err.is_conflict() || err.is_not_found() {
            self.ws.update(cx, |w, cx| w.refresh_plugins(cx));
        }
        self.ws.update(cx, |w, cx| w.handle_error(&err, cx));
        let info = self.info(cx).cloned();
        let every = next_refresh(info.as_ref(), self.view.as_ref()).map_or(RETRY, |d| d.max(RETRY));
        self.next_due = Some(Instant::now() + every);
    }

    pub(super) fn set_param(&mut self, param: String, value: String, cx: &mut Context<Self>) {
        if self.params.get(&param) == Some(&value) {
            return;
        }
        self.params.insert(param, value);
        self.reload(cx);
    }

    /// 点了按钮 / 列表行
    pub(super) fn run_action(&mut self, action: PluginAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PluginAction::Invoke { id, value, confirm } => match confirm {
                Some(body) => {
                    let this = cx.weak_entity();
                    let title = self.info(cx).map(|i| i.name.clone()).unwrap_or_else(|| self.id.clone());
                    let opts = ConfirmOpts { title, body, confirm_label: t!("plugin.confirm").to_string(), ..Default::default() };
                    dialogs::confirm(
                        opts,
                        move |_, cx| {
                            let (id, value) = (id.clone(), value.clone());
                            let _ = this.update(cx, |p, cx| p.invoke(id, value, cx));
                        },
                        window,
                        cx,
                    );
                }
                None => self.invoke(id, value, cx),
            },
            PluginAction::OpenUrl { url } => cx.open_url(&url),
            PluginAction::Copy { text } => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.ws.update(cx, |w, cx| w.toast(ToastKind::Success, t!("plugin.copied").to_string(), None, cx));
            }
            PluginAction::Refresh => self.reload(cx),
            PluginAction::SetParam { param, value } => self.set_param(param, value, cx),
            PluginAction::Unknown => {}
        }
    }

    fn invoke(&mut self, action: String, value: Option<String>, cx: &mut Context<Self>) {
        let input = PluginActionInput { project_id: self.project.clone(), params: self.params.clone(), action, value };
        let fut = self.ws.read(cx).client.plugin_action(&self.id, &input);
        // 动作回来的文档就是新面板：在路上的那次自动重画作废
        self.seq += 1;
        let seq = self.seq;
        self.in_flight = true;
        self.busy = true;
        self.next_due = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.seq == seq {
                    this.in_flight = false;
                    this.busy = false;
                }
                match result {
                    Ok(view) if this.seq == seq => this.apply(view, true, cx),
                    Ok(_) => {}
                    Err(err) => {
                        this.ws.update(cx, |w, cx| {
                            w.handle_error(&err, cx);
                            w.toast_sticky(ToastKind::Danger, t!("plugin.actionFailed").to_string(), Some(err.to_string()), cx);
                        });
                        // 面板可能已经跟着变了：重画一次看真实状态
                        if this.seq == seq {
                            this.next_due = Some(Instant::now());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn header(&self, info: Option<&PluginInfo>, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let name = info.map(|i| i.name.clone()).unwrap_or_else(|| self.id.clone());
        let glyph = info.map(|i| plugin_icon(&i.icon)).unwrap_or(IconName::Puzzle);
        let refresh: AnyElement = if self.busy {
            spinner("plugin-refresh-spin", IconName::RefreshCw, zpx(14.))
        } else {
            icon(IconName::RefreshCw).size(zpx(14.)).into_any_element()
        };
        div()
            .h(zpx(34.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr(zpx(6.))
            .border_b_1()
            .border_color(ui.border)
            .child(icon(glyph).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).font_weight(FontWeight::MEDIUM).child(name))
            .child(icon_button(
                "plugin-refresh",
                refresh,
                t!("plugin.refresh").to_string(),
                24.,
                false,
                cx,
                cx.listener(|this, _, _, cx| this.reload(cx)),
            ))
            .into_any_element()
    }
}

impl Render for PluginPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let listed = ws.plugins.is_some();
        let info = ws.plugin(&self.id).cloned();
        let header = self.header(info.as_ref(), cx);
        let body: AnyElement = match (&info, &self.view) {
            // 列表还没拉回来：先等着，别急着说"没有这个插件"
            (None, _) if !listed => hint(t!("plugin.loading").to_string(), cx).into_any_element(),
            (None, _) => hint(t!("plugin.missing", id = self.id).to_string(), cx).into_any_element(),
            (Some(i), _) if !i.enabled => hint(t!("plugin.disabled").to_string(), cx).into_any_element(),
            (Some(i), _) if i.error.is_some() => {
                hint_detail(t!("plugin.broken").to_string(), i.error.clone().unwrap_or_default(), cx).into_any_element()
            }
            (Some(_), None) => match &self.error {
                Some(err) => hint_detail(t!("plugin.failed").to_string(), err.clone(), cx).into_any_element(),
                None => hint(t!("plugin.loading").to_string(), cx).into_any_element(),
            },
            (Some(_), Some(view)) => self.render_view(view.clone(), cx),
        };
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(ui.foreground)
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }
}
