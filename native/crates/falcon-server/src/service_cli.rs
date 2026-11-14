//! `falcon service <install|uninstall|start|stop|restart|status>` 的执行部分：写服务配置、
//! 调 launchctl / systemctl、打印结果。纯函数（判定、渲染、路径）在 service.rs。移植自
//! `packages/server/src/service.ts` 的 `runServiceCli` / `launchd` / `systemd` / `relaunchJob`。
//!
//! Rust 版恒为单文件二进制（Node 版的 isSea() 恒真）：install 时先把自己拷到
//! `<dataDir>/bin/falcon` 再写配置，进程名就叫 falcon，升级也只是覆盖同一路径。

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
use anyhow::{Context, bail};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::config::home_dir;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
use crate::config::{ServerConfig, parse_args};
use crate::service::{ServiceCli, parse_service_cli};
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
use crate::service::{install_service_binary, service_bin_path};
#[cfg(target_os = "macos")]
use crate::service::{launchd_domain, launchd_plist, launchd_plist_path, launchd_status_report, launchd_target};
#[cfg(target_os = "linux")]
use crate::service::{SYSTEMD_NAME, systemctl_argv, systemd_unit, systemd_unit_path};

/// 跑 `falcon service …`，返回进程退出码。argv 是 `service` 之后的部分
pub fn run_service_cli(argv: &[String]) -> i32 {
    match parse_service_cli(argv) {
        ServiceCli::Usage { text, exit_code } => {
            println!("{text}");
            exit_code
        }
        ServiceCli::Error(msg) => {
            eprintln!("{msg}");
            1
        }
        ServiceCli::Run { cmd, server_args } => {
            let res = dispatch(cmd, server_args);
            match res {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e:#}");
                    1
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn dispatch(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    launchd(cmd, server_args)
}

#[cfg(target_os = "linux")]
fn dispatch(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    systemd(cmd, server_args)
}

#[cfg(windows)]
fn dispatch(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    schtasks(cmd, server_args)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn dispatch(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    let _ = (cmd, server_args);
    Err(anyhow::anyhow!("service 子命令不支持 {}", crate::sessions::login_env::node_platform()))
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn server_config(server_args: &[String]) -> anyhow::Result<ServerConfig> {
    parse_args(server_args, |k| std::env::var(k).ok())
}

/// 被守护进程的启动命令：先把自己落到固定名 `<dataDir>/bin/falcon`，再拼上启动参数
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn install_program_arguments(server_args: &[String]) -> anyhow::Result<Vec<String>> {
    let config = server_config(server_args)?;
    let exe = std::env::current_exe().context("找不到自身的可执行文件")?;
    let bin = install_service_binary(&exe, &service_bin_path(&config.data_dir))
        .with_context(|| format!("拷贝程序到 {} 失败", service_bin_path(&config.data_dir).display()))?;
    Ok(std::iter::once(bin.to_string_lossy().into_owned()).chain(server_args.iter().cloned()).collect())
}

/// 跑一条命令拿 stdout。失败时 can_fail 返回空串，否则报错（Node 版 execFileSync 抛出）
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn run(program: &str, args: &[&str], can_fail: bool) -> anyhow::Result<String> {
    let out = Command::new(program).args(args).stdin(std::process::Stdio::null()).output();
    match out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ if can_fail => Ok(String::new()),
        Ok(o) => bail!(
            "命令失败：{program} {}（{}）\n{}",
            args.join(" "),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim_end()
        ),
        Err(e) => bail!("命令起不来：{program}：{e}"),
    }
}

// ---- macOS: launchd（用户级 LaunchAgent）----

/// bootout 是异步的：job 尚在卸载时立刻 bootstrap 会报 "Bootstrap failed: 5:
/// Input/output error"。先等旧 job 真正消失，bootstrap 再留少量重试兜底。
#[cfg(target_os = "macos")]
fn relaunch_job(domain: &str, target: &str, plist_path: &Path) -> anyhow::Result<()> {
    run("launchctl", &["bootout", target], true)?;
    for _ in 0..50 {
        if run("launchctl", &["print", target], true)?.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let plist = plist_path.to_string_lossy();
    let mut attempt = 0;
    loop {
        match run("launchctl", &["bootstrap", domain, &plist], false) {
            Ok(_) => return Ok(()),
            Err(e) if attempt >= 4 => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

#[cfg(unix)]
fn uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn launchd(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    let plist_path = launchd_plist_path(home_dir());
    let domain = launchd_domain(uid());
    let target = launchd_target(&domain);

    match cmd {
        "install" => {
            let config = server_config(server_args)?;
            let log_dir = config.data_dir.join("logs");
            std::fs::create_dir_all(&log_dir)?; // launchd 不会替我们建日志目录
            let log_file = log_dir.join("falcon.log");
            let args = install_program_arguments(server_args)?;
            if let Some(parent) = plist_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&plist_path, launchd_plist(&args, &log_file.to_string_lossy()))?;
            relaunch_job(&domain, &target, &plist_path)?; // 覆盖安装：旧实例（含升级前的旧二进制）先移除
            println!(
                "已安装并启动（launchd，登录后自启，崩溃自动拉起）。\n程序: {}\n日志: {}",
                service_bin_path(&config.data_dir).display(),
                log_file.display()
            );
        }
        "uninstall" => {
            run("launchctl", &["bootout", &target], true)?;
            let _ = std::fs::remove_file(&plist_path);
            println!("已停止并移除服务。");
        }
        // 都是"按当前 plist 重新拉起"，覆盖二进制被替换过的场景
        "start" | "restart" => {
            if !plist_path.exists() {
                eprintln!("尚未安装，先执行 falcon service install");
                return Ok(1);
            }
            relaunch_job(&domain, &target, &plist_path)?;
            println!("{}", if cmd == "start" { "已启动。" } else { "已重启。" });
        }
        // KeepAlive=true 的 job 只能 bootout 才不会被拉起；plist 保留，start 可再拉起
        "stop" => {
            run("launchctl", &["bootout", &target], true)?;
            println!("已停止。");
        }
        "status" => {
            if !plist_path.exists() {
                eprintln!("未安装。");
                return Ok(1);
            }
            let out = run("launchctl", &["print", &target], true)?;
            println!("{}", launchd_status_report(&out));
        }
        _ => unreachable!("parse_service_cli 只放行认识的子命令"),
    }
    Ok(0)
}

// ---- Linux: systemd（root 装 system 级，普通用户装 user 级）----

#[cfg(target_os = "linux")]
fn systemctl(root_mode: bool, args: &[&str], can_fail: bool) -> anyhow::Result<String> {
    let argv = systemctl_argv(root_mode, args);
    let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    run(&argv[0], &rest, can_fail)
}

#[cfg(target_os = "linux")]
fn systemd(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    if !Path::new("/run/systemd/system").exists() {
        if cmd == "install" {
            // 没有 systemd（容器、alpine 等）：把 unit 打出来，交给用户接到自己的 init
            println!("未检测到 systemd。请把以下 unit 安装到你的 init 系统：\n");
            println!("{}", systemd_unit(&install_program_arguments(server_args)?, true));
            return Ok(1);
        }
        eprintln!("未检测到 systemd。");
        return Ok(1);
    }
    let root_mode = uid() == 0;
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let unit_path: PathBuf = systemd_unit_path(root_mode, xdg.as_deref(), home_dir());

    match cmd {
        "install" => {
            let config = server_config(server_args)?;
            if let Some(parent) = unit_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&unit_path, systemd_unit(&install_program_arguments(server_args)?, root_mode))?;
            systemctl(root_mode, &["daemon-reload"], false)?;
            systemctl(root_mode, &["enable", SYSTEMD_NAME], false)?;
            // 不用 enable --now：它对已在运行的服务是 no-op，升级重装时会继续跑旧二进制。
            // restart 对未运行的服务等于 start，首装/升级两个场景都对。
            systemctl(root_mode, &["restart", SYSTEMD_NAME], false)?;
            println!(
                "已安装并启动（systemd{}，开机自启，崩溃自动拉起）。\n程序: {}\n日志: journalctl {}-u {SYSTEMD_NAME} -f",
                if root_mode { "" } else { " --user" },
                service_bin_path(&config.data_dir).display(),
                if root_mode { "" } else { "--user " },
            );
            if !root_mode {
                let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
                println!("注意：user 级服务默认登出即停、开机不起，需执行一次:\n  sudo loginctl enable-linger {user}");
            }
        }
        "uninstall" => {
            systemctl(root_mode, &["disable", "--now", SYSTEMD_NAME], true)?;
            let _ = std::fs::remove_file(&unit_path);
            systemctl(root_mode, &["daemon-reload"], true)?;
            println!("已停止并移除服务。");
        }
        "start" | "stop" | "restart" => {
            systemctl(root_mode, &[cmd, SYSTEMD_NAME], false)?;
            println!(
                "{}",
                match cmd {
                    "start" => "已启动。",
                    "stop" => "已停止。",
                    _ => "已重启。",
                }
            );
        }
        "status" => {
            let out = systemctl(root_mode, &["status", "--no-pager", SYSTEMD_NAME], true)?;
            print!("{}", if out.is_empty() { "未在运行。\n".to_string() } else { out });
        }
        _ => unreachable!("parse_service_cli 只放行认识的子命令"),
    }
    Ok(0)
}

// ---- Windows: schtasks（当前用户的计划任务，纲要 1.2 的 XML；渲染与拆词在
// falcon_core::schtasks）。与 launchd 用户级 LaunchAgent 同语义：登录自启、失败重启、
// 不需要管理员。不是 SCM 的 Windows 服务——falcon 不是 service-aware 的二进制，
// 且那要管理员权限 ----

#[cfg(windows)]
fn schtasks_exec(args: &[&str], can_fail: bool) -> anyhow::Result<String> {
    run("schtasks", args, can_fail)
}

/// 已装任务 XML 里带的完整 argv（`schtasks /Query /XML`）。没装过或读不出都是 None
#[cfg(windows)]
fn existing_task_args() -> Option<Vec<String>> {
    let out = Command::new("schtasks")
        .args(["/Query", "/TN", falcon_core::schtasks::SCHTASKS_NAME, "/XML"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let xml = falcon_core::schtasks::decode_task_xml(&out.stdout)?;
    falcon_core::schtasks::schtasks_xml_actions(&xml)
}

/// `主机\用户`（或 `域\用户`）：任务只在这个用户登录时启动。whoami 拿不到是 None
#[cfg(windows)]
fn whoami() -> Option<String> {
    let out = Command::new("whoami").stdin(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(windows)]
fn task_installed() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", falcon_core::schtasks::SCHTASKS_NAME])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn schtasks(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    use falcon_core::schtasks::{SCHTASKS_NAME, schtasks_xml, utf16le_bytes};
    match cmd {
        "install" => {
            // 没带启动参数时保留已装任务的原参数（升级场景：任务 XML 里的 Command+Arguments）
            let keep: Vec<String> = if server_args.is_empty() {
                existing_task_args()
                    .map(|argv| falcon_core::service_args::parse_service_args(&argv).to_install_args())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let effective = if keep.is_empty() { server_args } else { &keep };
            let config = server_config(effective)?;
            let args = install_program_arguments(effective)?;
            let xml = schtasks_xml(&args, whoami().as_deref());
            let task_file = std::env::temp_dir().join(format!("falcon-task-{}.xml", std::process::id()));
            std::fs::write(&task_file, utf16le_bytes(&xml))?;
            let created = schtasks_exec(
                &["/Create", "/TN", SCHTASKS_NAME, "/XML", &task_file.to_string_lossy(), "/F"],
                false,
            );
            let _ = std::fs::remove_file(&task_file);
            created?;
            schtasks_exec(&["/Run", "/TN", SCHTASKS_NAME], false)?;
            println!(
                "已安装并启动（计划任务，登录后自启，崩溃自动拉起）。\n程序: {}",
                service_bin_path(&config.data_dir).display()
            );
        }
        "uninstall" => {
            schtasks_exec(&["/End", "/TN", SCHTASKS_NAME], true)?;
            schtasks_exec(&["/Delete", "/TN", SCHTASKS_NAME, "/F"], true)?;
            println!("已停止并移除服务。");
        }
        "start" | "restart" => {
            if !task_installed() {
                eprintln!("尚未安装，先执行 falcon service install");
                return Ok(1);
            }
            if cmd == "restart" {
                schtasks_exec(&["/End", "/TN", SCHTASKS_NAME], true)?;
            }
            schtasks_exec(&["/Run", "/TN", SCHTASKS_NAME], false)?;
            println!("{}", if cmd == "start" { "已启动。" } else { "已重启。" });
        }
        "stop" => {
            schtasks_exec(&["/End", "/TN", SCHTASKS_NAME], true)?;
            println!("已停止。");
        }
        "status" => {
            if !task_installed() {
                eprintln!("未安装。");
                return Ok(1);
            }
            let out = schtasks_exec(&["/Query", "/TN", SCHTASKS_NAME, "/V", "/FO", "LIST"], true)?;
            print!("{}", if out.is_empty() { "未在运行。\n".to_string() } else { format!("{out}\n") });
        }
        _ => unreachable!("parse_service_cli 只放行认识的子命令"),
    }
    Ok(0)
}
