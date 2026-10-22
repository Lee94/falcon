//! `/api/plugins/*`：右侧栏插件（ADR 0025）。
//!
//! - `GET /api/plugins`：装了哪些（含停用的与清单坏的），以及命令插件该放在哪；
//! - `POST /api/plugins/:id/render`：画一次面板，回 `PluginView`；
//! - `POST /api/plugins/:id/action`：点了 invoke 按钮，回新的 `PluginView`；
//! - `PUT /api/plugins/:id/enabled`：启用 / 停用，回整份列表。
//!
//! 错误码：没有这个插件 404；停用了 / 清单坏了 409（客户端据此重新拉列表）；参数不合法 400；
//! 插件跑了但失败（退出码、超时、输出不合法）502，`error` 里是给插件作者看的原话。

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use falcon_proto::{PluginActionInput, PluginList, PluginRenderInput, PluginView};
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult};
use super::input::{field, trimmed_non_empty};
use crate::plugins::{PluginError, disabled_set, set_enabled};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(list))
        .route("/api/plugins/{id}/render", post(render))
        .route("/api/plugins/{id}/action", post(action))
        .route("/api/plugins/{id}/enabled", put(enabled))
}

impl From<PluginError> for ApiError {
    fn from(e: PluginError) -> Self {
        let status = match e {
            PluginError::NotFound(_) => StatusCode::NOT_FOUND,
            PluginError::Unavailable(_) => StatusCode::CONFLICT,
            PluginError::BadInput(_) => StatusCode::BAD_REQUEST,
            PluginError::Failed(_) => StatusCode::BAD_GATEWAY,
        };
        ApiError::new(status, e.message())
    }
}

async fn scan(state: &AppState) -> ApiResult<PluginList> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || state.plugins.scan(&disabled_set(&state.db))).await.map_err(ApiError::internal)
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<PluginList>> {
    scan(&state).await.map(Json)
}

/// `params`：对象里的字符串值（别的类型的值丢掉，按 JS 口径不让整条请求 400）
fn params_of(body: &Value) -> BTreeMap<String, String> {
    match field(body, "params") {
        Some(Value::Object(m)) => m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect(),
        _ => BTreeMap::new(),
    }
}

async fn render(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<PluginView>> {
    let input = PluginRenderInput { project_id: trimmed_non_empty(&body.0, "projectId"), params: params_of(&body.0) };
    Ok(Json(state.plugins.render(&state.db, &state.engine, &id, input).await?))
}

async fn action(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<PluginView>> {
    let Some(action) = trimmed_non_empty(&body.0, "action") else {
        return Err(ApiError::bad_request("缺少 action"));
    };
    let input = PluginActionInput {
        project_id: trimmed_non_empty(&body.0, "projectId"),
        params: params_of(&body.0),
        action,
        value: body.str("value").map(str::to_string),
    };
    Ok(Json(state.plugins.action(&state.db, &state.engine, &id, input).await?))
}

async fn enabled(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<PluginList>> {
    let Some(Value::Bool(on)) = field(&body.0, "enabled") else {
        return Err(ApiError::bad_request("enabled 要是 true / false"));
    };
    let list = scan(&state).await?;
    if !list.plugins.iter().any(|p| p.id == id) {
        return Err(ApiError::not_found(format!("没有这个插件：{id}")));
    }
    set_enabled(&state.db, &id, *on);
    scan(&state).await.map(Json)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use super::super::test_support::{TestApp, json_of};

    #[tokio::test]
    async fn builtin_sysmon_is_listed_and_toggles() {
        let app = TestApp::new();
        let (status, body) = json_of(app.get("/api/plugins").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["dir"], json!(app.data_dir.join("plugins").to_string_lossy()));
        assert!(app.data_dir.join("plugins").is_dir(), "列表时顺手建出插件目录");
        assert_eq!(body["plugins"][0]["id"], "sysmon");
        assert_eq!(body["plugins"][0]["enabled"], true);

        let (status, body) = json_of(app.put_json("/api/plugins/sysmon/enabled", json!({ "enabled": false })).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["plugins"][0]["enabled"], false);
        let (status, body) = json_of(app.post_json("/api/plugins/sysmon/render", json!({})).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("已停用"));

        assert_eq!(app.put_json("/api/plugins/sysmon/enabled", json!({ "enabled": "yes" })).await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app.put_json("/api/plugins/nope/enabled", json!({ "enabled": true })).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sysmon_renders_this_machine() {
        let app = TestApp::new();
        let (status, body) = json_of(app.post_json("/api/plugins/sysmon/render", json!({ "params": { "machine": "local" } })).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["title"].as_str().unwrap().starts_with("本机"));
        let blocks = body["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "select");
        assert_eq!(blocks[0]["value"], "local");
        assert_eq!(blocks[1]["type"], "stats");
        assert!(blocks.iter().any(|b| b["type"] == "chart"));
        // 系统监控没有按钮
        let res = app.post_json("/api/plugins/sysmon/action", json!({ "action": "x" })).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tokens_plugin_renders_local_coding_agents() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        let claude_dir = app.root.join(".claude/projects/demo");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("s1.jsonl"),
            format!(
                concat!(
                    r#"{{"type":"ai-title","aiTitle":"重构插件模块"}}"#,
                    "\n",
                    r#"{{"type":"assistant","cwd":"{}","timestamp":"2026-10-23T08:00:00Z","message":{{"id":"m1","model":"claude-opus-5-5","usage":{{"input_tokens":500,"cache_read_input_tokens":2000,"cache_creation_input_tokens":300,"output_tokens":400}}}}}}"#,
                    "\n"
                ),
                wd.display()
            ),
        )
        .unwrap();

        let (status, v) = json_of(
            app.post_json("/api/plugins/tokens/render", json!({ "projectId": "p1", "params": { "range": "all" } }))
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert!(v["title"].as_str().unwrap().contains("1 段会话"));
        let blocks = v["blocks"].as_array().unwrap();
        assert!(blocks.iter().any(|b| b["type"] == "stats"));
        assert!(blocks.iter().any(|b| b["type"] == "chart"));
        assert!(blocks.iter().any(|b| b["type"] == "table"));
        assert!(blocks.iter().any(|b| b["type"] == "list"));

        // 进入单段会话详情
        let (status, detail) = json_of(
            app.post_json("/api/plugins/tokens/render", json!({ "params": { "session": "claude:s1" } }))
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(detail["title"].as_str().unwrap().contains("3.20K"));
    }

    #[tokio::test]
    async fn unknown_plugins_and_bad_input() {
        let app = TestApp::new();
        assert_eq!(app.post_json("/api/plugins/nope/render", json!({})).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(app.post_json("/api/plugins/..%2Fx/render", json!({})).await.status(), StatusCode::NOT_FOUND);
        let res = app.post_json("/api/plugins/sysmon/render", json!({ "params": { "Bad": "x" } })).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app.post_json("/api/plugins/sysmon/action", json!({})).await.status(), StatusCode::BAD_REQUEST);
    }

    #[cfg(unix)]
    mod command {
        use std::os::unix::fs::PermissionsExt as _;
        use std::path::Path;

        use super::*;

        fn install(app: &TestApp, id: &str, manifest: serde_json::Value, script: &str) {
            let dir = app.data_dir.join("plugins").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
            let run = dir.join("run.sh");
            std::fs::write(&run, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[tokio::test]
        async fn json_plugin_sees_params_project_and_actions() {
            let app = TestApp::new();
            let wd = app.local_project("p1");
            // 环境变量与 stdin 两条路都验：stdin 的 JSON 原样塞进一块等宽文字
            install(
                &app,
                "hello",
                json!({ "name": "你好", "command": ["./run.sh"], "scope": "project", "refresh": 5 }),
                r#"req=$(cat)
printf '{"title":"%s|%s|%s","blocks":[{"type":"text","text":"%s","mono":true},{"type":"stats","items":[{"label":"n","value":42}]}],"toast":%s}' \
  "$FALCON_PLUGIN_EVENT" "$FALCON_PARAM_MODE" "$FALCON_PROJECT_DIR" "$(printf '%s' "$req" | sed 's/"/\\"/g')" \
  "$( [ "$FALCON_PLUGIN_EVENT" = action ] && printf '{"tone":"success","title":"%s=%s"}' "$FALCON_PLUGIN_ACTION" "$FALCON_PLUGIN_VALUE" || printf null)""#,
            );
            let (_, list) = json_of(app.get("/api/plugins").await).await;
            let hello = list["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "hello").unwrap();
            assert_eq!((hello["name"].as_str(), hello["scope"].as_str(), hello["refresh"].as_f64()), (Some("你好"), Some("project"), Some(5.0)));

            let (status, v) =
                json_of(app.post_json("/api/plugins/hello/render", json!({ "projectId": "p1", "params": { "mode": "fast" } })).await).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["title"], json!(format!("render|fast|{}", wd.display())));
            let req: serde_json::Value = serde_json::from_str(v["blocks"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(req["event"], "render");
            assert_eq!(req["project"]["id"], "p1");
            assert_eq!(req["params"]["mode"], "fast");
            assert_eq!(v["blocks"][1]["items"][0]["value"], "42", "数字收成字符串");
            assert!(v.get("toast").is_none());
            assert!(app.data_dir.join("plugin-data/hello").is_dir());

            let (status, v) =
                json_of(app.post_json("/api/plugins/hello/action", json!({ "action": "go", "value": "v1" })).await).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["toast"]["title"], "go=v1");
            assert!(Path::new(&app.data_dir).join("plugins/hello/run.sh").exists());
        }

        #[tokio::test]
        async fn failures_are_502_with_the_reason() {
            let app = TestApp::new();
            install(&app, "boom", json!({ "command": "./run.sh" }), "echo 'something broke' >&2; exit 4");
            let (status, v) = json_of(app.post_json("/api/plugins/boom/render", json!({})).await).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(v["error"], "插件退出码 4：\nsomething broke");

            install(&app, "junk", json!({ "command": "./run.sh" }), "echo not json");
            let (status, v) = json_of(app.post_json("/api/plugins/junk/render", json!({})).await).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert!(v["error"].as_str().unwrap().contains("不是合法的 JSON"));

            install(&app, "text", json!({ "command": "./run.sh", "output": "text" }), "echo '  up 3 days'");
            let (status, v) = json_of(app.post_json("/api/plugins/text/render", json!({})).await).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["blocks"], json!([{ "type": "text", "text": "  up 3 days", "mono": true }]));
            assert_eq!(app.post_json("/api/plugins/text/action", json!({ "action": "x" })).await.status(), StatusCode::BAD_REQUEST);

            install(&app, "broken", json!({ "name": "坏的" }), "true");
            let (status, v) = json_of(app.post_json("/api/plugins/broken/render", json!({})).await).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(v["error"].as_str().unwrap().contains("缺少 command"));
        }
    }

    /// 内置的 Docker 插件对着一个假的 `docker`（放在插件进程 PATH 的最前面）：不碰这台机器上真的守护进程
    #[cfg(unix)]
    mod docker {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;

        const WEB: &str = "b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62";
        const DB: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        /// 假的 docker：按子命令回实测形状的输出，参数记进 docker.log；`down` 文件在时假装守护进程没开
        fn fake_docker(app: &TestApp) {
            let inspect = format!(
                concat!(
                    r#"{{"Id":"{web}","Name":"/web","Created":"2026-10-16T14:55:43Z","RestartCount":0,"State":{{"Status":"running","ExitCode":0,"StartedAt":"2026-10-16T14:55:43Z","FinishedAt":"0001-01-01T00:00:00Z"}},"Config":{{"Image":"nginx:1.27","Labels":{{"com.docker.compose.project":"shop"}},"Cmd":["nginx"]}},"HostConfig":{{"RestartPolicy":{{"Name":"no"}}}},"NetworkSettings":{{"Ports":{{"80/tcp":[{{"HostIp":"0.0.0.0","HostPort":"8080"}}]}}}},"Mounts":[]}}"#,
                    "\n",
                    r#"{{"Id":"{db}","Name":"/db","Created":"2026-10-16T14:55:43Z","RestartCount":0,"State":{{"Status":"exited","ExitCode":1,"StartedAt":"2026-10-16T14:55:43Z","FinishedAt":"2026-10-17T14:55:43Z"}},"Config":{{"Image":"postgres:16","Labels":{{"com.docker.compose.project":"shop"}}}},"HostConfig":{{}},"NetworkSettings":{{}},"Mounts":null}}"#
                ),
                web = WEB,
                db = DB
            );
            std::fs::write(app.bin.join("inspect.jsonl"), inspect).unwrap();
            let script = format!(
                r#"#!/bin/sh
here=$(dirname "$0")
echo "$*" >> "$here/docker.log"
if [ -e "$here/down" ]; then echo "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?" >&2; exit 1; fi
case "$1 $2" in
  "ps -a") printf '%s
%s
' {WEB} {DB} ;;
  "container inspect")
    # container inspect --format <模板> <id>…：只问一个就只回那一个
    if [ $# -gt 5 ]; then cat "$here/inspect.jsonl"; else
      case "$5" in
        {WEB}) head -n 1 "$here/inspect.jsonl" ;;
        {DB}) tail -n 1 "$here/inspect.jsonl" ;;
        *) echo "Error: No such container: $5" >&2; exit 1 ;;
      esac
    fi ;;
  "info --format") echo '{{"ServerVersion":"29.8.2","OperatingSystem":"FakeOS","NCPU":4,"MemTotal":8589934592}}' ;;
  "stats --no-stream") echo '{{"ID":"{WEB}","CPUPerc":"12.00%","MemUsage":"10MiB / 8GiB","MemPerc":"0.12%","NetIO":"1kB / 2kB","BlockIO":"0B / 0B","PIDs":"3"}}' ;;
  "logs --tail")
    echo "2026-10-16T14:55:44Z second"
    echo "2026-10-16T14:55:43.5Z first" >&2 ;;
  "container stop"|"container rm") echo ok ;;
  "container prune") echo "Total reclaimed space: 12MB" ;;
  "image ls") echo '{{"ID":"5aabe105d210","Repository":"nginx","Tag":"1.27","Size":"190MB","CreatedAt":"2026-10-20 16:13:46 +0800 CST","Containers":"1"}}' ;;
  "system df") echo '{{"Type":"Images","TotalCount":"1","Active":"1","Size":"190MB","Reclaimable":"0B (0%)"}}' ;;
  "image rm") echo "Error response from daemon: conflict: unable to remove repository reference "nginx:1.27" (must force) - container {WEB} is using its referenced image" >&2; exit 1 ;;
  *) echo "fake docker: $*" >&2; exit 2 ;;
esac
"#
            );
            let exe = app.bin.join("docker");
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn log(app: &TestApp) -> String {
            std::fs::read_to_string(app.bin.join("docker.log")).unwrap_or_default()
        }

        async fn render(app: &TestApp, params: serde_json::Value) -> serde_json::Value {
            let (status, v) = json_of(app.post_json("/api/plugins/docker/render", json!({ "params": params })).await).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            v
        }

        fn find<'a>(v: &'a serde_json::Value, ty: &str) -> Vec<&'a serde_json::Value> {
            v["blocks"].as_array().unwrap().iter().filter(|b| b["type"] == ty).collect()
        }

        #[tokio::test]
        async fn lists_containers_and_samples_usage_on_the_side() {
            let app = TestApp::new();
            fake_docker(&app);
            let v = render(&app, json!({ "machine": "local" })).await;
            assert_eq!(v["title"], "本机 · FakeOS · Docker 29.8.2");
            let selects = find(&v, "select");
            assert_eq!((selects[0]["param"].as_str(), selects[1]["param"].as_str()), (Some("machine"), Some("tab")));
            let rows = &find(&v, "list")[0]["items"];
            assert_eq!(rows[0]["title"], "web");
            assert_eq!(rows[0]["action"], json!({ "kind": "setParam", "param": "container", "value": WEB }));
            assert_eq!(rows[1]["tone"], "danger", "异常退出");
            assert_eq!(find(&v, "heading")[0]["text"], "shop");
            // 占用在旁边采：第一次多半还是"…"，过一会儿再画就有了
            let mut cpu = String::new();
            for _ in 0..40 {
                let v = render(&app, json!({ "machine": "local" })).await;
                cpu = find(&v, "stats")[0]["items"][2]["value"].as_str().unwrap().to_string();
                if cpu != "…" {
                    assert_eq!(find(&v, "list")[0]["items"][0]["badge"], "12%");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert_eq!(cpu, "3.0%", "12% 按单核计，四核折成整机");
            assert!(log(&app).contains(&format!("container inspect --format {}", r#"{"Id":{{json .Id}}"#)));
        }

        #[tokio::test]
        async fn detail_merges_logs_and_remove_goes_back_to_the_list() {
            let app = TestApp::new();
            fake_docker(&app);
            // 先看一眼列表（记下 id → 名字，toast 里叫得出名字）
            render(&app, json!({ "machine": "local" })).await;
            let v = render(&app, json!({ "machine": "local", "container": WEB })).await;
            assert_eq!(find(&v, "heading")[0]["text"], "web");
            let logs = find(&v, "text").into_iter().find(|b| b["mono"] == true).unwrap();
            assert_eq!(logs["text"], "first\nsecond", "stdout 与 stderr 按时间戳合回原来的先后");

            // 不合法的 id 在服务端就挡掉，不会成为 docker 的参数
            let res = app.post_json("/api/plugins/docker/action", json!({ "action": "container.stop", "value": "--all" })).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            assert_eq!(app.post_json("/api/plugins/docker/action", json!({ "action": "system.prune" })).await.status(), StatusCode::BAD_REQUEST);

            let (status, v) = json_of(
                app.post_json(
                    "/api/plugins/docker/action",
                    json!({ "action": "container.stop", "value": WEB, "params": { "machine": "local", "container": WEB } }),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["toast"], json!({ "tone": "success", "title": "已停止 web" }));
            assert!(v.get("params").is_none(), "还在详情里");
            assert!(log(&app).contains(&format!("container stop -t 10 {WEB}")));

            let (_, v) = json_of(
                app.post_json(
                    "/api/plugins/docker/action",
                    json!({ "action": "container.remove", "value": DB, "params": { "machine": "local", "container": DB } }),
                )
                .await,
            )
            .await;
            assert_eq!(v["toast"]["title"], "已删除容器 db");
            assert_eq!(v["params"], json!({ "container": "" }), "删掉了详情里的容器：回到列表");
            assert!(find(&v, "list").len() == 1);

            // 详情里的容器已经不在了（别处删的）：回到列表
            let v = render(&app, json!({ "machine": "local", "container": "feedfacefeedface" })).await;
            assert_eq!(v["params"], json!({ "container": "" }));
            assert_eq!(find(&v, "list")[0]["items"].as_array().unwrap().len(), 2);

            let (_, v) = json_of(app.post_json("/api/plugins/docker/action", json!({ "action": "containers.prune" })).await).await;
            assert_eq!(v["toast"], json!({ "tone": "success", "title": "已清理停止的容器", "body": "腾出 12MB" }));
        }

        #[tokio::test]
        async fn images_tab_errors_and_daemon_down() {
            let app = TestApp::new();
            fake_docker(&app);
            let v = render(&app, json!({ "machine": "local", "tab": "images" })).await;
            assert_eq!(find(&v, "list")[0]["items"][0]["title"], "nginx:1.27");
            assert_eq!(find(&v, "stats")[0]["items"][0]["value"], "190MB");

            // docker 报的错原样进 toast
            let (_, v) = json_of(
                app.post_json("/api/plugins/docker/action", json!({ "action": "image.remove", "value": "nginx:1.27", "params": { "tab": "images" } }))
                    .await,
            )
            .await;
            assert_eq!(v["toast"]["tone"], "danger");
            assert_eq!(v["toast"]["title"], "删除镜像 nginx:1.27 失败");
            assert!(v["toast"]["body"].as_str().unwrap().contains("is using its referenced image"));

            std::fs::write(app.bin.join("down"), "").unwrap();
            let v = render(&app, json!({ "machine": "local" })).await;
            let notice = &find(&v, "notice")[0];
            assert_eq!(notice["tone"], "warning");
            assert!(notice["text"].as_str().unwrap().contains("没在运行"), "{notice}");
            assert_eq!(v["refresh"], 10.0);
        }
    }
}
