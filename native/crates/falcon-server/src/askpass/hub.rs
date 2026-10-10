//! sudo askpass 的会合点。移植自 `packages/server/src/askpass/hub.ts`。
//!
//! helper 长轮询 `POST /api/askpass`，网页对话框 `POST /api/askpass/:id/answer` 把密码送回来。
//! 密码只在等待的那一端过一遭，不落盘、不打日志。
//!
//! hub 两边都要用：HTTP 处理器（多线程）等答复，会话核心（LocalSet）要列出待答的弹窗、
//! 推给 Viewer。所以它是 `Send + Sync`，弹窗通知经回调转出去（核心那边接成一条 channel）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot;

use super::scripts::ASKPASS_TIMEOUT_MS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskpassPrompt {
    pub id: String,
    pub prompt: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AskpassError {
    /// 用户在弹窗上点了取消
    #[error("cancelled")]
    Cancelled,
    /// 两分钟没人答
    #[error("timeout")]
    Timeout,
}

struct Pending {
    prompt: AskpassPrompt,
    reply: oneshot::Sender<Result<String, AskpassError>>,
}

type OnPrompt = Box<dyn Fn(AskpassPrompt) + Send + Sync>;

pub struct AskpassHub {
    pub token: String,
    /// helper 要 POST 的源，listen 之后才能定端口
    origin: Mutex<String>,
    on_prompt: Mutex<Option<OnPrompt>>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl Default for AskpassHub {
    fn default() -> Self {
        Self::new()
    }
}

impl AskpassHub {
    pub fn new() -> Self {
        let mut token = [0u8; 24];
        let _ = getrandom::fill(&mut token);
        AskpassHub {
            token: hex::encode(token),
            origin: Mutex::new("http://127.0.0.1:4923".into()),
            on_prompt: Mutex::new(None),
            pending: Mutex::default(),
        }
    }

    pub fn set_origin(&self, origin: &str) {
        *self.origin.lock().unwrap_or_else(|e| e.into_inner()) = origin.trim_end_matches('/').to_string();
    }

    pub fn set_on_prompt(&self, f: impl Fn(AskpassPrompt) + Send + Sync + 'static) {
        *self.on_prompt.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    pub fn helper_url(&self) -> String {
        format!("{}/api/askpass", self.origin.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// `Authorization: Bearer <token>`
    pub fn token_matches(&self, header: Option<&str>) -> bool {
        let Some(h) = header else { return false };
        let h = h.trim();
        let Some(rest) = h.get(..7).filter(|p| p.eq_ignore_ascii_case("bearer ")).map(|_| &h[7..]) else {
            // 也认 "Bearer\t" 之类：TS 的正则是 Bearer\s+
            let mut it = h.splitn(2, char::is_whitespace);
            return it.next().is_some_and(|b| b.eq_ignore_ascii_case("bearer"))
                && it.next().and_then(|t| t.split_whitespace().next()) == Some(self.token.as_str());
        };
        rest.split_whitespace().next() == Some(self.token.as_str())
    }

    /// helper 的长轮询：登记弹窗、通知界面，等答复（或取消 / 两分钟超时）
    pub async fn request(&self, prompt: &str, session_id: Option<&str>) -> Result<String, AskpassError> {
        let id = uuid_v4();
        let prompt = AskpassPrompt {
            id: id.clone(),
            prompt: if prompt.is_empty() { "Password:".into() } else { prompt.into() },
            session_id: session_id.filter(|s| !s.is_empty()).map(str::to_string),
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), Pending { prompt: prompt.clone(), reply: tx });
        if let Some(f) = self.on_prompt.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            // 通知 UI 失败不该把 helper 的请求打掉——用户还可能从别的入口答
            f(prompt);
        }
        match tokio::time::timeout(Duration::from_millis(ASKPASS_TIMEOUT_MS), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(AskpassError::Cancelled),
            Err(_) => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(AskpassError::Timeout)
            }
        }
    }

    fn settle(&self, id: &str, result: Result<String, AskpassError>) -> bool {
        let Some(p) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id) else {
            return false;
        };
        let _ = p.reply.send(result);
        true
    }

    pub fn answer(&self, id: &str, password: String) -> bool {
        self.settle(id, Ok(password))
    }

    pub fn cancel(&self, id: &str) -> bool {
        self.settle(id, Err(AskpassError::Cancelled))
    }

    pub fn peek(&self, id: &str) -> Option<AskpassPrompt> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|p| p.prompt.clone())
    }

    /// 新 Viewer 连上来时补发还在等的弹窗，避免打开页面时已经错过通知
    pub fn pending_prompts(&self) -> Vec<AskpassPrompt> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).values().map(|p| p.prompt.clone()).collect()
    }
}

/// `crypto.randomUUID()` 的等价物（v4，小写带连字符）
pub fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn answer_resolves_the_waiting_helper() {
        let hub = Arc::new(AskpassHub::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        hub.set_on_prompt(move |p| s2.lock().unwrap().push(p));
        let h2 = hub.clone();
        let waiter = tokio::spawn(async move { h2.request("", Some("sess")).await });
        tokio::task::yield_now().await;
        while hub.pending_prompts().is_empty() {
            tokio::task::yield_now().await;
        }
        let p = hub.pending_prompts().pop().unwrap();
        assert_eq!((p.prompt.as_str(), p.session_id.as_deref()), ("Password:", Some("sess")));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(hub.answer(&p.id, "pw".into()));
        assert_eq!(waiter.await.unwrap(), Ok("pw".to_string()));
        assert!(!hub.answer(&p.id, "again".into()));
        assert!(hub.pending_prompts().is_empty());
    }

    #[tokio::test]
    async fn cancel_rejects() {
        let hub = Arc::new(AskpassHub::new());
        let h2 = hub.clone();
        let waiter = tokio::spawn(async move { h2.request("sudo:", None).await });
        while hub.pending_prompts().is_empty() {
            tokio::task::yield_now().await;
        }
        let id = hub.pending_prompts()[0].id.clone();
        assert!(hub.cancel(&id));
        assert_eq!(waiter.await.unwrap(), Err(AskpassError::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_and_forgets() {
        let hub = AskpassHub::new();
        assert_eq!(hub.request("x", None).await, Err(AskpassError::Timeout));
        assert!(hub.pending_prompts().is_empty());
    }

    #[test]
    fn bearer_token() {
        let hub = AskpassHub::new();
        let t = hub.token.clone();
        assert!(hub.token_matches(Some(&format!("Bearer {t}"))));
        assert!(hub.token_matches(Some(&format!("  bearer   {t}  "))));
        assert!(!hub.token_matches(Some("Bearer nope")));
        assert!(!hub.token_matches(Some(&t)));
        assert!(!hub.token_matches(None));
        hub.set_origin("http://127.0.0.1:4961/");
        assert_eq!(hub.helper_url(), "http://127.0.0.1:4961/api/askpass");
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }
}
