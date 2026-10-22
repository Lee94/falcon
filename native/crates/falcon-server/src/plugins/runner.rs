//! 起命令插件的进程：不经 shell 的 argv、登录环境 + 插件变量、请求写进 stdin、限时、限量。
//!
//! 与 meegle 的 `run_to_end` 同一个读法（两条流一起读、进程退出后给一小段宽限收尾），多了两件事：
//! - **限量**：stdout 超过 [`MAX_STDOUT`] 就结束进程——插件死循环打日志不该把后端内存吃光；
//!   stderr 只留最后 [`STDERR_TAIL`] 字节，失败时给人看的是最后那几行；
//! - **整组结束**（Unix）：插件多半是 shell 脚本，脚本里再起的 `curl` / `docker` 是孙进程，
//!   只杀脚本的话它们攥着管道接着跑。spawn 时自成一个进程组，超时就把整组 SIGKILL。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use falcon_proto::{PLUGIN_API, PluginEvent, PluginRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// stdout 的上限：一份面板文档有几十 KB 已经很大了
pub const MAX_STDOUT: usize = 1024 * 1024;
/// 失败时附上的 stderr 尾巴
pub const STDERR_TAIL: usize = 8 * 1024;
/// 进程退出后等孙进程把管道里剩下的写完 / 关掉的宽限
const STDIO_DRAIN: Duration = Duration::from_millis(300);

/// 一次 spawn 的全部输入
#[derive(Debug, Clone)]
pub struct Spawn {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited,
    TimedOut,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub ended: Ended,
}

/// 给插件进程的环境：基底（登录环境）去掉同名的 falcon 插件变量，再铺上这一次的请求。
/// 请求在 stdin 里也有一份完整的 JSON；环境变量是给 shell 脚本的——它们没有 jq 也能读参数
pub fn plugin_env(
    base: &[(String, String)],
    id: &str,
    dir: &str,
    data: &str,
    req: &PluginRequest,
) -> Vec<(String, String)> {
    // 后端若是从一个 falcon 插件 / 终端里起的（开发时常见），基底里会混着上一层的这些变量
    let ours = |k: &str| k.starts_with("FALCON_PLUGIN_") || k.starts_with("FALCON_PARAM_") || k.starts_with("FALCON_PROJECT_");
    let mut env: Vec<(String, String)> = base.iter().filter(|(k, _)| !ours(k)).cloned().collect();
    let mut set = |k: &str, v: &str| env.push((k.to_string(), v.to_string()));
    set("FALCON_PLUGIN_API", &PLUGIN_API.to_string());
    set("FALCON_PLUGIN_ID", id);
    set("FALCON_PLUGIN_DIR", dir);
    set("FALCON_PLUGIN_DATA", data);
    set("FALCON_PLUGIN_EVENT", req.event.as_str());
    if req.event == PluginEvent::Action {
        set("FALCON_PLUGIN_ACTION", req.action.as_deref().unwrap_or(""));
        set("FALCON_PLUGIN_VALUE", req.value.as_deref().unwrap_or(""));
    }
    for (k, v) in &req.params {
        set(&format!("FALCON_PARAM_{}", k.to_ascii_uppercase()), v);
    }
    if let Some(p) = &req.project {
        set("FALCON_PROJECT_ID", &p.id);
        set("FALCON_PROJECT_NAME", &p.name);
        set("FALCON_PROJECT_TYPE", &p.project_type);
        if let Some(d) = &p.working_dir {
            set("FALCON_PROJECT_DIR", d);
        }
        if let Some(s) = &p.ssh {
            set("FALCON_PROJECT_SSH", s);
        }
    }
    env
}

/// 失败时给人看的话（中间夹着 stderr 的最后几行）；跑成功了是 None
pub fn failure(out: &RunOutput, timeout: Duration) -> Option<String> {
    let head = match (out.ended, out.code) {
        (Ended::TimedOut, _) => format!("插件 {} 秒内没有跑完，已结束它", timeout.as_secs()),
        (Ended::TooLarge, _) => format!("插件输出超过 {} MB，已结束它", MAX_STDOUT / 1024 / 1024),
        (Ended::Exited, Some(0)) => return None,
        (Ended::Exited, Some(code)) => format!("插件退出码 {code}"),
        (Ended::Exited, None) => "插件被信号结束".to_string(),
    };
    let tail = stderr_tail(&out.stderr, 2000);
    Some(if tail.is_empty() { head } else { format!("{head}：\n{tail}") })
}

/// stderr 的最后 `max` 个字符（去掉首尾空白）
fn stderr_tail(s: &str, max: usize) -> String {
    let s = s.trim();
    let n = s.chars().count();
    if n <= max { s.to_string() } else { format!("…{}", s.chars().skip(n - max).collect::<String>()) }
}

/// 跑一次。`Err` 只有起不来（程序不存在、没有执行权限）
pub async fn run(spec: Spawn) -> Result<RunOutput, String> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    // windowsHide：别闪出一个黑色控制台窗口
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("找不到要跑的程序 {}", spec.program.display()),
        std::io::ErrorKind::PermissionDenied => format!("{} 没有执行权限（chmod +x？）", spec.program.display()),
        _ => format!("起不来 {}：{e}", spec.program.display()),
    })?;
    // 另开一个任务写 stdin：插件要是先打一大段输出再读输入，同一个任务里先写后读会互相等死。
    // 插件不读 stdin（多数 shell 插件只看环境变量）时写会撞 EPIPE，不算错
    if let Some(mut stdin) = child.stdin.take() {
        let body = spec.stdin;
        tokio::spawn(async move {
            let _ = stdin.write_all(&body).await;
            let _ = stdin.shutdown().await;
        });
    }
    let mut out = child.stdout.take().expect("stdout 是 piped");
    let mut err = child.stderr.take().expect("stderr 是 piped");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let (mut ob, mut eb) = (vec![0u8; 16 * 1024], vec![0u8; 8 * 1024]);
    let (mut out_open, mut err_open, mut code, mut exited) = (true, true, None, false);
    let deadline = tokio::time::sleep(spec.timeout);
    // 先占位，进程退出时 reset 成 STDIO_DRAIN 之后
    let drain = tokio::time::sleep(spec.timeout);
    tokio::pin!(deadline, drain);
    let ended = loop {
        if !out_open && !err_open && exited {
            break Ended::Exited;
        }
        tokio::select! {
            r = out.read(&mut ob), if out_open => match r {
                Ok(n) if n > 0 => {
                    stdout.extend_from_slice(&ob[..n]);
                    if stdout.len() > MAX_STDOUT {
                        break Ended::TooLarge;
                    }
                }
                _ => out_open = false,
            },
            r = err.read(&mut eb), if err_open => match r {
                Ok(n) if n > 0 => {
                    stderr.extend_from_slice(&eb[..n]);
                    if stderr.len() > STDERR_TAIL * 2 {
                        stderr.drain(..stderr.len() - STDERR_TAIL);
                    }
                }
                _ => err_open = false,
            },
            status = child.wait(), if !exited => {
                exited = true;
                code = status.ok().and_then(|s| s.code());
                drain.as_mut().reset(tokio::time::Instant::now() + STDIO_DRAIN);
            }
            _ = &mut drain, if exited => break Ended::Exited,
            _ = &mut deadline => break Ended::TimedOut,
        }
    };
    if ended != Ended::Exited || !exited {
        kill_group(&mut child);
    }
    Ok(RunOutput {
        code: if ended == Ended::Exited { code } else { None },
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        ended,
    })
}

fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // 负 pid = 整个进程组（spawn 时 process_group(0)，组号就是它自己的 pid）
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use falcon_proto::PluginProject;

    use super::*;

    fn req(event: PluginEvent) -> PluginRequest {
        PluginRequest {
            api: PLUGIN_API,
            event,
            action: Some("restart".into()),
            value: Some("web".into()),
            params: BTreeMap::from([("machine".to_string(), "local".to_string())]),
            project: Some(PluginProject {
                id: "p1".into(),
                name: "mojito".into(),
                project_type: "ssh".into(),
                working_dir: Some("/srv/app".into()),
                ssh: Some("fay@box:22".into()),
            }),
        }
    }

    fn get<'a>(env: &'a [(String, String)], k: &str) -> Option<&'a str> {
        env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn env_carries_the_request_and_drops_stale_falcon_vars() {
        let base = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("FALCON_PARAM_OLD".to_string(), "x".to_string()),
            ("FALCON_PLUGIN_ID".to_string(), "parent".to_string()),
            ("FALCON_SESSION_ID".to_string(), "s".to_string()),
        ];
        let env = plugin_env(&base, "hello", "/p/hello", "/d/hello", &req(PluginEvent::Action));
        assert_eq!(get(&env, "PATH"), Some("/usr/bin"));
        assert_eq!(get(&env, "FALCON_SESSION_ID"), Some("s"), "不是插件变量的照留");
        assert_eq!(get(&env, "FALCON_PARAM_OLD"), None);
        assert_eq!(get(&env, "FALCON_PLUGIN_ID"), Some("hello"));
        assert_eq!(env.iter().filter(|(k, _)| k == "FALCON_PLUGIN_ID").count(), 1);
        assert_eq!(get(&env, "FALCON_PLUGIN_EVENT"), Some("action"));
        assert_eq!(get(&env, "FALCON_PLUGIN_ACTION"), Some("restart"));
        assert_eq!(get(&env, "FALCON_PLUGIN_VALUE"), Some("web"));
        assert_eq!(get(&env, "FALCON_PARAM_MACHINE"), Some("local"));
        assert_eq!(get(&env, "FALCON_PROJECT_DIR"), Some("/srv/app"));
        assert_eq!(get(&env, "FALCON_PROJECT_SSH"), Some("fay@box:22"));
        let env = plugin_env(&[], "hello", "/p", "/d", &req(PluginEvent::Render));
        assert_eq!(get(&env, "FALCON_PLUGIN_ACTION"), None, "画面板时不带动作");
    }

    #[test]
    fn failure_messages() {
        let out = |ended, code, stderr: &str| RunOutput { code, stdout: String::new(), stderr: stderr.into(), ended };
        let t = Duration::from_secs(10);
        assert_eq!(failure(&out(Ended::Exited, Some(0), "warn"), t), None);
        assert_eq!(failure(&out(Ended::Exited, Some(2), " boom \n"), t).unwrap(), "插件退出码 2：\nboom");
        assert_eq!(failure(&out(Ended::Exited, None, ""), t).unwrap(), "插件被信号结束");
        assert!(failure(&out(Ended::TimedOut, None, ""), t).unwrap().contains("10 秒内没有跑完"));
        assert!(failure(&out(Ended::TooLarge, None, ""), t).unwrap().contains("超过 1 MB"));
        let long = "x".repeat(5000);
        assert!(failure(&out(Ended::Exited, Some(1), &long), t).unwrap().ends_with(&"x".repeat(2000)));
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt as _;

        use super::super::*;

        fn script(dir: &std::path::Path, body: &str) -> PathBuf {
            let p = dir.join("run.sh");
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn spawn(program: PathBuf, cwd: &std::path::Path, timeout: Duration) -> Spawn {
            Spawn {
                program,
                args: vec!["arg1".into()],
                cwd: cwd.to_path_buf(),
                env: vec![("PATH".into(), "/usr/bin:/bin".into()), ("GREETING".into(), "你好".into())],
                stdin: b"{\"api\":1}".to_vec(),
                timeout,
            }
        }

        #[tokio::test]
        async fn reads_stdin_env_args_and_cwd() {
            let dir = tempfile::tempdir().unwrap();
            let p = script(dir.path(), r#"read -r line; printf '%s|%s|%s|%s' "$line" "$GREETING" "$1" "$(basename "$PWD")"; echo oops >&2"#);
            let out = run(spawn(p, dir.path(), Duration::from_secs(5))).await.unwrap();
            let base = dir.path().file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(out.stdout, format!("{{\"api\":1}}|你好|arg1|{base}"));
            assert_eq!(out.stderr.trim(), "oops");
            assert_eq!((out.code, out.ended), (Some(0), Ended::Exited));
        }

        #[tokio::test]
        async fn exit_codes_are_values_not_errors() {
            let dir = tempfile::tempdir().unwrap();
            let p = script(dir.path(), "exit 3");
            let out = run(spawn(p, dir.path(), Duration::from_secs(5))).await.unwrap();
            assert_eq!(out.code, Some(3));
        }

        #[tokio::test]
        async fn timeout_kills_the_whole_group() {
            let dir = tempfile::tempdir().unwrap();
            // 孙进程攥着 stdout：只杀 sh 的话这里会一直等到 sleep 结束
            let p = script(dir.path(), "sleep 30 & sleep 30");
            let started = std::time::Instant::now();
            let out = run(spawn(p, dir.path(), Duration::from_millis(300))).await.unwrap();
            assert_eq!(out.ended, Ended::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[tokio::test]
        async fn runaway_output_is_cut_off() {
            let dir = tempfile::tempdir().unwrap();
            let p = script(dir.path(), "yes falcon");
            let out = run(spawn(p, dir.path(), Duration::from_secs(10))).await.unwrap();
            assert_eq!(out.ended, Ended::TooLarge);
        }

        #[tokio::test]
        async fn missing_and_non_executable_programs() {
            let dir = tempfile::tempdir().unwrap();
            let err = run(spawn(dir.path().join("nope"), dir.path(), Duration::from_secs(1))).await.unwrap_err();
            assert!(err.contains("找不到"), "{err}");
            let p = dir.path().join("plain.sh");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            let err = run(spawn(p, dir.path(), Duration::from_secs(1))).await.unwrap_err();
            assert!(err.contains("执行权限"), "{err}");
        }
    }
}
