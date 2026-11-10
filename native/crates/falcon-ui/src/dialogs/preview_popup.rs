//! 右侧「文件」预览与「修改」差异预览的全局放大浮层。
//!
//! 右侧面板宽通常只有两三百像素，看长代码行、大图或双栏 diff 都局促。点预览工具栏上的放大按钮，
//! 在全局对话框层铺开一扇大尺寸预览窗，左侧附一条可收起的快捷文件栏：
//! - 文件预览浮层：搜索框为空时按当前目录平铺浏览（可进出子目录、切换隐藏文件），输入关键字时
//!   实时按全项目文件索引打分检索，点任意文件即在右侧大视图切换预览；支持编辑保存与未保存修改拦截。
//! - 差异预览浮层：列出当前工作区全部未提交改动文件（支持关键字筛选、列表 / 目录树切换、多仓库成员切换），
//!   点任意文件或按上下键即在右侧切换查看双栏 / 单栏 diff。

use std::collections::HashSet;
use std::rc::Rc;

use falcon_core::file_path::{format_size, is_hidden_name, join_host_path, parent_nav_path, parse_nav_path};
use falcon_core::file_search::{FILE_SEARCH_LIMIT, basename, dirname, filter_files};
use falcon_proto::{GitFileChange, WorkspaceEntry, WorkspaceEntryKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, FontWeight, IntoElement, KeyDownEvent, Render, ScrollStrategy,
    SharedString, Stateful, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};
use rust_i18n::t;

use crate::panels::diff_view::{DiffPopupChrome, DiffView};
use crate::panels::file_view::{FilePopupChrome, FileView};
use crate::panels::files::icon_for;
use crate::panels::git::common::{
    self, FileItem, FileListHost, FileRow, build_rows, file_view_mode, file_view_toggle, hint as git_hint,
    porcelain_status, render_file_row, tool_button as git_tool_button,
};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{DiffTabTarget, Workspace};
use crate::zoom::zpx;

const SIDEBAR_W: f32 = 264.;
const ROW_H: f32 = 28.;
const POPUP_MAX_W: f32 = 1360.;
const POPUP_MAX_H: f32 = 860.;
const POPUP_MARGIN: f32 = 64.;

fn open_popup_dialog<V: Render + 'static>(
    view: Entity<V>,
    on_cancel: impl Fn(&mut Window, &mut App) -> bool + 'static,
    focus: Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    let on_cancel = Rc::new(on_cancel);
    window.open_dialog(cx, move |dialog, window, _| {
        let vp = window.viewport_size();
        let w = (vp.width - zpx(POPUP_MARGIN)).min(zpx(POPUP_MAX_W));
        let h = (vp.height - zpx(POPUP_MARGIN)).min(zpx(POPUP_MAX_H));
        let on_cancel = on_cancel.clone();
        dialog
            .w(w)
            .h(h)
            .margin_top((vp.height - h) * 0.5)
            .p_0()
            .close_button(false)
            // 搜索框里的回车用于切换文件，不能把整个预览浮层当成"确定"关掉
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, window, cx| on_cancel(window, cx))
            .child(div().h(h - zpx(2.)).child(view.clone()))
    });
    window.defer(cx, move |window, cx| focus.update(cx, |i, cx| i.focus(window, cx)));
}

// ============================================================================
// 文件预览放大浮层
// ============================================================================

pub struct FilePopupInit {
    pub project_id: String,
    pub cwd: String,
    pub show_hidden: bool,
    pub path: String,
    /// 在浮层中切换了预览文件时通知右侧面板（参数：所在目录 cwd、文件路径 path）
    pub on_select: Option<Rc<dyn Fn(&str, &str, &mut Window, &mut App)>>,
    /// 在浮层中保存了文件时通知右侧面板
    pub on_saved: Option<Rc<dyn Fn(&str, &mut App)>>,
}

pub fn open_file(ws: &Entity<Workspace>, init: FilePopupInit, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| FilePreviewPopup::new(ws.clone(), init, window, cx));
    let focus = view.read(cx).search.clone();
    let cancel_view = view.clone();
    open_popup_dialog(
        view,
        move |window, cx| cancel_view.update(cx, |this, cx| this.try_close(window, cx)),
        focus,
        window,
        cx,
    );
}

struct FilePreviewPopup {
    ws: Entity<Workspace>,
    project_id: String,
    working_dir: Option<String>,
    roots: Vec<String>,
    cwd: String,
    show_hidden: bool,
    sidebar_open: bool,
    search: Entity<InputState>,
    dir_loading: bool,
    dir_entries: Option<Vec<WorkspaceEntry>>,
    dir_truncated: bool,
    dir_error: Option<String>,
    dir_gen: u64,
    index_loading: bool,
    index_paths: Option<Vec<String>>,
    index_truncated: bool,
    index_error: Option<String>,
    search_hits: Vec<String>,
    search_cursor: usize,
    current_path: String,
    file_view: Entity<FileView>,
    pending_path: Option<String>,
    force_close: bool,
    dir_scroll: UniformListScrollHandle,
    search_scroll: UniformListScrollHandle,
    on_select: Option<Rc<dyn Fn(&str, &str, &mut Window, &mut App)>>,
    _subs: Vec<Subscription>,
}

impl FilePreviewPopup {
    fn new(ws: Entity<Workspace>, init: FilePopupInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("files.filterFiles").to_string()));
        let (working_dir, roots) = {
            let w = ws.read(cx);
            let project = w.owner_project(&init.project_id);
            let wd = project.as_ref().and_then(|p| p.working_dir.clone());
            let roots = project
                .as_ref()
                .and_then(|p| p.multi.as_ref())
                .map(|m| m.repos.iter().map(|r| r.dir.clone()).collect())
                .unwrap_or_default();
            (wd, roots)
        };
        let pid = init.project_id.clone();
        let path = init.path.clone();
        let on_saved = init.on_saved;
        let file_view = cx.new(|cx| {
            let mut v = FileView::popup(ws.clone(), pid, path, window, cx);
            if let Some(on_saved) = on_saved {
                v.set_on_saved(on_saved);
            }
            v
        });
        let subs = vec![cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => this.on_search_change(cx),
            InputEvent::PressEnter { .. } => this.activate_search_cursor(window, cx),
            _ => {}
        })];
        let mut this = Self {
            ws,
            project_id: init.project_id,
            working_dir,
            roots,
            cwd: init.cwd,
            show_hidden: init.show_hidden,
            sidebar_open: true,
            search,
            dir_loading: false,
            dir_entries: None,
            dir_truncated: false,
            dir_error: None,
            dir_gen: 0,
            index_loading: false,
            index_paths: None,
            index_truncated: false,
            index_error: None,
            search_hits: Vec::new(),
            search_cursor: 0,
            current_path: init.path,
            file_view,
            pending_path: None,
            force_close: false,
            dir_scroll: UniformListScrollHandle::new(),
            search_scroll: UniformListScrollHandle::new(),
            on_select: init.on_select,
            _subs: subs,
        };
        this.load_dir(cx);
        this
    }

    fn is_searching(&self, cx: &App) -> bool {
        !self.search.read(cx).value().trim().is_empty()
    }

    fn host_path_of(&self, path: &str) -> String {
        join_host_path(self.working_dir.as_deref(), path)
    }

    fn same_file(&self, a: &str, b: &str) -> bool {
        self.host_path_of(a) == self.host_path_of(b)
    }

    fn load_dir(&mut self, cx: &mut Context<Self>) {
        self.dir_gen += 1;
        let generation = self.dir_gen;
        self.dir_loading = true;
        self.dir_error = None;
        let pid = self.project_id.clone();
        let cwd = self.cwd.clone();
        let fut = self.ws.read(cx).client.list_files(&pid, (!cwd.is_empty()).then_some(cwd.as_str()));
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.dir_gen != generation {
                    return;
                }
                match result {
                    Ok(listing) => {
                        this.dir_loading = false;
                        this.dir_entries = Some(listing.entries);
                        this.dir_truncated = listing.truncated;
                        this.dir_error = None;
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.dir_loading = false;
                        this.dir_entries = None;
                        this.dir_truncated = false;
                        this.dir_error = Some(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn ensure_index(&mut self, cx: &mut Context<Self>) {
        if self.index_paths.is_some() || self.index_loading {
            return;
        }
        self.index_loading = true;
        self.index_error = None;
        let pid = self.project_id.clone();
        let fut = self.ws.read(cx).client.index_files(&pid);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                this.index_loading = false;
                match result {
                    Ok(index) => {
                        this.index_paths = Some(index.paths);
                        this.index_truncated = index.truncated;
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.index_error = Some(err.to_string());
                    }
                }
                this.refilter(cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_search_change(&mut self, cx: &mut Context<Self>) {
        if self.is_searching(cx) {
            self.ensure_index(cx);
        }
        self.refilter(cx);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        if query.trim().is_empty() {
            self.search_hits.clear();
            self.search_cursor = 0;
            cx.notify();
            return;
        }
        self.search_hits = self
            .index_paths
            .as_deref()
            .map(|p| filter_files(p, &query, FILE_SEARCH_LIMIT))
            .unwrap_or_default();
        self.search_cursor = self
            .search_hits
            .iter()
            .position(|p| self.same_file(p, &self.current_path))
            .unwrap_or(0);
        self.search_scroll.scroll_to_item(self.search_cursor, ScrollStrategy::Top);
        cx.notify();
    }

    fn visible_dir_entries(&self) -> Vec<WorkspaceEntry> {
        let mut entries: Vec<WorkspaceEntry> = self
            .dir_entries
            .iter()
            .flatten()
            .filter(|e| self.show_hidden || !is_hidden_name(&e.name))
            .cloned()
            .collect();
        entries.sort_by(|a, b| {
            if a.kind != b.kind {
                return if a.kind == WorkspaceEntryKind::Dir {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        });
        entries
    }

    fn navigable_files(&self, cx: &App) -> Vec<String> {
        if self.is_searching(cx) {
            self.search_hits.clone()
        } else {
            self.visible_dir_entries()
                .into_iter()
                .filter(|e| e.kind == WorkspaceEntryKind::File)
                .map(|e| e.path)
                .collect()
        }
    }

    fn step_bounds(&self, cx: &App) -> (bool, bool) {
        let files = self.navigable_files(cx);
        step_availability(&files, |p| self.same_file(p, &self.current_path))
    }

    fn step_file(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.navigable_files(cx);
        let Some(next) = step_neighbor(&files, |p| self.same_file(p, &self.current_path), delta) else {
            return;
        };
        self.select_file(next, window, cx);
    }

    fn navigate_dir(&mut self, next_cwd: String, cx: &mut Context<Self>) {
        if self.cwd == next_cwd {
            return;
        }
        self.cwd = next_cwd;
        self.dir_scroll = UniformListScrollHandle::new();
        self.load_dir(cx);
    }

    fn select_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.same_file(&self.current_path, &path) {
            return;
        }
        if self.file_view.read(cx).dirty() {
            let weak = cx.weak_entity();
            let target = path.clone();
            let name = basename(&self.current_path).to_string();
            self.confirm_discard_popup(&name, move |_, cx| {
                weak.update(cx, |this, cx| {
                    this.pending_path = Some(target.clone());
                    cx.notify();
                })
                .ok();
            }, window, cx);
            return;
        }
        self.apply_select_file(path, window, cx);
    }

    fn apply_select_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.current_path = path.clone();
        let pid = self.project_id.clone();
        self.file_view.update(cx, |v, cx| v.set_target(pid, path.clone(), cx));
        if self.is_searching(cx) {
            if let Some(ix) = self.search_hits.iter().position(|p| self.same_file(p, &path)) {
                self.search_cursor = ix;
                self.search_scroll.scroll_to_item(ix, ScrollStrategy::Top);
            }
        } else {
            let entries = self.visible_dir_entries();
            if let Some(ix) = entries.iter().position(|e| e.kind == WorkspaceEntryKind::File && self.same_file(&e.path, &path)) {
                self.dir_scroll.scroll_to_item(ix, ScrollStrategy::Top);
            }
        }
        if let Some(on_select) = self.on_select.clone() {
            let cwd = if self.is_searching(cx) {
                parent_nav_path(&self.host_path_of(&path)).unwrap_or_else(|| self.cwd.clone())
            } else {
                self.cwd.clone()
            };
            on_select(&cwd, &path, window, cx);
        }
        cx.notify();
    }

    fn activate_search_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_searching(cx) {
            return;
        }
        if let Some(path) = self.search_hits.get(self.search_cursor).cloned() {
            self.select_file(path, window, cx);
        }
    }

    fn on_search_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "escape" if self.is_searching(cx) => {
                cx.stop_propagation();
                self.search.update(cx, |i, cx| i.set_value("", window, cx));
                self.refilter(cx);
            }
            "down" => {
                if self.is_searching(cx) {
                    let n = self.search_hits.len();
                    if n > 0 {
                        self.search_cursor = (self.search_cursor + 1) % n;
                        self.search_scroll.scroll_to_item(self.search_cursor, ScrollStrategy::Top);
                        cx.stop_propagation();
                        cx.notify();
                    }
                } else {
                    cx.stop_propagation();
                    self.step_file(1, window, cx);
                }
            }
            "up" => {
                if self.is_searching(cx) {
                    let n = self.search_hits.len();
                    if n > 0 {
                        self.search_cursor = (self.search_cursor + n - 1) % n;
                        self.search_scroll.scroll_to_item(self.search_cursor, ScrollStrategy::Top);
                        cx.stop_propagation();
                        cx.notify();
                    }
                } else {
                    cx.stop_propagation();
                    self.step_file(-1, window, cx);
                }
            }
            _ => {}
        }
    }

    /// 浮层关闭前检查未保存修改；返回 true 允许当场关闭，false 则留在浮层等待确认结果
    fn try_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.force_close || !self.file_view.read(cx).dirty() {
            return true;
        }
        let weak = cx.weak_entity();
        let name = basename(&self.current_path).to_string();
        self.confirm_discard_popup(
            &name,
            move |window, cx| {
                weak.update(cx, |this, _| this.force_close = true).ok();
                window.defer(cx, |window, cx| window.close_dialog(cx));
            },
            window,
            cx,
        );
        false
    }

    fn confirm_discard_popup(
        &mut self,
        name: &str,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_view.update(cx, |v, cx| v.set_subdialog(true, cx));
        let fv_ok = self.file_view.downgrade();
        let fv_cancel = self.file_view.downgrade();
        let on_confirm = Rc::new(on_confirm);
        let body_text = t!("files.discardBody", name = name).to_string();
        window.open_dialog(cx, move |dialog, _, cx| {
            let ui = Ui::global(cx).clone();
            let on_confirm = on_confirm.clone();
            let fv_ok = fv_ok.clone();
            let fv_cancel = fv_cancel.clone();
            dialog
                .title(t!("files.discardTitle").to_string())
                .w(zpx(440.))
                .child(div().text_sm().text_color(ui.muted_foreground).child(body_text.clone()))
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                        .child(
                            DialogAction::new().child(
                                Button::new("popup-discard-ok")
                                    .danger()
                                    .label(t!("files.discardConfirm").to_string()),
                            ),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    fv_ok.update(cx, |v, cx| v.set_subdialog(false, cx)).ok();
                    on_confirm(window, cx);
                    true
                })
                .on_cancel(move |_, _, cx| {
                    fv_cancel.update(cx, |v, cx| v.set_subdialog(false, cx)).ok();
                    true
                })
        });
    }

    fn render_root_menu(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.roots.is_empty() {
            return None;
        }
        let roots = self.roots.clone();
        let current = self.cwd.clone();
        let weak = cx.weak_entity();
        Some(
            Button::new("file-popup-roots")
                .outline()
                .xsmall()
                .label(t!("files.projectFolders").to_string())
                .dropdown_caret(true)
                .w_full()
                .mb_2()
                .dropdown_menu(move |mut menu, _, _| {
                    for root in &roots {
                        let (weak, root) = (weak.clone(), root.clone());
                        let checked = parse_nav_path(&root, None).as_deref() == Some(current.as_str());
                        menu = menu.item(PopupMenuItem::new(root.clone()).checked(checked).on_click(move |_, _, cx| {
                            weak.update(cx, |this, cx| {
                                if let Some(path) = parse_nav_path(&root, None) {
                                    this.navigate_dir(path, cx);
                                }
                            })
                            .ok();
                        }));
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let searching = self.is_searching(cx);

        let search_box = div()
            .capture_key_down(cx.listener(Self::on_search_key))
            .child(
                Input::new(&self.search)
                    .small()
                    .cleanable(true)
                    .prefix(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground)),
            );

        let content: AnyElement = if searching {
            self.render_search_list(&ui, cx)
        } else {
            self.render_dir_browser(&ui, cx)
        };

        div()
            .id("file-popup-sidebar")
            .w(zpx(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(ui.app.opacity(0.6))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .children(if searching { None } else { self.render_root_menu(cx) })
                    .child(search_box),
            )
            .child(content)
    }

    fn render_dir_browser(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let host_cwd = self.host_path_of(&self.cwd);
        let parent = parent_nav_path(&host_cwd);
        let cwd_label = host_cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&host_cwd)
            .to_string();
        let cwd_tip: SharedString = host_cwd.into();
        let hidden_tip = if self.show_hidden { t!("files.hideHidden") } else { t!("files.showHidden") };

        let nav = div()
            .flex_none()
            .h(zpx(28.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .child(
                mini_icon_btn("fp-up", IconName::ArrowLeft, t!("files.parent"), parent.is_none(), false, false, ui)
                    .when_some(parent, |b, p| b.on_click(cx.listener(move |this, _, _, cx| this.navigate_dir(p.clone(), cx)))),
            )
            .child(
                div()
                    .id("fp-cwd")
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .px_1()
                    .text_size(zpx(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.muted_foreground)
                    .tooltip(move |window, cx| Tooltip::new(cwd_tip.clone()).build(window, cx))
                    .child(cwd_label),
            )
            .child(
                mini_icon_btn(
                    "fp-hidden",
                    if self.show_hidden { IconName::Eye } else { IconName::EyeOff },
                    hidden_tip,
                    false,
                    self.show_hidden,
                    false,
                    ui,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_hidden = !this.show_hidden;
                    cx.notify();
                })),
            )
            .child(
                mini_icon_btn("fp-refresh", IconName::RefreshCw, t!("files.refresh"), false, false, self.dir_loading, ui)
                    .on_click(cx.listener(|this, _, _, cx| this.load_dir(cx))),
            );

        let entries = Rc::new(self.visible_dir_entries());
        let list_body: AnyElement = if let (Some(err), None) = (&self.dir_error, &self.dir_entries) {
            sidebar_note(t!("files.loadFailed").to_string(), Some(err.clone()), ui)
        } else if self.dir_loading && self.dir_entries.is_none() {
            sidebar_note(t!("files.loading").to_string(), None, ui)
        } else if entries.is_empty() {
            sidebar_note(t!("files.empty").to_string(), None, ui)
        } else {
            let items = entries.clone();
            uniform_list(
                "file-popup-dir-list",
                items.len(),
                cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    let ui = Ui::global(cx).clone();
                    range
                        .filter_map(|i| items.get(i).map(|entry| this.render_dir_row(i, entry, &ui, cx)))
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .px_2()
            .py_1()
            .track_scroll(&self.dir_scroll)
            .into_any_element()
        };

        let mut col = div().flex_1().min_h_0().flex().flex_col().child(nav).child(list_body);
        if self.dir_truncated {
            let n = self.dir_entries.as_ref().map_or(0, Vec::len);
            col = col.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(t!("files.truncated", n = n).to_string()),
            );
        }
        col.into_any_element()
    }

    fn render_dir_row(&self, ix: usize, entry: &WorkspaceEntry, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let is_dir = entry.kind == WorkspaceEntryKind::Dir;
        let active = !is_dir && self.same_file(&entry.path, &self.current_path);
        let glyph = if is_dir { IconName::Folder } else { icon_for(&entry.name) };
        let path = entry.path.clone();
        let tip: SharedString = entry.path.clone().into();
        let hover_bg = ui.background.opacity(0.7);
        let fg = ui.foreground;

        let mut row = div()
            .id(SharedString::from(format!("fp-dir-row-{ix}")))
            .h(zpx(ROW_H))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap(zpx(6.))
            .rounded(radius::SM)
            .cursor_pointer()
            .text_size(zpx(12.))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                if is_dir {
                    this.navigate_dir(path.clone(), cx);
                } else {
                    this.select_file(path.clone(), window, cx);
                }
            }))
            .child(
                icon(glyph)
                    .size(zpx(14.))
                    .flex_none()
                    .text_color(if active { ui.tint_foreground } else { ui.muted_foreground }),
            )
            .child(div().flex_1().min_w_0().truncate().child(entry.name.clone()));

        if !is_dir && let Some(size) = entry.size {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(zpx(10.5))
                    .text_color(if active { ui.tint_foreground.opacity(0.8) } else { ui.muted_foreground })
                    .child(format_size(Some(size as f64))),
            );
        }

        if active {
            row.bg(ui.tint).text_color(ui.tint_foreground).font_weight(FontWeight::MEDIUM).into_any_element()
        } else {
            row.text_color(fg).hover(move |s| s.bg(hover_bg)).into_any_element()
        }
    }

    fn render_search_list(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        if let Some(err) = &self.index_error {
            return sidebar_note(t!("quickOpen.failed").to_string(), Some(err.clone()), ui);
        }
        if self.index_paths.is_none() {
            return sidebar_note(t!("quickOpen.loading").to_string(), None, ui);
        }
        if self.search_hits.is_empty() {
            return sidebar_note(t!("files.noMatchingFiles").to_string(), None, ui);
        }
        let hits = Rc::new(self.search_hits.clone());
        let cursor = self.search_cursor;
        let list = uniform_list(
            "file-popup-search-list",
            hits.len(),
            cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                let ui = Ui::global(cx).clone();
                range
                    .filter_map(|i| {
                        let path = hits.get(i)?.clone();
                        let active = this.same_file(&path, &this.current_path);
                        let focused = i == cursor;
                        let base = basename(&path).to_string();
                        let dir = dirname(&path).to_string();
                        let tip: SharedString = path.clone().into();
                        let hover_bg = ui.background.opacity(0.7);
                        let mut row = div()
                            .id(SharedString::from(format!("fp-hit-{i}")))
                            .h(zpx(ROW_H))
                            .w_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .gap(zpx(6.))
                            .rounded(radius::SM)
                            .cursor_pointer()
                            .text_size(zpx(12.))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| this.select_file(path.clone(), window, cx)))
                            .child(
                                icon(icon_for(&base))
                                    .size(zpx(14.))
                                    .flex_none()
                                    .text_color(if active { ui.tint_foreground } else { ui.muted_foreground }),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(base));
                        if !dir.is_empty() {
                            row = row.child(
                                div()
                                    .flex_none()
                                    .max_w(zpx(92.))
                                    .truncate()
                                    .text_size(zpx(10.5))
                                    .text_color(if active { ui.tint_foreground.opacity(0.8) } else { ui.muted_foreground })
                                    .child(dir),
                            );
                        }
                        let row = if active {
                            row.bg(ui.tint).text_color(ui.tint_foreground).font_weight(FontWeight::MEDIUM)
                        } else if focused {
                            row.bg(ui.muted).text_color(ui.foreground)
                        } else {
                            row.text_color(ui.foreground).hover(move |s| s.bg(hover_bg))
                        };
                        Some(row.into_any_element())
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .px_2()
        .py_1()
        .track_scroll(&self.search_scroll);

        let mut col = div().flex_1().min_h_0().flex().flex_col().child(list);
        if self.index_truncated {
            let n = self.index_paths.as_ref().map_or(0, Vec::len);
            col = col.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(t!("quickOpen.truncated", n = n).to_string()),
            );
        }
        col.into_any_element()
    }
}

impl Render for FilePreviewPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(next) = self.pending_path.take() {
            self.apply_select_file(next, window, cx);
        }
        let (has_prev, has_next) = self.step_bounds(cx);
        let sidebar_open = self.sidebar_open;
        let (weak_toggle, weak_step, weak_close) = (cx.weak_entity(), cx.weak_entity(), cx.weak_entity());
        self.file_view.update(cx, |v, _| {
            v.set_popup_chrome(FilePopupChrome {
                sidebar_open,
                has_prev,
                has_next,
                on_toggle_sidebar: Rc::new(move |_, cx| {
                    weak_toggle
                        .update(cx, |this, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        })
                        .ok();
                }),
                on_step: Rc::new(move |delta, window, cx| {
                    weak_step.update(cx, |this, cx| this.step_file(delta, window, cx)).ok();
                }),
                on_close: Rc::new(move |window, cx| {
                    let can_close = weak_close
                        .update(cx, |this, cx| this.try_close(window, cx))
                        .unwrap_or(true);
                    if can_close {
                        window.close_dialog(cx);
                    }
                }),
            });
        });

        let ui = Ui::global(cx).clone();
        let mut root = div()
            .id("file-preview-popup")
            .size_full()
            .flex()
            .rounded(radius::LG)
            .overflow_hidden()
            .bg(ui.background)
            .text_color(ui.foreground);
        if self.sidebar_open {
            root = root.child(self.render_sidebar(cx));
        }
        root.child(div().flex_1().min_w_0().h_full().child(self.file_view.clone()))
    }
}

// ============================================================================
// 差异预览放大浮层
// ============================================================================

pub struct DiffPopupInit {
    pub project_id: String,
    pub repo: Option<String>,
    pub repo_name: Option<String>,
    pub initial_files: Vec<FileItem>,
    pub target: DiffTabTarget,
    /// 在浮层中切换了改动文件时通知右侧「修改」面板
    pub on_select: Option<Rc<dyn Fn(&DiffTabTarget, &mut App)>>,
}

pub fn open_diff(ws: &Entity<Workspace>, init: DiffPopupInit, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| DiffPreviewPopup::new(ws.clone(), init, window, cx));
    let focus = view.read(cx).search.clone();
    open_popup_dialog(view, |_, _| true, focus, window, cx);
}

struct DiffPreviewPopup {
    ws: Entity<Workspace>,
    project_id: String,
    repo: Option<String>,
    repo_name: Option<String>,
    files: Vec<FileItem>,
    busy: bool,
    error: Option<String>,
    seq: u64,
    collapsed: HashSet<String>,
    sidebar_open: bool,
    search: Entity<InputState>,
    active_path: Option<String>,
    diff_view: Entity<DiffView>,
    list_scroll: UniformListScrollHandle,
    on_select: Option<Rc<dyn Fn(&DiffTabTarget, &mut App)>>,
    _subs: Vec<Subscription>,
}

impl DiffPreviewPopup {
    fn new(ws: Entity<Workspace>, init: DiffPopupInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("git.filterChangedFiles").to_string()));
        let active_path = Some(init.target.file.path.clone());
        let target = init.target;
        let diff_view = cx.new(|cx| {
            let mut v = DiffView::new_local(ws.clone(), cx);
            v.set_local(Some(target), cx);
            v
        });
        let subs = vec![
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    let filtered = this.filtered_files(cx);
                    if let Some(first) = filtered.first()
                        && this.active_path.as_deref() != Some(first.path.as_str())
                    {
                        this.select_item(first.clone(), window, cx);
                    }
                }
                _ => {}
            }),
            cx.observe(&ws, |this, _, cx| this.sync_repo(cx)),
        ];
        let need_load = init.initial_files.is_empty();
        let mut this = Self {
            ws,
            project_id: init.project_id,
            repo: init.repo,
            repo_name: init.repo_name,
            files: init.initial_files,
            busy: false,
            error: None,
            seq: 0,
            collapsed: HashSet::new(),
            sidebar_open: true,
            search,
            active_path,
            diff_view,
            list_scroll: UniformListScrollHandle::new(),
            on_select: init.on_select,
            _subs: subs,
        };
        if need_load {
            this.load_working(false, cx);
        }
        this
    }

    fn sync_repo(&mut self, cx: &mut Context<Self>) {
        let next_repo = {
            let w = self.ws.read(cx);
            falcon_core::workspace::select_multi_repo_dir(&w.state.multi_repo, w.project(&self.project_id))
        };
        if next_repo != self.repo {
            self.repo = next_repo;
            self.files.clear();
            self.repo_name = None;
            self.collapsed.clear();
            self.active_path = None;
            self.diff_view.update(cx, |v, cx| v.set_local(None, cx));
            self.load_working(true, cx);
            cx.notify();
        }
    }

    fn load_working(&mut self, auto_pick_first: bool, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        self.busy = true;
        self.error = None;
        let pid = self.project_id.clone();
        let repo = self.repo.clone();
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.git_working(&pid, repo.as_deref()).await;
            this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(next) => {
                        this.repo_name = next.repo_name;
                        this.files = next
                            .files
                            .iter()
                            .map(|f| FileItem {
                                path: f.path.clone(),
                                orig_path: f.orig_path.clone(),
                                status: porcelain_status(&f.index, &f.work),
                                added: f.added,
                                deleted: f.deleted,
                            })
                            .collect();
                        this.error = None;
                        let still_exists = this
                            .active_path
                            .as_ref()
                            .is_some_and(|p| this.files.iter().any(|f| &f.path == p));
                        if !still_exists || auto_pick_first {
                            if let Some(first) = this.files.first().cloned() {
                                this.apply_item(first, cx);
                            } else {
                                this.active_path = None;
                                this.diff_view.update(cx, |v, cx| v.set_local(None, cx));
                            }
                        }
                    }
                    Err(err) => {
                        this.error = Some(err.to_string());
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.load_working(false, cx);
        self.diff_view.update(cx, |v, cx| v.refresh_now(cx));
    }

    fn filtered_files(&self, cx: &App) -> Vec<FileItem> {
        let query = self.search.read(cx).value();
        filter_changed_files(&self.files, query.as_ref())
    }

    fn step_bounds(&self, cx: &App) -> (bool, bool) {
        let filtered = self.filtered_files(cx);
        let Some(active) = self.active_path.as_deref() else {
            return (false, !filtered.is_empty());
        };
        step_availability(&filtered, |f| f.path == active)
    }

    fn step_file(&mut self, delta: isize, _window: &mut Window, cx: &mut Context<Self>) {
        let filtered = self.filtered_files(cx);
        let next = match self.active_path.as_deref() {
            Some(active) => step_neighbor(&filtered, |f| f.path == active, delta),
            None => (delta > 0).then(|| filtered.first().cloned()).flatten(),
        };
        if let Some(item) = next {
            self.apply_item(item, cx);
        }
    }

    fn select_item(&mut self, item: FileItem, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_item(item, cx);
    }

    fn apply_item(&mut self, item: FileItem, cx: &mut Context<Self>) {
        let target = DiffTabTarget {
            project_id: self.project_id.clone(),
            file: GitFileChange {
                path: item.path.clone(),
                orig_path: item.orig_path,
                index: item.status,
                work: " ".into(),
            },
            repo: self.repo.clone(),
        };
        self.active_path = Some(item.path);
        self.diff_view.update(cx, |v, cx| v.set_local(Some(target.clone()), cx));
        if let Some(on_select) = self.on_select.clone() {
            on_select(&target, cx);
        }
        cx.notify();
    }

    fn on_search_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let non_empty = !self.search.read(cx).value().trim().is_empty();
        match e.keystroke.key.as_str() {
            "escape" if non_empty => {
                cx.stop_propagation();
                self.search.update(cx, |i, cx| i.set_value("", window, cx));
                cx.notify();
            }
            "down" => {
                cx.stop_propagation();
                self.step_file(1, window, cx);
            }
            "up" => {
                cx.stop_propagation();
                self.step_file(-1, window, cx);
            }
            _ => {}
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let project = self.ws.read(cx).project(&self.project_id).cloned();
        let filtered = Rc::new(self.filtered_files(cx));
        let query_non_empty = !self.search.read(cx).value().trim().is_empty();
        // 输入筛选词时改用平铺列表，按打分高低排在最上面，不用点开层层目录找
        let mode = if query_non_empty {
            common::FileViewMode::List
        } else {
            file_view_mode(cx)
        };
        let rows: Rc<Vec<FileRow>> = Rc::new(build_rows(&filtered, mode, self.repo_name.as_deref(), &self.collapsed));

        let mut top = div().flex_none().px_3().pt_3().pb_2().flex().flex_col().gap_2();
        if let Some(sel) = common::multi_repo_select("diff-popup", &self.ws, project.as_ref(), cx) {
            top = top.child(sel);
        }
        top = top.child(
            div()
                .capture_key_down(cx.listener(Self::on_search_key))
                .child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground)),
                ),
        );

        let bar = div()
            .flex_none()
            .h(zpx(28.))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(t!("git.nFiles", n = filtered.len()).to_string()),
            )
            .child(file_view_toggle("diff-popup", cx))
            .child(
                git_tool_button("diff-popup-refresh-list", IconName::RefreshCw, t!("git.refresh").to_string(), false, cx)
                    .loading(self.busy)
                    .loading_icon(IconName::RefreshCw)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
            );

        let body: AnyElement = if let Some(err) = &self.error {
            git_hint(t!("changes.loadFailed").to_string(), Some(err.clone()), cx).into_any_element()
        } else if self.busy && self.files.is_empty() {
            git_hint(t!("git.loading").to_string(), None, cx).into_any_element()
        } else if self.files.is_empty() {
            git_hint(t!("changes.clean").to_string(), None, cx).into_any_element()
        } else if filtered.is_empty() {
            git_hint(t!("git.noMatchingFiles").to_string(), None, cx).into_any_element()
        } else {
            let active = self.active_path.clone();
            let items = filtered.clone();
            let rows_ref = rows.clone();
            uniform_list(
                "diff-popup-files",
                rows_ref.len(),
                cx.processor(move |_: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|i| render_file_row("diff-popup", &rows_ref[i], &items, None, active.as_deref(), cx))
                        .collect::<Vec<AnyElement>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .min_h_0()
            .py_1()
            .into_any_element()
        };

        div()
            .id("diff-popup-sidebar")
            .w(zpx(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(ui.app.opacity(0.6))
            .child(top)
            .child(bar)
            .child(body)
    }
}

impl FileListHost for DiffPreviewPopup {
    fn open_file(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let filtered = self.filtered_files(cx);
        if let Some(item) = filtered.get(idx).cloned() {
            self.select_item(item, window, cx);
        }
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_string());
        }
        cx.notify();
    }
}

impl Render for DiffPreviewPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (has_prev, has_next) = self.step_bounds(cx);
        let sidebar_open = self.sidebar_open;
        let (weak_toggle, weak_step) = (cx.weak_entity(), cx.weak_entity());
        self.diff_view.update(cx, |v, _| {
            v.set_popup_chrome(DiffPopupChrome {
                sidebar_open,
                has_prev,
                has_next,
                on_toggle_sidebar: Rc::new(move |_, cx| {
                    weak_toggle
                        .update(cx, |this, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        })
                        .ok();
                }),
                on_step: Rc::new(move |delta, window, cx| {
                    weak_step.update(cx, |this, cx| this.step_file(delta, window, cx)).ok();
                }),
                on_close: Rc::new(|window, cx| window.close_dialog(cx)),
            });
        });

        let ui = Ui::global(cx).clone();
        let mut root = div()
            .id("diff-preview-popup")
            .size_full()
            .flex()
            .rounded(radius::LG)
            .overflow_hidden()
            .bg(ui.background)
            .text_color(ui.foreground);
        if self.sidebar_open {
            root = root.child(self.render_sidebar(cx));
        }
        root.child(div().flex_1().min_w_0().h_full().child(self.diff_view.clone()))
    }
}

// ============================================================================
// 共用小件与纯函数
// ============================================================================

fn mini_icon_btn(
    id: &'static str,
    name: IconName,
    label: impl Into<SharedString>,
    disabled: bool,
    pressed: bool,
    spinning: bool,
    ui: &Ui,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let fg = ui.foreground;
    let muted = ui.muted;
    let glyph: AnyElement = if spinning {
        Spinner::new().icon(name).with_size(zpx(13.)).into_any_element()
    } else {
        icon(name).size(zpx(13.)).into_any_element()
    };
    let base = div()
        .id(id)
        .size(zpx(22.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(5.))
        .text_color(if pressed { ui.foreground } else { ui.muted_foreground })
        .when(pressed, |d| d.bg(muted))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(glyph);
    if disabled {
        return base.opacity(0.45);
    }
    base.cursor_pointer().hover(move |s| s.bg(muted).text_color(fg))
}

fn sidebar_note(text: String, detail: Option<String>, ui: &Ui) -> AnyElement {
    let mut d = div()
        .px_3()
        .py_4()
        .text_xs()
        .line_height(zpx(19.5))
        .text_color(ui.muted_foreground)
        .child(text);
    if let Some(detail) = detail.filter(|s| !s.is_empty()) {
        d = d.child(div().mt_1().text_size(zpx(11.)).child(detail));
    }
    d.into_any_element()
}

/// 当前项在有序列表里能否往前 / 往后翻页
fn step_availability<T>(items: &[T], is_current: impl Fn(&T) -> bool) -> (bool, bool) {
    let Some(ix) = items.iter().position(is_current) else {
        return (false, false);
    };
    (ix > 0, ix + 1 < items.len())
}

/// 从当前项沿 `delta`（`-1` 上一个 / `1` 下一个）取相邻项
fn step_neighbor<T: Clone>(items: &[T], is_current: impl Fn(&T) -> bool, delta: isize) -> Option<T> {
    let ix = items.iter().position(is_current)?;
    let next = ix.checked_add_signed(delta)?;
    items.get(next).cloned()
}

/// 筛选改动文件：空查询保留原顺序（目录树稳定），有查询时按路径搜索权重排序
fn filter_changed_files(files: &[FileItem], query: &str) -> Vec<FileItem> {
    if query.trim().is_empty() {
        return files.to_vec();
    }
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let matched = filter_files(&paths, query, FILE_SEARCH_LIMIT);
    matched
        .into_iter()
        .filter_map(|p| files.iter().find(|f| f.path == p).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(path: &str) -> FileItem {
        FileItem {
            path: path.into(),
            orig_path: None,
            status: "M".into(),
            added: Some(2),
            deleted: Some(1),
        }
    }

    #[test]
    fn step_navigation_respects_boundaries() {
        let items = vec!["a.rs", "b.rs", "c.rs"];
        assert_eq!(step_availability(&items, |x| *x == "a.rs"), (false, true));
        assert_eq!(step_availability(&items, |x| *x == "b.rs"), (true, true));
        assert_eq!(step_availability(&items, |x| *x == "c.rs"), (true, false));
        assert_eq!(step_availability(&items, |x| *x == "missing.rs"), (false, false));

        assert_eq!(step_neighbor(&items, |x| *x == "a.rs", -1), None);
        assert_eq!(step_neighbor(&items, |x| *x == "a.rs", 1), Some("b.rs"));
        assert_eq!(step_neighbor(&items, |x| *x == "c.rs", 1), None);
        assert_eq!(step_neighbor(&items, |x| *x == "c.rs", -1), Some("b.rs"));
    }

    #[test]
    fn filter_changed_preserves_order_when_empty_and_ranks_on_query() {
        let files = vec![
            changed("docs/readme.md"),
            changed("src/panels/files.rs"),
            changed("src/files.rs"),
        ];
        assert_eq!(filter_changed_files(&files, "  "), files);

        let ranked = filter_changed_files(&files, "files.rs");
        assert_eq!(ranked.len(), 2);
        // 同为 basename 精确前缀命中时，更短路径排在前面
        assert_eq!(ranked[0].path, "src/files.rs");
        assert_eq!(ranked[1].path, "src/panels/files.rs");
    }
}
