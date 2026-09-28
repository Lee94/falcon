//! 右侧「修改」面板（web 的 `components/ChangesPanel.tsx`）：工作区里全部未提交的改动，
//! 勾选后提交；修订上次提交、提交并推送、丢弃所选、冲突时继续 / 中止 / 取 ours / theirs。
//!
//! 勾选状态**不写 index**，只是"这次要提交哪些"的前端选择。服务端按 pathspec 提交那些路径，
//! 用户在终端里 `git add` 的暂存内容不受影响（见服务端 git/command.ts 的 commitArgs 注释）。
//!
//! 轮询 4s（比侧栏那条 8s 快：这个面板是用户正盯着看的，改完文件切回来还显示旧列表会让人
//! 以为坏了），只在面板真的显示着时轮询——右侧栏切走时视图不卸载，但不该还在后台打 git。

use std::collections::HashSet;
use std::time::Duration;

use falcon_proto::{GitCommitInput, GitConflictKind, GitOpInput, GitSyncResult, GitTakeSide, GitWorkingChanges};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, IntoElement, Render, Task, UniformListScrollHandle, Window, div,
    uniform_list,
};
use rust_i18n::t;

use super::git::common::{
    self, Check, FileItem, FileListHost, FileRow, build_rows, checkbox, file_view_mode, file_view_toggle, hint,
    porcelain_status, render_file_row, tool_button,
};
use crate::dialogs::{self, ConfirmOpts};
use crate::theme::Ui;
use crate::workspace::{RightPanelId, ToastKind, Workspace};
use crate::zoom::zpx;

const POLL: Duration = Duration::from_millis(4000);

/// 提交的键位提示。不注册成全局绑定——只在提交框里生效，全局绑定会在终端里抢走 ⌘↵
#[cfg(target_os = "macos")]
const COMMIT_CHORD: &str = "⌘↵";
#[cfg(not(target_os = "macos"))]
const COMMIT_CHORD: &str = "Ctrl+↵";

pub struct ChangesPanel {
    ws: Entity<Workspace>,
    /// (项目, 多仓库成员)：换了就清空重来
    target: Option<(String, Option<String>)>,
    data: Option<GitWorkingChanges>,
    files: Vec<FileItem>,
    error: Option<String>,
    /// 手动刷新的转圈
    busy: bool,
    committing: bool,
    amend: bool,
    /// 存的是**取消勾选**的那些，不是选中的：轮询里新出现的文件天然是选中的——反过来存
    /// 选中集的话，每轮都要跟新列表做一次并集，而"刚保存的文件没被勾上"是最容易漏掉的 bug
    excluded: HashSet<String>,
    /// 目录树里折叠着的目录
    collapsed: HashSet<String>,
    message: Entity<TextareaState>,
    list_scroll: UniformListScrollHandle,
    /// 目标每换一次加一：晚到的旧结果丢掉
    seq: u64,
    in_flight: Option<u64>,
    visible: bool,
    /// 提交成功后要清空提交信息，但 set_value 要 Window，请求回调里没有——留到下一次渲染做
    pending_clear: bool,
    _poll: Task<()>,
}

impl ChangesPanel {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(t!("changes.messagePlaceholder").to_string()));
        cx.subscribe(&message, |_, _, ev: &InputEvent, cx| {
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
                        this.load(cx);
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
            data: None,
            files: Vec::new(),
            error: None,
            busy: false,
            committing: false,
            amend: false,
            excluded: HashSet::new(),
            collapsed: HashSet::new(),
            message,
            list_scroll: UniformListScrollHandle::new(),
            seq: 0,
            in_flight: None,
            visible: false,
            pending_clear: false,
            _poll: poll,
        };
        this.sync(cx);
        this
    }

    fn repo(&self) -> Option<String> {
        self.target.as_ref().and_then(|t| t.1.clone())
    }

    fn project_id(&self) -> Option<String> {
        self.target.as_ref().map(|t| t.0.clone())
    }

    /// 跟着工作区走：换项目 / 换成员就清空重来，面板重新露出来时立刻拉一次
    fn sync(&mut self, cx: &mut Context<Self>) {
        let ws = self.ws.read(cx);
        let visible = ws.ready() && ws.state.right_open && ws.state.right_panel == RightPanelId::Changes;
        let target = ws.focus_project_id().map(|pid| {
            let repo = falcon_core::workspace::select_multi_repo_dir(&ws.state.multi_repo, ws.project(&pid));
            (pid, repo)
        });
        let changed = target != self.target;
        if changed {
            self.target = target;
            self.seq += 1;
            self.data = None;
            self.files.clear();
            self.error = None;
            self.excluded.clear();
            self.collapsed.clear();
            self.amend = false;
            self.busy = false;
            self.clear_message(cx);
        }
        let appeared = visible && !self.visible;
        self.visible = visible;
        if visible && (changed || appeared) {
            self.load(cx);
        }
    }

    fn clear_message(&mut self, cx: &mut Context<Self>) {
        self.pending_clear = true;
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id() else {
            return;
        };
        if self.in_flight == Some(self.seq) {
            return;
        }
        let seq = self.seq;
        self.in_flight = Some(seq);
        let repo = self.repo();
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.git_working(&pid, repo.as_deref()).await;
            this.update(cx, |this, cx| {
                if this.in_flight == Some(seq) {
                    this.in_flight = None;
                }
                if this.seq != seq {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(next) => {
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
                        this.data = Some(next);
                        this.error = None;
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
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        // 写操作之后：上一轮轮询的结果可能是动手之前的，作废它
        self.seq += 1;
        self.in_flight = None;
        self.load(cx);
    }

    fn selected(&self) -> Vec<&FileItem> {
        self.files.iter().filter(|f| !self.excluded.contains(&f.path)).collect()
    }

    fn toast(&self, ok: bool, title: String, detail: String, cx: &mut Context<Self>) {
        // 失败时 detail 是 git 的原话——"为什么被拒"只有它说得清
        let kind = if ok { ToastKind::Success } else { ToastKind::Danger };
        let body = (!detail.trim().is_empty()).then_some(detail);
        // 失败的原话要读完（往往还得照着敲命令），不自动消失（web 的 sticky: !res.ok）
        self.ws.update(cx, |w, cx| if ok { w.toast(kind, title, body, cx) } else { w.toast_sticky(kind, title, body, cx) });
    }

    /// 发一次写操作：转圈、toast、成功后刷新。`after_ok` 在成功时额外清理本地状态
    fn run(
        &mut self,
        title: String,
        req: impl std::future::Future<Output = falcon_client::ApiResult<GitSyncResult>> + 'static,
        after_ok: fn(&mut Self),
        cx: &mut Context<Self>,
    ) {
        self.committing = true;
        cx.notify();
        let seq = self.seq;
        cx.spawn(async move |this, cx| {
            let result = req.await;
            this.update(cx, |this, cx| {
                this.committing = false;
                match result {
                    Ok(res) => {
                        this.toast(res.ok, title, res.detail, cx);
                        if res.ok && this.seq == seq {
                            after_ok(this);
                            this.reload(cx);
                        }
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| {
                            w.handle_error(&err, cx);
                            w.toast(ToastKind::Danger, title, Some(err.to_string()), cx);
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn can_commit(&self, cx: &Context<Self>) -> bool {
        !self.selected().is_empty()
            && !self.committing
            && (self.amend || !self.message.read(cx).value().trim().is_empty())
    }

    fn commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id() else { return };
        if !self.can_commit(cx) {
            return;
        }
        let selected = self.selected();
        // 全选时走 all（服务端 git add -A + 无 pathspec 的 commit）：命令行长度恒定，"提交
        // 全部改动"不受文件数限制。列表被截断时更是只能走它
        let all = selected.len() == self.files.len();
        let paths = (!all).then(|| {
            // 重命名要把新旧两个路径都给 git，只给新路径会丢掉删除那一半
            selected
                .iter()
                .flat_map(|f| f.orig_path.iter().cloned().chain(std::iter::once(f.path.clone())))
                .collect::<Vec<_>>()
        });
        let input = GitCommitInput {
            message: self.message.read(cx).value().trim().to_string(),
            all,
            paths,
            amend: Some(self.amend),
            push: Some(push),
        };
        let repo = self.repo();
        let client = self.ws.read(cx).client.clone();
        let title = t!(if push {
            "changes.commitAndPush"
        } else {
            "changes.commit"
        })
        .to_string();
        self.run(
            title,
            async move { client.git_commit_changes(&pid, &input, repo.as_deref()).await },
            |this| {
                this.amend = false;
                this.excluded.clear();
                this.pending_clear = true;
            },
            cx,
        );
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id() else { return };
        let n = self.selected().len();
        if self.committing || n == 0 {
            return;
        }
        let this = cx.entity();
        dialogs::confirm(
            ConfirmOpts {
                title: t!("changes.discardTitle", n = n).to_string(),
                body: t!("changes.discardBody").to_string(),
                confirm_label: t!("changes.discardConfirm").to_string(),
                ..Default::default()
            },
            move |_, cx| {
                this.update(cx, |this, cx| {
                    let mut tracked = Vec::new();
                    let mut untracked = Vec::new();
                    for f in this.selected() {
                        if f.status == "?" {
                            untracked.push(f.path.clone());
                        } else {
                            tracked.extend(f.orig_path.iter().cloned());
                            tracked.push(f.path.clone());
                        }
                    }
                    let op = GitOpInput::Restore {
                        paths: tracked,
                        untracked: Some(untracked),
                    };
                    let repo = this.repo();
                    let client = this.ws.read(cx).client.clone();
                    let pid = pid.clone();
                    this.run(
                        t!("changes.discard").to_string(),
                        async move { client.git_op(&pid, &op, repo.as_deref()).await },
                        |this| this.excluded.clear(),
                        cx,
                    );
                })
            },
            window,
            cx,
        );
    }

    fn conflict_op(&mut self, op: GitOpInput, title: String, cx: &mut Context<Self>) {
        let Some(pid) = self.project_id() else { return };
        if self.committing {
            return;
        }
        let repo = self.repo();
        let client = self.ws.read(cx).client.clone();
        self.run(
            title,
            async move { client.git_op(&pid, &op, repo.as_deref()).await },
            |_| {},
            cx,
        );
    }

    fn take(&mut self, side: GitTakeSide, window: &mut Window, cx: &mut Context<Self>) {
        let unmerged: Vec<String> = self
            .selected()
            .iter()
            .filter(|f| f.status == "U")
            .map(|f| f.path.clone())
            .collect();
        if unmerged.is_empty() {
            return;
        }
        let ours = side == GitTakeSide::Ours;
        let label = t!(if ours { "changes.takeOurs" } else { "changes.takeTheirs" }).to_string();
        let this = cx.entity();
        let label2 = label.clone();
        dialogs::confirm(
            ConfirmOpts {
                title: t!(if ours {
                    "changes.takeOursTitle"
                } else {
                    "changes.takeTheirsTitle"
                })
                .to_string(),
                body: t!("changes.takeBody").to_string(),
                confirm_label: label,
                ..Default::default()
            },
            move |_, cx| {
                let op = GitOpInput::Take {
                    side,
                    paths: unmerged.clone(),
                };
                let title = label2.clone();
                this.update(cx, |this, cx| this.conflict_op(op, title, cx));
            },
            window,
            cx,
        );
    }

    fn set_amend(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = next;
        // 勾上"修订"且提交信息还空着：预填上次的提交说明
        let head = self.data.as_ref().and_then(|d| d.head_message.clone());
        if next
            && self.message.read(cx).value().trim().is_empty()
            && let Some(head) = head
        {
            self.message.update(cx, |m, cx| m.set_value(head, window, cx));
        }
        cx.notify();
    }
}

impl FileListHost for ChangesPanel {
    fn open_file(&mut self, idx: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let (Some(pid), Some(f)) = (self.project_id(), self.files.get(idx)) else {
            return;
        };
        let file = falcon_proto::GitFileChange {
            path: f.path.clone(),
            orig_path: f.orig_path.clone(),
            // 差异窗口靠 index == "?" 判断走不走 --no-index 伪 diff
            index: f.status.clone(),
            work: " ".into(),
        };
        let repo = self.repo();
        self.ws.update(cx, |w, cx| w.open_diff(&pid, file, None, repo, cx));
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_string());
        }
        cx.notify();
    }

    fn toggle_select(&mut self, paths: &[String], next: bool, cx: &mut Context<Self>) {
        for p in paths {
            if next {
                self.excluded.remove(p);
            } else {
                self.excluded.insert(p.clone());
            }
        }
        cx.notify();
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.pending_clear) {
            self.message.update(cx, |m, cx| m.set_value("", window, cx));
        }
        let ui = Ui::global(cx).clone();
        let project = self
            .project_id()
            .and_then(|pid| self.ws.read(cx).project(&pid).cloned());

        let busy = self.busy;
        let mut header = div()
            .h(zpx(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
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
                    .child(t!("changes.title").to_string()),
            );
        if let Some(sel) = common::multi_repo_select("changes", &self.ws, project.as_ref(), cx) {
            header = header.child(sel);
        }
        header = header.child(file_view_toggle("changes", cx)).child(
            tool_button(
                "changes-refresh",
                IconName::RefreshCw,
                t!("git.refresh").to_string(),
                false,
                cx,
            )
            .loading(busy)
            .loading_icon(IconName::RefreshCw)
            .disabled(self.target.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                this.busy = true;
                this.reload(cx);
                cx.notify();
            })),
        );

        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .child(header);

        let Some(_) = &self.target else {
            return root.child(hint(t!("changes.noProject").to_string(), None, cx));
        };
        let Some(data) = &self.data else {
            return match &self.error {
                Some(err) => root.child(hint(t!("changes.loadFailed").to_string(), Some(err.clone()), cx)),
                None => root.child(hint(t!("git.loading").to_string(), None, cx)),
            };
        };
        if !data.available {
            return root.child(hint(
                t!(common::reason_key(data.reason)).to_string(),
                data.detail.clone(),
                cx,
            ));
        }
        if data.file_count == 0 && data.conflict.is_none() {
            return root.child(hint(t!("changes.clean").to_string(), None, cx));
        }

        let conflict = data.conflict.as_ref().map(|c| c.kind);
        let file_count = data.file_count;
        let truncated = data.file_count as usize > data.files.len();
        let head_message = data.head_message.is_some();
        let repo_name = data.repo_name.clone();

        let selected_n = self.selected().len();
        let unmerged_n = self.selected().iter().filter(|f| f.status == "U").count();
        let committing = self.committing;
        let mut root = root;

        if let Some(kind) = conflict {
            root = root.child(self.conflict_bar(kind, unmerged_n > 0, file_count == 0, cx));
        }
        if file_count == 0 {
            return root.child(hint(t!("changes.conflictResolved").to_string(), None, cx));
        }

        // ---- 计数条：已选 n / total、丢弃所选、全选 ----
        let all_state = if selected_n == 0 {
            Check::Off
        } else if selected_n == self.files.len() {
            Check::On
        } else {
            Check::Mixed
        };
        let entity = cx.entity();
        root = root.child(
            div()
                .h(zpx(28.))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .pl_3()
                .pr_2()
                .border_b_1()
                .border_color(ui.border)
                .bg(ui.muted.opacity(0.4))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(t!("changes.count", n = selected_n, total = file_count).to_string()),
                )
                .child(
                    Button::new("changes-discard")
                        .ghost()
                        .small()
                        .px(zpx(6.))
                        .icon(IconName::Undo2)
                        .label(t!("changes.discard").to_string())
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .tooltip(t!("changes.discard").to_string())
                        .disabled(selected_n == 0 || committing || conflict.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
                )
                .child(checkbox(
                    "changes-all",
                    all_state,
                    false,
                    Some(t!("changes.selectAll").to_string().into()),
                    cx,
                    // 半选点一下变全选（"我要全部"），与目录行的行为一致
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            let all: Vec<String> = this.files.iter().map(|f| f.path.clone()).collect();
                            this.toggle_select(&all, all_state != Check::On, cx);
                        })
                    },
                )),
        );

        // ---- 文件列表 ----
        let mut rows = build_rows(&self.files, file_view_mode(cx), repo_name.as_deref(), &self.collapsed);
        if truncated {
            rows.push(FileRow::Note(
                t!("git.moreFiles", n = file_count as usize - self.files.len()).to_string(),
            ));
        }
        let rows = std::rc::Rc::new(rows);
        let list = uniform_list(
            "changes-files",
            rows.len(),
            cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|i| render_file_row("changes", &rows[i], &this.files, Some(&this.excluded), cx))
                    .collect::<Vec<AnyElement>>()
            }),
        )
        .track_scroll(&self.list_scroll)
        .flex_1()
        .min_h_0()
        .py_1();
        root = root.child(div().relative().flex_1().min_h_0().flex().flex_col().child(list));

        // ---- 提交框（冲突进行中时不出：那时该做的是继续 / 中止）----
        if conflict.is_none() {
            let can = self.can_commit(cx);
            let amend = self.amend;
            let entity = cx.entity();
            let amend_row = div()
                .id("changes-amend")
                .mt(zpx(6.))
                .flex()
                .items_center()
                .gap_2()
                .text_size(zpx(11.))
                .when(!committing && head_message, |d| d.cursor_pointer())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.committing && head_message {
                        this.set_amend(!amend, window, cx);
                    }
                }))
                .child(checkbox(
                    "changes-amend-box",
                    if amend { Check::On } else { Check::Off },
                    committing || !head_message,
                    None,
                    cx,
                    move |_, window, cx| entity.update(cx, |this, cx| this.set_amend(!amend, window, cx)),
                ))
                .child(t!("changes.amend").to_string());
            let commit_btn = if committing {
                Button::new("changes-commit")
                    .primary()
                    .small()
                    .flex_1()
                    .icon(IconName::RefreshCw)
                    .loading(true)
                    .loading_icon(IconName::RefreshCw)
            } else {
                Button::new("changes-commit")
                    .primary()
                    .small()
                    .flex_1()
                    .child(t!("changes.commit").to_string())
                    .child(div().text_color(ui.primary_foreground.opacity(0.6)).child(COMMIT_CHORD))
            }
            .text_xs()
            .disabled(!can)
            .tooltip(format!("{} · {COMMIT_CHORD}", t!("changes.commit")))
            .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx)));
            root = root.child(
                div()
                    .flex_none()
                    .p_2()
                    .border_t_1()
                    .border_color(ui.border)
                    // ⌘/Ctrl+Enter 提交。光 Enter 不行——提交信息是多行的，正文换行比少敲一个
                    // 修饰键重要得多。在捕获阶段拦下，输入框就不会顺手插一个换行
                    .capture_action(cx.listener(|this, e: &Enter, _, cx| {
                        if e.secondary {
                            cx.stop_propagation();
                            this.commit(false, cx);
                        }
                    }))
                    .child(
                        // 高度写死三行（web 的 rows=3）：TextareaState 的 rows 只在 auto_grow 下才影响布局
                        Textarea::new(&self.message).h(zpx(62.)).text_xs(),
                    )
                    .child(amend_row)
                    .child(
                        div().mt(zpx(6.)).flex().gap(zpx(6.)).child(commit_btn).child(
                            Button::new("changes-commit-push")
                                .secondary()
                                .small()
                                .flex_1()
                                .text_xs()
                                .label(t!("changes.commitAndPush").to_string())
                                .tooltip(t!("changes.commitAndPush").to_string())
                                .disabled(!can)
                                .on_click(cx.listener(|this, _, _, cx| this.commit(true, cx))),
                        ),
                    ),
            );
        }
        root
    }
}

impl ChangesPanel {
    fn conflict_bar(&self, kind: GitConflictKind, can_take: bool, empty: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let key = match kind {
            GitConflictKind::CherryPick => "changes.conflict_cherry_pick",
            GitConflictKind::Revert => "changes.conflict_revert",
            GitConflictKind::Rebase => "changes.conflict_rebase",
            _ => "changes.conflict_merge",
        };
        let busy = self.committing;
        let mut buttons = div()
            .mt(zpx(6.))
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new("conflict-continue")
                    .primary()
                    .small()
                    .text_xs()
                    .label(t!("changes.conflictContinue").to_string())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.conflict_op(GitOpInput::Continue, t!("changes.conflictContinue").to_string(), cx)
                    })),
            )
            .child(
                Button::new("conflict-abort")
                    .outline()
                    .small()
                    .text_xs()
                    .label(t!("changes.conflictAbort").to_string())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.conflict_op(GitOpInput::Abort, t!("changes.conflictAbort").to_string(), cx)
                    })),
            );
        if !empty {
            buttons = buttons
                .child(
                    Button::new("conflict-ours")
                        .ghost()
                        .small()
                        .text_xs()
                        .label(t!("changes.takeOurs").to_string())
                        .disabled(busy || !can_take)
                        .on_click(cx.listener(|this, _, window, cx| this.take(GitTakeSide::Ours, window, cx))),
                )
                .child(
                    Button::new("conflict-theirs")
                        .ghost()
                        .small()
                        .text_xs()
                        .label(t!("changes.takeTheirs").to_string())
                        .disabled(busy || !can_take)
                        .on_click(cx.listener(|this, _, window, cx| this.take(GitTakeSide::Theirs, window, cx))),
                );
        }
        div()
            .flex_none()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(ui.border)
            .bg(ui.warning.opacity(0.1))
            .child(
                div()
                    .text_size(zpx(11.))
                    .line_height(zpx(18.))
                    .text_color(ui.warning)
                    .child(t!(key).to_string()),
            )
            .child(buttons)
            .into_any_element()
    }
}
