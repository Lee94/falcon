//! sudo askpass 的三条路由（routes.ts 的 `/api/askpass*`）。helper 的长轮询不走登录 cookie，
//! 验的是写进宿主机 conf 的专用 Bearer 令牌——helper 不是浏览器。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::askpass::hub::AskpassError;

/// 挂在公开路由上（自己验 Bearer）
pub fn helper_router() -> Router<AppState> {
    Router::new().route("/api/askpass", post(request))
}

/// 要登录：网页对话框列出 / 答复
pub fn router() -> Router<AppState> {
    Router::new().route("/api/askpass/pending", get(pending)).route("/api/askpass/{id}/answer", post(answer))
}

/// sudo askpass helper 长轮询。
async fn request(State(state): State<AppState>, headers: HeaderMap, body: LenientJson) -> ApiResult<Json<Value>> {
    let header = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !state.askpass.token_matches(header) {
        return Err(ApiError::unauthorized());
    }
    let prompt = body.str("prompt").filter(|p| !p.is_empty()).unwrap_or("Password:");
    match state.askpass.request(prompt, body.str("sessionId")).await {
        Ok(password) => Ok(Json(json!({ "password": password }))),
        Err(AskpassError::Cancelled) => Err(ApiError::conflict("cancelled")),
        Err(AskpassError::Timeout) => Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout")),
    }
}

async fn pending(State(state): State<AppState>) -> Json<Value> {
    Json(Value::Array(
        state.askpass.pending_prompts().into_iter().map(|p| json!({ "id": p.id, "prompt": p.prompt })).collect(),
    ))
}

async fn answer(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Value>> {
    if body.0.get("cancel") == Some(&Value::Bool(true)) {
        if !state.askpass.cancel(&id) {
            return Err(ApiError::not_found("不存在"));
        }
        return Ok(ok());
    }
    let Some(password) = body.str("password") else { return Err(ApiError::bad_request("missing password")) };
    if !state.askpass.answer(&id, password.to_string()) {
        return Err(ApiError::not_found("不存在"));
    }
    Ok(ok())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::api::test_support::{TestApp, json_of};
    use crate::askpass::install::{askpass_session_env, prepend_path, write_local_askpass};
    use crate::sessions::agent::{AGENT_NATIVE_ENV, write_local_launcher, write_local_native_launcher};
    use falcon_proto::SessionAgent;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Stdio;
    use std::time::Duration;

    /// 登录 shell 可以重建 PATH；CLI 的管道子进程仍须找到包装，并完成真实 helper 的请求/答复。
    /// sudo 与 Claude 都用替身，不提权、不调用模型，密码只使用测试值。
    #[tokio::test]
    async fn agent_askpass_survives_login_shell_path_reset() {
        for native in [false, true] {
            let app = TestApp::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            app.state.askpass.set_origin(&format!("http://{}", listener.local_addr().unwrap()));
            let router = app.router.clone();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let askpass_bin = write_local_askpass(&app.data_dir, &app.state.askpass).unwrap();
            let executable = |path: &std::path::Path, body: &str| {
                std::fs::write(path, body).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            let shadow = app.root.join("shadow-bin");
            std::fs::create_dir(&shadow).unwrap();
            executable(&shadow.join("sudo"), "#!/bin/sh\necho WRONG_SUDO >&2\nexit 93\n");
            executable(&app.bin.join("claude"), "#!/bin/sh\nexec sudo true </dev/null\n");
            let real_sudo = app.bin.join("real-sudo");
            executable(&real_sudo, r#"#!/bin/sh
[ "$1" = -A ] || exit 91
password=$("$SUDO_ASKPASS" 'Test sudo password:') || exit $?
[ "$password" = test-password ] || exit 92
printf 'ASKPASS_COMPLETE\n'
"#);
            let shell = app.root.join("login-shell");
            executable(&shell, r#"#!/bin/sh
if [ "$3" = -c ]; then
  PATH="$FAKE_LOGIN_PATH"
  export PATH
  exec /bin/sh -c "$4"
fi
"#);
            let launcher = if native {
                write_local_native_launcher(&app.data_dir, SessionAgent::Claude, shell.to_str().unwrap())
            } else {
                write_local_launcher(&app.data_dir, SessionAgent::Claude, shell.to_str().unwrap())
            }.unwrap();
            let login_path = format!("{}:{}:/usr/bin:/bin", app.bin.display(), shadow.display());
            let mut env = vec![("PATH".to_string(), login_path.clone())];
            prepend_path(&mut env, askpass_bin.to_str().unwrap());
            env.extend(askpass_session_env(askpass_bin.to_str().unwrap(), "askpass-test-session"));
            // macOS 自带 Python 不一定可用，helper 从测试进程的 PATH 找开发环境里的 Python。
            let python = std::env::var("PATH").unwrap_or_default().split(':')
                .map(|dir| std::path::Path::new(dir).join("python3"))
                .find(|path| path.is_file()).expect("askpass helper 需要 Python 3");
            std::os::unix::fs::symlink(python, app.bin.join("python3")).unwrap();
            let mut child = tokio::process::Command::new("/bin/sh")
                .arg(launcher)
                .env_clear()
                .envs(env)
                .env("HOME", &app.root)
                .env("FAKE_LOGIN_PATH", login_path)
                .env("FALCON_REAL_SUDO", real_sudo)
                .env(AGENT_NATIVE_ENV, if native { "1" } else { "" })
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn().unwrap();
            let prompt = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let (_, pending) = json_of(app.get("/api/askpass/pending").await).await;
                    if let Some(prompt) = pending.as_array().and_then(|list| list.first()) {
                        break prompt.clone();
                    }
                    assert!(child.try_wait().unwrap().is_none(), "CLI 在发出密码请求前退出，native={native}");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("CLI 应发出密码请求");
            assert_eq!(prompt["prompt"], "Test sudo password:");
            let id = prompt["id"].as_str().unwrap();
            assert_eq!(app.state.askpass.peek(id).unwrap().session_id.as_deref(), Some("askpass-test-session"));
            let (status, _) = json_of(app.post_json(&format!("/api/askpass/{id}/answer"), json!({"password": "test-password"})).await).await;
            assert_eq!(status, StatusCode::OK);
            let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await.unwrap().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("ASKPASS_COMPLETE"));
            assert!(app.state.askpass.pending_prompts().is_empty());
            server.abort();
        }
    }
}
