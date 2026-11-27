//! 新建线程（⌘P / 侧栏顶上的「新建线程」，ADR 0031）。
//!
//! 照「先写要做的事，再挑在哪儿做」的输入框式样：上面一大块写第一条消息，底下一排小按钮选
//! **机器**、**环境**和**第一个动作**（Claude 对话 / 普通终端 / 某个 Agent CLI），回车开始。
//! 不选模型：模型是各家 Agent 自己的设置，这里只决定线程开场时在画布上开什么；开场之后在线程里
//! 照样能再开别的 Agent 或终端（侧栏线程行的 ＋）。
//!
//! - 线程不单独填名称，取第一条消息的第一行（`project_tree::thread_name_from_prompt`），之后可改；
//! - 第一条消息交给谁：Claude 对话连上后自动发出；能带初始提示词的 CLI（`SessionAgent::accepts_prompt`）
//!   开场就收到；普通终端与其余 CLI 不收，这段文字只用作名称，输入框下面写明；
//! - 机器上还没有环境就在这里叠开环境表单（位置已定，直接进目录选择器），建好自动选回来；
//! - 节点离线或连接中的主机不能选：上面的环境不在列表里，新建环境也会失败。

use falcon_core::project_tree::{thread_environment, thread_name_from_prompt};
use falcon_proto::{Project, ProjectType, SessionAgent};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Disableable, Icon, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, WeakEntity, Window, div};
use rust_i18n::t;

use crate::dialogs::project_form::kit::error_line;
use crate::dialogs::project_form::{self, OnCreated, Preset};
use crate::menus::{MenuItemSpec, to_popup};
use crate::session_icon::session_icon;
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ThreadStart, Workspace};
use crate::zoom::zpx;

/// 「本机」在机器选择里的 id（主机用自己的 id）
const MACHINE_LOCAL: &str = "__local__";

/// 环境所在的机器：本地环境是本机，SSH 环境是它绑定的主机。没绑主机的存量 SSH 环境不归任何
/// 一台机器，不出现在选项里——新线程总要落在一台能选的机器上
fn env_machine(p: &Project) -> Option<String> {
    match p.project_type {
        ProjectType::Local => Some(MACHINE_LOCAL.to_owned()),
        ProjectType::Ssh => p.host_id.clone().filter(|id| !id.is_empty()),
    }
}

struct Machine {
    id: String,
    name: String,
    icon: IconName,
    /// 节点离线 / 连接中：不能选，菜单里写明原因
    unavailable: Option<String>,
}

fn machines(w: &Workspace) -> Vec<Machine> {
    let mut out =
        vec![Machine { id: MACHINE_LOCAL.into(), name: crate::labels::local_word(), icon: IconName::Monitor, unavailable: None }];
    for host in &w.hosts {
        let unavailable = w.node_status(&host.id).filter(|node| !node.online).map(|node| {
            if node.error.is_some() { t!("sidebar.hostOffline") } else { t!("sidebar.hostConnecting") }.to_string()
        });
        out.push(Machine { id: host.id.clone(), name: host.name.clone(), icon: IconName::Server, unavailable });
    }
    out
}

fn environments<'a>(w: &'a Workspace, machine: &str) -> Vec<&'a Project> {
    w.projects.iter().filter(|p| p.is_environment() && env_machine(p).as_deref() == Some(machine)).collect()
}

/// 打开时预选的环境：调用方指定的 → 当前选中的线程或环境所在的 → 第一个能选的
fn initial_env(w: &Workspace, requested: &str) -> Option<String> {
    let usable = |id: &str| w.project(id).is_some_and(|p| p.is_environment() && env_machine(p).is_some());
    if usable(requested) {
        return Some(requested.to_owned());
    }
    let current = w.state.selected_project_id.as_deref().and_then(|id| w.project(id));
    let current_env = current.and_then(|p| if p.is_environment() { Some(p.id.as_str()) } else { thread_environment(p) });
    if let Some(id) = current_env.filter(|id| usable(id)) {
        return Some(id.to_owned());
    }
    w.projects.iter().find(|p| p.is_environment() && env_machine(p).is_some()).map(|p| p.id.clone())
}

/// 这个环境里能选的第一个动作：Claude 对话要宿主机上有 Claude Code；Agent 只列那台机器上测到的 CLI
fn start_options(w: &Workspace, env: Option<&str>) -> Vec<ThreadStart> {
    let agents: &[SessionAgent] = match env {
        Some(env) => w.session_agents(env),
        None => &w.local_agents,
    };
    let mut out = Vec::new();
    if agents.contains(&SessionAgent::Claude) {
        out.push(ThreadStart::Chat);
    }
    out.push(ThreadStart::Terminal);
    out.extend(agents.iter().map(|a| ThreadStart::Agent(*a)));
    out
}

fn start_label(start: ThreadStart) -> String {
    match start {
        ThreadStart::Chat => t!("claudeChat.title").to_string(),
        ThreadStart::Terminal => t!("agent.terminal").to_string(),
        ThreadStart::Agent(a) => t!(format!("agent.{}", a.as_str())).to_string(),
    }
}

fn start_icon(start: ThreadStart) -> Icon {
    match start {
        ThreadStart::Chat => session_icon(Some(SessionAgent::Claude)),
        ThreadStart::Terminal => session_icon(None),
        ThreadStart::Agent(a) => session_icon(Some(a)),
    }
}

/// 第一条消息会不会真的交给这个动作
fn takes_prompt(start: ThreadStart) -> bool {
    match start {
        ThreadStart::Chat => true,
        ThreadStart::Terminal => false,
        ThreadStart::Agent(a) => a.accepts_prompt(),
    }
}

pub struct NewThread {
    ws: Entity<Workspace>,
    /// [`MACHINE_LOCAL`] 或主机 id
    machine: String,
    env: Option<String>,
    start: ThreadStart,
    /// 用户在下拉里亲手选过第一个动作：之后换环境，只要新环境里还有它就不改
    start_picked: bool,
    prompt: Entity<TextareaState>,
    busy: bool,
    error: Option<String>,
}

/// 新建线程。`env` 是预选的环境 id，空串表示按 [`initial_env`] 挑
pub fn open(ws: &Entity<Workspace>, env: &str, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| NewThread::new(ws.clone(), env, window, cx));
    let f = form.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = f.clone();
        // 写了一半手滑点到遮罩上不能全丢：只认「关闭」与 Esc
        dialog
            .w(zpx(640.))
            .margin_top(zpx(80.))
            .close_button(false)
            .overlay_closable(false)
            .entrance(DialogEntrance::FadeSlide)
            .child(f.clone())
            // 输入框开了 submit_on_enter：回车（不带 Shift）一路冒到这里；Shift+回车换行
            .on_ok(move |_, window, cx| {
                submit.update(cx, |this, cx| this.submit(window, cx));
                false
            })
    });
    // 对话框打开时会把焦点收到自己身上：打开之后再落到输入框
    let prompt = form.read(cx).prompt.clone();
    prompt.update(cx, |i, cx| i.focus(window, cx));
}

impl NewThread {
    fn new(ws: Entity<Workspace>, env: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (env, machine) = {
            let w = ws.read(cx);
            let env = initial_env(w, env);
            let machine = env.as_deref().and_then(|id| w.project(id)).and_then(env_machine);
            (env, machine.unwrap_or_else(|| MACHINE_LOCAL.to_owned()))
        };
        let prompt = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 12).submit_on_enter(true));
        cx.observe(&prompt, |_, _, cx| cx.notify()).detach();
        // 别处新增 / 删除了主机或环境、节点上下线、CLI 清单拉回来了：选择跟着收敛
        cx.observe(&ws, |this, _, cx| {
            this.reconcile(cx);
            cx.notify();
        })
        .detach();
        let mut this =
            Self { ws, machine, env, start: ThreadStart::Terminal, start_picked: false, prompt, busy: false, error: None };
        this.reconcile(cx);
        this.sync_placeholder(window, cx);
        this
    }

    /// 选择与工作区对齐：机器没了退回本机，环境不在这台机器上了换成第一个，第一个动作不在
    /// 新环境的选项里就按默认挑（上一次新建线程用的 → Claude 对话 → 普通终端）
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let w = self.ws.read(cx);
        if self.machine != MACHINE_LOCAL && !w.hosts.iter().any(|h| h.id == self.machine) {
            self.machine = MACHINE_LOCAL.to_owned();
        }
        let envs = environments(w, &self.machine);
        if !self.env.as_ref().is_some_and(|id| envs.iter().any(|p| &p.id == id)) {
            self.env = envs.first().map(|p| p.id.clone());
        }
        let options = start_options(w, self.env.as_deref());
        if !(self.start_picked && options.contains(&self.start)) {
            self.start = w
                .last_thread_start
                .filter(|s| options.contains(s))
                .or_else(|| options.contains(&ThreadStart::Chat).then_some(ThreadStart::Chat))
                .unwrap_or(ThreadStart::Terminal);
        }
    }

    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = if takes_prompt(self.start) { t!("newThread.placeholder") } else { t!("newThread.placeholderName") };
        self.prompt.update(cx, |i, cx| i.set_placeholder(text.to_string(), window, cx));
    }

    fn set_machine(&mut self, id: String, cx: &mut Context<Self>) {
        if self.machine == id {
            return;
        }
        self.machine = id;
        self.env = None;
        self.error = None;
        self.reconcile(cx);
        cx.notify();
    }

    fn set_env(&mut self, id: String, cx: &mut Context<Self>) {
        self.env = Some(id);
        self.error = None;
        self.reconcile(cx);
        cx.notify();
    }

    fn set_start(&mut self, start: ThreadStart, window: &mut Window, cx: &mut Context<Self>) {
        self.start = start;
        self.start_picked = true;
        self.sync_placeholder(window, cx);
        cx.notify();
    }

    /// 叠开的环境表单建好了：切到它所在的机器并选中它
    fn pick_environment(&mut self, env: &Project, window: &mut Window, cx: &mut Context<Self>) {
        let Some(machine) = env_machine(env) else { return };
        self.machine = machine;
        self.env = Some(env.id.clone());
        self.error = None;
        self.reconcile(cx);
        self.sync_placeholder(window, cx);
        cx.notify();
    }

    /// 在选中的机器上叠开环境表单（位置已定，直接进目录选择器）
    fn create_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let preset = if self.machine == MACHINE_LOCAL {
            Preset::local(true)
        } else {
            Preset::ssh(Some(self.machine.clone()), true)
        };
        let form = cx.entity().downgrade();
        let on_created: OnCreated = std::rc::Rc::new(move |env: &Project, window, cx| {
            form.update(cx, |f, cx| f.pick_environment(env, window, cx)).ok();
        });
        project_form::open_environment(&self.ws, preset, on_created, window, cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(env) = self.env.clone() else {
            self.error = Some(t!("newThread.needEnvironment").to_string());
            cx.notify();
            return;
        };
        let text = self.prompt.read(cx).value().trim().to_owned();
        let name = thread_name_from_prompt(&text).unwrap_or_else(|| t!("newThread.untitled").to_string());
        let start = self.start;
        self.busy = true;
        self.error = None;
        let request = self.ws.read(cx).client.create_thread(&env, &name, "");
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(thread) => {
                        let prompt = (!text.is_empty() && takes_prompt(start)).then_some(text);
                        this.ws.update(cx, |w, cx| {
                            // 列表刷新是异步的：先把线程放进去，下面的打开 / 新建才找得到它
                            if !w.projects.iter().any(|p| p.id == thread.id) {
                                w.projects.push(thread.clone());
                            }
                            w.last_thread_start = Some(start);
                            match start {
                                ThreadStart::Chat => w.open_chat(&thread.id, prompt, cx),
                                ThreadStart::Terminal => w.new_terminal(&thread.id, None, None, cx),
                                ThreadStart::Agent(agent) => w.new_terminal_with_prompt(&thread.id, Some(agent), prompt, cx),
                            }
                            w.refresh_inventory(cx);
                        });
                        window.close_dialog(cx);
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn machine_menu(&self, me: &WeakEntity<Self>, cx: &App) -> Vec<MenuItemSpec> {
        machines(self.ws.read(cx))
            .into_iter()
            .map(|m| {
                let label = match &m.unavailable {
                    Some(status) => format!("{} · {status}", m.name),
                    None => m.name.clone(),
                };
                let (me, id) = (me.clone(), m.id.clone());
                MenuItemSpec::new(label, move |_, cx| {
                    me.update(cx, |this, cx| this.set_machine(id.clone(), cx)).ok();
                })
                .icon(icon(m.icon))
                .checked(m.id == self.machine)
                .disabled(m.unavailable.is_some())
            })
            .collect()
    }

    fn env_menu(&self, me: &WeakEntity<Self>, cx: &App) -> Vec<MenuItemSpec> {
        let mut items: Vec<MenuItemSpec> = environments(self.ws.read(cx), &self.machine)
            .into_iter()
            .map(|p| {
                let (me, id) = (me.clone(), p.id.clone());
                MenuItemSpec::new(p.name.clone(), move |_, cx| {
                    me.update(cx, |this, cx| this.set_env(id.clone(), cx)).ok();
                })
                .icon(icon(IconName::Folder))
                .checked(self.env.as_deref() == Some(p.id.as_str()))
            })
            .collect();
        let me = me.clone();
        let create = MenuItemSpec::new(t!("newThread.createEnvironment").to_string(), move |window, cx| {
            me.update(cx, |this, cx| this.create_environment(window, cx)).ok();
        })
        .icon(icon(IconName::FolderPlus));
        items.push(if items.is_empty() { create } else { create.sep() });
        items
    }

    fn start_menu(&self, me: &WeakEntity<Self>, cx: &App) -> Vec<MenuItemSpec> {
        let options = start_options(self.ws.read(cx), self.env.as_deref());
        // Claude 对话 / 普通终端 与各家 CLI 之间画一道线
        let first_agent = options.iter().position(|s| matches!(s, ThreadStart::Agent(_)));
        options
            .into_iter()
            .enumerate()
            .map(|(i, start)| {
                let me = me.clone();
                let item = MenuItemSpec::new(start_label(start), move |window, cx| {
                    me.update(cx, |this, cx| this.set_start(start, window, cx)).ok();
                })
                .icon(start_icon(start))
                .checked(start == self.start);
                if Some(i) == first_agent { item.sep() } else { item }
            })
            .collect()
    }
}

/// 底栏上的一个选项按钮：图标 + 当前值，点开是下拉菜单
fn chip(id: &str, glyph: Icon, label: String, items: Vec<MenuItemSpec>, disabled: bool) -> impl IntoElement {
    Button::new(SharedString::from(id.to_string()))
        .outline()
        .small()
        .icon(glyph)
        .label(label)
        .disabled(disabled)
        .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone()))
}

impl Render for NewThread {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let me = cx.entity().downgrade();
        let (machine, env_name) = {
            let w = self.ws.read(cx);
            let machine = machines(w).into_iter().find(|m| m.id == self.machine);
            let env_name = self.env.as_deref().and_then(|id| w.project(id)).map(|p| p.name.clone());
            (machine, env_name)
        };
        let (machine_icon, machine_name) =
            machine.map(|m| (m.icon, m.name)).unwrap_or_else(|| (IconName::Monitor, crate::labels::local_word()));
        let has_text = !self.prompt.read(cx).value().trim().is_empty();
        // 第一条消息交不出去时说清楚：这段文字只用作线程名称
        let hint = (has_text && !takes_prompt(self.start))
            .then(|| t!("newThread.nameOnly", agent = start_label(self.start)).to_string());

        let machine_chip = chip("nt-machine", icon(machine_icon), machine_name, self.machine_menu(&me, cx), self.busy);
        let env_chip = chip(
            "nt-env",
            icon(IconName::Folder),
            env_name.unwrap_or_else(|| t!("newThread.noEnvironment").to_string()),
            self.env_menu(&me, cx),
            self.busy,
        );
        let start_chip = chip("nt-start", start_icon(self.start), start_label(self.start), self.start_menu(&me, cx), self.busy);
        let submit = Button::new("nt-submit")
            .primary()
            .small()
            .icon(IconName::ArrowUp)
            .label(t!("newThread.start").to_string())
            .loading(self.busy)
            .disabled(self.busy || self.env.is_none())
            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)));

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().child(
                    Button::new("nt-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .label(t!("common.close").to_string())
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
            .child(Textarea::new(&self.prompt).appearance(false))
            .child(
                // 下沉的一条（借窗口底色，不加边框，ADR 0011）：几个选项在左，开始在右
                div()
                    .mt_1()
                    .p(zpx(8.))
                    .rounded(radius::LG)
                    .bg(ui.app)
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(machine_chip)
                    .child(env_chip)
                    .child(start_chip)
                    .child(div().flex_1())
                    .child(submit),
            )
            // 提示与错误写在底栏下面：边打字边出现时，上面的选项按钮不跟着挪位置
            .when_some(hint, |d, hint| d.child(div().px_1().text_xs().text_color(ui.muted_foreground).child(hint)))
            .when_some(self.error.clone(), |d, e| d.child(error_line(e, cx)))
    }
}
