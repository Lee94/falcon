//! 后端本机的 Tailscale：找出 CLI，跑 `status --json`，收成 [`falcon_proto::TailscaleDevices`]。
//!
//! 跟 cloudflared 一样只在 falcon 后端本机起进程，不经 shell，也不进会话引擎。
//! 添加远端主机用的是设备的 MagicDNS 名或 Tailscale IP，认证仍是普通 SSH。

mod command;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use falcon_proto::TailscaleDevices;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::sessions::local::local_base_env;

pub use command::{StatusRun, assemble, candidate_paths, resolve_bin, status_args, suggested_username};

const MAX_STDOUT: usize = 4 * 1024 * 1024;
const MAX_STDERR: usize = 16 * 1024;

/// `GET /api/tailscale/devices` 的实现
pub async fn list_devices() -> TailscaleDevices {
    let env = local_base_env().await;
    let user = suggested_username(&env);
    let bin = find_bin(&env);
    let run = match &bin {
        Some(bin) => Some(run_status(bin, &env).await),
        None => None,
    };
    assemble(bin.is_some(), run.as_ref(), &user)
}

fn find_bin(env: &[(String, String)]) -> Option<PathBuf> {
    let get = |key: &str| env.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str());
    let exe = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };
    let sep = if cfg!(windows) { ';' } else { ':' };
    let path_hits: Vec<PathBuf> = get("PATH")
        .unwrap_or("")
        .split(sep)
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(exe))
        .collect();
    let candidates: Vec<PathBuf> = candidate_paths(cfg!(windows)).into_iter().map(PathBuf::from).collect();
    resolve_bin(get("FALCON_TAILSCALE_BIN"), path_hits.iter().map(PathBuf::as_path), candidates.iter().map(PathBuf::as_path), |p| {
        p.is_file()
    })
}

async fn run_status(program: &Path, env: &[(String, String)]) -> StatusRun {
    let mut cmd = Command::new(program);
    cmd.args(status_args())
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // 单独成组。超时只结束这个 CLI；本机的 tailscaled 是另一个服务，不能跟着被结束
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return StatusRun::NotFound,
        Err(err) => return StatusRun::SpawnFailed(err.to_string()),
    };
    let mut out = child.stdout.take().expect("stdout 是 piped");
    let mut err = child.stderr.take().expect("stderr 是 piped");
    let read = async move {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let (big_out, big_err) = tokio::join!(
            read_capped(&mut out, &mut stdout, MAX_STDOUT),
            read_capped(&mut err, &mut stderr, MAX_STDERR),
        );
        if big_out || big_err {
            return ReadDone::TooLarge;
        }
        // 退出码不看：status --json 在未登录时有的版本是 1，stdout 仍是完整 JSON
        let _ = child.wait().await;
        ReadDone::Finished {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    };
    match tokio::time::timeout(Duration::from_secs(command::STATUS_TIMEOUT_SECS), read).await {
        Err(_) => StatusRun::TimedOut,
        Ok(ReadDone::TooLarge) => StatusRun::TooLarge,
        Ok(ReadDone::Finished { stdout, stderr }) => StatusRun::Output { stdout, stderr },
    }
}

enum ReadDone {
    TooLarge,
    Finished { stdout: String, stderr: String },
}

/// 读到 EOF 返回 false；超过 `cap` 返回 true，调用方结束进程
async fn read_capped(reader: &mut (impl AsyncRead + Unpin), buf: &mut Vec<u8>, cap: usize) -> bool {
    let mut tmp = [0u8; 8192];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > cap {
                    return true;
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_status_reads_stdout_json_from_the_given_bin() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tailscale");
        std::fs::write(&bin, "#!/bin/sh\nprintf '%s' '{\"BackendState\":\"Running\",\"Peer\":{}}'\n").unwrap();
        let mut perm = std::fs::metadata(&bin).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&bin, perm).unwrap();
        let env = vec![("USER".into(), "ada".into()), ("PATH".into(), "/usr/bin:/bin".into())];
        let run = run_status(&bin, &env).await;
        let list = assemble(true, Some(&run), "ada");
        assert!(list.available, "{run:?} {}", list.message.as_deref().unwrap_or(""));
        assert!(list.devices.is_empty());
    }
}
