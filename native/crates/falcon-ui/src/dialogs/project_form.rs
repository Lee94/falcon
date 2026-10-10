//! 新建 / 编辑项目表单（web 的 `components/ProjectForm.tsx`）。
//!
//! 三档填法：本地文件夹 / 远程 SSH / 多仓库。第三档「多仓库」是**纯 UI 概念**：服务端的
//! type 仍只有 local / ssh，提交时映射成 type + repos（选了主机 ⇒ ssh，否则 local；编辑时跟
//! existing 走，PUT 不许改类型）。
//!
//! - SSH 从已保存主机里选（可就地「添加主机」→ 主机表单，存完回调把新主机选进来）；存量项目
//!   没有 hostId，可以继续手写 SSH 字段，也可以改绑到已保存主机。
//! - 工作目录旁的「浏览…」打开 [`FolderPicker`]：列的是**宿主机**上的目录（本地项目是后端
//!   所在机器，SSH 项目是远端），不是这台 Mac 的系统文件夹选择器。选择器嵌在同一个对话框里
//!   （web 同样不叠第二层），Esc 只退回表单。
//! - 从侧栏某台服务器的菜单进来（preset 带了 kind）：类型已定，单仓库直接进选目录，选完就创建；
//!   多仓库先弹一次「添加仓库」的选择器。这两种情况一个都没选就取消 = 反悔，整个对话框一起关。
//! - shell 下拉：自动侦测宿主机可用的 shell（本地问后端本机，SSH 问远端），失败静默——退回
//!   "默认 + 自定义"，不打断创建。

pub(crate) mod kit;

use falcon_client::ProbeTarget;
use falcon_proto::{Project, ProjectInput, ProjectType, ShellsInfo, SshConfigInput, SshHost};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, Task, Window, div};
use rust_i18n::t;

use self::kit::{Opt, OptState, Options, SegItem, Tone, error_line, field, segmented};
use crate::dialogs::folder_picker::FolderPicker;
use crate::dialogs::host_form::{self, SshFields, SshInit};
use crate::workspace::Workspace;
use crate::zoom::zpx;

/// 打开表单时的预填（web 的 ProjectFormPreset）
#[derive(Clone, Debug, Default)]
pub struct Preset {
    /// "local" / "ssh"；None = 让用户选
    pub kind: Option<&'static str>,
    pub host_id: Option<String>,
    pub multi: bool,
}

impl Preset {
    pub fn local(multi: bool) -> Self {
        Self { kind: Some("local"), host_id: None, multi }
    }
    pub fn ssh(host_id: Option<String>, multi: bool) -> Self {
        Self { kind: Some("ssh"), host_id, multi }
    }
}

/// shell 下拉的两个哨兵值（web 同名：Radix Select 不接受空字符串当 value）
const SHELL_AUTO: &str = "__auto__";
const SHELL_CUSTOM: &str = "__custom__";
/// 多仓库档「位置」下拉的本机哨兵，理由同上
const MULTI_LOCAL: &str = "__local__";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKind {
    Local,
    Ssh,
    Multi,
}

pub struct ProjectForm {
    ws: Entity<Workspace>,
    existing: Option<Project>,
    kind: FormKind,
    /// 从某台服务器进来：类型已定，不显示三档切换，选完目录就创建
    locked: bool,
    /// 从主机菜单进「新建多仓库项目」：先弹一次添加仓库
    preset_multi: bool,
    name: Entity<InputState>,
    working_dir: Entity<InputState>,
    /// 多仓库档的成员清单（成员路径原文，可手改可从选择器追加）
    repos: Vec<Entity<InputState>>,
    /// 派生产物的成员由派生决定：只读展示，提交不带 repos（PUT 会拒）
    repos_readonly: bool,
    shell: String,
    shell_custom: bool,
    shells: Option<ShellsInfo>,
    shell_select: Entity<OptState>,
    shell_input: Entity<InputState>,
    default_wt: Entity<InputState>,
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
    /// 上一次侦测 shell 的目标，目标变了才重新侦测
    shells_key: Option<String>,
    hosts_seen: Vec<(String, String)>,
    _shells_task: Option<Task<()>>,
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
            Some(p) if p.multi.is_some() => FormKind::Multi,
            Some(p) if p.project_type == ProjectType::Ssh => FormKind::Ssh,
            Some(_) => FormKind::Local,
            None if preset.multi => FormKind::Multi,
            None if preset.kind == Some("ssh") => FormKind::Ssh,
            None => FormKind::Local,
        };
        let preset_multi = existing.is_none() && preset.multi;
        let locked = existing.is_none() && preset.kind.is_some();
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
            FormKind::Multi => None,
        };
        let working_dir = text(existing.as_ref().and_then(|p| p.working_dir.clone()).unwrap_or_default(), placeholder, window, cx);
        let repos: Vec<Entity<InputState>> = existing
            .as_ref()
            .and_then(|p| p.multi.as_ref())
            .map(|m| m.repos.iter().map(|r| text(r.dir.clone(), None, window, cx)).collect())
            .unwrap_or_default();
        let repos_readonly = existing.as_ref().is_some_and(|p| p.multi.is_some() && p.worktree.is_some());
        let shell = existing.as_ref().and_then(|p| p.shell.clone()).unwrap_or_default();
        let shell_input = text(shell.clone(), None, window, cx);
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
        let shell_select = cx.new(|cx| kit::new_select(Options::default(), SHELL_AUTO, window, cx));
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
        cx.subscribe_in(&shell_select, window, |this, _, ev: &SelectEvent<Options>, window, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else { return };
            this.pick_shell(v.clone(), window, cx);
        })
        .detach();
        // 主机列表变了（从这里新增了一台、别处删了一台）就重建两个下拉
        cx.observe_in(&ws, window, |this, ws, window, cx| {
            let seen: Vec<(String, String)> = ws.read(cx).hosts.iter().map(|h| (h.id.clone(), h.name.clone())).collect();
            if seen != this.hosts_seen {
                this.hosts_seen = seen;
                this.sync_host_selects(window, cx);
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
            preset_multi,
            name,
            working_dir,
            repos,
            repos_readonly,
            shell,
            shell_custom: false,
            shells: None,
            shell_select,
            shell_input,
            default_wt,
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
            shells_key: None,
            hosts_seen,
            _shells_task: None,
        };
        this.sync_shell_select(window, cx);
        this.reprobe_shells(window, cx);
        // 从服务器进来：单仓库直接选文件夹；多仓库先弹一次添加仓库，选完落回表单继续添加
        if preset_multi {
            this.open_repo_picker(window, cx);
        } else if locked {
            this.open_picker(window, cx);
        }
        this
    }

    // ---------------- 派生出来的判断（web 里那几个 const） ----------------

    fn project_type(&self) -> ProjectType {
        match self.kind {
            FormKind::Local => ProjectType::Local,
            FormKind::Ssh => ProjectType::Ssh,
            FormKind::Multi => match &self.existing {
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

    fn title(&self) -> String {
        if self.any_picking() {
            if self.project_type() == ProjectType::Ssh {
                t!("project.pickRemoteTitle").to_string()
            } else {
                t!("project.pickTitle").to_string()
            }
        } else if self.existing.is_some() {
            t!("project.editTitle").to_string()
        } else {
            t!("project.createTitle").to_string()
        }
    }

    fn shell_value(&self, cx: &App) -> String {
        if self.shell_custom {
            self.shell_input.read(cx).value().to_string()
        } else {
            self.shell.clone()
        }
    }

    /// 目录浏览 / shell 侦测的目标：编辑已有 SSH 项目问它的远端，选了主机问那台主机
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
            FormKind::Multi => "",
        };
        self.working_dir.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
        self.reprobe_shells(window, cx);
        cx.notify();
    }

    fn set_host(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.host_id = id;
        self.sync_host_selects(window, cx);
        self.reprobe_shells(window, cx);
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

    /// 侦测宿主机可用的 shell。本地问后端本机；SSH 有目标（选了主机或编辑已有 SSH 项目）才问远端
    fn reprobe_shells(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (key, target) = if self.project_type() == ProjectType::Local {
            (Some("local".to_string()), ProbeTarget::Backend)
        } else if !self.host_id.is_empty() {
            (Some(format!("host:{}", self.host_id)), ProbeTarget::SshHost(self.host_id.clone()))
        } else {
            match self.existing.as_ref().filter(|p| p.project_type == ProjectType::Ssh) {
                Some(p) => (Some(format!("project:{}", p.id)), ProbeTarget::Project(p.id.clone())),
                None => (None, ProbeTarget::Backend),
            }
        };
        if key == self.shells_key {
            return;
        }
        self.shells_key = key.clone();
        self.shells = None;
        self.sync_shell_select(window, cx);
        let Some(key) = key else {
            self._shells_task = None;
            return;
        };
        let request = self.ws.read(cx).client.list_shells(&target);
        self._shells_task = Some(cx.spawn_in(window, async move |this, cx| {
            // 失败静默：表单退回"默认 + 自定义"，不打断创建
            let Ok(info) = request.await else { return };
            this.update_in(cx, |this, window, cx| {
                if this.shells_key.as_deref() == Some(key.as_str()) {
                    this.shells = Some(info);
                    this.sync_shell_select(window, cx);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// 下拉里必须包含当前值，哪怕侦测没跑完或它不在侦测结果里（存量项目手填过）
    fn sync_shell_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let auto_label = match &self.shells {
            Some(info) => t!("project.shellAutoDetected", shell = folder_name(&info.default)).to_string(),
            None => t!("project.shellAuto").to_string(),
        };
        let mut shells: Vec<String> = self.shells.as_ref().map(|s| s.shells.clone()).unwrap_or_default();
        if !self.shell.is_empty() && !self.shell_custom && !shells.contains(&self.shell) {
            shells.insert(0, self.shell.clone());
        }
        let mut opts = vec![Opt::new(SHELL_AUTO, auto_label)];
        opts.extend(shells.into_iter().map(|s| Opt::new(s.clone(), s)));
        opts.push(Opt::new(SHELL_CUSTOM, t!("project.shellCustom").to_string()));
        let value = if self.shell_custom {
            SHELL_CUSTOM.to_string()
        } else if self.shell.is_empty() {
            SHELL_AUTO.to_string()
        } else {
            self.shell.clone()
        };
        kit::reset_select(&self.shell_select, Options::flat(opts), &value, window, cx);
    }

    fn pick_shell(&mut self, v: String, window: &mut Window, cx: &mut Context<Self>) {
        if v == SHELL_AUTO {
            self.shell.clear();
            self.shell_custom = false;
        } else if v == SHELL_CUSTOM {
            // 自定义框从当前值起填
            let current = self.shell.clone();
            let placeholder = if self.project_type() == ProjectType::Local { "pwsh.exe / zsh" } else { "/usr/bin/zsh" };
            self.shell_input.update(cx, |i, cx| {
                i.set_value(current, window, cx);
                i.set_placeholder(placeholder, window, cx);
            });
            self.shell_custom = true;
        } else {
            self.shell = v;
            self.shell_custom = false;
        }
        self.sync_shell_select(window, cx);
        cx.notify();
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
        let on_select: crate::dialogs::folder_picker::OnSelect = std::rc::Rc::new(move |dir, window, cx| {
            this_sel
                .update(cx, |f, cx| {
                    if for_repo {
                        f.pick_repo_folder(dir, window, cx)
                    } else {
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
        let picker = cx.new(|cx| FolderPicker::new(client, target, remote, &initial, on_select, on_close, window, cx));
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
        if self.locked && !next_name.is_empty() {
            self.save(next_name, dir, window, cx);
            return;
        }
        self.leave_picker(window, cx);
    }

    /// 多仓库档「添加仓库」：选中即追加，不关外层表单；重复选中静默去重
    fn pick_repo_folder(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !self.repos.iter().any(|r| r.read(cx).value().as_ref() == dir) {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(dir));
            self.repos.push(input);
        }
        self.leave_picker(window, cx);
    }

    /// 选择器的取消 / Esc。返回 true = 反悔了，整个对话框一起关
    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        if self.picking_repo {
            if self.preset_multi && self.repos.is_empty() {
                return true;
            }
        } else if self.locked && self.working_dir.read(cx).value().is_empty() {
            return true;
        }
        self.leave_picker(window, cx);
        false
    }

    fn leave_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picking = false;
        self.picking_repo = false;
        self.picker = None;
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
        !self.busy
            && !self.name.read(cx).value().is_empty()
            && !(self.project_type() == ProjectType::Ssh && self.using_saved_host() && self.host_id.is_empty())
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
        // 错误写在表单上：选择器开着就收起来，不然看不见（web 在请求失败时同样退回表单）
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
            working_dir: nonempty(&working_dir),
            shell: nonempty(&self.shell_value(cx)),
            // 派生产物的成员由派生决定，不上送（PUT 会拒）；容器每次全量替换
            repos: multi.then_some(clean_repos),
            host_id: (ty == ProjectType::Ssh && !self.host_id.is_empty()).then(|| self.host_id.clone()),
            ssh,
            default_worktree_branch: nonempty(self.default_wt.read(cx).value().trim()),
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
                                w.select_project(&project.id, cx);
                            }
                            w.refresh_projects(cx);
                            w.refresh_hosts(cx);
                        });
                        window.close_dialog(cx);
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

    fn render_add_host(&self, cx: &mut Context<Self>) -> Button {
        Button::new("pf-add-host")
            .outline()
            .label(t!("host.add").to_string())
            .on_click(cx.listener(|this, _, window, cx| {
                let form = cx.entity().downgrade();
                let on_saved: host_form::OnSaved = std::rc::Rc::new(move |h: &SshHost, window, cx| {
                    let id = h.id.clone();
                    form.update(cx, |f, cx| f.set_host(id, window, cx)).ok();
                });
                host_form::open(&this.ws, None, Some(on_saved), window, cx);
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

    fn render_form(&mut self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let ty = self.project_type();
        let hosts = self.ws.read(cx).hosts.clone();
        let selected_host = hosts.iter().find(|h| h.id == self.host_id).cloned();
        let conn = selected_host.as_ref().map(falcon_core::host_color::ssh_conn);
        let mut form = div().flex().flex_col().gap_3();

        if self.existing.is_none() && !self.locked {
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

        form = form.child(field(Some(t!("project.name").to_string()), Input::new(&self.name), None, cx));

        match (self.kind, ty) {
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
                let hint = self.repos_readonly.then(|| (t!("multi.reposReadonly").to_string(), Tone::Muted));
                form = form.child(field(Some(t!("multi.repos").to_string()), self.render_repos(cx), hint, cx));
                if !self.repos_readonly {
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

        let mut shell = div().flex().flex_col().gap_2().child(Select::new(&self.shell_select).w_full());
        if self.shell_custom {
            shell = shell.child(Input::new(&self.shell_input));
        }
        form = form.child(field(Some(t!("project.shell").to_string()), shell, None, cx));

        if self.existing.as_ref().is_none_or(|p| p.worktree.is_none()) {
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
    let form = cx.new(|cx| ProjectForm::new(ws.clone(), edit, preset, window, cx));
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
