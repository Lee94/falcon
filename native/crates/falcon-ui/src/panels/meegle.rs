//! 右侧「飞书项目」面板（旧 React 版的 components/MeeglePanel.tsx，ADR 0010）：falcon 后端
//! 本机上 meegle CLI 的一扇窗口。
//!
//! 和 Git / 文件面板不同，它**不跟着焦点项目走**——飞书项目的登录态是整台机器（跑 falcon
//! 的那台）一份，待办也是跨空间的。三页：「待办」是 mywork 的四个列表；「空间」是选一个
//! 空间之后按关键字搜视图与工作项（CLI 没有"列出全部视图"的接口）；「固定」是粘贴飞书项目
//! 链接直接打开，以及固定过的视图 / 工作项（存在后端 SQLite）。点视图 / 工作项在面板内
//! 下钻（栈），顶上一个返回键；要看全貌就点外链去飞书。
//!
//! 状态由后端 `/api/meegle/status` 说了算：没装 CLI 给安装提示，没登录给登录卡片（device-code，
//! 后端拉起登录进程，这里给授权链接、每 2s 轮询）。业务请求撞上 409 说明登录态中途没了，
//! 重新拉一次状态让面板自己切过去（[`MeeglePanel::on_unavailable`]）。
//!
//! 列表 / 详情走前端缓存（`falcon_core::meegle_cache`）：面板卸载（关掉右侧栏）后再开立刻画出
//! 上次的结果，30s 内不打网络，之后后台静默再拉；顶栏刷新清两边。缓存按服务端配置分开——
//! 一个 App 可以同时连好几台 falcon 服务端，每台背后是不同机器上的 CLI 登录态。
//!
//! 与 React 版的差别：React 版里切页 / 下钻会卸载下面那页（回来时从缓存重画、筛选与滚动
//! 丢了）；这里各页是常驻的 Entity，返回时停在原来的位置。

mod common;
mod detail;
mod login;
mod pins;
mod space;
mod todo;
mod view;
pub(crate) mod widgets;

use std::collections::HashMap;
use std::time::Duration;

use falcon_core::meegle_cache::MeegleCache;
use falcon_core::meegle_drill::{Drill, parse_drill, serialize_drill};
use falcon_proto::{MeeglePin, MeeglePinInput, MeeglePinKind, MeegleStatus, MeegleUrlTarget, MeegleView, MeegleWorkItem};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, FontWeight, IntoElement, Render, SharedString, Task, Window, div};
use rust_i18n::t;

use self::common::{Ctx, cached, toolbar};
use self::detail::ItemDetail;
use self::login::{LoginCard, install_hint};
use self::pins::PinsSection;
use self::space::SpaceSection;
use self::todo::TodoSection;
use self::view::ViewItems;
use self::widgets::{hint, icon_button, segmented, spinning};
use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

const TAB_KEY: &str = "falcon.meegle.tab";
const DRILL_KEY: &str = "falcon.meegle.drill";
/// 登录进行中：轮询必须绕过缓存，授权一完成登录卡片就换成正文
const LOGIN_POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Todo,
    Space,
    Pins,
}

impl Tab {
    fn as_str(self) -> &'static str {
        match self {
            Tab::Todo => "todo",
            Tab::Space => "space",
            Tab::Pins => "pins",
        }
    }

    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("space") => Tab::Space,
            Some("pins") => Tab::Pins,
            _ => Tab::Todo,
        }
    }
}

/// 下钻栈每层对应的页。懒建：只有栈顶被画到时才建
enum Page {
    View(Entity<ViewItems>),
    Item(Entity<ItemDetail>),
}

/// 这台服务端的前端缓存。按服务端配置分：面板随右侧栏卸载，缓存得比它活得久。
/// 放线程局部而不是 static：浏览器里在飞的 load 不是 `Send`，进不了 static（同
/// falcon_core::meegle_cache）；面板只在 GPUI 的前台线程上建，原生上线程局部就是进程级
fn cache_for(profile_id: &str) -> MeegleCache {
    thread_local! {
        static CACHES: std::cell::RefCell<HashMap<String, MeegleCache>> = Default::default();
    }
    CACHES.with(|c| c.borrow_mut().entry(profile_id.to_string()).or_default().clone())
}

/// 沿用 React 版存在 localStorage 里的键；localStorage 天然按服务端（origin）分（浏览器版
/// 只有 `local` 一份配置，不加后缀），原生的偏好文件是全局一份，远处的服务端加上配置 id 后缀
pub(super) fn pref_key(ws: &Entity<Workspace>, base: &str, cx: &App) -> String {
    let profile = &ws.read(cx).profile;
    if profile.is_local() { base.to_string() } else { format!("{base}@{}", profile.id) }
}

pub struct MeeglePanel {
    ws: Entity<Workspace>,
    ctx: Ctx,
    status: Option<MeegleStatus>,
    status_error: Option<String>,
    checking: bool,
    tab: Tab,
    drill: Vec<Drill>,
    pages: Vec<Option<Page>>,
    /// 固定列表是 falcon 自己的数据：登录后拉一次，增删改在本地同步
    pub(super) pins: Option<Vec<MeeglePin>>,
    last_user: Option<String>,
    todo: Option<Entity<TodoSection>>,
    space: Option<Entity<SpaceSection>>,
    pins_view: Option<Entity<PinsSection>>,
    login: Option<Entity<LoginCard>>,
    login_poll: Option<Task<()>>,
}

impl MeeglePanel {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (client, profile_id) = {
            let w = ws.read(cx);
            (w.client.clone(), w.profile.id.clone())
        };
        let cache = cache_for(&profile_id);
        let ctx = Ctx { ws: ws.clone(), client, panel: cx.weak_entity(), cache: cache.clone() };
        let tab = Tab::parse(Prefs::global(cx).get(&pref_key(&ws, TAB_KEY, cx)));
        // 关掉右侧栏面板就卸了，下次打开要停回原处：栈本身就是"打开位置"
        let drill = parse_drill(Prefs::global(cx).get(&pref_key(&ws, DRILL_KEY, cx)));
        let status = cache.peek::<MeegleStatus>("status");
        let mut this = Self {
            ws,
            ctx,
            last_user: status.as_ref().and_then(|s| s.user.as_ref().map(|u| u.key.clone())),
            status,
            status_error: None,
            checking: false,
            tab,
            pages: drill.iter().map(|_| None).collect(),
            drill,
            pins: cache.peek::<Vec<MeeglePin>>("pins"),
            todo: None,
            space: None,
            pins_view: None,
            login: None,
            login_poll: None,
        };
        this.load_status(false, cx);
        if this.ready() {
            this.load_pins(cx);
        }
        this.sync_login_poll(cx);
        this
    }

    fn ready(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.installed && s.authenticated)
    }

    pub(super) fn find_pin(&self, kind: MeeglePinKind, space_key: &str, target_id: &str) -> Option<&MeeglePin> {
        self.pins.as_ref()?.iter().find(|p| p.kind == kind && p.space_key == space_key && p.target_id == target_id)
    }

    // ---------------- 状态 ----------------

    fn load_status(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let cache = self.ctx.cache.clone();
        let hit = cache.peek::<MeegleStatus>("status");
        if let Some(h) = hit.clone().filter(|_| !fresh) {
            self.set_status(h, cx);
        }
        if !fresh && hit.is_some() && !cache.stale("status", None) {
            return;
        }
        let bust = fresh || hit.is_some();
        if hit.is_none() || fresh {
            self.checking = true;
            cx.notify();
        }
        let fut = cached(&cache, "status", bust, self.ctx.client.meegle_status(fresh));
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.checking = false;
                match res {
                    Ok(st) => {
                        this.status_error = None;
                        this.set_status(st, cx);
                    }
                    Err(err) => {
                        if let Some(api) = err.downcast_ref::<falcon_client::ApiError>() {
                            this.ws.update(cx, |w, cx| w.handle_error(api, cx));
                        }
                        this.status_error = Some(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_status(&mut self, st: MeegleStatus, cx: &mut Context<Self>) {
        let was_ready = self.ready();
        let key = st.user.as_ref().map(|u| u.key.clone());
        let switched = matches!((&self.last_user, &key), (Some(last), Some(k)) if last != k);
        if key.is_some() {
            self.last_user = key;
        }
        self.status = Some(st);
        if switched {
            // 换了账号：上次停的工作项 / 视图多半已经无权打开，回根页重来
            self.ctx.cache.clear();
            self.drill.clear();
            self.pages.clear();
            self.save_drill(cx);
            self.reload_children(cx);
        }
        let ready = self.ready();
        if was_ready && !ready {
            // 登录态没了：各页的数据都作废，重新登录后从头建
            self.todo = None;
            self.space = None;
            self.pins_view = None;
            self.pages.iter_mut().for_each(|p| *p = None);
        }
        if ready {
            self.login = None;
            if !was_ready || switched {
                self.load_pins(cx);
            }
        }
        if self.status.as_ref().is_some_and(|s| !s.installed) {
            self.login = None;
        }
        self.sync_login_poll(cx);
        cx.notify();
    }

    fn sync_login_poll(&mut self, cx: &mut Context<Self>) {
        let pending = self.status.as_ref().is_some_and(|s| s.login.is_some());
        if !pending {
            self.login_poll = None;
            return;
        }
        if self.login_poll.is_some() {
            return;
        }
        self.login_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LOGIN_POLL).await;
                if this.update(cx, |p, cx| p.load_status(true, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// 业务请求撞上 409（没装 / 没登录）：面板要切到对应提示，重新问一次状态
    pub(super) fn on_unavailable(&mut self, cx: &mut Context<Self>) {
        self.ctx.cache.clear();
        self.load_status(true, cx);
    }

    /// 登录卡片发起 / 取消登录之后：换人了，查询缓存整表作废
    pub(super) fn login_changed(&mut self, cx: &mut Context<Self>) {
        self.ctx.cache.clear();
        self.load_status(true, cx);
    }

    /// 顶栏刷新：清两边的缓存，各页带 `fresh` 重拉
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.ctx.cache.clear();
        let clear = self.ctx.client.meegle_clear_cache();
        cx.spawn(async move |_, _| {
            let _ = clear.await;
        })
        .detach();
        self.reload_children(cx);
        self.load_status(true, cx);
        if self.ready() {
            self.load_pins(cx);
        }
    }

    /// React 版的 `epoch + 1`：还活着的各页带 `fresh` 重拉
    fn reload_children(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = &self.todo {
            t.update(cx, |t, cx| t.refresh(cx));
        }
        if let Some(s) = &self.space {
            s.update(cx, |s, cx| s.refresh(cx));
        }
        for page in self.pages.iter().flatten() {
            match page {
                Page::View(v) => v.update(cx, |v, cx| v.refresh(cx)),
                Page::Item(d) => d.update(cx, |d, cx| d.refresh(cx)),
            }
        }
    }

    // ---------------- 固定 ----------------

    fn load_pins(&mut self, cx: &mut Context<Self>) {
        if let Some(hit) = self.ctx.cache.peek::<Vec<MeeglePin>>("pins") {
            self.pins = Some(hit);
        }
        let fut = self.ctx.client.meegle_pins();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(list) => {
                        this.ctx.cache.write("pins", list.clone());
                        this.pins = Some(list);
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.pins = Some(Vec::new());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_pins(&mut self, next: Vec<MeeglePin>, cx: &mut Context<Self>) {
        self.ctx.cache.write("pins", next.clone());
        self.pins = Some(next);
        cx.notify();
    }

    fn pin_failed(&mut self, err: &falcon_client::ApiError, cx: &mut Context<Self>) {
        self.ws.update(cx, |w, cx| {
            w.handle_error(err, cx);
            w.toast(ToastKind::Danger, t!("toast.failed").to_string(), Some(err.message.clone()), cx);
        });
    }

    /// 已固定就取消，没固定就固定（同一个东西服务端只存一条）
    pub(super) fn toggle_pin(&mut self, input: MeeglePinInput, cx: &mut Context<Self>) {
        let existing = self.find_pin(input.kind, &input.space_key, &input.target_id).map(|p| p.id.clone());
        let client = self.ctx.client.clone();
        cx.spawn(async move |this, cx| {
            if let Some(id) = existing {
                let res = client.meegle_unpin(&id).await;
                this.update(cx, |this, cx| match res {
                    Ok(_) => {
                        let next = this.pins.clone().unwrap_or_default().into_iter().filter(|p| p.id != id).collect();
                        this.set_pins(next, cx);
                    }
                    Err(err) => this.pin_failed(&err, cx),
                })
                .ok();
            } else {
                let res = client.meegle_pin(&input).await;
                this.update(cx, |this, cx| match res {
                    Ok(created) => {
                        let mut next = this.pins.clone().unwrap_or_default();
                        if !next.iter().any(|p| p.id == created.id) {
                            next.push(created);
                        }
                        this.set_pins(next, cx);
                    }
                    Err(err) => this.pin_failed(&err, cx),
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn rename_pin(&mut self, id: String, label: String, cx: &mut Context<Self>) {
        let fut = self.ctx.client.meegle_rename_pin(&id, &label);
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| match res {
                Ok(pin) => {
                    let next = this.pins.clone().unwrap_or_default().into_iter().map(|p| if p.id == id { pin.clone() } else { p }).collect();
                    this.set_pins(next, cx);
                }
                Err(err) => this.pin_failed(&err, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn remove_pin(&mut self, id: String, cx: &mut Context<Self>) {
        let fut = self.ctx.client.meegle_unpin(&id);
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| match res {
                Ok(_) => {
                    let next = this.pins.clone().unwrap_or_default().into_iter().filter(|p| p.id != id).collect();
                    this.set_pins(next, cx);
                }
                Err(err) => this.pin_failed(&err, cx),
            })
            .ok();
        })
        .detach();
    }

    // ---------------- 下钻 ----------------

    fn save_drill(&self, cx: &mut Context<Self>) {
        let key = pref_key(&self.ws, DRILL_KEY, cx);
        cx.global_mut::<Prefs>().set(&key, serialize_drill(&self.drill));
    }

    fn push(&mut self, d: Drill, cx: &mut Context<Self>) {
        self.drill.push(d);
        self.pages.push(None);
        self.save_drill(cx);
        cx.notify();
    }

    pub(super) fn pop(&mut self, cx: &mut Context<Self>) {
        self.drill.pop();
        self.pages.pop();
        self.save_drill(cx);
        cx.notify();
    }

    pub(super) fn open_item(&mut self, item: &MeegleWorkItem, cx: &mut Context<Self>) {
        self.push(
            Drill::Item {
                space_key: item.space_key.clone(),
                space_name: item.space_name.clone(),
                type_key: Some(item.type_key.clone()).filter(|k| !k.is_empty()),
                url: item.url.clone(),
                id: item.id.clone(),
                title: item.name.clone(),
            },
            cx,
        );
    }

    pub(super) fn open_view(&mut self, space_key: &str, view: &MeegleView, space_name: Option<String>, cx: &mut Context<Self>) {
        self.push(
            Drill::View {
                space_key: space_key.to_string(),
                space_name,
                type_key: Some(view.type_key.clone()).filter(|k| !k.is_empty()),
                url: None,
                view_id: view.id.clone(),
                label: view.name.clone(),
                multi: false,
                type_name: Some(view.type_name.clone()).filter(|k| !k.is_empty()),
            },
            cx,
        );
    }

    pub(super) fn open_pin(&mut self, pin: &MeeglePin, cx: &mut Context<Self>) {
        let d = if pin.kind == MeeglePinKind::WorkItem {
            Drill::Item {
                space_key: pin.space_key.clone(),
                space_name: pin.space_name.clone(),
                type_key: pin.type_key.clone(),
                url: pin.url.clone(),
                id: pin.target_id.clone(),
                title: pin.label.clone(),
            }
        } else {
            Drill::View {
                space_key: pin.space_key.clone(),
                space_name: pin.space_name.clone(),
                type_key: pin.type_key.clone(),
                url: pin.url.clone(),
                view_id: pin.target_id.clone(),
                label: pin.label.clone(),
                multi: pin.kind == MeeglePinKind::MultiProjectView,
                type_name: None,
            }
        };
        self.push(d, cx);
    }

    /// 粘贴的飞书项目链接：后端解析成三类目标之一，直接下钻。任务的结果是给输入框显示的错误
    pub(super) fn open_url(&mut self, url: String, cx: &mut Context<Self>) -> Task<Option<String>> {
        let fut = self.ctx.client.meegle_resolve_url(&url);
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| match res {
                Ok(target) => {
                    let d = match target {
                        MeegleUrlTarget::WorkItem { space_key, space_name, type_key, id, url } => Drill::Item {
                            space_key,
                            space_name,
                            type_key: Some(type_key).filter(|k| !k.is_empty()),
                            url: Some(url),
                            id,
                            title: String::new(),
                        },
                        MeegleUrlTarget::View { space_key, space_name, view_id, type_key, url } => Drill::View {
                            space_key,
                            space_name,
                            type_key,
                            url: Some(url),
                            label: t!("meegle.viewLabel", id = view_id).to_string(),
                            view_id,
                            multi: false,
                            type_name: None,
                        },
                        MeegleUrlTarget::MultiProjectView { space_key, space_name, view_id, url } => Drill::View {
                            space_key,
                            space_name,
                            type_key: None,
                            url: Some(url),
                            label: t!("meegle.multiViewLabel", id = view_id).to_string(),
                            view_id,
                            multi: true,
                            type_name: None,
                        },
                    };
                    this.push(d, cx);
                    None
                }
                Err(err) => {
                    this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    if err.is_conflict() {
                        this.on_unavailable(cx);
                    }
                    Some(err.message.clone())
                }
            })
            .unwrap_or(None)
        })
    }

    fn switch_tab(&mut self, next: Tab, cx: &mut Context<Self>) {
        self.tab = next;
        self.drill.clear();
        self.pages.clear();
        self.save_drill(cx);
        let key = pref_key(&self.ws, TAB_KEY, cx);
        cx.global_mut::<Prefs>().set(&key, next.as_str().to_string());
        cx.notify();
    }

    // ---------------- 画 ----------------

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let user = self.status.as_ref().and_then(|s| s.user.clone());
        div()
            .h(zpx(34.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr(zpx(6.))
            .border_b_1()
            .border_color(ui.border)
            .child(icon(IconName::ListTodo).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).font_weight(FontWeight::MEDIUM).child(t!("meegle.title").to_string()))
            .when_some(user, |d, user| {
                // 头像走飞书 CDN（经 http.rs 装给 GPUI 的外链客户端取），没有或取不到时用 React 版的
                // "图裂了"兜底：首字圆点
                let title: SharedString =
                    user.email.as_ref().map(|e| format!("{} · {e}", user.name)).unwrap_or_else(|| user.name.clone()).into();
                let initial: String = user.name.chars().take(1).collect();
                let (dot_bg, dot_fg) = (ui.accent, ui.accent_foreground);
                let dot = move || -> AnyElement {
                    div()
                        .flex_none()
                        .size(zpx(16.))
                        .rounded_full()
                        .bg(dot_bg)
                        .text_color(dot_fg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(zpx(9.))
                        .child(initial.clone())
                        .into_any_element()
                };
                let avatar = match user.avatar_url.clone().filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
                    Some(url) => {
                        use gpui_kit::StyledImage as _;
                        gpui_kit::img(SharedString::from(url))
                            .flex_none()
                            .size(zpx(16.))
                            .rounded_full()
                            .with_fallback(dot.clone())
                            .into_any_element()
                    }
                    None => dot(),
                };
                d.child(
                    div()
                        .id("meegle-user")
                        .flex()
                        .min_w_0()
                        .max_w(zpx(112.))
                        .items_center()
                        .gap_1()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                        .child(avatar)
                        .child(div().min_w_0().truncate().child(user.name.clone())),
                )
            })
            .child(icon_button(
                "meegle-refresh",
                if self.checking {
                    spinning(IconName::RefreshCw, 14.).into_any_element()
                } else {
                    icon(IconName::RefreshCw).size(zpx(14.)).into_any_element()
                },
                t!("meegle.refresh").to_string(),
                24.,
                self.checking,
                cx,
                cx.listener(|this, _, _, cx| this.refresh(cx)),
            ))
    }

    fn page_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let top = self.drill.last()?.clone();
        let ix = self.drill.len() - 1;
        if self.pages.len() < self.drill.len() {
            self.pages.resize_with(self.drill.len(), || None);
        }
        if self.pages[ix].is_none() {
            let ctx = self.ctx.clone();
            self.pages[ix] = Some(match &top {
                Drill::View { .. } => Page::View(cx.new(|cx| ViewItems::new(ctx, top.clone(), window, cx))),
                Drill::Item { .. } => Page::Item(cx.new(|cx| ItemDetail::new(ctx, top.clone(), cx))),
            });
        }
        Some(match self.pages[ix].as_ref()? {
            Page::View(v) => v.clone().into_any_element(),
            Page::Item(d) => d.clone().into_any_element(),
        })
    }

    fn tab_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let ctx = self.ctx.clone();
        let section: AnyElement = match self.tab {
            Tab::Todo => self.todo.get_or_insert_with(|| cx.new(|cx| TodoSection::new(ctx, window, cx))).clone().into_any_element(),
            Tab::Space => self.space.get_or_insert_with(|| cx.new(|cx| SpaceSection::new(ctx, window, cx))).clone().into_any_element(),
            Tab::Pins => self.pins_view.get_or_insert_with(|| cx.new(|cx| PinsSection::new(ctx, window, cx))).clone().into_any_element(),
        };
        let this = cx.weak_entity();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar(cx).child(segmented(
                "meegle-tab",
                self.tab,
                vec![
                    (Tab::Todo, t!("meegle.tabTodo").to_string()),
                    (Tab::Space, t!("meegle.tabSpace").to_string()),
                    (Tab::Pins, t!("meegle.tabPins").to_string()),
                ],
                move |tab, _, cx| {
                    let _ = this.update(cx, |p, cx| p.switch_tab(tab, cx));
                },
                cx,
            )))
            .child(section)
            .into_any_element()
    }
}

impl Render for MeeglePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let header = self.header(cx).into_any_element();
        let status = self.status.clone();
        let body: AnyElement = match &status {
            None => {
                let text = match &self.status_error {
                    Some(err) => t!("native.meegle.labelled", label = t!("meegle.statusFailed"), value = err).to_string(),
                    None => t!("meegle.loading").to_string(),
                };
                hint(text, cx).into_any_element()
            }
            Some(st) if !st.installed => {
                let this = cx.weak_entity();
                install_hint(st.bin.clone(), self.checking, cx, move |_, cx| {
                    let _ = this.update(cx, |p, cx| p.load_status(true, cx));
                })
                .into_any_element()
            }
            Some(st) if !st.authenticated => {
                let ctx = self.ctx.clone();
                let st = st.clone();
                self.login.get_or_insert_with(|| cx.new(|cx| LoginCard::new(ctx, &st, window, cx))).clone().into_any_element()
            }
            Some(_) => match self.page_view(window, cx) {
                Some(page) => page,
                None => self.tab_view(window, cx),
            },
        };
        let expires = status
            .as_ref()
            .filter(|s| s.installed && s.authenticated)
            .and_then(|s| s.expires_in_minutes)
            .filter(|m| *m <= 15.0);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(ui.foreground)
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .when_some(expires, |d, minutes| {
                d.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(ui.border)
                        .text_size(zpx(11.))
                        .text_color(ui.warning)
                        .child(t!("meegle.expires", minutes = minutes.round() as i64).to_string()),
                )
            })
    }
}
