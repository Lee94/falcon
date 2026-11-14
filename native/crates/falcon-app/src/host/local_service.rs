//! 本机服务托管：`native/xtask/assets/macos-pkg/launcher.sh` 的原生版。
//!
//! Falcon.app 的 Resources 里带着单文件服务程序（`cargo xtask server` 的产物）。启动时：
//! 1. `falcon service install`——幂等，兼作升级（它会把 Resources/falcon 挪进
//!    `<dataDir>/bin/falcon` 并注册 / 重启用户级 LaunchAgent）。**带上已安装服务原来的
//!    `--host` / `--port` / `--data-dir`**（从 LaunchAgent plist 里读）：不带参数的 install
//!    会写默认配置，装过自定义端口 / 数据目录的人一升级，服务就换了个家，会话全看不见了
//!    （pkg 的 postinstall 与 launcher.sh 用同目录的 `service-args.sh` 做同一件事）；
//! 2. 等端口起来：launchd 拉起服务要时间，端口起来之前 TCP 连会被拒。
//!    "本机"配置连的端口同样取自 plist（见 [`installed_service_args`]）。
//!
//! 会话活在 launchd 服务里，不在 App 进程里——**退出 App = 所有窗口 Detach**，永远不会
//! Terminate。关掉窗口不等于退出：macOS 上菜单栏还在，启动 / 停止 / 重启走同一套
//! `falcon service` 子命令（停止必须 bootout，否则 KeepAlive 会把进程拉起来）。
//!
//! Windows 的本机服务是计划任务（schtasks）：安装包把 falcon-server.exe 装在客户端同目录，
//! `falcon service` 在 Windows 上注册登录自启 + 失败重启的任务。客户端读回启动参数的方式与
//! macOS 对应：`schtasks /Query /XML` 的任务 XML 里取 `<Command>` 与 `<Arguments>`。

use std::time::Duration;

use falcon_platform::{LanAccessState, LocalService, ServiceControl, ServicePresence, ServiceStatus};

/// 捆绑的服务程序：`Falcon.app/Contents/Resources/falcon`
#[cfg(target_os = "macos")]
fn bundled_server() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let contents = exe.parent()?.parent()?;
    let bin = contents.join("Resources").join("falcon");
    bin.is_file().then_some(bin)
}

/// 捆绑的服务程序：Windows 安装包里与 Falcon.exe 同目录的 falcon-server.exe
#[cfg(windows)]
fn bundled_server() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?.join("falcon-server.exe");
    bin.is_file().then_some(bin)
}

/// `~/Library/LaunchAgents/com.falcon.server.plist`。没家目录时是 None
#[cfg(target_os = "macos")]
fn launch_agent_plist() -> Option<std::path::PathBuf> {
    Some(
        directories::BaseDirs::new()?
            .home_dir()
            .join("Library/LaunchAgents/com.falcon.server.plist"),
    )
}

#[cfg(target_os = "macos")]
fn agent_installed() -> bool {
    launch_agent_plist().is_some_and(|path| path.is_file())
}

/// plist 里的 `ProgramArguments`。没装过、读不出来都是 None。用系统自带的 plutil 转 JSON，
/// 不为读一个 plist 加依赖
fn program_arguments() -> Option<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        let plist = launch_agent_plist()?;
        if !plist.is_file() {
            return None;
        }
        let out = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments", "json", "-o", "-"])
            .arg(&plist)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("schtasks")
            .args(["/Query", "/TN", falcon_core::schtasks::schtasks_name().as_str(), "/XML"])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let xml = falcon_core::schtasks::decode_task_xml(&out.stdout)?;
        falcon_core::schtasks::schtasks_xml_actions(&xml)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}

/// 已经注册过的用户级 LaunchAgent（服务端 `service.rs` 的 `com.falcon.server`）的启动参数。
/// 没装过、读不出来都是 None
pub fn installed_service_args() -> Option<falcon_core::service_args::ServiceArgs> {
    program_arguments().map(|argv| falcon_core::service_args::parse_service_args(&argv))
}

/// LaunchAgent 正在跑的那份服务程序（`<dataDir>/bin/falcon`）。更新前若
/// 它带了 `uchg`，`service install` 换不掉它
#[cfg(target_os = "macos")]
pub(crate) fn installed_program_path() -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(program_arguments()?.into_iter().next()?);
    path.is_file().then_some(path)
}

/// 菜单栏能拿来执行 `falcon service …` 的程序。App 包里捆的那份优先（和启动时 install
/// 用同一份）；开发构建没有包，就用 LaunchAgent 正在指着的那份
#[cfg(any(target_os = "macos", windows))]
fn service_program() -> Option<std::path::PathBuf> {
    if let Some(bin) = bundled_server() {
        return Some(bin);
    }
    let path = std::path::PathBuf::from(program_arguments()?.into_iter().next()?);
    path.is_file().then_some(path)
}

/// [`LocalService`] 的原生实现。macOS 上捆着服务程序就先 `service install`，各平台都等端口
pub struct BundledService;

impl LocalService for BundledService {
    fn ensure_running(&self, url: &str) -> ServiceStatus {
        ensure_running(url)
    }

    fn presence(&self) -> Option<ServicePresence> {
        #[cfg(target_os = "macos")]
        {
            Some(query_presence())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn control(&self, op: ServiceControl) -> Result<ServicePresence, String> {
        #[cfg(target_os = "macos")]
        {
            let res = run_control(op);
            match res.error {
                None => Ok(res.presence),
                Some(ServiceControlError::NoProgram) => Err("找不到本机服务程序".into()),
                Some(ServiceControlError::Failed(msg)) => Err(msg),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = op;
            Err("当前平台不支持托管本机服务".into())
        }
    }

    fn lan_access(&self) -> Option<LanAccessState> {
        query_lan_access()
    }

    fn set_lan_access(&self, enabled: bool) -> Result<LanAccessState, String> {
        apply_lan_access(enabled)
    }
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) fn query_lan_access() -> Option<LanAccessState> {
    service_program()?;
    let args = installed_service_args().unwrap_or_default();
    let port = args.port.unwrap_or(falcon_core::service_args::DEFAULT_PORT);
    Some(LanAccessState {
        enabled: args.is_lan_exposed(),
        port,
        urls: lan_ipv4_urls(port),
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn query_lan_access() -> Option<LanAccessState> {
    None
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) fn apply_lan_access(enabled: bool) -> Result<LanAccessState, String> {
    let next = installed_service_args().unwrap_or_default().with_lan_exposed(enabled);
    let mut argv = vec!["service".to_string(), "install".to_string()];
    argv.extend(next.to_install_args());
    run_service(&argv).map_err(|err| match err {
        ServiceControlError::NoProgram => "找不到本机服务程序".to_string(),
        ServiceControlError::Failed(msg) => msg,
    })?;
    let _ = wait_for_port(&next.local_url());
    query_lan_access().ok_or_else(|| "无法读取本机服务状态".to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn apply_lan_access(_enabled: bool) -> Result<LanAccessState, String> {
    Err("当前平台不支持托管本机服务".to_string())
}

/// 枚举本机物理/局域网网卡的 IPv4 基址，排除回环、链路本地、TUN 隧道与 Clash Fake-IP 段
#[cfg(target_os = "macos")]
fn lan_ipv4_urls(port: u16) -> Vec<String> {
    let mut primary = Vec::new();
    let mut secondary = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 || ifap.is_null() {
            return primary;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            let flags = ifa.ifa_flags as i32;
            if (flags & libc::IFF_UP) == 0 || (flags & libc::IFF_LOOPBACK) != 0 {
                continue;
            }
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let name = if ifa.ifa_name.is_null() {
                ""
            } else {
                std::ffi::CStr::from_ptr(ifa.ifa_name).to_str().unwrap_or("")
            };
            if name.starts_with("utun")
                || name.starts_with("awdl")
                || name.starts_with("llw")
                || name.starts_with("gif")
                || name.starts_with("stf")
                || name.starts_with("anpi")
            {
                continue;
            }
            let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
            let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            let octets = ip.octets();
            if ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
            {
                continue;
            }
            let url = format!("http://{ip}:{port}");
            if name.starts_with("en") {
                if !primary.contains(&url) && !secondary.contains(&url) {
                    primary.push(url);
                }
            } else if !primary.contains(&url) && !secondary.contains(&url) {
                secondary.push(url);
            }
        }
        libc::freeifaddrs(ifap);
    }
    primary.extend(secondary);
    primary
}

/// Windows 上枚举本机物理 / 局域网网卡的 IPv4，走 PowerShell 的 Get-NetIPAddress；
/// 回环 / 链路本地 / Clash Fake-IP 段与虚拟交换机在 Rust 侧滤掉，以太网 / Wi-Fi 排前面
#[cfg(windows)]
fn lan_ipv4_urls(port: u16) -> Vec<String> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object { \"$($_.IPAddress)|$($_.InterfaceAlias)\" }",
        ])
        .stdin(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut primary = Vec::new();
    let mut secondary = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((ip, alias)) = line.trim().split_once('|') else {
            continue;
        };
        let Ok(ip) = ip.parse::<std::net::Ipv4Addr>() else {
            continue;
        };
        let o = ip.octets();
        if ip.is_loopback()
            || ip.is_link_local()
            || ip.is_unspecified()
            || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        {
            continue;
        }
        if alias.starts_with("vEthernet") || alias.starts_with("Teredo") || alias.starts_with("isatap") {
            continue;
        }
        let url = format!("http://{ip}:{port}");
        if alias.starts_with("Ethernet") || alias.starts_with("WLAN") || alias.starts_with("Wi-Fi") {
            if !primary.contains(&url) && !secondary.contains(&url) {
                primary.push(url);
            }
        } else if !primary.contains(&url) && !secondary.contains(&url) {
            secondary.push(url);
        }
    }
    primary.extend(secondary);
    primary
}

/// 阻塞执行，放在后台线程里调。
fn ensure_running(url: &str) -> ServiceStatus {
    #[cfg(any(target_os = "macos", windows))]
    if let Some(bin) = bundled_server() {
        let keep = installed_service_args().map(|a| a.to_install_args()).unwrap_or_default();
        match std::process::Command::new(&bin).args(["service", "install"]).args(&keep).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                log::warn!("service install 失败：{stderr}");
            }
            Err(err) => log::warn!("拉不起捆绑的服务程序 {}：{err}", bin.display()),
        }
    }
    wait_for_port(url)
}

fn wait_for_port(url: &str) -> ServiceStatus {
    let Ok(parsed) = url::Url::parse(url) else {
        return ServiceStatus::Unreachable(format!("基址无效：{url}"));
    };
    let host = parsed.host_str().unwrap_or("127.0.0.1").to_string();
    let port = parsed.port_or_known_default().unwrap_or(4923);
    let mut last_err = String::new();
    // 与 launcher.sh 同一个节奏：最多 40 × 0.5s
    for _ in 0..40 {
        match std::net::TcpStream::connect_timeout(
            &format!("{host}:{port}")
                .parse()
                .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
            Duration::from_millis(500),
        ) {
            Ok(_) => return ServiceStatus::Ready,
            Err(err) => last_err = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    ServiceStatus::Unreachable(last_err)
}

#[cfg(any(target_os = "macos", windows))]
#[derive(Debug)]
pub(crate) enum ServiceControlError {
    /// 没有可执行的 falcon：开发构建不在 .app 里，也没有已安装的二进制
    NoProgram,
    Failed(String),
}

#[cfg(target_os = "macos")]
pub(crate) struct ControlResult {
    pub presence: ServicePresence,
    pub error: Option<ServiceControlError>,
}

/// `falcon service` 实际要跑的 argv。没安装时启动和重启都落成 install，并带上原来的
/// `--host` / `--port` / `--data-dir`（空则仍走服务端默认值）。停止一个没安装的服务是空操作
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum ControlPlan {
    Nothing,
    Run(Vec<String>),
}

#[cfg(any(target_os = "macos", test))]
fn control_plan(op: ServiceControl, installed: bool, keep: &[String]) -> ControlPlan {
    match (op, installed) {
        (ServiceControl::Stop, false) => ControlPlan::Nothing,
        (ServiceControl::Stop, true) => ControlPlan::Run(vec!["service".into(), "stop".into()]),
        (ServiceControl::Start, true) => ControlPlan::Run(vec!["service".into(), "start".into()]),
        (ServiceControl::Restart, true) => ControlPlan::Run(vec!["service".into(), "restart".into()]),
        (ServiceControl::Start | ServiceControl::Restart, false) => {
            let mut args = vec!["service".into(), "install".into()];
            args.extend(keep.iter().cloned());
            ControlPlan::Run(args)
        }
    }
}

/// `launchctl print` 的 stdout。只认每个键第一次出现：输出里子段落会把 `state` 再写一遍
#[cfg(any(target_os = "macos", test))]
fn presence_from_launchd(installed: bool, print_output: &str) -> ServicePresence {
    if !installed {
        return ServicePresence::NotInstalled;
    }
    let mut state = None;
    let mut pid = None;
    for line in print_output.lines() {
        if state.is_none() {
            state = field_value(line, "state").map(str::to_string);
        }
        if pid.is_none() {
            pid = field_value(line, "pid").and_then(|value| value.parse().ok());
        }
    }
    if state.as_deref() == Some("running") {
        ServicePresence::Running { pid }
    } else {
        ServicePresence::Stopped
    }
}

/// `state = running` / `pid=42`。键名必须到边界，避免 `stated` 被当成 `state`
#[cfg(any(target_os = "macos", test))]
fn field_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(key)?;
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '=') {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    (!rest.is_empty()).then_some(rest)
}

#[cfg(target_os = "macos")]
fn launchd_target() -> String {
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}/com.falcon.server")
}

/// 问 launchd 现在什么状态。阻塞，放在后台执行器上调
#[cfg(target_os = "macos")]
pub(crate) fn query_presence() -> ServicePresence {
    let installed = agent_installed();
    if !installed {
        return ServicePresence::NotInstalled;
    }
    // print 在 job 已经被 bootout 时失败，stdout 是空的，那就是「装了但没在跑」
    let output = std::process::Command::new("launchctl")
        .args(["print", &launchd_target()])
        .output();
    let stdout = match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(err) => {
            log::warn!("launchctl print 起不来：{err}");
            String::new()
        }
    };
    presence_from_launchd(true, &stdout)
}

#[cfg(target_os = "macos")]
pub(crate) fn run_control(op: ServiceControl) -> ControlResult {
    let installed = agent_installed();
    let keep = installed_service_args().map(|args| args.to_install_args()).unwrap_or_default();
    let ran = match control_plan(op, installed, &keep) {
        ControlPlan::Nothing => Ok(()),
        ControlPlan::Run(args) => run_service(&args),
    };
    let error = ran.err();
    // bootout 是异步的：stop 的命令已经返回，print 仍可能看见旧的 running。等它真正消失
    let presence = if error.is_none() && matches!(op, ServiceControl::Stop) {
        wait_until_not_running()
    } else {
        query_presence()
    };
    if let Some(err) = &error {
        log::warn!("本机服务 {op:?} 失败：{err:?}");
    }
    ControlResult { presence, error }
}

#[cfg(target_os = "macos")]
fn wait_until_not_running() -> ServicePresence {
    let mut last = query_presence();
    for _ in 0..20 {
        if !matches!(last, ServicePresence::Running { .. }) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
        last = query_presence();
    }
    last
}

#[cfg(any(target_os = "macos", windows))]
fn run_service(args: &[String]) -> Result<(), ServiceControlError> {
    let program = service_program().ok_or(ServiceControlError::NoProgram)?;
    let out = std::process::Command::new(&program)
        .args(args)
        .output()
        .map_err(|err| ServiceControlError::Failed(err.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let raw = if !stderr.trim().is_empty() { stderr } else { stdout };
    let msg = brief(raw.trim()).unwrap_or_else(|| format!("exit {}", out.status));
    Err(ServiceControlError::Failed(msg))
}

/// 菜单项按这段文案撑开宽度，只留一行、并且限制长度。空串返回 None，调用方改用退出码
#[cfg(any(target_os = "macos", windows, test))]
const MENU_TEXT_LIMIT: usize = 32;

#[cfg(any(target_os = "macos", windows, test))]
fn brief(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?.trim();
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(MENU_TEXT_LIMIT).collect();
    if chars.next().is_some() {
        Some(format!("{head}…"))
    } else {
        Some(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 装没装过服务都成立：读得出来就必须能拼出一个 http 基址
    #[test]
    fn installed_args_yield_a_url() {
        if let Some(args) = installed_service_args() {
            let url = args.local_url();
            assert!(url.starts_with("http://"));
        }
    }

    #[test]
    fn presence_reads_the_first_state_and_pid() {
        let text = "\
    state = running
    pid = 4242
        state = waiting
        pid = 1
";
        assert_eq!(
            presence_from_launchd(true, text),
            ServicePresence::Running { pid: Some(4242) }
        );
        assert_eq!(presence_from_launchd(true, ""), ServicePresence::Stopped);
        assert_eq!(
            presence_from_launchd(false, "state = running\npid = 1"),
            ServicePresence::NotInstalled
        );
        assert_eq!(
            presence_from_launchd(true, "stated = running\n"),
            ServicePresence::Stopped
        );
    }

    #[test]
    fn control_plan_installs_with_the_previous_args_when_missing() {
        let keep = ["--port".to_string(), "6789".to_string()];
        assert_eq!(
            control_plan(ServiceControl::Start, false, &keep),
            ControlPlan::Run(vec![
                "service".into(),
                "install".into(),
                "--port".into(),
                "6789".into()
            ])
        );
        assert_eq!(
            control_plan(ServiceControl::Restart, false, &[]),
            ControlPlan::Run(vec!["service".into(), "install".into()])
        );
        assert_eq!(
            control_plan(ServiceControl::Start, true, &keep),
            ControlPlan::Run(vec!["service".into(), "start".into()])
        );
        assert_eq!(
            control_plan(ServiceControl::Restart, true, &keep),
            ControlPlan::Run(vec!["service".into(), "restart".into()])
        );
        assert_eq!(control_plan(ServiceControl::Stop, false, &keep), ControlPlan::Nothing);
        assert_eq!(
            control_plan(ServiceControl::Stop, true, &keep),
            ControlPlan::Run(vec!["service".into(), "stop".into()])
        );
    }

    #[test]
    fn brief_keeps_one_short_line() {
        assert_eq!(brief("").as_deref(), None);
        assert_eq!(brief("  \n尚未安装\n更多").as_deref(), Some("尚未安装"));
        let long = "a".repeat(MENU_TEXT_LIMIT + 1);
        let got = brief(&long).unwrap();
        assert!(got.ends_with('…'));
        assert_eq!(got.chars().count(), MENU_TEXT_LIMIT + 1);
    }
}
