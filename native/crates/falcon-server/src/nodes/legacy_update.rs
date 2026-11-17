//! 没有节点接口的旧服务只能通过已上传的新二进制升级。命令在远端本机运行，
//! 因而停掉旧服务不会切断执行者；普通 `node ensure` 仍然绝不进入这条路径。
//!
//! 旧版没有升级维护协议，无法复用服务端在线升级的交接。这里在停服后备份数据库，
//! 候选程序一旦启动就不再自动恢复旧库：它没有维护入口，可能已经开始接受业务写入。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};

use super::bootstrap::{self, Descriptor, LegacyServer};

const STOP_TIMEOUT: Duration = Duration::from_secs(60);
const READY_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug)]
struct LaunchUncertain(String);
impl std::fmt::Display for LaunchUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for LaunchUncertain {}

#[derive(Debug)]
struct StopConflict(String);
impl std::fmt::Display for StopConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for StopConflict {}

pub(super) fn run_cli(argv: &[String]) -> anyhow::Result<Descriptor> {
    ensure!(
        argv.len() == 2 && argv[0] == "--data-dir" && !argv[1].is_empty(),
        "{} 用法: falcon node upgrade --data-dir <root>",
        std::str::from_utf8(bootstrap::LEGACY_UPGRADE_CAPABILITY_MARKER).expect("能力标识是 ASCII")
    );
    let root = PathBuf::from(&argv[1]);
    ensure!(root.is_absolute(), "node upgrade 的数据目录必须是绝对路径");
    std::fs::create_dir_all(&root)?;
    let root = bootstrap::canonical_root(&root)?;
    // 与 ensure 共用跨进程锁。两个入口同时升级时，第二个看到新节点后直接复用，
    // 不会把刚启动的候选当成旧服务再停一次。
    let _lock = bootstrap::lock(&root, "node-ensure.lock", READY_TIMEOUT + Duration::from_secs(5))?;
    if let Some(node) = bootstrap::live_descriptor(&root)? {
        return Ok(node);
    }
    let servers = bootstrap::legacy_servers(&root);
    let [old] = servers.as_slice() else {
        bail!("目标数据目录检测到 {} 个旧服务进程，拒绝自动替换；请先确认只保留一个服务", servers.len());
    };
    let old = old.clone();
    ensure!(
        !old.unknown_root,
        "已有 Falcon 进程 {}，但无法确定它的数据目录，拒绝升级",
        old.pid
    );
    ensure!(bootstrap::process_alive(old.pid), "旧版 Falcon 服务进程 {} 已退出", old.pid);
    ensure!(!old.args.is_empty(), "无法读取旧服务 {} 的启动参数", old.pid);
    let old_exe = old.exe.canonicalize().context("无法解析旧服务程序路径")?;
    let new_exe = std::env::current_exe()?.canonicalize()?;
    ensure!(old_exe != new_exe, "新旧服务程序路径相同，拒绝替换正在执行的文件");
    let managed = managed_process(&old)?;
    ensure!(bootstrap::respawn_helpers(old.pid).is_empty(), "旧服务已有 respawn 重启帮手，等待重启完成后再升级");
    if !managed {
        ensure!(!old.environ.is_empty(), "无法读取非托管旧服务的环境，拒绝改用 SSH 会话环境启动");
    }
    let id = hex::encode(rand::random::<[u8; 16]>());
    let work = root.join("legacy-updates").join(id);
    std::fs::create_dir_all(work.parent().expect("升级备份目录有父目录"))?;
    #[allow(unused_mut)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&work)?;

    if let Err(error) = stop_server(&root, &old, managed) {
        if error.downcast_ref::<StopConflict>().is_some() {
            // 已经观察到停服检查中的帮手/服务冲突，随后两次扫描之间状态可能继续变化。
            // 这里绝不再手工启动旧服务；帮手若已 spawn 后退出，再启一份就会重复。
            return Err(error).context("停服现场确认有冲突，未备份或替换文件，也不会再次启动旧服务");
        }
        if !bootstrap::legacy_servers(&root).is_empty() {
            return Err(error).context("旧服务仍在运行或已由重启帮手恢复，未备份或替换文件");
        }
        if !bootstrap::respawn_helpers(old.pid).is_empty() {
            return Err(error).context("旧服务的 respawn 帮手仍在执行重启，未备份或替换文件");
        }
        let restart = start_server(&root, &old, &old_exe, managed).err();
        return Err(error).context(format!(
            "停服结果未确认；{}",
            restart.map_or("旧服务已按原参数恢复启动".into(), |e| format!("旧服务恢复启动失败：{e:#}"))
        ));
    }
    if let Err(error) = backup_databases(&root, &work) {
        let restart = start_server(&root, &old, &old_exe, managed).err();
        return Err(error).context(format!(
            "数据库备份失败；{}",
            restart.map_or("旧服务已按原参数重新启动".into(), |e| format!("旧服务恢复启动失败：{e:#}"))
        ));
    }
    let backup_exe = match replace_executable(&new_exe, &old_exe, &work) {
        Ok(path) => path,
        Err(error) => {
            let restart = start_server(&root, &old, &old_exe, managed).err();
            return Err(error).context(format!(
                "替换程序前失败；{}",
                restart.map_or("旧服务已按原参数重新启动".into(), |e| format!("旧服务恢复启动失败：{e:#}"))
            ));
        }
    };

    let candidate = match start_server(&root, &old, &old_exe, managed) {
        Ok(pid) => pid,
        Err(error) if managed || error.downcast_ref::<LaunchUncertain>().is_some() => {
            // systemctl/launchctl/schtasks 返回失败不能证明候选没有创建进程。
            // 此时不能换回旧程序，更不能恢复旧库；保留现场让健康查询继续判断。
            bail!("新服务启动命令结果未确认：{error:#}；保留候选程序与数据库，升级前备份在 {}", work.display());
        }
        Err(error) => {
            // 普通 Command::spawn 返回错误时尚未启动候选，可以安全换回原程序。
            let restore = restore_executable(&old_exe, &backup_exe);
            let restart = if restore.is_ok() { start_server(&root, &old, &old_exe, managed).err() } else { None };
            bail!(
                "新服务启动失败：{error:#}；程序恢复：{}；旧服务启动：{}",
                restore.map_or_else(|e| format!("失败：{e:#}"), |_| "已恢复".into()),
                restart.map_or("已执行".into(), |e| format!("失败：{e:#}"))
            );
        }
    };
    match wait_ready(&root, old.pid, candidate) {
        Ok(node) => {
            ensure!(node.version == env!("CARGO_PKG_VERSION"), "新节点版本与升级程序不符");
            let _ = std::fs::remove_file(&backup_exe);
            Ok(node)
        }
        Err(error) => {
            // 不做旧库回滚：旧版没有维护入口，无法证明候选尚未接受写入。
            // 旧程序副本与同目录换出的文件、停服后的数据库备份都保留供人工恢复。
            bail!(
                "新节点未通过健康检查：{error:#}；保留新程序与当前数据库，不覆盖已接受的写入。升级前备份在 {}",
                work.display()
            )
        }
    }
}

fn backup_databases(root: &Path, work: &Path) -> anyhow::Result<Vec<String>> {
    let mut databases = Vec::new();
    for name in ["falcon.db", "mojito.db"] {
        let source = root.join(name);
        if !source.exists() {
            continue;
        }
        let backup = work.join(format!("{name}.backup"));
        let connection = rusqlite::Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.backup(rusqlite::MAIN_DB, &backup, None::<fn(rusqlite::backup::Progress)>)?;
        File::open(&backup)?.sync_all()?;
        databases.push(name.into());
    }
    ensure!(!databases.is_empty(), "没有找到待备份的数据库");
    Ok(databases)
}

/// 把旧程序换到同目录备份名，再把已校验的新程序改名到原路径。系统服务配置、
/// 文件所有者和启动 argv 都继续指向原路径；Windows 必须先停服，运行中的 exe 不能改名。
fn replace_executable(new_exe: &Path, old_exe: &Path, work: &Path) -> anyhow::Result<PathBuf> {
    let parent = old_exe.parent().context("旧服务程序没有所在目录")?;
    let old_name = old_exe.file_name().context("旧服务程序没有文件名")?.to_string_lossy();
    let id = hex::encode(rand::random::<[u8; 8]>());
    let staged = parent.join(format!(".{old_name}.new-{id}"));
    let backup = parent.join(format!(".{old_name}.old-{id}"));
    std::fs::copy(old_exe, work.join("falcon.old")).context("备份旧服务程序失败")?;
    std::fs::copy(new_exe, &staged).context("暂存新服务程序失败")?;
    std::fs::set_permissions(&staged, std::fs::metadata(old_exe)?.permissions())?;
    File::open(&staged)?.sync_all()?;
    if let Err(error) = std::fs::rename(old_exe, &backup) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).context("换出旧服务程序失败");
    }
    if let Err(error) = std::fs::rename(&staged, old_exe) {
        let restore = std::fs::rename(&backup, old_exe);
        let _ = std::fs::remove_file(&staged);
        return match restore {
            Ok(()) => Err(error).context("换入新服务程序失败，旧程序已恢复原路径"),
            Err(restore) => Err(error).context(format!("换入新服务程序失败，且旧程序恢复失败：{restore}；旧程序备份在 {}", backup.display())),
        };
    }
    Ok(backup)
}

fn restore_executable(exe: &Path, backup: &Path) -> anyhow::Result<()> {
    if exe.exists() {
        let failed = exe.with_file_name(format!(".falcon-failed-{}", hex::encode(rand::random::<[u8; 8]>())));
        std::fs::rename(exe, failed)?;
    }
    std::fs::rename(backup, exe)?;
    Ok(())
}

fn wait_ready(root: &Path, old_pid: u32, candidate: Option<u32>) -> anyhow::Result<Descriptor> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_error = None;
    loop {
        match bootstrap::live_descriptor(root) {
            Ok(Some(node)) if node.pid != old_pid => return Ok(node),
            Ok(_) => {}
            Err(error) => last_error = Some(format!("{error:#}")),
        }
        if candidate.is_some_and(|pid| !bootstrap::process_alive(pid)) {
            bail!("新服务进程已退出：{}", bootstrap::log_tail(root));
        }
        ensure!(
            Instant::now() < deadline,
            "等待新节点就绪超时：{}；{}",
            last_error.as_deref().unwrap_or("尚未发布节点描述"),
            bootstrap::log_tail(root)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop_server(root: &Path, server: &LegacyServer, managed: bool) -> anyhow::Result<()> {
    if !bootstrap::respawn_helpers(server.pid).is_empty() {
        return Err(anyhow::Error::new(StopConflict("旧服务已有 respawn 重启帮手".into())));
    }
    if managed {
        ensure!(crate::service_cli::run_service_cli(&["stop".into()]) == 0, "停止原系统服务失败");
    } else {
        terminate(server.pid)?;
    }
    wait_dead(server.pid)?;
    // respawn 默认每 150ms 检查一次旧 pid。等待后再同时扫帮手与服务进程，
    // 不能仅凭旧 pid 消失就认定没有另一份旧服务正在打开数据库。
    std::thread::sleep(Duration::from_millis(350));
    if !bootstrap::respawn_helpers(server.pid).is_empty() {
        return Err(anyhow::Error::new(StopConflict("旧服务的 respawn 帮手可能重新启动服务".into())));
    }
    if !bootstrap::legacy_servers(root).is_empty() {
        return Err(anyhow::Error::new(StopConflict("旧服务停止后检测到同目录服务进程".into())));
    }
    Ok(())
}

fn start_server(root: &Path, server: &LegacyServer, exe: &Path, managed: bool) -> anyhow::Result<Option<u32>> {
    if managed {
        ensure!(crate::service_cli::run_service_cli(&["start".into()]) == 0, "启动系统服务失败");
        return Ok(None);
    }
    let mut command = Command::new(exe);
    command.args(server.args.iter().skip(1));
    if !server.environ.is_empty() {
        command.env_clear();
        for item in &server.environ {
            if let Some((key, value)) = item.to_str().and_then(|s| s.split_once('=')) {
                command.env(key, value);
            }
        }
    }
    if let Some(cwd) = &server.cwd {
        command.current_dir(cwd);
    }
    let log_path = root.join("node.log");
    let log = OpenOptions::new().create(true).append(true).open(log_path)?;
    command
        .env_remove("FALCON_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    detach(&mut command);
    match command.spawn() {
        Ok(child) => Ok(Some(child.id())),
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(5) => {
            // WMI 不能传原环境；把连接配置固定为 argv，避免退回 SSH/WMI 的默认端口。
            let args = wmi_server_args(server, root);
            let cwd = server.cwd.as_deref().unwrap_or(root);
            match bootstrap::wmi_launch_with_args(exe, &args, cwd) {
                Ok(pid) => Ok(Some(pid)),
                Err(error) => Err(anyhow::Error::new(LaunchUncertain(format!(
                    "WMI 启动结果未确认，候选可能已经创建：{error:#}"
                )))),
            }
        }
        Err(error) => Err(error).context("按原参数启动 Falcon 服务失败"),
    }
}

#[cfg(any(windows, test))]
fn wmi_server_args(server: &LegacyServer, root: &Path) -> Vec<String> {
    let argument = |name: &str| {
        server.args.windows(2).rev().find(|pair| pair[0] == name).map(|pair| pair[1].to_string_lossy().into_owned())
    };
    let environment = |name: &str| {
        server.environ.iter().filter_map(|value| value.to_str()).find_map(|value| {
            let (key, value) = value.split_once('=')?;
            key.eq_ignore_ascii_case(name).then(|| value.to_string())
        })
    };
    let host = argument("--host").or_else(|| environment("FALCON_HOST")).or_else(|| environment("MOJITO_HOST")).unwrap_or_else(|| "127.0.0.1".into());
    let port = argument("--port").or_else(|| environment("FALCON_PORT")).or_else(|| environment("MOJITO_PORT")).unwrap_or_else(|| "4923".into());
    let mut args: Vec<String> = server.args.iter().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect();
    args.extend(["--host".into(), host, "--port".into(), port, "--data-dir".into(), root.to_string_lossy().into_owned()]);
    args
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0100_0000);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}

#[cfg(unix)]
fn terminate(pid: u32) -> anyhow::Result<()> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(error).context(format!("终止旧服务进程 {pid} 失败"));
    }
    Ok(())
}

#[cfg(windows)]
fn terminate(pid: u32) -> anyhow::Result<()> {
    if !bootstrap::process_alive(pid) {
        return Ok(());
    }
    ensure!(
        Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status()?.success(),
        "终止旧服务进程失败"
    );
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn terminate(_pid: u32) -> anyhow::Result<()> {
    bail!("当前系统不支持旧服务升级")
}

fn wait_dead(pid: u32) -> anyhow::Result<()> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    while bootstrap::process_alive(pid) {
        ensure!(Instant::now() < deadline, "等待旧服务进程 {pid} 退出超时");
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn managed_process(server: &LegacyServer) -> anyhow::Result<bool> {
    let marked = server.environ.iter().filter_map(|value| value.to_str()).any(|value| {
        value.strip_prefix("SYSTEMD_EXEC_PID=").and_then(|pid| pid.parse::<u32>().ok()) == Some(server.pid)
    }) || std::fs::read_to_string(format!("/proc/{}/cgroup", server.pid))
        .is_ok_and(|cg| cg.lines().any(|line| line.rsplit('/').next() == Some("falcon.service")));
    let argv = crate::service::systemctl_argv(unsafe { libc::geteuid() } == 0, &["show", crate::service::SYSTEMD_NAME, "--property=MainPID", "--value"]);
    let output = match Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !marked => return Ok(false),
        Err(error) => return Err(error).context("无法确认 Falcon 的 systemd 托管状态，拒绝直接终止进程"),
    };
    if output.status.success()
        && String::from_utf8_lossy(&output.stdout).trim().parse::<u32>() == Ok(server.pid)
    {
        return Ok(true);
    }
    ensure!(!marked, "旧服务属于 systemd，但 systemctl 未确认其 MainPID，拒绝直接终止进程");
    Ok(false)
}

#[cfg(target_os = "macos")]
fn managed_process(server: &LegacyServer) -> anyhow::Result<bool> {
    let domain = crate::service::launchd_domain(unsafe { libc::getuid() });
    let target = crate::service::launchd_target(&domain);
    let output = match Command::new("launchctl").args(["print", &target]).stdin(Stdio::null()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("无法检查 Falcon 的 launchd 托管状态"),
    };
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout).lines().map(str::trim).any(|line| {
            line.strip_prefix("pid")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .is_some_and(|value| value.trim().parse::<u32>() == Ok(server.pid))
        }))
}

#[cfg(windows)]
fn managed_process(server: &LegacyServer) -> anyhow::Result<bool> {
    let name = falcon_core::schtasks::schtasks_name();
    let output = match Command::new("schtasks").args(["/Query", "/TN", name.as_str(), "/XML"]).stdin(Stdio::null()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("无法检查 Falcon 的计划任务"),
    };
    if !output.status.success() {
        return Ok(false);
    }
    let Some(xml) = falcon_core::schtasks::decode_task_xml(&output.stdout) else { return Ok(false) };
    let Some(argv) = falcon_core::schtasks::schtasks_xml_actions(&xml) else { return Ok(false) };
    let Some(program) = argv.first() else { return Ok(false) };
    let left = std::fs::canonicalize(program).unwrap_or_else(|_| PathBuf::from(program));
    let right = std::fs::canonicalize(&server.exe).unwrap_or_else(|_| server.exe.clone());
    Ok(left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn managed_process(_server: &LegacyServer) -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_backup_uses_stopped_source_and_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let connection = rusqlite::Connection::open(root.join("falcon.db")).unwrap();
        connection.execute("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT)", []).unwrap();
        connection.execute("INSERT INTO settings VALUES ('instance_id', 'node-a')", []).unwrap();
        drop(connection);
        let files = backup_databases(&root, &work).unwrap();
        assert_eq!(files, ["falcon.db"]);
        let backup = rusqlite::Connection::open(work.join("falcon.db.backup")).unwrap();
        assert_eq!(backup.query_row("SELECT value FROM settings WHERE key='instance_id'", [], |row| row.get::<_, String>(0)).unwrap(), "node-a");
    }

    #[test]
    fn database_backup_requires_an_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        assert!(backup_databases(dir.path(), dir.path()).unwrap_err().to_string().contains("没有找到"));
    }

    #[test]
    fn executable_replacement_keeps_a_rollback_copy_and_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let old = bin.join(if cfg!(windows) { "falcon.exe" } else { "falcon" });
        let new = bin.join("falcon-node-new");
        std::fs::write(&old, "old-program").unwrap();
        std::fs::write(&new, "new-program").unwrap();
        let backup = replace_executable(&new, &old, &work).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"new-program");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-program");
        assert_eq!(std::fs::read(work.join("falcon.old")).unwrap(), b"old-program");
        restore_executable(&old, &backup).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"old-program");
        assert!(!backup.exists());
    }

    #[test]
    fn wmi_fallback_pins_connection_configuration_into_argv() {
        let server = LegacyServer {
            pid: 10,
            exe: PathBuf::from("falcon.exe"),
            args: vec!["falcon.exe".into(), "--port".into(), "5111".into()],
            environ: vec!["FALCON_HOST=0.0.0.0".into(), "FALCON_PORT=5222".into()],
            cwd: None,
            unknown_root: false,
        };
        let args = wmi_server_args(&server, Path::new("C:\\Data\\Falcon"));
        let value = |name: &str| args.windows(2).rev().find(|pair| pair[0] == name).map(|pair| pair[1].as_str());
        assert_eq!(value("--host"), Some("0.0.0.0"));
        assert_eq!(value("--port"), Some("5111"));
        assert_eq!(value("--data-dir"), Some("C:\\Data\\Falcon"));
    }
}
