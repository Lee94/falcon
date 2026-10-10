//! `falcon service <install|uninstall|start|stop|restart|status>` 的执行部分：写服务配置、
//! 调 launchctl / systemctl、打印结果。纯函数（判定、渲染、路径）在 service.rs。移植自
//! `packages/server/src/service.ts` 的 `runServiceCli` / `launchd` / `systemd` / `relaunchJob`。
//!
//! Rust 版恒为单文件二进制（Node 版的 isSea() 恒真）：install 时先把自己拷到
//! `<dataDir>/bin/falcon` 再写配置，进程名就叫 falcon，升级也只是覆盖同一路径。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};

use crate::config::{ServerConfig, home_dir, is_loopback, parse_args};
use crate::service::{
    SYSTEMD_NAME, ServiceCli, install_service_binary, launchd_domain, launchd_plist, launchd_plist_path,
    launchd_status_report, launchd_target, parse_service_cli, service_bin_path, systemctl_argv, systemd_unit,
    systemd_unit_path,
};

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
            let res = if cfg!(target_os = "macos") {
                launchd(cmd, server_args)
            } else if cfg!(target_os = "linux") {
                systemd(cmd, server_args)
            } else {
                Err(anyhow::anyhow!("service 子命令不支持 {}", crate::sessions::login_env::node_platform()))
            };
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

fn server_config(server_args: &[String]) -> anyhow::Result<ServerConfig> {
    parse_args(server_args, |k| std::env::var(k).ok())
}

/// 被守护进程的启动命令：先把自己落到固定名 `<dataDir>/bin/falcon`，再拼上启动参数
fn install_program_arguments(server_args: &[String]) -> anyhow::Result<Vec<String>> {
    let config = server_config(server_args)?;
    let exe = std::env::current_exe().context("找不到自身的可执行文件")?;
    let bin = install_service_binary(&exe, &service_bin_path(&config.data_dir))
        .with_context(|| format!("拷贝程序到 {} 失败", service_bin_path(&config.data_dir).display()))?;
    Ok(std::iter::once(bin.to_string_lossy().into_owned()).chain(server_args.iter().cloned()).collect())
}

/// 跑一条命令拿 stdout。失败时 can_fail 返回空串，否则报错（Node 版 execFileSync 抛出）
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

/// 绑非 localhost 且尚未设密码时服务会启动即退，被 init 反复拉起——提前把话说明白
fn warn_if_non_loopback(server_args: &[String]) -> anyhow::Result<ServerConfig> {
    let config = server_config(server_args)?;
    if !is_loopback(&config.host) {
        println!(
            "注意：绑定 {}（非 localhost）要求已设置访问密码，否则服务会反复启动失败。\n如未设置，请先在 localhost 启动并通过界面设置密码（数据目录需一致）。",
            config.host
        );
    }
    Ok(config)
}

// ---- macOS: launchd（用户级 LaunchAgent）----

/// bootout 是异步的：job 尚在卸载时立刻 bootstrap 会报 "Bootstrap failed: 5:
/// Input/output error"。先等旧 job 真正消失，bootstrap 再留少量重试兜底。
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

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

fn launchd(cmd: &str, server_args: &[String]) -> anyhow::Result<i32> {
    let plist_path = launchd_plist_path(home_dir());
    let domain = launchd_domain(uid());
    let target = launchd_target(&domain);

    match cmd {
        "install" => {
            let config = warn_if_non_loopback(server_args)?;
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

fn systemctl(root_mode: bool, args: &[&str], can_fail: bool) -> anyhow::Result<String> {
    let argv = systemctl_argv(root_mode, args);
    let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    run(&argv[0], &rest, can_fail)
}

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
            let config = warn_if_non_loopback(server_args)?;
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
