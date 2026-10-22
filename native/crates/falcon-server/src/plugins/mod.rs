//! 右侧栏插件的宿主（ADR 0025，作者手册 `docs/plugins.md`）。
//!
//! 两种插件，对客户端是同一个样子（`/api/plugins/*` 回 [`PluginView`]）：
//! - **内置**（`builtin`）：falcon-server 自己实现：系统监控（[`sysmon`]）与 Docker（[`docker`]），
//!   看哪台机器由它们共用的 [`machine`] 定；
//! - **命令**（`command`）：用户把一个目录放进 `<数据目录>/plugins/<id>/`，里面一份 `plugin.json`
//!   说要跑什么。服务端在**后端本机**起进程（不经 shell、不跟项目走——与 meegle / cloudflared 同一个
//!   口径），请求经 stdin 与环境变量交过去，stdout 是面板文档。
//!
//! 插件不经 API 安装：能往数据目录里放可执行文件的人，本来就能以 falcon 的身份跑任何东西；
//! 而能登录 falcon 的人只能触发已经装好的插件、传的只有白名单过的参数——argv 永远来自磁盘上的
//! 清单，不来自请求。清单每次请求都重新读：改了 `plugin.json` 立刻生效，不用重启后端。
//!
//! 同一个插件、同一份请求（参数 + 焦点项目）同时只跑一个进程：两扇窗口都开着同一个面板时
//! 共用一次结果（[`PluginHost::render`]）。点按钮（action）不合并——两次点击就是两次。

pub mod docker;
pub mod fmt;
pub mod machine;
pub mod manifest;
pub mod runner;
pub mod sysmon;
pub mod view;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use falcon_proto::{
    PLUGIN_API, PluginActionInput, PluginEvent, PluginInfo, PluginKind, PluginList, PluginProject, PluginRenderInput,
    PluginRequest, PluginView,
};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};

use self::manifest::{BUILTIN_IDS, MAX_MANIFEST_BYTES, MAX_PLUGINS, Manifest, Output, valid_action, valid_id, valid_param};
use crate::db::Db;
use crate::engine::EngineHandle;
use crate::sessions::local::BaseEnvFn;

/// settings 表里记停用了哪些插件（JSON 字符串数组）。记"停用的"而不是"启用的"：新放进去的插件
/// 与内置插件缺省就是开着的
const DISABLED_KEY: &str = "plugins.disabled";
/// 一次请求最多带多少个参数、每个值多长
const MAX_PARAMS: usize = 16;
const MAX_PARAM_VALUE: usize = 1024;

/// 插件请求失败的几种。HTTP 状态码在 api/plugins.rs 里对上
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// 404：没有这个插件
    NotFound(String),
    /// 409：停用了，或清单写坏了（设置页里看得到原因）
    Unavailable(String),
    /// 400：参数 / 动作 id 不合法
    BadInput(String),
    /// 502：插件跑了但失败了（退出码非零、超时、输出不是合法 JSON）
    Failed(String),
}

impl PluginError {
    pub fn message(&self) -> &str {
        match self {
            PluginError::NotFound(m) | PluginError::Unavailable(m) | PluginError::BadInput(m) | PluginError::Failed(m) => m,
        }
    }
}

type RunFuture = Shared<BoxFuture<'static, Result<PluginView, PluginError>>>;

struct Inner {
    /// `<数据目录>/plugins`：命令插件放这里
    dir: PathBuf,
    /// `<数据目录>/plugin-data`：每个插件一个子目录，插件自己存状态（`FALCON_PLUGIN_DATA`）
    data: PathBuf,
    base_env: BaseEnvFn,
    sysmon: sysmon::Sysmon,
    docker: docker::Docker,
    /// 在跑的画面板请求（见模块注释），跑完自己摘掉
    inflight: Mutex<HashMap<String, RunFuture>>,
}

/// 插件宿主。`Send + Sync`，挂在 AppState 上（不进会话引擎：命令插件是本机进程，
/// 只有内置插件看远端时才借引擎拿 SSH 链路）。克隆共享同一份状态
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

/// 锁里没有会 panic 的代码；真中毒了也照常用里面的数据
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 找到的一个插件
enum Found {
    Builtin(PluginInfo),
    Command { info: PluginInfo, manifest: Option<Manifest>, dir: PathBuf },
}

impl Found {
    fn info(&self) -> &PluginInfo {
        match self {
            Found::Builtin(info) | Found::Command { info, .. } => info,
        }
    }
}

/// 内置插件，按右侧栏上的先后
fn builtin_infos(disabled: &HashSet<String>) -> Vec<PluginInfo> {
    vec![sysmon::info(!disabled.contains(sysmon::ID)), docker::info(!disabled.contains(docker::ID))]
}

/// 停用了哪些（读坏了当作一个都没停）
pub fn disabled_set(db: &Db) -> HashSet<String> {
    db.get_setting(DISABLED_KEY)
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub fn set_enabled(db: &Db, id: &str, enabled: bool) {
    let mut set = disabled_set(db);
    if enabled {
        set.remove(id);
    } else {
        set.insert(id.to_string());
    }
    let mut list: Vec<String> = set.into_iter().collect();
    list.sort();
    db.set_setting(DISABLED_KEY, &serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()));
}

/// 读一个命令插件的目录：清单坏了照样回一条 info（带 error），设置页要把原因列出来
fn read_command_plugin(dir: &Path, id: &str, enabled: bool) -> (PluginInfo, Option<Manifest>) {
    let mut info = PluginInfo {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        icon: "puzzle".into(),
        kind: PluginKind::Command,
        scope: falcon_proto::PluginScope::Global,
        refresh: None,
        enabled,
        error: None,
        dir: Some(dir.to_string_lossy().into_owned()),
    };
    let fail = |mut info: PluginInfo, error: String| {
        info.error = Some(error);
        (info, None)
    };
    if !valid_id(id) {
        return fail(info, "目录名只能用小写字母、数字、- 与 _（最长 48 个字符），它就是插件的 id".into());
    }
    if BUILTIN_IDS.contains(&id) {
        return fail(info, "与内置插件重名，换个目录名".into());
    }
    let path = dir.join("plugin.json");
    match std::fs::metadata(&path) {
        Err(_) => return fail(info, "目录里没有 plugin.json".into()),
        Ok(m) if m.len() > MAX_MANIFEST_BYTES => return fail(info, "plugin.json 太大了（超过 64 KB）".into()),
        Ok(_) => {}
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return fail(info, format!("读不了 plugin.json：{e}")),
    };
    match manifest::parse_manifest(id, &text) {
        Ok(m) => {
            info.name = m.name.clone();
            info.description = m.description.clone();
            info.icon = m.icon.clone();
            info.scope = m.scope;
            info.refresh = m.refresh;
            (info, Some(m))
        }
        Err(e) => fail(info, e),
    }
}

fn check_params(params: &BTreeMap<String, String>) -> Result<(), PluginError> {
    if params.len() > MAX_PARAMS {
        return Err(PluginError::BadInput(format!("参数太多（最多 {MAX_PARAMS} 个）")));
    }
    for (k, v) in params {
        if !valid_param(k) {
            return Err(PluginError::BadInput(format!("参数名不合法：{k}")));
        }
        if v.len() > MAX_PARAM_VALUE {
            return Err(PluginError::BadInput(format!("参数 {k} 太长")));
        }
    }
    Ok(())
}

/// 焦点项目 → 插件看得到的那部分（不含凭据）
fn project_context(db: &Db, project_id: Option<&str>) -> Option<PluginProject> {
    let row = db.get_project(project_id?)?;
    let ssh = (row.project_type == "ssh").then(|| {
        format!(
            "{}@{}:{}",
            row.ssh_username.as_deref().unwrap_or(""),
            row.ssh_host.as_deref().unwrap_or(""),
            row.ssh_port.unwrap_or(22)
        )
    });
    Some(PluginProject { id: row.id, name: row.name, project_type: row.project_type, working_dir: row.working_dir, ssh })
}

impl PluginHost {
    pub fn new(data_dir: &Path, base_env: BaseEnvFn) -> Self {
        let data = data_dir.join("plugin-data");
        PluginHost {
            inner: Arc::new(Inner {
                dir: data_dir.join("plugins"),
                docker: docker::Docker::new(base_env.clone(), data.join(docker::ID)),
                data,
                base_env,
                sysmon: sysmon::Sysmon::default(),
                inflight: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 命令插件放在哪
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// 列出全部插件：内置的在前，命令插件按 id 排。同步 I/O（读目录与清单），调用方放 spawn_blocking。
    /// 目录还不存在就顺手建出来：设置页告诉用户"放到这里"，那里得真有个文件夹
    pub fn scan(&self, disabled: &HashSet<String>) -> PluginList {
        let mut plugins = builtin_infos(disabled);
        let _ = std::fs::create_dir_all(&self.inner.dir);
        let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(&self.inner.dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()) && e.path().is_dir())
                    .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
                    // 隐藏目录（.git、.DS_Store 所在的那种）不是插件
                    .filter(|(name, _)| !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        for (id, dir) in dirs.into_iter().take(MAX_PLUGINS) {
            plugins.push(read_command_plugin(&dir, &id, !disabled.contains(&id)).0);
        }
        PluginList { dir: self.inner.dir.to_string_lossy().into_owned(), plugins }
    }

    fn find(&self, id: &str, disabled: &HashSet<String>) -> Option<Found> {
        if let Some(info) = builtin_infos(disabled).into_iter().find(|i| i.id == id) {
            return Some(Found::Builtin(info));
        }
        let enabled = !disabled.contains(id);
        // id 不合法的也不去拼路径：它来自 URL
        if !valid_id(id) {
            return None;
        }
        let dir = self.inner.dir.join(id);
        if !dir.is_dir() {
            return None;
        }
        let (info, manifest) = read_command_plugin(&dir, id, enabled);
        Some(Found::Command { info, manifest, dir })
    }

    /// 找插件并确认它能用（存在、启用、清单没坏）
    async fn usable(&self, db: &Db, id: &str) -> Result<Found, PluginError> {
        let disabled = disabled_set(db);
        let this = self.clone();
        let id_owned = id.to_string();
        let found = tokio::task::spawn_blocking(move || this.find(&id_owned, &disabled))
            .await
            .map_err(|e| PluginError::Failed(e.to_string()))?
            .ok_or_else(|| PluginError::NotFound(format!("没有这个插件：{id}")))?;
        let info = found.info();
        if !info.enabled {
            return Err(PluginError::Unavailable(format!("插件「{}」已停用", info.name)));
        }
        if let Some(e) = &info.error {
            return Err(PluginError::Unavailable(e.clone()));
        }
        Ok(found)
    }

    /// 画一次面板
    pub async fn render(
        &self,
        db: &Arc<Db>,
        engine: &EngineHandle,
        id: &str,
        input: PluginRenderInput,
    ) -> Result<PluginView, PluginError> {
        check_params(&input.params)?;
        match self.usable(db, id).await? {
            Found::Builtin(info) => {
                let view = match info.id.as_str() {
                    docker::ID => self.inner.docker.render(db, engine, &input).await,
                    _ => self.inner.sysmon.render(db, engine, &input).await,
                };
                Ok(view::sanitize(view))
            }
            Found::Command { manifest, dir, .. } => {
                let manifest = manifest.expect("usable 已经挡掉了清单坏的");
                let req = PluginRequest {
                    api: PLUGIN_API,
                    event: PluginEvent::Render,
                    action: None,
                    value: None,
                    params: input.params,
                    project: project_context(db, input.project_id.as_deref()),
                };
                self.run_shared(id.to_string(), dir, manifest, req).await
            }
        }
    }

    /// 点了面板上的 invoke 按钮：带着动作再跑一次插件，回新的面板
    pub async fn action(
        &self,
        db: &Arc<Db>,
        engine: &EngineHandle,
        id: &str,
        input: PluginActionInput,
    ) -> Result<PluginView, PluginError> {
        check_params(&input.params)?;
        if !valid_action(&input.action) {
            return Err(PluginError::BadInput(format!("动作 id 不合法：{}", input.action)));
        }
        if input.value.as_ref().is_some_and(|v| v.len() > MAX_PARAM_VALUE) {
            return Err(PluginError::BadInput("动作的 value 太长".into()));
        }
        match self.usable(db, id).await? {
            Found::Builtin(info) if info.id == docker::ID => {
                Ok(view::sanitize(self.inner.docker.action(db, engine, &input).await?))
            }
            Found::Builtin(info) => Err(PluginError::BadInput(format!("「{}」没有动作 {}", info.name, input.action))),
            Found::Command { manifest, dir, .. } => {
                let manifest = manifest.expect("usable 已经挡掉了清单坏的");
                if manifest.output == Output::Text {
                    return Err(PluginError::BadInput("纯文字输出的插件没有动作".into()));
                }
                let req = PluginRequest {
                    api: PLUGIN_API,
                    event: PluginEvent::Action,
                    action: Some(input.action),
                    value: input.value,
                    params: input.params,
                    project: project_context(db, input.project_id.as_deref()),
                };
                run_command(self.inner.clone(), id.to_string(), dir, manifest, req).await
            }
        }
    }

    /// 同一份请求在跑就等它的结果，没有才起一个。进程跑在独立的任务上：等结果的请求都断开了，
    /// 进程也照样跑完、把自己从表里摘掉（不然留下一个永远不结束的条目）
    async fn run_shared(&self, id: String, dir: PathBuf, manifest: Manifest, req: PluginRequest) -> Result<PluginView, PluginError> {
        let key = format!("{id}\n{}", serde_json::to_string(&req).unwrap_or_default());
        let fut = {
            let mut map = lock(&self.inner.inflight);
            match map.get(&key) {
                Some(f) => f.clone(),
                None => {
                    let inner = self.inner.clone();
                    let k = key.clone();
                    let task = tokio::spawn(async move {
                        let res = run_command(inner.clone(), id, dir, manifest, req).await;
                        lock(&inner.inflight).remove(&k);
                        res
                    });
                    let f = async move {
                        task.await.unwrap_or_else(|e| Err(PluginError::Failed(format!("插件任务异常结束：{e}"))))
                    }
                    .boxed()
                    .shared();
                    map.insert(key, f.clone());
                    f
                }
            }
        };
        fut.await
    }
}

async fn run_command(
    inner: Arc<Inner>,
    id: String,
    dir: PathBuf,
    manifest: Manifest,
    req: PluginRequest,
) -> Result<PluginView, PluginError> {
    let data = inner.data.join(&id);
    let _ = tokio::fs::create_dir_all(&data).await;
    let base = (inner.base_env)().await;
    let env = runner::plugin_env(&base, &id, &dir.to_string_lossy(), &data.to_string_lossy(), &req);
    let spawn = runner::Spawn {
        program: manifest::resolve_program(&dir, &manifest.command[0]),
        args: manifest.command[1..].to_vec(),
        cwd: dir,
        env,
        stdin: serde_json::to_vec(&req).unwrap_or_default(),
        timeout: manifest.timeout,
    };
    let out = runner::run(spawn).await.map_err(PluginError::Failed)?;
    if let Some(msg) = runner::failure(&out, manifest.timeout) {
        return Err(PluginError::Failed(msg));
    }
    let view = match manifest.output {
        Output::Json => view::parse_view(&out.stdout).map_err(|e| {
            // JSON 坏了多半是脚本中途报了错、把报错打进了 stdout：附上 stderr 帮人找原因
            let tail = out.stderr.trim();
            PluginError::Failed(if tail.is_empty() { e } else { format!("{e}\n{}", view::clip(tail.to_string(), 2000)) })
        })?,
        Output::Text => view::text_view(&out.stdout),
    };
    Ok(view::sanitize(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(dir: &Path) -> PluginHost {
        PluginHost::new(dir, Arc::new(|| Box::pin(async { Arc::new(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]) })))
    }

    fn put(dir: &Path, id: &str, manifest: Option<&str>) {
        let d = dir.join("plugins").join(id);
        std::fs::create_dir_all(&d).unwrap();
        if let Some(m) = manifest {
            std::fs::write(d.join("plugin.json"), m).unwrap();
        }
    }

    #[test]
    fn scan_lists_builtin_first_and_explains_broken_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        put(tmp.path(), "zeta", Some(r#"{"name":"Z","command":"true","icon":"box","refresh":5}"#));
        put(tmp.path(), "alpha", Some("{"));
        put(tmp.path(), "empty", None);
        put(tmp.path(), "Bad Name", Some(r#"{"command":"true"}"#));
        put(tmp.path(), "sysmon", Some(r#"{"command":"true"}"#));
        put(tmp.path(), ".hidden", Some(r#"{"command":"true"}"#));
        std::fs::write(tmp.path().join("plugins/readme.txt"), "x").unwrap();
        let list = h.scan(&HashSet::from(["zeta".to_string()]));
        assert_eq!(list.dir, tmp.path().join("plugins").to_string_lossy());
        let ids: Vec<_> = list.plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["sysmon", "docker", "Bad Name", "alpha", "empty", "sysmon", "zeta"]);
        let by = |i: usize| &list.plugins[i];
        assert_eq!((by(0).kind, by(1).kind), (PluginKind::Builtin, PluginKind::Builtin));
        assert!(by(0).enabled && by(1).enabled);
        assert!(by(2).error.as_deref().unwrap().contains("目录名"));
        assert!(by(3).error.as_deref().unwrap().contains("不是合法的 JSON"));
        assert!(by(4).error.as_deref().unwrap().contains("没有 plugin.json"));
        assert!(by(5).error.as_deref().unwrap().contains("内置插件重名"));
        let z = by(6);
        assert_eq!((z.name.as_str(), z.icon.as_str(), z.refresh, z.enabled, z.error.as_deref()), ("Z", "box", Some(5.0), false, None));
    }

    #[test]
    fn enable_and_disable_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        assert!(disabled_set(&db).is_empty());
        set_enabled(&db, "sysmon", false);
        set_enabled(&db, "hello", false);
        assert_eq!(disabled_set(&db), HashSet::from(["sysmon".to_string(), "hello".to_string()]));
        set_enabled(&db, "sysmon", true);
        assert_eq!(disabled_set(&db), HashSet::from(["hello".to_string()]));
        db.set_setting(DISABLED_KEY, "garbage");
        assert!(disabled_set(&db).is_empty());
    }

    #[test]
    fn params_are_whitelisted() {
        assert!(check_params(&BTreeMap::from([("machine".into(), "local".into())])).is_ok());
        assert!(matches!(check_params(&BTreeMap::from([("A".into(), "x".into())])), Err(PluginError::BadInput(_))));
        let many: BTreeMap<String, String> = (0..17).map(|i| (format!("p{i}"), String::new())).collect();
        assert!(check_params(&many).is_err());
        assert!(check_params(&BTreeMap::from([("p".into(), "x".repeat(MAX_PARAM_VALUE + 1))])).is_err());
    }
}
