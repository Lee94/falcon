//! 右侧「文件」面板：项目工作目录的目录浏览器（web 的 components/FilesPanel.tsx，ADR 0009），
//! 附带下载 / 上传（ADR 0008）。
//!
//! 与「修改」面板的分工：那边列的是 git 眼里"这次动了什么"，这边列的是磁盘上真正有什么——
//! 未跟踪的、被 ignore 的、生成出来的，都在。目录可能有几万项，只能按层拉（`GET /files?path=`）。
//!
//! 交互按文件管理器而不是树：顶上路径栏 + 图标工具栏，当前目录平铺成表（复选框 / 名称 /
//! 大小 / 修改时间）。点目录进去、点文件开查看窗口；多选走复选框和 Shift / ⌘ 点击，右键
//! 复制路径 / 下载 / 重命名 / 删除。原生多一件 web 没有的：从访达把文件 / 文件夹拖进来即上传
//! （落在目录行上就传进那个目录）。
//!
//! 宿主机路径一律走 falcon-core 的 file_path（`join_host_path` / `parse_nav_path`），不用
//! `std::path`：宿主机可能是 Linux / Windows，与这台 Mac 无关。

pub(crate) mod transfer;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use falcon_core::file_path::{format_size, is_hidden_name, join_host_path, parent_rel, parse_nav_path};
use falcon_proto::{WorkspaceEntry, WorkspaceEntryKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, Entity, ExternalPaths, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, MouseButton, Pixels, Render, SharedString, Stateful,
    Subscription, UniformListScrollHandle, WeakEntity, Window, canvas, div, px, uniform_list,
};
use rust_i18n::t;

use crate::dialogs::{self, ConfirmItem, ConfirmOpts};
use crate::menus::{MenuItemSpec, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ActiveView, ToastKind, Workspace};
use crate::zoom::zpx;

const ROW_H: f32 = 28.;
const CHECK_COL: f32 = 24.;
/// 3.5rem / 9.5rem：与 web 的表格列宽一致
const SIZE_COL: f32 = 56.;
const MTIME_COL: f32 = 152.;
/// 面板够宽才露出大小 / 修改时间（web 的容器查询 @[300px] / @[360px]，ADR 0009 决定二）
const SHOW_SIZE_AT: f32 = 300.;
const SHOW_MTIME_AT: f32 = 360.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Mtime,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortDir {
    Asc,
    Desc,
}

#[derive(Default)]
struct Listing {
    loading: bool,
    entries: Option<Vec<WorkspaceEntry>>,
    truncated: bool,
    error: Option<String>,
}

enum DraftKind {
    Mkdir,
    Rename { path: String },
}

/// 就地编辑的名字：新建文件夹那一行，或正在改名的那一行
struct Draft {
    kind: DraftKind,
    input: Entity<InputState>,
    /// 请求在路上：回车之后紧跟着的失焦不能再提交一次（否则第二次 mkdir 撞 409）
    busy: bool,
    _sub: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Off,
    On,
    Mixed,
}

pub struct FilesPanel {
    ws: Entity<Workspace>,
    project_id: Option<String>,
    working_dir: Option<String>,
    /// 当前目录（工作目录相对，`""` 是工作目录本身）
    cwd: String,
    show_hidden: bool,
    sort: (SortKey, SortDir),
    selected: HashSet<String>,
    last_clicked: Option<String>,
    draft: Option<Draft>,
    /// Esc 取消时紧跟着的失焦不算提交
    ignore_blur: bool,
    listing: Listing,
    /// 排好序、滤过隐藏文件的当前目录。每帧都要用（表头、工具栏、可见的那几行），而排序
    /// 两千项要上千次小写化比较，所以只在列表 / 排序 / 显隐变化时重算
    rows_cache: RefCell<Option<Rc<Vec<WorkspaceEntry>>>>,
    /// 过期响应的闸门：换目录 / 换项目后，前一次还在路上的列表回来了也不认
    generation: u64,
    path_input: Entity<InputState>,
    width: Pixels,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl FilesPanel {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_input = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&path_input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_path(window, cx),
                _ => {}
            }),
            cx.observe_in(&ws, window, |this, _, window, cx| this.sync_project(window, cx)),
        ];
        let mut this = Self {
            ws,
            project_id: None,
            working_dir: None,
            cwd: String::new(),
            show_hidden: false,
            sort: (SortKey::Name, SortDir::Asc),
            selected: HashSet::new(),
            last_clicked: None,
            draft: None,
            ignore_blur: false,
            listing: Listing { loading: true, ..Default::default() },
            rows_cache: RefCell::new(None),
            generation: 0,
            path_input,
            width: px(0.),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.sync_project(window, cx);
        this
    }

    // ---------------- 数据 ----------------

    /// 跟着工作区的"焦点项目"走（侧栏选中的项目优先，否则当前窗口所属项目）
    fn sync_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (pid, wd) = {
            let ws = self.ws.read(cx);
            let pid = ws.focus_project_id();
            let wd = pid.as_deref().and_then(|p| ws.project(p)).and_then(|p| p.working_dir.clone());
            (pid, wd)
        };
        if pid != self.project_id {
            self.project_id = pid;
            self.working_dir = wd;
            self.cwd.clear();
            self.selected.clear();
            self.draft = None;
            self.last_clicked = None;
            self.listing = Listing { loading: self.project_id.is_some(), ..Default::default() };
            self.invalidate_rows();
            self.scroll = UniformListScrollHandle::new();
            self.reload(cx);
            self.reset_path_input(window, cx);
        } else if wd != self.working_dir {
            self.working_dir = wd;
            self.reset_path_input(window, cx);
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else {
            self.listing = Listing::default();
            self.invalidate_rows();
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        // 旧条目留着：刷新时列表不闪空
        self.listing.loading = true;
        self.listing.error = None;
        let cwd = self.cwd.clone();
        let fut = self.ws.read(cx).client.list_files(&pid, (!cwd.is_empty()).then_some(cwd.as_str()));
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(listing) => {
                        let live: HashSet<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
                        this.selected.retain(|p| live.contains(p.as_str()));
                        this.listing = Listing {
                            loading: false,
                            entries: Some(listing.entries),
                            truncated: listing.truncated,
                            error: None,
                        };
                        this.invalidate_rows();
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.listing = Listing { loading: false, entries: None, truncated: false, error: Some(err.to_string()) };
                        this.invalidate_rows();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn host_path(&self) -> String {
        join_host_path(self.working_dir.as_deref(), &self.cwd)
    }

    fn reset_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = if self.project_id.is_some() { self.host_path() } else { String::new() };
        self.path_input.update(cx, |i, cx| {
            if i.value().as_ref() != value {
                i.set_value(value, window, cx);
            }
        });
    }

    fn go_to(&mut self, next: String, window: &mut Window, cx: &mut Context<Self>) {
        self.cwd = next;
        self.selected.clear();
        self.draft = None;
        self.last_clicked = None;
        self.scroll = UniformListScrollHandle::new();
        self.reload(cx);
        self.reset_path_input(window, cx);
    }

    /// 路径栏回车 / 失焦：越界的输入立刻弹回去，而不是先跳再吃一个 400（ADR 0009 决定一）
    fn commit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_id.is_none() {
            return;
        }
        let draft = self.path_input.read(cx).value().to_string();
        match parse_nav_path(&draft, self.working_dir.as_deref()) {
            None => {
                self.ws.update(cx, |w, cx| w.toast(ToastKind::Danger, t!("files.pathOutside").to_string(), None, cx));
                self.reset_path_input(window, cx);
            }
            Some(next) if next == self.cwd => self.reset_path_input(window, cx),
            Some(next) => self.go_to(next, window, cx),
        }
    }

    fn invalidate_rows(&self) {
        self.rows_cache.borrow_mut().take();
    }

    fn visible(&self) -> Rc<Vec<WorkspaceEntry>> {
        if let Some(rows) = self.rows_cache.borrow().as_ref() {
            return rows.clone();
        }
        let rows = Rc::new(self.compute_visible());
        *self.rows_cache.borrow_mut() = Some(rows.clone());
        rows
    }

    /// 当前要显示的条目：藏不藏点开头的、按什么排。目录永远在前
    fn compute_visible(&self) -> Vec<WorkspaceEntry> {
        let mut v: Vec<WorkspaceEntry> = self
            .listing
            .entries
            .iter()
            .flatten()
            .filter(|e| self.show_hidden || !is_hidden_name(&e.name))
            .cloned()
            .collect();
        let (key, dir) = self.sort;
        v.sort_by(|a, b| {
            if a.kind != b.kind {
                return if a.kind == WorkspaceEntryKind::Dir { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            // localeCompare(sensitivity: base) 的近似：不分大小写；相等时保持服务端给的顺序
            let c = match key {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Size => a.size.map_or(-1, |s| s as i128).cmp(&b.size.map_or(-1, |s| s as i128)),
                SortKey::Mtime => a.mtime.unwrap_or(0).cmp(&b.mtime.unwrap_or(0)),
            };
            if dir == SortDir::Asc { c } else { c.reverse() }
        });
        v
    }

    fn entry(&self, path: &str) -> Option<&WorkspaceEntry> {
        self.listing.entries.as_ref()?.iter().find(|e| e.path == path)
    }

    /// 当前目录里已有的名字（上传前先问"覆盖吗"用）
    fn names_in(&self, dir: &str) -> HashSet<String> {
        if dir != self.cwd {
            return HashSet::new();
        }
        self.listing.entries.iter().flatten().map(|e| e.name.clone()).collect()
    }

    // ---------------- 选择 ----------------

    fn toggle_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == key {
            (key, if self.sort.1 == SortDir::Asc { SortDir::Desc } else { SortDir::Asc })
        } else {
            // 名字默认 A→Z，大小 / 时间默认大 / 新的在前
            (key, if key == SortKey::Name { SortDir::Asc } else { SortDir::Desc })
        };
        self.invalidate_rows();
        cx.notify();
    }

    fn toggle_one(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.selected.remove(path) {
            self.selected.insert(path.to_string());
        }
        self.last_clicked = Some(path.to_string());
        cx.notify();
    }

    fn select_range(&mut self, path: &str, additive: bool, cx: &mut Context<Self>) {
        let visible = self.visible();
        let idx = visible.iter().position(|e| e.path == path);
        let from = match &self.last_clicked {
            Some(last) => visible.iter().position(|e| &e.path == last),
            None => idx,
        };
        let (Some(idx), Some(from)) = (idx, from) else {
            self.toggle_one(path, cx);
            return;
        };
        let (lo, hi) = (from.min(idx), from.max(idx));
        if !additive {
            self.selected.clear();
        }
        for e in &visible[lo..=hi] {
            self.selected.insert(e.path.clone());
        }
        cx.notify();
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible();
        let all = !visible.is_empty() && visible.iter().all(|e| self.selected.contains(&e.path));
        self.selected = if all { HashSet::new() } else { visible.iter().map(|e| e.path.clone()).collect() };
        cx.notify();
    }

    fn on_row_click(&mut self, path: &str, e: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if e.click_count() >= 2 {
            if let Some(entry) = self.entry(path).cloned() {
                self.activate(&entry, window, cx);
            }
            return;
        }
        let m = e.modifiers();
        let additive = if cfg!(target_os = "macos") { m.platform } else { m.control || m.platform };
        if m.shift {
            self.select_range(path, additive, cx);
        } else if additive {
            self.toggle_one(path, cx);
        } else {
            self.selected = HashSet::from([path.to_string()]);
            self.last_clicked = Some(path.to_string());
            cx.notify();
        }
    }

    /// 点名字 / 双击：目录进去，文件在画布上开查看窗口（预览语义：换内容不新开）
    fn activate(&mut self, entry: &WorkspaceEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = HashSet::from([entry.path.clone()]);
        self.last_clicked = Some(entry.path.clone());
        if entry.kind == WorkspaceEntryKind::Dir {
            self.go_to(entry.path.clone(), window, cx);
        } else if let Some(pid) = self.project_id.clone() {
            let path = entry.path.clone();
            self.ws.update(cx, |w, cx| w.open_file(&pid, &path, cx));
            cx.notify();
        }
    }

    // ---------------- 动作 ----------------

    fn copy_paths(&mut self, paths: &[String], cx: &mut Context<Self>) {
        let text = paths
            .iter()
            .map(|p| join_host_path(self.working_dir.as_deref(), p))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.ws.update(cx, |w, cx| w.toast(ToastKind::Success, t!("files.copiedPath").to_string(), None, cx));
    }

    /// 只下载文件；文件夹跳过并提示——打包是终端里的事（ADR 0009）
    fn download_paths(&mut self, paths: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else { return };
        let files: Vec<String> = paths
            .iter()
            .filter(|p| self.entry(p).is_some_and(|e| e.kind == WorkspaceEntryKind::File))
            .cloned()
            .collect();
        let dirs = paths.len() - files.len();
        transfer::download(self.ws.clone(), pid, files, window, cx);
        if dirs > 0 {
            self.ws.update(cx, |w, cx| {
                w.toast(ToastKind::Info, t!("files.downloadSkipDirs", count = dirs).to_string(), None, cx)
            });
        }
    }

    fn after_upload(this: WeakEntity<Self>) -> impl FnOnce(bool, &mut Window, &mut App) + 'static {
        move |changed, _, cx| {
            if changed {
                this.update(cx, |p, cx| p.reload(cx)).ok();
            }
        }
    }

    fn upload_to(&mut self, dir: String, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else { return };
        let existing = self.names_in(&dir);
        let done = Self::after_upload(cx.weak_entity());
        transfer::pick_and_upload(self.ws.clone(), pid, dir, folder, existing, done, window, cx);
    }

    /// 从访达拖进来的文件 / 文件夹
    fn upload_dropped(&mut self, dir: String, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else { return };
        let existing = self.names_in(&dir);
        let done = Self::after_upload(cx.weak_entity());
        transfer::upload_local(self.ws.clone(), pid, dir, paths.paths().to_vec(), existing, done, window, cx);
    }

    fn remove_paths(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_id.is_none() || paths.is_empty() {
            return;
        }
        let list = self
            .listing
            .entries
            .iter()
            .flatten()
            .filter(|e| paths.contains(&e.path))
            .map(|e| ConfirmItem {
                name: e.name.clone(),
                state: None,
                meta: Some(if e.kind == WorkspaceEntryKind::Dir {
                    t!("files.folder").to_string()
                } else {
                    format_size(e.size.map(|s| s as f64))
                }),
            })
            .collect();
        let this = cx.weak_entity();
        dialogs::confirm(
            ConfirmOpts {
                title: t!("files.deleteTitle", count = paths.len()).to_string(),
                body: t!("files.deleteBody").to_string(),
                list,
                footnote: None,
                confirm_label: t!("files.delete").to_string(),
                danger: true,
            },
            move |_, cx| {
                let paths = paths.clone();
                this.update(cx, |p, cx| p.do_remove(paths, cx)).ok();
            },
            window,
            cx,
        );
    }

    fn do_remove(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else { return };
        let fut = self.ws.read(cx).client.remove_files(&pid, &paths);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                let ws = this.ws.clone();
                match result {
                    Ok(res) => {
                        close_matching_file(&ws, &pid, &res.removed, cx);
                        let first_err = res.errors.first().map(|e| e.error.clone());
                        ws.update(cx, |w, cx| {
                            if res.errors.is_empty() {
                                w.toast(ToastKind::Success, t!("files.deleted", count = res.removed.len()).to_string(), None, cx);
                            } else if !res.removed.is_empty() {
                                let title = t!("files.deletePartial", ok = res.removed.len(), fail = res.errors.len()).to_string();
                                w.toast(ToastKind::Danger, title, first_err, cx);
                            } else {
                                w.toast(ToastKind::Danger, t!("files.deleteFailed").to_string(), first_err, cx);
                            }
                        });
                        if !res.removed.is_empty() {
                            this.reload(cx);
                        }
                    }
                    Err(err) => ws.update(cx, |w, cx| {
                        w.handle_error(&err, cx);
                        w.toast(ToastKind::Danger, t!("files.deleteFailed").to_string(), Some(err.to_string()), cx);
                    }),
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_draft(&mut self, kind: DraftKind, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.commit_draft(cx),
            InputEvent::Blur => {
                if std::mem::take(&mut this.ignore_blur) {
                    return;
                }
                this.commit_draft(cx);
            }
            _ => {}
        });
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.ignore_blur = false;
        self.draft = Some(Draft { kind, input, busy: false, _sub: sub });
        cx.notify();
    }

    fn start_mkdir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken: HashSet<String> = self.listing.entries.iter().flatten().map(|e| e.name.clone()).collect();
        let name = unique_name(&t!("files.newFolderName"), &taken);
        self.scroll.scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
        self.start_draft(DraftKind::Mkdir, name, window, cx);
    }

    fn start_rename(&mut self, entry: &WorkspaceEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.start_draft(DraftKind::Rename { path: entry.path.clone() }, entry.name.clone(), window, cx);
    }

    fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        self.ignore_blur = true;
        self.draft = None;
        cx.notify();
    }

    fn commit_draft(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id.clone() else { return };
        let Some(draft) = &mut self.draft else { return };
        if draft.busy {
            return;
        }
        let name = draft.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.draft = None;
            cx.notify();
            return;
        }
        let client = self.ws.read(cx).client.clone();
        match &draft.kind {
            DraftKind::Mkdir => {
                draft.busy = true;
                let path = if self.cwd.is_empty() { name.clone() } else { format!("{}/{name}", self.cwd) };
                let fut = client.mkdir(&pid, &path, false);
                cx.spawn(async move |this, cx| {
                    let result = fut.await;
                    this.update(cx, |this, cx| {
                        let ws = this.ws.clone();
                        match result {
                            Ok(_) => {
                                ws.update(cx, |w, cx| {
                                    w.toast(ToastKind::Success, t!("files.createdFolder", name = name).to_string(), None, cx)
                                });
                                this.draft = None;
                                this.reload(cx);
                            }
                            Err(err) => {
                                ws.update(cx, |w, cx| {
                                    w.handle_error(&err, cx);
                                    w.toast(ToastKind::Danger, t!("files.mkdirFailed").to_string(), Some(err.to_string()), cx);
                                });
                                if let Some(d) = &mut this.draft {
                                    d.busy = false;
                                }
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            DraftKind::Rename { path } => {
                let path = path.clone();
                if self.entry(&path).is_none_or(|e| e.name == name) {
                    self.draft = None;
                    cx.notify();
                    return;
                }
                if let Some(d) = &mut self.draft {
                    d.busy = true;
                }
                let fut = client.rename_file(&pid, &path, &name);
                cx.spawn(async move |this, cx| {
                    let result = fut.await;
                    this.update(cx, |this, cx| {
                        let ws = this.ws.clone();
                        match result {
                            Ok(res) => {
                                // 改名后让查看窗口跟到新路径上（open_file 是就地换内容，不会多开一扇）
                                let follow = ws.read(cx).file_tab().is_some_and(|f| f.project_id == pid && f.path == path);
                                ws.update(cx, |w, cx| {
                                    if follow {
                                        w.open_file(&pid, &res.path, cx);
                                    }
                                    w.toast(ToastKind::Success, t!("files.renamed", name = name).to_string(), None, cx);
                                });
                                this.draft = None;
                                this.reload(cx);
                            }
                            Err(err) => {
                                ws.update(cx, |w, cx| {
                                    w.handle_error(&err, cx);
                                    w.toast(ToastKind::Danger, t!("files.renameFailed").to_string(), Some(err.to_string()), cx);
                                });
                                if let Some(d) = &mut this.draft {
                                    d.busy = false;
                                }
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// 右键菜单：右键只开菜单、不改选中——点到未勾的项就只对这一项动手，已勾的多项仍对整组
    fn row_menu(&self, entry: &WorkspaceEntry, this: WeakEntity<Self>) -> Vec<MenuItemSpec> {
        let targets: Vec<WorkspaceEntry> = if self.selected.contains(&entry.path) && self.selected.len() > 1 {
            self.visible().iter().filter(|e| self.selected.contains(&e.path)).cloned().collect()
        } else {
            vec![entry.clone()]
        };
        let multi = targets.len() > 1;
        let target_paths: Vec<String> = targets.iter().map(|e| e.path.clone()).collect();
        let files: Vec<String> = targets.iter().filter(|e| e.kind == WorkspaceEntryKind::File).map(|e| e.path.clone()).collect();
        let is_dir = entry.kind == WorkspaceEntryKind::Dir;
        let mut items = Vec::new();

        if !multi {
            let (w, e) = (this.clone(), entry.clone());
            let label = if is_dir { t!("files.openFolder") } else { t!("files.open") };
            items.push(MenuItemSpec::new(label.to_string(), move |window, cx| {
                w.update(cx, |p, cx| p.activate(&e, window, cx)).ok();
            }));
        }
        {
            let (w, paths) = (this.clone(), target_paths.clone());
            items.push(MenuItemSpec::new(t!("files.copyPath").to_string(), move |_, cx| {
                w.update(cx, |p, cx| p.copy_paths(&paths, cx)).ok();
            }));
        }
        if !files.is_empty() {
            let w = this.clone();
            items.push(MenuItemSpec::new(t!("files.download").to_string(), move |window, cx| {
                w.update(cx, |p, cx| p.download_paths(&files, window, cx)).ok();
            }));
        }
        if !multi && is_dir {
            let (w, dir) = (this.clone(), entry.path.clone());
            items.push(MenuItemSpec::new(t!("files.uploadHere").to_string(), move |window, cx| {
                w.update(cx, |p, cx| p.upload_to(dir.clone(), false, window, cx)).ok();
            }));
        }
        if !multi {
            let (w, e) = (this.clone(), entry.clone());
            items.push(MenuItemSpec::new(t!("files.rename").to_string(), move |window, cx| {
                w.update(cx, |p, cx| p.start_rename(&e, window, cx)).ok();
            }));
        }
        {
            let w = this;
            items.push(
                MenuItemSpec::new(t!("files.delete").to_string(), move |window, cx| {
                    w.update(cx, |p, cx| p.remove_paths(target_paths.clone(), window, cx)).ok();
                })
                .danger()
                .sep(),
            );
        }
        items
    }

    // ---------------- 渲染 ----------------

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let ui = Ui::global(cx).clone();
        let has_project = self.project_id.is_some();
        let parent = parent_rel(&self.cwd);
        let selected_files: Vec<String> = self
            .visible()
            .iter()
            .filter(|e| e.kind == WorkspaceEntryKind::File && self.selected.contains(&e.path))
            .map(|e| e.path.clone())
            .collect();
        let selected_all: Vec<String> = self.selected.iter().cloned().collect();

        let mut bar = div().mt_1().flex().items_center().gap(zpx(2.));
        bar = bar.child(tool_button("files-parent", IconName::ArrowLeft, t!("files.parent"), !has_project || parent.is_none(), false, false, false, &ui).on_click(
            cx.listener(move |this, _, window, cx| {
                if let Some(p) = parent.clone() {
                    this.go_to(p, window, cx);
                }
            }),
        ));
        bar = bar.child(
            // 加载中转的是按钮自己的图标（web 的 animate-spin），不换成别的加载图形
            tool_button("files-refresh", IconName::RefreshCw, t!("files.refresh"), !has_project, false, false, self.listing.loading && has_project, &ui)
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
        );
        let hidden_label = if self.show_hidden { t!("files.hideHidden") } else { t!("files.showHidden") };
        bar = bar.child(
            tool_button(
                "files-hidden",
                if self.show_hidden { IconName::Eye } else { IconName::EyeOff },
                hidden_label,
                !has_project,
                self.show_hidden,
                false,
                false,
                &ui,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_hidden = !this.show_hidden;
                this.invalidate_rows();
                cx.notify();
            })),
        );
        bar = bar.child(
            tool_button("files-mkdir", IconName::FolderPlus, t!("files.newFolder"), !has_project, false, false, false, &ui)
                .on_click(cx.listener(|this, _, window, cx| this.start_mkdir(window, cx))),
        );
        bar = bar.child(
            tool_button("files-upload", IconName::Upload, t!("files.upload"), !has_project, false, false, false, &ui)
                .on_click(cx.listener(|this, _, window, cx| this.upload_to(this.cwd.clone(), false, window, cx))),
        );
        bar = bar.child(
            tool_button("files-upload-dir", IconName::FolderUp, t!("files.uploadFolder"), !has_project, false, false, false, &ui)
                .on_click(cx.listener(|this, _, window, cx| this.upload_to(this.cwd.clone(), true, window, cx))),
        );
        bar = bar.child(
            tool_button("files-download", IconName::Download, t!("files.downloadMany"), !has_project || selected_files.is_empty(), false, false, false, &ui)
                .on_click(cx.listener(move |this, _, window, cx| this.download_paths(&selected_files, window, cx))),
        );
        bar = bar.child(div().mx(zpx(2.)).h(zpx(16.)).w(zpx(1.)).bg(ui.border));
        bar.child(
            tool_button("files-delete", IconName::Trash, t!("files.delete"), !has_project || selected_all.is_empty(), false, true, false, &ui)
                .on_click(cx.listener(move |this, _, window, cx| this.remove_paths(selected_all.clone(), window, cx))),
        )
    }

    fn header(&self, show_size: bool, show_mtime: bool, cx: &mut Context<Self>) -> Div {
        let ui = Ui::global(cx).clone();
        let visible = self.visible();
        let all = !visible.is_empty() && visible.iter().all(|e| self.selected.contains(&e.path));
        let some = visible.iter().any(|e| self.selected.contains(&e.path));
        let state = if all { Check::On } else if some { Check::Mixed } else { Check::Off };
        let (key, dir) = self.sort;

        let mut head = div()
            .flex_none()
            .h(zpx(ROW_H))
            .mx_1()
            .flex()
            .items_center()
            .text_size(zpx(11.))
            .text_color(ui.muted_foreground)
            .child(
                div()
                    .id("files-check-all")
                    .w(zpx(CHECK_COL))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .when(!visible.is_empty(), |d| {
                        d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| this.toggle_all(cx)))
                    })
                    .tooltip(|window, cx| Tooltip::new(t!("files.selectAll").to_string()).build(window, cx))
                    .child(check_box(state, visible.is_empty(), &ui)),
            )
            .child(sort_head("files-sort-name", t!("files.name"), key == SortKey::Name, dir, false, &ui).flex_1().on_click(
                cx.listener(|this, _, _, cx| this.toggle_sort(SortKey::Name, cx)),
            ));
        if show_size {
            head = head.child(
                sort_head("files-sort-size", t!("files.size"), key == SortKey::Size, dir, true, &ui)
                    .w(zpx(SIZE_COL))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sort(SortKey::Size, cx))),
            );
        }
        if show_mtime {
            head = head.child(
                sort_head("files-sort-mtime", t!("files.mtime"), key == SortKey::Mtime, dir, true, &ui)
                    .w(zpx(MTIME_COL))
                    .pr_2()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sort(SortKey::Mtime, cx))),
            );
        }
        head
    }

    fn draft_input(&self, draft: &Draft, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel_draft(cx);
                }
            }))
            .child(Input::new(&draft.input).xsmall())
            .into_any_element()
    }

    fn render_rows(&mut self, range: std::ops::Range<usize>, show_size: bool, show_mtime: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ui = Ui::global(cx).clone();
        let visible = self.visible();
        let mkdir_row = matches!(self.draft.as_ref().map(|d| &d.kind), Some(DraftKind::Mkdir));
        let offset = usize::from(mkdir_row);
        let opened = {
            let ws = self.ws.read(cx);
            match &ws.state.active {
                ActiveView::File { project_id, path } if Some(project_id) == self.project_id.as_ref() => Some(path.clone()),
                _ => None,
            }
        };
        let this = cx.weak_entity();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            if mkdir_row && ix == 0 {
                let draft = self.draft.as_ref().expect("mkdir 行只在有草稿时存在");
                out.push(
                    div()
                        .id("files-mkdir-row")
                        .w_full()
                        .h(zpx(ROW_H))
                        .flex()
                        .items_center()
                        .rounded(radius::SM)
                        .bg(ui.accent.opacity(0.4))
                        .text_size(zpx(11.5))
                        .child(div().w(zpx(CHECK_COL)).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(zpx(6.))
                                .pr_2()
                                .child(icon(IconName::Folder).size(zpx(14.)).text_color(ui.muted_foreground))
                                .child(self.draft_input(draft, cx)),
                        )
                        .into_any_element(),
                );
                continue;
            }
            let Some(entry) = visible.get(ix - offset).cloned() else { continue };
            let on = self.selected.contains(&entry.path);
            let is_open = opened.as_deref() == Some(entry.path.as_str());
            let renaming = matches!(self.draft.as_ref().map(|d| &d.kind), Some(DraftKind::Rename { path }) if *path == entry.path);
            let is_dir = entry.kind == WorkspaceEntryKind::Dir;
            let icon_name = if is_dir { IconName::Folder } else { icon_for(&entry.name) };

            let path_click = entry.path.clone();
            let path_check = entry.path.clone();
            let entry_open = entry.clone();
            let tip: SharedString = entry.path.clone().into();

            let name_cell: AnyElement = if renaming {
                self.draft_input(self.draft.as_ref().expect("改名行只在有草稿时存在"), cx)
            } else {
                div()
                    .id(SharedString::from(format!("files-name-{}", entry.path)))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if e.standard_click() {
                            this.activate(&entry_open, window, cx);
                        }
                    }))
                    .child(entry.name.clone())
                    .into_any_element()
            };

            let mut row = div()
                .id(SharedString::from(format!("files-row-{}", entry.path)))
                .w_full()
                .h(zpx(ROW_H))
                .flex()
                .items_center()
                .rounded(radius::SM)
                .text_size(zpx(11.5))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    if !renaming {
                        this.on_row_click(&path_click, e, window, cx);
                    }
                }))
                .child(
                    // 复选框单独吃掉按下：点它只勾选，不触发整行的单选（web 的 data-file-check）
                    div()
                        .id(SharedString::from(format!("files-check-{}", entry.path)))
                        .w(zpx(CHECK_COL))
                        .h_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_one(&path_check, cx);
                        }))
                        .child(check_box(if on { Check::On } else { Check::Off }, false, &ui)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(zpx(6.))
                        .pr_1()
                        .child(icon(icon_name).size(zpx(14.)).flex_none().text_color(ui.muted_foreground))
                        .child(name_cell),
                );
            if show_size {
                row = row.child(
                    div()
                        .w(zpx(SIZE_COL))
                        .flex_none()
                        .pr_1()
                        .truncate()
                        .text_right()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(if is_dir { "—".to_string() } else { format_size(entry.size.map(|s| s as f64)) }),
                );
            }
            if show_mtime {
                row = row.child(
                    div()
                        .w(zpx(MTIME_COL))
                        .flex_none()
                        .pr_2()
                        .truncate()
                        .text_right()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(crate::local_time::format_mtime(entry.mtime)),
                );
            }
            row = if on {
                row.bg(ui.tint)
            } else if is_open {
                row.bg(ui.accent.opacity(0.4)).hover(|s| s.bg(ui.muted))
            } else {
                row.hover(|s| s.bg(ui.muted))
            };
            if is_dir {
                // 拖到目录行上：传进那个目录（落点比面板空白处更具体，先接住）
                let dir = entry.path.clone();
                let tint = ui.tint;
                row = row
                    .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(tint))
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                        this.upload_dropped(dir.clone(), paths, window, cx)
                    }));
            }
            let menu_entry = entry.clone();
            let menu_this = this.clone();
            let row = row.context_menu(move |menu, _, cx| {
                let Some(panel) = menu_this.upgrade() else { return menu };
                let items = panel.read(cx).row_menu(&menu_entry, menu_this.clone());
                to_popup(menu, items)
            });
            out.push(row.into_any_element());
        }
        out
    }

    fn hint(text: String, detail: Option<String>, ui: &Ui) -> Div {
        let mut d = div()
            .px_3()
            .py_4()
            .text_xs()
            .line_height(zpx(20.))
            .text_color(ui.muted_foreground)
            .child(text);
        if let Some(detail) = detail {
            d = d.child(div().mt_1().text_size(zpx(11.)).child(detail));
        }
        d
    }
}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let has_project = self.project_id.is_some();
        // 断点按界面缩放折算（web 的容器查询量的是 CSS px，放大页面同样会先收起列）
        let width = self.width.as_f32() / crate::zoom::zoom();
        let (show_size, show_mtime) = (width >= SHOW_SIZE_AT, width >= SHOW_MTIME_AT);

        // ---- 顶上：路径栏 + 工具栏 ----
        let top = div()
            .flex_none()
            .px_2()
            .py(zpx(6.))
            .border_b_1()
            .border_color(ui.border)
            .child(
                div()
                    .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "escape" {
                            cx.stop_propagation();
                            this.reset_path_input(window, cx);
                            window.focus(&this.focus, cx);
                        }
                    }))
                    .child(Input::new(&self.path_input).small().disabled(!has_project)),
            )
            .child(self.toolbar(cx));

        // ---- 表 ----
        let body: AnyElement = if !has_project {
            Self::hint(t!("files.noProject").to_string(), None, &ui).into_any_element()
        } else if let (Some(err), None) = (&self.listing.error, &self.listing.entries) {
            Self::hint(t!("files.loadFailed").to_string(), Some(err.clone()), &ui).into_any_element()
        } else {
            let visible_len = self.visible().len();
            let mkdir_row = matches!(self.draft.as_ref().map(|d| &d.kind), Some(DraftKind::Mkdir));
            let count = visible_len + usize::from(mkdir_row);
            let mut table = div().flex_1().min_h_0().flex().flex_col().child(self.header(show_size, show_mtime, cx));
            if self.listing.loading && self.listing.entries.is_none() {
                table = table.child(Self::hint(t!("files.loading").to_string(), None, &ui).py_3());
            } else if count == 0 {
                table = table.child(Self::hint(t!("files.empty").to_string(), None, &ui).py_3());
            } else {
                table = table.child(
                    uniform_list(
                        "files-list",
                        count,
                        cx.processor(move |this, range, _window, cx| this.render_rows(range, show_size, show_mtime, cx)),
                    )
                    .flex_1()
                    .px_1()
                    .track_scroll(&self.scroll),
                );
            }
            if self.listing.truncated {
                let n = self.listing.entries.as_ref().map_or(0, Vec::len);
                table = table.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(t!("files.truncated", n = n).to_string()),
                );
            }
            table.into_any_element()
        };

        let view = cx.entity();
        let tint = ui.tint.opacity(0.35);
        div()
            .id("files-panel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .text_color(ui.foreground)
            .child(top)
            .child(body)
            // 面板宽度决定露出几列（web 用容器查询）；跨过断点才重画
            .child(
                canvas(
                    move |bounds, _, cx| {
                        view.update(cx, |this, cx| {
                            let z = crate::zoom::zoom();
                            let old = this.width.as_f32() / z;
                            let new = bounds.size.width.as_f32() / z;
                            this.width = bounds.size.width;
                            let crossed = |at: f32| (old >= at) != (new >= at);
                            if crossed(SHOW_SIZE_AT) || crossed(SHOW_MTIME_AT) {
                                cx.notify();
                            }
                        })
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when(has_project, |d| {
                d.drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(tint))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        let dir = this.cwd.clone();
                        this.upload_dropped(dir, paths, window, cx)
                    }))
            })
    }
}

// ---------------- 小件 ----------------

/// 工具栏按钮（web 的 IconBtn：ghost、icon-xs）：不可用时变淡且不响应；按下态用 muted 底
fn tool_button(
    id: &'static str,
    name: IconName,
    label: impl Into<SharedString>,
    disabled: bool,
    pressed: bool,
    danger: bool,
    spinning: bool,
    ui: &Ui,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let glyph: AnyElement = if spinning {
        Spinner::new().icon(name).with_size(zpx(14.)).into_any_element()
    } else {
        icon(name).size(zpx(14.)).into_any_element()
    };
    let base = div()
        .id(id)
        .size(zpx(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(6.))
        .text_color(if pressed { ui.foreground } else { ui.muted_foreground })
        .when(pressed, |d| d.bg(ui.muted))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(glyph);
    if disabled {
        return base.opacity(0.5);
    }
    let hover_fg = if danger { ui.destructive } else { ui.foreground };
    base.cursor_pointer().hover(move |s| s.bg(ui.muted).text_color(hover_fg))
}

/// 已排序的那一列带个箭头
fn sort_head(id: &'static str, label: impl Into<SharedString>, active: bool, dir: SortDir, right: bool, ui: &Ui) -> Stateful<Div> {
    let fg = ui.foreground;
    let mut head = div()
        .id(id)
        .h(zpx(ROW_H))
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap(zpx(2.))
        .px_1()
        .cursor_pointer()
        .hover(move |s| s.text_color(fg))
        .when(right, |d| d.justify_end())
        .child(div().truncate().child(label.into()));
    if active {
        head = head.child(
            icon(if dir == SortDir::Asc { IconName::ChevronUp } else { IconName::ChevronDown })
                .size(zpx(12.))
                .flex_none(),
        );
    }
    head
}

/// 14px 的小复选框（web 的 Checkbox size-3.5）；表头"全选"要有半选态
fn check_box(state: Check, disabled: bool, ui: &Ui) -> Div {
    let on = state != Check::Off;
    let mut b = div()
        .size(zpx(14.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(4.))
        .border_1()
        .border_color(if on { ui.primary } else { ui.input })
        .when(on, |d| d.bg(ui.primary))
        .when(disabled, |d| d.opacity(0.5));
    match state {
        Check::On => b = b.child(icon(IconName::Check).size(zpx(10.)).text_color(ui.primary_foreground)),
        Check::Mixed => b = b.child(icon(IconName::Minus).size(zpx(10.)).text_color(ui.primary_foreground)),
        Check::Off => {}
    }
    b
}

fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !taken.contains(n)).unwrap_or_else(|| base.to_string())
}

/// 删掉的文件如果正开着（或它在被删的目录里），把那扇查看窗口收起来
fn close_matching_file(ws: &Entity<Workspace>, project_id: &str, removed: &[String], cx: &mut App) {
    let hit = ws.read(cx).file_tab().is_some_and(|f| {
        f.project_id == project_id && removed.iter().any(|p| f.path == *p || f.path.starts_with(&format!("{p}/")))
    });
    if hit {
        ws.update(cx, |w, cx| w.close_file(cx));
    }
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif", "svg"];
const TEXT_EXT: &[&str] = &["md", "markdown", "txt", "log", "rst", "adoc"];
const CODE_EXT: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "jsonc", "css", "scss", "html", "vue", "py", "go", "rs", "java", "kt",
    "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "fish", "ps1", "sql", "yml", "yaml",
    "toml", "ini", "conf", "xml",
];

fn icon_for(name: &str) -> IconName {
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => String::new(),
    };
    if IMAGE_EXT.contains(&ext.as_str()) {
        IconName::FileImage
    } else if TEXT_EXT.contains(&ext.as_str()) {
        IconName::FileText
    } else if CODE_EXT.contains(&ext.as_str()) {
        IconName::FileCode
    } else {
        IconName::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_name_appends_number() {
        let taken: HashSet<String> = ["新建文件夹".to_string(), "新建文件夹 2".to_string()].into();
        assert_eq!(unique_name("新建文件夹", &taken), "新建文件夹 3");
        assert_eq!(unique_name("x", &taken), "x");
    }

    #[test]
    fn icon_by_extension() {
        assert_eq!(icon_for("a.PNG"), IconName::FileImage);
        assert_eq!(icon_for("README.md"), IconName::FileText);
        assert_eq!(icon_for("main.rs"), IconName::FileCode);
        assert_eq!(icon_for(".gitignore"), IconName::File);
        assert_eq!(icon_for("Makefile"), IconName::File);
    }
}
