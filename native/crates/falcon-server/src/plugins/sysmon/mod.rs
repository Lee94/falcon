//! 内置插件「系统监控」：CPU / 内存 / 负载 / 网络 / 磁盘 / 进程，带近三分钟的走势。
//!
//! 看哪台机器由面板上的「机器」选项（参数 `machine`）定：`auto` 跟着焦点项目走（SSH 项目看它的
//! 远端、其余看本机），`local` 是 falcon 后端本机，`host:<id>` 是一台已保存的 SSH Host。本机走
//! sysinfo（local.rs），Linux 远端走一条 /proc 探测脚本（linux.rs）；macOS / Windows 远端如实说
//! 不支持，机器选项照样在，好让人换一台。
//!
//! 采样**跟着请求走**，没有后台定时器：面板开着就两秒来一次，没人看就不采。几扇窗口同时开着同一台
//! 机器的面板时，[`REUSE`] 之内的请求共用上一次的采样（也保证 sysinfo 两次刷新之间够久）；
//! 隔了 [`GAP`] 没人看，走势与远端的累计计数作废重来——不然重新打开时第一格是"过去十分钟的平均"。

mod linux;
mod local;
mod render;
mod sample;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use falcon_proto::{HostKind, PluginInfo, PluginKind, PluginRenderInput, PluginScope, PluginTone, PluginView};

use self::local::LocalSampler;
use self::render::{HISTORY, Point};
use self::sample::Snapshot;
use super::machine::{RemoteError, Target, remote_exec, resolve};
use crate::db::Db;
use crate::engine::EngineHandle;
use crate::host::build_command_line;

pub const ID: &str = "sysmon";

/// 这么久之内的请求共用上一次采样
const REUSE: Duration = Duration::from_millis(1500);
/// 隔这么久没人看，走势作废
const GAP: Duration = Duration::from_secs(15);
/// 远端第一次探测没有"上一次"：隔这么久再探一次，算出第一格 CPU / 网速
const FIRST_DELTA: Duration = Duration::from_millis(600);
/// 一次远端探测（含连 SSH）的上限
const REMOTE_TIMEOUT: Duration = Duration::from_secs(12);

pub fn info(enabled: bool) -> PluginInfo {
    PluginInfo {
        id: ID.into(),
        name: "系统监控".into(),
        description: Some("CPU、内存、负载、网络、磁盘与占用最高的进程；看 falcon 后端本机或 Linux 远端主机".into()),
        icon: "activity".into(),
        kind: PluginKind::Builtin,
        // 机器选项缺省跟着焦点项目走
        scope: PluginScope::Project,
        refresh: Some(render::REFRESH),
        enabled,
        error: None,
        dir: None,
    }
}

#[derive(Default)]
struct MachineState {
    last: Option<(Instant, Snapshot)>,
    history: VecDeque<Point>,
    /// 远端上一次的累计计数
    prev: Option<(Instant, linux::Counters)>,
}

#[derive(Default)]
pub struct Sysmon {
    local: Arc<Mutex<Option<LocalSampler>>>,
    machines: Mutex<HashMap<String, Arc<tokio::sync::Mutex<MachineState>>>>,
}

/// 锁里没有会 panic 的代码；真中毒了也照常用里面的数据
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sysmon {
    fn machine(&self, key: &str) -> Arc<tokio::sync::Mutex<MachineState>> {
        lock(&self.machines).entry(key.to_string()).or_default().clone()
    }

    pub async fn render(&self, db: &Db, engine: &EngineHandle, input: &PluginRenderInput) -> PluginView {
        let r = resolve(db, &input.params, input.project_id.as_deref());
        // 同一台机器的采样排队：后到的请求等前一个采完，再按 REUSE 直接用它的结果
        let cell = self.machine(&r.key);
        let mut st = cell.lock().await;
        let now = Instant::now();
        if let Some((t, snap)) = &st.last
            && now.duration_since(*t) < REUSE
        {
            return render::view(&r.choice, snap, &st.history);
        }
        if st.last.as_ref().is_some_and(|(t, _)| now.duration_since(*t) > GAP) {
            st.history.clear();
            st.prev = None;
        }
        let label = r.choice.label.clone();
        let snap = match r.target {
            Target::Local => {
                let local = self.local.clone();
                let sampled = tokio::task::spawn_blocking(move || lock(&local).get_or_insert_with(LocalSampler::new).sample()).await;
                match sampled {
                    Ok(s) => s,
                    Err(e) => return render::notice_view(&r.choice, PluginTone::Danger, format!("采样失败：{e}")),
                }
            }
            target => match sample_remote(engine, target, &mut st).await {
                Ok(s) => s,
                Err(RemoteError::Unsupported(os)) => {
                    let text = format!("{label} 是 {os}。系统监控暂时只看得了 falcon 后端本机与 Linux 远端主机。");
                    return render::notice_view(&r.choice, PluginTone::Warning, text);
                }
                Err(RemoteError::Failed(e)) => {
                    return render::notice_view(&r.choice, PluginTone::Danger, format!("读不到 {label} 的系统状态：{e}"));
                }
            },
        };
        let now = Instant::now();
        st.history.push_back(Point::of(&snap));
        while st.history.len() > HISTORY {
            st.history.pop_front();
        }
        let view = render::view(&r.choice, &snap, &st.history);
        st.last = Some((now, snap));
        view
    }
}

async fn sample_remote(engine: &EngineHandle, target: Target, st: &mut MachineState) -> Result<Snapshot, RemoteError> {
    let probe_once = || async {
        let out = probe(engine, &target).await?;
        linux::parse_probe(&out).map_err(RemoteError::Failed)
    };
    if st.prev.is_none() {
        let first = probe_once().await?;
        st.prev = Some((Instant::now(), first));
        tokio::time::sleep(FIRST_DELTA).await;
    }
    let cur = probe_once().await?;
    let now = Instant::now();
    let snap = {
        let prev = st.prev.as_ref().map(|(t, c)| (c, now.duration_since(*t).as_secs_f64()));
        linux::snapshot(&cur, prev)
    };
    st.prev = Some((now, cur));
    Ok(snap)
}

/// 在远端跑一次探测脚本，回它的输出
async fn probe(engine: &EngineHandle, target: &Target) -> Result<String, RemoteError> {
    let res = remote_exec(engine, target, REMOTE_TIMEOUT, |facts| {
        if facts.kind == HostKind::Windows {
            return Err(RemoteError::Unsupported("Windows".into()));
        }
        let uname = facts.uname.clone().unwrap_or_default();
        let os = uname.split_whitespace().next().unwrap_or("").to_string();
        if os != "Linux" {
            return Err(RemoteError::Unsupported(if os.is_empty() { "未知系统".into() } else { os }));
        }
        // 经 sh -c 跑：远端登录 shell 是 fish / nu 也不影响脚本的语法
        Ok(build_command_line(HostKind::Posix, &["sh", "-c", linux::PROBE], &[] as &[(&str, &str)], &[]))
    })
    .await?;
    // 退出码只是旁证：最后一条 ps 两种写法都不认时也是非零，前面的段照样有用
    if res.stdout.trim().is_empty() {
        return Err(RemoteError::Failed(if res.stderr.trim().is_empty() { "探测脚本没有输出".into() } else { res.stderr.trim().to_string() }));
    }
    Ok(res.stdout)
}
