//! 移植自 `packages/server/src/meegle/client.ts`。TS 没有它的单测；文件末的用例是 Rust 侧
//! 补的，用临时目录里的假 `meegle` 脚本跑真进程（不碰网络、不碰真实登录态）。
//!
//! meegle CLI 的执行层：起进程、超时、缓存、扇出，把 command.rs 归一化出来的东西
//! 组装成面板要的形状。所有 I/O 都在这一个文件里。
//!
//! 只跑在 falcon 后端所在的机器上（见 command.rs 顶部说明），环境用本地 PTY 同一套
//! 登录环境（[`crate::sessions::local::local_base_env`]）：后端由 launchd / 服务守护拉起时
//! PATH 里往往没有 /opt/homebrew/bin 或 npm 全局目录，不这么做 `meegle` 就"未安装"。
//!
//! # 执行模型（与 TS 的差别）
//!
//! - [`MeegleClient`] 是 `Send + Sync` 的，克隆便宜（内部一个 `Arc`），挂在 axum 的多线程
//!   状态里直接用。它只起本机进程，不碰 `Rc` 的 SSH 链路，不需要会话核心的 LocalSet。
//! - 缓存是 falcon-core 的 [`TtlCache`]（与 TS 共用的 `ttlCache.ts` 同一套语义：TTL、
//!   同 key 并发合并、失败不入库、clear 抬代数）。TS 的缓存是异构的，这里存
//!   `Arc<dyn Any + Send + Sync>`，取时按类型认。
//! - **缓存的 load 一律 `tokio::spawn` 出去跑**。Node 的 Promise 不管有没有人等都会跑完；
//!   Rust 的 future 是惰性的，HTTP 请求中途断开时 axum 会丢掉 handler 的 future，共享的
//!   load 就停在半路（CLI 进程挂着、in-flight 条目留着，直到下一个同 key 的请求来推它）。
//!   spawn 出去才和 Node 一样跑完、照常写回缓存。登录进程的看护同理。
//! - 错误是 [`MeegleError`]（reason + message + code）。经缓存共享出去的错误是
//!   `Arc<anyhow::Error>`，取回时 downcast 回 [`MeegleError`]——"登录 / 安装问题仍回 409"
//!   这类判断（[`MeegleClient::work_item`]）在缓存另一侧也成立。
//! - 超时 / 取消登录时杀进程用 SIGKILL（tokio 的 `start_kill`），Node 的 `proc.kill()` 是
//!   SIGTERM。meegle 是一次性的 CLI，没有要收尾的东西。

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use falcon_core::ttl_cache::{LoadError, TtlCache, TtlCacheLoadOpts};
use falcon_proto::{
    MEEGLE_PAGE_SIZE, MeegleLogin, MeeglePage, MeegleSearchResult, MeegleSpace, MeegleStatus, MeegleTodoAction,
    MeegleTodoItem, MeegleUrlTarget, MeegleUser, MeegleView, MeegleWorkItem, MeegleWorkItemDetail, MeegleWorkItemType,
};
use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::future::{BoxFuture, Shared};
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::bin::resolve_meegle_bin;
use super::command::{
    CLI_ENV, CLI_PAGE_SIZE, FieldMetadata, MqlRow, ParsedUrl, business_text, chunk, comment_args, detail_context,
    fields_args, first_line, group_for_lookup, is_attachment_field, js, login_args, me_args, mql_by_ids,
    mql_next_args, mql_recent, mql_search, multi_view_items_args, normalize_comments, normalize_detail,
    normalize_fields, normalize_multi_view_items, normalize_mql_rows, normalize_spaces, normalize_todo,
    normalize_types, normalize_user, normalize_view_items, normalize_views, parse_cli_json, parse_login_prompt,
    parse_status, parse_url_target, query_args, spaces_args, status_args, todo_args, types_args, url_decode_args,
    version_args, view_items_args, view_search_args, work_item_args, work_item_url,
};
use super::pagination::logical_page;
use crate::exec::Utf8Decoder;
use crate::sessions::local::{BaseEnvFn, local_base_env_fn};

/// 一次 CLI 调用的上限。每条命令都是一次到飞书的网络往返，实测 0.5–2s，30s 已是病态
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
/// `auth status` 与 `version` 的上限（TS 在调用处写死的 15s / 10s）
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// 登录进程从启动到打出授权链接：要先去服务端申请 device code
const LOGIN_PROMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// 用户迟迟不授权就把登录进程收掉，别让它一直挂着
const LOGIN_MAX: Duration = Duration::from_secs(10 * 60);
/// 查询结果默认 5 分钟；空间/类型变动少，类型单独加长。刷新按钮带 fresh 跳过
const CACHE_MS: i64 = 5 * 60_000;
const STATUS_CACHE_MS: i64 = 30_000;
/// 同时起几个 CLI 进程：视图 / 工作项搜索要按类型扇出，十来个类型串行太慢，全开又太重。
/// 实测 `view search` 按租户限 5 qps（超了报 `rate limit, … qps: 5`），视图与工作项两路
/// 并行各 3 个、外加下面的限流重试，十几个类型的空间搜一次刚好不撞。
const FANOUT: usize = 3;
/// 撞限流后等多久再试（第 n 次重试等 n 倍）；重试次数见 [`MeegleClient::call`]
const RATE_LIMIT_BACKOFF: Duration = Duration::from_millis(700);
const RATE_LIMIT_RETRIES: u32 = 2;
/// 搜索与最近列表均最多 100 条；MQL 的两页传输不等于两页 REST。
const SEARCH_CAP: usize = MEEGLE_PAGE_SIZE as usize;

// ---- 错误 ----

/// [`MeegleError`] 的原因。前两个是 shared 的 `MeegleUnavailableReason`（随 409 回给前端，
/// 前端据此切到安装 / 登录提示），后两个只在服务端内部
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeegleErrorReason {
    NotInstalled,
    NotAuthenticated,
    /// CLI 跑了但飞书那边报错（或超时），路由回 502
    CliError,
    /// 用户给的东西本身不对（链接不支持等），路由回 400
    BadInput,
}

impl MeegleErrorReason {
    /// 线上的字面量（错误体的 `reason`）
    pub fn as_str(self) -> &'static str {
        match self {
            MeegleErrorReason::NotInstalled => "not-installed",
            MeegleErrorReason::NotAuthenticated => "not-authenticated",
            MeegleErrorReason::CliError => "cli-error",
            MeegleErrorReason::BadInput => "bad-input",
        }
    }

    /// routes.ts 的 `fail()`：cli-error 502、bad-input 400、其余（没装 / 没登录）409
    pub fn http_status(self) -> u16 {
        match self {
            MeegleErrorReason::CliError => 502,
            MeegleErrorReason::BadInput => 400,
            MeegleErrorReason::NotInstalled | MeegleErrorReason::NotAuthenticated => 409,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MeegleError {
    pub reason: MeegleErrorReason,
    pub message: String,
    /// CLI 错误信封里的 code（`UNKNOWN_COMMAND` / `BAD_OUTPUT` / 飞书的错误码）
    pub code: Option<String>,
}

impl MeegleError {
    pub fn new(reason: MeegleErrorReason, message: impl Into<String>) -> Self {
        MeegleError { reason, message: message.into(), code: None }
    }

    fn cli(message: impl Into<String>) -> Self {
        Self::new(MeegleErrorReason::CliError, message)
    }

    fn bad_input(message: impl Into<String>) -> Self {
        Self::new(MeegleErrorReason::BadInput, message)
    }

    fn not_installed() -> Self {
        Self::new(MeegleErrorReason::NotInstalled, "meegle CLI 未安装")
    }

    fn not_authenticated() -> Self {
        Self::new(MeegleErrorReason::NotAuthenticated, "尚未登录飞书项目")
    }

    /// 没装 / 没登录：这两种得原样回 409 让面板切提示，不能被"降级显示"吞掉
    pub fn is_unavailable(&self) -> bool {
        matches!(self.reason, MeegleErrorReason::NotInstalled | MeegleErrorReason::NotAuthenticated)
    }

    /// HTTP 状态码，见 [`MeegleErrorReason::http_status`]
    pub fn status(&self) -> u16 {
        self.reason.http_status()
    }

    /// 错误体 `{ error, reason, code? }`（code 为 undefined 时 JSON.stringify 不写这个键）
    pub fn body(&self) -> Value {
        let mut body = Map::new();
        body.insert("error".into(), Value::String(self.message.clone()));
        body.insert("reason".into(), Value::String(self.reason.as_str().into()));
        if let Some(code) = &self.code {
            body.insert("code".into(), Value::String(code.clone()));
        }
        Value::Object(body)
    }

    /// 缓存另一侧拿到的共享错误 → 原来的 MeegleError（别的错误按 cli-error 报）
    fn from_load(err: &LoadError) -> Self {
        err.downcast_ref::<MeegleError>().cloned().unwrap_or_else(|| MeegleError::cli(format!("{err:#}")))
    }
}

impl From<MeegleError> for crate::api::error::ApiError {
    fn from(err: MeegleError) -> Self {
        let status = axum::http::StatusCode::from_u16(err.status()).unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
        crate::api::error::ApiError { status, body: err.body() }
    }
}

/// 查询选项。`fresh`：跳过缓存（面板刷新按钮，`?fresh=1`）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeegleQueryOpts {
    pub fresh: bool,
}

impl MeegleQueryOpts {
    /// routes.ts 的 `queryOpts`：`?fresh=1` 或 `?fresh=true` 才算
    pub fn from_fresh_param(fresh: Option<&str>) -> Self {
        MeegleQueryOpts { fresh: matches!(fresh, Some("1" | "true")) }
    }
}

// ---- 进程 ----

/// spawn 本身失败（ENOENT / 超时）；有它就别看 stdout
#[derive(Debug, Clone, PartialEq, Eq)]
enum SpawnError {
    NotFound,
    Timeout,
    Other(String),
}

/// TS 的 `ExecResult`。退出码没有收：CLI 的两条流都当 JSON 试，退出码只是旁证，没人看
#[derive(Debug, Clone)]
struct CliExec {
    stdout: String,
    stderr: String,
    spawn_error: Option<SpawnError>,
}

/// 各处的超时与退避。生产上是文件头那几个常量，测试里缩短
#[derive(Debug, Clone)]
struct Timing {
    cli: Duration,
    status: Duration,
    version: Duration,
    login_prompt: Duration,
    login_max: Duration,
    rate_limit_backoff: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            cli: CLI_TIMEOUT,
            status: STATUS_TIMEOUT,
            version: VERSION_TIMEOUT,
            login_prompt: LOGIN_PROMPT_TIMEOUT,
            login_max: LOGIN_MAX,
            rate_limit_backoff: RATE_LIMIT_BACKOFF,
        }
    }
}

/// `probeStatus` 的结果
#[derive(Debug, Clone, PartialEq)]
struct Probed {
    installed: bool,
    authenticated: bool,
    host: Option<String>,
    expires_in_minutes: Option<f64>,
}

type AnyValue = Arc<dyn Any + Send + Sync>;
type LoginFuture = Shared<BoxFuture<'static, Result<MeegleLogin, MeegleError>>>;

/// 正等着用户授权的那个登录进程
struct LoginEntry {
    id: u64,
    prompt: MeegleLogin,
    /// 取消即杀进程（进程本身归看护任务所有）
    kill: CancellationToken,
}

#[derive(Default)]
struct LoginState {
    current: Option<LoginEntry>,
    /// 正在起、还没打出授权链接的那一次（编号, 共享结果）
    starting: Option<(u64, LoginFuture)>,
    next_id: u64,
}

struct Inner {
    bin: String,
    base_env: BaseEnvFn,
    timing: Timing,
    version: Mutex<Option<String>>,
    host: Mutex<Option<String>>,
    /// 用户 / 空间 / 类型 / 待办 / 搜索 / 详情共用；登录换人时整表清掉
    cache: TtlCache<AnyValue>,
    login: Mutex<LoginState>,
}

/// meegle CLI 客户端。克隆共享同一份状态
#[derive(Clone)]
pub struct MeegleClient {
    inner: Arc<Inner>,
}

/// 锁里没有会 panic 的代码；真中毒了也照常用里面的数据
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MeegleClient {
    /// `bin` 是要 spawn 的可执行文件（见 [`resolve_meegle_bin`]），`base_env` 是基底环境
    pub fn new(bin: String, base_env: BaseEnvFn) -> Self {
        Self::build(bin, base_env, Timing::default())
    }

    /// 生产配置：按进程环境解析 CLI，基底环境用本机登录环境
    pub fn from_env() -> Self {
        Self::new(resolve_meegle_bin(|k| std::env::var(k).ok()), local_base_env_fn())
    }

    fn build(bin: String, base_env: BaseEnvFn, timing: Timing) -> Self {
        log::info!("meegle CLI: {bin}");
        MeegleClient {
            inner: Arc::new(Inner {
                bin,
                base_env,
                timing,
                version: Mutex::new(None),
                host: Mutex::new(None),
                cache: TtlCache::new(CACHE_MS),
                login: Mutex::new(LoginState::default()),
            }),
        }
    }

    /// 实际拿来 spawn 的可执行文件
    pub fn bin(&self) -> &str {
        &self.inner.bin
    }

    /// `{...baseEnv, ...CLI_ENV}` 的命令。不经 shell，argv 原样交给 CLI
    fn command(&self, env: &[(String, String)], args: &[String]) -> Command {
        let mut cmd = Command::new(&self.inner.bin);
        cmd.args(args).env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
        for (k, v) in CLI_ENV {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        // windowsHide：别闪出一个黑色控制台窗口
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd
    }

    async fn exec(&self, args: &[String], timeout: Duration) -> CliExec {
        let env = (self.inner.base_env)().await;
        match self.command(&env, args).spawn() {
            Ok(child) => run_to_end(child, timeout).await,
            Err(e) => {
                let spawn_error =
                    if e.kind() == std::io::ErrorKind::NotFound { SpawnError::NotFound } else { SpawnError::Other(e.to_string()) };
                CliExec { stdout: String::new(), stderr: e.to_string(), spawn_error: Some(spawn_error) }
            }
        }
    }

    /// 跑一条业务命令，把 CLI 的三种失败翻成 MeegleError；撞了服务端限流就退避重试
    async fn call(&self, args: Vec<String>) -> Result<Value, MeegleError> {
        let mut attempt = 0u32;
        loop {
            let res = self.exec(&args, self.inner.timing.cli).await;
            match res.spawn_error {
                Some(SpawnError::NotFound) => return Err(MeegleError::not_installed()),
                Some(SpawnError::Timeout) => return Err(MeegleError::cli("meegle 超时没有响应")),
                Some(SpawnError::Other(msg)) => return Err(MeegleError::cli(msg)),
                None => {}
            }
            let failure = match parse_cli_json(&res.stdout, &res.stderr) {
                Ok(data) => return Ok(data),
                Err(f) => f,
            };
            if failure.code == "UNKNOWN_COMMAND" {
                // 命令清单要登录后才从服务端拉得到：未登录时任何业务命令都是 unknown command
                let st = self.probe_status().await;
                if !st.authenticated {
                    return Err(MeegleError::not_authenticated());
                }
            }
            // /rate limit/i
            if attempt < RATE_LIMIT_RETRIES && failure.message.to_ascii_lowercase().contains("rate limit") {
                tokio::time::sleep(self.inner.timing.rate_limit_backoff * (attempt + 1)).await;
                attempt += 1;
                continue;
            }
            return Err(MeegleError {
                reason: MeegleErrorReason::CliError,
                message: failure.message,
                code: Some(failure.code),
            });
        }
    }

    // ---- 缓存 ----

    /// TS 的 `cache.getOrLoad(key, load, { ...opts, ttlMs })`。load 拿到一份客户端的克隆，
    /// 被 spawn 出去跑（见文件头）
    async fn cached<T, F, Fut>(&self, key: String, opts: MeegleQueryOpts, ttl_ms: Option<i64>, load: F) -> Result<T, MeegleError>
    where
        T: Clone + Send + Sync + 'static,
        F: FnOnce(MeegleClient) -> Fut,
        Fut: Future<Output = Result<T, MeegleError>> + Send + 'static,
    {
        let me = self.clone();
        let fut = self.inner.cache.get_or_load(
            &key,
            move || {
                let task = tokio::spawn(load(me));
                async move {
                    match task.await {
                        Ok(Ok(v)) => Ok(Arc::new(v) as AnyValue),
                        Ok(Err(e)) => Err(anyhow::Error::new(e)),
                        Err(join) => Err(anyhow::anyhow!("meegle 查询任务异常结束：{join}")),
                    }
                }
            },
            TtlCacheLoadOpts { fresh: opts.fresh, ttl_ms, now: None },
        );
        let value = fut.await.map_err(|e| MeegleError::from_load(&e))?;
        value.downcast_ref::<T>().cloned().ok_or_else(|| MeegleError::cli(format!("meegle 缓存里 {key} 的值类型对不上")))
    }

    fn peek<T: Clone + 'static>(&self, key: &str) -> Option<T> {
        self.inner.cache.peek(key, None)?.value.downcast_ref::<T>().cloned()
    }

    /// 面板刷新按钮：丢掉查询缓存，下一次请求重新打 CLI
    pub fn clear_cache(&self) {
        self.inner.cache.clear();
    }

    // ---- 状态 / 登录 ----

    async fn probe_status(&self) -> Probed {
        let res = self.exec(&status_args(), self.inner.timing.status).await;
        if res.spawn_error == Some(SpawnError::NotFound) {
            return Probed { installed: false, authenticated: false, host: None, expires_in_minutes: None };
        }
        let st = match parse_cli_json(&res.stdout, &res.stderr) {
            Ok(data) => parse_status(&data),
            Err(_) => super::command::ParsedStatus { authenticated: false, host: None, expires_in_minutes: None },
        };
        *lock(&self.inner.host) = st.host.clone();
        Probed { installed: true, authenticated: st.authenticated, host: st.host, expires_in_minutes: st.expires_in_minutes }
    }

    /// `GET /api/meegle/status`。不会失败：CLI 没装 / 没登录都体现在返回值里
    pub async fn status(&self, opts: MeegleQueryOpts) -> MeegleStatus {
        // 登录进行中每 2s 会变，不能吃缓存；授权完成后登录进程退出，看护任务会整表清缓存
        if self.login_prompt().is_some() {
            return self.load_status().await;
        }
        match self.cached("status".into(), opts, Some(STATUS_CACHE_MS), |c| async move { Ok(c.load_status().await) }).await {
            Ok(status) => status,
            // load_status 本身不会失败；只有 load 任务 panic 才走到这里，那就不经缓存再问一次
            Err(_) => self.load_status().await,
        }
    }

    async fn load_status(&self) -> MeegleStatus {
        let st = self.probe_status().await;
        let mut status = MeegleStatus {
            installed: st.installed,
            bin: Some(self.inner.bin.clone()),
            version: None,
            authenticated: st.authenticated,
            host: st.host,
            expires_in_minutes: st.expires_in_minutes,
            user: None,
            login: None,
        };
        if !st.installed {
            return status;
        }
        let (version, user) = tokio::join!(self.cli_version(), async {
            if st.authenticated { self.me().await.ok().flatten() } else { None }
        });
        status.version = version;
        status.user = user;
        status.login = self.login_prompt();
        status
    }

    async fn cli_version(&self) -> Option<String> {
        if let Some(v) = lock(&self.inner.version).clone() {
            return Some(v);
        }
        let res = self.exec(&version_args(), self.inner.timing.version).await;
        let line = first_line(&res.stdout);
        // /^\d/
        if res.spawn_error.is_none() && line.starts_with(|c: char| c.is_ascii_digit()) {
            *lock(&self.inner.version) = Some(line);
        }
        lock(&self.inner.version).clone()
    }

    async fn me(&self) -> Result<Option<MeegleUser>, MeegleError> {
        if let Some(hit) = self.peek::<MeegleUser>("user") {
            return Ok(Some(hit));
        }
        let Some(user) = normalize_user(&self.call(me_args()).await?) else { return Ok(None) };
        self.inner.cache.set("user", Arc::new(user.clone()), Some(CACHE_MS * 2), None);
        Ok(Some(user))
    }

    /// 正在进行的登录（已打出授权链接的那个）
    fn login_prompt(&self) -> Option<MeegleLogin> {
        lock(&self.inner.login).current.as_ref().map(|l| l.prompt.clone())
    }

    /// 拉起 `auth login --device-code`，等它打出授权链接就返回；进程留着直到用户在
    /// 浏览器里授权完成（CLI 自己轮询）。同一时刻只有一个登录进程：再点一次就把
    /// 同一份链接再给一遍，别起第二个把第一个的 code 作废。
    pub async fn start_login(&self, host: &str) -> Result<MeegleLogin, MeegleError> {
        let pending = {
            let mut st = lock(&self.inner.login);
            if let Some(cur) = &st.current {
                return Ok(cur.prompt.clone());
            }
            match &st.starting {
                Some((_, pending)) => pending.clone(),
                None => {
                    st.next_id += 1;
                    let id = st.next_id;
                    let me = self.clone();
                    let host = host.to_string();
                    // 锁还拿着：任务里清 starting 要等这边登记完才轮得到
                    let task = tokio::spawn(async move {
                        let result = me.spawn_login(host).await;
                        let mut st = lock(&me.inner.login);
                        // finally：只清自己那条
                        if st.starting.as_ref().is_some_and(|(i, _)| *i == id) {
                            st.starting = None;
                        }
                        drop(st);
                        result
                    });
                    let shared: LoginFuture = async move {
                        task.await.unwrap_or_else(|e| Err(MeegleError::cli(format!("登录进程异常：{e}"))))
                    }
                    .boxed()
                    .shared();
                    st.starting = Some((id, shared.clone()));
                    shared
                }
            }
        };
        pending.await
    }

    async fn spawn_login(&self, host: String) -> Result<MeegleLogin, MeegleError> {
        let env = (self.inner.base_env)().await;
        let child = self.command(&env, &login_args(&host)).spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { MeegleError::not_installed() } else { MeegleError::cli(e.to_string()) }
        })?;
        let id = {
            let mut st = lock(&self.inner.login);
            st.next_id += 1;
            st.next_id
        };
        let (tx, rx) = oneshot::channel();
        tokio::spawn(self.clone().watch_login(child, host, id, tx));
        rx.await.unwrap_or_else(|_| Err(MeegleError::cli("登录进程异常退出")))
    }

    /// 看护登录进程直到它退出：抓授权链接、两道超时、取消即杀、退出时清缓存。
    /// `tx` 是给 [`Self::start_login`] 的那一次结果（拿到链接 / 起不来）
    async fn watch_login(
        self,
        mut child: Child,
        host: String,
        id: u64,
        tx: oneshot::Sender<Result<MeegleLogin, MeegleError>>,
    ) {
        let mut tx = Some(tx);
        let settle = |tx: &mut Option<oneshot::Sender<_>>, result: Result<MeegleLogin, MeegleError>| {
            if let Some(tx) = tx.take() {
                let _ = tx.send(result);
            }
        };
        let kill = CancellationToken::new();
        let mut stdout = child.stdout.take().expect("stdout 是 piped");
        let mut stderr = child.stderr.take().expect("stderr 是 piped");
        // 两条流各自流式解码（Node 是逐块 toString，会切坏跨块的多字节字符），拼进同一段文本
        let (mut out_dec, mut err_dec) = (Utf8Decoder::default(), Utf8Decoder::default());
        let mut text = String::new();
        let prompt_timer = tokio::time::sleep(self.inner.timing.login_prompt);
        let max_timer = tokio::time::sleep(self.inner.timing.login_max);
        // 进程退出后剩余输出的宽限（同 run_to_end 的 STDIO_DRAIN），先占位
        let drain = tokio::time::sleep(self.inner.timing.login_max);
        tokio::pin!(prompt_timer, max_timer, drain);
        let (mut out_open, mut err_open, mut max_fired, mut killed) = (true, true, false, false);
        // 退出码；外层 None = 还没退出
        let mut exit: Option<Option<i32>> = None;
        let (mut ob, mut eb) = (vec![0u8; 8192], vec![0u8; 8192]);

        while out_open || err_open || exit.is_none() {
            let chunk = tokio::select! {
                r = stdout.read(&mut ob), if out_open => match r {
                    Ok(n) if n > 0 => out_dec.write(&ob[..n]),
                    _ => { out_open = false; continue; }
                },
                r = stderr.read(&mut eb), if err_open => match r {
                    Ok(n) if n > 0 => err_dec.write(&eb[..n]),
                    _ => { err_open = false; continue; }
                },
                _ = &mut prompt_timer, if tx.is_some() => {
                    let _ = child.start_kill();
                    settle(&mut tx, Err(MeegleError::cli("等不到授权链接，登录进程已停止")));
                    continue;
                }
                // 打出链接之前不看它（那段时间归 prompt_timer 管）：TS 里这道定时器若早于链接
                // 触发就永远错过了，这里等链接出来再判，届时已超时就立刻收掉
                _ = &mut max_timer, if !max_fired && tx.is_none() => {
                    max_fired = true;
                    if lock(&self.inner.login).current.as_ref().is_some_and(|c| c.id == id) {
                        log::warn!("meegle 登录等待超时，停止登录进程");
                        let _ = child.start_kill();
                    }
                    continue;
                }
                _ = kill.cancelled(), if !killed => {
                    killed = true;
                    let _ = child.start_kill();
                    continue;
                }
                status = child.wait(), if exit.is_none() => {
                    exit = Some(status.ok().and_then(|s| s.code()));
                    drain.as_mut().reset(tokio::time::Instant::now() + STDIO_DRAIN);
                    continue;
                }
                _ = &mut drain, if exit.is_some() => break,
            };
            text.push_str(&chunk);
            if tx.is_some()
                && let Some(p) = parse_login_prompt(&text)
            {
                let prompt = MeegleLogin { host: host.clone(), url: p.url, code: p.code };
                lock(&self.inner.login).current = Some(LoginEntry { id, prompt: prompt.clone(), kill: kill.clone() });
                settle(&mut tx, Ok(prompt));
            }
        }
        text.push_str(&out_dec.end());
        text.push_str(&err_dec.end());

        let code = exit.flatten();
        {
            let mut st = lock(&self.inner.login);
            if st.current.as_ref().is_some_and(|c| c.id == id) {
                st.current = None;
            }
        }
        // 登录态变了：用户、空间、待办都可能换人
        self.inner.cache.clear();
        let code = code.map_or_else(|| "null".to_string(), |c| c.to_string());
        log::info!("meegle auth login 退出（{code}）");
        let line = first_line(&text);
        settle(&mut tx, Err(MeegleError::cli(if line.is_empty() { format!("登录进程退出（{code}）") } else { line })));
    }

    /// 取消正在进行的登录（已打出链接的那个；还在起的不管，同 TS）
    pub fn cancel_login(&self) {
        if let Some(cur) = lock(&self.inner.login).current.take() {
            cur.kill.cancel();
        }
    }

    // ---- 空间 / 类型 ----

    /// 带关键字是精确查找（粘贴链接换 project_key），不进最近访问列表的缓存
    pub async fn spaces(&self, keyword: Option<&str>, opts: MeegleQueryOpts) -> Result<Vec<MeegleSpace>, MeegleError> {
        if let Some(kw) = keyword.map(js::trim).filter(|k| !k.is_empty()) {
            return Ok(normalize_spaces(&self.call(spaces_args(Some(kw), None)).await?).spaces);
        }
        self.cached("spaces".into(), opts, None, |c| async move {
            Ok(normalize_spaces(&c.call(spaces_args(None, None)).await?).spaces)
        })
        .await
    }

    pub async fn types(&self, space_key: &str, opts: MeegleQueryOpts) -> Result<Vec<MeegleWorkItemType>, MeegleError> {
        let space = space_key.to_string();
        self.cached(format!("types:{space_key}"), opts, Some(CACHE_MS * 2), |c| async move {
            Ok(normalize_types(&c.call(types_args(&space)).await?))
        })
        .await
    }

    async fn fields(&self, space_key: &str, type_key: &str, opts: MeegleQueryOpts) -> Result<Vec<FieldMetadata>, MeegleError> {
        let (space, ty) = (space_key.to_string(), type_key.to_string());
        self.cached(format!("fields:{space_key}:{type_key}"), opts, Some(CACHE_MS * 2), |c| async move {
            let mut fields = Vec::new();
            let mut page = 1;
            loop {
                let result = normalize_fields(&c.call(fields_args(&space, &ty, page)).await?, page);
                fields.extend(result.items);
                if !result.has_more {
                    return Ok(fields);
                }
                page += 1;
            }
        })
        .await
    }

    async fn resolve_rows(&self, data: &Value, space_key: &str, type_key: &str) -> Vec<MqlRow> {
        let mut rows = normalize_mql_rows(data);
        if rows.iter().any(|row| row.business_value.is_some()) {
            match self.fields(space_key, type_key, MeegleQueryOpts::default()).await {
                Ok(fields) => {
                    let options = fields.iter().find(|f| f.key == "business").map(|f| &f.options);
                    for row in &mut rows {
                        if row.business.is_none() {
                            row.business = business_text(row.business_value.as_ref(), options);
                        }
                    }
                }
                Err(e) => log::warn!("meegle 业务名称解析失败（{space_key}/{type_key}）: {e}"),
            }
        }
        rows
    }

    async fn query_hundred(&self, space_key: &str, type_key: &str, mql: &str) -> Result<Vec<MqlRow>, MeegleError> {
        let data = self.call(query_args(space_key, mql)).await?;
        let mut rows = self.resolve_rows(&data, space_key, type_key).await;
        let next = if rows.len() == CLI_PAGE_SIZE as usize { mql_next_args(space_key, &data) } else { None };
        if let Some(next) = next {
            let more = self.call(next).await?;
            rows.extend(self.resolve_rows(&more, space_key, type_key).await);
        }
        rows.truncate(SEARCH_CAP);
        Ok(rows)
    }

    /// simple_name → 空间：先翻最近访问过的缓存，没有再按 simple_name 查一次（可能有同名无权限的，只认精确匹配）
    async fn space_by_simple_name(&self, simple_name: &str) -> Result<Option<MeegleSpace>, MeegleError> {
        let recent = self.spaces(None, MeegleQueryOpts::default()).await.unwrap_or_default();
        if let Some(hit) = recent.into_iter().find(|s| s.simple_name == simple_name) {
            return Ok(Some(hit));
        }
        Ok(self.spaces(Some(simple_name), MeegleQueryOpts::default()).await?.into_iter().find(|s| s.simple_name == simple_name))
    }

    /// 粘贴的飞书项目链接 → 面板能开的目标。`url decode` 是 CLI 本地解析（路由表在它那边，
    /// 别自己拆路径）；再把 simple_name 换成权威的 project_key。
    pub async fn resolve_url(&self, url: &str) -> Result<MeegleUrlTarget, MeegleError> {
        let target = parse_url_target(&self.call(url_decode_args(url)).await?).map_err(MeegleError::bad_input)?;
        let host = self.host_or_probe().await;
        let (t_host, simple_name) = match &target {
            ParsedUrl::WorkItem { host, simple_name, .. }
            | ParsedUrl::View { host, simple_name, .. }
            | ParsedUrl::MultiProjectView { host, simple_name, .. } => (host.clone(), simple_name.clone()),
        };
        if let Some(h) = host.as_deref().filter(|h| !h.is_empty())
            && !t_host.is_empty()
            && t_host != h
        {
            return Err(MeegleError::bad_input(format!("链接的站点（{t_host}）与当前登录的站点（{h}）不一致")));
        }
        let space = self
            .space_by_simple_name(&simple_name)
            .await?
            .ok_or_else(|| MeegleError::bad_input(format!("找不到空间 {simple_name}，可能没有权限")))?;
        let (space_key, space_name) = (space.key.clone(), Some(space.name.clone()));
        Ok(match target {
            ParsedUrl::WorkItem { type_key, id, .. } => MeegleUrlTarget::WorkItem {
                url: work_item_url(host.as_deref(), Some(&space.simple_name), &type_key, &id).unwrap_or_else(|| url.to_string()),
                space_key,
                space_name,
                type_key,
                id,
            },
            ParsedUrl::MultiProjectView { view_id, .. } => {
                MeegleUrlTarget::MultiProjectView { space_key, space_name, view_id, url: url.to_string() }
            }
            ParsedUrl::View { view_id, type_key, .. } => {
                MeegleUrlTarget::View { space_key, space_name, view_id, type_key, url: url.to_string() }
            }
        })
    }

    /// 拼详情页 URL 要空间的 simple_name；只认最近访问过的空间，查不到就没有链接
    async fn simple_name_of(&self, space_key: &str) -> Option<String> {
        let spaces = self.spaces(None, MeegleQueryOpts::default()).await.ok()?;
        spaces.into_iter().find(|s| s.key == space_key).map(|s| s.simple_name).filter(|s| !s.is_empty())
    }

    async fn host_or_probe(&self) -> Option<String> {
        if let Some(h) = lock(&self.inner.host).clone() {
            return Some(h);
        }
        self.probe_status().await;
        lock(&self.inner.host).clone()
    }

    async fn enabled_types(
        &self,
        space_key: &str,
        type_key: Option<&str>,
        opts: MeegleQueryOpts,
    ) -> Result<Vec<MeegleWorkItemType>, MeegleError> {
        let types: Vec<_> = self.types(space_key, opts).await?.into_iter().filter(|t| !t.disabled).collect();
        // if (!typeKey)：空串也算没给
        let Some(type_key) = type_key.filter(|t| !t.is_empty()) else { return Ok(types) };
        match types.into_iter().find(|t| t.key == type_key) {
            Some(one) => Ok(vec![one]),
            None => Err(MeegleError::cli("工作项类型不存在")),
        }
    }

    // ---- 搜索 / 浏览 ----

    /// 视图与工作项两路并行；一路里某个类型失败只记一条错误，其余照常出结果
    pub async fn search(
        &self,
        space_key: &str,
        keyword: &str,
        type_key: Option<&str>,
        opts: MeegleQueryOpts,
    ) -> Result<MeegleSearchResult, MeegleError> {
        let key = format!("search:{space_key}:{keyword}:{}", type_key.unwrap_or(""));
        let (space, kw, ty) = (space_key.to_string(), keyword.to_string(), type_key.map(str::to_string));
        self.cached(key, opts, None, move |c| async move { c.search_uncached(&space, &kw, ty.as_deref(), opts).await })
            .await
    }

    async fn search_uncached(
        &self,
        space_key: &str,
        keyword: &str,
        type_key: Option<&str>,
        opts: MeegleQueryOpts,
    ) -> Result<MeegleSearchResult, MeegleError> {
        let types = self.enabled_types(space_key, type_key, opts).await?;
        let errors = Mutex::new(Vec::new());
        let (views, items) = tokio::join!(
            self.search_views(space_key, &types, keyword, &errors),
            self.search_items(space_key, &types, keyword, &errors)
        );
        Ok(MeegleSearchResult { views, items, errors: errors.into_inner().unwrap_or_else(|e| e.into_inner()) })
    }

    async fn search_views(
        &self,
        space_key: &str,
        types: &[MeegleWorkItemType],
        keyword: &str,
        errors: &Mutex<Vec<String>>,
    ) -> Vec<MeegleView> {
        let jobs = types
            .iter()
            .map(|ty| {
                async move {
                    match self.call(view_search_args(space_key, &ty.key, keyword)).await {
                        Ok(data) => normalize_views(&data, &ty.key, &ty.name),
                        Err(e) => {
                            lock(errors).push(format!("{}：{}", ty.name, e.message));
                            Vec::new()
                        }
                    }
                }
                .boxed()
            })
            .collect();
        let per_type = map_limit(jobs, FANOUT).await;
        per_type.into_iter().flatten().collect()
    }

    async fn search_items(
        &self,
        space_key: &str,
        types: &[MeegleWorkItemType],
        keyword: &str,
        errors: &Mutex<Vec<String>>,
    ) -> Vec<MeegleWorkItem> {
        let (host, simple_name) = tokio::join!(self.host_or_probe(), self.simple_name_of(space_key));
        let (host, simple_name) = (host.as_deref(), simple_name.as_deref());
        let jobs = types
            .iter()
            .map(|ty| {
                async move {
                    match self.query_hundred(space_key, &ty.key, &mql_search(space_key, &ty.key, keyword, None)).await {
                        Ok(rows) => rows.into_iter().map(|r| row_to_item(r, space_key, ty, host, simple_name)).collect(),
                        Err(e) => {
                            lock(errors).push(format!("{}：{}", ty.name, e.message));
                            Vec::new()
                        }
                    }
                }
                .boxed()
            })
            .collect();
        let per_type: Vec<Vec<MeegleWorkItem>> = map_limit(jobs, FANOUT).await;
        let mut items: Vec<MeegleWorkItem> = per_type.into_iter().flatten().collect();
        // id 单调递增，跨类型合并后仍按"最新在前"（先比长度再比字典序 = 按数值倒序）
        items.sort_by(|a, b| b.id.len().cmp(&a.id.len()).then_with(|| b.id.cmp(&a.id)));
        items.truncate(SEARCH_CAP);
        items
    }

    pub async fn recent(&self, space_key: &str, type_key: &str, opts: MeegleQueryOpts) -> Result<Vec<MeegleWorkItem>, MeegleError> {
        let (space, ty) = (space_key.to_string(), type_key.to_string());
        self.cached(format!("recent:{space_key}:{type_key}"), opts, None, move |c| async move {
            let types = c.enabled_types(&space, Some(&ty), opts).await?;
            let Some(ty) = types.first() else { return Err(MeegleError::cli("工作项类型不存在")) };
            let (host, simple_name) = tokio::join!(c.host_or_probe(), c.simple_name_of(&space));
            let rows = c.query_hundred(&space, &ty.key, &mql_recent(&space, &ty.key, None)).await?;
            Ok(rows.into_iter().map(|r| row_to_item(r, &space, ty, host.as_deref(), simple_name.as_deref())).collect())
        })
        .await
    }

    pub async fn view_items(
        &self,
        space_key: &str,
        view_id: &str,
        page: u32,
        opts: MeegleQueryOpts,
    ) -> Result<MeeglePage<MeegleWorkItem>, MeegleError> {
        let (space, view) = (space_key.to_string(), view_id.to_string());
        self.cached(format!("view:{space_key}:{view_id}:{page}"), opts, None, move |c| async move {
            let host = c.host_or_probe().await;
            let (c2, space2, view2, host2) = (&c, &space, &view, &host);
            let mut result = logical_page(page, |p| async move {
                let data = c2.call(view_items_args(space2, view2, p)).await?;
                Ok::<_, MeegleError>(normalize_view_items(&data, host2.as_deref(), p))
            })
            .await?;
            c.enrich(result.items.iter_mut().collect()).await;
            Ok(result)
        })
        .await
    }

    /// 全景视图：CLI 只给名字 / 空间 / id / 类型，状态与外链按待办同一套补
    pub async fn multi_view_items(
        &self,
        space_key: &str,
        view_id: &str,
        page: u32,
        opts: MeegleQueryOpts,
    ) -> Result<MeeglePage<MeegleWorkItem>, MeegleError> {
        let (space, view) = (space_key.to_string(), view_id.to_string());
        self.cached(format!("mview:{space_key}:{view_id}:{page}"), opts, None, move |c| async move {
            let (c2, space2, view2) = (&c, &space, &view);
            let mut result = logical_page(page, |p| async move {
                let data = c2.call(multi_view_items_args(space2, view2, p)).await?;
                Ok::<_, MeegleError>(normalize_multi_view_items(&data, p))
            })
            .await?;
            c.enrich(result.items.iter_mut().collect()).await;
            Ok(result)
        })
        .await
    }

    pub async fn work_item(&self, space_key: &str, id: &str, opts: MeegleQueryOpts) -> Result<MeegleWorkItemDetail, MeegleError> {
        let (space, id_owned) = (space_key.to_string(), id.to_string());
        self.cached(format!("item:{space_key}:{id}"), opts, None, move |c| async move {
            let (space, id) = (space.as_str(), id_owned.as_str());
            let host = c.host_or_probe().await;
            let Some(mut detail) = normalize_detail(&c.call(work_item_args(space, id, None)).await?, host.as_deref()) else {
                return Err(MeegleError::cli("工作项不存在或没有权限"));
            };

            let type_key = detail.item.type_key.clone();
            let context = async {
                let fields = c.fields(space, &type_key, opts).await?;
                let keys: Vec<String> =
                    fields.iter().filter(|f| f.key == "business" || is_attachment_field(f)).map(|f| f.key.clone()).collect();
                if keys.is_empty() {
                    return Ok(None);
                }
                let extra = c.call(work_item_args(space, id, Some(&keys))).await?;
                Ok::<_, MeegleError>(Some(detail_context(&extra, &fields)))
            };
            match context.await {
                Ok(Some(ctx)) => ctx.assign_to(&mut detail),
                Ok(None) => {}
                // 登录 / CLI 可用性错误仍须回 409，让面板切回登录提示。
                Err(e) if e.is_unavailable() => return Err(e),
                Err(e) => {
                    // 可选字段配置 / 读取权限不应挡住基础详情；不拿 ID 假装业务名称。
                    log::warn!("meegle 详情上下文补充失败: {e}");
                    detail.context_fields_unavailable = Some(true);
                    detail.attachments_unavailable = Some(true);
                }
            }

            let comments = async {
                let first = normalize_comments(&c.call(comment_args(space, id, 1)).await?);
                detail.comments = Some(first.comments);
                // 评论单页 20 条。完整上下文比静默截断更可靠，但设上限避免异常工作项无限放大复制内容。
                let pages = first.total_pages.min(10.0);
                let mut page = 2u32;
                while f64::from(page) <= pages {
                    let more = normalize_comments(&c.call(comment_args(space, id, page)).await?);
                    detail.comments.get_or_insert_with(Vec::new).extend(more.comments);
                    page += 1;
                }
                if first.total_pages > pages {
                    detail.comments_unavailable = Some(true);
                }
                Ok::<_, MeegleError>(())
            };
            match comments.await {
                Ok(()) => {}
                Err(e) if e.is_unavailable() => return Err(e),
                Err(e) => {
                    log::warn!("meegle 评论读取失败: {e}");
                    detail.comments_unavailable = Some(true);
                }
            }
            Ok(detail)
        })
        .await
    }

    /// 我的待办：mywork 给的行没有名字，见 [`Self::enrich`]
    pub async fn todo(&self, action: MeegleTodoAction, page: u32, opts: MeegleQueryOpts) -> Result<MeeglePage<MeegleTodoItem>, MeegleError> {
        self.cached(format!("todo:{}:{page}", action.as_str()), opts, None, move |c| async move {
            let c2 = &c;
            let mut result = logical_page(page, |p| async move {
                let data = c2.call(todo_args(action, p)).await?;
                Ok::<_, MeegleError>(normalize_todo(&data, p))
            })
            .await?;
            c.enrich(result.items.iter_mut().map(|t| &mut t.item).collect()).await;
            Ok(result)
        })
        .await
    }

    /// 给列表行补齐：名字（空的才补）、状态、业务、更新时间、类型名、空间名、外链。mywork 与
    /// 全景视图给的行都只有 id 一类的骨架，按 空间 × 类型 分组用 MQL 一次补 50 个；补不到
    /// （没权限、类型停用）就留空，前端显示 #id，别让整页失败。
    async fn enrich(&self, mut items: Vec<&mut MeegleWorkItem>) {
        if items.is_empty() {
            return;
        }
        let host = self.host_or_probe().await;
        let groups =
            group_for_lookup(items.iter().map(|it| (it.space_key.as_str(), it.type_key.as_str(), it.id.as_str())));
        let batches: Vec<(&str, &str, Vec<String>)> = groups
            .iter()
            .flat_map(|g| {
                chunk(&g.ids, CLI_PAGE_SIZE as usize).into_iter().map(move |ids| (g.space_key.as_str(), g.type_key.as_str(), ids))
            })
            .collect();
        let rows: Mutex<HashMap<String, MqlRow>> = Mutex::default();
        let rows_ref = &rows;
        let jobs = batches
            .iter()
            .map(|(space, ty, ids)| {
                let (space, ty) = (*space, *ty);
                async move {
                    match self.call(query_args(space, &mql_by_ids(space, ty, ids))).await {
                        Ok(data) => {
                            for row in self.resolve_rows(&data, space, ty).await {
                                lock(rows_ref).insert(format!("{space} {ty} {}", row.id), row);
                            }
                        }
                        Err(e) => log::warn!("meegle 补待办名称失败（{space}/{ty}）: {e}"),
                    }
                }
                .boxed()
            })
            .collect();
        map_limit(jobs, FANOUT).await;
        let rows = rows.into_inner().unwrap_or_else(|e| e.into_inner());

        let mut space_keys: Vec<&str> = Vec::new();
        for g in &groups {
            if !space_keys.contains(&g.space_key.as_str()) {
                space_keys.push(&g.space_key);
            }
        }
        // 空间名：全景视图的行只给 project_key，"这条属于哪个空间"全靠这里补（待办自带 project_name，
        // 不覆盖）。spaces() 只有最近访问过的空间，跨到没访问过的空间就补不到，留空即可。
        let space_names: HashMap<String, String> = self
            .spaces(None, MeegleQueryOpts::default())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.key, s.name))
            .collect();
        let lookups = futures::future::join_all(space_keys.iter().map(|&key| {
            async move {
                let simple = self.simple_name_of(key).await;
                // 类型名只是装饰
                let types = self.types(key, MeegleQueryOpts::default()).await.unwrap_or_default();
                (key, simple, types)
            }
            .boxed()
        }))
        .await;
        let mut simple_names: HashMap<&str, Option<String>> = HashMap::new();
        let mut type_names: HashMap<String, String> = HashMap::new();
        for (key, simple, types) in lookups {
            simple_names.insert(key, simple);
            for t in types {
                type_names.insert(format!("{key} {}", t.key), t.name);
            }
        }

        for it in items.iter_mut() {
            if let Some(row) = rows.get(&format!("{} {} {}", it.space_key, it.type_key, it.id)) {
                if it.name.is_empty() && !row.name.is_empty() {
                    it.name = row.name.clone();
                }
                // row.x ?? it.x
                if row.status.is_some() {
                    it.status = row.status.clone();
                }
                if row.business.is_some() {
                    it.business = row.business.clone();
                }
                if row.updated_at.is_some() {
                    it.updated_at = row.updated_at.clone();
                }
            }
            if it.type_name.is_none() {
                it.type_name = type_names.get(&format!("{} {}", it.space_key, it.type_key)).cloned();
            }
            if it.space_name.is_none() {
                it.space_name = space_names.get(&it.space_key).cloned();
            }
            if it.url.is_none() {
                let simple = simple_names.get(it.space_key.as_str()).and_then(|s| s.as_deref());
                it.url = work_item_url(host.as_deref(), simple, &it.type_key, &it.id);
            }
        }
    }
}

fn row_to_item(
    row: MqlRow,
    space_key: &str,
    ty: &MeegleWorkItemType,
    host: Option<&str>,
    simple_name: Option<&str>,
) -> MeegleWorkItem {
    MeegleWorkItem {
        url: work_item_url(host, simple_name, &ty.key, &row.id),
        id: row.id,
        name: row.name,
        business: row.business,
        space_key: space_key.to_string(),
        space_name: None,
        type_key: ty.key.clone(),
        type_name: Some(ty.name.clone()),
        status: row.status,
        updated_at: row.updated_at,
    }
}

/// 有上限的并发 map，结果按输入顺序（TS 的 `mapLimit`：limit 个 worker 依次领活）。
///
/// 收的是已经装箱的 future（调用处 `.boxed()`）而不是闭包：把闭包交给 `Stream::map` 再放进
/// 要 `Send` 的 future 里，编译器证不出闭包对任意生命周期都成立（"implementation of FnOnce
/// is not general enough"）。future 是惰性的，一次只推 `limit` 个，与 worker 模型等价
async fn map_limit<R>(futs: Vec<BoxFuture<'_, R>>, limit: usize) -> Vec<R> {
    futures::stream::iter(futs).buffered(limit.max(1)).collect().await
}

/// 子进程退出后，管道里剩下的输出最多再等多久（见 [`run_to_end`]）
const STDIO_DRAIN: Duration = Duration::from_millis(200);

/// 收完一个 CLI 进程：两条流攒字节、结束时一次解码（逐块转字符串会切坏跨包的 UTF-8
/// 多字节字符），超时就杀掉并带上已收到的输出（TS 的 `{ code: null, ...out(), spawnError: "TIMEOUT" }`）。
///
/// 两条流都关了、进程也退了才算完（Node 的 `close` 事件）——但进程退了之后最多再等
/// [`STDIO_DRAIN`]：macOS 上没有 pipe2，std 建管道是 `pipe()` 再补 CLOEXEC，中间那一瞬
/// 别的线程 spawn 的子进程会把这根管道的写端继承走。继承它的若是常驻进程（会话 PTY 里的
/// Zellij 客户端、px0），这边永远等不到 EOF，每次调用都白等到 30s 超时（测试并行时实测
/// 撞上）。libuv 用一把 cloexec 锁挡掉了这个竞争，Node 版没有这个坑。进程已经退出，它写的
/// 东西都在管道缓冲里，读到暂时没有新数据就够了
async fn run_to_end(mut child: Child, timeout: Duration) -> CliExec {
    let mut out = child.stdout.take().expect("stdout 是 piped");
    let mut err = child.stderr.take().expect("stderr 是 piped");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let (mut ob, mut eb) = (vec![0u8; 16 * 1024], vec![0u8; 16 * 1024]);
    let (mut out_open, mut err_open, mut exited) = (true, true, false);
    let deadline = tokio::time::sleep(timeout);
    // 先占位，进程退出时 reset 成 STDIO_DRAIN 之后
    let drain = tokio::time::sleep(timeout);
    tokio::pin!(deadline, drain);
    let timed_out = loop {
        if !out_open && !err_open && exited {
            break false;
        }
        tokio::select! {
            r = out.read(&mut ob), if out_open => match r {
                Ok(n) if n > 0 => stdout.extend_from_slice(&ob[..n]),
                _ => out_open = false,
            },
            r = err.read(&mut eb), if err_open => match r {
                Ok(n) if n > 0 => stderr.extend_from_slice(&eb[..n]),
                _ => err_open = false,
            },
            _ = child.wait(), if !exited => {
                exited = true;
                drain.as_mut().reset(tokio::time::Instant::now() + STDIO_DRAIN);
            }
            _ = &mut drain, if exited => break false,
            _ = &mut deadline => break true,
        }
    };
    if timed_out {
        let _ = child.start_kill();
    }
    CliExec {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        spawn_error: timed_out.then_some(SpawnError::Timeout),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;

    /// 测试里的假 CLI：一段 sh 脚本，每次调用先把 argv 追加到 calls.log，再按 `body` 回话。
    /// 环境只给 PATH（不跑用户的 login shell）
    struct Fake {
        dir: tempfile::TempDir,
    }

    impl Fake {
        fn new(body: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls.log");
            let script = format!("#!/bin/sh\nLOG='{}'\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n{body}\n", log.display());
            let bin = dir.path().join("meegle");
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn client_with(&self, timing: Timing) -> MeegleClient {
            MeegleClient::build(self.path("meegle").to_string_lossy().into_owned(), fixed_env(self.dir.path()), timing)
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.path("calls.log")).unwrap_or_default().lines().map(str::to_string).collect()
        }

        fn count(&self, prefix: &str) -> usize {
            self.calls().iter().filter(|c| c.starts_with(prefix)).count()
        }
    }

    fn fixed_env(dir: &Path) -> BaseEnvFn {
        let env = Arc::new(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("FAKE_DIR".to_string(), dir.to_string_lossy().into_owned()),
        ]);
        Arc::new(move || {
            let env = env.clone();
            Box::pin(async move { env })
        })
    }

    fn quick() -> Timing {
        Timing {
            cli: Duration::from_secs(10),
            status: Duration::from_secs(10),
            version: Duration::from_secs(10),
            login_prompt: Duration::from_secs(10),
            login_max: Duration::from_secs(60),
            rate_limit_backoff: Duration::from_millis(10),
        }
    }

    /// 已登录、各命令按 case 回固定 JSON 的 CLI
    const LOGGED_IN: &str = r#"
case "$*" in
  "auth status --format json") echo '{"authenticated":true,"host":"project.feishu.cn","expires_in_minutes":42}' ;;
  "version") echo '1.0.23' ;;
  "user search"*) echo '[{"user_key":"u1","name_cn":"张三","email":"z@example.com"}]' ;;
  "project search --page-num=1 --format json") echo '{"projects":[{"project_key":"p1","name":"空间一","simple_name":"sp1"}],"pagination":{"has_more":false}}' ;;
  "project search --project-key=sp2"*) echo '{"projects":[{"project_key":"p2","name":"空间二","simple_name":"sp2"},{"project_key":"px","name":"同名","simple_name":"sp2x"}]}' ;;
  "workitem meta-types --project-key=p1"*) echo '{"list":[{"type_key":"story","name":"需求","api_name":"story","is_disable":2},{"type_key":"issue","name":"缺陷","api_name":"issue","is_disable":2},{"type_key":"old","name":"停用","api_name":"old","is_disable":1}]}' ;;
  "workitem meta-fields"*) echo '{"list":[{"field_key":"business","field_name":"业务线","field_type":"_business","option":[{"option_id":"b1","option_name":"数据"}]}],"pagination":{"has_more":false}}' ;;
  "view search --project-key=p1 --view-scope=story"*) echo '[{"view_id":"v1","view_name":"需求视图"}]' ;;
  "view search --project-key=p1 --view-scope=issue"*) echo '{"data":null,"error":{"code":"E1","message":"error=ErrX,message=Service Internal Error,biz error: 没有权限,retriable=false"}}' >&2; exit 1 ;;
  "workitem query --project-key=p1 --mql=SELECT"*"FROM \`p1\`.\`story\`"*"IN ("*) echo '{"data":{"1":[{"moql_field_list":[{"key":"work_item_id","value":{"long_value":101}},{"key":"name","value":{"string_value":"补上的名字"}},{"key":"work_item_status","value":{"string_value":"进行中"}},{"key":"business","value":{"string_value":"b1"}}]}]}}' ;;
  "workitem query --project-key=p1 --mql=SELECT"*"FROM \`p1\`.\`story\`"*) echo '{"data":{"1":[{"moql_field_list":[{"key":"work_item_id","value":{"long_value":9}},{"key":"name","value":{"string_value":"九"}}]},{"moql_field_list":[{"key":"work_item_id","value":{"long_value":10}},{"key":"name","value":{"string_value":"十"}}]}]}}' ;;
  "workitem query --project-key=p1 --mql=SELECT"*"FROM \`p1\`.\`issue\`"*) echo '{"data":{"1":[{"moql_field_list":[{"key":"work_item_id","value":{"long_value":12}},{"key":"name","value":{"string_value":"十二"}}]}]}}' ;;
  "mywork todo --action=todo --page-num=1"*) echo '{"total":1,"list":[{"project_key":"p1","project_name":"空间一","work_item_info":{"work_item_id":"101","work_item_name":"","work_item_type_key":"story"},"node_info":{"node_name":"开发"}}]}' ;;
  "url decode --url=https://project.feishu.cn/sp1/story/detail/101"*) echo '{"url_kind":"workitem_detail","host":"project.feishu.cn","simple_name":"sp1","work_item_type":"story","work_item_id":"101"}' ;;
  "url decode --url=https://project.feishu.cn/sp2/storyView/v9"*) echo '{"url_kind":"view_story","host":"project.feishu.cn","simple_name":"sp2","view_id":"v9","work_item_type":"story"}' ;;
  "url decode --url=https://meegle.com/"*) echo '{"url_kind":"workitem_detail","host":"meegle.com","simple_name":"sp1","work_item_type":"story","work_item_id":"1"}' ;;
  "url decode"*) echo '{"url_kind":"view_chart","host":"project.feishu.cn","simple_name":"sp1","view_id":"c1"}' ;;
  "workitem get --project-key=p1 --work-item-id=101 --fields="*) echo '{"work_item_fields":[{"key":"business","value":"b1"}]}' ;;
  "workitem get --project-key=p1 --work-item-id=101"*) echo '{"work_item_attribute":{"work_item_id":101,"work_item_name":"详情","owned_project":{"key":"p1","name":"空间一","simple_name":"sp1"},"work_item_type":{"key":"story","name":"需求"}},"work_item_fields":[{"key":"description","value":"描述"}]}' ;;
  "comment list"*"--page-num=1"*) echo '{"comments":[{"content":"第一页"}],"pagination":{"total_pages":2}}' ;;
  "comment list"*"--page-num=2"*) echo '{"data":null,"error":{"code":"E2","message":"message=boom,retriable=false"}}' >&2; exit 1 ;;
  *) echo "unknown command \"$1\" for \"meegle\""; exit 1 ;;
esac
"#;

    #[tokio::test]
    async fn not_installed_is_409_with_the_attempted_path() {
        let client = MeegleClient::build("/nonexistent/falcon-test/meegle".into(), fixed_env(Path::new("/")), quick());
        let status = client.status(MeegleQueryOpts::default()).await;
        assert!(!status.installed && !status.authenticated);
        assert_eq!(status.bin.as_deref(), Some("/nonexistent/falcon-test/meegle"));
        let err = client.spaces(None, MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!(err.reason, MeegleErrorReason::NotInstalled);
        assert_eq!(err.status(), 409);
        assert_eq!(err.body(), json!({ "error": "meegle CLI 未安装", "reason": "not-installed" }));
    }

    #[tokio::test]
    async fn unknown_command_while_logged_out_is_not_authenticated() {
        let fake = Fake::new(
            r#"case "$*" in
  "auth status --format json") echo '{"authenticated":false,"host":null}'; exit 1 ;;
  *) echo "unknown command \"$1\" for \"meegle\""; exit 1 ;;
esac"#,
        );
        let client = fake.client_with(quick());
        let err = client.spaces(None, MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!(err.reason, MeegleErrorReason::NotAuthenticated);
        assert_eq!(err.status(), 409);
        // 业务命令一次，再问一次 auth status
        assert_eq!(fake.count("project search"), 1);
        assert_eq!(fake.count("auth status"), 1);
        let status = client.status(MeegleQueryOpts::default()).await;
        assert!(status.installed && !status.authenticated);
        assert_eq!(status.user, None);
    }

    #[tokio::test]
    async fn status_reports_version_user_and_host_and_is_cached() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let status = client.status(MeegleQueryOpts::default()).await;
        assert!(status.installed && status.authenticated);
        assert_eq!(status.host.as_deref(), Some("project.feishu.cn"));
        assert_eq!(status.expires_in_minutes, Some(42.0));
        assert_eq!(status.version.as_deref(), Some("1.0.23"));
        assert_eq!(status.user.as_ref().map(|u| u.name.as_str()), Some("张三"));
        assert_eq!(status.login, None);
        // 30s 内再问吃缓存；fresh 打穿
        client.status(MeegleQueryOpts::default()).await;
        assert_eq!(fake.count("auth status"), 1);
        client.status(MeegleQueryOpts { fresh: true }).await;
        assert_eq!(fake.count("auth status"), 2);
        // 版本只问一次（进程内记住），用户走缓存
        assert_eq!(fake.count("version"), 1);
        assert_eq!(fake.count("user search"), 1);
    }

    #[tokio::test]
    async fn queries_are_cached_until_fresh_or_clear() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let opts = MeegleQueryOpts::default();
        let spaces = client.spaces(None, opts).await.unwrap();
        assert_eq!(spaces, vec![MeegleSpace { key: "p1".into(), name: "空间一".into(), simple_name: "sp1".into() }]);
        client.spaces(None, opts).await.unwrap();
        assert_eq!(fake.count("project search"), 1);
        client.spaces(None, MeegleQueryOpts { fresh: true }).await.unwrap();
        assert_eq!(fake.count("project search"), 2);
        client.clear_cache();
        client.spaces(None, opts).await.unwrap();
        assert_eq!(fake.count("project search"), 3);
        // 带关键字是精确查找，不进缓存；关键字照 JS trim
        let found = client.spaces(Some("  sp2 "), opts).await.unwrap();
        assert_eq!(found[0].key, "p2");
        client.spaces(Some("sp2"), opts).await.unwrap();
        assert_eq!(fake.count("project search --project-key=sp2"), 2);
        // 并发的同 key 请求合并成一个进程
        client.clear_cache();
        let (a, b) = tokio::join!(client.types("p1", opts), client.types("p1", opts));
        assert_eq!(a.unwrap().len(), 3);
        assert_eq!(b.unwrap().len(), 3);
        assert_eq!(fake.count("workitem meta-types"), 1);
    }

    #[tokio::test]
    async fn cli_errors_are_502_with_the_innermost_message_and_code() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        // view search 对 issue 类型报错；对 story 正常——搜索结果里只记一条错误
        let result = client.search("p1", "需", None, MeegleQueryOpts::default()).await.unwrap();
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].type_name, "需求");
        assert_eq!(result.errors, vec!["缺陷：没有权限".to_string()]);
        // 两个类型的工作项合并后按 id 数值倒序；停用的类型不查
        let ids: Vec<&str> = result.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["12", "10", "9"]);
        assert_eq!(result.items[0].type_name.as_deref(), Some("缺陷"));
        assert_eq!(result.items[0].url.as_deref(), Some("https://project.feishu.cn/sp1/issue/detail/12"));
        assert_eq!(fake.count("view search --project-key=p1 --view-scope=old"), 0);
        // 指定了不存在 / 停用的类型
        let err = client.search("p1", "x", Some("old"), MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!((err.reason, err.message.as_str(), err.status()), (MeegleErrorReason::CliError, "工作项类型不存在", 502));

        let fake = Fake::new(
            r#"printf '%s\n' '{"data":null,"error":{"code":"ErrViewNotExist","message":"error=ErrViewNotExist,message=view not exist,retriable=false\nlogid: 1"}}' >&2; exit 1"#,
        );
        let err = fake.client_with(quick()).types("p1", MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!(err.status(), 502);
        assert_eq!(err.body(), json!({ "error": "view not exist", "reason": "cli-error", "code": "ErrViewNotExist" }));
    }

    #[tokio::test]
    async fn rate_limit_backs_off_and_retries_twice() {
        // 前两次撞限流，第三次成功
        let fake = Fake::new(
            r#"n=$(grep -c . "$LOG")
if [ "$n" -le 2 ]; then echo '{"data":null,"error":{"code":"E","message":"rate limit, qps: 5"}}' >&2; exit 1; fi
echo '{"list":[]}'"#,
        );
        let client = fake.client_with(quick());
        assert_eq!(client.types("p1", MeegleQueryOpts::default()).await.unwrap(), Vec::new());
        assert_eq!(fake.calls().len(), 3);
        // 一直撞：重试两次后放弃，报原文
        let fake = Fake::new(r#"echo '{"data":null,"error":{"code":"E","message":"Rate Limit exceeded"}}' >&2; exit 1"#);
        let err = fake.client_with(quick()).types("p1", MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!(err.message, "Rate Limit exceeded");
        assert_eq!(fake.calls().len(), 3);
    }

    #[tokio::test]
    async fn a_hung_cli_times_out() {
        let fake = Fake::new("exec sleep 30");
        let mut timing = quick();
        timing.cli = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let err = fake.client_with(timing).types("p1", MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!((err.reason, err.message.as_str()), (MeegleErrorReason::CliError, "meegle 超时没有响应"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn multibyte_output_survives_chunking() {
        // 大段中文分好几块写出来：跨块的多字节字符不能被切坏
        let fake = Fake::new(
            r#"printf '{"list":[{"type_key":"t","name":"'
i=0; while [ $i -lt 3000 ]; do printf '中文'; i=$((i+1)); done
printf '","api_name":"t","is_disable":2}]}\n'"#,
        );
        let types = fake.client_with(quick()).types("p1", MeegleQueryOpts::default()).await.unwrap();
        assert_eq!(types[0].name, "中文".repeat(3000));
    }

    #[tokio::test]
    async fn todo_rows_are_enriched_by_mql_and_space_lookups() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let page = client.todo(MeegleTodoAction::Todo, 1, MeegleQueryOpts::default()).await.unwrap();
        assert_eq!((page.page, page.has_more, page.total), (1, false, Some(1)));
        let item = &page.items[0];
        assert_eq!(item.node_name.as_deref(), Some("开发"));
        assert_eq!(item.item.name, "补上的名字");
        assert_eq!(item.item.status.as_deref(), Some("进行中"));
        // business 是叶子 ID：按字段元数据解析成名称
        assert_eq!(item.item.business.as_deref(), Some("数据"));
        assert_eq!(item.item.type_name.as_deref(), Some("需求"));
        // 待办自带 project_name，不覆盖
        assert_eq!(item.item.space_name.as_deref(), Some("空间一"));
        assert_eq!(item.item.url.as_deref(), Some("https://project.feishu.cn/sp1/story/detail/101"));
        // 再要同一页吃缓存
        client.todo(MeegleTodoAction::Todo, 1, MeegleQueryOpts::default()).await.unwrap();
        assert_eq!(fake.count("mywork todo"), 1);
    }

    #[tokio::test]
    async fn recent_lists_one_enabled_type() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let items = client.recent("p1", "story", MeegleQueryOpts::default()).await.unwrap();
        assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["9", "10"]);
        assert_eq!(items[0].url.as_deref(), Some("https://project.feishu.cn/sp1/story/detail/9"));
        let err = client.recent("p1", "old", MeegleQueryOpts::default()).await.unwrap_err();
        assert_eq!(err.message, "工作项类型不存在");
    }

    #[tokio::test]
    async fn resolve_url_maps_simple_name_to_project_key() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let target = client.resolve_url("https://project.feishu.cn/sp1/story/detail/101").await.unwrap();
        assert_eq!(
            target,
            MeegleUrlTarget::WorkItem {
                space_key: "p1".into(),
                space_name: Some("空间一".into()),
                type_key: "story".into(),
                id: "101".into(),
                url: "https://project.feishu.cn/sp1/story/detail/101".into(),
            }
        );
        // 最近访问里没有：按 simple_name 精确查（同名前缀的那条不算）
        let target = client.resolve_url("https://project.feishu.cn/sp2/storyView/v9").await.unwrap();
        assert!(matches!(target, MeegleUrlTarget::View { ref space_key, ref view_id, .. } if space_key == "p2" && view_id == "v9"));
        // 站点不一致、页面类型打不开：400
        let err = client.resolve_url("https://meegle.com/sp1/story/detail/1").await.unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(err.message.contains("不一致"), "{}", err.message);
        let err = client.resolve_url("https://project.feishu.cn/sp1/chart").await.unwrap_err();
        assert_eq!((err.status(), err.message.as_str()), (400, "这类页面打不开（view_chart）"));
    }

    #[tokio::test]
    async fn work_item_detail_keeps_base_when_comments_fail() {
        let fake = Fake::new(LOGGED_IN);
        let client = fake.client_with(quick());
        let detail = client.work_item("p1", "101", MeegleQueryOpts::default()).await.unwrap();
        assert_eq!(detail.item.name, "详情");
        assert_eq!(detail.item.business.as_deref(), Some("数据"));
        assert_eq!(detail.context_fields_unavailable, None);
        // 第二页评论失败：保留第一页，标不完整
        assert_eq!(detail.comments.as_ref().map(Vec::len), Some(1));
        assert_eq!(detail.comments_unavailable, Some(true));
        assert_eq!(detail.item.url.as_deref(), Some("https://project.feishu.cn/sp1/story/detail/101"));
    }

    const LOGIN: &str = r#"
case "$*" in
  "auth login --host=project.feishu.cn --device-code")
    echo "请在浏览器中打开下面的链接完成授权"
    echo "URL: https://project.feishu.cn/device?code=1"
    echo "Authorization code: ABCD-1234"
    exec sleep 30 ;;
  "auth login --host=quiet.example.com --device-code") exec sleep 30 ;;
  "auth login --host=fail.example.com --device-code") echo "Error: host not reachable"; exit 3 ;;
  "auth status --format json") echo '{"authenticated":false}'; exit 1 ;;
  "version") echo '1.0.23' ;;
  *) echo "unknown command"; exit 1 ;;
esac
"#;

    async fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..400 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("等不到条件成立");
    }

    #[tokio::test]
    async fn device_code_login_shares_one_process_and_cancel_kills_it() {
        let fake = Fake::new(LOGIN);
        let client = fake.client_with(quick());
        let (a, b) = tokio::join!(client.start_login("project.feishu.cn"), client.start_login("project.feishu.cn"));
        let expected = MeegleLogin {
            host: "project.feishu.cn".into(),
            url: "https://project.feishu.cn/device?code=1".into(),
            code: "ABCD-1234".into(),
        };
        assert_eq!(a.unwrap(), expected);
        assert_eq!(b.unwrap(), expected);
        // 再点一次给同一份链接，不起第二个进程
        assert_eq!(client.start_login("project.feishu.cn").await.unwrap(), expected);
        assert_eq!(fake.count("auth login"), 1);
        // 登录进行中：状态带着链接，且不吃缓存
        let status = client.status(MeegleQueryOpts::default()).await;
        assert_eq!(status.login, Some(expected.clone()));
        client.status(MeegleQueryOpts::default()).await;
        assert_eq!(fake.count("auth status"), 2);

        // 取消：进程被杀、链接消失、缓存清空
        client.inner.cache.set("marker", Arc::new(1u32), None, None);
        client.cancel_login();
        assert_eq!(client.login_prompt(), None);
        wait_until(|| client.peek::<u32>("marker").is_none()).await;
        // 再登录是新的一次
        client.start_login("project.feishu.cn").await.unwrap();
        assert_eq!(fake.count("auth login"), 2);
        client.cancel_login();
    }

    #[tokio::test]
    async fn login_without_a_prompt_times_out_or_reports_the_exit() {
        let fake = Fake::new(LOGIN);
        let mut timing = quick();
        timing.login_prompt = Duration::from_millis(300);
        let client = fake.client_with(timing);
        let err = client.start_login("quiet.example.com").await.unwrap_err();
        assert_eq!((err.status(), err.message.as_str()), (502, "等不到授权链接，登录进程已停止"));
        // 起来就退出的：报它打的第一行（另一个客户端、正常的等待时长，免得并行跑测试时被超时抢先）
        let client = fake.client_with(quick());
        let err = client.start_login("fail.example.com").await.unwrap_err();
        assert_eq!(err.message, "Error: host not reachable");
        // 失败的那次不留 starting：下一次照常起
        let err = client.start_login("fail.example.com").await.unwrap_err();
        assert_eq!(err.message, "Error: host not reachable");
        assert_eq!(fake.count("auth login --host=fail.example.com"), 2);

        let missing = MeegleClient::build("/nonexistent/falcon-test/meegle".into(), fixed_env(Path::new("/")), quick());
        assert_eq!(missing.start_login("project.feishu.cn").await.unwrap_err().reason, MeegleErrorReason::NotInstalled);
    }

    #[tokio::test]
    async fn login_max_wait_kills_the_process() {
        let fake = Fake::new(LOGIN);
        let mut timing = quick();
        timing.login_max = Duration::from_millis(500);
        let client = fake.client_with(timing);
        assert_eq!(client.start_login("project.feishu.cn").await.unwrap().code, "ABCD-1234");
        // 用户迟迟不授权：进程被收掉，链接消失
        wait_until(|| client.login_prompt().is_none()).await;
        assert_eq!(fake.count("auth login"), 1);
    }

    #[test]
    fn fresh_param_and_error_mapping() {
        assert!(MeegleQueryOpts::from_fresh_param(Some("1")).fresh);
        assert!(MeegleQueryOpts::from_fresh_param(Some("true")).fresh);
        assert!(!MeegleQueryOpts::from_fresh_param(Some("yes")).fresh);
        assert!(!MeegleQueryOpts::from_fresh_param(None).fresh);
        assert_eq!(MeegleErrorReason::BadInput.http_status(), 400);
        assert_eq!(MeegleErrorReason::NotAuthenticated.http_status(), 409);
        let api: crate::api::error::ApiError = MeegleError::bad_input("不是合法的链接").into();
        assert_eq!(api.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(api.body, json!({ "error": "不是合法的链接", "reason": "bad-input" }));
    }
}

/// 编译期的约束：客户端要能放进 axum 的状态，公开方法的 future 要能在多线程运行时上跑
#[cfg(test)]
mod send_check {
    use super::*;

    fn send<T: Send>(_: T) {}
    fn send_sync<T: Send + Sync>(_: &T) {}

    #[test]
    fn client_and_its_futures_are_send() {
        let env: BaseEnvFn = Arc::new(|| Box::pin(async { Arc::new(Vec::new()) }));
        let c = MeegleClient::new("meegle".into(), env);
        let opts = MeegleQueryOpts::default();
        send_sync(&c);
        // 只构造不 poll：什么都不会跑
        send(c.status(opts));
        send(c.start_login("project.feishu.cn"));
        send(c.spaces(Some("x"), opts));
        send(c.types("p", opts));
        send(c.resolve_url("https://project.feishu.cn/x"));
        send(c.search("p", "k", None, opts));
        send(c.recent("p", "t", opts));
        send(c.view_items("p", "v", 1, opts));
        send(c.multi_view_items("p", "v", 1, opts));
        send(c.work_item("p", "1", opts));
        send(c.todo(MeegleTodoAction::Todo, 1, opts));
    }
}
