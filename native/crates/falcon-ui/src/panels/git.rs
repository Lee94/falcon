//! 右侧「历史」面板（旧 React 版的 `components/GitPanel.tsx`）：分支 / 同步状态头、提交图泳道、
//! 按分支 / 作者 / 关键字筛选、分页加载、提交详情与文件列表、17 种 git op 的菜单与确认框。
//!
//! 三份数据各有各的节奏，刻意不合并成一个轮询（沿用 React 版）：
//! - 头部的 ahead/behind 走 5s 轮询（Pull / Push 上的数字要跟得上远端），只在面板显示着时；
//! - 列表只在筛选变化 / 手动刷新 / 写操作之后重取——正读着的历史在眼皮底下重排会让人跟丢
//!   位置，而历史本来就不怎么变；
//! - 详情随选中项走。
//!
//! 提交列表是 `uniform_list`：React 版为了"方向键连按不掉帧"给每行加 memo，这里只画视口里那
//! 几十行，没有这层讲究。
//!
//! 右键菜单：整个面板只挂一个 ContextMenu，行 / 分支徽标 / 分支树 / Push 按钮在右键按下时
//! 记下"点的是谁"，菜单在下一帧按它构建。不给每行各挂一个——徽标嵌在提交行里，两个
//! ContextMenu 会同时弹出（GPUI 冒泡时外层的先收到事件，内层拦不住）。

pub mod common;
mod dialogs;
mod graph;

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use falcon_client::{GitLogQuery, GitSyncAction};
use falcon_core::git_graph::{GraphRow, layout_commit_graph};
use falcon_proto::{
    GitCommitDetail, GitFileChange, GitLogCommit, GitOpInput, GitRefKind, GitRefLabel, GitRefsInfo, GitSnapshot,
    GitSyncResult,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, Entity, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent,
    Render, SharedString, Task, UniformListScrollHandle, Window, div, relative, uniform_list,
};
use rust_i18n::t;

use self::common::{
    FILE_ROW_H, FileItem, FileListHost, FileRow, build_rows, file_view_mode, file_view_toggle, format_when, hint,
    reason_key, render_file_row, tool_button,
};
use self::graph::{ROW_H, graph_cell};
use crate::dialogs::{self as app_dialogs, ConfirmOpts};
use crate::menus::{MenuItemSpec, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{DiffCommit, RightPanelId, ToastKind, Workspace};
use crate::zoom::zpx;

/// 头部 Pull / Push 计数的轮询间隔。列表本身不轮询
const POLL: Duration = Duration::from_millis(5000);
/// 搜索框每敲一个字都发一次 git log 太重：停手这么久才查（React 版用 useDeferredValue 降频）
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// 分支树一行的高度与最大高度（React 版的 h-6 / max-h-36）
const TREE_ROW_H: f32 = 24.;
const TREE_MAX_H: f32 = 144.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Syncing {
    Pull,
    Push,
    Fetch,
    Op,
}

/// 右键点的是谁（见文件头）
#[derive(Clone, Debug)]
enum MenuTarget {
    Commit(String),
    Ref(GitRefLabel),
    /// Push 按钮：右键强制推送
    Push,
}

/// 分支树压平后的一行
#[derive(Clone, Debug)]
enum TreeRow {
    All,
    Group(&'static str),
    Ref { label: GitRefLabel, dot: bool },
}

/// 下方的提交详情
struct Detail {
    sha: String,
    data: Option<GitCommitDetail>,
    error: Option<String>,
    expanded: bool,
    copied: bool,
    files: Vec<FileItem>,
    collapsed: HashSet<String>,
    scroll: UniformListScrollHandle,
    _copy_reset: Task<()>,
}

pub struct GitPanel {
    ws: Entity<Workspace>,
    /// (项目, 多仓库成员)：换了就清空重来——留着上一个仓库的分支名只会得到空列表
    target: Option<(String, Option<String>)>,
    snap: Option<GitSnapshot>,
    refs: Option<GitRefsInfo>,
    commits: Vec<GitLogCommit>,
    graph: Vec<GraphRow>,
    has_more: bool,
    log_error: Option<String>,
    loading: bool,
    loading_more: bool,

    query: Entity<InputState>,
    /// 真正拿去查的关键字（防抖之后）
    applied_query: String,
    branch: Option<String>,
    author: Option<String>,
    filters_open: bool,
    tree_open: bool,
    selected: Option<String>,
    syncing: Option<Syncing>,
    detail: Option<Detail>,

    author_search: Entity<InputState>,
    menu_target: Option<MenuTarget>,

    list_scroll: UniformListScrollHandle,
    tree_scroll: UniformListScrollHandle,
    /// 目标每换一次加一：晚到的旧结果丢掉
    seq: u64,
    /// 列表每重取一次加一（筛选变了的时候，上一次的结果不能再落地）
    log_seq: u64,
    snap_in_flight: Option<u64>,
    visible: bool,
    /// 换项目时要清空搜索框，但 set_value 要 Window——留到下一次渲染做
    pending_clear_query: bool,
    _debounce: Task<()>,
    _poll: Task<()>,
}

impl GitPanel {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("git.searchCommits").to_string()));
        cx.subscribe(&query, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                let value = input.read(cx).value().trim().to_string();
                this._debounce = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                    this.update(cx, |this, cx| {
                        if this.applied_query != value {
                            this.applied_query = value;
                            this.load_log(cx);
                        }
                    })
                    .ok();
                });
                cx.notify();
            }
        })
        .detach();
        let author_search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("git.searchUser").to_string()));
        cx.subscribe(&author_search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&ws, |this, _, cx| {
            this.sync(cx);
            cx.notify();
        })
        .detach();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this.update(cx, |this, cx| {
                    if this.visible {
                        this.load_snapshot(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            ws,
            target: None,
            snap: None,
            refs: None,
            commits: Vec::new(),
            graph: Vec::new(),
            has_more: false,
            log_error: None,
            loading: true,
            loading_more: false,
            query,
            applied_query: String::new(),
            branch: None,
            author: None,
            filters_open: true,
            tree_open: true,
            selected: None,
            syncing: None,
            detail: None,
            author_search,
            menu_target: None,
            list_scroll: UniformListScrollHandle::new(),
            tree_scroll: UniformListScrollHandle::new(),
            seq: 0,
            log_seq: 0,
            snap_in_flight: None,
            visible: false,
            pending_clear_query: false,
            _debounce: Task::ready(()),
            _poll: poll,
        };
        this.sync(cx);
        this
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.syncing.is_some()
    }

    fn pid(&self) -> Option<String> {
        self.target.as_ref().map(|t| t.0.clone())
    }

    fn repo(&self) -> Option<String> {
        self.target.as_ref().and_then(|t| t.1.clone())
    }

    /// 跟着工作区走：换项目 / 换成员就清空重来；面板重新露出来时整份重取（React 版是重新挂载）
    fn sync(&mut self, cx: &mut Context<Self>) {
        let ws = self.ws.read(cx);
        let visible = ws.ready() && ws.state.right_open && ws.state.right_panel == RightPanelId::Git;
        let target = ws.focus_project_id().map(|pid| {
            let repo = falcon_core::workspace::select_multi_repo_dir(&ws.state.multi_repo, ws.project(&pid));
            (pid, repo)
        });
        let changed = target != self.target;
        if changed {
            self.target = target;
            self.seq += 1;
            self.snap = None;
            self.refs = None;
            self.commits.clear();
            self.graph.clear();
            self.has_more = false;
            self.selected = None;
            self.detail = None;
            self.log_error = None;
            self.applied_query.clear();
            self.pending_clear_query = true;
            self.branch = None;
            self.author = None;
            self._debounce = Task::ready(());
        }
        let appeared = visible && !self.visible;
        self.visible = visible;
        if visible && (changed || appeared) {
            self.refresh_all(cx);
        }
    }

    /// React 版的 tick：头部、筛选候选、列表一起重取
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.snap_in_flight = None;
        self.load_snapshot(cx);
        self.load_refs(cx);
        self.load_log(cx);
    }

    fn load_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        if self.snap_in_flight == Some(self.seq) {
            return;
        }
        let seq = self.seq;
        self.snap_in_flight = Some(seq);
        let (client, repo) = (self.ws.read(cx).client.clone(), self.repo());
        cx.spawn(async move |this, cx| {
            let result = client.git_snapshot(&pid, repo.as_deref()).await;
            this.update(cx, |this, cx| {
                if this.snap_in_flight == Some(seq) {
                    this.snap_in_flight = None;
                }
                if this.seq != seq {
                    return;
                }
                match result {
                    Ok(s) => this.snap = Some(s),
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 筛选下拉 / 分支树的候选值。跟着刷新重取，新建的分支才会出现
    fn load_refs(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        let seq = self.seq;
        let (client, repo) = (self.ws.read(cx).client.clone(), self.repo());
        cx.spawn(async move |this, cx| {
            let result = client.git_refs(&pid, repo.as_deref()).await;
            this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                match result {
                    Ok(r) => this.refs = Some(r),
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn log_query(&self, skip: u32) -> GitLogQuery {
        GitLogQuery {
            branch: self.branch.clone(),
            author: self.author.clone(),
            q: (!self.applied_query.is_empty()).then(|| self.applied_query.clone()),
            skip,
            repo: self.repo(),
        }
    }

    /// 列表：筛选变化或手动刷新时重取第一页
    fn load_log(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        self.log_seq += 1;
        let (seq, log_seq) = (self.seq, self.log_seq);
        self.loading = true;
        self.loading_more = false;
        let query = self.log_query(0);
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.git_log(&pid, &query).await;
            this.update(cx, |this, cx| {
                if this.seq != seq || this.log_seq != log_seq {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(page) => {
                        this.log_error = if page.available {
                            None
                        } else {
                            Some(
                                page.detail
                                    .clone()
                                    .unwrap_or_else(|| t!(reason_key(page.reason)).to_string()),
                            )
                        };
                        this.has_more = page.has_more;
                        this.graph = layout_commit_graph(&page.commits);
                        this.commits = page.commits;
                        // 选中项在新结果里还在就留着，否则收起详情——保持一个已经不在列表里
                        // 的选中项，详情区会跟列表说着两件事
                        if let Some(sel) = &this.selected
                            && !this.commits.iter().any(|c| &c.sha == sel)
                        {
                            this.selected = None;
                            this.detail = None;
                        }
                    }
                    Err(err) => {
                        this.log_error = Some(err.to_string());
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

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        if self.loading_more || self.loading {
            return;
        }
        self.loading_more = true;
        let (seq, log_seq) = (self.seq, self.log_seq);
        let query = self.log_query(self.commits.len() as u32);
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.git_log(&pid, &query).await;
            this.update(cx, |this, cx| {
                if this.seq != seq || this.log_seq != log_seq {
                    return;
                }
                this.loading_more = false;
                match result {
                    Ok(page) => {
                        // 去重后再接：翻页这一趟里若有新提交进来，skip 会让同一条出现两次
                        let seen: HashSet<String> = this.commits.iter().map(|c| c.sha.clone()).collect();
                        this.commits
                            .extend(page.commits.into_iter().filter(|c| !seen.contains(&c.sha)));
                        this.graph = layout_commit_graph(&this.commits);
                        this.has_more = page.has_more;
                    }
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn set_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        if self.branch != branch {
            self.branch = branch;
            self.load_log(cx);
        }
    }

    fn set_author(&mut self, author: Option<String>, cx: &mut Context<Self>) {
        if self.author != author {
            self.author = author;
            self.load_log(cx);
        }
    }

    // ---------------- 选中与详情 ----------------

    fn select(&mut self, sha: Option<String>, cx: &mut Context<Self>) {
        if self.selected == sha {
            return;
        }
        self.selected = sha.clone();
        self.detail = None;
        if let (Some(sha), Some(pid)) = (sha, self.pid()) {
            self.detail = Some(Detail {
                sha: sha.clone(),
                data: None,
                error: None,
                expanded: false,
                copied: false,
                files: Vec::new(),
                collapsed: HashSet::new(),
                scroll: UniformListScrollHandle::new(),
                _copy_reset: Task::ready(()),
            });
            let (client, repo) = (self.ws.read(cx).client.clone(), self.repo());
            cx.spawn(async move |this, cx| {
                let result = client.git_commit(&pid, &sha, repo.as_deref()).await;
                this.update(cx, |this, cx| {
                    let Some(d) = this.detail.as_mut().filter(|d| d.sha == sha) else {
                        return;
                    };
                    match result {
                        Ok(next) if next.available => {
                            d.files = next
                                .files
                                .iter()
                                .map(|f| FileItem {
                                    path: f.path.clone(),
                                    orig_path: f.orig_path.clone(),
                                    status: f.status.clone(),
                                    added: f.added,
                                    deleted: f.deleted,
                                })
                                .collect();
                            d.data = Some(next);
                        }
                        Ok(next) => {
                            d.error = Some(
                                next.detail
                                    .clone()
                                    .unwrap_or_else(|| t!(reason_key(next.reason)).to_string()),
                            )
                        }
                        Err(err) => {
                            d.error = Some(err.to_string());
                            this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn copy_detail_sha(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_mut() else { return };
        let sha = d.data.as_ref().map(|x| x.sha.clone()).unwrap_or_else(|| d.sha.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(sha));
        d.copied = true;
        d._copy_reset = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| {
                if let Some(d) = this.detail.as_mut() {
                    d.copied = false;
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    // ---------------- 写操作 ----------------

    fn toast(&self, ok: bool, title: String, detail: String, cx: &mut Context<Self>) {
        // 成功也要出提示：一次 --ff-only 的 pull 常常什么都不发生（Already up to date），没有
        // 回声的话按钮像是没反应。失败的 body 是 git 的原话——"为什么被拒"只有它说得清，
        // 而且往往直接把该敲的命令印在里面（比如没有 upstream 时的 --set-upstream）
        let kind = if ok { ToastKind::Success } else { ToastKind::Danger };
        let body = (!detail.trim().is_empty()).then_some(detail);
        // 失败的原话要读完（往往还得照着敲命令），不自动消失（React 版的 sticky: !res.ok）
        self.ws.update(cx, |w, cx| if ok { w.toast(kind, title, body, cx) } else { w.toast_sticky(kind, title, body, cx) });
    }

    /// 发一次写请求：占住 syncing、toast、成功后整份刷新。返回的 Task 给出成功与否
    fn run_request(
        &mut self,
        kind: Syncing,
        title: String,
        req: impl std::future::Future<Output = falcon_client::ApiResult<GitSyncResult>> + 'static,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.syncing.is_some() {
            return Task::ready(false);
        }
        self.syncing = Some(kind);
        cx.notify();
        let seq = self.seq;
        cx.spawn(async move |this, cx| {
            let result = req.await;
            this.update(cx, |this, cx| {
                this.syncing = None;
                let ok = match result {
                    Ok(res) => {
                        this.toast(res.ok, title, res.detail, cx);
                        res.ok
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| {
                            w.handle_error(&err, cx);
                            w.toast(ToastKind::Danger, title, Some(err.to_string()), cx);
                        });
                        false
                    }
                };
                if ok && this.seq == seq {
                    this.refresh_all(cx);
                    // 侧栏的分支名 / +N −M 也跟着变了
                    this.ws.update(cx, |w, cx| {
                        w.refresh_changes(cx);
                        w.refresh_projects(cx);
                    });
                }
                cx.notify();
                ok
            })
            .unwrap_or(false)
        })
    }

    pub(crate) fn run_op(&mut self, op: GitOpInput, title: String, cx: &mut Context<Self>) -> Task<bool> {
        let Some(pid) = self.pid() else {
            return Task::ready(false);
        };
        let (client, repo) = (self.ws.read(cx).client.clone(), self.repo());
        self.run_request(
            Syncing::Op,
            title,
            async move { client.git_op(&pid, &op, repo.as_deref()).await },
            cx,
        )
    }

    fn sync_remote(&mut self, kind: Syncing, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        let (client, repo) = (self.ws.read(cx).client.clone(), self.repo());
        let (title, fut): (String, std::pin::Pin<Box<dyn std::future::Future<Output = _>>>) = match kind {
            Syncing::Pull => (
                t!("git.pull").to_string(),
                Box::pin(async move { client.git_sync(&pid, GitSyncAction::Pull, repo.as_deref()).await }),
            ),
            Syncing::Push => (
                t!("git.push").to_string(),
                Box::pin(async move { client.git_sync(&pid, GitSyncAction::Push, repo.as_deref()).await }),
            ),
            _ => (
                t!("git.fetch").to_string(),
                Box::pin(async move { client.git_op(&pid, &GitOpInput::Fetch, repo.as_deref()).await }),
            ),
        };
        self.run_request(kind, title, fut, cx).detach();
    }

    /// 先确认再动手（React 版的 confirmOp）
    fn confirm_op(
        this: &Entity<Self>,
        title: String,
        body: String,
        confirm_label: String,
        op: GitOpInput,
        window: &mut Window,
        cx: &mut App,
    ) {
        let entity = this.clone();
        let op_title = confirm_label.clone();
        app_dialogs::confirm(
            ConfirmOpts {
                title,
                body,
                confirm_label,
                ..Default::default()
            },
            move |_, cx| {
                let (op, title) = (op.clone(), op_title.clone());
                entity.update(cx, |p, cx| p.run_op(op, title, cx)).detach();
            },
            window,
            cx,
        );
    }

    // ---------------- 菜单 ----------------

    fn is_head_commit(&self, commit: &GitLogCommit) -> bool {
        match self.snap.as_ref().and_then(|s| s.head_sha.as_deref()) {
            Some(head) if !head.is_empty() => commit.short == head || commit.sha.starts_with(head),
            _ => false,
        }
    }

    fn commit_menu(&self, commit: &GitLogCommit, this: &Entity<Self>) -> Vec<MenuItemSpec> {
        let (sha, short, subject) = (commit.sha.clone(), commit.short.clone(), commit.subject.clone());
        // 一个确认型菜单项：标题 / 正文 / 按钮文案 + 要发的 op
        let confirm_item = |label: String, title: String, body: String, op: GitOpInput| {
            let e = this.clone();
            let l = label.clone();
            MenuItemSpec::new(label, move |window, cx| {
                Self::confirm_op(&e, title.clone(), body.clone(), l.clone(), op.clone(), window, cx)
            })
        };
        let with_subject = |key: &str| format!("{subject}\n\n{}", t!(key));
        let mut items = vec![
            {
                let sha = sha.clone();
                MenuItemSpec::new(t!("git.copySha").to_string(), move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(sha.clone()))
                })
            },
            confirm_item(
                t!("git.checkoutRev").to_string(),
                t!("git.checkoutRevTitle", short = short).to_string(),
                t!("git.checkoutRevBody").to_string(),
                GitOpInput::Checkout {
                    rev: sha.clone(),
                    detach: Some(true),
                },
            ),
            {
                let (e, sha, short) = (this.clone(), sha.clone(), short.clone());
                MenuItemSpec::new(t!("git.newBranch").to_string(), move |window, cx| {
                    dialogs::new_branch(e.clone(), sha.clone(), short.clone(), window, cx)
                })
            },
            confirm_item(
                t!("git.cherryPick").to_string(),
                t!("git.cherryPickTitle", short = short).to_string(),
                with_subject("git.cherryPickBody"),
                GitOpInput::CherryPick { sha: sha.clone() },
            ),
            confirm_item(
                t!("git.merge").to_string(),
                t!("git.mergeTitle", name = short).to_string(),
                with_subject("git.mergeBody"),
                GitOpInput::Merge { rev: sha.clone() },
            ),
            confirm_item(
                t!("git.rebase").to_string(),
                t!("git.rebaseTitle", name = short).to_string(),
                with_subject("git.rebaseBody"),
                GitOpInput::Rebase { rev: sha.clone() },
            ),
            confirm_item(
                t!("git.revert").to_string(),
                t!("git.revertTitle", short = short).to_string(),
                with_subject("git.revertBody"),
                GitOpInput::Revert { sha: sha.clone() },
            )
            .sep(),
            {
                let (e, sha, short) = (this.clone(), sha.clone(), short.clone());
                MenuItemSpec::new(t!("git.reset").to_string(), move |window, cx| {
                    dialogs::reset(e.clone(), sha.clone(), short.clone(), window, cx)
                })
            },
            confirm_item(
                t!("git.drop").to_string(),
                t!("git.dropTitle", short = short).to_string(),
                t!("git.dropBody").to_string(),
                GitOpInput::Drop { sha: sha.clone() },
            )
            .sep(),
        ];
        // 压进上一条 / 改说明只对最新一次提交有效
        if self.is_head_commit(commit) {
            items.push(confirm_item(
                t!("git.squash").to_string(),
                t!("git.squashTitle", short = short).to_string(),
                t!("git.squashBody").to_string(),
                GitOpInput::Squash { sha: sha.clone() },
            ));
            let (e, sha, short, subject) = (this.clone(), sha.clone(), short.clone(), subject.clone());
            items.push(MenuItemSpec::new(t!("git.reword").to_string(), move |window, cx| {
                dialogs::reword(e.clone(), sha.clone(), short.clone(), subject.clone(), window, cx)
            }));
        }
        items
    }

    fn force_push_item(this: &Entity<Self>) -> MenuItemSpec {
        let e = this.clone();
        MenuItemSpec::new(t!("git.forcePush").to_string(), move |window, cx| {
            Self::confirm_op(
                &e,
                t!("git.forcePushTitle").to_string(),
                t!("git.forcePushBody").to_string(),
                t!("git.forcePush").to_string(),
                GitOpInput::Push {
                    force_with_lease: Some(true),
                },
                window,
                cx,
            )
        })
    }

    fn ref_menu(label: &GitRefLabel, this: &Entity<Self>) -> Vec<MenuItemSpec> {
        let name = label.name.clone();
        let confirm_item = |item: String, title: String, body: String, confirm: String, op: GitOpInput| {
            let e = this.clone();
            MenuItemSpec::new(item, move |window, cx| {
                Self::confirm_op(&e, title.clone(), body.clone(), confirm.clone(), op.clone(), window, cx)
            })
        };
        let merge = confirm_item(
            t!("git.merge").to_string(),
            t!("git.mergeTitle", name = name).to_string(),
            t!("git.mergeBody").to_string(),
            t!("git.merge").to_string(),
            GitOpInput::Merge { rev: name.clone() },
        );
        if label.kind == GitRefKind::Tag {
            return vec![
                confirm_item(
                    t!("git.checkoutTag").to_string(),
                    t!("git.checkoutRevTitle", short = name).to_string(),
                    t!("git.checkoutRevBody").to_string(),
                    t!("git.checkoutTag").to_string(),
                    GitOpInput::Checkout {
                        rev: name.clone(),
                        detach: Some(true),
                    },
                ),
                merge,
            ];
        }
        let mut items = vec![
            confirm_item(
                t!("git.checkoutBranch").to_string(),
                t!("git.checkoutBranchTitle", name = name).to_string(),
                t!("git.checkoutBranchBody").to_string(),
                t!("git.checkoutBranch").to_string(),
                GitOpInput::CheckoutBranch {
                    branch: name.clone(),
                    create_tracking: Some(label.kind == GitRefKind::Remote),
                },
            ),
            merge,
            confirm_item(
                t!("git.rebase").to_string(),
                t!("git.rebaseTitle", name = name).to_string(),
                t!("git.rebaseBody").to_string(),
                t!("git.rebase").to_string(),
                GitOpInput::Rebase { rev: name.clone() },
            ),
        ];
        if label.head == Some(true) {
            items.push(Self::force_push_item(this).sep());
        }
        items
    }

    /// 右键菜单在下一帧构建时来问：点的是谁
    fn menu_items(&self, this: &Entity<Self>) -> Vec<MenuItemSpec> {
        match &self.menu_target {
            Some(MenuTarget::Commit(sha)) => match self.commits.iter().find(|c| &c.sha == sha) {
                Some(c) => self.commit_menu(c, this),
                None => Vec::new(),
            },
            Some(MenuTarget::Ref(label)) => Self::ref_menu(label, this),
            Some(MenuTarget::Push) => vec![Self::force_push_item(this)],
            None => Vec::new(),
        }
    }
}

impl FileListHost for GitPanel {
    fn open_file(&mut self, idx: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.pid() else { return };
        let Some(d) = &self.detail else { return };
        let (Some(data), Some(f)) = (&d.data, d.files.get(idx)) else {
            return;
        };
        let subject = data.message.split('\n').next().unwrap_or("").to_string();
        // 详情里的状态是 raw diff 的单字母，塞进 index 那一列——差异窗口的状态标签认的正是这个位置
        let file = GitFileChange {
            path: f.path.clone(),
            orig_path: f.orig_path.clone(),
            index: f.status.clone(),
            work: " ".into(),
        };
        let commit = DiffCommit {
            sha: data.sha.clone(),
            short: data.short.clone(),
            subject,
        };
        let repo = self.repo();
        self.ws
            .update(cx, |w, cx| w.open_diff(&pid, file, Some(commit), repo, cx));
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(d) = self.detail.as_mut()
            && !d.collapsed.remove(path)
        {
            d.collapsed.insert(path.to_string());
        }
        cx.notify();
    }
}

// ---------------- 渲染 ----------------

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.pending_clear_query) {
            self.query.update(cx, |q, cx| q.set_value("", window, cx));
        }
        let ui = Ui::global(cx).clone();
        let this = cx.entity();
        let project = self.pid().and_then(|pid| self.ws.read(cx).project(&pid).cloned());

        let root = div()
            .id("git-panel")
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            // 每次右键先清掉上一次的目标：点在空白处就不该弹出上一次的菜单
            .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, _| {
                if e.button == MouseButton::Right {
                    this.menu_target = None;
                }
            }))
            .child(self.header(project.as_ref(), cx));

        let menu_owner = this.clone();
        let root = root.context_menu(move |menu, _, cx| {
            let items = menu_owner.read(cx).menu_items(&menu_owner);
            to_popup(menu, items)
        });

        if self.target.is_none() {
            return root
                .child(hint(t!("git.noProject").to_string(), None, cx))
                .into_any_element();
        }
        if let Some(snap) = self.snap.as_ref().filter(|s| !s.available) {
            return root
                .child(hint(t!(reason_key(snap.reason)).to_string(), snap.detail.clone(), cx))
                .into_any_element();
        }

        let filtered = self.branch.is_some() || self.author.is_some() || !self.applied_query.is_empty();
        let search = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(zpx(6.))
            .px_3()
            .pt(zpx(10.))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.query)
                        .small()
                        .cleanable(true)
                        .prefix(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground)),
                ),
            )
            .child(
                tool_button(
                    "git-filters",
                    IconName::Funnel,
                    t!("git.filters").to_string(),
                    false,
                    cx,
                )
                .when(filtered, |b| b.text_color(ui.foreground))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.filters_open = !this.filters_open;
                    cx.notify();
                })),
            );
        let mut root = root.child(search);
        if self.filters_open {
            root = root.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pt_2()
                    .child(self.author_filter(cx)),
            );
        }
        root = root.child(self.branch_tree(cx));
        root = root.child(self.commit_list(filtered, cx));
        if self.detail.is_some() {
            root = root.child(self.detail_view(cx));
        }
        root.into_any_element()
    }
}

impl GitPanel {
    fn header(&self, project: Option<&falcon_proto::Project>, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let unavailable = self.snap.as_ref().is_some_and(|s| !s.available);
        let disabled = self.target.is_none() || unavailable || self.syncing.is_some();
        let behind = self.snap.as_ref().and_then(|s| s.behind_count()).unwrap_or(0);
        let ahead = self.snap.as_ref().and_then(|s| s.ahead_count()).unwrap_or(0);
        let mut h = div()
            .h(zpx(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(zpx(6.))
            .pl_3()
            .pr_2()
            .border_b_1()
            .border_color(ui.border)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(zpx(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t!("git.history").to_string()),
            );
        if let Some(sel) = common::multi_repo_select("git", &self.ws, project, cx) {
            h = h.child(sel);
        }
        h = h
            .child(self.sync_button(Syncing::Pull, behind, disabled, cx))
            .child(
                div()
                    .id("git-push-wrap")
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, _, cx| {
                            this.menu_target = Some(MenuTarget::Push);
                            cx.stop_propagation();
                        }),
                    )
                    .child(self.sync_button(Syncing::Push, ahead, disabled, cx)),
            )
            .child(
                tool_button(
                    "git-fetch",
                    IconName::CloudDownload,
                    t!("git.fetch").to_string(),
                    false,
                    cx,
                )
                .loading(self.syncing == Some(Syncing::Fetch))
                .loading_icon(IconName::RefreshCw)
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.sync_remote(Syncing::Fetch, cx))),
            )
            .child(
                tool_button(
                    "git-refresh",
                    IconName::RefreshCw,
                    t!("git.refresh").to_string(),
                    false,
                    cx,
                )
                .loading(self.loading && self.target.is_some())
                .loading_icon(IconName::RefreshCw)
                .disabled(self.target.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
            );
        h.into_any_element()
    }

    /// Pull / Push 按钮。计数为 0 时**不禁用**：behind 是 5 秒前的事实，用户点它多半正是因为
    /// "我知道远端有东西了"。有待同步的那个用实底（secondary）拉开对比，0 的那个退成描边
    fn sync_button(&self, kind: Syncing, count: u32, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let (id, label, arrow, tip) = match kind {
            Syncing::Pull => (
                "git-pull",
                t!("git.pull").to_string(),
                IconName::ArrowDown,
                t!("git.pull").to_string(),
            ),
            _ => (
                "git-push",
                t!("git.push").to_string(),
                IconName::ArrowUp,
                t!("git.pushHint").to_string(),
            ),
        };
        let busy = self.syncing == Some(kind);
        let b = Button::new(id)
            .xsmall()
            .h(zpx(28.))
            .px_2()
            .tooltip(tip)
            .disabled(disabled);
        let b = if count > 0 { b.secondary() } else { b.outline() };
        let arrow_el: AnyElement = if busy {
            gpui_kit::component::spinner::Spinner::new()
                .icon(IconName::RefreshCw)
                .into_any_element()
        } else {
            icon(arrow).size(zpx(12.)).into_any_element()
        };
        b.child(div().text_size(zpx(11.)).font_weight(FontWeight::MEDIUM).child(label))
            .child(arrow_el)
            .child(div().text_size(zpx(11.)).child(count.to_string()))
            .on_click(cx.listener(move |this, _, _, cx| this.sync_remote(kind, cx)))
            .into_any_element()
    }

    // ---------------- 作者筛选 ----------------

    /// 作者下拉（React 版 `FilterMenu`）。用 Popover 而不是菜单：顶上要放一个搜索框。搜索在本地做：
    /// 候选就是作者名，几十上百条，本地过滤比一次往返快得多
    fn author_filter(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let label = self.author.clone().unwrap_or_else(|| t!("git.user").to_string());
        let this = cx.entity();
        let trigger = Button::new("git-author-trigger")
            .ghost()
            .small()
            .h(zpx(28.))
            .px(zpx(6.))
            .text_xs()
            .text_color(if self.author.is_some() {
                ui.foreground
            } else {
                ui.muted_foreground
            })
            .child(div().min_w_0().truncate().child(label))
            .child(
                icon(IconName::ChevronDown)
                    .size(zpx(12.))
                    .text_color(ui.muted_foreground),
            );
        // 开关交给 Popover 自己管（不受控），选中一项后用它的 state 收起——少一份要与弹层同步的状态
        Popover::new("git-author-filter")
            // 内容自己排版（搜索框贴边、列表自带内边距），照 React 版的 PopoverContent p-0
            .p_0()
            .track_focus(&self.author_search.focus_handle(cx))
            .on_open_change(cx.listener(|this, open: &bool, window, cx| {
                // 关掉时清空搜索：下次打开该是完整列表，而不是上次翻到一半的样子
                if !*open {
                    this.author_search.update(cx, |s, cx| s.set_value("", window, cx));
                }
                cx.notify();
            }))
            .trigger(trigger)
            .content(move |_, window, cx| {
                let popover = cx.entity();
                this.update(cx, |this, cx| this.author_menu(popover, window, cx))
            })
            .into_any_element()
    }

    fn author_menu(
        &mut self,
        popover: Entity<PopoverState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let needle = self.author_search.read(cx).value().trim().to_lowercase();
        let authors = self.refs.as_ref().map(|r| r.authors.clone()).unwrap_or_default();
        // "我"单独提到最上面：按自己筛是最常用的一档。me 得真的在作者名单里才给——配了
        // user.name 但没在这个仓库提交过的话，点下去只会得到一页空白
        let me = self
            .refs
            .as_ref()
            .and_then(|r| r.me.clone())
            .filter(|m| authors.contains(m));
        let matches = |s: &str| needle.is_empty() || s.to_lowercase().contains(&needle);
        let mut list = div()
            .id("git-author-list")
            .max_h(zpx(288.))
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col();
        list = list.child(self.filter_row(
            &popover,
            "all",
            t!("git.allUsers").to_string(),
            None,
            self.author.is_none(),
            true,
            cx,
        ));
        let mut any = false;
        if let Some(me) = &me {
            let label = t!("git.me").to_string();
            if matches(&label) {
                any = true;
                list = list.child(self.filter_row(
                    &popover,
                    "me",
                    label,
                    Some(me.clone()),
                    self.author.as_deref() == Some(me.as_str()),
                    false,
                    cx,
                ));
            }
        }
        let visible: Vec<&String> = authors.iter().filter(|a| matches(a)).collect();
        if !visible.is_empty() {
            if me.is_some() && any {
                list = list.child(div().my_1().h(zpx(1.)).bg(ui.border));
            }
            any = true;
            for (i, a) in visible.into_iter().enumerate() {
                let on = self.author.as_deref() == Some(a.as_str());
                list =
                    list.child(self.filter_row(&popover, &format!("a{i}"), a.clone(), Some(a.clone()), on, false, cx));
            }
        }
        if !any {
            list = list.child(
                div()
                    .px_2()
                    .py_3()
                    .flex()
                    .justify_center()
                    .text_xs()
                    .text_color(ui.muted_foreground)
                    .child(t!("git.noFilterMatch").to_string()),
            );
        }
        div()
            .w(zpx(256.))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(zpx(36.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(zpx(10.))
                    .border_b_1()
                    .border_color(ui.border)
                    .child(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.author_search).appearance(false).small()),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    /// 下拉里的一行。选中项整行铺主色底
    #[allow(clippy::too_many_arguments)]
    fn filter_row(
        &self,
        popover: &Entity<PopoverState>,
        id: &str,
        label: String,
        value: Option<String>,
        selected: bool,
        bold: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let tip: SharedString = label.clone().into();
        div()
            .id(SharedString::from(format!("git-author-{id}")))
            .h(zpx(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(radius::MD)
            .text_xs()
            .cursor_pointer()
            .when(bold, |d| d.font_weight(FontWeight::MEDIUM))
            .when(selected, |d| d.bg(ui.primary).text_color(ui.primary_foreground))
            .when(!selected, |d| d.hover(|s| s.bg(ui.muted)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click({
                // 不用 cx.listener：收起弹层会回调 on_open_change（它要更新本面板），
                // 在本面板的 update 里再 update 自己会 panic
                let (popover, panel) = (popover.clone(), cx.entity());
                move |_, window, cx| {
                    popover.update(cx, |s, cx| s.dismiss(window, cx));
                    panel.update(cx, |this, cx| this.set_author(value.clone(), cx));
                }
            })
            // 圆点那一格恒定占位：有点没点的行左边缘要对齐
            .child(div().size(zpx(6.)).flex_none())
            .child(div().min_w_0().truncate().child(label))
            .into_any_element()
    }

    // ---------------- 分支树 ----------------

    fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = vec![TreeRow::All];
        let Some(refs) = &self.refs else { return rows };
        let local: Vec<_> = refs.branches.iter().filter(|b| !b.remote).collect();
        let remote: Vec<_> = refs.branches.iter().filter(|b| b.remote).collect();
        let head_upstream = local.iter().find(|b| b.head).and_then(|b| b.upstream.clone());
        if !local.is_empty() {
            rows.push(TreeRow::Group("git.localBranches"));
            rows.extend(local.iter().map(|b| TreeRow::Ref {
                label: GitRefLabel {
                    name: b.name.clone(),
                    kind: GitRefKind::Local,
                    head: Some(b.head),
                },
                dot: b.head,
            }));
        }
        if !remote.is_empty() {
            rows.push(TreeRow::Group("git.remoteBranches"));
            rows.extend(remote.iter().map(|b| TreeRow::Ref {
                label: GitRefLabel {
                    name: b.name.clone(),
                    kind: GitRefKind::Remote,
                    head: None,
                },
                dot: head_upstream.as_deref() == Some(b.name.as_str()),
            }));
        }
        if !refs.tags.is_empty() {
            rows.push(TreeRow::Group("git.tags"));
            rows.extend(refs.tags.iter().map(|name| TreeRow::Ref {
                label: GitRefLabel {
                    name: name.clone(),
                    kind: GitRefKind::Tag,
                    head: None,
                },
                dot: false,
            }));
        }
        rows
    }

    /// 窄栏里的分支树（IDEA Log 左侧那栏在 260px 里放不下，所以叠在提交列表上面）：点一行
    /// 就是筛 log，右键走检出 / 合并 / 变基
    fn branch_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let open = self.tree_open;
        let toggle = div()
            .id("git-tree-toggle")
            .h(zpx(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .text_size(zpx(11.))
            .text_color(ui.muted_foreground)
            .cursor_pointer()
            .hover(|s| s.text_color(ui.foreground))
            .tooltip(move |window, cx| {
                Tooltip::new(
                    t!(if open {
                        "git.collapseBranches"
                    } else {
                        "git.expandBranches"
                    })
                    .to_string(),
                )
                .build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.tree_open = !this.tree_open;
                cx.notify();
            }))
            .child(
                icon(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(zpx(12.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(t!("git.branchesTree").to_string()),
            );
        let mut wrap = div()
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(ui.border)
            .child(toggle);
        if open {
            let rows = Rc::new(self.tree_rows());
            let height = (rows.len() as f32 * TREE_ROW_H + 4.).min(TREE_MAX_H);
            let list = uniform_list(
                "git-tree",
                rows.len(),
                cx.processor(move |this: &mut Self, range: Range<usize>, _, cx| {
                    range.map(|i| this.tree_row(&rows[i], i, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.tree_scroll)
            .size_full();
            wrap = wrap.child(div().h(zpx(height)).px_1().pb_1().child(list));
        }
        wrap.into_any_element()
    }

    fn tree_row(&self, row: &TreeRow, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        match row {
            TreeRow::Group(key) => div()
                .h(zpx(TREE_ROW_H))
                .px_2()
                .flex()
                .items_end()
                .pb(zpx(2.))
                .text_size(zpx(10.))
                .text_color(ui.muted_foreground)
                .child(t!(*key).to_string())
                .into_any_element(),
            TreeRow::All | TreeRow::Ref { .. } => {
                let (label, value, dot, ref_label) = match row {
                    TreeRow::Ref { label, dot } => {
                        (label.name.clone(), Some(label.name.clone()), *dot, Some(label.clone()))
                    }
                    _ => (t!("git.allRefs").to_string(), None, false, None),
                };
                let active = self.branch == value;
                let tip: SharedString = label.clone().into();
                let mut el = div()
                    .id(SharedString::from(format!("git-tree-{i}")))
                    .h(zpx(TREE_ROW_H))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(zpx(6.))
                    .px_2()
                    .rounded(radius::MD)
                    .text_size(zpx(11.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(ui.tint).text_color(ui.tint_foreground))
                    .when(!active, |d| d.hover(|s| s.bg(ui.muted)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_branch(value.clone(), cx)))
                    .child(
                        div()
                            .size(zpx(6.))
                            .flex_none()
                            .rounded_full()
                            .when(dot, |d| d.bg(if active { ui.tint_foreground } else { ui.graph[0] })),
                    )
                    .child(div().min_w_0().truncate().child(label));
                if let Some(ref_label) = ref_label {
                    el = el.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.menu_target = Some(MenuTarget::Ref(ref_label.clone()));
                            cx.stop_propagation();
                        }),
                    );
                }
                el.into_any_element()
            }
        }
    }

    // ---------------- 提交列表 ----------------

    fn commit_list(&self, filtered: bool, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = if let Some(err) = &self.log_error {
            hint(t!("git.loadFailed").to_string(), Some(err.clone()), cx).into_any_element()
        } else if self.commits.is_empty() {
            let key = if self.loading {
                "git.loading"
            } else if filtered {
                "git.noMatch"
            } else {
                "git.noCommits"
            };
            hint(t!(key).to_string(), None, cx).into_any_element()
        } else {
            let count = self.commits.len() + usize::from(self.has_more);
            uniform_list(
                "git-commits",
                count,
                cx.processor(|this: &mut Self, range: Range<usize>, _, cx| {
                    range.map(|i| this.commit_row(i, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .size_full()
            .into_any_element()
        };
        div().flex_1().min_h_0().pt(zpx(6.)).child(body).into_any_element()
    }

    fn commit_row(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let Some(commit) = self.commits.get(i) else {
            // 列表尾巴：加载更多
            let loading = self.loading_more;
            return div()
                .h(zpx(ROW_H))
                .px_3()
                .flex()
                .items_center()
                .child(
                    Button::new("git-load-more")
                        .ghost()
                        .small()
                        .w_full()
                        .h(zpx(28.))
                        .text_xs()
                        .text_color(ui.muted_foreground)
                        .label(t!(if loading { "git.loading" } else { "git.loadMore" }).to_string())
                        .disabled(loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
                .into_any_element();
        };
        let selected = self.selected.as_deref() == Some(commit.sha.as_str());
        let sha = commit.sha.clone();
        let sha_menu = commit.sha.clone();
        let tip: SharedString = commit.subject.clone().into();
        let meta = if commit.authored_at != 0 {
            format!("{} · {}", commit.author, format_when(commit.authored_at))
        } else {
            commit.author.clone()
        };
        let mut title = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(zpx(6.))
            .child(div().min_w_0().truncate().text_xs().child(commit.subject.clone()));
        for (j, r) in commit.refs.iter().enumerate() {
            title = title.child(self.ref_badge(r, i, j, cx));
        }
        let graph = self
            .graph
            .get(i)
            .map(|row| graph_cell(row, ui.graph, ui.background).into_any_element());
        div()
            .id(SharedString::from(format!("git-commit-{sha}")))
            .h(zpx(ROW_H))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .pl_2()
            .pr_3()
            .cursor_pointer()
            .when(selected, |d| d.bg(ui.tint).text_color(ui.tint_foreground))
            .when(!selected, |d| d.hover(|s| s.bg(ui.muted.opacity(0.5))))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                let next = if this.selected.as_deref() == Some(sha.as_str()) {
                    None
                } else {
                    Some(sha.clone())
                };
                this.select(next, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.menu_target = Some(MenuTarget::Commit(sha_menu.clone()));
                    this.select(Some(sha_menu.clone()), cx);
                }),
            )
            .children(graph)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .justify_center()
                    .gap(zpx(1.))
                    .child(title)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(11.))
                            .line_height(zpx(16.))
                            .text_color(if selected {
                                ui.tint_foreground.opacity(0.7)
                            } else {
                                ui.muted_foreground
                            })
                            .child(meta),
                    ),
            )
            .into_any_element()
    }

    /// 分支 / 标签徽标。本地分支与远程分支要一眼分得开，所以不共用一个色
    fn ref_badge(&self, r: &GitRefLabel, i: usize, j: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let (bg, fg) = match r.kind {
            GitRefKind::Tag => (ui.warning.opacity(0.15), ui.warning),
            GitRefKind::Remote => (ui.muted, ui.muted_foreground),
            _ => (ui.primary.opacity(0.15), ui.foreground),
        };
        let label = r.clone();
        div()
            .id(SharedString::from(format!("git-ref-{i}-{j}")))
            .max_w(zpx(112.))
            .flex_none()
            .truncate()
            .rounded(zpx(4.))
            .px_1()
            .text_size(zpx(10.))
            // 行高写死：行高固定成 ROW_H 之后，徽标不能再靠继承来的行高去撑
            .line_height(zpx(16.))
            .bg(bg)
            .text_color(fg)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.menu_target = Some(MenuTarget::Ref(label.clone()));
                    cx.stop_propagation();
                }),
            )
            .child(r.name.clone())
            .into_any_element()
    }

    // ---------------- 提交详情 ----------------

    fn detail_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let Some(d) = &self.detail else {
            return div().into_any_element();
        };
        let short = d
            .data
            .as_ref()
            .map(|x| x.short.clone())
            .unwrap_or_else(|| d.sha.chars().take(7).collect());
        let who = d
            .data
            .as_ref()
            .map(|x| format!("{}, {}", x.author, format_when(x.authored_at)))
            .unwrap_or_default();
        let copied = d.copied;
        let header = div()
            .h(zpx(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap(zpx(6.))
            .px_3()
            .child(
                div()
                    .flex_none()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(short),
            )
            .child(crate::ui::icon_button(
                "git-detail-copy",
                if copied { IconName::Check } else { IconName::Copy },
                t!("git.copySha").to_string(),
                zpx(18.),
                cx,
                {
                    let e = cx.entity();
                    move |_, _, cx| e.update(cx, |this, cx| this.copy_detail_sha(cx))
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(who),
            )
            .child(file_view_toggle("git-detail", cx))
            .child(
                tool_button(
                    "git-detail-close",
                    IconName::X,
                    t!("git.closeDetail").to_string(),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
            );
        let wrap = div()
            .flex_none()
            .max_h(relative(0.55))
            .min_h_0()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(ui.border)
            .child(header);
        if let Some(err) = &d.error {
            return wrap.child(hint(err.clone(), None, cx)).into_any_element();
        }
        let Some(data) = &d.data else {
            return wrap
                .child(hint(t!("git.loading").to_string(), None, cx))
                .into_any_element();
        };
        let subject = data.message.split('\n').next().unwrap_or("").to_string();
        let body = data.message[subject.len()..].trim().to_string();
        let expanded = d.expanded;
        let mut msg_row = div().flex().items_start().gap(zpx(6.)).child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .line_height(zpx(20.))
                .font_weight(FontWeight::MEDIUM)
                .when(!expanded, |d| d.line_clamp(2))
                .child(if expanded {
                    data.message.trim_end().to_string()
                } else {
                    subject
                }),
        );
        if !body.is_empty() {
            msg_row = msg_row.child(crate::ui::icon_button(
                "git-detail-expand",
                if expanded {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                },
                t!(if expanded { "git.collapse" } else { "git.expand" }).to_string(),
                zpx(18.),
                cx,
                {
                    let e = cx.entity();
                    move |_, _, cx| {
                        e.update(cx, |this, cx| {
                            if let Some(d) = this.detail.as_mut() {
                                d.expanded = !d.expanded;
                            }
                            cx.notify();
                        })
                    }
                },
            ));
        }
        // 展开的长说明可能比面板还高：给它一个上限，自己滚
        let msg = div()
            .id("git-detail-msg")
            .flex_none()
            .px_3()
            .pb(zpx(10.))
            .when(expanded, |d| d.max_h(zpx(200.)).overflow_y_scroll())
            .child(msg_row);

        let mut rows = build_rows(&d.files, file_view_mode(cx), None, &d.collapsed);
        if data.file_count as usize > d.files.len() {
            rows.push(FileRow::Note(
                t!("git.moreFiles", n = data.file_count as usize - d.files.len()).to_string(),
            ));
        }
        if data.file_count == 0 {
            rows.push(FileRow::Note(t!("git.noFileChanges").to_string()));
        }
        let rows = Rc::new(rows);
        let n = rows.len();
        let list = uniform_list(
            "git-detail-files",
            n,
            cx.processor(move |this: &mut Self, range: Range<usize>, _, cx| {
                let Some(d) = this.detail.as_ref() else {
                    return Vec::new();
                };
                range
                    .map(|i| render_file_row("git-detail", &rows[i], &d.files, None, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&d.scroll)
        .w_full()
        .h(zpx(n as f32 * FILE_ROW_H))
        .flex_shrink(1.)
        .min_h_0();
        // 文件列表按内容高，详情顶到 55% 时由它让出高度（header 与说明不缩）
        wrap.child(msg)
            .child(div().flex_shrink(1.).min_h_0().pb_2().flex().flex_col().child(list))
            .into_any_element()
    }
}
