//! 新建 / 编辑项目表单（旧 React 版的 `components/ProjectForm.tsx`）。
//!
//! 环境对应宿主机上的一个或多个工作目录：选单个目录时可在创建时选 Git 基础分支，选多个目录时
//! 通过虚拟项目目录聚合。旧项目仍沿用各自的填法。
//! 服务端的 type 只有 local / ssh；编辑时跟 existing 走，PUT 不许改类型。
//!
//! - SSH 从已保存主机里选（可就地「添加主机」→ 主机表单，存完回调把新主机选进来）；存量项目
//!   没有 hostId，可以继续手写 SSH 字段，也可以改绑到已保存主机。
//! - 工作目录旁的「浏览…」/「添加文件夹…」打开 [`FolderPicker`]：列的是**宿主机**上的目录（本地项目是后端
//!   所在机器，SSH 项目是远端），不是这台 Mac 的系统文件夹选择器。选择器嵌在同一个对话框里
//!   （沿用 React 版，不叠第二层），Esc 只退回表单。
//! - 从主机菜单或新建线程叠开环境表单时直接打开多选目录选择器。环境选好目录后回到表单，单个目录可
//!   继续选基础分支、多个目录可直接继续增删；旧项目保留选好目录就创建的快捷路径。
//! - 不提供环境上下文与 shell 覆盖：多文件夹的说明由用户写进成员目录的 AGENTS.md，shell 用宿主机
//!   探测到的登录 shell。

pub(crate) mod kit;

use std::time::Duration;

use falcon_client::ProbeTarget;
use falcon_proto::{Project, ProjectInput, ProjectType, SshConfigInput, SshHost};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, Task, Window, div};
use rust_i18n::t;

use self::kit::{Opt, OptState, Options, SegItem, Tone, error_line, field, segmented};
use crate::dialogs::folder_picker::FolderPicker;
use crate::dialogs::host_form::{self, SshFields, SshInit};
use crate::workspace::Workspace;
use crate::zoom::zpx;

/// 打开表单时的预填（React 版的 ProjectFormPreset）
#[derive(Clone, Debug, Default)]
pub struct Preset {
    /// "local" / "ssh"；None = 让用户选
    pub kind: Option<&'static str>,
    pub host_id: Option<String>,
    pub environment: bool,
    pub initial_dirs: Vec<String>,
}

impl Preset {
    pub fn local(environment: bool) -> Self {
        Self { kind: Some("local"), environment, ..Self::default() }
    }
    pub fn ssh(host_id: Option<String>, environment: bool) -> Self {
        Self { kind: Some("ssh"), host_id, environment, ..Self::default() }
    }
}

/// 新建成功后的回调。新建线程的表单叠开环境表单时靠它把刚建好的环境选进下拉框
/// （同 `host_form::OnSaved`）
pub type OnCreated = std::rc::Rc<dyn Fn(&Project, &mut Window, &mut App)>;

/// 多仓库档「位置」下拉的本机哨兵（沿用 React 版的名字：它的 Radix Select 不接受空字符串当 value）
const MULTI_LOCAL: &str = "__local__";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKind {
    Local,
    Ssh,
    Multi,
    Environment,
}

enum BranchProbe {
    Idle,
    Loading,
    PlainDirectory,
    Git,
    Failed(String),
}

pub struct ProjectForm {
    ws: Entity<Workspace>,
    existing: Option<Project>,
    kind: FormKind,
    /// 从主机菜单进来时位置已定，不显示主机选择。
    locked: bool,
    name: Entity<InputState>,
    working_dir: Entity<InputState>,
    /// 多仓库档的成员清单（成员路径原文，可手改可从选择器追加）
    repos: Vec<Entity<InputState>>,
    /// 派生产物的成员由派生决定：只读展示，提交不带 repos（PUT 会拒）
    repos_readonly: bool,
    default_wt: Entity<InputState>,
    base_branch: Entity<SelectState<SearchableVec<Opt>>>,
    branch_probe: BranchProbe,
    branch_probe_id: u64,
    /// 上次发起分支探测的 `(host_id, path)`，避免编辑其它字段或空行时重复探测
    branch_probe_key: Option<(String, String)>,
    _branch_task: Option<Task<()>>,
    host_id: String,
    host_select: Entity<OptState>,
    location_select: Entity<OptState>,
    /// 存量项目手写的 SSH 字段
    legacy_ssh: SshFields,
    path_hint: Option<(bool, String)>,
    error: Option<String>,
    busy: bool,
    picking: bool,
    picking_repo: bool,
    picker: Option<Entity<FolderPicker>>,
    hosts_seen: Vec<(String, String)>,
    /// 只在新建时调用，编辑不调
    on_created: Option<OnCreated>,
}

fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

fn folder_name(dir: &str) -> String {
    dir.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or_default().to_string()
}

fn parent_of(p: &str) -> String {
    let s = p.trim_end_matches(['\\', '/']);
    match s.rfind(['/', '\\']) {
        Some(i) if i > 0 => s[..i].to_string(),
        _ => s.to_string(),
    }
}

fn host_options(hosts: &[SshHost]) -> Vec<Opt> {
    hosts
        .iter()
        .map(|h| Opt::new(h.id.clone(), h.name.clone()).detail(falcon_core::host_color::ssh_conn(h)))
        .collect()
}

impl ProjectForm {
    fn new(ws: Entity<Workspace>, existing: Option<Project>, preset: Preset, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = match &existing {
            Some(p) if p.is_environment() => FormKind::Environment,
            Some(p) if p.multi.is_some() => FormKind::Multi,
            Some(p) if p.project_type == ProjectType::Ssh => FormKind::Ssh,
            Some(_) => FormKind::Local,
            None if preset.environment => FormKind::Environment,
            None if preset.kind == Some("ssh") => FormKind::Ssh,
            None if preset.kind == Some("local") => FormKind::Local,
            None => FormKind::Environment,
        };
        let locked = existing.is_none()
            && (preset.kind == Some("local") || preset.host_id.is_some());
        let text = |value: String, placeholder: Option<String>, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let s = InputState::new(window, cx).default_value(value);
                match placeholder {
                    Some(p) => s.placeholder(p),
                    None => s,
                }
            })
        };
        let name = text(existing.as_ref().map(|p| p.name.clone()).unwrap_or_default(), None, window, cx);
        let placeholder = match kind {
            FormKind::Local => Some("D:\\code\\my-project".to_string()),
            FormKind::Ssh => Some("/home/user/project".to_string()),
            FormKind::Multi | FormKind::Environment => None,
        };
        let working_dir = text(
            existing
                .as_ref()
                .and_then(|p| p.working_dir.clone())
                .or_else(|| (kind != FormKind::Environment).then(|| preset.initial_dirs.first().cloned()).flatten())
                .unwrap_or_default(),
            placeholder,
            window,
            cx,
        );
        let initial_env_dirs: Vec<String> = if kind == FormKind::Environment {
            if let Some(p) = &existing {
                if let Some(m) = &p.multi {
                    m.repos.iter().map(|r| r.dir.clone()).collect()
                } else {
                    p.working_dir.clone().into_iter().collect()
                }
            } else if !preset.initial_dirs.is_empty() {
                preset.initial_dirs.clone()
            } else if !locked {
                vec![String::new()]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let repos: Vec<Entity<InputState>> = if kind == FormKind::Environment {
            initial_env_dirs.into_iter().map(|d| Self::make_env_dir_input(d, window, cx)).collect()
        } else {
            existing
                .as_ref()
                .and_then(|p| p.multi.as_ref())
                .map(|m| m.repos.iter().map(|r| text(r.dir.clone(), None, window, cx)).collect())
                .unwrap_or_default()
        };
        let repos_readonly = existing
            .as_ref()
            .is_some_and(|p| p.multi.is_some() && (p.worktree.is_some() || p.thread_environment_id.is_some()));
        let default_wt = text(
            existing.as_ref().and_then(|p| p.default_worktree_branch.clone()).unwrap_or_default(),
            Some(t!("project.defaultWorktreeBranchPlaceholder").to_string()),
            window,
            cx,
        );
        let host_id = existing
            .as_ref()
            .and_then(|p| p.host_id.clone())
            .or(preset.host_id.clone())
            .unwrap_or_default();
        let hosts = ws.read(cx).hosts.clone();
        let host_select = cx.new(|cx| kit::new_select(Options::flat(host_options(&hosts)), &host_id, window, cx));
        let location_select = cx.new(|cx| {
            let mut opts = vec![Opt::new(MULTI_LOCAL, t!("multi.locationLocal").to_string())];
            opts.extend(host_options(&hosts));
            let value = if host_id.is_empty() { MULTI_LOCAL } else { host_id.as_str() };
            kit::new_select(Options::flat(opts), value, window, cx)
        });
        let base_branch = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::<Opt>::new()), None, window, cx).searchable(true)
        });
        let legacy_init = existing
            .as_ref()
            .and_then(|p| p.ssh.as_ref())
            .map(|s| SshInit {
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                auth_method: s.auth_method,
                key_path: s.key_path.clone().unwrap_or_default(),
            })
            .unwrap_or_default();
        let has_secret = existing.as_ref().and_then(|p| p.ssh.as_ref()).is_some_and(|s| s.has_secret);
        let legacy_ssh = SshFields::new(legacy_init, has_secret, window, cx);
        legacy_ssh.on_change(window, cx, |_: &mut Self, cx| cx.notify());

        // 名字空着时提交按钮是灰的：跟着输入重画
        cx.subscribe_in(&name, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(&working_dir, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                this.path_hint = None;
                cx.notify();
            }
            // 只有本地项目的路径在失焦时去后端校验一次（远端路径校验不了）
            InputEvent::Blur if this.kind == FormKind::Local => this.validate_path(window, cx),
            _ => {}
        })
        .detach();
        cx.subscribe_in(&host_select, window, |this, _, ev: &SelectEvent<Options>, window, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else { return };
            this.set_host(v.clone(), window, cx);
        })
        .detach();
        cx.subscribe_in(&location_select, window, |this, _, ev: &SelectEvent<Options>, window, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else { return };
            let id = if v == MULTI_LOCAL { String::new() } else { v.clone() };
            this.set_host(id, window, cx);
        })
        .detach();
        // 主机列表变了（从这里新增了一台、别处删了一台）就重建两个下拉
        cx.observe_in(&ws, window, |this, ws, window, cx| {
            let seen: Vec<(String, String)> = ws.read(cx).hosts.iter().map(|h| (h.id.clone(), h.name.clone())).collect();
            if seen != this.hosts_seen {
                let had_host = this.hosts_seen.iter().any(|(id, _)| id == &this.host_id);
                let has_host = seen.iter().any(|(id, _)| id == &this.host_id);
                this.hosts_seen = seen;
                this.sync_host_selects(window, cx);
                if had_host != has_host {
                    this.reprobe_branches(true, window, cx);
                }
            }
            cx.notify();
        })
        .detach();

        let hosts_seen = hosts.iter().map(|h| (h.id.clone(), h.name.clone())).collect();
        let mut this = Self {
            ws,
            existing,
            kind,
            locked,
            name,
            working_dir,
            repos,
            repos_readonly,
            default_wt,
            base_branch,
            branch_probe: BranchProbe::Idle,
            branch_probe_id: 0,
            branch_probe_key: None,
            _branch_task: None,
            host_id,
            host_select,
            location_select,
            legacy_ssh,
            path_hint: None,
            error: None,
            busy: false,
            picking: false,
            picking_repo: false,
            picker: None,
            hosts_seen,
            on_created: None,
        };
        this.reprobe_branches(true, window, cx);
        if locked {
            if this.kind == FormKind::Environment && this.env_dirs(cx).is_empty() {
                this.open_env_picker(window, cx);
            } else if this.kind != FormKind::Environment && this.working_dir.read(cx).value().is_empty() {
                this.open_picker(window, cx);
            }
        }
        this
    }

    fn make_env_dir_input(value: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Change) {
                this.reprobe_branches(false, window, cx);
                cx.notify();
            }
        })
        .detach();
        input
    }

    fn env_dirs(&self, cx: &App) -> Vec<String> {
        self.repos
            .iter()
            .map(|r| r.read(cx).value().trim().to_string())
            .filter(|r| !r.is_empty())
            .collect()
    }

    fn env_has_threads(&self, cx: &App) -> bool {
        let Some(env) = self.existing.as_ref().filter(|p| p.is_environment()) else {
            return false;
        };
        self.ws.read(cx).projects.iter().any(|p| {
            p.thread_environment_id.as_deref() == Some(env.id.as_str())
                || p.worktree.as_ref().is_some_and(|w| w.source_project_id == env.id)
        })
    }

    /// 编辑环境时：单目录环境的目录不可修改；多目录环境在已有线程后不可修改，无线程时允许增删
    fn env_dirs_readonly(&self, cx: &App) -> bool {
        let Some(env) = self.existing.as_ref() else {
            return false;
        };
        env.multi.is_none() || self.env_has_threads(cx)
    }

    fn ensure_env_dir_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == FormKind::Environment && self.repos.is_empty() && !self.env_dirs_readonly(cx) {
            self.repos.push(Self::make_env_dir_input(String::new(), window, cx));
        }
    }

    // ---------------- 派生出来的判断（React 版里那几个 const） ----------------

    fn project_type(&self) -> ProjectType {
        match self.kind {
            FormKind::Local => ProjectType::Local,
            FormKind::Ssh => ProjectType::Ssh,
            FormKind::Multi | FormKind::Environment => match &self.existing {
                Some(p) => p.project_type,
                None if !self.host_id.is_empty() => ProjectType::Ssh,
                None => ProjectType::Local,
            },
        }
    }

    /// 存量项目没有 hostId：可以继续手写 ssh，也可以改绑到已保存主机
    fn legacy(&self) -> bool {
        self.existing.as_ref().is_some_and(|p| p.host_id.is_none())
    }

    fn using_saved_host(&self) -> bool {
        self.project_type() == ProjectType::Ssh && (!self.host_id.is_empty() || !self.legacy())
    }

    fn can_browse_remote(&self) -> bool {
        !self.host_id.is_empty() || self.existing.as_ref().is_some_and(|p| p.project_type == ProjectType::Ssh)
    }

    fn any_picking(&self) -> bool {
        self.picking || self.picking_repo
    }

    fn environment(&self) -> bool {
        self.kind == FormKind::Environment || self.existing.as_ref().is_some_and(Project::is_environment)
    }

    fn title(&self) -> String {
        if self.any_picking() {
            if self.project_type() == ProjectType::Ssh {
                t!("project.pickRemoteTitle").to_string()
            } else {
                t!("project.pickTitle").to_string()
            }
        } else if self.environment() {
            if self.existing.is_some() { t!("env.edit").to_string() } else { t!("env.create").to_string() }
        } else if self.existing.is_some() {
            t!("project.editTitle").to_string()
        } else {
            t!("project.createTitle").to_string()
        }
    }

    /// 目录浏览与分支探测的目标：编辑已有 SSH 项目问它的远端，选了主机问那台主机
    fn probe_target(&self) -> ProbeTarget {
        match &self.existing {
            Some(p) if p.project_type == ProjectType::Ssh => ProbeTarget::Project(p.id.clone()),
            _ if !self.host_id.is_empty() => ProbeTarget::SshHost(self.host_id.clone()),
            _ => ProbeTarget::Backend,
        }
    }

    // ---------------- 状态变更 ----------------

    fn set_kind(&mut self, kind: FormKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.path_hint = None;
        let placeholder = match kind {
            FormKind::Local => "D:\\code\\my-project",
            FormKind::Ssh => "/home/user/project",
            FormKind::Multi | FormKind::Environment => "",
        };
        self.working_dir.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
        cx.notify();
    }

    fn set_host(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.host_id == id {
            return;
        }
        self.host_id = id;
        self.path_hint = None;
        self.sync_host_selects(window, cx);
        self.reprobe_branches(true, window, cx);
        cx.notify();
    }

    fn has_environment_host(&self) -> bool {
        self.host_id.is_empty() || self.hosts_seen.iter().any(|(id, _)| id == &self.host_id)
    }

    fn reprobe_branches(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != FormKind::Environment || self.existing.is_some() {
            return;
        }
        let dirs = self.env_dirs(cx);
        if dirs.len() != 1 || !self.has_environment_host() {
            self._branch_task = None;
            self.branch_probe_id += 1;
            self.branch_probe_key = None;
            self.base_branch.update(cx, |select, cx| {
                select.set_items(SearchableVec::new(Vec::new()), window, cx);
            });
            self.branch_probe = BranchProbe::Idle;
            cx.notify();
            return;
        }
        let path = dirs[0].clone();
        let key = (self.host_id.clone(), path.clone());
        if !force && self.branch_probe_key.as_ref() == Some(&key) {
            return;
        }
        self._branch_task = None;
        self.branch_probe_id += 1;
        let id = self.branch_probe_id;
        self.branch_probe_key = Some(key);
        self.base_branch.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(Vec::new()), window, cx);
        });
        self.branch_probe = BranchProbe::Loading;
        let client = self.ws.read(cx).client.clone();
        let target = self.probe_target();
        self._branch_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let result = client.environment_branches(&target, &path).await;
            this.update_in(cx, |this, window, cx| {
                // 路径和宿主机可能在请求过程中切换，旧结果不能恢复之前的分支选择。
                if this.branch_probe_id != id {
                    return;
                }
                this.branch_probe = match result {
                    Ok(Some(branches)) => {
                        let mut options = vec![Opt::new("", t!("env.baseDefault").to_string())];
                        options.extend(branches.into_iter().map(|branch| Opt::new(branch.name.clone(), branch.name)));
                        this.base_branch.update(cx, |select, cx| {
                            select.set_items(SearchableVec::new(options), window, cx);
                            select.set_selected_value(&String::new(), window, cx);
                        });
                        BranchProbe::Git
                    }
                    Ok(None) => BranchProbe::PlainDirectory,
                    Err(err) => BranchProbe::Failed(err.message),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn sync_host_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hosts = self.ws.read(cx).hosts.clone();
        kit::reset_select(&self.host_select, Options::flat(host_options(&hosts)), &self.host_id, window, cx);
        let mut opts = vec![Opt::new(MULTI_LOCAL, t!("multi.locationLocal").to_string())];
        opts.extend(host_options(&hosts));
        let value = if self.host_id.is_empty() { MULTI_LOCAL.to_string() } else { self.host_id.clone() };
        kit::reset_select(&self.location_select, Options::flat(opts), &value, window, cx);
    }

    fn validate_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.working_dir.read(cx).value().to_string();
        if dir.trim().is_empty() {
            self.path_hint = None;
            cx.notify();
            return;
        }
        let request = self.ws.read(cx).client.validate_path(&dir);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                // 结果回来前又改过路径：这个结论已经过时
                if this.working_dir.read(cx).value().as_ref() != dir {
                    return;
                }
                this.path_hint = Some(match result {
                    Ok(r) if r.ok => (true, t!("project.pathOk").to_string()),
                    Ok(r) => (false, r.error.unwrap_or_else(|| "?".into())),
                    Err(err) => (false, err.message.clone()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---------------- 目录选择器 ----------------

    fn make_picker(&mut self, initial: String, for_repo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let (this_sel, this_close) = (this.clone(), this);
        let on_select: crate::dialogs::folder_picker::OnSelect = std::rc::Rc::new(move |dirs, window, cx| {
            this_sel
                .update(cx, |f, cx| {
                    if for_repo {
                        f.pick_repo_folders(dirs, window, cx)
                    } else if let Some(dir) = dirs.into_iter().next() {
                        f.pick_folder(dir, window, cx)
                    }
                })
                .ok();
        });
        let on_close: crate::dialogs::folder_picker::OnClose = std::rc::Rc::new(move |window, cx| {
            this_close
                .update(cx, |f, cx| {
                    if f.close_picker(window, cx) {
                        window.close_dialog(cx);
                    }
                })
                .ok();
        });
        let client = self.ws.read(cx).client.clone();
        let target = self.probe_target();
        let remote = self.project_type() == ProjectType::Ssh;
        let picker = cx.new(|cx| FolderPicker::new(client, target, remote, for_repo, &initial, on_select, on_close, window, cx));
        picker.update(cx, |p, cx| p.focus(window, cx));
        self.picker = Some(picker);
    }

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_type() == ProjectType::Ssh && !self.can_browse_remote() && !self.locked {
            return;
        }
        let initial = self.working_dir.read(cx).value().to_string();
        self.picking = true;
        self.make_picker(initial, false, window, cx);
        cx.notify();
    }

    fn open_repo_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 连续添加同级仓库是常态：选择器从上一个成员的父目录开起
        let last = self
            .repos
            .iter()
            .map(|r| r.read(cx).value().trim().to_string())
            .filter(|r| !r.is_empty())
            .last();
        let initial = last.map(|r| parent_of(&r)).unwrap_or_default();
        self.picking_repo = true;
        self.make_picker(initial, true, window, cx);
        cx.notify();
    }

    fn open_env_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_type() == ProjectType::Ssh && !self.can_browse_remote() && !self.locked {
            return;
        }
        self.open_repo_picker(window, cx);
    }

    fn pick_folder(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let base = folder_name(&dir);
        let typed = self.name.read(cx).value().trim().to_string();
        let next_name = if typed.is_empty() { base.clone() } else { typed.clone() };
        self.working_dir.update(cx, |i, cx| i.set_value(dir.clone(), window, cx));
        self.path_hint = Some((true, t!("project.pathOk").to_string()));
        if self.existing.is_none() && typed.is_empty() && !base.is_empty() {
            self.name.update(cx, |i, cx| i.set_value(base.clone(), window, cx));
        }
        // 从服务器新建：选完目录就创建，不再问本机还是 SSH
        if self.locked && !self.environment() && !next_name.is_empty() {
            self.save(next_name, dir, window, cx);
            return;
        }
        self.leave_picker(window, cx);
    }

    /// 多仓库档 / 环境表单「添加文件夹」：一次追加全部选中目录，不关外层表单；重复选中静默去重
    fn pick_repo_folders(&mut self, dirs: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.kind == FormKind::Environment {
            self.repos.retain(|r| !r.read(cx).value().trim().is_empty());
            for dir in dirs {
                let dir = dir.trim().to_string();
                if !dir.is_empty() && !self.repos.iter().any(|r| r.read(cx).value().trim() == dir) {
                    self.repos.push(Self::make_env_dir_input(dir, window, cx));
                }
            }
            let selected = self.env_dirs(cx);
            if self.existing.is_none() && self.name.read(cx).value().trim().is_empty() && selected.len() == 1 {
                let base = folder_name(&selected[0]);
                if !base.is_empty() {
                    self.name.update(cx, |i, cx| i.set_value(base, window, cx));
                }
            }
            self.reprobe_branches(false, window, cx);
            self.leave_picker(window, cx);
            return;
        }
        for dir in dirs {
            if !self.repos.iter().any(|r| r.read(cx).value().as_ref() == dir) {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(dir));
                self.repos.push(input);
            }
        }
        self.leave_picker(window, cx);
    }

    /// 选择器的取消 / Esc。返回 true = 反悔了，整个对话框一起关
    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        if !self.picking_repo && self.locked && !self.environment() && self.working_dir.read(cx).value().is_empty() {
            return true;
        }
        self.leave_picker(window, cx);
        false
    }

    fn leave_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picking = false;
        self.picking_repo = false;
        self.picker = None;
        self.ensure_env_dir_row(window, cx);
        self.name.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    // ---------------- 多仓库成员 ----------------

    fn move_repo(&mut self, i: usize, up: bool, cx: &mut Context<Self>) {
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        if let Some(j) = j.filter(|j| *j < self.repos.len()) {
            self.repos.swap(i, j);
            cx.notify();
        }
    }

    fn repo_values(&self, cx: &App) -> Vec<String> {
        self.repos.iter().map(|r| r.read(cx).value().to_string()).collect()
    }

    // ---------------- 提交 ----------------

    fn can_submit(&self, cx: &App) -> bool {
        if self.busy || self.name.read(cx).value().trim().is_empty() {
            return false;
        }
        if self.project_type() == ProjectType::Ssh && self.using_saved_host() && self.host_id.is_empty() {
            return false;
        }
        if self.kind == FormKind::Environment {
            let dirs = self.env_dirs(cx);
            if self.existing.is_some() {
                return self.env_dirs_readonly(cx) || !dirs.is_empty();
            }
            if !self.has_environment_host() || dirs.is_empty() {
                return false;
            }
            return dirs.len() > 1
                || matches!(self.branch_probe, BranchProbe::Git | BranchProbe::PlainDirectory | BranchProbe::Failed(_));
        }
        true
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.any_picking() || !self.can_submit(cx) {
            return;
        }
        let name = self.name.read(cx).value().to_string();
        let dir = self.working_dir.read(cx).value().to_string();
        self.save(name, dir, window, cx);
    }

    fn fail(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.error = Some(message);
        // 错误写在表单上：选择器开着就收起来，不然看不见（照 React 版：请求失败时退回表单）
        if self.any_picking() {
            self.leave_picker(window, cx);
        }
        cx.notify();
    }

    fn save(&mut self, name: String, working_dir: String, window: &mut Window, cx: &mut Context<Self>) {
        let ty = self.project_type();
        if ty == ProjectType::Ssh && self.using_saved_host() && self.host_id.is_empty() {
            self.fail(t!("host.required").to_string(), window, cx);
            return;
        }
        let clean_repos: Vec<String> = self
            .repo_values(cx)
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        let multi = self.kind == FormKind::Multi && !self.repos_readonly;
        if multi && clean_repos.is_empty() {
            self.fail(t!("multi.needRepo").to_string(), window, cx);
            return;
        }
        let (env_working_dir, env_repos, env_base_branch) = if self.kind == FormKind::Environment {
            match &self.existing {
                None => {
                    if clean_repos.is_empty() {
                        self.fail(t!("multi.needRepo").to_string(), window, cx);
                        return;
                    }
                    if clean_repos.len() == 1 {
                        let branch = self
                            .base_branch
                            .read(cx)
                            .selected_value()
                            .filter(|branch| !branch.is_empty())
                            .cloned();
                        (Some(clean_repos[0].clone()), None, branch)
                    } else {
                        (None, Some(clean_repos.clone()), None)
                    }
                }
                Some(existing) => {
                    let send_repos = existing.multi.is_some() && !self.env_dirs_readonly(cx);
                    if send_repos && clean_repos.is_empty() {
                        self.fail(t!("multi.needRepo").to_string(), window, cx);
                        return;
                    }
                    (existing.working_dir.clone(), send_repos.then_some(clean_repos.clone()), None)
                }
            }
        } else {
            (None, None, None)
        };
        let ssh = (ty == ProjectType::Ssh && self.host_id.is_empty()).then(|| {
            let v = self.legacy_ssh.values(cx);
            SshConfigInput {
                host: v.host,
                port: v.port,
                username: v.username,
                auth_method: v.auth_method,
                key_path: (v.auth_method == falcon_proto::SshAuthMethod::Key).then_some(v.key_path),
                secret: nonempty(&v.secret),
            }
        });
        let input = ProjectInput {
            name,
            project_type: ty,
            working_dir: if self.kind == FormKind::Environment { env_working_dir } else { nonempty(&working_dir) },
            // 表单不再提供 shell 覆盖：新建项目用宿主机的登录 shell；编辑已有项目时原样带回，
            // 避免把旧项目已保存的自定义 shell 悄悄清掉
            shell: self.existing.as_ref().and_then(|p| p.shell.clone()),
            // 环境上下文输入已移除；空串只是「这是环境」的判别标志，服务端据此选择环境接口
            environment_context: self.environment().then(String::new),
            // 派生产物与线程的成员不可改，不上送（PUT 会拒）；容器每次全量替换
            repos: if self.kind == FormKind::Environment { env_repos } else { multi.then_some(clean_repos) },
            host_id: (ty == ProjectType::Ssh && !self.host_id.is_empty()).then(|| self.host_id.clone()),
            ssh,
            default_worktree_branch: if self.kind == FormKind::Environment {
                env_base_branch
            } else {
                nonempty(self.default_wt.read(cx).value().trim())
            },
        };
        self.busy = true;
        self.error = None;
        if let Some(p) = &self.picker {
            p.update(cx, |p, cx| p.set_confirming(true, cx));
        }
        let client = self.ws.read(cx).client.clone();
        let edit_id = self.existing.as_ref().map(|p| p.id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = match &edit_id {
                Some(id) => client.update_project(id, &input).await,
                None => client.create_project(&input).await,
            };
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if let Some(p) = &this.picker {
                    p.update(cx, |p, cx| p.set_confirming(false, cx));
                }
                match result {
                    Ok(project) => {
                        let created = edit_id.is_none();
                        this.ws.update(cx, |w, cx| {
                            if created {
                                // 新建的项目要立刻选中：列表刷新是异步的，先把它放进去，
                                // 不然 select_project 找不到它会直接忽略
                                if !w.projects.iter().any(|p| p.id == project.id) {
                                    w.projects.push(project.clone());
                                }
                                if project.environment_context.is_none() {
                                    w.select_project(&project.id, cx);
                                } else {
                                    let machine = project.host_id.as_ref().map(|id| format!("s:host:{id}")).unwrap_or_else(|| "s:local".into());
                                    w.state.collapsed.remove(&machine);
                                }
                            }
                            w.refresh_inventory(cx);
                        });
                        // 先关自己：叠在下面的表单拿回焦点后再收到新建的项目
                        window.close_dialog(cx);
                        if created && let Some(on_created) = this.on_created.clone() {
                            on_created(&project, window, cx);
                        }
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.message.clone());
                        if this.picking {
                            this.leave_picker(window, cx);
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

    /// 对话框的 Esc：选择器开着就只退回表单；返回 true 才让对话框自己关
    fn on_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.any_picking() {
            return self.close_picker(window, cx);
        }
        true
    }

    // ---------------- 渲染 ----------------

    fn on_host_saved(&self, cx: &mut Context<Self>) -> host_form::OnSaved {
        let form = cx.entity().downgrade();
        std::rc::Rc::new(move |h: &SshHost, window, cx| {
            let id = h.id.clone();
            form.update(cx, |f, cx| f.set_host(id, window, cx)).ok();
        })
    }

    fn render_add_host(&self, cx: &mut Context<Self>) -> Button {
        Button::new("pf-add-host")
            .outline()
            .label(t!("host.add").to_string())
            .on_click(cx.listener(|this, _, window, cx| {
                host_form::open(&this.ws, None, Some(this.on_host_saved(cx)), window, cx);
            }))
    }

    fn render_repos(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let mut list = div().flex().flex_col().gap(zpx(6.));
        let n = self.repos.len();
        for (i, repo) in self.repos.iter().enumerate() {
            let mut row = div().flex().items_center().gap_1().child(div().flex_1().min_w_0().child(Input::new(repo).readonly(self.repos_readonly)));
            if !self.repos_readonly {
                row = row
                    .child(
                        Button::new(SharedString::from(format!("pf-repo-up-{i}")))
                            .ghost()
                            .icon(IconName::ArrowUp)
                            .tooltip(t!("multi.moveUp").to_string())
                            .disabled(i == 0)
                            .on_click(cx.listener(move |this, _, _, cx| this.move_repo(i, true, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("pf-repo-down-{i}")))
                            .ghost()
                            .icon(IconName::ArrowDown)
                            .tooltip(t!("multi.moveDown").to_string())
                            .disabled(i + 1 == n)
                            .on_click(cx.listener(move |this, _, _, cx| this.move_repo(i, false, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("pf-repo-rm-{i}")))
                            .ghost()
                            .icon(IconName::X)
                            .tooltip(t!("multi.removeRepo").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if i < this.repos.len() {
                                    this.repos.remove(i);
                                    cx.notify();
                                }
                            })),
                    );
            }
            list = list.child(row);
        }
        if !self.repos_readonly {
            let disabled = self.project_type() == ProjectType::Ssh && !self.can_browse_remote();
            list = list.child(
                Button::new("pf-add-repo")
                    .outline()
                    .label(t!("multi.addRepo").to_string())
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.open_repo_picker(window, cx))),
            );
        }
        list
    }

    fn render_env_dirs(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let readonly = self.env_dirs_readonly(cx);
        let disabled_browse = self.project_type() == ProjectType::Ssh && !self.can_browse_remote();
        let mut wrap = div().flex().flex_col().gap(zpx(6.));
        let n = self.repos.len();
        if !readonly && n == 1 && self.repos[0].read(cx).value().trim().is_empty() {
            return wrap.child(
                div()
                    .flex()
                    .gap(zpx(10.))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.repos[0])))
                    .child(
                        Button::new("pf-env-browse")
                            .outline()
                            .label(t!("project.browse").to_string())
                            .disabled(disabled_browse)
                            .on_click(cx.listener(|this, _, window, cx| this.open_env_picker(window, cx))),
                    ),
            );
        }
        let mut rows = div().flex().flex_col().gap(zpx(6.));
        for (i, repo) in self.repos.iter().enumerate() {
            let mut row = div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_1().min_w_0().child(Input::new(repo).readonly(readonly)));
            if !readonly {
                if n > 1 {
                    row = row
                        .child(
                            Button::new(SharedString::from(format!("pf-env-up-{i}")))
                                .ghost()
                                .icon(IconName::ArrowUp)
                                .tooltip(t!("multi.moveUp").to_string())
                                .disabled(i == 0)
                                .on_click(cx.listener(move |this, _, _, cx| this.move_repo(i, true, cx))),
                        )
                        .child(
                            Button::new(SharedString::from(format!("pf-env-down-{i}")))
                                .ghost()
                                .icon(IconName::ArrowDown)
                                .tooltip(t!("multi.moveDown").to_string())
                                .disabled(i + 1 == n)
                                .on_click(cx.listener(move |this, _, _, cx| this.move_repo(i, false, cx))),
                        );
                }
                row = row.child(
                    Button::new(SharedString::from(format!("pf-env-rm-{i}")))
                        .ghost()
                        .icon(IconName::X)
                        .tooltip(t!("env.removeDirectory").to_string())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if i < this.repos.len() {
                                this.repos.remove(i);
                                this.ensure_env_dir_row(window, cx);
                                this.reprobe_branches(false, window, cx);
                                cx.notify();
                            }
                        })),
                );
            }
            rows = rows.child(row);
        }
        if n > 4 {
            wrap = wrap.child(div().id("pf-env-dirs").max_h(zpx(180.)).overflow_y_scroll().p(zpx(1.)).child(rows));
        } else {
            wrap = wrap.child(rows);
        }
        if !readonly {
            wrap = wrap.child(
                Button::new("pf-env-add-dir")
                    .outline()
                    .label(t!("env.addDirectory").to_string())
                    .disabled(disabled_browse)
                    .on_click(cx.listener(|this, _, window, cx| this.open_env_picker(window, cx))),
            );
        }
        wrap
    }

    fn render_form(&mut self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let ty = self.project_type();
        let hosts = self.ws.read(cx).hosts.clone();
        let selected_host = hosts.iter().find(|h| h.id == self.host_id).cloned();
        let conn = selected_host.as_ref().map(falcon_core::host_color::ssh_conn);
        let durable_thread = self.existing.as_ref().is_some_and(|p| p.thread_environment_id.is_some());
        let mut form = div().flex().flex_col().gap_3();

        if self.existing.is_none() && !self.locked && self.kind != FormKind::Environment {
            let seg = |kind: FormKind, label: String, cx: &mut Context<Self>| SegItem {
                label,
                on: self.kind == kind,
                on_click: Box::new(cx.listener(move |this, _, window, cx| this.set_kind(kind, window, cx))),
            };
            let items = vec![
                seg(FormKind::Local, t!("project.typeLocal").to_string(), cx),
                seg(FormKind::Ssh, t!("project.typeSsh").to_string(), cx),
                seg(FormKind::Multi, t!("multi.typeMulti").to_string(), cx),
            ];
            form = form.child(segmented("pf-kind", items, cx));
        }

        let name_label = if self.environment() { t!("env.environmentName") } else { t!("project.name") };
        form = form.child(field(Some(name_label.to_string()), Input::new(&self.name), None, cx));

        match (self.kind, ty) {
            (FormKind::Environment, _) => {
                let editing = self.existing.is_some();
                if !editing && !self.locked {
                    let control = div()
                        .flex()
                        .gap(zpx(10.))
                        .child(div().flex_1().min_w_0().child(Select::new(&self.location_select).w_full()))
                        .child(self.render_add_host(cx));
                    form = form.child(field(Some(t!("multi.location").to_string()), control, conn.map(|c| (c, Tone::Muted)), cx));
                }
                let dir_count = self.env_dirs(cx).len();
                let dir_hint = if editing && self.existing.as_ref().is_some_and(|p| p.multi.is_some()) && self.env_has_threads(cx) {
                    Some((t!("env.dirsLockedByThreads").to_string(), Tone::Muted))
                } else if !editing && dir_count > 1 {
                    Some((t!("env.multiDirsHint").to_string(), Tone::Muted))
                } else {
                    None
                };
                form = form.child(field(Some(t!("env.directory").to_string()), self.render_env_dirs(cx), dir_hint, cx));
                let branch_label = Some(t!("env.baseBranch").to_string());
                if editing {
                    if self.existing.as_ref().is_some_and(|p| p.default_worktree_branch.is_some()) {
                        form = form.child(field(branch_label, Input::new(&self.default_wt).readonly(true), None, cx));
                    }
                } else if dir_count == 1 {
                    form = match &self.branch_probe {
                        BranchProbe::Idle | BranchProbe::PlainDirectory => form,
                        BranchProbe::Loading => form.child(kit::note(t!("env.branchesLoading").to_string(), cx)),
                        BranchProbe::Git => form.child(field(
                            branch_label,
                            Select::new(&self.base_branch).w_full(),
                            Some((t!("env.baseHint").to_string(), Tone::Muted)),
                            cx,
                        )),
                        BranchProbe::Failed(message) => form
                            .child(error_line(message.clone(), cx))
                            .child(Button::new("pf-env-retry").outline().label(t!("env.retryBranches").to_string())
                                .on_click(cx.listener(|this, _, window, cx| this.reprobe_branches(true, window, cx)))),
                    };
                }
            }
            (FormKind::Multi, _) => {
                // 从主机菜单进来位置已定死，不再显示下拉
                if self.existing.is_none() && !self.locked {
                    let control = div()
                        .flex()
                        .gap(zpx(10.))
                        .child(div().flex_1().min_w_0().child(Select::new(&self.location_select).w_full()))
                        .child(self.render_add_host(cx));
                    form = form.child(field(Some(t!("multi.location").to_string()), control, conn.clone().map(|c| (c, Tone::Muted)), cx));
                }
                let hint = self.repos_readonly.then(|| {
                    let key = if durable_thread { "env.threadDirsReadonly" } else { "multi.reposReadonly" };
                    (t!(key).to_string(), Tone::Muted)
                });
                let repos_label = if durable_thread { t!("env.directory") } else { t!("multi.repos") };
                form = form.child(field(Some(repos_label.to_string()), self.render_repos(cx), hint, cx));
                if !self.environment() && !self.repos_readonly {
                    let all_empty = self.repo_values(cx).iter().all(|r| r.trim().is_empty());
                    let control = div()
                        .flex()
                        .gap(zpx(10.))
                        .child(div().flex_1().min_w_0().child(Input::new(&self.working_dir)))
                        .child(
                            Button::new("pf-common-parent")
                                .outline()
                                .label(t!("multi.useCommonParent").to_string())
                                .disabled(all_empty)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let dir = falcon_core::multi_path::common_parent_dir(&this.repo_values(cx));
                                    this.working_dir.update(cx, |i, cx| i.set_value(dir, window, cx));
                                    cx.notify();
                                })),
                        );
                    form = form.child(field(
                        Some(t!("project.workingDir").to_string()),
                        control,
                        Some((t!("multi.workingDirHint").to_string(), Tone::Muted)),
                        cx,
                    ));
                }
            }
            (_, _) if durable_thread => {
                form = form.child(field(
                    Some(t!("env.directory").to_string()),
                    Input::new(&self.working_dir).readonly(true),
                    None,
                    cx,
                ));
            }
            (_, ProjectType::Local) => {
                let control = div()
                    .flex()
                    .gap(zpx(10.))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.working_dir)))
                    .child(
                        Button::new("pf-browse")
                            .outline()
                            .label(t!("project.browse").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
                    );
                let hint = self.path_hint.clone().map(|(ok, text)| (text, if ok { Tone::Ok } else { Tone::Err }));
                form = form.child(field(Some(t!("project.workingDir").to_string()), control, hint, cx));
            }
            (_, _) => {
                let legacy = self.legacy();
                if !self.locked {
                    let label = if legacy && self.host_id.is_empty() { t!("host.bind") } else { t!("host.title") };
                    let hint = if hosts.is_empty() {
                        Some((t!("host.emptyCreate").to_string(), Tone::Muted))
                    } else {
                        conn.clone().map(|c| (c, Tone::Muted))
                    };
                    let control = if hosts.is_empty() {
                        div().flex().child(self.render_add_host(cx))
                    } else {
                        div()
                            .flex()
                            .gap(zpx(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Select::new(&self.host_select).placeholder(t!("host.pick").to_string()).w_full()),
                            )
                            .child(self.render_add_host(cx))
                    };
                    form = form.child(field(Some(label.to_string()), control, hint, cx));
                }
                if legacy && self.host_id.is_empty() {
                    form = form.children(self.legacy_ssh.render(cx));
                }
                let can_browse = self.can_browse_remote();
                let hint = match self.path_hint.clone() {
                    Some((ok, text)) => Some((text, if ok { Tone::Ok } else { Tone::Err })),
                    None if !can_browse => Some((t!("project.pickNeedHost").to_string(), Tone::Muted)),
                    None => None,
                };
                let control = div()
                    .flex()
                    .gap(zpx(10.))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.working_dir)))
                    .child(
                        Button::new("pf-browse")
                            .outline()
                            .label(t!("project.browse").to_string())
                            .disabled(!can_browse)
                            .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
                    );
                form = form.child(field(Some(t!("project.remoteDir").to_string()), control, hint, cx));
            }
        }

        if !self.environment() && !durable_thread && self.existing.as_ref().is_none_or(|p| p.worktree.is_none()) {
            form = form.child(field(
                Some(t!("project.defaultWorktreeBranch").to_string()),
                Input::new(&self.default_wt),
                Some((t!("project.defaultWorktreeBranchHint").to_string(), Tone::Muted)),
                cx,
            ));
        }

        if let Some(e) = self.error.clone() {
            form = form.child(error_line(e, cx));
        }

        let submit_label = if self.existing.is_some() { t!("project.save") } else { t!("project.create") };
        form.child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("pf-cancel")
                        .outline()
                        .label(t!("common.cancel").to_string())
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("pf-submit")
                        .primary()
                        .label(submit_label.to_string())
                        .loading(self.busy)
                        .disabled(!self.can_submit(cx))
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
        )
    }
}

impl Render for ProjectForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.picker.clone().filter(|_| self.any_picking()) {
            return div().child(picker);
        }
        self.render_form(cx)
    }
}

pub fn open(ws: &Entity<Workspace>, edit: Option<Project>, preset: Preset, window: &mut Window, cx: &mut App) {
    present(ws, edit, preset, None, window, cx);
}

/// 新建环境，建好后调 `on_created`。新建线程的表单从这里叠开环境表单
pub fn open_environment(ws: &Entity<Workspace>, mut preset: Preset, on_created: OnCreated, window: &mut Window, cx: &mut App) {
    preset.environment = true;
    present(ws, None, preset, Some(on_created), window, cx);
}

fn present(
    ws: &Entity<Workspace>,
    edit: Option<Project>,
    preset: Preset,
    on_created: Option<OnCreated>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| {
        let mut form = ProjectForm::new(ws.clone(), edit, preset, window, cx);
        form.on_created = on_created;
        form
    });
    let f = form.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let (title, wide) = {
            let s = f.read(cx);
            (s.title(), s.any_picking())
        };
        let (submit, escape) = (f.clone(), f.clone());
        // lockOverlay：点遮罩不关、不显示右上角的关闭钮（填了一半手滑就全丢）
        dialog
            .title(title)
            .w(zpx(if wide { 576. } else { 448. }))
            .entrance(gpui_kit::component::dialog::DialogEntrance::FadeSlide)
            .overlay_closable(false)
            .close_button(false)
            .child(f.clone())
            .on_ok(move |_, window, cx| {
                submit.update(cx, |this, cx| this.submit(window, cx));
                false
            })
            .on_cancel(move |_, window, cx| escape.update(cx, |this, cx| this.on_escape(window, cx)))
    });
    // 对话框打开时会把焦点收到自己身上：打开之后再落到该落的输入框
    let (picker, name) = {
        let f = form.read(cx);
        (f.picker.clone().filter(|_| f.any_picking()), f.name.clone())
    };
    match picker {
        Some(p) => p.update(cx, |p, cx| p.focus(window, cx)),
        None => name.update(cx, |i, cx| i.focus(window, cx)),
    }
}
