//! Windows 通知区域（托盘）图标：macOS 菜单栏状态项（menu_bar.rs）的对应物。
//!
//! 窗口可以全部关掉，进程还留着：会话在计划任务拉起的服务里，不在这个进程里。左键点图标把
//! 窗口叫回来，右键菜单与 macOS 同一套：打开、服务状态、启动 / 停止 / 重启、局域网访问、
//! 检查更新、退出。GPUI 在 Windows 上关掉最后一扇窗口不会自己退出（`close_one_window` 的
//! 返回值没人用），关窗即退出是 main.rs 里自己调的 quit，托盘在时就不调。
//!
//! 消息靠一扇不显示的顶层窗口收。GPUI 的主循环是 `GetMessageW(None)`，本线程所有窗口的消息都
//! 会分派到这里。不能用 `HWND_MESSAGE` 的纯消息窗口：它收不到 `TaskbarCreated` 广播，资源管理器
//! 重启后图标就丢了。
//!
//! 窗口过程里**不直接借 App**：它可能在 GPUI 自己的模态循环里被调到（`CreateProcessW`
//! 也会泵消息，gpui-fast-windows 的 `restart` 注释里写过），那时 AppCell 正被借着，
//! `AsyncApp::update` 会双重借用 panic。动作一律经 `AsyncApp::spawn` 推到前台执行器，
//! 它只往队列里投任务、不借 App。同理 `TrackPopupMenuEx` 是模态的、期间前台任务照常跑，
//! 弹菜单时不能借着 `TRAY`。

use std::cell::RefCell;
use std::time::{Duration, Instant};

use falcon_ui::MenuBarText;
use gpui_kit::AsyncApp;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT,
    NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos, GetSystemMetrics,
    HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, LoadIconW, LoadImageW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, PostMessageW, RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SetForegroundWindow,
    SetMenuDefaultItem, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_APP,
    WM_CONTEXTMENU, WM_MOUSEMOVE, WM_NULL, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::local_service::{ControlResult, ServiceControl, ServiceControlError, ServicePresence};

const CMD_OPEN: usize = 1;
const CMD_START: usize = 3;
const CMD_STOP: usize = 4;
const CMD_RESTART: usize = 5;
const CMD_UPDATE: usize = 6;
const CMD_QUIT: usize = 7;
const CMD_LAN: usize = 8;
/// 状态行只是显示，不可点。给它一个 id 是为了 `AppendMenuW` 有东西可填
const CMD_STATUS: usize = 2;

/// 图标的回调消息。`NOTIFYICON_VERSION_4` 下 lParam 低字是事件，wParam 是锚点的屏幕坐标
const WM_TRAY: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
/// 键盘选中图标（Enter / 空格）。windows-sys 没导出这个组合值，按 shellapi.h 拼
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
/// 鼠标划过图标时顺手刷新状态，免得右键时菜单上还是很久以前的。划过的消息很密，限个频
const HOVER_REFRESH: Duration = Duration::from_secs(3);

enum MenuStatus {
    Querying,
    Presence(ServicePresence),
    Busy(ServiceControl),
    Failed { message: String, presence: ServicePresence },
}

struct Tray {
    app: AsyncApp,
    hwnd: HWND,
    icon: HICON,
    /// 资源管理器重启后广播的消息号，收到就把图标重新挂上
    taskbar_created: u32,
    /// 一次启动 / 停止 / 重启进行中。刷新回来的旧结果不能把它盖掉
    epoch: u64,
    refresh_id: u64,
    last_refresh: Option<Instant>,
    busy: bool,
    lan_enabled: Option<bool>,
    status: MenuStatus,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

pub fn installed() -> bool {
    TRAY.with(|slot| slot.borrow().is_some())
}

pub fn install(cx: &mut gpui_kit::App) {
    let Some((hwnd, taskbar_created)) = create_window() else {
        log::warn!("建不出托盘的消息窗口，没有托盘图标");
        return;
    };
    let icon = load_icon();
    if !add_icon(hwnd, icon) {
        log::warn!("Shell_NotifyIconW 添加托盘图标失败");
        return;
    }
    TRAY.with(|slot| {
        *slot.borrow_mut() = Some(Tray {
            app: cx.to_async(),
            hwnd,
            icon,
            taskbar_created,
            epoch: 0,
            refresh_id: 0,
            last_refresh: None,
            busy: false,
            lan_enabled: None,
            status: MenuStatus::Querying,
        })
    });
    // 进程退出不销毁窗口，图标会一直留在通知区域，等鼠标划过才消失。退出前主动摘掉
    cx.on_app_quit(|_| {
        remove_icon();
        async {}
    })
    .detach();
    sync_tip();
    refresh();
}

fn create_window() -> Option<(HWND, u32)> {
    let class = wide("FalconTray");
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        // 同一进程里注册第二次会失败（ERROR_CLASS_ALREADY_EXISTS），不影响建窗口，不看返回值
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        Some((hwnd, taskbar_created))
    }
}

/// exe 资源段里 ID 1 的图标（build.rs 的 windows_resources，debug 构建也有），按小图标尺寸取，
/// 高 DPI 下系统给的尺寸跟着变。取不到退回系统的默认应用图标
fn load_icon() -> HICON {
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let icon = LoadImageW(
            hinstance,
            1 as windows_sys::core::PCWSTR,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        );
        if icon.is_null() { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) } else { icon as HICON }
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn add_icon(hwnd: HWND, icon: HICON) -> bool {
    let mut data = icon_data(hwnd);
    // VERSION_4 下系统默认不显示 szTip（要应用自己画浮窗），NIF_SHOWTIP 让它照旧显示
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    fill(&mut data.szTip, "Falcon");
    unsafe {
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            return false;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
    true
}

fn remove_icon() {
    let hwnd = TRAY.with(|slot| slot.borrow().as_ref().map(|tray| tray.hwnd));
    if let Some(hwnd) = hwnd {
        let data = icon_data(hwnd);
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        let event = (lparam & 0xFFFF) as u32;
        match event {
            NIN_SELECT | NIN_KEYSELECT => defer(|cx| open_window(cx)),
            WM_CONTEXTMENU => {
                // 锚点：鼠标右键时是光标位置，键盘（Shift+F10 / 菜单键）时是图标位置
                let x = (wparam & 0xFFFF) as u16 as i16 as i32;
                let y = ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                show_menu(hwnd, x, y);
            }
            WM_MOUSEMOVE => refresh_if_stale(),
            _ => {}
        }
        return 0;
    }
    let taskbar_created = TRAY.with(|slot| slot.borrow().as_ref().map(|tray| (tray.taskbar_created, tray.icon)));
    if let Some((taskbar_created, icon)) = taskbar_created
        && taskbar_created != 0
        && msg == taskbar_created
    {
        add_icon(hwnd, icon);
        sync_tip();
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    refresh();
    let Some(menu) = build_menu() else {
        return;
    };
    let (x, y) = if x == 0 && y == 0 { cursor_pos() } else { (x, y) };
    unsafe {
        // 菜单的拥有者不是前台窗口时，点菜单外面菜单不会收起（MSDN TrackPopupMenu 备注）。
        // 收起后补一条空消息，否则第二次右键菜单会一闪就没
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            x,
            y,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if cmd > 0 {
            on_command(cmd as usize);
        }
    }
}

fn cursor_pos() -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

/// 每次右键现建一份，按当时的状态定启用 / 勾选。弹出期间刷新回来的结果只更新提示文字，
/// 菜单要下次打开才变，和 macOS 上菜单开着时改项目的效果差不多
fn build_menu() -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HMENU> {
    let text = falcon_ui::menu_bar_text();
    let (update_title, update_enabled) = falcon_ui::app_update_menu_state();
    let view = TRAY.with(|slot| {
        let slot = slot.borrow();
        let tray = slot.as_ref()?;
        Some(MenuView::of(tray, &text))
    })?;
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let item = |id: usize, title: &str, enabled: bool, checked: bool| {
            let mut flags = MF_STRING;
            if !enabled {
                flags |= MF_GRAYED;
            }
            if checked {
                flags |= MF_CHECKED;
            }
            AppendMenuW(menu, flags, id, wide(&menu_title(title)).as_ptr());
        };
        let separator = || {
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        };
        item(CMD_OPEN, &text.open, true, false);
        // 左键点图标做的就是这一项，加粗标出来（Windows 托盘菜单的惯例）
        SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
        separator();
        item(CMD_STATUS, &view.label, false, false);
        item(CMD_START, &text.start, view.can_start, false);
        item(CMD_STOP, &text.stop, view.can_stop, false);
        item(CMD_RESTART, &text.restart, view.can_restart, false);
        item(CMD_LAN, &text.lan_access, view.can_toggle_lan, view.lan_on);
        separator();
        item(CMD_UPDATE, &update_title, update_enabled, false);
        separator();
        item(CMD_QUIT, &text.quit, true, false);
        Some(menu)
    }
}

struct MenuView {
    label: String,
    can_start: bool,
    can_stop: bool,
    can_restart: bool,
    can_toggle_lan: bool,
    lan_on: bool,
}

impl MenuView {
    fn of(tray: &Tray, text: &MenuBarText) -> Self {
        let presence = match &tray.status {
            MenuStatus::Presence(presence) | MenuStatus::Failed { presence, .. } => Some(*presence),
            MenuStatus::Querying | MenuStatus::Busy(_) => None,
        };
        let known = presence.is_some() && !tray.busy;
        let running = matches!(presence, Some(ServicePresence::Running { .. }));
        let installed = matches!(presence, Some(ServicePresence::Stopped | ServicePresence::Running { .. }));
        MenuView {
            label: status_label(text, &tray.status),
            can_start: known && !running,
            can_stop: known && running,
            can_restart: known && installed,
            can_toggle_lan: known && tray.lan_enabled.is_some(),
            lan_on: tray.lan_enabled == Some(true),
        }
    }
}

fn on_command(cmd: usize) {
    match cmd {
        CMD_OPEN => defer(|cx| open_window(cx)),
        CMD_START => control(ServiceControl::Start),
        CMD_STOP => control(ServiceControl::Stop),
        CMD_RESTART => control(ServiceControl::Restart),
        CMD_LAN => toggle_lan(),
        CMD_UPDATE => defer(|cx| {
            falcon_ui::check_for_app_update(cx);
            cx.activate(true);
        }),
        CMD_QUIT => defer(falcon_ui::quit),
        _ => {}
    }
}

fn open_window(cx: &mut gpui_kit::App) {
    falcon_ui::present_window(cx);
    cx.activate(true);
}

/// 把动作推到前台执行器上，等这一轮消息处理完、App 没被借着时再做
fn defer(f: impl FnOnce(&mut gpui_kit::App) + 'static) {
    let Some(app) = TRAY.with(|slot| slot.borrow().as_ref().map(|tray| tray.app.clone())) else {
        return;
    };
    app.spawn(async move |cx| cx.update(f)).detach();
}

/// 开始一次启动 / 停止 / 重启 / 切换局域网：置忙、换代，返回新的代号。已经在忙就是 None
fn begin_busy(op: ServiceControl) -> Option<(AsyncApp, u64)> {
    let started = TRAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let tray = slot.as_mut()?;
        if tray.busy {
            return None;
        }
        tray.busy = true;
        tray.epoch += 1;
        tray.status = MenuStatus::Busy(op);
        Some((tray.app.clone(), tray.epoch))
    });
    if started.is_some() {
        sync_tip();
    }
    started
}

fn control(op: ServiceControl) {
    let Some((app, epoch)) = begin_busy(op) else {
        return;
    };
    app.spawn(async move |cx| {
        let result = cx.background_executor().spawn(async move { crate::local_service::run_control(op) }).await;
        publish_control(epoch, result);
    })
    .detach();
}

fn toggle_lan() {
    let next = TRAY.with(|slot| slot.borrow().as_ref().map(|tray| tray.lan_enabled != Some(true)));
    let Some(next) = next else {
        return;
    };
    let Some((app, epoch)) = begin_busy(ServiceControl::Restart) else {
        return;
    };
    app.spawn(async move |cx| {
        let outcome = cx
            .background_executor()
            .spawn(async move {
                let res = crate::local_service::apply_lan_access(next);
                let presence = crate::local_service::query_presence();
                (res, presence)
            })
            .await;
        publish_lan_toggle(epoch, outcome);
    })
    .detach();
}

fn refresh_if_stale() {
    let stale = TRAY.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|tray| tray.last_refresh.is_none_or(|at| at.elapsed() >= HOVER_REFRESH))
    });
    if stale {
        refresh();
    }
}

fn refresh() {
    let started = TRAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let tray = slot.as_mut()?;
        if tray.busy {
            return None;
        }
        tray.refresh_id += 1;
        tray.last_refresh = Some(Instant::now());
        Some((tray.app.clone(), tray.epoch, tray.refresh_id))
    });
    let Some((app, epoch, id)) = started else {
        return;
    };
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
        publish_presence(epoch, id, presence, lan);
    })
    .detach();
}

fn publish_presence(epoch: u64, id: u64, presence: ServicePresence, lan: Option<bool>) {
    update_tray(|tray| {
        if tray.busy || tray.epoch != epoch || tray.refresh_id != id {
            return false;
        }
        tray.lan_enabled = lan;
        tray.status = MenuStatus::Presence(presence);
        true
    });
}

fn publish_lan_toggle(epoch: u64, outcome: (Result<falcon_platform::LanAccessState, String>, ServicePresence)) {
    update_tray(|tray| {
        if tray.epoch != epoch {
            return false;
        }
        tray.busy = false;
        match outcome.0 {
            Ok(state) => {
                tray.lan_enabled = Some(state.enabled);
                tray.status = MenuStatus::Presence(outcome.1);
            }
            Err(msg) => {
                tray.status = MenuStatus::Failed { message: falcon_ui::menu_bar_failed(&msg), presence: outcome.1 };
            }
        }
        true
    });
}

fn publish_control(epoch: u64, result: ControlResult) {
    update_tray(|tray| {
        if tray.epoch != epoch {
            return false;
        }
        tray.busy = false;
        tray.status = match result.error {
            None => MenuStatus::Presence(result.presence),
            Some(err) => MenuStatus::Failed { message: error_text(&err), presence: result.presence },
        };
        true
    });
}

/// 改状态，改了就同步提示文字。借用在同步提示之前放掉
fn update_tray(f: impl FnOnce(&mut Tray) -> bool) {
    let changed = TRAY.with(|slot| slot.borrow_mut().as_mut().is_some_and(f));
    if changed {
        sync_tip();
    }
}

/// 鼠标停在图标上的提示：应用名 + 版本 + 服务状态。同时跑着几份时，靠版本号认是哪一份
fn sync_tip() {
    let text = falcon_ui::menu_bar_text();
    let Some((hwnd, label)) =
        TRAY.with(|slot| slot.borrow().as_ref().map(|tray| (tray.hwnd, status_label(&text, &tray.status))))
    else {
        return;
    };
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_TIP | NIF_SHOWTIP;
    fill(&mut data.szTip, &tip_text(env!("CARGO_PKG_VERSION"), &label));
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn tip_text(version: &str, status: &str) -> String {
    format!("Falcon v{version}\n{status}")
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

/// Win32 菜单把 `&` 当助记符前缀，文案里的 `&`（比如错误信息）要写成 `&&` 才原样显示
fn menu_title(title: &str) -> String {
    title.replace('&', "&&")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 写进定长的 UTF-16 缓冲，超长截断并留出结尾的 0。不在代理对中间截断
fn fill(buf: &mut [u16], s: &str) {
    let max = buf.len().saturating_sub(1);
    let mut len = 0;
    for ch in s.chars() {
        let n = ch.len_utf16();
        if len + n > max {
            break;
        }
        ch.encode_utf16(&mut buf[len..len + n]);
        len += n;
    }
    buf[len..].fill(0);
}

#[cfg(test)]
mod tests {
    use super::{fill, menu_title, tip_text};

    #[test]
    fn fill_truncates_and_terminates() {
        let mut buf = [1u16; 4];
        fill(&mut buf, "abcdef");
        assert_eq!(buf, [b'a' as u16, b'b' as u16, b'c' as u16, 0]);
        let mut buf = [1u16; 4];
        fill(&mut buf, "a");
        assert_eq!(buf, [b'a' as u16, 0, 0, 0]);
    }

    #[test]
    fn fill_keeps_surrogate_pairs_whole() {
        // 🐦 占两个 UTF-16 单元，只剩一格时整个丢掉，不留半个代理
        let mut buf = [1u16; 3];
        fill(&mut buf, "a🐦");
        assert_eq!(buf, [b'a' as u16, 0, 0]);
    }

    #[test]
    fn menu_title_escapes_ampersand() {
        assert_eq!(menu_title("a & b"), "a && b");
    }

    #[test]
    fn tip_has_version_and_status() {
        assert_eq!(tip_text("0.4.28", "运行中"), "Falcon v0.4.28\n运行中");
    }
}
