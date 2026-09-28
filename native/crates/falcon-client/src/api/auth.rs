//! 认证与 askpass（`/api/auth/*`、`/api/askpass/*`）。

use std::future::Future;

use falcon_proto::{AskpassPrompt, AuthStatus, OkResponse};
use reqwest::Method;
use serde::Serialize;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;

impl FalconClient {
    /// `GET /api/auth/status`：要不要登录、登了没有、设没设过密码。不需要认证。
    pub fn auth_status(&self) -> impl Future<Output = ApiResult<AuthStatus>> + Send + 'static {
        self.get("/api/auth/status".to_owned())
    }

    /// `POST /api/auth/login`。成功后从 Set-Cookie 取出 `falcon_token` 存起来，之后的
    /// REST 与 WS 升级都带上它，并推 [`AuthEvent::LoggedIn`](crate::AuthEvent::LoggedIn)。
    /// 密码错误是 401（`is_unauthorized()`），不会触发自动重登。
    ///
    /// 不会顺手记住密码：要自动重登另调
    /// [`set_relogin_password`](FalconClient::set_relogin_password)。
    pub fn login(&self, password: &str) -> impl Future<Output = ApiResult<()>> + Send + 'static {
        let password = password.to_owned();
        self.call(move |inner| async move { inner.login_with(&password).await })
    }

    /// `POST /api/auth/logout`。不管请求成没成功，本地的 token 与重登密码都清掉，
    /// 推 [`AuthEvent::LoggedOut`](crate::AuthEvent::LoggedOut)：用户要的是"这台机器
    /// 上不再登着"，服务端没收到只意味着那枚 token 会自然过期。
    pub fn logout(&self) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        let fut = self.bare::<OkResponse>(Method::POST, "/api/auth/logout".to_owned());
        let client = self.clone();
        async move {
            let res = fut.await;
            client.inner.logged_out();
            res
        }
    }

    /// `POST /api/auth/password`：设置（`current` 为 `None`）或修改访问密码。
    /// 至少 6 位；已设过密码时必须带对 `current`，而且当前得是登录状态。
    ///
    /// 第一次设密码之后服务端就要求认证了：这个客户端手里还没有 token，接下来的
    /// 请求会 401——要继续用就先 [`login`](FalconClient::login)。
    pub fn set_password(
        &self,
        next: &str,
        current: Option<&str>,
    ) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            next: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            current: Option<&'a str>,
        }
        self.json(Method::POST, "/api/auth/password".to_owned(), json_body(&Body { next, current }))
    }

    /// `GET /api/askpass/pending`：WS 断开期间错过的 sudo / SSH 密码提示（兜底轮询）。
    pub fn pending_askpass(&self) -> impl Future<Output = ApiResult<Vec<AskpassPrompt>>> + Send + 'static {
        self.get("/api/askpass/pending".to_owned())
    }

    /// `POST /api/askpass/:id/answer`：把密码交给等着的 helper。提示已经不在了是 404。
    pub fn answer_askpass(
        &self,
        id: &str,
        password: &str,
    ) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            password: &'a str,
        }
        self.json(
            Method::POST,
            format!("/api/askpass/{}/answer", seg(id)),
            json_body(&Body { password }),
        )
    }

    /// 同一个端点带 `{ cancel: true }`：用户点了取消，helper 那头按取消处理。
    pub fn cancel_askpass(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        #[derive(Serialize)]
        struct Body {
            cancel: bool,
        }
        self.json(
            Method::POST,
            format!("/api/askpass/{}/answer", seg(id)),
            json_body(&Body { cancel: true }),
        )
    }
}
