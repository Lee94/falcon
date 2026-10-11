//! [`FalconClient`] 本体：基址、登录态、请求执行（含 401 自动重登与重放一次）。
//!
//! 具体的 REST 方法按功能域分在 `api/` 下，这里只有它们共用的那层管道。

use std::future::Future;
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use bytes::Bytes;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use parking_lot::{Mutex, RwLock};
use reqwest::header::CONTENT_TYPE;
#[cfg(not(target_family = "wasm"))]
use reqwest::header::{CONTENT_LENGTH, COOKIE, HeaderMap, SET_COOKIE};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

#[cfg(not(target_family = "wasm"))]
use crate::ProgressFn;
use crate::error::{ApiError, ApiErrorKind, ApiResult};
use crate::runtime::{self, MaybeSend};

/// 服务端登录 cookie 的名字（`packages/server/src/auth.ts` 的 `COOKIE_NAME`）。
pub(crate) const COOKIE_NAME: &str = "falcon_token";

/// 连 falcon 服务端的客户端。`Clone` 很便宜（内部 `Arc`），一台服务端配置一个，
/// 窗口里各处拿克隆用；登录态与重登密码在所有克隆之间共享。
#[derive(Clone)]
pub struct FalconClient {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    /// `http(s)://host:port`（可能带反代的路径前缀），末尾不带 `/`
    base: String,
    /// 同一个地址换成 `ws(s)://`
    ws_base: String,
    /// 建不起来（TLS 初始化失败）时把原因存下，每个请求都如实报出来
    http: Result<reqwest::Client, String>,
    pub(crate) auth: Auth,
}

/// 登录态变化，经 [`FalconClient::subscribe_auth`] 推给 app。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthEvent {
    /// 登录成功（手动登录或自动重登），拿到了新的 token。停在"需要登录"的会话
    /// socket 会自己接着连，不用 app 再逐个叫醒。
    LoggedIn,
    /// 需要重新登录：服务端说 401 / 4401，而自动重登没设密码或密码被拒。
    /// 同一轮只推一次（几十个轮询请求同时 401 不会刷屏），登录成功后才会再推。
    LoginRequired,
    /// 主动登出。
    LoggedOut,
}

/// 客户端眼里的当前登录态。服务端没设密码、绑回环地址时根本不需要登录，
/// 这时一直停在 `Unknown`——判断"要不要登录"以 `auth_status()` 为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthState {
    Unknown,
    LoggedIn,
    LoginRequired,
    LoggedOut,
}

/// [`AuthEvent`] 的订阅端。是 `futures` 的 `Stream`，与执行器无关；丢掉即退订。
pub type AuthEvents = UnboundedReceiver<AuthEvent>;

pub(crate) struct Auth {
    token: RwLock<Option<String>>,
    password: RwLock<Option<String>>,
    /// 同一时刻只让一个重登在跑：服务端重启后侧栏轮询、会话 socket 会同时撞上
    /// 401 / 4401，没有这把锁就是 N 次并发登录、N 个互相覆盖的 token。
    relogin: tokio::sync::Mutex<()>,
    state: Mutex<AuthState>,
    subs: Mutex<Vec<UnboundedSender<AuthEvent>>>,
}

impl Auth {
    fn new() -> Self {
        Auth {
            token: RwLock::new(None),
            password: RwLock::new(None),
            relogin: tokio::sync::Mutex::new(()),
            state: Mutex::new(AuthState::Unknown),
            subs: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn token(&self) -> Option<String> {
        self.token.read().clone()
    }

    /// 浏览器里 Cookie 是脚本不许设的请求头，登录 cookie 又是 httpOnly：由浏览器自己带
    /// （同源 fetch / WebSocket 默认就带），这里不出手。
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn cookie_header(&self) -> Option<String> {
        self.token.read().as_ref().map(|t| format!("{COOKIE_NAME}={t}"))
    }

    fn password(&self) -> Option<String> {
        self.password.read().clone()
    }

    fn emit(&self, ev: AuthEvent) {
        // 顺手清掉已经丢了接收端的订阅
        self.subs.lock().retain(|tx| tx.unbounded_send(ev).is_ok());
    }

    fn set_state(&self, next: AuthState) -> bool {
        let mut s = self.state.lock();
        let changed = *s != next;
        *s = next;
        changed
    }

    fn logged_in(&self, token: String) {
        *self.token.write() = Some(token);
        self.set_state(AuthState::LoggedIn);
        // token 换了就是一次有意义的事件（该醒的 socket 要醒），不去重
        self.emit(AuthEvent::LoggedIn);
    }

    /// 服务端拒了 `stale` 这枚 token（或压根没 token）。
    ///
    /// 只有当前 token 仍是那枚过期的才清掉：别的请求可能已经重登拿到了新的，
    /// 一个迟到的 401 不该把新 token 也作废。
    pub(crate) fn login_required(&self, stale: &Option<String>) {
        {
            let mut tok = self.token.write();
            if tok.is_some() && *tok != *stale {
                return;
            }
            *tok = None;
        }
        if self.set_state(AuthState::LoginRequired) {
            self.emit(AuthEvent::LoginRequired);
        }
    }

    fn logged_out(&self) {
        *self.token.write() = None;
        // 主动登出之后还拿着密码自动登回去，那登出就没有意义了
        *self.password.write() = None;
        self.set_state(AuthState::LoggedOut);
        self.emit(AuthEvent::LoggedOut);
    }
}

/// 一次重登的结果。
pub(crate) enum Relogin {
    /// 拿到了新 token（自己登的，或者等锁期间别人已经登好了）
    Done,
    /// 没设重登密码
    NoPassword,
    /// 密码被服务端拒了（已清掉存着的密码，等 app 弹框）
    Rejected,
    /// 登录请求本身没发成功（网络），下一次再试
    Failed(ApiError),
}

// ---------------- 请求 ----------------

/// 一个可以重放的请求描述：401 重登后要原样再发一次，所以不能是已经消费掉的
/// reqwest `RequestBuilder`。
pub(crate) struct Req {
    pub method: Method,
    /// `/api/...`，query 已经编好
    pub path: String,
    pub body: ReqBody,
    /// 401 时要不要自动重登再重放。只有登录请求本身不要——它的 401 就是"密码错误"。
    pub relogin: bool,
}

pub(crate) enum ReqBody {
    Empty,
    Json(Bytes),
    Raw { data: Bytes, content_type: String },
    /// 本机文件按流上传。每次发送（含重登后的重放）都重新打开文件、重新计进度。
    #[cfg(not(target_family = "wasm"))]
    File { path: PathBuf, progress: Option<Arc<ProgressFn>> },
}

impl Req {
    pub(crate) fn new(method: Method, path: String, body: ReqBody) -> Self {
        Req { method, path, body, relogin: true }
    }
}

impl FalconClient {
    /// `base_url` 是 `http(s)://host:port`（反代挂在子路径下时带上前缀），所有
    /// `/api/*`、`/ws/*` 路径拼在它后面。
    ///
    /// 不会失败：TLS 初始化不了、基址协议不对这类问题留到发请求时如实报错，
    /// 窗口照样能开起来显示"连不上"。
    pub fn new(base_url: Url) -> Self {
        let mut url = base_url;
        url.set_query(None);
        url.set_fragment(None);
        let base = url.as_str().trim_end_matches('/').to_owned();
        let ws_base = match url.scheme() {
            "https" => format!("wss{}", &base["https".len()..]),
            "http" => format!("ws{}", &base["http".len()..]),
            _ => base.clone(),
        };
        let http = match url.scheme() {
            "http" | "https" => build_http(),
            other => Err(format!("不支持的协议 {other}://，服务端地址应当是 http(s)://host:port")),
        };
        FalconClient { inner: Arc::new(Inner { base, ws_base, http, auth: Auth::new() }) }
    }

    /// 规范化之后的基址（末尾不带 `/`）。
    pub fn base_url(&self) -> &str {
        &self.inner.base
    }

    /// 当前的登录 token（`falcon_token` cookie 的值）。
    pub fn token(&self) -> Option<String> {
        self.inner.auth.token()
    }

    /// 设置自动重登用的访问密码（app 从钥匙串取出来）。设了之后：REST 收到 401
    /// 时自动登录一次并重放原请求一次，WS 收到 4401 时自动登录再重连；再失败才
    /// 返回未认证 / 推 [`AuthEvent::LoginRequired`]。
    ///
    /// 服务端的 token 只在内存里，后端一重启就全部失效——这正是它要解决的事。
    /// 密码被服务端拒绝时客户端会清掉它（别拿着一个错密码反复试），
    /// [`logout`](FalconClient::logout) 也会清掉。
    pub fn set_relogin_password(&self, password: Option<String>) {
        *self.inner.auth.password.write() = password;
    }

    pub fn has_relogin_password(&self) -> bool {
        self.inner.auth.password.read().is_some()
    }

    /// 订阅登录态变化。每个订阅端各自收到全部后续事件；丢掉即退订。
    pub fn subscribe_auth(&self) -> AuthEvents {
        let (tx, rx) = unbounded();
        self.inner.auth.subs.lock().push(tx);
        rx
    }

    pub fn auth_state(&self) -> AuthState {
        *self.inner.auth.state.lock()
    }

    /// 把一段工作 spawn 到网络运行时上，返回与执行器无关的 future（见 crate 文档）。
    /// 闭包在第一次 poll 时才被调用：future 是惰性的，不 poll 就不发请求。
    pub(crate) fn call<T, Fut>(
        &self,
        f: impl FnOnce(Arc<Inner>) -> Fut + MaybeSend + 'static,
    ) -> impl Future<Output = ApiResult<T>> + MaybeSend + 'static
    where
        T: MaybeSend + 'static,
        Fut: Future<Output = ApiResult<T>> + MaybeSend + 'static,
    {
        let inner = self.inner.clone();
        async move {
            runtime::run(f(inner))
                .await
                .unwrap_or_else(|_| Err(ApiError::internal("网络运行时已关停")))
        }
    }

    /// 发一个请求、解 JSON 响应。请求体由 [`json_body`] 在调用时就序列化好（参数
    /// 不跨 await 借用），序列化失败留到 await 时报出来。
    ///
    /// 刻意不对请求体类型泛型：edition 2024 的 `impl Trait` 会捕获全部泛型参数，
    /// 一个借用了 `&str` 的请求体类型会让返回的 future 带上那个生命周期。
    pub(crate) fn json<T>(
        &self,
        method: Method,
        path: String,
        body: ApiResult<ReqBody>,
    ) -> impl Future<Output = ApiResult<T>> + MaybeSend + 'static
    where
        T: DeserializeOwned + MaybeSend + 'static,
    {
        self.call(move |inner| async move {
            let req = Req::new(method, path, body?);
            inner.fetch_json(&req).await
        })
    }

    pub(crate) fn get<T>(&self, path: String) -> impl Future<Output = ApiResult<T>> + MaybeSend + 'static
    where
        T: DeserializeOwned + MaybeSend + 'static,
    {
        self.json(Method::GET, path, Ok(ReqBody::Empty))
    }

    /// 不带请求体的 POST / DELETE（React 版里 `request("POST", url)` 那种）。
    pub(crate) fn bare<T>(&self, method: Method, path: String) -> impl Future<Output = ApiResult<T>> + MaybeSend + 'static
    where
        T: DeserializeOwned + MaybeSend + 'static,
    {
        self.json(method, path, Ok(ReqBody::Empty))
    }
}

/// 把请求体序列化成 JSON。
pub(crate) fn json_body<B: Serialize + ?Sized>(body: &B) -> ApiResult<ReqBody> {
    serde_json::to_vec(body)
        .map(|v| ReqBody::Json(Bytes::from(v)))
        .map_err(|e| ApiError::internal(format!("请求体序列化失败：{e}")))
}

#[cfg(target_family = "wasm")]
fn build_http() -> Result<reqwest::Client, String> {
    // 浏览器的 fetch：TLS、连接池、代理、超时都归浏览器管，能设的只剩请求头。
    // credentials 用 fetch 的缺省 same-origin——服务端就是页面的源，登录 cookie 自动带上
    reqwest::Client::builder().build().map_err(|e| crate::error::error_chain(&e))
}

#[cfg(not(target_family = "wasm"))]
fn build_http() -> Result<reqwest::Client, String> {
    use crate::error::error_chain;
    use crate::runtime::runtime;
    let tls = crate::tls::client_config()?;
    // hyper 的连接池在 build 时就要一个 tokio 上下文来挂后台任务
    let _guard = runtime().enter();
    reqwest::Client::builder()
        .use_preconfigured_tls((*tls).clone())
        .user_agent(concat!("falcon-native/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        // 比 Node 的 keepAliveTimeout（fastify 默认 72s）短：空闲连接必须由我们先关，
        // 否则会撞上"服务端刚关、我们正好拿它发请求"的竞态，报一个莫名的连接重置
        .pool_idle_timeout(Duration::from_secs(55))
        // 按键延迟敏感的是 WS，但 REST 也没理由等 Nagle
        .tcp_nodelay(true)
        // 不走代理：WS 那边是自己连 TCP 的，不认代理；REST 走代理、WS 不走只会制造
        // "列表能刷、终端连不上"这种更难查的状况。开发机上的 https_proxy 环境变量
        // 还会把 127.0.0.1 也劫走。
        .no_proxy()
        .build()
        .map_err(|e| error_chain(&e))
}

impl Inner {
    pub(crate) fn ws_url(&self, path: &str) -> String {
        format!("{}{}", self.ws_base, path)
    }

    fn http(&self) -> ApiResult<&reqwest::Client> {
        self.http.as_ref().map_err(|e| ApiError::internal(e.clone()))
    }

    async fn build(&self, req: &Req) -> ApiResult<reqwest::RequestBuilder> {
        #[allow(unused_mut)]
        let mut rb = self.http()?.request(req.method.clone(), format!("{}{}", self.base, req.path));
        #[cfg(not(target_family = "wasm"))]
        if let Some(cookie) = self.auth.cookie_header() {
            rb = rb.header(COOKIE, cookie);
        }
        Ok(match &req.body {
            ReqBody::Empty => rb,
            ReqBody::Json(b) => rb.header(CONTENT_TYPE, "application/json").body(b.clone()),
            ReqBody::Raw { data, content_type } => {
                rb.header(CONTENT_TYPE, content_type.as_str()).body(data.clone())
            }
            #[cfg(not(target_family = "wasm"))]
            ReqBody::File { path, progress } => {
                let file = tokio::fs::File::open(path)
                    .await
                    .map_err(|e| ApiError::io("打开要上传的文件失败", &e))?;
                // 长度取自这个已打开的句柄：与随后读出来的字节是同一个文件
                let len = file
                    .metadata()
                    .await
                    .map_err(|e| ApiError::io("读取要上传的文件失败", &e))?
                    .len();
                // Content-Length 必须准确（ADR 0008 决定三）：服务端收满这个数才把临时
                // 文件改名到位。流式 body 自己不知道长度，这里显式写头——hyper 对用户
                // 显式给的 Content-Length 照单全收，body 短了它会报错而不是悄悄截断。
                rb.header(CONTENT_TYPE, "application/octet-stream")
                    .header(CONTENT_LENGTH, len)
                    .body(crate::api::transfer::file_body(file, len, progress.clone()))
            }
        })
    }

    async fn send_once(&self, req: &Req) -> ApiResult<reqwest::Response> {
        let rb = self.build(req).await?;
        rb.send().await.map_err(|e| ApiError::network(&e))
    }

    /// 发请求；401 时（设了重登密码就）重登一次再重放一次。返回的响应可能仍是
    /// 非 2xx，由调用方按各自的方式收尾。
    pub(crate) async fn execute(self: &Arc<Self>, req: &Req) -> ApiResult<reqwest::Response> {
        let used = self.auth.token();
        let resp = self.send_once(req).await?;
        if resp.status() != StatusCode::UNAUTHORIZED || !req.relogin {
            return Ok(resp);
        }
        match self.relogin(used).await {
            Relogin::Done => {
                drop(resp);
                let used = self.auth.token();
                let resp = self.send_once(req).await?;
                if resp.status() == StatusCode::UNAUTHORIZED {
                    self.auth.login_required(&used);
                }
                Ok(resp)
            }
            // NoPassword / Rejected 已经在 relogin 里推过 LoginRequired；
            // Failed 是登录请求没发出去，把原来的 401 交回去就好
            Relogin::NoPassword | Relogin::Rejected | Relogin::Failed(_) => Ok(resp),
        }
    }

    pub(crate) async fn fetch_json<T: DeserializeOwned>(self: &Arc<Self>, req: &Req) -> ApiResult<T> {
        let resp = self.execute(req).await?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| ApiError::network(&e))?;
        if !(200..300).contains(&status) {
            return Err(ApiError::http(status, &body));
        }
        serde_json::from_slice(&body).map_err(|e| ApiError::decode(status, &e, &body))
    }

    /// 自动重登。`stale` 是撞上 401 / 4401 的那次请求用的 token。
    ///
    /// 拿到锁之后先看 token 是不是已经换过了——等锁的这段时间里别人（另一个请求、
    /// 另一条会话 socket）可能已经登好了，那就直接用新的，不再登第二次。
    pub(crate) async fn relogin(self: &Arc<Self>, stale: Option<String>) -> Relogin {
        let _guard = self.auth.relogin.lock().await;
        let current = self.auth.token();
        if current.is_some() && current != stale {
            return Relogin::Done;
        }
        let Some(password) = self.auth.password() else {
            self.auth.login_required(&stale);
            return Relogin::NoPassword;
        };
        match self.login_with(&password).await {
            Ok(()) => Relogin::Done,
            Err(e) if e.is_unauthorized() => {
                *self.auth.password.write() = None;
                self.auth.login_required(&stale);
                Relogin::Rejected
            }
            Err(e) => Relogin::Failed(e),
        }
    }

    /// `POST /api/auth/login`，从 Set-Cookie 里取 token 存起来。
    pub(crate) async fn login_with(self: &Arc<Self>, password: &str) -> ApiResult<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            password: &'a str,
        }
        let body = serde_json::to_vec(&Body { password })
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let req = Req {
            method: Method::POST,
            path: "/api/auth/login".to_owned(),
            body: ReqBody::Json(Bytes::from(body)),
            relogin: false,
        };
        let resp = self.send_once(&req).await?;
        let status = resp.status().as_u16();
        #[cfg(not(target_family = "wasm"))]
        let token = token_from_headers(resp.headers());
        // 浏览器读不到 Set-Cookie（httpOnly，而且 fetch 本来就把它藏起来），cookie 已经
        // 进了浏览器的罐子。这里记一枚每次登录都不同的占位 token：登录态的"新旧"判断
        // （login_required / relogin 里比 token）照旧成立
        #[cfg(target_family = "wasm")]
        let token = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(1);
            Some(format!("browser-{}", SEQ.fetch_add(1, Ordering::Relaxed)))
        };
        let body = resp.bytes().await.map_err(|e| ApiError::network(&e))?;
        if !(200..300).contains(&status) {
            return Err(ApiError::http(status, &body));
        }
        let Some(token) = token else {
            return Err(ApiError {
                status: Some(status),
                message: format!(
                    "登录成功但响应里没有 {COOKIE_NAME} cookie（Set-Cookie 被反向代理去掉了？）"
                ),
                body: serde_json::from_slice(&body).ok(),
                kind: ApiErrorKind::Decode,
            });
        };
        self.auth.logged_in(token);
        Ok(())
    }

    pub(crate) fn logged_out(&self) {
        self.auth.logged_out();
    }
}

/// 从响应头里挑出 `falcon_token`。
#[cfg(not(target_family = "wasm"))]
///
/// 只认名字对得上的那一条：反代可能自己也种 cookie（会话粘滞之类），那些不归我们管。
/// 服务端登出时会种一个清空的同名 cookie（值为空），那不算 token。
fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers.get_all(SET_COOKIE).iter().find_map(|v| {
        let v = v.to_str().ok()?;
        let pair = v.split(';').next()?.trim();
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COOKIE_NAME && !value.trim().is_empty())
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn picks_falcon_token_among_cookies() {
        let mut h = HeaderMap::new();
        h.append(SET_COOKIE, HeaderValue::from_static("route=abc; Path=/"));
        h.append(
            SET_COOKIE,
            HeaderValue::from_static(
                "falcon_token=0123abcd; Max-Age=2592000; Path=/; HttpOnly; SameSite=Lax",
            ),
        );
        assert_eq!(token_from_headers(&h).as_deref(), Some("0123abcd"));
    }

    #[test]
    fn cleared_cookie_is_not_a_token() {
        let mut h = HeaderMap::new();
        h.append(
            SET_COOKIE,
            HeaderValue::from_static("falcon_token=; Max-Age=0; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT"),
        );
        assert_eq!(token_from_headers(&h), None);
        assert_eq!(token_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn base_url_normalization() {
        let c = FalconClient::new(Url::parse("https://falcon.example/sub/?x=1#y").unwrap());
        assert_eq!(c.base_url(), "https://falcon.example/sub");
        assert_eq!(c.inner.ws_url("/ws/sessions/a"), "wss://falcon.example/sub/ws/sessions/a");
        let c = FalconClient::new(Url::parse("http://127.0.0.1:4923").unwrap());
        assert_eq!(c.base_url(), "http://127.0.0.1:4923");
        assert_eq!(c.inner.ws_url("/ws/x"), "ws://127.0.0.1:4923/ws/x");
    }

    #[test]
    fn login_required_does_not_clobber_a_fresh_token() {
        let auth = Auth::new();
        let mut rx = {
            let (tx, rx) = unbounded();
            auth.subs.lock().push(tx);
            rx
        };
        auth.logged_in("new".into());
        // 一个拿着旧 token 的请求迟到的 401：新 token 不能被清掉，也不推事件
        auth.login_required(&Some("old".into()));
        assert_eq!(auth.token().as_deref(), Some("new"));
        assert_eq!(rx.try_recv().ok(), Some(AuthEvent::LoggedIn));
        assert!(rx.try_recv().is_err(), "不该有更多事件");
        // 真的是当前 token 被拒：清掉，推一次；再来一次不重复推
        auth.login_required(&Some("new".into()));
        auth.login_required(&None);
        assert_eq!(auth.token(), None);
        assert_eq!(rx.try_recv().ok(), Some(AuthEvent::LoginRequired));
        assert!(rx.try_recv().is_err());
    }
}
