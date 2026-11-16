//! macOS 菜单栏状态项。
//!
//! 窗口可以全部关掉，进程还留着：会话在 launchd 里，不在这个进程里。状态项上能把窗口叫回来，
//! 也能启动 / 停止 / 重启本机服务。目标对象必须自己留着——`NSMenuItem.target` 和菜单的
//! delegate 都是弱引用，没人留就会在点击时变成空指针。
//!
//! `autoenablesItems` 要关掉。开着的话，AppKit 在菜单弹出前按 `validateMenuItem:` 把我们设好的
//! 启用状态盖掉；这个目标不实现那个方法，停着的服务也会变成可以再停一次。
//!
//! 本地更新不退出旧进程（没存的文件还在），新进程再挂一个状态项。两只鸟一模一样，左边的
//! 应用菜单也都叫 Falcon。同时跑着多份时，图标旁边和左边的应用菜单写上版本号；工作区版本
//! 经常不加，版本号一样再写可执行文件的修改时间。只剩一份时把字去掉，平时菜单栏仍是一只鸟。

use std::cell::{Cell, RefCell};
use std::time::Duration;

use falcon_ui::MenuBarText;
use gpui_kit::{ActivationPolicy, AsyncApp};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSButton, NSCellImagePosition, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu,
    NSMenuDelegate, NSMenuItem, NSRunningApplication, NSStatusBar, NSStatusBarButton, NSStatusItem,
    NSVariableStatusItemLength, NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidLaunchApplicationNotification,
    NSWorkspaceDidTerminateApplicationNotification,
};
use objc2_foundation::{MainThreadMarker, NSBundle, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString};

use crate::local_service::{ControlResult, ServiceControl, ServiceControlError, ServicePresence};

const TAG_OPEN: isize = 1;
const TAG_STATUS: isize = 2;
const TAG_START: isize = 3;
const TAG_STOP: isize = 4;
const TAG_RESTART: isize = 5;
const TAG_UPDATE: isize = 6;
const TAG_QUIT: isize = 7;
const TAG_LAN: isize = 8;

enum MenuStatus {
    Querying,
    Presence(ServicePresence),
    Busy(ServiceControl),
    Failed { message: String, presence: ServicePresence },
}

struct MenuIvars {
    app: AsyncApp,
    menu: Retained<NSMenu>,
    item: RefCell<Option<Retained<NSStatusItem>>>,
    /// 一次启动 / 停止 / 重启进行中。刷新回来的旧结果不能把它盖掉
    epoch: Cell<u64>,
    refresh_id: Cell<u64>,
    /// 版本标记的延迟重算。新的一次盖掉还没到的旧结果
    identity_epoch: Cell<u64>,
    busy: Cell<bool>,
    lan_enabled: Cell<Option<bool>>,
    status: RefCell<MenuStatus>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuIvars]
    struct MenuController;

    unsafe impl NSObjectProtocol for MenuController {}

    unsafe impl NSMenuDelegate for MenuController {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.sync_update_item();
            self.refresh();
        }
    }

    impl MenuController {
        #[unsafe(method(act:))]
        fn act(&self, sender: &NSMenuItem) {
            self.on_act(sender.tag());
        }

        #[unsafe(method(peersChanged:))]
        fn peers_changed(&self, note: &NSNotification) {
            if note_is_peer(note) {
                self.schedule_identity();
            }
        }
    }
);

impl MenuController {
    fn new(mtm: MainThreadMarker, app: AsyncApp) -> Retained<Self> {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let this = Self::alloc(mtm).set_ivars(MenuIvars {
            app,
            menu,
            item: RefCell::new(None),
            epoch: Cell::new(0),
            refresh_id: Cell::new(0),
            identity_epoch: Cell::new(0),
            busy: Cell::new(false),
            lan_enabled: Cell::new(None),
            status: RefCell::new(MenuStatus::Querying),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.populate(mtm);
        let delegate = ProtocolObject::from_ref(&*this);
        this.ivars().menu.setDelegate(Some(delegate));
        this
    }

    fn populate(&self, mtm: MainThreadMarker) {
        let text = falcon_ui::menu_bar_text();
        let menu = &self.ivars().menu;
        self.add_item(mtm, menu, &text.open, TAG_OPEN, true);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(mtm, menu, &text.querying, TAG_STATUS, false);
        self.add_item(mtm, menu, &text.start, TAG_START, false);
        self.add_item(mtm, menu, &text.stop, TAG_STOP, false);
        self.add_item(mtm, menu, &text.restart, TAG_RESTART, false);
        self.add_item(mtm, menu, &text.lan_access, TAG_LAN, false);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let (update, update_enabled) = falcon_ui::app_update_menu_state();
        self.add_item(mtm, menu, &update, TAG_UPDATE, update_enabled);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(mtm, menu, &text.quit, TAG_QUIT, true);
    }

    fn add_item(&self, mtm: MainThreadMarker, menu: &NSMenu, title: &str, tag: isize, enabled: bool) {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(sel!(act:)),
                &NSString::from_str(""),
            )
        };
        // target 是弱引用。控制器由线程局部变量留着，和这个菜单一样长
        let target: &AnyObject = unsafe { &*std::ptr::from_ref(self).cast::<AnyObject>() };
        unsafe { item.setTarget(Some(target)) };
        item.setTag(tag);
        item.setEnabled(enabled);
        menu.addItem(&item);
    }

    fn on_act(&self, tag: isize) {
        match tag {
            TAG_OPEN => self.open_window(),
            TAG_START => self.control(ServiceControl::Start),
            TAG_STOP => self.control(ServiceControl::Stop),
            TAG_RESTART => self.control(ServiceControl::Restart),
            TAG_LAN => self.toggle_lan(),
            TAG_UPDATE => self.check_update(),
            TAG_QUIT => self.quit(),
            _ => {}
        }
    }

    fn open_window(&self) {
        // 没窗口时激活策略是 Accessory（不进 Dock）。先改回普通应用再开窗口，否则 GPUI 把它当
        // 无界面进程，窗口出不来。改完必须 activate，否则菜单栏要点一次 Dock 才出现
        self.ivars().app.update(|cx| {
            cx.set_activation_policy(ActivationPolicy::Regular);
            falcon_ui::present_window(cx);
            cx.activate(true);
        });
    }

    fn quit(&self) {
        self.ivars().app.update(falcon_ui::quit);
    }

    fn check_update(&self) {
        self.ivars().app.update(|cx| {
            cx.set_activation_policy(ActivationPolicy::Regular);
            falcon_ui::check_for_app_update(cx);
            cx.activate(true);
        });
        self.sync_update_item();
    }

    fn sync_update_item(&self) {
        let (title, enabled) = falcon_ui::app_update_menu_state();
        if let Some(item) = self.ivars().menu.itemWithTag(TAG_UPDATE) {
            item.setTitle(&NSString::from_str(&title));
            item.setEnabled(enabled);
        }
    }

    fn control(&self, op: ServiceControl) {
        if self.ivars().busy.get() {
            return;
        }
        self.ivars().busy.set(true);
        let epoch = self.ivars().epoch.get() + 1;
        self.ivars().epoch.set(epoch);
        *self.ivars().status.borrow_mut() = MenuStatus::Busy(op);
        self.sync_menu();
        let app = self.ivars().app.clone();
        app.spawn(async move |cx| {
            let result = cx.background_executor().spawn(async move { crate::local_service::run_control(op) }).await;
            cx.update(move |_| publish_control(epoch, result));
        })
        .detach();
    }

    fn toggle_lan(&self) {
        if self.ivars().busy.get() {
            return;
        }
        let next = self.ivars().lan_enabled.get() != Some(true);
        self.ivars().busy.set(true);
        let epoch = self.ivars().epoch.get() + 1;
        self.ivars().epoch.set(epoch);
        *self.ivars().status.borrow_mut() = MenuStatus::Busy(ServiceControl::Restart);
        self.sync_menu();
        let app = self.ivars().app.clone();
        app.spawn(async move |cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let res = crate::local_service::apply_lan_access(next);
                    let presence = crate::local_service::query_presence();
                    (res, presence)
                })
                .await;
            cx.update(move |_| publish_lan_toggle(epoch, outcome));
        })
        .detach();
    }

    fn refresh(&self) {
        if self.ivars().busy.get() {
            return;
        }
        let epoch = self.ivars().epoch.get();
        let id = self.ivars().refresh_id.get() + 1;
        self.ivars().refresh_id.set(id);
        let app = self.ivars().app.clone();
        app.spawn(async move |cx| {
            let (presence, lan) = cx
                .background_executor()
                .spawn(async {
                    (
                        crate::local_service::query_presence(),
                        crate::local_service::query_lan_access().map(|s| s.enabled),
                    )
                })
                .await;
            cx.update(move |_| publish_presence(epoch, id, presence, lan));
        })
        .detach();
    }

    fn sync_menu(&self) {
        let mtm = self.mtm();
        let text = falcon_ui::menu_bar_text();
        let ivars = self.ivars();
        let status = ivars.status.borrow();
        let label = status_label(&text, &status);
        let presence = match &*status {
            MenuStatus::Presence(presence) | MenuStatus::Failed { presence, .. } => Some(*presence),
            MenuStatus::Querying | MenuStatus::Busy(_) => None,
        };
        drop(status);
        let menu = &ivars.menu;
        if let Some(item) = menu.itemWithTag(TAG_STATUS) {
            item.setTitle(&NSString::from_str(&label));
        }
        let busy = ivars.busy.get();
        let known = presence.is_some() && !busy;
        let running = matches!(presence, Some(ServicePresence::Running { .. }));
        let installed = matches!(presence, Some(ServicePresence::Stopped | ServicePresence::Running { .. }));
        set_enabled(menu, TAG_START, known && !running);
        set_enabled(menu, TAG_STOP, known && running);
        set_enabled(menu, TAG_RESTART, known && installed);
        if let Some(item) = menu.itemWithTag(TAG_LAN) {
            let state = if ivars.lan_enabled.get() == Some(true) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            };
            item.setState(state);
            item.setEnabled(known && ivars.lan_enabled.get().is_some());
        }
        self.sync_update_item();
        let dimmed = matches!(presence, Some(ServicePresence::Stopped | ServicePresence::NotInstalled));
        if let Some(item) = ivars.item.borrow().as_ref()
            && let Some(button) = item.button(mtm)
        {
            button.setAppearsDisabled(dimmed);
        }
    }

    fn watch_peers(&self) {
        // 通知中心不留住观察者。控制器在线程局部变量里，和这个进程一样长
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let observer = as_object(self);
        unsafe {
            center.addObserver_selector_name_object(
                observer,
                sel!(peersChanged:),
                Some(NSWorkspaceDidLaunchApplicationNotification),
                None,
            );
            center.addObserver_selector_name_object(
                observer,
                sel!(peersChanged:),
                Some(NSWorkspaceDidTerminateApplicationNotification),
                None,
            );
        }
    }

    fn schedule_identity(&self) {
        self.apply_identity();
        // Launch Services 把新进程放进 runningApplications 可能晚于通知，过一会儿再算一次
        let epoch = self.ivars().identity_epoch.get().wrapping_add(1);
        self.ivars().identity_epoch.set(epoch);
        let app = self.ivars().app.clone();
        app.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            cx.update(move |_| {
                with_controller(|controller| {
                    if controller.ivars().identity_epoch.get() == epoch {
                        controller.apply_identity();
                    }
                });
            });
        })
        .detach();
    }

    fn apply_identity(&self) {
        let mtm = self.mtm();
        let mark = status_item_title(&running_copies());
        if let Some(item) = self.ivars().item.borrow().as_ref()
            && let Some(button) = item.button(mtm)
        {
            let button = as_button(&button);
            let title = if mark.is_empty() && button.image().is_none() { "F" } else { mark.as_str() };
            button.setImagePosition(NSCellImagePosition::ImageLeft);
            button.setTitle(&NSString::from_str(title));
        }
        set_app_menu_title(&app_menu_title(&mark), mtm);
    }
}

fn status_label(text: &MenuBarText, status: &MenuStatus) -> String {
    match status {
        MenuStatus::Querying => text.querying.clone(),
        MenuStatus::Busy(ServiceControl::Start) => text.busy_start.clone(),
        MenuStatus::Busy(ServiceControl::Stop) => text.busy_stop.clone(),
        MenuStatus::Busy(ServiceControl::Restart) => text.busy_restart.clone(),
        MenuStatus::Presence(ServicePresence::NotInstalled) => text.missing.clone(),
        MenuStatus::Presence(ServicePresence::Stopped) => text.stopped.clone(),
        MenuStatus::Presence(ServicePresence::Running { pid: None }) => text.running.clone(),
        MenuStatus::Presence(ServicePresence::Running { pid: Some(pid) }) => falcon_ui::menu_bar_running(*pid),
        MenuStatus::Failed { message, .. } => message.clone(),
    }
}

fn error_text(err: &ServiceControlError) -> String {
    match err {
        ServiceControlError::NoProgram => falcon_ui::menu_bar_text().no_program,
        ServiceControlError::Failed(message) => falcon_ui::menu_bar_failed(message),
    }
}

fn set_enabled(menu: &NSMenu, tag: isize, enabled: bool) {
    if let Some(item) = menu.itemWithTag(tag) {
        item.setEnabled(enabled);
    }
}

fn publish_presence(epoch: u64, id: u64, presence: ServicePresence, lan: Option<bool>) {
    with_controller(|controller| {
        let apply = {
            let ivars = controller.ivars();
            if ivars.busy.get() || ivars.epoch.get() != epoch || ivars.refresh_id.get() != id {
                false
            } else {
                ivars.lan_enabled.set(lan);
                *ivars.status.borrow_mut() = MenuStatus::Presence(presence);
                true
            }
        };
        if apply {
            controller.sync_menu();
        }
    });
}

fn publish_lan_toggle(
    epoch: u64,
    outcome: (Result<falcon_platform::LanAccessState, String>, ServicePresence),
) {
    with_controller(|controller| {
        let apply = {
            let ivars = controller.ivars();
            if ivars.epoch.get() != epoch {
                false
            } else {
                ivars.busy.set(false);
                match outcome.0 {
                    Ok(state) => {
                        ivars.lan_enabled.set(Some(state.enabled));
                        *ivars.status.borrow_mut() = MenuStatus::Presence(outcome.1);
                    }
                    Err(msg) => {
                        *ivars.status.borrow_mut() = MenuStatus::Failed {
                            message: falcon_ui::menu_bar_failed(&msg),
                            presence: outcome.1,
                        };
                    }
                }
                true
            }
        };
        if apply {
            controller.sync_menu();
        }
    });
}

fn publish_control(epoch: u64, result: ControlResult) {
    with_controller(|controller| {
        let apply = {
            let ivars = controller.ivars();
            if ivars.epoch.get() != epoch {
                false
            } else {
                ivars.busy.set(false);
                *ivars.status.borrow_mut() = match result.error {
                    None => MenuStatus::Presence(result.presence),
                    Some(err) => MenuStatus::Failed { message: error_text(&err), presence: result.presence },
                };
                true
            }
        };
        if apply {
            controller.sync_menu();
        }
    });
}

fn with_controller(f: impl FnOnce(&MenuController)) {
    CONTROLLER.with(|slot| {
        if let Some(controller) = slot.borrow().as_ref() {
            f(controller);
        }
    });
}

thread_local! {
    static CONTROLLER: RefCell<Option<Retained<MenuController>>> = RefCell::new(None);
}

pub fn installed() -> bool {
    CONTROLLER.with(|slot| slot.borrow().is_some())
}

pub fn install(cx: &gpui_kit::App) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("不在主线程上，没有菜单栏图标");
        return;
    };
    let controller = MenuController::new(mtm, cx.to_async());
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    set_icon(&item, mtm);
    item.setMenu(Some(&controller.ivars().menu));
    *controller.ivars().item.borrow_mut() = Some(item);
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
    controller.watch_peers();
    controller.schedule_identity();
    controller.refresh();
}

fn set_icon(item: &NSStatusItem, mtm: MainThreadMarker) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    let ns_button = as_button(&button);
    if let Some(image) = menu_icon() {
        // 模板图跟着菜单栏的深浅走。彩色应用图标有整块底，缩到这里会变成一个黑方块
        image.setTemplate(true);
        image.setSize(NSSize::new(18.0, 18.0));
        ns_button.setImage(Some(&image));
        ns_button.setTitle(&NSString::from_str(""));
    } else {
        ns_button.setTitle(&NSString::from_str("F"));
    }
}

fn menu_icon() -> Option<Retained<NSImage>> {
    for name in ["bird.fill", "bird"] {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            Some(&NSString::from_str("Falcon")),
        ) {
            return Some(image);
        }
    }
    None
}

/// `NSStatusBarButton` 的超类是 `NSButton`，绑定不会把超类方法带到子类上
fn as_button(button: &NSStatusBarButton) -> &NSButton {
    unsafe { &*std::ptr::from_ref(button).cast::<NSButton>() }
}

fn as_object<T: ?Sized>(obj: &T) -> &AnyObject {
    unsafe { &*std::ptr::from_ref(obj).cast::<AnyObject>() }
}

/// 左边应用菜单的名字，和 `falcon-ui` 里 `set_menus` 的第一项一致。只剩一份时改回这个
const APP_MENU_NAME: &str = "Falcon";

fn set_app_menu_title(title: &str, mtm: MainThreadMarker) {
    let Some(menu) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return;
    };
    let Some(item) = menu.itemAtIndex(0) else {
        return;
    };
    let title = NSString::from_str(title);
    item.setTitle(&title);
    if let Some(submenu) = item.submenu() {
        submenu.setTitle(&title);
    }
}

fn running_copies() -> Vec<RunningCopy> {
    let current = NSRunningApplication::currentApplication();
    let mut found_self = false;
    let mut copies = Vec::new();
    for app in peer_apps(&current) {
        let is_self = same_process(&app, &current);
        found_self |= is_self;
        copies.push(copy_of(&app, is_self));
    }
    if !found_self {
        copies.push(copy_of(&current, true));
    }
    copies
}

fn peer_apps(current: &NSRunningApplication) -> Vec<Retained<NSRunningApplication>> {
    if let Some(bundle_id) = nonempty_bundle_id(current) {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(&bundle_id));
        return (0..apps.count()).map(|index| apps.objectAtIndex(index)).collect();
    }
    // `cargo run` 没有 bundle id，按可执行文件名认同一份程序
    let name = executable_name(current);
    let apps = NSWorkspace::sharedWorkspace().runningApplications();
    (0..apps.count())
        .map(|index| apps.objectAtIndex(index))
        .filter(|app| !name.is_empty() && executable_name(app) == name)
        .collect()
}

fn copy_of(app: &NSRunningApplication, is_self: bool) -> RunningCopy {
    let version = bundle_short_version(app).unwrap_or_else(|| {
        if is_self {
            env!("CARGO_PKG_VERSION").to_string()
        } else {
            String::new()
        }
    });
    RunningCopy { version, built: executable_built(app), pid: app.processIdentifier() as i32, is_self }
}

fn bundle_short_version(app: &NSRunningApplication) -> Option<String> {
    let url = app.bundleURL()?;
    let bundle = NSBundle::bundleWithURL(&url)?;
    let value = bundle.objectForInfoDictionaryKey(&NSString::from_str("CFBundleShortVersionString"))?;
    let text = value.downcast_ref::<NSString>()?.to_string();
    let text = text.trim();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

fn executable_built(app: &NSRunningApplication) -> Option<BuiltAt> {
    let url = app.executableURL()?;
    let path = url.path()?.to_string();
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(built_at_local(i64::try_from(secs).ok()?))
}

fn built_at_local(secs: i64) -> BuiltAt {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return BuiltAt { hour: 0, minute: 0, second: 0 };
    }
    BuiltAt {
        hour: tm.tm_hour.clamp(0, 23) as u8,
        minute: tm.tm_min.clamp(0, 59) as u8,
        second: tm.tm_sec.clamp(0, 60) as u8,
    }
}

fn executable_name(app: &NSRunningApplication) -> String {
    app.executableURL()
        .and_then(|url| url.path())
        .map(|path| path.to_string())
        .and_then(|path| path.rsplit('/').next().map(str::to_string))
        .unwrap_or_default()
}

fn nonempty_bundle_id(app: &NSRunningApplication) -> Option<String> {
    let id = app.bundleIdentifier()?.to_string();
    let id = id.trim();
    if id.is_empty() { None } else { Some(id.to_string()) }
}

fn same_process(a: &NSRunningApplication, b: &NSRunningApplication) -> bool {
    let left = a.processIdentifier();
    let right = b.processIdentifier();
    left >= 0 && left == right
}

fn note_is_peer(note: &NSNotification) -> bool {
    // 读不出是哪个应用就重算一次。退出通知上 bundle id 有时已经空了
    let Some(app) = note_app(note) else {
        return true;
    };
    is_peer_app(&app)
}

fn note_app(note: &NSNotification) -> Option<Retained<NSRunningApplication>> {
    let info = note.userInfo()?;
    let key = unsafe { NSWorkspaceApplicationKey };
    info.objectForKey(as_object(key))?.downcast().ok()
}

fn is_peer_app(app: &NSRunningApplication) -> bool {
    let current = NSRunningApplication::currentApplication();
    if let Some(mine) = nonempty_bundle_id(&current) {
        return match nonempty_bundle_id(app) {
            Some(theirs) => theirs == mine,
            None => true,
        };
    }
    let mine = executable_name(&current);
    let theirs = executable_name(app);
    theirs.is_empty() || theirs == mine
}

/// 一份正在跑的 Falcon。`built` 是可执行文件修改时间的当地时分秒
struct RunningCopy {
    version: String,
    built: Option<BuiltAt>,
    pid: i32,
    is_self: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BuiltAt {
    hour: u8,
    minute: u8,
    second: u8,
}

/// 同时只有一份时返回空串，状态项保持一只鸟。多份时返回能互相区分的短标记
fn status_item_title(copies: &[RunningCopy]) -> String {
    let Some(me) = copies.iter().find(|copy| copy.is_self) else {
        return String::new();
    };
    if copies.len() < 2 {
        return String::new();
    }
    let version = version_label(me.version.trim());
    // 读不到的版本号不能当成「另一版」：否则两份都只显示自己的版本，标记又一样
    let same_version: Vec<_> = copies
        .iter()
        .filter(|copy| {
            let version = copy.version.trim();
            version.is_empty() || version == me.version.trim()
        })
        .collect();
    if same_version.len() == 1 {
        return version;
    }
    if let Some(built) = me.built {
        let minute = |stamp: BuiltAt| (stamp.hour, stamp.minute);
        let same_minute = same_version
            .iter()
            .filter(|copy| copy.built.is_some_and(|stamp| minute(stamp) == minute(built)))
            .count();
        if same_minute == 1 {
            return format!("{version} {:02}:{:02}", built.hour, built.minute);
        }
        let same_second = same_version.iter().filter(|copy| copy.built == Some(built)).count();
        if same_second == 1 {
            return format!("{version} {:02}:{:02}:{:02}", built.hour, built.minute, built.second);
        }
    }
    if me.pid >= 0 {
        return format!("{version} · {}", me.pid);
    }
    version
}

fn version_label(version: &str) -> String {
    if version.is_empty() { APP_MENU_NAME.to_string() } else { format!("v{version}") }
}

fn app_menu_title(mark: &str) -> String {
    if mark.is_empty() { APP_MENU_NAME.to_string() } else { format!("{APP_MENU_NAME} {mark}") }
}

#[cfg(test)]
mod tests {
    use super::{BuiltAt, RunningCopy, app_menu_title, status_item_title};

    fn copy(version: &str, built: Option<(u8, u8, u8)>, pid: i32, is_self: bool) -> RunningCopy {
        RunningCopy {
            version: version.to_string(),
            built: built.map(|(hour, minute, second)| BuiltAt { hour, minute, second }),
            pid,
            is_self,
        }
    }

    #[test]
    fn single_copy_has_no_title() {
        assert_eq!(status_item_title(&[copy("0.3.19", Some((9, 52, 1)), 1, true)]), "");
    }

    #[test]
    fn different_versions_show_the_version() {
        let copies = [
            copy("0.3.18", Some((9, 0, 0)), 1, true),
            copy("0.3.19", Some((10, 0, 0)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.18");
    }

    #[test]
    fn same_version_shows_build_clock() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 1, true),
            copy("0.3.19", Some((16, 40, 2)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52");
        let flipped = [
            copy("0.3.19", Some((9, 52, 1)), 1, false),
            copy("0.3.19", Some((16, 40, 2)), 2, true),
        ];
        assert_eq!(status_item_title(&flipped), "v0.3.19 16:40");
    }

    #[test]
    fn unreadable_peer_version_uses_the_clock() {
        let copies = [copy("0.3.19", Some((9, 52, 1)), 1, true), copy("", Some((16, 40, 2)), 2, false)];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52");
    }

    #[test]
    fn same_minute_shows_seconds() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 1, true),
            copy("0.3.19", Some((9, 52, 40)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52:01");
    }

    #[test]
    fn same_build_shows_pid() {
        let copies = [copy("0.3.19", Some((9, 52, 1)), 11, true), copy("0.3.19", Some((9, 52, 1)), 22, false)];
        assert_eq!(status_item_title(&copies), "v0.3.19 · 11");
    }

    #[test]
    fn missing_build_time_uses_pid() {
        let copies = [copy("0.3.19", None, 11, true), copy("0.3.19", None, 22, false)];
        assert_eq!(status_item_title(&copies), "v0.3.19 · 11");
    }

    #[test]
    fn no_self_is_blank() {
        assert_eq!(status_item_title(&[copy("0.3.19", None, 11, false)]), "");
    }

    #[test]
    fn app_menu_uses_the_same_mark() {
        assert_eq!(app_menu_title(""), "Falcon");
        assert_eq!(app_menu_title("v0.3.19 09:52"), "Falcon v0.3.19 09:52");
    }
}
