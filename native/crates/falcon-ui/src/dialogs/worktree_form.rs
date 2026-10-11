//! 派生附属项目表单（旧 React 版的 `components/WorktreeForm.tsx`，ADR 0002 / 0003）。
//!
//! 两张表刻意分开（沿用 React 版）：
//! - **单仓库**：先 `GET /repo` 预检（环境事实写在 derivable / reason 里，不是错误），新建分支
//!   （可选基点）或检出已有分支；目录默认是仓库同级的平铺路径，这里只做**预览**
//!   （`falcon_core::worktree_path`），用户没手改过目录就不上送 dir——预览与服务端万一漂移
//!   也绝不会建到别处去。远程分支收敛进 new-branch + startPoint=origin/x：直接检出 origin/x
//!   会得到 detached HEAD，里面的提交在 remove 之后立刻不可达。
//! - **多仓库容器**：`GET /repos` 逐成员预检，按模式与分支名**预演**每个成员会发生什么
//!   （`falcon_core::multi_derive`，权威判定仍在服务端），有一个会失败就整单拦下（全有或全无）。
//!   提交不带 dir：集中目录由服务端算，这里只渲染只读预览。失败时的成员归因与回滚残留原样呈现。

use falcon_client::DeriveInput;
use falcon_core::multi_derive::{MemberAction, action_blocks_submit, common_local_branches, evaluate_member, member_basename};
use falcon_core::reason::worktree_reason_text;
use falcon_core::worktree_path::{preview_dir, preview_multi_dir};
use falcon_proto::{
    MultiDeriveError, MultiRepoProbe, MultiWorktreeInput, MultiWorktreeMode, Project, RepoBranch, RepoInfo,
    WorktreeInput, WorktreeMode,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use crate::dialogs::project_form::kit::{self, Opt, OptState, Options, SegItem, Tone, Tr, error_line, field, note, segmented};
use crate::theme::{Ui, radius};
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

#[derive(Clone, Debug, Default)]
pub struct Preset {
    /// 飞书项目工作项拖进来时预填的分支名（带了它就是 new-branch 模式，照 React 版的 preset）
    pub branch: Option<String>,
    /// 预填的项目名
    pub name: Option<String>,
    /// 新建分支的基点；缺省用源项目的 defaultWorktreeBranch，再缺省 HEAD
    pub start_point: Option<String>,
}

pub fn open(ws: &Entity<Workspace>, source_project_id: &str, preset: Preset, window: &mut Window, cx: &mut App) {
    let Some(project) = ws.read(cx).project(source_project_id).cloned() else {
        return;
    };
    // 多仓库容器走批量派生表单；单仓库路径对应 React 版的 SingleWorktreeForm
    if project.multi.is_some() {
        let form = cx.new(|cx| MultiForm::new(ws.clone(), project, preset, window, cx));
        let title = t!("multi.deriveTitle", name = form.read(cx).project.name.clone()).to_string();
        open_dialog(form.clone(), title, |f, window, cx| f.update(cx, |this, cx| this.submit(window, cx)), window, cx);
        form.read(cx).branch_text.clone().update(cx, |i, cx| i.focus(window, cx));
    } else {
        let form = cx.new(|cx| SingleForm::new(ws.clone(), project, preset, window, cx));
        let title = t!("worktree.title", name = form.read(cx).project.name.clone()).to_string();
        open_dialog(form.clone(), title, |f, window, cx| f.update(cx, |this, cx| this.submit(window, cx)), window, cx);
        form.read(cx).new_branch.clone().update(cx, |i, cx| i.focus(window, cx));
    }
}

fn open_dialog<V: Render + 'static>(
    form: Entity<V>,
    title: String,
    submit: impl Fn(&Entity<V>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let submit = std::rc::Rc::new(submit);
    window.open_dialog(cx, move |dialog, _, _| {
        let (f, submit) = (form.clone(), submit.clone());
        // lockOverlay：点遮罩不关
        dialog
            .title(title.clone())
            .overlay_closable(false)
            .close_button(false)
            .child(form.clone())
            .on_ok(move |_, window, cx| {
                // 回车 = 提交；成功后由提交流程自己关对话框
                submit(&f, window, cx);
                false
            })
    });
}

fn footer(busy: bool, can_submit: bool, on_submit: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static) -> gpui_kit::Div {
    div()
        .mt_1()
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new("wt-cancel")
                .outline()
                .label(t!("common.cancel").to_string())
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .child(
            Button::new("wt-submit")
                .primary()
                .label(if busy { t!("worktree.creating") } else { t!("worktree.create") }.to_string())
                .disabled(busy || !can_submit)
                .on_click(on_submit),
        )
}

fn reason_line(reason: Option<falcon_proto::WorktreeFailure>, detail: Option<&str>) -> String {
    let mut s = worktree_reason_text(&Tr, reason);
    if let Some(d) = detail.filter(|d| !d.is_empty()) {
        s.push_str(" · ");
        s.push_str(d);
    }
    s
}

/// 把文字输入的变化转成重画 + 回调
fn on_text_change<V: 'static>(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, window, cx| {
        if matches!(ev, InputEvent::Change) {
            f(this, window, cx);
            cx.notify();
        }
    })
    .detach();
}

// ---------------- 单仓库 ----------------

pub struct SingleForm {
    ws: Entity<Workspace>,
    project: Project,
    info: Option<RepoInfo>,
    load_error: Option<String>,
    mode: WorktreeMode,
    new_branch: Entity<InputState>,
    start_point: String,
    start_select: Entity<OptState>,
    picked_ref: String,
    pick_select: Entity<OptState>,
    name: Entity<InputState>,
    dir: Entity<InputState>,
    /// 用户手改过目录：之后不再跟着分支名自动变，提交时才带上 dir
    dir_touched: bool,
    error: Option<String>,
    busy: bool,
}

impl SingleForm {
    fn new(ws: Entity<Workspace>, project: Project, preset: Preset, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_start = project.default_worktree_branch.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "HEAD".into());
        let new_branch = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("worktree.branchPlaceholder").to_string())
                .default_value(preset.branch.clone().unwrap_or_default())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("worktree.namePlaceholder").to_string())
                .default_value(preset.name.clone().unwrap_or_default())
        });
        let dir = cx.new(|cx| InputState::new(window, cx).placeholder(t!("worktree.dirHint").to_string()));
        let start_select = cx.new(|cx| kit::new_select(Options::default(), "", window, cx));
        let pick_select = cx.new(|cx| kit::new_select(Options::default(), "", window, cx));
        on_text_change(&new_branch, window, cx, |this, window, cx| this.sync_derived(window, cx));
        // set_value 不发 Change：只有用户自己敲的才算"改过目录"
        on_text_change(&dir, window, cx, |this, _, _| this.dir_touched = true);
        on_text_change(&name, window, cx, |_, _, _| {});
        cx.subscribe_in(&start_select, window, |this, _, ev: &SelectEvent<Options>, _, cx| {
            if let SelectEvent::Confirm(Some(v)) = ev {
                this.start_point = v.clone();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(&pick_select, window, |this, _, ev: &SelectEvent<Options>, window, cx| {
            if let SelectEvent::Confirm(Some(v)) = ev {
                this.picked_ref = v.clone();
                this.sync_derived(window, cx);
                cx.notify();
            }
        })
        .detach();

        let request = ws.read(cx).client.repo_info(&project.id);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(info) => this.set_info(info, window, cx),
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.load_error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        Self {
            ws,
            project,
            info: None,
            load_error: None,
            mode: WorktreeMode::NewBranch,
            new_branch,
            start_point: preset.start_point.unwrap_or(default_start),
            start_select,
            picked_ref: String::new(),
            pick_select,
            name,
            dir,
            dir_touched: false,
            error: None,
            busy: false,
        }
    }

    fn set_info(&mut self, info: RepoInfo, window: &mut Window, cx: &mut Context<Self>) {
        // 默认选中：源项目配置的默认基点（没被占用时）→ 第一条没被占用的本地分支 → 任一没被占用的
        let preferred = self.project.default_worktree_branch.clone().filter(|s| !s.is_empty());
        let free = |b: &&RepoBranch| b.checked_out_at.is_none();
        let prefer = preferred.as_ref().and_then(|p| {
            info.branches
                .iter()
                .filter(free)
                .find(|b| &b.name == p || b.local_name.as_ref() == Some(p))
        });
        let first_local = info.branches.iter().filter(free).find(|b| !b.remote);
        self.picked_ref = prefer
            .or(first_local)
            .or_else(|| info.branches.iter().find(free))
            .map(|b| b.name.clone())
            .unwrap_or_default();
        self.info = Some(info);
        self.sync_selects(window, cx);
        self.sync_derived(window, cx);
    }

    fn local(&self) -> Vec<RepoBranch> {
        self.info.as_ref().map(|i| i.branches.iter().filter(|b| !b.remote).cloned().collect()).unwrap_or_default()
    }

    fn remote(&self) -> Vec<RepoBranch> {
        self.info.as_ref().map(|i| i.branches.iter().filter(|b| b.remote).cloned().collect()).unwrap_or_default()
    }

    fn picked(&self) -> Option<&RepoBranch> {
        self.info.as_ref()?.branches.iter().find(|b| b.name == self.picked_ref)
    }

    /// 当前会落到哪条**本地**分支上——目录名与项目名都跟着它
    fn effective_branch(&self, cx: &App) -> String {
        match self.mode {
            WorktreeMode::ExistingBranch => self
                .picked()
                .map(|b| b.local_name.clone().unwrap_or_else(|| b.name.clone()))
                .unwrap_or_default(),
            _ => self.new_branch.read(cx).value().trim().to_string(),
        }
    }

    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else { return };
        let (local, remote) = (self.local(), self.remote());

        // 基点：当前 HEAD 顶在最前；预填的基点不在清单里也要能显示出来
        let head_label = match &info.head_branch {
            Some(b) => t!("worktree.startPointHead").replace("%{ref}", b),
            None => t!("worktree.startPointDetached", sha = info.head_sha.clone().unwrap_or_else(|| "?".into())).to_string(),
        };
        let mut top = vec![Opt::new("HEAD", head_label)];
        let known = self.start_point == "HEAD" || info.branches.iter().any(|b| b.name == self.start_point);
        if !known && !self.start_point.is_empty() {
            top.push(Opt::new(self.start_point.clone(), self.start_point.clone()));
        }
        let plain = |bs: &[RepoBranch]| bs.iter().map(|b| Opt::new(b.name.clone(), b.name.clone())).collect::<Vec<_>>();
        let options = Options::grouped(
            top,
            vec![
                (t!("worktree.groupLocal").to_string(), plain(&local)),
                (t!("worktree.groupRemote").to_string(), plain(&remote)),
            ],
        );
        kit::reset_select(&self.start_select, options, &self.start_point, window, cx);

        // 检出已有分支：已在别处检出的灰掉，并写明在哪
        let branch_opt = |b: &RepoBranch| match &b.checked_out_at {
            Some(at) => Opt::new(b.name.clone(), format!("{} · {}", b.name, t!("worktree.inUse", path = at.clone()))).disabled(true),
            None => Opt::new(b.name.clone(), b.name.clone()),
        };
        let options = Options::grouped(
            Vec::new(),
            vec![
                (t!("worktree.groupLocal").to_string(), local.iter().map(branch_opt).collect()),
                (t!("worktree.groupRemote").to_string(), remote.iter().map(branch_opt).collect()),
            ],
        );
        kit::reset_select(&self.pick_select, options, &self.picked_ref, window, cx);
    }

    /// 跟着分支走的两样：目录预览（没手改过时）与项目名的占位
    fn sync_derived(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.effective_branch(cx);
        if !self.dir_touched {
            let auto = match (self.mode, self.picked()) {
                (WorktreeMode::ExistingBranch, Some(p)) => p.suggested_dir.clone(),
                _ => preview_dir(self.info.as_ref().and_then(|i| i.repo_dir.as_deref()).unwrap_or(""), &branch),
            };
            if self.dir.read(cx).value().as_ref() != auto {
                self.dir.update(cx, |i, cx| i.set_value(auto, window, cx));
            }
        }
        let placeholder = if branch.is_empty() { t!("worktree.namePlaceholder").to_string() } else { branch };
        self.name.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
    }

    fn set_mode(&mut self, mode: WorktreeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.sync_derived(window, cx);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.effective_branch(cx);
        if self.busy || branch.is_empty() || !self.info.as_ref().is_some_and(|i| i.derivable) {
            return;
        }
        let local_names: Vec<String> = self.local().into_iter().map(|b| b.name).collect();
        let remote_pick = (self.mode == WorktreeMode::ExistingBranch)
            .then(|| self.picked().filter(|b| b.remote).cloned())
            .flatten();
        let name = self.name.read(cx).value().trim().to_string();
        let dir = self.dir.read(cx).value().trim().to_string();
        let mut input = match (&self.mode, remote_pick) {
            (WorktreeMode::ExistingBranch, Some(r)) if !local_names.contains(&branch) => WorktreeInput {
                name: None,
                mode: WorktreeMode::NewBranch,
                branch: branch.clone(),
                start_point: Some(r.name),
                dir: None,
            },
            (WorktreeMode::ExistingBranch, _) => {
                WorktreeInput { name: None, mode: WorktreeMode::ExistingBranch, branch: branch.clone(), start_point: None, dir: None }
            }
            _ => WorktreeInput {
                name: None,
                mode: WorktreeMode::NewBranch,
                branch: branch.clone(),
                start_point: Some(self.start_point.clone()),
                dir: None,
            },
        };
        input.name = (!name.is_empty()).then_some(name);
        input.dir = (self.dir_touched && !dir.is_empty()).then_some(dir);
        self.busy = true;
        self.error = None;
        let request = self.ws.read(cx).client.create_worktree(&self.project.id, DeriveInput::Single(input));
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(created) => {
                        let body = t!(
                            "worktree.createdBody",
                            dir = created.working_dir.clone().unwrap_or_default(),
                            branch = created.worktree.as_ref().map(|w| w.branch.clone()).unwrap_or(branch.clone())
                        );
                        this.ws.update(cx, |w, cx| {
                            w.refresh_projects(cx);
                            w.refresh_hosts(cx);
                            w.toast(
                                ToastKind::Success,
                                t!("worktree.created", name = created.name.clone()).to_string(),
                                Some(body.to_string()),
                                cx,
                            );
                        });
                        window.close_dialog(cx);
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for SingleForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut d = div().flex().flex_col().gap_3();
        let Some(info) = self.info.clone() else {
            return match self.load_error.clone() {
                Some(e) => d.child(error_line(e, cx)),
                None => d.child(note(t!("worktree.probing").to_string(), cx)),
            };
        };
        if !info.derivable {
            return d
                .child(div().text_sm().text_color(Ui::global(cx).muted_foreground).child(t!("worktree.notDerivable").to_string()))
                .child(error_line(reason_line(info.reason, info.detail.as_deref()), cx))
                .child(
                    div().flex().justify_end().child(
                        Button::new("wt-close")
                            .outline()
                            .label(t!("common.close").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                );
        }
        d = d.child(note(t!("worktree.intro", repo = info.repo_dir.clone().unwrap_or_default()).to_string(), cx));
        let seg = |mode: WorktreeMode, label: String, cx: &mut Context<Self>| SegItem {
            label,
            on: self.mode == mode,
            on_click: Box::new(cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx))),
        };
        let items = vec![
            seg(WorktreeMode::NewBranch, t!("worktree.modeNew").to_string(), cx),
            seg(WorktreeMode::ExistingBranch, t!("worktree.modeExisting").to_string(), cx),
        ];
        d = d.child(segmented("wt-mode", items, cx));
        if self.mode == WorktreeMode::ExistingBranch {
            d = d.child(field(Some(t!("worktree.pickBranch").to_string()), Select::new(&self.pick_select).w_full(), None, cx));
        } else {
            d = d
                .child(field(Some(t!("worktree.branch").to_string()), Input::new(&self.new_branch), None, cx))
                .child(field(Some(t!("worktree.startPoint").to_string()), Select::new(&self.start_select).w_full(), None, cx));
        }
        let occupied = self.mode == WorktreeMode::ExistingBranch && self.picked().is_some_and(|p| p.dir_occupied) && !self.dir_touched;
        let hint = if occupied {
            (t!("worktree.dirOccupied").to_string(), Tone::Err)
        } else {
            (t!("worktree.dirHint").to_string(), Tone::Muted)
        };
        d = d
            .child(field(Some(t!("worktree.targetDir").to_string()), Input::new(&self.dir), Some(hint), cx))
            .child(field(Some(t!("worktree.name").to_string()), Input::new(&self.name), None, cx));
        if let Some(e) = self.error.clone() {
            d = d.child(error_line(e, cx));
        }
        let can = !self.effective_branch(cx).is_empty();
        d.child(footer(self.busy, can, cx.listener(|this, _, window, cx| this.submit(window, cx))))
    }
}

// ---------------- 多仓库批量派生 ----------------

pub struct MultiForm {
    ws: Entity<Workspace>,
    project: Project,
    probe: Option<MultiRepoProbe>,
    load_error: Option<String>,
    mode: MultiWorktreeMode,
    branch_text: Entity<InputState>,
    picked_ref: String,
    pick_select: Entity<OptState>,
    name: Entity<InputState>,
    error: Option<String>,
    busy: bool,
}

impl MultiForm {
    fn new(ws: Entity<Workspace>, project: Project, preset: Preset, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let branch_text = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("worktree.branchPlaceholder").to_string())
                .default_value(preset.branch.clone().unwrap_or_default())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("worktree.namePlaceholder").to_string())
                .default_value(preset.name.clone().unwrap_or_default())
        });
        let pick_select = cx.new(|cx| kit::new_select(Options::default(), "", window, cx));
        on_text_change(&branch_text, window, cx, |this, window, cx| this.sync_name(window, cx));
        on_text_change(&name, window, cx, |_, _, _| {});
        cx.subscribe_in(&pick_select, window, |this, _, ev: &SelectEvent<Options>, window, cx| {
            if let SelectEvent::Confirm(Some(v)) = ev {
                this.picked_ref = v.clone();
                this.sync_name(window, cx);
                cx.notify();
            }
        })
        .detach();
        let request = ws.read(cx).client.repo_info_multi(&project.id);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(probe) => {
                        this.probe = Some(probe);
                        this.sync_select(window, cx);
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.load_error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            ws,
            project,
            probe: None,
            load_error: None,
            // 带着预填分支进来（飞书项目拖拽）是 new-branch（照 React 版的 preset.mode）
            mode: if preset.branch.is_some() { MultiWorktreeMode::NewBranch } else { MultiWorktreeMode::Auto },
            branch_text,
            picked_ref: String::new(),
            pick_select,
            name,
            error: None,
            busy: false,
        }
    }

    fn branch(&self, cx: &App) -> String {
        match self.mode {
            MultiWorktreeMode::ExistingBranch => self.picked_ref.trim().to_string(),
            _ => self.branch_text.read(cx).value().trim().to_string(),
        }
    }

    fn sync_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(probe) = &self.probe else { return };
        let infos: Vec<&RepoInfo> = probe.members.iter().map(|m| &m.info).collect();
        let items = common_local_branches(&infos)
            .into_iter()
            .map(|b| match b.used_at {
                Some(at) => Opt::new(b.name.clone(), format!("{} · {}", b.name, t!("worktree.inUse", path = at))).disabled(true),
                None => Opt::new(b.name.clone(), b.name),
            })
            .collect();
        kit::reset_select(&self.pick_select, Options::flat(items), &self.picked_ref, window, cx);
    }

    fn sync_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.branch(cx);
        let placeholder = if branch.is_empty() { t!("worktree.namePlaceholder").to_string() } else { branch };
        self.name.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
    }

    fn set_mode(&mut self, mode: MultiWorktreeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.sync_name(window, cx);
        cx.notify();
    }

    /// 逐成员预演：分支名还没填时只报"能不能派生"
    fn evals(&self, cx: &App) -> Vec<(String, RepoInfo, Option<MemberAction>)> {
        let branch = self.branch(cx);
        self.probe
            .as_ref()
            .map(|p| {
                p.members
                    .iter()
                    .map(|m| {
                        let action = (!branch.is_empty()).then(|| evaluate_member(&m.info, self.mode, &branch));
                        (m.dir.clone(), m.info.clone(), action)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn blocked_count(&self, cx: &App) -> usize {
        self.evals(cx)
            .iter()
            .filter(|(_, info, action)| match action {
                Some(a) => action_blocks_submit(a),
                None => !info.derivable,
            })
            .count()
    }

    fn member_line(&self, info: &RepoInfo, action: &Option<MemberAction>) -> (String, bool) {
        let Some(action) = action else {
            return if info.derivable {
                let head = info.head_branch.clone().or(info.head_sha.clone()).unwrap_or_else(|| "?".into());
                (t!("multi.memberOk", head = head).to_string(), false)
            } else {
                (reason_line(info.reason, info.detail.as_deref()), true)
            };
        };
        match action {
            MemberAction::Blocked { reason, detail } => (reason_line(*reason, detail.as_deref()), true),
            MemberAction::Create => match self.project.default_worktree_branch.clone().filter(|s| !s.is_empty()) {
                Some(r) => // `ref` 是关键字，不能当 t! 的参数名
                (t!("multi.willCreateFrom").replace("%{ref}", &r), false),
                None => (t!("multi.willCreate").to_string(), false),
            },
            MemberAction::Checkout => (t!("multi.willCheckout").to_string(), false),
            MemberAction::BranchExists => (t!("multi.memberBranchExists").to_string(), true),
            MemberAction::BranchMissing => (t!("multi.memberBranchMissing").to_string(), true),
            MemberAction::BranchInUse { at } => (t!("worktree.inUse", path = at.clone()).to_string(), true),
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.branch(cx);
        if self.busy || branch.is_empty() || self.probe.is_none() || self.blocked_count(cx) > 0 {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let members = self.probe.as_ref().map(|p| p.members.len()).unwrap_or(0);
        let input = MultiWorktreeInput { name: (!name.is_empty()).then_some(name), mode: self.mode, branch: branch.clone(), dir: None };
        self.busy = true;
        self.error = None;
        let request = self.ws.read(cx).client.create_worktree(&self.project.id, DeriveInput::Multi(input));
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(created) => {
                        let n = created.multi.as_ref().map(|m| m.repos.len()).unwrap_or(members);
                        let body = t!(
                            "multi.createdBody",
                            dir = created.working_dir.clone().unwrap_or_default(),
                            n = n,
                            branch = created.worktree.as_ref().map(|w| w.branch.clone()).unwrap_or(branch.clone())
                        );
                        this.ws.update(cx, |w, cx| {
                            w.refresh_projects(cx);
                            w.refresh_hosts(cx);
                            w.toast(
                                ToastKind::Success,
                                t!("worktree.created", name = created.name.clone()).to_string(),
                                Some(body.to_string()),
                                cx,
                            );
                        });
                        window.close_dialog(cx);
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        // 服务端的成员归因错误体优先；读不出结构就原样显示 error 字符串
                        let body: Option<MultiDeriveError> = err.body_as();
                        this.error = Some(match body.as_ref().and_then(|b| b.member.as_ref()) {
                            Some(m) => {
                                let mut s = t!(
                                    "multi.memberFailed",
                                    repo = member_basename(&m.dir),
                                    reason = worktree_reason_text(&Tr, Some(m.reason))
                                )
                                .to_string();
                                if let Some(d) = m.detail.as_deref().filter(|d| !d.is_empty()) {
                                    s.push_str(" · ");
                                    s.push_str(d);
                                }
                                s
                            }
                            None => err.message.clone(),
                        });
                        // 回滚没删干净：路径原样告诉用户（警告类通知不自动消失）
                        if let Some(left) = body.and_then(|b| b.leftover).filter(|l| !l.is_empty()) {
                            this.ws.update(cx, |w, cx| {
                                w.toast(ToastKind::Warning, t!("worktree.leftoverTitle").to_string(), Some(left.join("\n")), cx)
                            });
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for MultiForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let mut d = div().flex().flex_col().gap_3();
        let Some(probe) = self.probe.clone() else {
            return match self.load_error.clone() {
                Some(e) => d.child(error_line(e, cx)),
                None => d.child(note(t!("worktree.probing").to_string(), cx)),
            };
        };
        let branch = self.branch(cx);
        let evals = self.evals(cx);
        d = d.child(note(t!("multi.intro", n = probe.members.len()).to_string(), cx));

        let mut members = div()
            .id("mwt-members")
            .max_h(zpx(192.))
            .overflow_y_scroll()
            .rounded(radius::MD)
            .bg(ui.app)
            .p_2()
            .flex()
            .flex_col()
            .gap_1();
        for (dir, info, action) in &evals {
            let (text, err) = self.member_line(info, action);
            members = members.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_size(zpx(13.))
                    .child(div().flex_none().child(member_basename(info.repo_dir.as_deref().unwrap_or(dir)).to_string()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if err { ui.destructive } else { ui.muted_foreground })
                            .child(text),
                    ),
            );
        }
        d = d.child(members);

        let seg = |mode: MultiWorktreeMode, label: String, cx: &mut Context<Self>| SegItem {
            label,
            on: self.mode == mode,
            on_click: Box::new(cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx))),
        };
        let items = vec![
            seg(MultiWorktreeMode::Auto, t!("multi.modeAuto").to_string(), cx),
            seg(MultiWorktreeMode::NewBranch, t!("worktree.modeNew").to_string(), cx),
            seg(MultiWorktreeMode::ExistingBranch, t!("worktree.modeExisting").to_string(), cx),
        ];
        d = d.child(segmented("mwt-mode", items, cx));

        if self.mode == MultiWorktreeMode::ExistingBranch {
            let infos: Vec<&RepoInfo> = probe.members.iter().map(|m| &m.info).collect();
            let hint = common_local_branches(&infos)
                .is_empty()
                .then(|| (t!("multi.noCommonBranch").to_string(), Tone::Muted));
            d = d.child(field(Some(t!("worktree.pickBranch").to_string()), Select::new(&self.pick_select).w_full(), hint, cx));
        } else {
            d = d.child(field(Some(t!("worktree.branch").to_string()), Input::new(&self.branch_text), None, cx));
        }

        let central = preview_multi_dir(probe.base_dir.as_deref().unwrap_or(""), &self.project.name, &branch);
        if !central.is_empty() {
            let mut preview = div()
                .rounded(radius::MD)
                .bg(ui.app)
                .px_3()
                .py_2()
                .text_xs()
                .flex()
                .flex_col()
                .child(div().truncate().child(central));
            for (dir, info, _) in &evals {
                preview = preview.child(
                    div()
                        .pl_4()
                        .truncate()
                        .text_color(ui.muted_foreground)
                        .child(format!("{}/", member_basename(info.repo_dir.as_deref().unwrap_or(dir)))),
                );
            }
            d = d.child(field(
                Some(t!("multi.dirPreview").to_string()),
                preview,
                Some((t!("multi.dirPreviewHint").to_string(), Tone::Muted)),
                cx,
            ));
        }

        d = d.child(field(Some(t!("worktree.name").to_string()), Input::new(&self.name), None, cx));
        let blocked = self.blocked_count(cx);
        if blocked > 0 && !branch.is_empty() {
            d = d.child(error_line(t!("multi.blockedBy", n = blocked).to_string(), cx));
        }
        if let Some(e) = self.error.clone() {
            d = d.child(error_line(e, cx));
        }
        let can = !branch.is_empty() && blocked == 0;
        d.child(footer(self.busy, can, cx.listener(|this, _, window, cx| this.submit(window, cx))))
    }
}
