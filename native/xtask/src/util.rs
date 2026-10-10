//! 各任务共用的小工具：仓库路径、跑外部命令、下载、校验。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// 仓库根（native/ 的上一层）。不用 canonicalize：Windows 上它给的是 `\\?\C:\…` 形式的
/// 路径，ISCC 之类的老工具不认
pub fn root() -> PathBuf {
    let xtask = Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask.parent().and_then(Path::parent).expect("仓库根").to_path_buf()
}

/// native/ 工作区
pub fn native() -> PathBuf {
    root().join("native")
}

/// 发布版本号：native 工作区的版本（Cargo.toml 的 workspace.package.version）
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 当前平台名（与 release 产物后缀一致）：darwin-arm64 / linux-x64 / win32-x64……
pub fn host_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// 跑一条命令，继承标准输入输出；非零退出码报错
pub fn run<I, S>(program: impl AsRef<OsStr>, args: I, cwd: Option<&Path>) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_env(program, args, cwd, &[])
}

/// 同 [`run`]，另加几个环境变量
pub fn run_env<I, S>(program: impl AsRef<OsStr>, args: I, cwd: Option<&Path>, env: &[(&str, &OsStr)]) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let status = cmd.status().with_context(|| format!("起不来 {}（装了吗？）", program.to_string_lossy()))?;
    if !status.success() {
        bail!("{} 失败（{status}）", program.to_string_lossy());
    }
    Ok(())
}

/// 跑一条命令拿 stdout（stderr 照常打到终端）；非零退出码报错
pub fn output<I, S>(program: impl AsRef<OsStr>, args: I, cwd: Option<&Path>) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let mut cmd = Command::new(program);
    cmd.args(args).stderr(Stdio::inherit());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().with_context(|| format!("起不来 {}（装了吗？）", program.to_string_lossy()))?;
    if !out.status.success() {
        bail!("{} 失败（{}）", program.to_string_lossy(), out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 把 `input` 喂给命令的 stdin，拿 stdout 的原始字节
pub fn pipe<I, S>(program: impl AsRef<OsStr>, args: I, input: &[u8]) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::io::Write as _;
    let program = program.as_ref();
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("起不来 {}（装了吗？）", program.to_string_lossy()))?;
    let mut stdin = child.stdin.take().expect("stdin");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    writer.join().expect("写 stdin 的线程")?;
    if !out.status.success() {
        bail!("{} 失败（{}）", program.to_string_lossy(), out.status);
    }
    Ok(out.stdout)
}

/// 命令在不在 PATH 上（跑一下 `--version` 之类不靠谱：有的工具没有这个参数）
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

/// 要求某个外部工具在 PATH 上，否则带着安装提示报错
pub fn require_tool(program: &str, hint: &str) -> Result<PathBuf> {
    which(program).with_context(|| format!("找不到 {program}：{hint}"))
}

/// 用 curl 下载（不为 xtask 拉一整套 TLS 栈）。先写 .part 再改名，中途失败不留半截文件
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    println!("  下载 {url}");
    run(
        "curl",
        [
            OsStr::new("-fsSL"),
            OsStr::new("--retry"),
            OsStr::new("3"),
            OsStr::new("-A"),
            OsStr::new("falcon-xtask"),
            OsStr::new("-o"),
            part.as_os_str(),
            OsStr::new(url),
        ],
        None,
    )?;
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// 写文件并报一句"新建 / 未变 / 更新"
pub fn write_reporting(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let old = std::fs::read(path).ok();
    std::fs::write(path, content)?;
    let tag = match old {
        None => "新建",
        Some(o) if o == content => "未变",
        Some(_) => "更新",
    };
    let rel = path.strip_prefix(root()).unwrap_or(path);
    println!("{tag} {}（{} 字节）", rel.display(), content.len());
    Ok(())
}

/// 临时目录（结束时删掉）
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Result<Self> {
        let mut rnd = [0u8; 6];
        // 不为一个目录名引随机数库：时间 + pid 足够不撞
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        rnd.copy_from_slice(&nanos.to_le_bytes()[..6]);
        let dir = std::env::temp_dir().join(format!("falcon-{tag}-{}-{}", std::process::id(), hex::encode(rnd)));
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
