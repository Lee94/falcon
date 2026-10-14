//! 把守护进程放到宿主机上、确认它跑得起来（设计文档 10.5）。
//!
//! 二进制**由后端推过去**，不让宿主机自己下载（同 px0）：宿主机不必能出网、不必有
//! curl / tar；推过去的字节由后端从自己的构建里拿，文件名带内容哈希，所以"在不在"就是
//! "对不对"，最后再握一次手（`version --json`）挡住 noexec 挂载、架构不符、文件截断。
//!
//! - 本机：拷到 `<dataDir>/bin/`（[`super::bin::install_local`]）后握手；
//! - SSH POSIX：`exec_with_input` 把字节灌进 `cat > f.partial`，改名到位；
//! - SSH Windows：PowerShell 读不了 stdin 上的原始字节，走 ADR 0008 的 base64 行协议。
//!
//! 阶段经 `/ws/install/{project_id}` 上报：probing → uploading → verifying。

use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use falcon_proto::sessiond::VersionInfo;
use falcon_proto::{HostKind, InstallFailure, InstallStage, is_auto_retryable};
use tokio_util::sync::CancellationToken;

use super::bin::{SessiondBinary, binary_for, install_local};
use super::command::{
    SessiondTarget, binary_path, exec_command, exists_command, local_target, posix_install_command, version_argv,
    version_matches, windows_install_command,
};
use crate::exec::Exec as _;
use crate::sessions::ssh::SshLink;

/// 阶段回调：(阶段, 第几轮, 可读的远端命令)。attempt 从 1 起，>1 表示这一轮是自动重试
pub type StageFn<'a> = &'a dyn Fn(InstallStage, u32, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InstallError {
    pub reason: InstallFailure,
    pub message: String,
    pub detail: Option<String>,
}

impl InstallError {
    pub fn new(reason: InstallFailure, detail: Option<String>) -> Self {
        InstallError { reason, message: failure_text(reason).to_string(), detail }
    }

    fn cancelled() -> Self {
        Self::new(InstallFailure::Cancelled, None)
    }
}

pub fn failure_text(reason: InstallFailure) -> &'static str {
    match reason {
        InstallFailure::ArchUnsupported => "宿主机的系统 / 架构没有会话守护进程的构建",
        InstallFailure::DirNotWritable => "无法在宿主机的 falcon 目录里放会话守护进程",
        InstallFailure::ProbeFailed => "无法探测宿主机（命令未跑通或系统未识别）",
        InstallFailure::UploadFailed => "推送会话守护进程时中断",
        InstallFailure::VerifyFailed => "会话守护进程无法在宿主机上执行（可能是 noexec 挂载或架构不兼容）",
        InstallFailure::SessiondUnavailable => "这个 falcon 构建里没有宿主机需要的会话守护进程",
        InstallFailure::Cancelled | InstallFailure::Unknown => "已取消",
    }
}

/// 单次请求内的自动重试次数上限（含首次尝试）
pub const MAX_ATTEMPTS: u32 = 3;

/// 各次重试前的退避：够长到熬过一次网络抖动，又不至于让用户以为卡死了
const BACKOFF_MS: [u64; 2] = [1500, 4000];

/// 有限次自动重试，只对瞬时故障生效（见 falcon_proto::AUTO_RETRY_FAILURES）。
/// 取消在退避等待期间立即生效——用户点了取消就不该再等几秒。
pub async fn with_install_retry<T, F, Fut>(mut attempt_fn: F, cancel: Option<&CancellationToken>) -> Result<T, InstallError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, InstallError>>,
{
    let mut attempt = 1;
    loop {
        match attempt_fn(attempt).await {
            Ok(v) => return Ok(v),
            Err(err) => {
                let aborted = cancel.is_some_and(CancellationToken::is_cancelled);
                if !is_auto_retryable(err.reason) || aborted || attempt >= MAX_ATTEMPTS {
                    return Err(err);
                }
                let ms = BACKOFF_MS.get(attempt as usize - 1).copied().unwrap_or(BACKOFF_MS[BACKOFF_MS.len() - 1]);
                let wait = tokio::time::sleep(Duration::from_millis(ms));
                match cancel {
                    Some(token) => tokio::select! {
                        _ = wait => {}
                        _ = token.cancelled() => return Err(InstallError::cancelled()),
                    },
                    None => wait.await,
                }
                attempt += 1;
            }
        }
    }
}

/// 宿主机上已就位、握过手的守护进程
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployed {
    pub kind: HostKind,
    /// falcon 根目录（守护进程的 `--root`）
    pub root: String,
    /// 宿主机上二进制的绝对路径
    pub bin: String,
    pub target: SessiondTarget,
    /// 这份二进制里 libghostty-vt 的 commit：holder 的 VT 进程不是它时可以热升级（设计文档 2.3）
    pub ghostty: String,
}

fn ghostty_of(stdout: &str) -> String {
    stdout.lines().find_map(|l| serde_json::from_str::<VersionInfo>(l.trim()).ok()).map(|v| v.ghostty).unwrap_or_default()
}

fn binary(target: SessiondTarget) -> Result<SessiondBinary, InstallError> {
    binary_for(target).ok_or_else(|| {
        InstallError::new(
            InstallFailure::SessiondUnavailable,
            Some(format!("缺 {} 的守护进程：开发构建先跑 `cargo xtask sessiond --targets {}`", target.triple(), target.triple())),
        )
    })
}

/// 后端本机：拷进数据目录、握手
pub async fn ensure_local(data_dir: &Path) -> Result<Deployed, InstallError> {
    let target = local_target().ok_or_else(|| InstallError::new(InstallFailure::ArchUnsupported, None))?;
    let bin = binary(target)?;
    let path = install_local(data_dir, &bin).map_err(|e| InstallError::new(InstallFailure::DirNotWritable, Some(e.to_string())))?;
    let out = tokio::process::Command::new(&path)
        .args(["version", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| InstallError::new(InstallFailure::VerifyFailed, Some(e.to_string())))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !version_matches(&stdout, target) {
        return Err(InstallError::new(
            InstallFailure::VerifyFailed,
            Some(format!("{}{}", stdout.trim(), String::from_utf8_lossy(&out.stderr).trim())),
        ));
    }
    let root = std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    Ok(Deployed {
        kind: if cfg!(windows) { HostKind::Windows } else { HostKind::Posix },
        root: root.to_string_lossy().into_owned(),
        bin: path.to_string_lossy().into_owned(),
        target,
        ghostty: ghostty_of(&stdout),
    })
}

/// 远端：在就握手，不在就推，推完再握手。`attempt` 只用于上报阶段
pub async fn ensure_remote(
    link: &Rc<SshLink>,
    kind: HostKind,
    root: &str,
    target: SessiondTarget,
    on_stage: Option<StageFn<'_>>,
    cancel: Option<&CancellationToken>,
    attempt: u32,
) -> Result<Deployed, InstallError> {
    let stage = |s: InstallStage, cmd: String| {
        if let Some(f) = on_stage {
            f(s, attempt, Some(cmd));
        }
    };
    let aborted = || cancel.is_some_and(CancellationToken::is_cancelled);
    let bin = binary(target)?;
    let path = binary_path(kind, root, &bin.file_name());
    let version_cmd = exec_command(kind, &version_argv(&path));
    let deployed = |stdout: &str| Deployed { kind, root: root.to_string(), bin: path.clone(), target, ghostty: ghostty_of(stdout) };

    // 已经在了（文件名带内容哈希：在就是对的）：握手通过就直接用
    stage(InstallStage::Verifying, format!("{path} version --json"));
    let exists = link.exec(&exists_command(kind, &path), cancel).await;
    if aborted() {
        return Err(InstallError::cancelled());
    }
    if exists.as_ref().is_ok_and(|r| r.stdout.contains("yes")) {
        match link.exec(&version_cmd, cancel).await {
            Ok(r) if r.ok() && version_matches(&r.stdout, target) => return Ok(deployed(&r.stdout)),
            // 在但跑不起来：多半是上次推到一半的残骸（不会：先写 .partial 再改名），
            // 或者 noexec；重推一次再握手，结论以重推后的为准
            _ => {}
        }
    } else if let Err(e) = exists {
        return Err(InstallError::new(InstallFailure::ProbeFailed, Some(format!("{e:#}"))));
    }

    stage(InstallStage::Uploading, format!("{} 字节 → {path}", bin.bytes.len()));
    let (cmd, input) = match kind {
        HostKind::Windows => {
            let mut enc = crate::transfer::Base64LineEncoder::new();
            let mut lines = enc.feed(&bin.bytes);
            lines.extend(enc.finish());
            (windows_install_command(&path, bin.bytes.len() as u64), lines)
        }
        _ => (posix_install_command(&path), bin.bytes.to_vec()),
    };
    let res = link
        .exec_with_input(&cmd, &input)
        .await
        .map_err(|e| InstallError::new(InstallFailure::UploadFailed, Some(format!("{e:#}"))))?;
    if aborted() {
        return Err(InstallError::cancelled());
    }
    if !res.ok() {
        let text = [res.stderr.trim(), res.stdout.trim()].into_iter().find(|s| !s.is_empty()).unwrap_or("").to_string();
        let detail = if text.is_empty() { format!("远端命令退出码 {}", res.code.map_or("null".into(), |c| c.to_string())) } else { text };
        return Err(InstallError::new(upload_failure(&detail), Some(detail)));
    }

    stage(InstallStage::Verifying, format!("{path} version --json"));
    match link.exec(&version_cmd, cancel).await {
        Ok(r) if r.ok() && version_matches(&r.stdout, target) => Ok(deployed(&r.stdout)),
        Ok(r) => {
            let text = [r.stderr.trim(), r.stdout.trim()].into_iter().find(|s| !s.is_empty()).unwrap_or("没有输出").to_string();
            Err(InstallError::new(verify_failure(&text), Some(text)))
        }
        // 握手那一瞬链路断了与"二进制跑不动"是两码事：前者重试有救
        Err(e) => Err(InstallError::new(InstallFailure::ProbeFailed, Some(format!("验证握手时与宿主机失去联系：{e:#}")))),
    }
}

/// 推送命令失败时按报错分类：稳定的环境事实（目录写不了）重试白搭，其余都按瞬时故障自动重试。
///
/// 只认确实说明"这里写不了"的几种：权限不够（EACCES / EPERM）、只读文件系统、Windows 的拒绝访问。
/// 别按 "access" 一个词判——GNU coreutils 的 `chmod: cannot access '…': No such file or directory`、
/// .NET 的 "cannot access the file … because it is being used by another process" 都是另一条推送
/// 踩了同一个文件，下一次就好（2026-10-14 实际撞上过：被认成目录写不了，不重试，接回的会话被判没了）
fn upload_failure(text: &str) -> InstallFailure {
    let lower = text.to_ascii_lowercase();
    let stable = lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("read-only file system")
        || lower.contains("access is denied")
        || (lower.contains("access to the path") && lower.contains("denied"));
    if stable { InstallFailure::DirNotWritable } else { InstallFailure::UploadFailed }
}

/// 握手失败的分类。Text file busy 是有人还开着这个文件在写（并发的推送）——等一下再推一次就好，
/// 不是"这份二进制在这台机器上跑不起来"
fn verify_failure(text: &str) -> InstallFailure {
    if text.to_ascii_lowercase().contains("text file busy") { InstallFailure::UploadFailed } else { InstallFailure::VerifyFailed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retry_only_transient_failures() {
        let calls = std::cell::Cell::new(0);
        let r: Result<(), _> = with_install_retry(
            |_| {
                calls.set(calls.get() + 1);
                async { Err(InstallError::new(InstallFailure::VerifyFailed, None)) }
            },
            None,
        )
        .await;
        assert_eq!((r.unwrap_err().reason, calls.get()), (InstallFailure::VerifyFailed, 1));
    }

    #[test]
    fn only_real_permission_problems_count_as_dir_not_writable() {
        for stable in [
            "mkdir: cannot create directory '/h/.falcon/bin': Permission denied",
            "chmod: changing permissions of '/h/.falcon/bin': Operation not permitted",
            "sh: 1: cannot create /h/.falcon/bin/sd.partial.42: Read-only file system",
            "Access to the path 'C:\\Users\\a\\.falcon\\bin\\sd.exe.partial.7' is denied.",
            "Access is denied.",
        ] {
            assert_eq!(upload_failure(stable), InstallFailure::DirNotWritable, "{stable}");
        }
        for transient in [
            // 两条推送共用一个临时文件时实际报出来的（GNU coreutils）
            "chmod: cannot access '/h/.falcon/bin/falcon-sessiond-0.3.3-694c46fe.partial': No such file or directory",
            "mv: cannot stat '/h/.falcon/bin/sd.partial': No such file or directory",
            "The process cannot access the file 'C:\\h\\sd.exe.partial' because it is being used by another process.",
            "sh: 1: cannot create /h/.falcon/bin/sd.partial: Text file busy",
            "ESHORT",
            "远端命令退出码 1",
        ] {
            assert_eq!(upload_failure(transient), InstallFailure::UploadFailed, "{transient}");
        }
        assert_eq!(verify_failure("sh: 1: /h/.falcon/bin/sd: Text file busy"), InstallFailure::UploadFailed);
        assert_eq!(verify_failure("sh: 1: /h/.falcon/bin/sd: Permission denied"), InstallFailure::VerifyFailed);
    }

    #[test]
    fn ghostty_commit_is_read_from_the_handshake() {
        assert_eq!(ghostty_of("noise\n{\"version\":\"x\",\"protoMin\":1,\"protoMax\":1,\"ghostty\":\"abc\",\"target\":\"t\"}\n"), "abc");
        assert_eq!(ghostty_of(""), "");
    }
}
