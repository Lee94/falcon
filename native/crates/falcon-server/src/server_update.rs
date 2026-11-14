//! 服务端升级：只下载官方稳定 Release 的本平台单文件，分离帮手停服、备份、换入并核对。
//!
//! 数据库回滚的边界是开放正常业务之前。候选程序在 main 初始化引擎之前进入维护入口，
//! 仅迁移数据库并回答带任务标识的健康探测；帮手确认版本、实例与进程后提交，再放行 main。
//! 提交后即使正常启动失败也不再恢复旧库，避免覆盖已经接受的用户写入。

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use falcon_core::app_update::{self, ParseRelease, RELEASE_REPO, Release};
use falcon_proto::{ServerUpdateCheck, ServerUpdatePhase as Phase, ServerUpdateTask};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::AppState;
use crate::config::{ServerConfig, local_connect_host};
use crate::db::Db;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEADLINE: Duration = Duration::from_secs(90);
const MAX_BINARY: u64 = 512 * 1024 * 1024;
static RUNNING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);
static LISTENERS: LazyLock<Mutex<HashMap<PathBuf, u16>>> = LazyLock::new(Mutex::default);

/// main 在 bind 后登记实际端口，覆盖 --port 0 的随机分配值。
pub fn record_listener(config: &ServerConfig, actual_port: u16) {
    LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).insert(config.data_dir.clone(), actual_port);
}

pub fn platform(os: &str, arch: &str) -> Option<String> {
    let os = match os { "linux" => "linux", "macos" => "darwin", "windows" => "win32", _ => return None };
    let arch = match arch { "x86_64" => "x64", "aarch64" if os != "win32" => "arm64", _ => return None };
    Some(format!("{os}-{arch}"))
}

pub fn asset_name(version: &str, platform: &str) -> String {
    format!("falcon-v{version}-{platform}{}", if platform.starts_with("win32-") { ".exe" } else { "" })
}

fn http_builder() -> Result<reqwest::ClientBuilder> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?.with_platform_verifier()?.with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(reqwest::Client::builder().use_preconfigured_tls(tls).user_agent("falcon-server-update"))
}

fn http_client() -> Result<reqwest::Client> {
    Ok(http_builder()?.https_only(true).timeout(Duration::from_secs(300)).build()?)
}

/// 版本只用于选官方稳定 tag，不接受 URL 或命令。
pub async fn fetch_release(version: Option<&str>) -> Result<Release> {
    let url = match version {
        Some(v) => {
            let parsed = app_update::parse_version(v).context("升级版本必须是三段稳定版本号")?;
            format!("https://api.github.com/repos/{RELEASE_REPO}/releases/tags/v{}", app_update::version_string(parsed))
        }
        None => app_update::releases_latest_url(RELEASE_REPO),
    };
    let text = String::from_utf8(download_bytes(&http_client()?, &url, 4 * 1024 * 1024).await?)?;
    match app_update::parse_release(&text) {
        ParseRelease::Ready(release) => {
            if let Some(v) = version {
                ensure!(app_update::parse_version(v) == app_update::parse_version(&release.version), "发布版本与请求不一致");
            }
            Ok(release)
        }
        _ => bail!("没有找到可安装的官方稳定 Release"),
    }
}

pub(crate) fn release_url(release: &Release, name: &str) -> Result<String> {
    let expected = format!("https://github.com/{RELEASE_REPO}/releases/download/v{}/{name}", release.version);
    let mut found = release.assets.iter().filter(|asset| asset.name == name);
    let asset = found.next().with_context(|| format!("此 Release 缺少 {name}"))?;
    ensure!(found.next().is_none(), "Release 中有重复资产 {name}");
    ensure!(asset.url == expected && app_update::download_url_allowed(&asset.url, RELEASE_REPO), "发布产物下载地址不符合官方 Release 约定");
    Ok(expected)
}

pub fn checksum_for(manifest: &str, asset: &str) -> Result<String> {
    let mut result = None;
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let (digest, name) = line.split_once(char::is_whitespace).context("SHA256SUMS 格式错误")?;
        let name = name.trim_start().strip_prefix('*').unwrap_or(name.trim_start());
        ensure!(digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()), "SHA256SUMS 包含非法校验值");
        if name == asset {
            ensure!(result.is_none(), "SHA256SUMS 包含重复产物 {asset}");
            result = Some(digest.to_ascii_lowercase());
        }
    }
    result.with_context(|| format!("SHA256SUMS 缺少 {asset}"))
}

async fn download_bytes(client: &reqwest::Client, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(response.content_length().is_none_or(|n| n <= limit), "下载内容超过大小限制");
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(out.len() as u64 + chunk.len() as u64 <= limit, "下载内容超过大小限制");
        out.extend_from_slice(&chunk);
    }
    ensure!(!out.is_empty(), "下载内容为空");
    Ok(out)
}

/// 节点部署与当前实例升级共用的下载入口；校验成功才创建目标文件。
pub async fn download_verified_release(release: &Release, platform: &str, dest: &Path) -> Result<()> {
    let name = asset_name(&release.version, platform);
    let binary_url = release_url(release, &name)?;
    let sums_url = release_url(release, "SHA256SUMS")?;
    let client = http_client()?;
    let manifest = String::from_utf8(download_bytes(&client, &sums_url, 1024 * 1024).await?)?;
    let expected = checksum_for(&manifest, &name)?;
    let bytes = download_bytes(&client, &binary_url, MAX_BINARY).await?;
    ensure!(hex::encode(Sha256::digest(&bytes)) == expected, "服务端产物 SHA-256 校验失败");
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        executable(&dest)?;
        Ok(())
    }).await??;
    Ok(())
}

/// 与客户端应用更新同一口径：没有更高版本时，不要求旧 Release 已包含新增发布产物。
pub(crate) fn release_available(current: &str, release: &Release, platform: &str) -> Result<bool> {
    if !app_update::is_newer(&release.version, current) { return Ok(false); }
    release_url(release, &asset_name(&release.version, platform))?;
    release_url(release, "SHA256SUMS")?;
    Ok(true)
}

pub async fn check() -> ServerUpdateCheck {
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).unwrap_or_else(|| "unsupported".into());
    let mut check = ServerUpdateCheck { current_version: VERSION.into(), platform, ..Default::default() };
    match fetch_release(None).await {
        Ok(release) => {
            check.latest_version = Some(release.version.clone());
            match release_available(VERSION, &release, &check.platform) {
                Ok(available) => check.available = available,
                Err(err) => check.error = Some(err.to_string()),
            }
        }
        Err(err) => check.error = Some(format!("检查服务端更新失败：{err:#}")),
    }
    check
}

fn update_dir(data_dir: &Path) -> PathBuf { data_dir.join("server-update") }
fn task_path(data_dir: &Path) -> PathBuf { update_dir(data_dir).join("task.json") }
fn plan_path(data_dir: &Path) -> PathBuf { update_dir(data_dir).join("plan.json") }
fn owner_path(data_dir: &Path) -> PathBuf { update_dir(data_dir).join("active.lock") }

fn claim_instance(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(update_dir(data_dir))?;
    let path = owner_path(data_dir);
    if path.exists() {
        let owner: u32 = read_json(&path)?;
        ensure!(!pid_alive(owner), "同一数据目录已有进程正在升级");
        ensure!(!status(data_dir)?.phase.is_running(), "上次升级尚未完成，请先恢复该任务");
        std::fs::remove_file(&path)?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, &std::process::id())?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Plan {
    id: String,
    config_host: String,
    config_port: u16,
    data_dir: PathBuf,
    exe: PathBuf,
    old_version: String,
    instance_id: String,
    old_pid: u32,
    helper_pid: Option<u32>,
    candidate_pid: Option<u32>,
    managed: bool,
    argv: Vec<String>,
    cwd: PathBuf,
    old_digest: String,
    new_digest: String,
    /// waiting / candidate / rollback。只有 candidate 才允许新程序打开数据库。
    launch: String,
    /// 一旦置 true，正常业务可以写入，此后禁止恢复数据库备份。
    committed: bool,
    databases: Vec<String>,
}

impl Plan {
    fn work(&self) -> PathBuf { update_dir(&self.data_dir).join(&self.id) }
    fn candidate(&self) -> PathBuf { self.work().join(if cfg!(windows) { "candidate.exe" } else { "candidate" }) }
    fn backup_exe(&self) -> PathBuf { self.exe.with_file_name(format!(".falcon-update-{}.old", self.id)) }
    fn stage_exe(&self) -> PathBuf { self.exe.with_file_name(format!(".falcon-update-{}.new", self.id)) }
    fn save(&self) -> Result<()> { atomic_json(&plan_path(&self.data_dir), self) }
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("缺少升级状态目录")?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".state-{}-{}.tmp", std::process::id(), token()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&tmp)?;
        serde_json::to_writer(&mut file, value)?;
        file.sync_all()?;
        replace_file(&tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(tmp); }
    result
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> { std::fs::rename(from, to) }

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" { fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32; }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn token() -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("操作系统随机数不可用");
    hex::encode(bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?)?)
}

fn save_phase(data_dir: &Path, phase: Phase, error: Option<String>) -> Result<ServerUpdateTask> {
    let mut task: ServerUpdateTask = read_json(&task_path(data_dir))?;
    task.phase = phase;
    task.error = error;
    atomic_json(&task_path(data_dir), &task)?;
    if matches!(phase, Phase::Succeeded | Phase::Failed) {
        let _ = std::fs::remove_file(owner_path(data_dir));
    }
    Ok(task)
}

pub fn status(data_dir: &Path) -> Result<ServerUpdateTask> {
    if !task_path(data_dir).exists() { return Ok(ServerUpdateTask::default()); }
    let task: ServerUpdateTask = read_json(&task_path(data_dir))?;
    if matches!(task.phase, Phase::Installing | Phase::Restarting) {
        if let Ok(plan) = read_json::<Plan>(&plan_path(data_dir)) {
            if plan.helper_pid.is_some_and(|pid| !pid_alive(pid)) {
                let current: ServerUpdateTask = read_json(&task_path(data_dir))?;
                if !current.phase.is_running() { return Ok(current); }
                return save_phase(data_dir, Phase::Failed, Some(format!("升级帮手意外退出；请检查 {}，未确认安全恢复前保留维护状态", plan.work().display())));
            }
        }
    }
    Ok(task)
}

fn unresolved_rollback(plan: &Plan) -> bool {
    !plan.committed && plan.launch != "rollback" && plan.backup_exe().exists()
}

pub fn maintenance(data_dir: &Path) -> bool {
    let task = match status(data_dir) { Ok(task) => task, Err(_) => return true };
    match task.phase {
        Phase::Installing | Phase::Restarting => read_json::<Plan>(&plan_path(data_dir)).map_or(true, |p| !p.committed),
        Phase::Failed => read_json::<Plan>(&plan_path(data_dir)).is_ok_and(|p| unresolved_rollback(&p)),
        _ => false,
    }
}

pub fn start(state: AppState, version: &str) -> Result<ServerUpdateTask> {
    start_source(state, version, None)
}

/// 仅供持有目标节点令牌的网关调用。网关先按官方 Release 清单验证，此处不需要目标节点出网。
/// 节点令牌属于 SSH 部署信任边界；普通客户端只能调用接收版本号的官方升级接口。
pub async fn stage_verified_update(state: AppState, version: &str, bytes: Vec<u8>, expected_sha256: &str) -> Result<ServerUpdateTask> {
    ensure!(!bytes.is_empty() && bytes.len() as u64 <= MAX_BINARY, "服务端产物大小不合法");
    ensure!(expected_sha256.len() == 64 && expected_sha256.bytes().all(|b| b.is_ascii_hexdigit()), "升级 SHA-256 格式不合法");
    let expected = expected_sha256.to_ascii_lowercase();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        ensure!(hex::encode(Sha256::digest(&bytes)) == expected, "暂存服务端产物 SHA-256 校验失败");
        Ok(bytes)
    }).await??;
    start_source(state, version, Some(bytes))
}

fn start_source(state: AppState, version: &str, staged: Option<Vec<u8>>) -> Result<ServerUpdateTask> {
    let parsed = app_update::parse_version(version).context("升级版本必须是三段稳定版本号")?;
    let version = app_update::version_string(parsed);
    ensure!(app_update::is_newer(&version, VERSION), "目标版本必须高于当前服务端版本");
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).context("当前平台没有服务端发布产物")?;
    let data_dir = state.config.data_dir.clone();
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    ensure!(!running.contains(&data_dir) && !status(&data_dir)?.phase.is_running(), "此实例已有升级任务正在执行");
    ensure!(!state.restarting.load(std::sync::atomic::Ordering::SeqCst), "服务端正在重启");
    ensure!(!maintenance(&data_dir), "上次升级尚未安全恢复，不能开始新任务");
    let task = ServerUpdateTask { phase: if staged.is_some() { Phase::Verifying } else { Phase::Downloading }, target_version: Some(version.clone()), error: None };
    claim_instance(&data_dir)?;
    if let Err(err) = atomic_json(&task_path(&data_dir), &task) {
        let _ = std::fs::remove_file(owner_path(&data_dir));
        return Err(err);
    }
    running.insert(data_dir.clone());
    drop(running);
    tokio::spawn(async move {
        let result = prepare(&state, &version, &platform, staged).await;
        if let Err(err) = result {
            let _ = save_phase(&data_dir, Phase::Failed, Some(format!("升级失败：{err:#}")));
        }
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&data_dir);
    });
    Ok(task)
}

async fn prepare(state: &AppState, version: &str, platform: &str, staged: Option<Vec<u8>>) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(std::fs::metadata(&exe)?.uid() == unsafe { libc::geteuid() }, "可执行文件属于其他用户，无法在保留文件所有者的前提下升级");
    }
    let data_dir = state.config.data_dir.clone();
    let managed = crate::restart::supervised();
    if managed { validate_supervisor()?; }
    let port = LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).get(&data_dir).copied().unwrap_or(state.config.port);
    ensure!(port != 0, "尚未登记服务端实际监听端口");
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // 显式追加解析后的配置，覆盖相对 argv 与环境缺省；其余环境、工作目录及身份由帮手继承。
    argv.extend(["--host".into(), state.config.host.clone(), "--port".into(), port.to_string(), "--data-dir".into(), data_dir.to_string_lossy().into_owned()]);
    let mut plan = Plan {
        id: token(), config_host: state.config.host.clone(), config_port: port,
        data_dir, exe, old_version: VERSION.into(), instance_id: state.db.instance_id(), old_pid: std::process::id(),
        helper_pid: None, candidate_pid: None, managed, argv,
        cwd: std::env::current_dir()?, old_digest: String::new(), new_digest: String::new(),
        launch: "waiting".into(), committed: false, databases: Vec::new(),
    };
    std::fs::create_dir_all(plan.work())?;
    if let Some(bytes) = staged {
        let dest = plan.candidate();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            executable(&dest)
        }).await??;
    } else {
        let release = fetch_release(Some(version)).await?;
        download_verified_release(&release, platform, &plan.candidate()).await?;
    }
    save_phase(&plan.data_dir, Phase::Verifying, None)?;
    verify_candidate(&plan, version).await?;
    plan.old_digest = sha_file(&plan.exe)?;
    plan.new_digest = sha_file(&plan.candidate())?;
    // 跨卷时也只在目标目录内做改名。此时提前验证目标目录权限，避免停服后才发现无法换入。
    std::fs::copy(plan.candidate(), plan.stage_exe())?;
    std::fs::set_permissions(plan.stage_exe(), std::fs::metadata(&plan.exe)?.permissions())?;
    std::fs::File::open(plan.stage_exe())?.sync_all()?;
    let helper = plan.work().join(if cfg!(windows) { "helper.exe" } else { "helper" });
    std::fs::copy(&plan.exe, &helper)?;
    executable(&helper)?;
    plan.save()?;
    save_phase(&plan.data_dir, Phase::Installing, None)?;
    spawn_helper(&plan, &helper)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !plan.work().join("ready").exists() {
        ensure!(Instant::now() < deadline, "升级帮手未能启动，尚未停止当前服务");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let current: Plan = read_json(&plan_path(&plan.data_dir))?;
    ensure!(current.helper_pid.is_some_and(pid_alive) && status(&plan.data_dir)?.phase == Phase::Installing, "升级帮手在交接前退出");
    std::fs::write(plan.work().join("handoff"), b"ready")?;
    if !plan.managed { state.shutdown.notify_one(); }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Probe { version: String, platform: String, protocol: u32 }

async fn verify_candidate(plan: &Plan, version: &str) -> Result<()> {
    let isolated = plan.work().join("probe");
    // 老版本不认识子命令时可能启动服务器；显式临时目录、回环地址和随机端口使它无法碰真实服务。
    let mut cmd = tokio::process::Command::new(plan.candidate());
    cmd.args(["update-helper", "--probe", "--host", "127.0.0.1", "--port", "0", "--data-dir"])
        .arg(&isolated).env_remove("FALCON_SESSION_ID").kill_on_drop(true).stdin(Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(10), cmd.output()).await.context("候选程序不支持升级维护协议")??;
    ensure!(output.status.success(), "候选程序自检失败");
    let probe: Probe = serde_json::from_slice(&output.stdout).context("候选程序升级协议无效")?;
    ensure!(probe.version == version && probe.protocol == 1 && Some(probe.platform) == platform(std::env::consts::OS, std::env::consts::ARCH), "候选程序版本、平台或维护协议不符");
    Ok(())
}

fn validate_supervisor() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // 自定义 unit 也可能带 SYSTEMD_EXEC_PID；只允许控制确实托管当前 pid 的 Falcon unit。
        let mut cmd = Command::new("systemctl");
        if unsafe { libc::geteuid() } != 0 { cmd.arg("--user"); }
        let output = cmd.args(["show", crate::service::SYSTEMD_NAME, "--property=MainPID", "--value"]).output()?;
        ensure!(output.status.success() && String::from_utf8_lossy(&output.stdout).trim().parse::<u32>() == Ok(std::process::id()),
            "当前服务由自定义系统服务托管，无法保留其托管配置进行自动升级");
    }
    Ok(())
}

fn spawn_helper(plan: &Plan, helper: &Path) -> Result<()> {
    let mut cmd;
    #[cfg(target_os = "linux")]
    if plan.managed {
        // setsid 不能离开 systemd 的控制组，stop 会把同组帮手杀掉；独立 scope 保留原环境和身份。
        cmd = Command::new("systemd-run");
        if unsafe { libc::geteuid() } != 0 { cmd.arg("--user"); }
        cmd.args(["--scope", "--quiet"]).arg(format!("--unit=falcon-update-{}", plan.id)).arg(helper);
    } else { cmd = Command::new(helper); }
    #[cfg(not(target_os = "linux"))]
    { cmd = Command::new(helper); }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(plan.work().join("helper.log"))?;
    cmd.arg("update-helper").arg(&plan.data_dir).arg(&plan.id).current_dir(&plan.cwd)
        .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    detach(&mut cmd);
    cmd.spawn().context("启动独立升级帮手失败")?;
    Ok(())
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe { cmd.pre_exec(|| { if libc::setsid() < 0 { return Err(std::io::Error::last_os_error()); } Ok(()) }); }
}
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x00000008 | 0x00000200);
}
#[cfg(not(any(unix, windows)))]
fn detach(_: &mut Command) {}

fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?; }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sha_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut sha = Sha256::new();
    let mut buf = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        sha.update(&buf[..n]);
    }
    Ok(hex::encode(sha.finalize()))
}

fn pid_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, System};
    let pid = Pid::from_u32(pid);
    let mut system = System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()))
        .expect("sysinfo System");
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some_and(|p| p.status() != ProcessStatus::Zombie)
}

fn wait_dead(pid: u32) -> Result<()> {
    let deadline = Instant::now() + DEADLINE;
    while pid_alive(pid) {
        ensure!(Instant::now() < deadline, "等待进程 {pid} 退出超时");
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// main 在解析普通启动参数之前分流此子命令，帮手从独立副本运行以兼容 Windows 文件占用规则。
pub fn run_helper_cli(argv: &[String]) -> i32 {
    if argv.first().map(String::as_str) == Some("--probe") {
        let probe = Probe { version: VERSION.into(), platform: platform(std::env::consts::OS, std::env::consts::ARCH).unwrap_or_default(), protocol: 1 };
        println!("{}", serde_json::to_string(&probe).expect("序列化升级协议"));
        return 0;
    }
    let result = (|| -> Result<()> {
        ensure!(argv.len() == 2, "用法: falcon update-helper <data-dir> <task-id>");
        let data_dir = PathBuf::from(&argv[0]);
        let mut plan: Plan = read_json(&plan_path(&data_dir))?;
        ensure!(plan.id == argv[1] && plan.data_dir == data_dir && plan.old_pid != std::process::id(), "升级任务身份不符");
        plan.helper_pid = Some(std::process::id());
        plan.save()?;
        let result = helper_run(&mut plan);
        if let Err(error) = result {
            let message = if plan.committed {
                format!("升级已开放业务，保留新程序和当前数据库，不再回滚：{error:#}")
            } else {
                match rollback(&mut plan) {
                    Ok(()) => format!("升级失败，已恢复旧程序和升级前数据库：{error:#}"),
                    Err(rollback) => format!("升级失败：{error:#}；自动恢复失败，请检查 {}：{rollback:#}", plan.work().display()),
                }
            };
            save_phase(&data_dir, Phase::Failed, Some(message))?;
            bail!("升级任务失败");
        }
        Ok(())
    })();
    match result { Ok(()) => 0, Err(err) => { eprintln!("升级帮手失败：{err:#}"); 1 } }
}

fn helper_run(plan: &mut Plan) -> Result<()> {
    std::fs::write(plan.work().join("ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !plan.work().join("handoff").exists() {
        ensure!(Instant::now() < deadline, "父进程未交接升级任务");
        std::thread::sleep(Duration::from_millis(100));
    }
    if plan.managed { service("stop")?; }
    wait_dead(plan.old_pid)?;
    ensure!(sha_file(&plan.exe)? == plan.old_digest && sha_file(&plan.stage_exe())? == plan.new_digest, "停止服务后程序内容发生变化");
    backup_databases(plan)?;
    std::fs::rename(&plan.exe, plan.backup_exe()).context("备份旧程序失败")?;
    std::fs::rename(plan.stage_exe(), &plan.exe).context("换入新程序失败")?;
    plan.launch = "candidate".into();
    plan.save()?;
    save_phase(&plan.data_dir, Phase::Restarting, None)?;
    launch(plan)?;
    let health = wait_health(plan, &status(&plan.data_dir)?.target_version.context("缺少目标版本")?, true)?;
    plan.candidate_pid = Some(health.pid);
    // 此记录写入成功后才能让候选程序离开维护入口；后续故障不能恢复旧库。
    plan.committed = true;
    plan.save()?;
    wait_health(plan, &health.version, false)?;
    save_phase(&plan.data_dir, Phase::Succeeded, None)?;
    Ok(())
}

fn service(command: &str) -> Result<()> {
    ensure!(crate::service_cli::run_service_cli(&[command.into()]) == 0, "服务托管命令 {command} 失败");
    Ok(())
}

fn launch(plan: &mut Plan) -> Result<()> {
    if plan.managed { service("start")?; } else {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(plan.work().join("server.log"))?;
        let mut cmd = Command::new(&plan.exe);
        cmd.args(&plan.argv).current_dir(&plan.cwd).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
        detach(&mut cmd);
        plan.candidate_pid = Some(cmd.spawn()?.id());
        plan.save()?;
    }
    Ok(())
}

fn backup_databases(plan: &mut Plan) -> Result<()> {
    // 旧进程完全退出后才做快照，覆盖停服之前所有已确认的写入。两种库名都保留，避免迁移改选库。
    for name in ["falcon.db", "mojito.db"] {
        let source = plan.data_dir.join(name);
        if !source.exists() { continue; }
        let backup = plan.work().join(format!("{name}.backup"));
        let connection = rusqlite::Connection::open(&source)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.backup(rusqlite::MAIN_DB, &backup, None::<fn(rusqlite::backup::Progress)>)?;
        std::fs::File::open(&backup)?.sync_all()?;
        plan.databases.push(name.into());
    }
    ensure!(!plan.databases.is_empty(), "没有找到待备份的数据库");
    plan.save()
}

fn rollback(plan: &mut Plan) -> Result<()> {
    ensure!(!plan.committed, "业务已开放，禁止恢复旧数据库");
    // 交接前失败时旧进程还在运行；不能停止它，更不能覆盖它正在写的数据库。
    if !plan.backup_exe().exists() {
        if !pid_alive(plan.old_pid) && plan.work().join("handoff").exists() {
            // 备份数据库或换出旧程序之前失败：程序和数据库仍是原件，也要恢复已经停止的服务。
            plan.launch = "rollback".into();
            plan.candidate_pid = None;
            plan.save()?;
            launch(plan)?;
            wait_health(plan, &plan.old_version, false)?;
        }
        return Ok(());
    }
    if plan.managed { service("stop")?; }
    if let Some(pid) = plan.candidate_pid { stop_pid(pid)?; }
    // 托管候选的 pid 可能还没通过健康检查，startup_gate 会写入它，不凭端口盲杀其他进程。
    if let Ok(current) = read_json::<Plan>(&plan_path(&plan.data_dir)) {
        if let Some(pid) = current.candidate_pid.filter(|pid| Some(*pid) != plan.candidate_pid) { stop_pid(pid)?; }
    }
    restore_files(plan)?;
    plan.launch = "rollback".into();
    plan.candidate_pid = None;
    plan.save()?;
    // 健康确认之前继续保留任务锁与维护标记，避免恢复中的旧实例接受第二个升级任务。
    save_phase(&plan.data_dir, Phase::Restarting, Some("升级失败，已恢复旧程序与数据库，正在重新启动".into()))?;
    launch(plan)?;
    wait_health(plan, &plan.old_version, false)?;
    Ok(())
}

fn restore_files(plan: &Plan) -> Result<()> {
    ensure!(!plan.committed, "业务已开放，禁止恢复旧数据库");
    ensure!(sha_file(&plan.backup_exe())? == plan.old_digest, "旧程序备份内容不符");
    if plan.exe.exists() {
        std::fs::copy(&plan.exe, plan.exe.with_file_name(format!(".falcon-update-{}.failed", plan.id)))?;
    }
    for name in ["falcon.db", "mojito.db"] {
        let dest = plan.data_dir.join(name);
        let staged = plan.data_dir.join(format!(".{name}.restore-{}", plan.id));
        if plan.databases.iter().any(|n| n == name) {
            std::fs::copy(plan.work().join(format!("{name}.backup")), &staged)?;
            std::fs::File::open(&staged)?.sync_all()?;
        }
        for suffix in ["-wal", "-shm"] {
            let path = plan.data_dir.join(format!("{name}{suffix}"));
            if path.exists() { std::fs::remove_file(path)?; }
        }
        if staged.exists() { replace_file(&staged, &dest)?; }
        else if !plan.databases.iter().any(|n| n == name) && dest.exists() { std::fs::remove_file(&dest)?; }
    }
    // 数据库完整恢复后才换回可执行文件；备份始终保留，恢复中途失败可再次取证和恢复。
    std::fs::copy(plan.backup_exe(), plan.stage_exe())?;
    std::fs::File::open(plan.stage_exe())?.sync_all()?;
    replace_file(&plan.stage_exe(), &plan.exe)?;
    Ok(())
}

fn stop_pid(pid: u32) -> Result<()> {
    if !pid_alive(pid) { return Ok(()); }
    #[cfg(unix)]
    unsafe { ensure!(libc::kill(pid as i32, libc::SIGTERM) == 0, "终止候选进程失败"); }
    #[cfg(windows)]
    ensure!(Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status()?.success(), "终止候选进程失败");
    wait_dead(pid)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateHealth {
    pub version: String,
    pub instance_id: String,
    pub pid: u32,
    pub task_id: String,
    pub maintenance: bool,
}

pub fn health(config: &ServerConfig, db: &Db) -> Result<UpdateHealth> {
    let plan: Plan = read_json(&plan_path(&config.data_dir))?;
    Ok(UpdateHealth { version: VERSION.into(), instance_id: db.instance_id(), pid: std::process::id(), task_id: plan.id, maintenance: false })
}

fn wait_health(plan: &Plan, version: &str, maintenance: bool) -> Result<UpdateHealth> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let client = http_builder()?.no_proxy().timeout(Duration::from_secs(2)).build()?;
        let url = format!("http://{}:{}/api/system/update/health", local_connect_host(&plan.config_host), plan.config_port);
        let deadline = Instant::now() + DEADLINE;
        loop {
            let response = async {
                let bytes = client.get(&url).send().await?.error_for_status()?.bytes().await?;
                Ok::<_, anyhow::Error>(serde_json::from_slice::<UpdateHealth>(&bytes)?)
            }.await;
            if let Ok(health) = response {
                if health.version == version && health.instance_id == plan.instance_id && health.task_id == plan.id
                    && health.pid != plan.old_pid && health.maintenance == maintenance && pid_alive(health.pid)
                    && plan.candidate_pid.is_none_or(|pid| pid == health.pid) {
                    return Ok(health);
                }
            }
            if !plan.managed {
                if let Some(pid) = plan.candidate_pid { ensure!(pid_alive(pid), "候选服务进程在健康确认之前退出"); }
            }
            ensure!(Instant::now() < deadline, "升级健康检查超时，未确认真实版本、实例与进程");
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
}

/// main 在 Db::open、SecretBox 与引擎初始化之前调用。普通启动不会创建升级目录或更换程序。
pub fn startup_gate(config: &mut ServerConfig) -> Result<()> {
    let task = status(&config.data_dir)?;
    if matches!(task.phase, Phase::Downloading | Phase::Verifying) {
        if owner_path(&config.data_dir).exists() {
            let owner: u32 = read_json(&owner_path(&config.data_dir))?;
            ensure!(!pid_alive(owner), "此数据目录仍有其他服务端进程正在下载升级");
        }
        save_phase(&config.data_dir, Phase::Failed, Some("下载期间服务端重新启动，升级已中断".into()))?;
        return Ok(());
    }
    if task.phase == Phase::Failed {
        if let Ok(plan) = read_json::<Plan>(&plan_path(&config.data_dir)) {
            ensure!(!unresolved_rollback(&plan), "上次升级尚未安全恢复，拒绝打开数据库；备份目录 {}", plan.work().display());
        }
    }
    if !matches!(task.phase, Phase::Installing | Phase::Restarting) { return Ok(()); }
    let deadline = Instant::now() + DEADLINE;
    let mut plan: Plan;
    loop {
        plan = read_json(&plan_path(&config.data_dir))?;
        if config.port == 0 { config.port = plan.config_port; }
        ensure!(plan.config_host == config.host && plan.config_port == config.port && plan.data_dir == config.data_dir,
            "服务托管启动配置与实际升级实例不同，拒绝打开数据库");
        if plan.committed || plan.launch == "rollback" { return Ok(()); }
        ensure!(plan.helper_pid.is_some_and(pid_alive), "升级帮手已退出，维护状态保留，请检查升级目录");
        if plan.launch == "candidate" { break; }
        ensure!(Instant::now() < deadline, "等待升级帮手完成程序替换超时");
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(sha_file(&std::env::current_exe()?)? == plan.new_digest, "启动的不是本任务校验过的新程序");
    ensure!(status(&config.data_dir)?.target_version.as_deref() == Some(VERSION), "候选程序实际版本与升级任务不一致");
    plan.candidate_pid = Some(std::process::id());
    plan.save()?;
    // 只迁移并核对身份；引擎、存档清扫、会话恢复均未启动，因此回滚不会遗漏后台用户数据写入。
    let db = Db::open(&config.data_dir)?;
    crate::nodes::migration::migrate_local(&db, &config.data_dir)?;
    crate::nodes::migration::bind_legacy_hosts(&db)?;
    ensure!(db.instance_id() == plan.instance_id, "迁移后的实例身份不一致");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        use axum::{Json, Router, http::StatusCode, routing::get};
        let health = UpdateHealth { version: VERSION.into(), instance_id: plan.instance_id.clone(), pid: std::process::id(), task_id: plan.id.clone(), maintenance: true };
        let dir = config.data_dir.clone();
        let app = Router::new()
            .route("/api/system/update/health", get(move || { let health = health.clone(); async move { Json(health) } }))
            .route("/api/system/update/status", get(move || { let task = status(&dir).unwrap_or_default(); async move { Json(task) } }))
            .fallback(|| async { (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"服务端升级维护中，暂不接受业务请求"}))) });
        let listener = tokio::net::TcpListener::bind(format!("{}:{}", config.host, config.port)).await?;
        let serve = axum::serve(listener, app);
        let committed = async {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let current: Plan = read_json(&plan_path(&config.data_dir))?;
                if current.committed { return Ok::<_, anyhow::Error>(()); }
                ensure!(current.helper_pid.is_some_and(pid_alive) && Instant::now() < deadline, "升级维护等待确认超时，拒绝开放业务");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::select! { result = serve => { result?; bail!("维护监听意外停止") }, result = committed => result }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_assets_and_checksums_are_exact() {
        assert_eq!(platform("linux", "aarch64").as_deref(), Some("linux-arm64"));
        assert_eq!(platform("windows", "x86_64").as_deref(), Some("win32-x64"));
        assert_eq!(platform("windows", "aarch64"), None);
        let name = asset_name("1.2.3", "win32-x64");
        assert_eq!(name, "falcon-v1.2.3-win32-x64.exe");
        let sum = "a".repeat(64);
        assert_eq!(checksum_for(&format!("{sum}  {name}\n"), &name).unwrap(), sum);
        assert!(checksum_for(&format!("{sum}  {name}\n{sum}  {name}"), &name).is_err());
        assert!(checksum_for(&format!("{sum}  other"), &name).is_err());
        assert!(checksum_for("bad  file", "file").is_err());
    }

    #[test]
    fn current_or_older_releases_do_not_require_new_server_assets() {
        let release = Release { version: "1.2.3".into(), assets: Vec::new() };
        assert!(!release_available("1.2.3", &release, "linux-x64").unwrap());
        assert!(!release_available("1.2.4", &release, "linux-x64").unwrap());
        assert!(release_available("1.2.2", &release, "linux-x64").is_err());
    }

    #[test]
    fn release_assets_reject_foreign_tag_and_url() {
        use app_update::ReleaseAsset;
        let name = asset_name("1.2.3", "linux-x64");
        let mut release = Release { version: "1.2.3".into(), assets: vec![ReleaseAsset { name: name.clone(), url: format!("https://github.com/{RELEASE_REPO}/releases/download/v1.2.3/{name}") }] };
        assert!(release_url(&release, &name).is_ok());
        release.assets[0].url = format!("https://github.com/{RELEASE_REPO}/releases/download/v1.2.4/{name}");
        assert!(release_url(&release, &name).is_err());
        release.assets[0].url = "https://example.com/file".into();
        assert!(release_url(&release, &name).is_err());
    }

    #[test]
    fn persisted_task_and_interrupted_download_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(status(dir.path()).unwrap(), ServerUpdateTask::default());
        let task = ServerUpdateTask { phase: Phase::Downloading, target_version: Some("1.2.3".into()), error: None };
        atomic_json(&task_path(dir.path()), &task).unwrap();
        let mut config = ServerConfig { host: "127.0.0.1".into(), port: 4999, data_dir: dir.path().into() };
        startup_gate(&mut config).unwrap();
        let task = status(dir.path()).unwrap();
        assert_eq!(task.phase, Phase::Failed);
        assert_eq!(task.target_version.as_deref(), Some("1.2.3"));
        assert!(!maintenance(dir.path()));
    }

    fn fixture_plan(dir: &Path, port: u16) -> Plan {
        let db = Db::open(dir).unwrap();
        let exe = dir.join("falcon");
        std::fs::write(&exe, b"old-program").unwrap();
        let plan = Plan {
            id: token(), config_host: "127.0.0.1".into(), config_port: port, data_dir: dir.into(),
            old_digest: sha_file(&exe).unwrap(), exe, old_version: VERSION.into(), instance_id: db.instance_id(),
            old_pid: u32::MAX, helper_pid: Some(std::process::id()), candidate_pid: None, managed: false,
            argv: Vec::new(), cwd: dir.into(), new_digest: String::new(), launch: "candidate".into(), committed: false, databases: Vec::new(),
        };
        std::fs::create_dir_all(plan.work()).unwrap();
        plan
    }

    #[test]
    fn crashed_helper_leaves_a_final_error_and_keeps_unrestored_data_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = fixture_plan(dir.path(), 4997);
        plan.helper_pid = Some(u32::MAX);
        std::fs::copy(&plan.exe, plan.backup_exe()).unwrap();
        plan.save().unwrap();
        atomic_json(&task_path(dir.path()), &ServerUpdateTask { phase: Phase::Installing, target_version: Some("9.9.9".into()), error: None }).unwrap();
        assert_eq!(status(dir.path()).unwrap().phase, Phase::Failed);
        assert!(maintenance(dir.path()));
        let mut config = ServerConfig { host: "127.0.0.1".into(), port: 4997, data_dir: dir.path().into() };
        assert!(startup_gate(&mut config).is_err());
    }

    #[test]
    fn instance_lock_rejects_a_second_task() {
        let dir = tempfile::tempdir().unwrap();
        claim_instance(dir.path()).unwrap();
        assert!(claim_instance(dir.path()).is_err());
    }

    #[test]
    fn rollback_restores_accepted_writes_and_never_runs_after_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = fixture_plan(dir.path(), 4998);
        {
            let db = Db::open(dir.path()).unwrap();
            db.set_setting("accepted-write", "before-maintenance");
        }
        backup_databases(&mut plan).unwrap();
        std::fs::copy(&plan.exe, plan.backup_exe()).unwrap();
        std::fs::write(&plan.exe, b"new-program").unwrap();
        {
            let db = Db::open(dir.path()).unwrap();
            db.set_setting("accepted-write", "candidate-migration");
        }
        restore_files(&plan).unwrap();
        assert_eq!(std::fs::read(&plan.exe).unwrap(), b"old-program");
        {
            let db = Db::open(dir.path()).unwrap();
            assert_eq!(db.get_setting("accepted-write").as_deref(), Some("before-maintenance"));
            db.set_setting("accepted-write", "accepted-after-commit");
        }
        plan.committed = true;
        assert!(restore_files(&plan).is_err());
        assert_eq!(Db::open(dir.path()).unwrap().get_setting("accepted-write").as_deref(), Some("accepted-after-commit"));
    }

    #[test]
    fn candidate_maintenance_checks_identity_and_blocks_requests_until_commit() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut plan = fixture_plan(dir.path(), port);
        plan.new_digest = sha_file(&std::env::current_exe().unwrap()).unwrap();
        plan.save().unwrap();
        atomic_json(&task_path(dir.path()), &ServerUpdateTask { phase: Phase::Restarting, target_version: Some(VERSION.into()), error: None }).unwrap();
        let mut config = ServerConfig { host: "127.0.0.1".into(), port: 0, data_dir: dir.path().into() };
        let worker = std::thread::spawn(move || { startup_gate(&mut config).unwrap(); config.port });
        let health = wait_health(&plan, VERSION, true).unwrap();
        assert_eq!(health.instance_id, plan.instance_id);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let response = http_builder().unwrap().no_proxy().build().unwrap()
                .post(format!("http://127.0.0.1:{port}/api/projects")).send().await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        });
        let mut current: Plan = read_json(&plan_path(dir.path())).unwrap();
        assert_eq!(current.candidate_pid, Some(std::process::id()));
        assert!(!current.committed);
        current.committed = true;
        current.save().unwrap();
        assert_eq!(worker.join().unwrap(), port);
        assert!(!maintenance(dir.path()));
    }

    #[test]
    fn snapshot_keeps_wal_writes_and_instance_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let id = db.instance_id();
        db.set_setting("accepted-write", "must-survive");
        let source = crate::db::db_file(dir.path());
        let backup = dir.path().join("backup.db");
        let conn = rusqlite::Connection::open(&source).unwrap();
        conn.backup(rusqlite::MAIN_DB, &backup, None).unwrap();
        let copied = Db::with_connection(rusqlite::Connection::open(backup).unwrap()).unwrap();
        assert_eq!(copied.instance_id(), id);
        assert_eq!(copied.get_setting("accepted-write").as_deref(), Some("must-survive"));
    }
}
