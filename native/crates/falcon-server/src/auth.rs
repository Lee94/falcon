//! 登录态与原始字节令牌。移植自 `packages/server/src/auth.ts`。
//!
//! 登录 token **只存在内存里**，后端一重启全员重新登录（客户端靠存着的密码自动重登）。
//! 改成持久化属于行为变更，要单独决策（设计文档 §10），这里照旧。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::{hash_password, verify_password};

pub const COOKIE_NAME: &str = "falcon_token";
const TOKEN_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// 原始字节令牌的寿命。HTML 预览里懒加载的图片、滚到底才请求的字体，都拿着
/// 页面打开时那枚令牌，太短会让"开着不动一会儿"的预览悄悄缺图；再长则一个
/// 泄露的 URL 能读项目文件的窗口也跟着长。每次读文件都会换新的，6 小时够用。
const RAW_TOKEN_TTL_MS: i64 = 6 * 3600 * 1000;

const PASSWORD_HASH_KEY: &str = "password_hash";

/// Auth 只需要 settings 表的读写；单测用内存实现
pub trait SettingsStore: Send + Sync {
    fn get_setting(&self, key: &str) -> Option<String>;
    fn set_setting(&self, key: &str, value: &str);
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    hex::encode(buf)
}

struct RawEntry {
    project_id: String,
    expiry: i64,
}

#[derive(Default)]
struct State {
    tokens: HashMap<String, i64>,
    /// 原始字节路由的作用域令牌：token → { 项目, 到期 }。与登录 token 完全无关——
    /// 它会出现在 `<iframe src>` 的 URL 里、被沙箱内的脚本读到，所以只能授予
    /// "读这个项目的文件"这一件事，不能反推出登录态。按项目复用（同一项目短时间
    /// 内连续读文件不该攒出一堆），logout 时整批作废。
    raw_tokens: HashMap<String, RawEntry>,
    raw_by_project: HashMap<String, String>,
    /// password_hash 的内存缓存（None = 还没读过）。每个 HTTP 请求和 WS 建连都要过
    /// required()，不该每次都打一遍 SQLite；唯一的写入方是 set_password，同步更新缓存即可。
    hash_cache: Option<Option<String>>,
}

pub struct Auth<S: SettingsStore> {
    db: S,
    loopback: bool,
    state: Mutex<State>,
}

impl<S: SettingsStore> Auth<S> {
    pub fn new(db: S, loopback: bool) -> Self {
        Auth { db, loopback, state: Mutex::new(State::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn password_hash(&self, st: &mut State) -> Option<String> {
        if st.hash_cache.is_none() {
            st.hash_cache = Some(self.db.get_setting(PASSWORD_HASH_KEY));
        }
        st.hash_cache.clone().flatten()
    }

    pub fn password_set(&self) -> bool {
        let mut st = self.lock();
        self.password_hash(&mut st).is_some()
    }

    /// 需要认证 = 设置过密码，或对外绑定
    pub fn required(&self) -> bool {
        self.password_set() || !self.loopback
    }

    pub fn login(&self, password: &str) -> Option<String> {
        let mut st = self.lock();
        let stored = self.password_hash(&mut st)?;
        if !verify_password(password, &stored) {
            return None;
        }
        let token = random_hex(32);
        st.tokens.insert(token.clone(), now_ms() + TOKEN_TTL_MS);
        Some(token)
    }

    pub fn logout(&self, token: Option<&str>) {
        let mut st = self.lock();
        if let Some(t) = token {
            st.tokens.remove(t);
        }
        st.raw_tokens.clear();
        st.raw_by_project.clear();
    }

    /// 给一个项目签发（或复用）原始字节令牌。剩余寿命不足一半就换新的：
    /// 拿到手的令牌至少还能活 3 小时，前端不必关心到期。
    pub fn raw_token(&self, project_id: &str) -> String {
        let now = now_ms();
        let mut st = self.lock();
        if let Some(existing) = st.raw_by_project.get(project_id).cloned() {
            if st.raw_tokens.get(&existing).is_some_and(|e| e.expiry - now > RAW_TOKEN_TTL_MS / 2) {
                return existing;
            }
            st.raw_tokens.remove(&existing);
        }
        Self::prune_raw_tokens(&mut st, now);
        let token = random_hex(24);
        st.raw_tokens.insert(token.clone(), RawEntry { project_id: project_id.to_string(), expiry: now + RAW_TOKEN_TTL_MS });
        st.raw_by_project.insert(project_id.to_string(), token.clone());
        token
    }

    /// 令牌对得上项目且没过期。不认证的部署下调用方不该走到这里（先看 is_authenticated）
    pub fn raw_token_valid(&self, token: Option<&str>, project_id: &str) -> bool {
        let Some(token) = token else { return false };
        let mut st = self.lock();
        let Some(entry) = st.raw_tokens.get(token) else { return false };
        if entry.expiry < now_ms() {
            st.raw_tokens.remove(token);
            return false;
        }
        entry.project_id == project_id
    }

    fn prune_raw_tokens(st: &mut State, now: i64) {
        let expired: Vec<(String, String)> = st
            .raw_tokens
            .iter()
            .filter(|(_, e)| e.expiry < now)
            .map(|(t, e)| (t.clone(), e.project_id.clone()))
            .collect();
        for (token, project_id) in expired {
            st.raw_tokens.remove(&token);
            if st.raw_by_project.get(&project_id) == Some(&token) {
                st.raw_by_project.remove(&project_id);
            }
        }
    }

    /// 已设密码时必须带对当前密码
    pub fn set_password(&self, next: &str, current: Option<&str>) -> bool {
        let mut st = self.lock();
        if let Some(stored) = self.password_hash(&mut st)
            && !current.is_some_and(|c| verify_password(c, &stored))
        {
            return false;
        }
        let hash = hash_password(next);
        self.db.set_setting(PASSWORD_HASH_KEY, &hash);
        st.hash_cache = Some(Some(hash));
        true
    }

    /// `token` 是请求带来的 `falcon_token` cookie。命中则滑动续期
    pub fn is_authenticated(&self, token: Option<&str>) -> bool {
        if !self.required() {
            return true;
        }
        let Some(token) = token.filter(|t| !t.is_empty()) else { return false };
        let now = now_ms();
        let mut st = self.lock();
        match st.tokens.get(token) {
            Some(&expiry) if expiry >= now => {
                // 滑动过期
                st.tokens.insert(token.to_string(), now + TOKEN_TTL_MS);
                true
            }
            _ => {
                st.tokens.remove(token);
                false
            }
        }
    }

    /// 登录成功时种的 cookie（与 Node 版 @fastify/cookie 的属性一致）
    pub fn set_cookie_header(token: &str) -> String {
        format!("{COOKIE_NAME}={token}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax", TOKEN_TTL_MS / 1000)
    }

    /// 登出时清掉 cookie
    pub fn clear_cookie_header() -> String {
        format!("{COOKIE_NAME}=; Max-Age=0; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只需要 get_setting / set_setting；password_hash 设了，鉴权就是"需要的"
    #[derive(Default)]
    struct FakeDb(Mutex<HashMap<String, String>>);

    impl SettingsStore for FakeDb {
        fn get_setting(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set_setting(&self, key: &str, value: &str) {
            self.0.lock().unwrap().insert(key.into(), value.into());
        }
    }

    #[test]
    fn raw_token_scoped_reused_and_revoked_on_logout() {
        let auth = Auth::new(FakeDb::default(), true);
        let a = auth.raw_token("p1");
        assert!(a.len() >= 32);
        assert!(auth.raw_token_valid(Some(&a), "p1"));
        assert!(!auth.raw_token_valid(Some(&a), "p2"));
        assert!(!auth.raw_token_valid(None, "p1"));
        assert!(!auth.raw_token_valid(Some("nope"), "p1"));

        // 同一项目短时间内再要一枚，给的还是这枚
        assert_eq!(auth.raw_token("p1"), a);
        let b = auth.raw_token("p2");
        assert_ne!(a, b);
        assert!(auth.raw_token_valid(Some(&b), "p2"));

        auth.logout(None);
        assert!(!auth.raw_token_valid(Some(&a), "p1"));
        assert!(!auth.raw_token_valid(Some(&b), "p2"));
        // 作废后再签是新的
        assert_ne!(auth.raw_token("p1"), a);
    }

    #[test]
    fn raw_token_is_not_a_login_token() {
        let auth = Auth::new(FakeDb::default(), true);
        assert!(auth.set_password("secret1", None));
        let raw = auth.raw_token("p1");
        assert!(!auth.is_authenticated(Some(&raw)));
    }

    #[test]
    fn login_flow_and_password_change() {
        let auth = Auth::new(FakeDb::default(), true);
        // 回环、没设密码：不需要认证
        assert!(!auth.required() && auth.is_authenticated(None));
        assert_eq!(auth.login("x"), None);
        assert!(auth.set_password("pw1", None));
        assert!(auth.required());
        assert!(!auth.is_authenticated(None));
        let t = auth.login("pw1").unwrap();
        assert!(auth.is_authenticated(Some(&t)));
        assert_eq!(auth.login("bad"), None);
        // 改密码要带对当前密码
        assert!(!auth.set_password("pw2", None));
        assert!(!auth.set_password("pw2", Some("bad")));
        assert!(auth.set_password("pw2", Some("pw1")));
        assert!(auth.login("pw2").is_some());
        auth.logout(Some(&t));
        assert!(!auth.is_authenticated(Some(&t)));
        // 对外绑定时即使没密码也要认证
        assert!(Auth::new(FakeDb::default(), false).required());
    }
}
