//! 内置插件「Docker」：容器的启停、日志与占用，镜像与磁盘清理。
//!
//! 看哪台机器同系统监控（[`machine`](super::machine)：跟随当前项目 / 本机 / 一台 SSH Host）。
//! 一律跑那台机器上的 `docker` 命令行（为什么不直连 API socket 见 cli.rs 顶部）：本机在后端本机
//! spawn（登录 shell 的环境，`docker context` 照样生效），远端经项目 / 主机的 SSH 链路 exec，
//! POSIX 远端先由一段 sh 找到 docker 在哪（非交互的 SSH 拿不到登录 PATH），Windows 远端经 PowerShell。
//!
//! 画面板跟着请求走，没有后台定时器。唯一的例外是**占用**（`docker stats --no-stream`，实测要等
//! 一两秒）：它不在画面板的路上跑，而是画面板时顺手在旁边起一次（[`Docker::kick_stats`]），结果留在
//! 这台机器的状态里给下一次画用——列表三秒一画，数字最多晚一轮；第一次没有数时面板说"采样中"并早点
//! 再画。没人看就不画，也就不采。
//!
//! 按钮动作（启停、删除、清理）回的是动作做完之后重画的面板，带一条 toast 说结果；docker 报的错原样
//! 放进 toast（"image is being used by running container …" 这类话比我们转述的清楚）。

mod cli;
mod parse;
mod render;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use falcon_proto::{HostKind, PluginActionInput, PluginInfo, PluginKind, PluginRenderInput, PluginScope, PluginToast, PluginTone, PluginView};

use self::cli::{ActionKind, Failure};
use self::parse::{Daemon, Stats};
use self::render::{CONTAINER, Header, LOG_CHARS, LOG_LINES, Tab};
use super::PluginError;
use super::machine::{RemoteError, Resolved, Target, remote_exec, resolve};
use super::runner::{self, Ended, Spawn};
use crate::db::Db;
use crate::engine::EngineHandle;
use crate::host::build_command_line;
use crate::sessions::local::BaseEnvFn;

pub const ID: &str = "docker";

/// 列表 / 详情 / 镜像页里一条读命令的时限
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// 远端多给一截：头一次要连 SSH、探宿主机
const REMOTE_EXTRA: Duration = Duration::from_secs(10);
/// `docker stats` 跑完之后隔这么久才再起一次（面板三秒一画，差不多每画一次采一次）
const STATS_EVERY: Duration = Duration::from_secs(2);
/// 这么久以前的占用不拿出来用（面板关了一阵子又打开）
const STATS_STALE: Duration = Duration::from_secs(20);
const STATS_TIMEOUT: Duration = Duration::from_secs(20);
/// `docker info`（标题上的版本、核数、内存总量）不常变
const DAEMON_TTL: Duration = Duration::from_secs(60);

pub fn info(enabled: bool) -> PluginInfo {
    PluginInfo {
        id: ID.into(),
        name: "Docker".into(),
        description: Some("容器的启停、日志与资源占用，镜像与磁盘清理；看 falcon 后端本机或 SSH 远端主机".into()),
        icon: "container".into(),
        kind: PluginKind::Builtin,
        // 机器选项缺省跟着焦点项目走
        scope: PluginScope::Project,
        refresh: Some(render::REFRESH),
        enabled,
        error: None,
        dir: None,
    }
}

/// 一台机器上的 docker 怎么跑
#[derive(Clone)]
enum Runner {
    Local { program: PathBuf, env: Arc<Vec<(String, String)>>, cwd: PathBuf },
    Remote { engine: EngineHandle, target: Arc<Target> },
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Out {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl Runner {
    fn local(&self) -> bool {
        matches!(self, Runner::Local { .. })
    }

    /// 跑一次 docker。`args` 拿到"远端是不是 Windows"（inspect 的模板在那边要换，cli.rs）。
    /// 非零退出码照常返回；`Err` 只有起不来、超时与链路故障
    async fn run<F>(&self, args: F, timeout: Duration) -> Result<Out, String>
    where
        F: FnOnce(bool) -> Vec<String> + Send + 'static,
    {
        match self {
            Runner::Local { program, env, cwd } => {
                let out = runner::run(Spawn {
                    program: program.clone(),
                    args: args(false),
                    cwd: cwd.clone(),
                    env: (**env).clone(),
                    stdin: Vec::new(),
                    timeout,
                })
                .await?;
                match out.ended {
                    Ended::Exited => Ok(Out { code: out.code, stdout: out.stdout, stderr: out.stderr }),
                    Ended::TimedOut => Err(format!("docker 在 {} 秒内没有跑完", timeout.as_secs())),
                    Ended::TooLarge => Err("docker 的输出太大".into()),
                }
            }
            Runner::Remote { engine, target } => {
                let res = remote_exec(engine, target, timeout + REMOTE_EXTRA, move |facts| {
                    let windows = facts.kind == HostKind::Windows;
                    let mut argv: Vec<String> = if windows {
                        vec!["docker".into()]
                    } else {
                        ["sh", "-c", cli::POSIX_RUN, "sh"].map(String::from).to_vec()
                    };
                    argv.extend(args(windows));
                    Ok(build_command_line(facts.kind, &argv, &[] as &[(&str, &str)], &[]))
                })
                .await;
                match res {
                    Ok(r) => Ok(Out { code: r.code, stdout: r.stdout, stderr: r.stderr }),
                    Err(RemoteError::Failed(e) | RemoteError::Unsupported(e)) => Err(e),
                }
            }
        }
    }
}

/// 本机的 docker 在哪：登录 shell 的 PATH，再试常见的安装位置（cli.rs）
fn find_local(env: &[(String, String)]) -> Option<PathBuf> {
    let get = |k: &str| env.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
    let exe = if cfg!(windows) { "docker.exe" } else { "docker" };
    let sep = if cfg!(windows) { ';' } else { ':' };
    let from_path = get("PATH").into_iter().flat_map(|p| p.split(sep)).filter(|d| !d.is_empty()).map(|d| Path::new(d).join(exe));
    let fallbacks = cli::local_candidates(get("HOME"), cfg!(windows)).into_iter().map(PathBuf::from);
    from_path.chain(fallbacks).find(|p| p.is_file())
}

#[derive(Default)]
struct MachineState {
    /// 最近一次 `docker stats` 的结果（按容器 id）与它采完的时刻
    stats: Option<(Instant, HashMap<String, Stats>)>,
    stats_running: bool,
    daemon: Option<(Instant, Daemon)>,
    /// 容器 id → 名字：toast 里说"已停止 web"而不是一串十六进制（删掉之后也还叫得出名字）
    names: HashMap<String, String>,
}

/// 锁里没有会 panic 的代码；真中毒了也照常用里面的数据
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 读命令跑不成：docker 自己报的错（分了类），或者连那台机器都没连上
enum Problem {
    Docker(Failure),
    Link(String),
}

impl Problem {
    fn of(out: &Out) -> Problem {
        Problem::Docker(cli::classify(out.code, &out.stderr))
    }
}

pub struct Docker {
    base_env: BaseEnvFn,
    /// 本机 docker 的工作目录（插件的数据目录：docker 不读 cwd，但总得给一个存在的）
    cwd: PathBuf,
    machines: Mutex<HashMap<String, Arc<Mutex<MachineState>>>>,
}

impl Docker {
    pub fn new(base_env: BaseEnvFn, cwd: PathBuf) -> Self {
        Docker { base_env, cwd, machines: Mutex::new(HashMap::new()) }
    }

    fn machine(&self, key: &str) -> Arc<Mutex<MachineState>> {
        lock(&self.machines).entry(key.to_string()).or_default().clone()
    }

    /// 本机找不到 docker 是 None
    async fn runner(&self, engine: &EngineHandle, target: &Target) -> Option<Runner> {
        match target {
            Target::Local => {
                let env = (self.base_env)().await;
                let program = find_local(&env)?;
                let _ = tokio::fs::create_dir_all(&self.cwd).await;
                Some(Runner::Local { program, env, cwd: self.cwd.clone() })
            }
            t => Some(Runner::Remote { engine: engine.clone(), target: Arc::new(t.clone()) }),
        }
    }

    /// 在旁边起一次 `docker stats`（见模块注释）。在跑着一次、或刚采过，就不起
    fn kick_stats(&self, st: &Arc<Mutex<MachineState>>, runner: &Runner) {
        {
            let mut s = lock(st);
            if s.stats_running || s.stats.as_ref().is_some_and(|(t, _)| t.elapsed() < STATS_EVERY) {
                return;
            }
            s.stats_running = true;
        }
        let (st, runner) = (st.clone(), runner.clone());
        tokio::spawn(async move {
            let res = runner.run(|_| cli::stats_args(), STATS_TIMEOUT).await;
            let mut s = lock(&st);
            s.stats_running = false;
            if let Ok(out) = res
                && out.ok()
            {
                s.stats = Some((Instant::now(), parse::stats(&out.stdout)));
            }
        });
    }

    fn fresh_stats(st: &Arc<Mutex<MachineState>>) -> Option<HashMap<String, Stats>> {
        lock(st).stats.as_ref().filter(|(t, _)| t.elapsed() < STATS_STALE).map(|(_, s)| s.clone())
    }

    /// 守护进程的版本等（缓存 [`DAEMON_TTL`]）；拿不到是 None，只是标题上少几个字
    async fn daemon(runner: &Runner, st: &Arc<Mutex<MachineState>>) -> Option<Daemon> {
        if let Some((t, d)) = &lock(st).daemon
            && t.elapsed() < DAEMON_TTL
        {
            return Some(d.clone());
        }
        let out = runner.run(|_| cli::info_args(), READ_TIMEOUT).await.ok().filter(Out::ok)?;
        let d = parse::daemon(&out.stdout)?;
        lock(st).daemon = Some((Instant::now(), d.clone()));
        Some(d)
    }

    pub async fn render(&self, db: &Db, engine: &EngineHandle, input: &PluginRenderInput) -> PluginView {
        let r = resolve(db, &input.params, input.project_id.as_deref());
        let Some(runner) = self.runner(engine, &r.target).await else {
            return render::failure_view(&Header { machine: &r.choice, daemon: None }, &Failure::NotInstalled, true);
        };
        self.view(&r, &runner, &input.params).await
    }

    /// 按面板参数画：详情、镜像页或容器列表
    async fn view(&self, r: &Resolved, runner: &Runner, params: &BTreeMap<String, String>) -> PluginView {
        let st = self.machine(&r.key);
        let container = params.get(CONTAINER).filter(|id| cli::valid_container_id(id)).cloned();
        let (daemon, body) = tokio::join!(Self::daemon(runner, &st), async {
            match (container, Tab::of(params)) {
                (Some(id), _) => self.detail(runner, &st, id).await,
                (None, Tab::Images) => Self::images(runner).await.map(|(images, df)| Body::Images(images, df)),
                (None, Tab::Containers) => self.containers(runner, &st).await.map(Body::Containers),
            }
        });
        let h = Header { machine: &r.choice, daemon: daemon.as_ref() };
        let now = now_secs();
        match body {
            Ok(Body::Containers(list)) => render::containers_view(&h, &list, Self::fresh_stats(&st).as_ref(), now),
            Ok(Body::Detail(c, logs)) => {
                let stats = Self::fresh_stats(&st);
                render::detail_view(&h, &c, stats.as_ref().and_then(|s| parse::stats_for(s, &c.id)), &logs, now)
            }
            // 详情里的容器没了（删了、换了机器）：回到列表，把参数清掉
            Ok(Body::Gone(list)) => {
                let mut v = render::containers_view(&h, &list, Self::fresh_stats(&st).as_ref(), now);
                v.params.insert(CONTAINER.into(), String::new());
                v
            }
            Ok(Body::Images(images, df)) => render::images_view(&h, &images, &df, now),
            Err(Problem::Docker(f)) => render::failure_view(&h, &f, runner.local()),
            Err(Problem::Link(e)) => render::link_view(&h, &e),
        }
    }

    async fn containers(&self, runner: &Runner, st: &Arc<Mutex<MachineState>>) -> Result<Vec<parse::Container>, Problem> {
        let ids = runner.run(|_| cli::ps_args(), READ_TIMEOUT).await.map_err(Problem::Link)?;
        if !ids.ok() {
            return Err(Problem::of(&ids));
        }
        let ids: Vec<String> = ids.stdout.split_whitespace().filter(|s| cli::valid_container_id(s)).map(str::to_string).collect();
        let list = if ids.is_empty() {
            Vec::new()
        } else {
            // 两条命令之间被删掉的容器 inspect 会报 No such object、退非零，其余的照样打出来：不看退出码
            let out = runner.run(move |win| cli::inspect_args(&ids, win), READ_TIMEOUT).await.map_err(Problem::Link)?;
            parse::containers(&out.stdout)
        };
        {
            let mut s = lock(st);
            s.names.extend(list.iter().map(|c| (c.id.clone(), c.name.clone())));
        }
        if list.iter().any(|c| c.status == parse::Status::Running) {
            self.kick_stats(st, runner);
        }
        Ok(list)
    }

    async fn detail(&self, runner: &Runner, st: &Arc<Mutex<MachineState>>, id: String) -> Result<Body, Problem> {
        let one = vec![id.clone()];
        let (inspect, logs) = tokio::join!(
            runner.run(move |win| cli::inspect_args(&one, win), READ_TIMEOUT),
            runner.run(move |_| cli::logs_args(&id, LOG_LINES), READ_TIMEOUT),
        );
        let inspect = inspect.map_err(Problem::Link)?;
        let Some(c) = parse::containers(&inspect.stdout).into_iter().next() else {
            if inspect.ok() || inspect.stderr.to_ascii_lowercase().contains("no such") {
                return self.containers(runner, st).await.map(Body::Gone);
            }
            return Err(Problem::of(&inspect));
        };
        lock(st).names.insert(c.id.clone(), c.name.clone());
        if c.status == parse::Status::Running {
            self.kick_stats(st, runner);
        }
        let logs = match logs {
            Ok(out) if out.ok() => Ok(parse::merge_logs(&out.stdout, &out.stderr, LOG_LINES, LOG_CHARS)),
            Ok(out) => Err(out.stderr.trim().to_string()),
            Err(e) => Err(e),
        };
        Ok(Body::Detail(c, logs))
    }

    async fn images(runner: &Runner) -> Result<(Vec<parse::Image>, Vec<parse::DiskUse>), Problem> {
        let (images, df) = tokio::join!(runner.run(|_| cli::images_args(), READ_TIMEOUT), runner.run(|_| cli::df_args(), READ_TIMEOUT));
        let images = images.map_err(Problem::Link)?;
        if !images.ok() {
            return Err(Problem::of(&images));
        }
        // 磁盘占用读不到只是少几张卡片
        let df = df.ok().filter(Out::ok).map(|o| parse::disk_use(&o.stdout)).unwrap_or_default();
        Ok((parse::images(&images.stdout), df))
    }

    /// 点了面板上的按钮：跑那条命令，再按（可能改过的）参数重画，带一条 toast 说结果
    pub async fn action(&self, db: &Db, engine: &EngineHandle, input: &PluginActionInput) -> Result<PluginView, PluginError> {
        let spec = cli::action_spec(&input.action, input.value.as_deref()).map_err(PluginError::BadInput)?;
        let r = resolve(db, &input.params, input.project_id.as_deref());
        let Some(runner) = self.runner(engine, &r.target).await else {
            return Ok(render::failure_view(&Header { machine: &r.choice, daemon: None }, &Failure::NotInstalled, true));
        };
        let st = self.machine(&r.key);
        let who = spec.target.as_ref().map(|t| match spec.kind {
            ActionKind::RemoveImage => t.clone(),
            _ => lock(&st).names.get(t).cloned().unwrap_or_else(|| t[..t.len().min(12)].to_string()),
        });
        let args = spec.args.clone();
        let res = runner.run(move |_| args, spec.timeout).await;
        let ok = matches!(&res, Ok(out) if out.ok());
        let label = |head: &str| match &who {
            Some(w) => format!("{head} {w}"),
            None => head.to_string(),
        };
        let toast = match res {
            Ok(out) if out.ok() => PluginToast {
                tone: Some(PluginTone::Success),
                title: label(spec.kind.done()),
                body: cli::reclaimed(&out.stdout).map(|r| format!("腾出 {r}")),
            },
            Ok(out) => {
                let body = out.stderr.trim();
                PluginToast {
                    tone: Some(PluginTone::Danger),
                    title: format!("{} 失败", label(spec.kind.verb())),
                    body: Some(if body.is_empty() { format!("docker 退出码 {}", out.code.map_or("?".into(), |c| c.to_string())) } else { body.to_string() }),
                }
            }
            Err(e) => PluginToast { tone: Some(PluginTone::Danger), title: format!("{} 失败", label(spec.kind.verb())), body: Some(e) },
        };
        // 在详情里删掉了这个容器：回到列表
        let mut params = input.params.clone();
        let leave = ok && spec.kind == ActionKind::Remove && params.get(CONTAINER) == spec.target.as_ref();
        if leave {
            params.insert(CONTAINER.into(), String::new());
        }
        // 停了 / 删了的容器，旧的占用别再拿出来
        lock(&st).stats = None;
        let mut view = self.view(&r, &runner, &params).await;
        if leave {
            view.params.insert(CONTAINER.into(), String::new());
        }
        view.toast = Some(toast);
        Ok(view)
    }
}

enum Body {
    Containers(Vec<parse::Container>),
    Detail(parse::Container, Result<String, String>),
    Gone(Vec<parse::Container>),
    Images(Vec<parse::Image>, Vec<parse::DiskUse>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_docker_on_path_then_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(if cfg!(windows) { "docker.exe" } else { "docker" });
        std::fs::write(&exe, "").unwrap();
        let env = vec![("PATH".to_string(), format!("/nonexistent{}{}", if cfg!(windows) { ";" } else { ":" }, bin.display()))];
        assert_eq!(find_local(&env), Some(exe));
        // HOME 下的 OrbStack
        if !cfg!(windows) {
            let home = dir.path().join("home");
            let orb = home.join(".orbstack/bin");
            std::fs::create_dir_all(&orb).unwrap();
            std::fs::write(orb.join("docker"), "").unwrap();
            let env = vec![("PATH".to_string(), "/nonexistent".to_string()), ("HOME".to_string(), home.display().to_string())];
            let found = find_local(&env).unwrap();
            // 这台机器上真装了 docker 时，/usr/local/bin 排在 HOME 下的前面
            assert!(found == orb.join("docker") || found.starts_with("/usr/local/bin") || found.starts_with("/opt/homebrew"), "{found:?}");
        }
    }
}
