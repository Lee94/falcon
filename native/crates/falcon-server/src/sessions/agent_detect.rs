//! 检查目标宿主机的可执行文件，不启动 agent，也不触发安装或 agent 登录。
//! launchd 的 PATH 通常不完整，因此使用与本地终端相同的交互登录 shell 环境。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use falcon_proto::{SESSION_AGENTS, SessionAgent};
use tokio::sync::Mutex;

use super::local::resolve_base_env;
use super::login_env::env_get;
use crate::exec::Exec;
use crate::host::{HostKind, build_command_line, encode_powershell};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
static CACHE: Mutex<Option<(Instant, Vec<SessionAgent>)>> = Mutex::const_new(None);

const PROBE_BEGIN: &str = "__FALCON_AGENTS_BEGIN__";
const PROBE_END: &str = "__FALCON_AGENTS_END__";

/// 远端使用与会话启动脚本相同的交互登录 shell，不能用 SSH exec 默认的精简 PATH。
pub fn remote_agents_probe(kind: HostKind, shell: &str) -> String {
    if kind == HostKind::Windows {
        let mut script = format!("Write-Output '{PROBE_BEGIN}'; ");
        for agent in SESSION_AGENTS {
            let bin = agent.as_str();
            script.push_str(&format!("if (Get-Command '{bin}' -CommandType Application -ErrorAction SilentlyContinue) {{ Write-Output '{bin}' }}; "));
        }
        script.push_str(&format!("Write-Output '{PROBE_END}'"));
        return encode_powershell(&script);
    }
    let mut script = format!("printf '%s\\n' '{PROBE_BEGIN}'; ");
    for agent in SESSION_AGENTS {
        let bin = agent.as_str();
        script.push_str(&format!("command -v {bin} >/dev/null 2>&1 && printf '%s\\n' '{bin}'; "));
    }
    script.push_str(&format!("printf '%s\\n' '{PROBE_END}'"));
    build_command_line(kind, &[shell, "-i", "-l", "-c", &script], &[("FALCON_RESOLVING_ENV", "1")], &[])
}

fn agents_from_probe(stdout: &str) -> Option<Vec<SessionAgent>> {
    let lines: Vec<_> = stdout.lines().map(str::trim).collect();
    let begin = lines.iter().position(|line| *line == PROBE_BEGIN)?;
    let end = lines.iter().skip(begin + 1).position(|line| *line == PROBE_END)? + begin + 1;
    Some(SESSION_AGENTS.iter().copied().filter(|agent| lines[begin + 1..end].contains(&agent.as_str())).collect())
}

pub async fn detect_remote_agents(exec: &dyn Exec, kind: HostKind, shell: &str) -> Result<Vec<SessionAgent>, String> {
    let command = remote_agents_probe(kind, shell);
    let result = tokio::time::timeout(Duration::from_secs(15), exec.exec(&command, None))
        .await.map_err(|_| "检测远端 CLI 超时".to_string())?
        .map_err(|err| err.to_string())?;
    if !result.ok() {
        return Err("无法读取远端 CLI 清单".into());
    }
    agents_from_probe(&result.stdout).ok_or_else(|| "远端 CLI 检测未完成".into())
}

pub async fn detect_local_agents() -> Vec<SessionAgent> {
    // 并发窗口共用一次探测；短期缓存避免重复读取用户的 rc 文件。
    let mut cache = CACHE.lock().await;
    if let Some((checked_at, agents)) = cache.as_ref()
        && checked_at.elapsed() < REFRESH_INTERVAL
    {
        return agents.clone();
    }
    // 每次刷新重新解析环境：安装器新增 PATH 目录后，不必重启后端。
    let env = resolve_base_env().await;
    let agents = tokio::task::spawn_blocking(move || agents_in_env(&env))
        .await
        .unwrap_or_default();
    *cache = Some((Instant::now(), agents.clone()));
    agents
}

fn agents_in_env(env: &[(String, String)]) -> Vec<SessionAgent> {
    let get = |key: &str| {
        if cfg!(windows) {
            env.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        } else {
            env_get(env, key)
        }
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(get("PATH").unwrap_or_default()).collect();
    let extensions = executable_extensions(cfg!(windows), get("PATHEXT"));
    agents_in_paths(&dirs, &extensions)
}

fn executable_extensions(windows: bool, pathext: Option<&str>) -> Vec<String> {
    if !windows {
        return vec![String::new()];
    }
    pathext
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .filter(|ext| ext.starts_with('.') && !ext.contains(['/', '\\']))
        .map(str::to_ascii_lowercase)
        .collect()
}

fn agents_in_paths(dirs: &[PathBuf], extensions: &[String]) -> Vec<SessionAgent> {
    SESSION_AGENTS
        .iter()
        .copied()
        .filter(|agent| {
            dirs.iter().filter(|dir| dir.is_absolute()).any(|dir| {
                extensions
                    .iter()
                    .any(|ext| is_executable(&dir.join(format!("{}{ext}", agent.as_str()))))
            })
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_probe_ignores_shell_noise_duplicates_and_unknown_tools() {
        let output = format!("agy\nwelcome\n{PROBE_BEGIN}\nomp\nagy\nomp\nfuture-agent\n{PROBE_END}\nclaude\n");
        assert_eq!(agents_from_probe(&output), Some(vec![SessionAgent::Agy, SessionAgent::Omp]));
        assert_eq!(agents_from_probe(&format!("{PROBE_BEGIN}\n{PROBE_END}\n")), Some(Vec::new()));
        assert_eq!(agents_from_probe("claude\n"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_probe_uses_interactive_login_shell_and_only_resolves_commands() {
        use std::os::unix::fs::PermissionsExt;
        use crate::host::quote_posix;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("agent bin");
        std::fs::create_dir(&bin).unwrap();
        for name in ["agy", "omp"] {
            let path = bin.join(name);
            std::fs::write(&path, "this agent must never run").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let shell = tmp.path().join("login shell's test");
        std::fs::write(&shell, format!(
            "#!/bin/sh\n[ \"$1\" = '-i' ] && [ \"$2\" = '-l' ] && [ \"$3\" = '-c' ] || exit 7\nexport PATH={}\nprintf 'greeting\\n'\n/bin/sh -c \"$4\"\nprintf 'logout\\n'\n",
            quote_posix(bin.to_str().unwrap())
        )).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let agents = detect_remote_agents(&crate::exec::LocalExec, HostKind::Posix, shell.to_str().unwrap()).await.unwrap();
        assert_eq!(agents, [SessionAgent::Agy, SessionAgent::Omp]);
    }

    #[test]
    fn windows_remote_probe_checks_applications_without_running_agents() {
        use base64::Engine as _;
        let command = remote_agents_probe(HostKind::Windows, "powershell.exe");
        let encoded = command.split_whitespace().last().unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let units: Vec<_> = raw.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("Get-Command 'agy' -CommandType Application"));
        assert!(script.contains("Get-Command 'omp' -CommandType Application"));
        assert!(script.contains(PROBE_BEGIN) && script.contains(PROBE_END));
        assert!(!script.contains("--version"));
    }

    #[test]
    fn detects_installed_executables_and_refreshes_after_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("user bin");
        std::fs::create_dir(&bin).unwrap();
        let env = vec![
            ("PATH".into(), std::env::join_paths([&bin, &bin]).unwrap().to_string_lossy().into_owned()),
            ("PATHEXT".into(), ".CMD;.EXE".into()),
        ];
        assert!(agents_in_env(&env).is_empty());
        let ext = if cfg!(windows) { ".cmd" } else { "" };
        for agent in [
            SessionAgent::Claude,
            SessionAgent::Gemini,
            SessionAgent::Cursor,
            SessionAgent::Agy,
            SessionAgent::Omp,
        ] {
            let file = bin.join(format!("{}{ext}", agent.as_str()));
            // 使用无法运行的内容，验证检测不会执行 CLI。
            std::fs::write(&file, "this must never run").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        // 同名目录不能被当成已安装。
        std::fs::create_dir(bin.join(format!("codex{ext}"))).unwrap();
        assert_eq!(
            agents_in_env(&env),
            [
                SessionAgent::Claude,
                SessionAgent::Gemini,
                SessionAgent::Cursor,
                SessionAgent::Agy,
                SessionAgent::Omp,
            ]
        );
        std::fs::remove_file(bin.join(format!("gemini{ext}"))).unwrap();
        assert_eq!(
            agents_in_env(&env),
            [SessionAgent::Claude, SessionAgent::Cursor, SessionAgent::Agy, SessionAgent::Omp]
        );
        for agent in [SessionAgent::Agy, SessionAgent::Omp] {
            std::fs::remove_file(bin.join(format!("{}{ext}", agent.as_str()))).unwrap();
        }
        assert_eq!(agents_in_env(&env), [SessionAgent::Claude, SessionAgent::Cursor]);
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_executable_files_and_broken_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("claude");
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(tmp.path().join("missing"), tmp.path().join("codex")).unwrap();
        assert!(agents_in_paths(&[tmp.path().into()], &[String::new()]).is_empty());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(file, tmp.path().join("opencode")).unwrap();
        assert_eq!(
            agents_in_paths(&[tmp.path().into()], &[String::new()]),
            [SessionAgent::Claude, SessionAgent::OpenCode]
        );
    }

    #[test]
    fn windows_supports_npm_wrappers_and_pathext() {
        assert_eq!(
            executable_extensions(true, Some(".EXE;.CMD;.BAT")),
            [".exe", ".cmd", ".bat"]
        );
        assert!(executable_extensions(true, None).contains(&".cmd".into()));
        assert_eq!(executable_extensions(false, None), [""]);
    }
}
