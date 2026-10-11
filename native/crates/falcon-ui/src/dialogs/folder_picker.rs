//! 宿主机目录选择器（旧 React 版的 `components/FolderPicker.tsx`）：列的是**宿主机**上的目录，走
//! `/api/fs/list`（`path=""` 在 Windows 宿主机上是盘符列表），不能换成本机的系统文件夹选择器——
//! 远端项目的目录不在这台 Mac 上；连本机 falcon 服务端时也走这条路，行为照 React 版。
//!
//! 照 React 版**嵌在项目表单的对话框里**，不另开一层：表单切到"选目录"态时换成这个视图、
//! 标题换成"选择文件夹"、对话框变宽，Esc 只退回表单（由表单的 on_cancel 接，见
//! `project_form.rs`）。本地或 SSH 远端共用这一套，列哪台机器由 [`ProbeTarget`] 决定。
//!
//! 键盘：筛选框里 ↑↓ 移高亮、回车进入高亮的目录、筛选为空时退格回上一级；路径框回车跳转。
//! 这些键在 Input 的上下文里各自绑了动作（MoveUp / Enter / Backspace），GPUI 先分派动作、
//! 动作没人要才轮到 key_down 监听——所以这里用 `capture_action` 在捕获阶段截下来，
//! 而不是 `on_key_down`（那样永远等不到）。

use std::rc::Rc;

use falcon_client::{FalconClient, ProbeTarget};
use falcon_proto::FsListing;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Backspace, Enter, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::Disableable;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, ScrollHandle, SharedString, Task, Window, div};
use rust_i18n::t;

use crate::dialogs::project_form::kit::error_line;
use crate::theme::{Ui, radius};
use crate::zoom::zpx;

pub type OnSelect = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type OnClose = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct FolderPicker {
    client: FalconClient,
    target: ProbeTarget,
    /// 列的是 SSH 远端：加载中文案说"正在连接远端"
    remote: bool,
    listing: Option<FsListing>,
    /// 路径框（可手敲再跳转）
    draft: Entity<InputState>,
    filter: Entity<InputState>,
    highlight: usize,
    error: Option<String>,
    busy: bool,
    /// 选完目录后外面正在创建项目：两个按钮都不能再点
    confirming: bool,
    scroll: ScrollHandle,
    on_select: OnSelect,
    on_close: OnClose,
    _load: Option<Task<()>>,
}

impl FolderPicker {
    pub fn new(
        client: FalconClient,
        target: ProbeTarget,
        remote: bool,
        initial_path: &str,
        on_select: OnSelect,
        on_close: OnClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = cx.new(|cx| InputState::new(window, cx).default_value(initial_path.to_string()));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("project.pickFilter").to_string()));
        cx.subscribe_in(&filter, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                let n = this.visible(cx).len();
                this.highlight = this.highlight.min(n.saturating_sub(1));
                cx.notify();
            }
        })
        .detach();
        let mut this = Self {
            client,
            target,
            remote,
            listing: None,
            draft,
            filter,
            highlight: 0,
            error: None,
            busy: false,
            confirming: false,
            scroll: ScrollHandle::new(),
            on_select,
            on_close,
            _load: None,
        };
        // 只在打开时读一次初始路径；空串 = 家目录
        let init = initial_path.trim();
        this.load(if init.is_empty() { None } else { Some(init.to_string()) }, None, window, cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.filter.update(cx, |i, cx| i.focus(window, cx));
    }

    pub fn set_confirming(&mut self, confirming: bool, cx: &mut Context<Self>) {
        self.confirming = confirming;
        cx.notify();
    }

    /// 列一层目录。初始路径读不了（被删了 / 远端没有）就退回家目录，但把原因留着给人看
    fn load(&mut self, dir: Option<String>, keep_error: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        if keep_error.is_none() {
            self.error = None;
        }
        let request = self.client.list_dir(dir.as_deref(), &self.target);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(listing) => {
                        // Windows 宿主机的盘符列表 path 是空串：路径框里提示"此电脑"
                        let placeholder = if listing.path.is_empty() { t!("project.pickRoots").to_string() } else { String::new() };
                        this.draft.update(cx, |i, cx| {
                            i.set_value(listing.path.clone(), window, cx);
                            i.set_placeholder(placeholder, window, cx);
                        });
                        this.filter.update(cx, |i, cx| i.set_value("", window, cx));
                        this.listing = Some(listing);
                        this.highlight = 0;
                        this.scroll.scroll_to_item(0);
                        if keep_error.is_some() {
                            this.error = keep_error;
                        }
                    }
                    Err(err) => {
                        if dir.is_some() && this.listing.is_none() {
                            this.load(None, Some(err.message.clone()), window, cx);
                            return;
                        }
                        this.error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn visible(&self, cx: &App) -> Vec<falcon_proto::FsDirEntry> {
        let q = self.filter.read(cx).value().trim().to_lowercase();
        let entries = self.listing.as_ref().map(|l| l.entries.as_slice()).unwrap_or(&[]);
        entries
            .iter()
            .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    fn enter(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        self.load(Some(dir), None, window, cx);
    }

    fn jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 空串原样发：Windows 宿主机上是盘符列表
        let draft = self.draft.read(cx).value().trim().to_string();
        self.load(Some(draft), None, window, cx);
    }

    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(home) = self.listing.as_ref().map(|l| l.home.clone()) {
            self.load(Some(home), None, window, cx);
        }
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.listing.as_ref().and_then(|l| l.parent.clone()) {
            self.load(Some(parent), None, window, cx);
        }
    }

    fn go_roots(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // POSIX 的根就是 /；Windows 的 "" 是盘符列表
        let Some(listing) = &self.listing else { return };
        let root = if listing.roots.first().map(String::as_str) == Some("/") { "/" } else { "" };
        self.load(Some(root.to_string()), None, window, cx);
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.visible(cx).len();
        if n == 0 {
            return;
        }
        let next = (self.highlight as isize + delta).clamp(0, n as isize - 1) as usize;
        self.highlight = next;
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.listing.as_ref().map(|l| l.path.clone()).filter(|p| !p.is_empty()) else {
            return;
        };
        if self.confirming {
            return;
        }
        // 回调会回头改外面的表单（表单又会改这个视图的 confirming）：推迟到这次更新结束，
        // 免得同一个实体被重入更新
        let f = self.on_select.clone();
        window.defer(cx, move |window, cx| f(path, window, cx));
    }
}

impl Render for FolderPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let has_listing = self.listing.is_some();
        let has_parent = self.listing.as_ref().is_some_and(|l| l.parent.is_some());
        let can_select = self.listing.as_ref().is_some_and(|l| !l.path.is_empty());
        let busy = self.busy;

        let tool = |id: &'static str, icon: IconName, tip: String, enabled: bool| {
            Button::new(id).outline().icon(icon).tooltip(tip).disabled(!enabled)
        };
        let draft = Input::new(&self.draft).aria_label(t!("project.workingDir").to_string());
        let toolbar = div()
            .flex()
            .gap(zpx(6.))
            .child(
                tool("fp-home", IconName::House, t!("project.pickHome").to_string(), !busy && has_listing)
                    .on_click(cx.listener(|this, _, window, cx| this.go_home(window, cx))),
            )
            .child(
                tool("fp-up", IconName::ArrowUp, t!("project.pickUp").to_string(), !busy && has_parent)
                    .on_click(cx.listener(|this, _, window, cx| this.go_up(window, cx))),
            )
            .child(
                tool("fp-roots", IconName::HardDrive, t!("project.pickRoots").to_string(), !busy && has_listing)
                    .on_click(cx.listener(|this, _, window, cx| this.go_roots(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                        cx.stop_propagation();
                        this.jump(window, cx);
                    }))
                    .child(draft),
            )
            .child(
                Button::new("fp-jump")
                    .outline()
                    .label(t!("project.pickJump").to_string())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.jump(window, cx))),
            );

        let filter = div()
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                this.move_highlight(1, cx);
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                this.move_highlight(-1, cx);
            }))
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                cx.stop_propagation();
                if let Some(hit) = this.visible(cx).get(this.highlight) {
                    let path = hit.path.clone();
                    this.enter(path, window, cx);
                }
            }))
            .capture_action(cx.listener(move |this, _: &Backspace, window, cx| {
                // 筛选为空时退格 = 回上一级；有字时照常删字
                if this.filter.read(cx).value().is_empty() && has_parent {
                    cx.stop_propagation();
                    this.go_up(window, cx);
                }
            }))
            .child(Input::new(&self.filter));

        let visible = self.visible(cx);
        let placeholder = |text: String| {
            div()
                .px_3()
                .py_6()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(text)
        };
        let mut list = div()
            .id("fp-list")
            .h(zpx(288.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .rounded(radius::MD)
            .bg(ui.app)
            .p(zpx(4.))
            .flex()
            .flex_col();
        if busy && !has_listing {
            let text = if self.remote { t!("project.pickConnecting") } else { t!("project.pickLoading") };
            list = list.child(placeholder(text.to_string()));
        } else if visible.is_empty() {
            let text = if self.filter.read(cx).value().trim().is_empty() {
                t!("project.pickEmpty")
            } else {
                t!("project.pickEmptyFilter")
            };
            list = list.child(placeholder(text.to_string()));
        } else {
            for (i, entry) in visible.iter().enumerate() {
                let on = i == self.highlight;
                let hidden = entry.name.starts_with('.');
                let path = entry.path.clone();
                let mut row = div()
                    .id(SharedString::from(format!("fp-row-{i}")))
                    .flex_none()
                    .h(zpx(32.))
                    .px(zpx(10.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(radius::SM)
                    .text_size(zpx(13.))
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlight != i {
                            this.highlight = i;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.enter(path.clone(), window, cx)))
                    .child(crate::ui::icon(IconName::Folder).size(zpx(14.)).flex_none().text_color(ui.muted_foreground))
                    .child(div().min_w_0().truncate().child(entry.name.clone()));
                row = if on {
                    row.bg(ui.tint).text_color(ui.tint_foreground)
                } else if hidden {
                    row.text_color(ui.muted_foreground)
                } else {
                    row
                };
                list = list.child(row);
            }
        }

        let select_label = if self.confirming { t!("project.creating") } else { t!("project.pickSelect") };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .child(filter)
            .child(list)
            .when_some(self.error.clone(), |d, e| d.child(error_line(e, cx)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("fp-cancel")
                            .outline()
                            .label(t!("common.cancel").to_string())
                            .disabled(self.confirming)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let f = this.on_close.clone();
                                window.defer(cx, move |window, cx| f(window, cx));
                            })),
                    )
                    .child(
                        Button::new("fp-select")
                            .primary()
                            .label(select_label.to_string())
                            .disabled(!can_select || self.confirming)
                            .on_click(cx.listener(|this, _, window, cx| this.select(window, cx))),
                    ),
            )
    }
}
