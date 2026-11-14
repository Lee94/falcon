//! 宿主机节点的本机引导。普通服务在 bind 后发布描述；`node ensure` 只在同一根目录
//! 没有活节点时脱离 SSH 启动服务。系统文件锁随进程退出释放，不靠固定超时删除活锁。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::config::ServerConfig;
use crate::db::Db;

pub(super) const DESCRIPTOR: &str = "node.json";
/// 部署前在二进制中检查此标识：旧入口会把未知子命令当普通服务启动，不能试运行探测。
pub(super) const CAPABILITY_MARKER: &[u8] = b"FALCON_NODE_PROTOCOL_1";
const STARTING: &str = "node-starting.json";
const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Descriptor {
    pub node_id: String,
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub protocol_version: u32,
    pub platform: String,
    pub arch: String,
    pub data_dir: String,
    pub token: String,
}

impl std::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Descriptor")
            .field("node_id", &self.node_id)
            .field("pid", &self.pid)
            .field("port", &self.port)
            .field("version", &self.version)
            .field("token", &"[redacted]")
            .finish()
    }
}

pub(super) use falcon_proto::NodeInfo as Identity;

/// 常规服务持有的进程锁。显式 unlock 后再关闭 fd：并行线程刚 fork 的子进程可能尚未
/// exec，单靠关闭父 fd 会让锁短暂跟着继承的 open file description 存活（macOS 实测）。
#[derive(Debug)]
pub struct NodeProcessLock(File);

impl Drop for NodeProcessLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// 常规 main 在打开数据库、恢复 PTY 之前持有，并保留到进程退出。ensure 父进程不拿
/// 这把锁；它的子服务自己在 main 入口获取，避免 spawn 后和父进程互相等待。
pub fn lock_node_process(data_dir: &Path) -> anyhow::Result<NodeProcessLock> {
    std::fs::create_dir_all(data_dir)?;
    let root = canonical_root(data_dir)?;
    let file = private_open(&root.join("node-process.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(NodeProcessLock(file)),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "数据目录 {} 已由另一个 Falcon 服务进程使用；请连接现有节点，不会打开数据库或恢复会话",
            root.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("无法获取节点进程锁"),
    }
}

/// 迁移 API 使用的节点令牌只保存在宿主机根目录，不放进公开 `/api/node` 响应。
/// 单独的文件锁也保护直接启动的服务，避免两个首次请求拿到不同令牌。
pub fn node_token(data_dir: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(data_dir)?;
    let _lock = lock(data_dir, "node-token.lock", Duration::from_secs(5))?;
    let path = data_dir.join("node.token");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let token = text.trim();
            ensure!(
                token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
                "节点令牌文件格式不合法：{}",
                path.display()
            );
            private_permissions(&path)?;
            Ok(token.into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = hex::encode(rand::random::<[u8; 32]>());
            atomic_write(&path, token.as_bytes())?;
            Ok(token)
        }
        Err(e) => Err(e).with_context(|| format!("读取节点令牌 {} 失败", path.display())),
    }
}

/// main 在成功 bind 后、开始接受 HTTP 请求前调用；手工启动与已安装服务也发布同一格式。
pub fn publish(config: &ServerConfig, actual_port: u16, db: &Db) -> anyhow::Result<()> {
    ensure!(actual_port != 0, "节点描述必须使用 bind 后的实际端口");
    let root = canonical_root(&config.data_dir)?;
    let _lock = lock(&root, "node-publish.lock", Duration::from_secs(5))?;
    if let Some(previous) = read_descriptor(&root)? {
        // 两个服务即使碰巧同 nodeId，也不能同时打开同一份数据库并对外提供节点。
        if previous.pid != std::process::id() && process_alive(previous.pid) {
            bail!(
                "数据目录 {} 已由节点进程 {} 使用（端口 {}）；请连接现有服务",
                root.display(),
                previous.pid,
                previous.port
            );
        }
    }
    let descriptor = Descriptor {
        node_id: db.instance_id(),
        pid: std::process::id(),
        port: actual_port,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: 1,
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        data_dir: root.to_string_lossy().into_owned(),
        token: node_token(&root)?,
    };
    atomic_write(&root.join(DESCRIPTOR), &serde_json::to_vec(&descriptor)?)?;
    let _ = std::fs::remove_file(root.join(STARTING));
    Ok(())
}

/// main 对 `node` 分流后传剩余参数（`ensure --data-dir <root>`）。stdout 只输出一行 JSON。
pub fn run_node_cli(argv: &[String]) -> i32 {
    match ensure_cli(argv) {
        Ok(descriptor) => match serde_json::to_string(&descriptor) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("node ensure: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("node ensure: {e:#}");
            1
        }
    }
}

fn parse_cli(argv: &[String]) -> anyhow::Result<PathBuf> {
    ensure!(
        argv.len() == 3 && argv[0] == "ensure" && argv[1] == "--data-dir" && !argv[2].is_empty(),
        "{} 用法: falcon node ensure --data-dir <root>（必须显式指定数据目录）",
        std::str::from_utf8(CAPABILITY_MARKER).unwrap_or("节点协议1")
    );
    let root = PathBuf::from(&argv[2]);
    ensure!(root.is_absolute(), "node ensure 的数据目录必须是绝对路径");
    Ok(root)
}

fn ensure_cli(argv: &[String]) -> anyhow::Result<Descriptor> {
    let root = parse_cli(argv)?;
    std::fs::create_dir_all(&root)?;
    let root = canonical_root(&root)?;
    let _lock = lock(
        &root,
        "node-ensure.lock",
        READY_TIMEOUT + Duration::from_secs(5),
    )?;
    if let Some(node) = live_descriptor(&root)? {
        return Ok(node);
    }
    // 若上次 ensure 在 spawn 后意外退出，先等它的启动记录过期。记录在 spawn 前写入，
    // 因而没有“子进程已存在、描述和启动记录都不存在”的重复启动窗口。
    if starting_recent(&root) {
        let deadline = Instant::now() + READY_TIMEOUT;
        while starting_recent(&root) && Instant::now() < deadline {
            if let Some(node) = live_descriptor(&root)? {
                return Ok(node);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        ensure!(
            !starting_recent(&root),
            "另一个节点仍在启动，请稍后重试：{}",
            root.display()
        );
    }
    if let Some((pid, unknown_root)) = legacy_server(&root) {
        ensure!(
            !unknown_root,
            "已有 Falcon 进程 {pid}，但系统不允许确定它的数据目录；请先确认现有服务，拒绝另起节点"
        );
        bail!(
            "数据目录 {} 已有 Falcon 服务进程 {pid}，但没有可验证的节点描述；请先手动升级该服务，不会另起节点或覆盖现有服务",
            root.display()
        );
    }
    let pending = serde_json::json!({ "pid": std::process::id(), "createdAt": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs() });
    atomic_write(&root.join(STARTING), &serde_json::to_vec(&pending)?)?;
    // WMI 超时并不能证明它没有创建进程；启动报错也保留短期记录，后续 ensure 先验证。
    let mut launched = launch(&root)?;
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut probe_error = None;
    loop {
        // 描述文件在 bind 后、HTTP serve 前发布；允许这段很短的就绪窗口，仍不启动第二份。
        match live_descriptor(&root) {
            Ok(Some(node)) => return Ok(node),
            Ok(None) => {}
            Err(e) => probe_error = Some(format!("{e:#}")),
        }
        if launched.exited()? {
            let _ = std::fs::remove_file(root.join(STARTING));
            bail!("节点启动后立即退出：{}", log_tail(&root));
        }
        if Instant::now() >= deadline {
            // 不杀掉可能仍在初始化的服务，也不删除启动记录；下次 ensure 会再验证。
            bail!(
                "节点在 {} 秒内未就绪：{}；{}",
                READY_TIMEOUT.as_secs(),
                probe_error.as_deref().unwrap_or("没有有效描述"),
                log_tail(&root)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn canonical_root(root: &Path) -> anyhow::Result<PathBuf> {
    std::fs::canonicalize(root).with_context(|| format!("解析节点目录 {} 失败", root.display()))
}

pub(super) fn read_descriptor(root: &Path) -> anyhow::Result<Option<Descriptor>> {
    match std::fs::read(root.join(DESCRIPTOR)) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes)
                .context("节点描述文件损坏，拒绝另起服务；请检查 node.json")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("无法读取节点描述文件"),
    }
}

fn live_descriptor(root: &Path) -> anyhow::Result<Option<Descriptor>> {
    let Some(node) = read_descriptor(root)? else {
        return Ok(None);
    };
    ensure!(
        same_root(&node.data_dir, &root.to_string_lossy()),
        "节点描述的数据目录与请求不符，拒绝启动"
    );
    ensure!(
        node.protocol_version == 1,
        "节点协议不兼容：需要1，描述为{}；请手动升级节点",
        node.protocol_version
    );
    if !process_alive(node.pid) {
        return Ok(None);
    }
    let info = read_local_identity(node.port).with_context(|| {
        format!(
            "节点进程 {} 仍在运行，但 /api/node 无法验证（旧版服务需要手动升级）；保留现有服务",
            node.pid
        )
    })?;
    validate_identity(&node, &info, &root.to_string_lossy())?;
    Ok(Some(node))
}

pub(super) fn validate_identity(
    node: &Descriptor,
    info: &Identity,
    root: &str,
) -> anyhow::Result<()> {
    ensure!(
        node.protocol_version == 1 && info.protocol_version == 1,
        "节点协议不兼容：需要 1，描述为 {}、HTTP 为 {}；请手动升级节点",
        node.protocol_version,
        info.protocol_version
    );
    ensure!(
        node.port != 0 && node.pid != 0 && !node.node_id.is_empty(),
        "节点描述缺少有效身份、进程或端口"
    );
    ensure!(
        node.node_id == info.node_id,
        "节点 HTTP 身份与描述文件不符，端口可能已被其他服务使用"
    );
    ensure!(
        !info.version.is_empty() && node.version == info.version,
        "节点 HTTP 版本与描述文件不符"
    );
    ensure!(
        same_root(&node.data_dir, root) && same_root(&info.data_dir, root),
        "节点 HTTP 数据目录与请求的根目录不符"
    );
    ensure!(
        node.token.len() == 64 && node.token.bytes().all(|b| b.is_ascii_hexdigit()),
        "节点描述没有有效迁移令牌，请手动升级节点服务"
    );
    Ok(())
}

pub(super) fn same_root(left: &str, right: &str) -> bool {
    let normalize = |s: &str| {
        let path = s.replace('\\', "/");
        let path = path.strip_prefix("//?/").unwrap_or(&path);
        let path = if let Some(unc) = path.strip_prefix("UNC/") {
            format!("//{unc}")
        } else {
            path.to_string()
        };
        if path == "/" {
            path
        } else {
            path.trim_end_matches('/').to_string()
        }
    };
    let (left, right) = (normalize(left), normalize(right));
    if left.as_bytes().get(1) == Some(&b':') || left.starts_with("//") {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn read_local_identity(port: u16) -> anyhow::Result<Identity> {
    ensure!(port != 0, "节点端口无效");
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_millis(800)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(node_request(port).as_bytes())?;
    let mut bytes = Vec::new();
    stream.take(65_537).read_to_end(&mut bytes)?;
    parse_identity_response(&bytes)
}

pub(super) fn node_request(port: u16) -> String {
    format!(
        "GET /api/node HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    )
}

pub(super) fn parse_identity_response(bytes: &[u8]) -> anyhow::Result<Identity> {
    ensure!(bytes.len() <= 65_536, "节点身份响应超过大小限制");
    let split = bytes
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .context("节点没有返回 HTTP 响应")?;
    let headers = std::str::from_utf8(&bytes[..split])?;
    ensure!(
        headers
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            == Some("200"),
        "节点 /api/node 不可用（{}）；现有旧版服务需要手动升级",
        headers.lines().next().unwrap_or("未知状态")
    );
    // JSON 路由有 Content-Length；拒绝不确定的编码，不把代理错误页或流式内容当节点。
    ensure!(
        !headers
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:")),
        "节点身份响应使用了不支持的传输编码"
    );
    serde_json::from_slice(&bytes[split + 4..]).context("节点身份响应不是有效 JSON")
}

fn starting_recent(root: &Path) -> bool {
    std::fs::metadata(root.join(STARTING))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < READY_TIMEOUT + Duration::from_secs(10))
}

fn lock(root: &Path, name: &str, timeout: Duration) -> anyhow::Result<NodeProcessLock> {
    let file = private_open(&root.join(name))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(NodeProcessLock(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                ensure!(Instant::now() < deadline, "等待节点锁 {name} 超时");
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e).context("无法锁定节点数据目录"),
        }
    }
}

fn private_open(path: &Path) -> anyhow::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    private_permissions(path)?;
    Ok(file)
}

fn private_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let result = (|| {
        let mut file = private_open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Windows 的 rename 不能替换既有文件。调用方已持系统锁，先移除过期描述；
        // 该窗口由 ensure 锁和启动记录保护，不会触发第二次启动。
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&tmp, path)?;
        anyhow::Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn process_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, System};
    if pid == 0 {
        return false;
    }
    let pid = Pid::from_u32(pid);
    let mut system =
        System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()))
            .expect("sysinfo System");
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .is_some_and(|p| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
}

/// 旧版没有描述文件时也不允许在它的数据目录另起一份。只认 Falcon 主服务，排除
/// sessiond、桌面客户端和 node/service/respawn 帮手；显式目录优先，默认目录才看 HOME。
fn legacy_server(root: &Path) -> Option<(u32, bool)> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};
    let Ok(mut system) = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .with_environ(UpdateKind::Always)
                .with_cwd(UpdateKind::Always),
        ),
    ) else {
        return None;
    };
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.processes().iter().find_map(|(pid, p)| {
        if pid.as_u32() == std::process::id() {
            return None;
        }
        let exe = p.exe()?;
        let name = exe.file_name()?.to_string_lossy();
        if !is_server_name(&name) {
            return None;
        }
        let args = p.cmd();
        if args
            .get(1)
            .is_some_and(|a| matches!(a.to_str(), Some("node" | "service" | "respawn")))
        {
            return None;
        }
        let candidates = process_roots(args, p.environ(), p.cwd(), exe);
        if candidates.is_empty() {
            return Some((pid.as_u32(), true));
        }
        candidates
            .into_iter()
            .any(|candidate| std::fs::canonicalize(&candidate).unwrap_or(candidate) == root)
            .then_some((pid.as_u32(), false))
    })
}

/// 必须解析目标进程自己的 HOME 和 FALCON_DATA_DIR，不能拿 ensure 的 HOME 猜别的服务。
/// 同一账号可以通过隔离 sshd 或显式数据目录运行多个节点。这里只产路径，不读取其他库。
fn process_roots(
    args: &[std::ffi::OsString],
    environment: &[std::ffi::OsString],
    cwd: Option<&Path>,
    exe: &Path,
) -> Vec<PathBuf> {
    let env = |key: &str| {
        environment
            .iter()
            .filter_map(|v| v.to_str()?.split_once('='))
            .find_map(|(k, v)| {
                ((if cfg!(windows) {
                    k.eq_ignore_ascii_case(key)
                } else {
                    k == key
                }) && !v.is_empty())
                .then(|| PathBuf::from(v))
            })
    };
    let configured = args
        .windows(2)
        .filter(|pair| pair[0] == "--data-dir")
        .last()
        .map(|pair| PathBuf::from(&pair[1]))
        .or_else(|| env("FALCON_DATA_DIR"))
        .or_else(|| env("MOJITO_DATA_DIR"));
    if let Some(root) = configured {
        return if root.is_absolute() {
            vec![root]
        } else {
            cwd.map(|dir| vec![dir.join(root)]).unwrap_or_default()
        };
    }
    if let Some(home) = env("HOME").or_else(|| env("USERPROFILE")) {
        return vec![home.join(".falcon"), home.join(".mojito")];
    }
    // 某些系统不允许读进程环境；按标准安装位置仍能识别默认服务，但不猜当前调用方的 HOME。
    exe.parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "bin"))
        .and_then(Path::parent)
        .map(|root| vec![root.to_path_buf()])
        .unwrap_or_default()
}

pub(super) fn is_server_name(name: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name == "falcon"
        || name == "falcon-server"
        || name.starts_with("falcon-v")
        || name.starts_with("falcon-node-")
}

fn serve_args(root: &Path) -> Vec<String> {
    vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        "0".into(),
        "--data-dir".into(),
        root.to_string_lossy().into_owned(),
    ]
}

enum Launched {
    Child(std::process::Child),
    #[cfg(windows)]
    Pid(u32),
}

impl Launched {
    fn exited(&mut self) -> anyhow::Result<bool> {
        match self {
            Self::Child(child) => Ok(child.try_wait()?.is_some()),
            #[cfg(windows)]
            Self::Pid(pid) => Ok(!process_alive(*pid)),
        }
    }
}

fn launch(root: &Path) -> anyhow::Result<Launched> {
    let exe = std::env::current_exe()?;
    ensure!(
        exe.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_server_name),
        "当前进程不是 Falcon 服务端，不能用测试程序或客户端启动节点"
    );
    let mut command = Command::new(&exe);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("node.log"))?;
    command
        .args(serve_args(root))
        .current_dir(root)
        .env_remove("FALCON_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
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
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0100_0000);
    }
    match command.spawn() {
        Ok(child) => Ok(Launched::Child(child)),
        #[cfg(windows)]
        Err(e) if e.raw_os_error() == Some(5) => wmi_launch(&exe, root).map(Launched::Pid),
        Err(e) => Err(e).context("无法脱离启动节点"),
    }
}

#[cfg(windows)]
fn wmi_launch(exe: &Path, root: &Path) -> anyhow::Result<u32> {
    use base64::Engine as _;
    let args = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(serve_args(root))
        .map(|s| crate::sessiond::command::windows_quote_arg(&s))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "$ErrorActionPreference = 'Stop'; $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ CommandLine = {}; CurrentDirectory = {} }}; if ($r.ReturnValue -ne 0) {{ throw ('Win32_Process.Create failed: ' + $r.ReturnValue) }}; $r.ProcessId",
        crate::host::quote_powershell(&args),
        crate::host::quote_powershell(&root.to_string_lossy())
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut helper = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while helper.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = helper.kill();
            let _ = helper.wait();
            bail!("WMI 启动节点超时；保留启动记录，避免另起重复节点");
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let output = helper.wait_with_output()?;
    ensure!(
        output.status.success(),
        "WMI 启动节点失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("WMI 未返回节点进程ID")
}

fn log_tail(root: &Path) -> String {
    let path = root.join("node.log");
    let text = (|| {
        use std::io::{Seek, SeekFrom};
        let mut file = File::open(&path)?;
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(size.saturating_sub(4096)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        std::io::Result::Ok(String::from_utf8_lossy(&bytes).into_owned())
    })()
    .unwrap_or_default();
    let lines = text
        .lines()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    if lines.is_empty() {
        format!("日志 {} 为空", path.display())
    } else {
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(root: &str) -> Descriptor {
        Descriptor {
            node_id: "node-a".into(),
            pid: 123,
            port: 4948,
            version: "0.1.0".into(),
            protocol_version: 1,
            platform: "linux".into(),
            arch: "x86_64".into(),
            data_dir: root.into(),
            token: "a".repeat(64),
        }
    }

    #[test]
    fn descriptor_checks_identity_root_and_protocol_without_requiring_latest_version() {
        let node = descriptor("/tmp/node");
        let mut info = Identity {
            node_id: node.node_id.clone(),
            version: node.version.clone(),
            protocol_version: 1,
            data_dir: node.data_dir.clone(),
            platform: node.platform.clone(),
            arch: node.arch.clone(),
        };
        validate_identity(&node, &info, "/tmp/node").unwrap();
        info.node_id = "other".into();
        assert!(validate_identity(&node, &info, "/tmp/node").is_err());
        info.node_id = node.node_id.clone();
        info.protocol_version = 2;
        assert!(
            validate_identity(&node, &info, "/tmp/node")
                .unwrap_err()
                .to_string()
                .contains("协议不兼容")
        );
        info.protocol_version = 1;
        assert!(validate_identity(&node, &info, "/tmp/another").is_err());
        assert!(same_root("C:\\Users\\a\\.falcon", "c:/users/a/.falcon/"));
        assert!(same_root(r"\\?\C:\Users\a\.falcon", "c:/users/a/.falcon"));
        assert!(same_root(
            r"\\?\UNC\server\share\root",
            "//SERVER/share/root"
        ));
        assert!(!same_root("/", ""));
    }

    #[test]
    fn http_probe_rejects_old_server_and_malformed_response() {
        assert!(
            parse_identity_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .unwrap_err()
                .to_string()
                .contains("手动升级")
        );
        let body = br#"{"nodeId":"n","version":"1.2.3","protocolVersion":1,"platform":"linux","arch":"x86_64","dataDir":"/tmp/n"}"#;
        let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(body);
        assert_eq!(parse_identity_response(&response).unwrap().node_id, "n");
    }

    #[test]
    fn process_lock_rejects_duplicate_before_opening_database() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("fresh-node");
        let first = lock_node_process(&root).unwrap();
        assert!(!root.join("falcon.db").exists());
        assert!(
            lock_node_process(&root)
                .unwrap_err()
                .to_string()
                .contains("已由另一个")
        );
        drop(first);
        lock_node_process(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn guard_unlocks_while_fork_child_still_holds_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock_node_process(dir.path()).unwrap();
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // fork 后不进 Rust 分配器或锁；只用异步信号安全的系统调用等父进程验证完。
            unsafe {
                libc::close(pipe[1]);
                let mut byte = 0u8;
                while libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1) < 0 {}
                libc::_exit(0);
            }
        }
        unsafe {
            libc::close(pipe[0]);
        }
        drop(held);
        let reacquired = lock_node_process(dir.path());
        unsafe {
            libc::write(pipe[1], (&1u8 as *const u8).cast(), 1);
            libc::close(pipe[1]);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
        }
        reacquired.unwrap();
    }

    #[test]
    fn token_and_lock_are_private_stable_and_recover_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let token = node_token(dir.path()).unwrap();
        assert_eq!(node_token(dir.path()).unwrap(), token);
        let a = lock(dir.path(), "test.lock", Duration::ZERO).unwrap();
        assert!(lock(dir.path(), "test.lock", Duration::ZERO).is_err());
        drop(a);
        lock(dir.path(), "test.lock", Duration::ZERO).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("node.token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn existing_node_is_reused_without_launching_a_service() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_root(dir.path()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let db = Db::open(&root).unwrap();
        let config = ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            data_dir: root.clone(),
        };
        publish(&config, port, &db).unwrap();
        let json =
            serde_json::json!({ "nodeId": db.instance_id(), "version": env!("CARGO_PKG_VERSION"),
            "protocolVersion": 1, "platform": std::env::consts::OS, "arch": std::env::consts::ARCH, "dataDir": root.to_string_lossy() })
            .to_string();
        let http = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 1024];
            stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            )
            .unwrap();
        });
        let node = ensure_cli(&[
            "ensure".into(),
            "--data-dir".into(),
            root.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert_eq!(node.port, port);
        assert_eq!(node.node_id, db.instance_id());
        http.join().unwrap();
        assert!(!root.join(STARTING).exists());
        assert!(!root.join("node.log").exists());
    }

    #[test]
    fn operating_system_lock_excludes_another_process_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path(), "cross-process.lock", Duration::ZERO).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "nodes::bootstrap::tests::lock_worker",
                "--nocapture",
            ])
            .env("FALCON_NODE_LOCK_TEST_ROOT", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        drop(held);
        lock(dir.path(), "cross-process.lock", Duration::ZERO).unwrap();
    }

    #[test]
    fn lock_worker() {
        if let Some(root) = std::env::var_os("FALCON_NODE_LOCK_TEST_ROOT") {
            assert!(lock(Path::new(&root), "cross-process.lock", Duration::ZERO).is_err());
        }
    }

    #[test]
    fn legacy_detection_uses_each_process_environment_and_last_explicit_directory() {
        use std::ffi::OsString;
        let base = std::env::temp_dir().join("falcon-process-roots-vector");
        let home = base.join("another-owner");
        let exe = base.join("falcon");
        let args = vec![OsString::from("falcon")];
        let environment = vec![OsString::from(format!("HOME={}", home.display()))];
        assert_eq!(
            process_roots(&args, &environment, None, &exe),
            [home.join(".falcon"), home.join(".mojito")]
        );
        let configured = base.join("configured");
        let environment = vec![
            OsString::from(format!("HOME={}", home.display())),
            OsString::from(format!("FALCON_DATA_DIR={}", configured.display())),
        ];
        assert_eq!(process_roots(&args, &environment, None, &exe), [configured]);
        let args = vec![
            OsString::from("falcon"),
            OsString::from("--data-dir"),
            base.join("first").into_os_string(),
            OsString::from("--data-dir"),
            OsString::from("second"),
        ];
        let cwd = base.join("cwd");
        assert_eq!(
            process_roots(&args, &environment, Some(&cwd), &exe),
            [cwd.join("second")]
        );
        let installed = base.join("installed");
        assert_eq!(
            process_roots(&[], &[], None, &installed.join("bin").join("falcon")),
            [installed]
        );
    }

    #[test]
    fn cli_requires_explicit_absolute_root_and_background_server_is_loopback() {
        assert!(parse_cli(&["ensure".into()]).is_err());
        assert!(parse_cli(&["ensure".into(), "--data-dir".into(), "relative".into()]).is_err());
        let dir = tempfile::tempdir().unwrap();
        let args = serve_args(dir.path());
        assert_eq!(&args[..4], ["--host", "127.0.0.1", "--port", "0"]);
        assert!(!is_server_name("falcon-server-aabbccdd"));
        assert!(!is_server_name("falcon-desktop"));
        assert!(!is_server_name("falcon-app"));
        assert!(!is_server_name("falcon-app.exe"));
    }
}
