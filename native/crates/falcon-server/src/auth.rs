//! 原始字节令牌，以及仍留着的登录接口。移植自 `packages/server/src/auth.ts`。
//!
//! 能连上就允许使用：访问密码和是否绑在回环地址都不再挡请求。`required()` 恒为 false，
//! `is_authenticated` 恒为 true。登录、登出、设密码的路由还在，避免旧客户端直接 404，
//! 但设下来的哈希不会把门禁打开。HTML 预览的原始字节令牌与登录无关，继续只授
//! 「读绑定范围内的文件」。

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

/// 原 project token 仍按项目工作目录读取。绝对路径预览另签目录范围：根、宿主身份与 kind
/// 都在签发时固定，不能由 raw 请求重定位。文件直接位于磁盘 / UNC 根时只授权该文件，
/// 不能为了预览一个文件把整块盘交给不可信 HTML。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RawScope {
    Project,
    Directory {
        kind: falcon_proto::HostKind,
        host: String,
        root: String,
        only_file: Option<String>,
    },
}

struct RawEntry {
    project_id: String,
    scope: RawScope,
    expiry: i64,
}

#[derive(Default)]
struct State {
    tokens: HashMap<String, i64>,
    /// 原始字节路由的作用域令牌：token → { 项目, 范围, 到期 }。与登录 token 完全无关——
    /// 它会出现在 `<iframe src>` 的 URL 里、被沙箱内的脚本读到，所以只能授予
    /// "读绑定范围内的文件"这一件事，不能反推出登录态。按项目 + 完整范围复用，
    /// 同一项目下不同目录绝不重用 / 修改旧 token 的根，logout 时整批作废。
    raw_tokens: HashMap<String, RawEntry>,
    raw_by_scope: HashMap<(String, RawScope), String>,
    /// password_hash 的内存缓存（None = 还没读过）。status、登录和改密码会读它。
    /// 唯一的写入方是 set_password，同步更新缓存即可。`required()` 不再读它。
    hash_cache: Option<Option<String>>,
}

pub struct Auth<S: SettingsStore> {
    db: S,
    state: Mutex<State>,
}

impl<S: SettingsStore> Auth<S> {
    pub fn new(db: S) -> Self {
        Auth { db, state: Mutex::new(State::default()) }
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

    /// 能连上就允许使用。库里残留的 `password_hash`、非回环绑定都不构成门禁。
    pub fn required(&self) -> bool {
        false
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
        st.raw_by_scope.clear();
    }

    /// 给一个项目签发（或复用）原始字节令牌。剩余寿命不足一半就换新的：
    /// 拿到手的令牌至少还能活 3 小时，前端不必关心到期。
    pub fn raw_token(&self, project_id: &str) -> String {
        self.raw_token_for_scope(project_id, RawScope::Project)
    }

    pub(crate) fn raw_token_for_scope(&self, project_id: &str, scope: RawScope) -> String {
        let now = now_ms();
        let mut st = self.lock();
        let key = (project_id.to_string(), scope.clone());
        if let Some(existing) = st.raw_by_scope.get(&key).cloned() {
            if st.raw_tokens.get(&existing).is_some_and(|e| e.expiry - now > RAW_TOKEN_TTL_MS / 2) {
                return existing;
            }
            st.raw_tokens.remove(&existing);
        }
        Self::prune_raw_tokens(&mut st, now);
        let token = random_hex(24);
        st.raw_tokens.insert(token.clone(), RawEntry { project_id: project_id.to_string(), scope, expiry: now + RAW_TOKEN_TTL_MS });
        st.raw_by_scope.insert(key, token.clone());
        token
    }

    /// 令牌对得上项目且没过期。不认证的部署下调用方不该走到这里（先看 is_authenticated）
    pub fn raw_token_valid(&self, token: Option<&str>, project_id: &str) -> bool {
        self.raw_scope(token, project_id).is_some()
    }

    pub(crate) fn raw_scope(&self, token: Option<&str>, project_id: &str) -> Option<RawScope> {
        let token = token?;
        let mut st = self.lock();
        Self::prune_raw_tokens(&mut st, now_ms());
        let entry = st.raw_tokens.get(token)?;
        (entry.project_id == project_id).then(|| entry.scope.clone())
    }

    fn prune_raw_tokens(st: &mut State, now: i64) {
        st.raw_tokens.retain(|_, e| e.expiry >= now);
        // 同时清掉反向索引，避免不断预览新目录后积累已过期的目录身份。
        st.raw_by_scope.retain(|_, token| st.raw_tokens.contains_key(token));
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
        let auth = Auth::new(FakeDb::default());
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
    fn directory_tokens_are_immutable_scoped_expiring_and_revoked() {
        let auth = Auth::new(FakeDb::default());
        let scope = RawScope::Directory {
            kind: falcon_proto::HostKind::Posix, host: "local".into(), root: "/tmp/a".into(), only_file: None,
        };
        let token = auth.raw_token_for_scope("p1", scope.clone());
        assert_eq!(auth.raw_scope(Some(&token), "p1"), Some(scope.clone()));
        assert_eq!(auth.raw_token_for_scope("p1", scope.clone()), token);
        assert_ne!(auth.raw_token("p1"), token);
        assert_eq!(auth.raw_scope(Some(&token), "p2"), None);
        for changed in [
            RawScope::Directory { kind: falcon_proto::HostKind::Posix, host: "local".into(), root: "/tmp/b".into(), only_file: None },
            RawScope::Directory { kind: falcon_proto::HostKind::Posix, host: "other".into(), root: "/tmp/a".into(), only_file: None },
            RawScope::Directory { kind: falcon_proto::HostKind::Windows, host: "local".into(), root: "/tmp/a".into(), only_file: None },
            RawScope::Directory { kind: falcon_proto::HostKind::Posix, host: "local".into(), root: "/tmp/a".into(), only_file: Some("a.html".into()) },
        ] {
            assert_ne!(auth.raw_token_for_scope("p1", changed), token);
            assert_eq!(auth.raw_scope(Some(&token), "p1"), Some(scope.clone()));
        }
        auth.lock().raw_tokens.get_mut(&token).unwrap().expiry = now_ms() - 1;
        assert_eq!(auth.raw_scope(Some(&token), "p1"), None);
        assert!(!auth.lock().raw_by_scope.values().any(|t| t == &token));
        let next = auth.raw_token_for_scope("p1", scope.clone());
        assert_ne!(next, token);
        // 半寿命滚动换新不改旧 token 的根。
        auth.lock().raw_tokens.get_mut(&next).unwrap().expiry = now_ms() + RAW_TOKEN_TTL_MS / 2 - 1;
        let fresh = auth.raw_token_for_scope("p1", scope);
        assert_ne!(fresh, next);
        assert_eq!(auth.raw_scope(Some(&next), "p1"), None);
        auth.logout(None);
        assert_eq!(auth.raw_scope(Some(&fresh), "p1"), None);
        assert!(auth.lock().raw_tokens.is_empty() && auth.lock().raw_by_scope.is_empty());
    }

    /// 路由测试用真实 axum 请求验证过期；只改测试 Auth 的内存，不改时钟或真实数据。
    #[cfg(test)]
    impl<S: SettingsStore> Auth<S> {
        pub(crate) fn expire_raw_for_test(&self, token: &str) {
            self.lock().raw_tokens.get_mut(token).expect("issued raw token").expiry = now_ms() - 1;
        }
    }

    #[test]
    fn password_and_raw_token_do_not_gate_access() {
        let auth = Auth::new(FakeDb::default());
        assert!(!auth.required() && auth.is_authenticated(None));
        assert!(auth.set_password("secret1", None));
        // 设了密码、拿原始字节令牌冒充登录，访问仍然开放
        assert!(!auth.required());
        assert!(auth.is_authenticated(None));
        assert!(auth.is_authenticated(Some(&auth.raw_token("p1"))));
        let t = auth.login("secret1").unwrap();
        auth.logout(Some(&t));
        assert!(auth.is_authenticated(None));
        assert!(auth.is_authenticated(Some(&t)));
    }

    #[test]
    fn password_change_still_checks_the_current_password() {
        let auth = Auth::new(FakeDb::default());
        assert_eq!(auth.login("x"), None);
        assert!(auth.set_password("pw1", None));
        assert!(auth.login("pw1").is_some());
        assert_eq!(auth.login("bad"), None);
        assert!(!auth.set_password("pw2", None));
        assert!(!auth.set_password("pw2", Some("bad")));
        assert!(auth.set_password("pw2", Some("pw1")));
        assert!(auth.login("pw2").is_some());
        assert!(auth.is_authenticated(None));
    }
}
