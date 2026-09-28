//! ⌘P 转到文件（web 的 `components/FileQuickOpen.tsx`）：搜当前项目工作目录里的文件，回车在
//! 画布上打开文件窗口。
//!
//! 清单一次拉全（`/api/projects/:id/files/index`，git 仓库走 ls-files，有上限），过滤在客户端，
//! 打分用 `falcon_core::file_search`（文件名命中优于路径命中，前缀优于包含，子序列垫底）——
//! 交给通用模糊匹配会把路径里的斜杠当普通字符搅乱排序。
//!
//! 与命令面板长得一样（同一套对话框壳），但不复用 palette 的 Picker：这里要分"没选项目 /
//! 读取中 / 读不到 / 没有匹配"几种空状态，列表底下还有"只索引了前 N 个"的提示。

use falcon_core::file_search::{FILE_SEARCH_LIMIT, basename, dirname, filter_files};
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, ScrollHandle,
    SharedString, Subscription, Window, div,
};
use rust_i18n::t;

use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::Workspace;
use crate::zoom::zpx;

pub fn open(ws: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| QuickOpen::new(ws.clone(), window, cx));
    let dialog_view = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = dialog_view.clone();
        // 回车：输入框的 Enter 动作会往上传到对话框的 Confirm（单行输入框只发 PressEnter 不吃掉
        // 按键），对话框随即关闭、连同这个视图一起释放——等 PressEnter 事件送到时订阅者已经没了。
        // 所以回车接在对话框的 on_ok 上，关之前先把文件打开
        dialog
            .w(zpx(580.))
            .close_button(false)
            .margin_top(zpx(80.))
            .on_ok(move |_, _, cx| {
                confirm.update(cx, |v, cx| v.open_selected(cx));
                true
            })
            .child(dialog_view.clone())
    });
    // 打开对话框会把焦点交给对话框自己的容器（Root::open_dialog），输入框得在那之后再要一次，
    // 否则打字进不了搜索框
    let input = view.read(cx).input.clone();
    input.update(cx, |i, cx| i.focus(window, cx));
}

struct QuickOpen {
    ws: Entity<Workspace>,
    project_id: Option<String>,
    input: Entity<InputState>,
    /// None = 还在读
    paths: Option<Vec<String>>,
    truncated: bool,
    error: Option<String>,
    hits: Vec<String>,
    selected: usize,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl QuickOpen {
    fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("quickOpen.placeholder").to_string()));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                this.refilter(cx);
            }
        });
        // 焦点项目优先，没有就第一个项目（与 web 一致）
        let project_id = {
            let w = ws.read(cx);
            w.focus_project_id().or_else(|| w.projects.first().map(|p| p.id.clone()))
        };
        if let Some(pid) = &project_id {
            let fut = ws.read(cx).client.index_files(pid);
            cx.spawn(async move |this, cx| {
                let result = fut.await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok(index) => {
                            this.paths = Some(index.paths);
                            this.truncated = index.truncated;
                        }
                        Err(err) => {
                            this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                            this.error = Some(err.to_string());
                        }
                    }
                    this.refilter(cx);
                })
                .ok();
            })
            .detach();
        }
        Self {
            ws,
            project_id,
            input,
            paths: None,
            truncated: false,
            error: None,
            hits: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            _sub: sub,
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.hits = self.paths.as_deref().map(|p| filter_files(p, &query, FILE_SEARCH_LIMIT)).unwrap_or_default();
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// 在画布上打开选中的那一项（关对话框由调用方负责）
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(pid), Some(path)) = (self.project_id.clone(), self.hits.get(self.selected).cloned()) else { return };
        self.ws.update(cx, |w, cx| w.open_file(&pid, &path, cx));
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        self.open_selected(cx);
        window.close_dialog(cx);
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.hits.len();
        match e.keystroke.key.as_str() {
            "down" if n > 0 => {
                self.selected = (self.selected + 1) % n;
                self.scroll.scroll_to_item(self.selected);
                cx.stop_propagation();
                cx.notify();
            }
            "up" if n > 0 => {
                self.selected = (self.selected + n - 1) % n;
                self.scroll.scroll_to_item(self.selected);
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Focusable for QuickOpen {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let empty = |text: String, detail: Option<String>| {
            let mut d = div()
                .py_6()
                .flex()
                .flex_col()
                .items_center()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(text);
            if let Some(detail) = detail {
                d = d.child(div().mt_1().text_size(zpx(11.)).child(detail));
            }
            d
        };

        let mut list = div()
            .id("quick-open-list")
            .max_h(zpx(352.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(zpx(2.));
        if self.project_id.is_none() {
            list = list.child(empty(t!("quickOpen.noProject").to_string(), None));
        } else if let Some(err) = &self.error {
            list = list.child(empty(t!("quickOpen.failed").to_string(), Some(err.clone())));
        } else if self.paths.is_none() {
            list = list.child(empty(t!("quickOpen.loading").to_string(), None));
        } else if self.hits.is_empty() {
            list = list.child(empty(t!("quickOpen.empty").to_string(), None));
        } else {
            for (i, path) in self.hits.iter().enumerate() {
                let dir = dirname(path);
                let mut row = div()
                    .id(SharedString::from(format!("qo-{i}")))
                    .h(zpx(30.))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(radius::SM)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(i, window, cx)))
                    .child(icon(IconName::FileText).size(zpx(14.)).flex_none().text_color(ui.muted_foreground))
                    .child(div().flex_1().min_w_0().truncate().child(basename(path).to_string()));
                if !dir.is_empty() {
                    row = row.child(
                        div()
                            .flex_none()
                            .max_w(zpx(200.))
                            .truncate()
                            .text_size(zpx(11.))
                            .text_color(ui.muted_foreground)
                            .child(dir.to_string()),
                    );
                }
                row = if i == self.selected { row.bg(ui.tint).text_color(ui.tint_foreground) } else { row.hover(|s| s.bg(ui.muted)) };
                list = list.child(row);
            }
        }

        let mut root = div()
            .flex()
            .flex_col()
            .gap_2()
            .capture_key_down(cx.listener(Self::on_key))
            .child(Input::new(&self.input))
            .child(list);
        if self.truncated && self.paths.is_some() {
            let n = self.paths.as_ref().map_or(0, Vec::len);
            root = root.child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(t!("quickOpen.truncated", n = n).to_string()),
            );
        }
        root
    }
}
