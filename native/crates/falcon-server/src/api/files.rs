//! 项目工作目录的文件路由（routes.ts 里 `fileHostFor` 往下那一段）：列目录、Quick Open 索引、
//! 查看、原始字节（ADR 0007）、下载 / 上传（ADR 0008）、mkdir / rename / remove（ADR 0009）。
//!
//! 文件操作要么碰 SshLink、要么（多仓库容器）要会话引擎里的虚拟目录缓存，所以一律经
//! `engine.call` 在引擎上跑；本机那一路的阻塞 fs 在 files.rs 里进了阻塞线程池，不占引擎。
//! 下载 / 上传只在引擎上做"判错 + 开通道"，搬字节回到 HTTP 线程上（见 transfer.rs）。
//!
//! 状态码照 Node 版**按错误消息原文**分：读失败一律 400（路径不存在、没权限、SSH 连不上，
//! 都是调用方能处理并且该如实转述的事实），原始字节与下载遇到"路径不存在或不可访问"回 404，
//! 写操作遇到"同名文件已存在"回 409。

use std::rc::Rc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use falcon_proto::{
    FileOpResult, FileRemoveResult, UploadResult, WORKSPACE_RAW_CAP, WorkspaceFile, WorkspaceIndex, WorkspaceListing,
    WorktreeFailure,
};
use serde_json::Value;

use super::auth_routes::LenientJson;
use super::body::{Media, drain_in_background, media_of, query_param, unsupported_media_type};
use super::error::{ApiError, ApiResult};
use super::{AppState, encode_uri_component};
use crate::db::ProjectRow;
use crate::engine::Engine;
use crate::files::{
    FileHost, cap_file_index, index_workspace, list_workspace, mime_of, mkdir_workspace,
    read_workspace_bytes, read_workspace_file, remove_workspace, rename_workspace,
};
use crate::git::error::worktree_failure_text;
use crate::git::repo::{RunOpts, list_repo_files};
use crate::sessions::local::local_kind;
use crate::sessions::ssh::SshLink;
use crate::transfer::{content_disposition, open_download, prepare_upload, receive_upload};
use crate::zellij::host::HostKind;

/// 要登录的那几条
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/files", get(list))
        .route("/api/projects/{id}/files/index", get(index))
        .route("/api/projects/{id}/file", get(file))
        .route("/api/projects/{id}/download", get(download))
        .route("/api/projects/{id}/upload", put(upload))
        .route("/api/projects/{id}/mkdir", post(mkdir))
        .route("/api/projects/{id}/rename", post(rename))
        .route("/api/projects/{id}/remove", post(remove))
}

/// 原始字节路由：自己验作用域令牌，不走登录 cookie（挂在 public_router 上）。
/// 路径形状 `.../raw/<token>/<path>`；`<path>` 为空时 Fastify 的 `*` 也能匹配上（回 400），
/// axum 的通配段不收空串，单独挂一条
pub fn raw_router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/raw/{token}/{*rel}", get(raw))
        .route("/api/projects/{id}/raw/{token}/", get(raw_empty))
}

fn project_not_found() -> ApiError {
    ApiError::not_found("项目不存在")
}

// ---------------- fileHostFor ----------------

/// 文件操作的执行环境与工作目录根（TS 的 `fileHostFor` 的结果）
struct Located {
    kind: HostKind,
    link: Option<Rc<SshLink>>,
    root: String,
}

impl Located {
    fn host(&self) -> FileHost<'_> {
        match &self.link {
            None => FileHost::Local { kind: self.kind },
            Some(link) => FileHost::Remote { kind: self.kind, remote: &**link },
        }
    }
}

/// 文件操作的执行环境：本地项目直接走本机文件系统，SSH 项目复用会话
/// 的链路，与 /api/fs/list 同一条路子。
///
/// SSH 项目的工作目录可以留空（表单里就是可选的），那时以远端家目录为根——
/// 和会话启动时的行为一致。多仓库容器留空则以虚拟项目目录为根（会话 cwd
/// 同款语义，见 manager.ensure_virtual_dir）：首次打开文件面板会顺手把目录建出来。
async fn file_host_for(engine: &Rc<Engine>, row: &ProjectRow) -> Result<Located, String> {
    let container = row.multi_repos.is_some() && row.source_project_id.is_none();
    let working_dir = row.working_dir.clone().filter(|d| !d.is_empty());
    if row.project_type == "local" {
        let root = match working_dir {
            Some(dir) => dir,
            None if container => engine.sessions.ensure_virtual_dir(row, false).await.map_err(|e| e.to_string())?,
            None => return Err(worktree_failure_text(WorktreeFailure::NoWorkingDir).to_string()),
        };
        return Ok(Located { kind: local_kind(), link: None, root });
    }
    let link = engine.sessions.get_link(row);
    let facts = link.host_facts().await.map_err(|e| e.to_string())?;
    let root = match working_dir {
        Some(dir) => dir,
        None if container => engine.sessions.ensure_virtual_dir(row, false).await.map_err(|e| e.to_string())?,
        None => facts.home.clone(),
    };
    Ok(Located { kind: facts.kind, link: Some(link), root })
}

// ---------------- 列目录 / 索引 / 查看 ----------------

/// 列工作目录里的一层。`path` 是工作目录相对路径，缺省为工作目录本身。
///
/// 读失败回 400 而不是 500：路径不存在、没权限、SSH 连不上，都是调用方能处理
/// 并且该向用户如实转述的事实。
async fn list(State(state): State<AppState>, Path(id): Path<String>, uri: Uri) -> ApiResult<Json<WorkspaceListing>> {
    let p = query_param(&uri, "path");
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            list_workspace(&loc.host(), &loc.root, p.as_deref()).await.map_err(|e| e.to_string())
        })
        .await?;
    res.map(Json).map_err(ApiError::bad_request)
}

/// Quick Open 的文件路径清单。git 仓库走 ls-files（尊重 gitignore），
/// 否则遍历工作目录并跳过 node_modules 之类。失败回 400，与列目录同一套。
async fn index(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<WorkspaceIndex>> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            let listed = async {
                let host = engine.git_host(&row).await?;
                list_repo_files(&host, &loc.root, &RunOpts::default()).await
            }
            .await;
            match listed {
                Ok(Some(paths)) => return Ok(cap_file_index(paths)),
                // 链路挂了就别假装能列文件
                Err(e) if e.reason == WorktreeFailure::LinkFailed => return Err(e.message),
                // git 缺失 / 不是仓库：改走遍历
                _ => {}
            }
            index_workspace(&loc.host(), &loc.root).await.map_err(|e| e.to_string())
        })
        .await?;
    res.map(Json).map_err(ApiError::bad_request)
}

/// 原始字节前缀（含新鲜令牌），图片与 HTML 预览拼它用
fn raw_base_for(state: &AppState, project_id: &str) -> String {
    format!("/api/projects/{}/raw/{}/", encode_uri_component(project_id), state.auth.raw_token(project_id))
}

/// 读一个文件供查看 tab 渲染。二进制 / 超大文件也回 200，形状里写清是什么。
/// 顺带给出这个项目的原始字节前缀（含新鲜令牌），图片与 HTML 预览拼它用。
async fn file(State(state): State<AppState>, Path(id): Path<String>, uri: Uri) -> ApiResult<Json<WorkspaceFile>> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(p) = query_param(&uri, "path").filter(|p| !p.is_empty()) else {
        return Err(ApiError::bad_request("缺少文件路径"));
    };
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            read_workspace_file(&loc.host(), &loc.root, &p).await.map_err(|e| e.to_string())
        })
        .await?;
    let preview = res.map_err(ApiError::bad_request)?;
    Ok(Json(WorkspaceFile { preview, raw_base: raw_base_for(&state, &id) }))
}

// ---------------- 原始字节（ADR 0007） ----------------

async fn raw(
    State(state): State<AppState>,
    Path((id, token, rel)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    raw_core(state, id, token, rel, headers).await.unwrap_or_else(IntoResponse::into_response)
}

async fn raw_empty(
    State(state): State<AppState>,
    Path((id, token)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    raw_core(state, id, token, String::new(), headers).await.unwrap_or_else(IntoResponse::into_response)
}

/// 原始字节：把工作目录里的一个文件按它本来的 Content-Type 原样吐给浏览器。
/// 图片预览的 `<img src>`、HTML 预览的 `<iframe src>` 以及页面里相对路径引用的
/// CSS / JS / 图片 / 字体都走这里。
///
/// 路由是**路径形状**而不是 `?path=`：HTML 里的 `./style.css` 要能按 URL 规则
/// 相对当前文档解析到 `.../raw/<token>/docs/style.css`，query 形状做不到。
///
/// 鉴权与别的 /api 不同（不经 require_login）：HTML 预览跑在没有
/// allow-same-origin 的沙箱 iframe 里，origin 是 opaque 的，浏览器不给它的子资源
/// 请求带 SameSite=Lax 的登录 cookie，所以凭据只能放在 URL 里——一枚只能读这个
/// 项目文件的作用域令牌（Auth::raw_token）。登录 cookie 若在（用户直接在新标签页
/// 打开原始地址）也认。
///
/// 响应头是另一半护栏：`Content-Security-Policy: sandbox` 让这个 HTML 即使被
/// 当成顶层页面打开也跑在 opaque origin 里，碰不到本站的 cookie / storage /
/// 其它接口；nosniff 防止把 octet-stream 猜成脚本。no-store 是因为用户改完文件
/// 按刷新就想看到新的，这里不发 ETag。
async fn raw_core(state: AppState, id: String, token: String, rel: String, headers: HeaderMap) -> ApiResult<Response> {
    if !state.authenticated(&headers) && !state.auth.raw_token_valid(Some(&token), &id) {
        return Err(ApiError::unauthorized());
    }
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    if rel.is_empty() {
        return Err(ApiError::bad_request("缺少文件路径"));
    }
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            read_workspace_bytes(&loc.host(), &loc.root, &rel, WORKSPACE_RAW_CAP).await.map_err(|e| e.to_string())
        })
        .await?;
    let (name, read) = res.map_err(not_found_or_bad_request)?;
    if read.size > read.bytes.len() as u64 {
        // 截断的字节对浏览器没有意义（半张图、半个脚本），如实拒绝
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("文件超过 {}MB", WORKSPACE_RAW_CAP / 1024 / 1024),
        ));
    }
    Ok((
        [
            ("content-type", mime_of(&name).to_string()),
            ("content-length", read.bytes.len().to_string()),
            ("cache-control", "no-store".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
            ("content-security-policy", "sandbox allow-scripts allow-forms allow-popups allow-modals".to_string()),
            ("referrer-policy", "no-referrer".to_string()),
        ],
        read.bytes,
    )
        .into_response())
}

/// 读类路由（原始字节、下载）的错误：路径不存在回 404，其余 400
fn not_found_or_bad_request(message: String) -> ApiError {
    let status =
        if message == "路径不存在或不可访问" { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
    ApiError::new(status, message)
}

/// 写类路由（上传、mkdir、rename）的错误：同名文件已存在回 409，其余 400
fn conflict_or_bad_request(message: String) -> ApiError {
    let status = if message == "同名文件已存在" { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST };
    ApiError::new(status, message)
}

// ---------------- 下载 / 上传（ADR 0008） ----------------

/// 下载工作目录里的一个文件：整个文件按流接到响应上，没有原始字节路由那个
/// 16MB 上限（ADR 0008）。
///
/// 鉴权就是普通的登录 cookie：下载由主页面发起的同源导航触发（`<a download>`），
/// 浏览器会带 cookie，用不着原始字节路由那种放在 URL 里的作用域令牌。
/// Content-Type 一律 octet-stream + attachment：这是"存到本地"，不是"在浏览器里
/// 看"，浏览器按文件名后缀自己认类型。
async fn download(State(state): State<AppState>, Path(id): Path<String>, uri: Uri) -> ApiResult<Response> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(p) = query_param(&uri, "path").filter(|p| !p.is_empty()) else {
        return Err(ApiError::bad_request("缺少文件路径"));
    };
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            open_download(&loc.host(), &loc.root, &p).await
        })
        .await?;
    let src = res.map_err(not_found_or_bad_request)?;
    Ok((
        [
            ("content-type", "application/octet-stream".to_string()),
            ("content-length", src.size.to_string()),
            ("content-disposition", content_disposition(&src.name)),
            ("cache-control", "no-store".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
        ],
        Body::from_stream(src.body.into_stream()),
    )
        .into_response())
}

/// 上传一个文件到工作目录里的 `path` 目录下，文件名是 `name`，请求体是原始字节
/// （application/octet-stream），按流写到宿主机。
///
/// Content-Length 是必需的：宿主机那头收满这个数才把文件改名到位，中途断开的
/// 上传不会留下截断的文件。浏览器给 File 请求体一定带这个头。
///
/// 同名文件已存在且没带 overwrite=1 时回 409，前端问过用户再重发；此时请求体
/// 可能还没收完，剩下的读掉丢弃（drain_in_background），连接不会被掐断。
async fn upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let mut stream = body.into_data_stream();
    let res = upload_core(state, id, uri, headers, &mut stream).await;
    // 没读完的请求体读掉丢弃（读完了的这里立刻结束）
    drain_in_background(stream);
    match res {
        Ok(out) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn upload_core(
    state: AppState,
    id: String,
    uri: Uri,
    headers: HeaderMap,
    body: &mut axum::body::BodyDataStream,
) -> ApiResult<UploadResult> {
    // Fastify 先按 Content-Type 选解析器（routes.ts 给 application/octet-stream 注册的那个
    // 原样交出流），认不出的类型在进路由之前就是 415
    let media = media_of(&headers);
    if media == Media::Unsupported {
        return Err(unsupported_media_type());
    }
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(name) = query_param(&uri, "name").filter(|n| !n.is_empty()) else {
        return Err(ApiError::bad_request("缺少文件名"));
    };
    // `Number(req.headers["content-length"])` 必须是非负整数；chunked 的请求没有这个头
    let size = headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let Some(size) = size else {
        return Err(ApiError::new(StatusCode::LENGTH_REQUIRED, "缺少 Content-Length"));
    };
    // 空文件时 Fastify 可能跳过解析器（没有 Content-Type）；别的 Content-Type 会解成
    // JSON / 字符串 / Buffer，那不是这条路由要的
    if media != Media::OctetStream && size != 0 {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "请求体必须是 application/octet-stream"));
    }
    let dir = query_param(&uri, "path");
    let overwrite = query_param(&uri, "overwrite").as_deref() == Some("1");
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            prepare_upload(&loc.host(), &loc.root, dir.as_deref(), &name, size, overwrite).await
        })
        .await?;
    let sink = res.map_err(conflict_or_bad_request)?;
    if media != Media::OctetStream {
        // 空请求体（size 0）却不是 octet-stream：Node 版换成一个空流，这里同样不读它
        let mut empty = futures::stream::empty::<Result<bytes::Bytes, std::io::Error>>();
        return receive_upload(sink, size, overwrite, &mut empty).await.map_err(conflict_or_bad_request);
    }
    receive_upload(sink, size, overwrite, body).await.map_err(conflict_or_bad_request)
}

// ---------------- mkdir / rename / remove（ADR 0009） ----------------

/// 字符串字段；空串当没给（Node 版的 `if (!p)`）。别的类型 Node 版会在 `.split` 上抛
/// TypeError 变成 400 的另一句话，这里一并当没给
fn non_empty_str(body: &LenientJson, key: &str) -> Option<String> {
    body.str(key).filter(|s| !s.is_empty()).map(str::to_string)
}

/// 在工作目录里建一个文件夹。`path` 是工作目录相对路径（含要建的那一段）。
/// recursive 给文件夹上传用：中间层已存在当成功；已存在一个同名文件仍是 409。
async fn mkdir(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<FileOpResult>> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(p) = non_empty_str(&body, "path") else { return Err(ApiError::bad_request("缺少路径")) };
    let recursive = body.0.get("recursive") == Some(&Value::Bool(true));
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            mkdir_workspace(&loc.host(), &loc.root, &p, recursive).await.map_err(|e| e.to_string())
        })
        .await?;
    res.map(Json).map_err(conflict_or_bad_request)
}

/// 重命名工作目录里的一项。只改最后一段名字，不移动到别的目录。
async fn rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<FileOpResult>> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(p) = non_empty_str(&body, "path") else { return Err(ApiError::bad_request("缺少路径")) };
    let Some(name) = non_empty_str(&body, "name") else { return Err(ApiError::bad_request("缺少文件名")) };
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            rename_workspace(&loc.host(), &loc.root, &p, &name).await.map_err(|e| e.to_string())
        })
        .await?;
    res.map(Json).map_err(conflict_or_bad_request)
}

/// 删除工作目录里的若干项。每条单独试，部分失败仍 200，细节在 errors 里。
/// 空路径（工作目录本身）会被丢掉；前端不该把它送来。
async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<FileRemoveResult>> {
    let row = state.db.get_project(&id).ok_or_else(project_not_found)?;
    let Some(Value::Array(items)) = body.0.get("paths").filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) else {
        return Err(ApiError::bad_request("缺少路径"));
    };
    let Some(paths) = items.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<String>>>() else {
        return Err(ApiError::bad_request("路径不合法"));
    };
    let res = state
        .engine
        .call(move |engine| async move {
            let loc = file_host_for(&engine, &row).await?;
            Ok::<_, String>(remove_workspace(&loc.host(), &loc.root, &paths).await)
        })
        .await?;
    res.map(Json).map_err(ApiError::bad_request)
}

#[cfg(test)]
mod tests {
    //! 本地项目上把每条路由走一遍：状态码、错误文案、响应头与 Node 版一致。
    //! 远端那一路的命令串与脚本在 files.rs / transfer.rs 的单测里（经本机 sh 实跑）。
    use super::super::test_support::{TestApp, body_bytes, header, json_of, write};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;

    #[tokio::test]
    async fn list_and_unknown_project() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        write(&wd.join("b.txt"), "bb");
        write(&wd.join("src/a.rs"), "a");
        let (status, body) = json_of(app.get("/api/projects/p1/files").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["path"], "");
        assert_eq!(body["truncated"], false);
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries[0]["name"], "src");
        assert_eq!(entries[0]["kind"], "dir");
        assert!(entries[0].get("size").is_none());
        assert_eq!(entries[1]["path"], "b.txt");
        assert_eq!(entries[1]["size"], 2);

        let (status, body) = json_of(app.get("/api/projects/p1/files?path=src").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["entries"][0]["path"], "src/a.rs");

        let (status, body) = json_of(app.get("/api/projects/p1/files?path=..%2Fx").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不合法" })));
        let (status, body) = json_of(app.get("/api/projects/p1/files?path=nope").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不存在或不可访问" })));
        let (status, body) = json_of(app.get("/api/projects/zz/files").await).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "项目不存在" })));
    }

    #[tokio::test]
    async fn local_project_without_working_dir() {
        let app = TestApp::new();
        app.state.db.insert_project(&crate::db::ProjectRow {
            id: "nowd".into(),
            name: "nowd".into(),
            project_type: "local".into(),
            ..Default::default()
        });
        let (status, body) = json_of(app.get("/api/projects/nowd/files").await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "项目没有指定工作目录，无从判断在哪个仓库里");
    }

    #[tokio::test]
    async fn index_walks_non_git_dirs_and_respects_gitignore_in_repos() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        write(&wd.join("a.txt"), "a");
        write(&wd.join("node_modules/x/index.js"), "x");
        write(&wd.join("src/b.rs"), "b");
        let (status, body) = json_of(app.get("/api/projects/p1/files/index").await).await;
        assert_eq!(status, StatusCode::OK);
        let mut paths: Vec<String> =
            body["paths"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        paths.sort();
        assert_eq!(paths, ["a.txt", "src/b.rs"]);
        assert_eq!(body["truncated"], false);

        // git 仓库走 ls-files：未跟踪但没被忽略的也在，被 .gitignore 掉的不在
        if crate::exec::local_exec("command -v git", None).await.ok() {
            let repo = app.local_project("repo");
            write(&repo.join(".gitignore"), "ignored.log\n");
            write(&repo.join("kept.rs"), "k");
            write(&repo.join("ignored.log"), "i");
            write(&repo.join("dist/out.js"), "o");
            let init = format!("git -C '{}' init -q", repo.display());
            assert!(crate::exec::local_exec(&init, None).await.ok());
            let (status, body) = json_of(app.get("/api/projects/repo/files/index").await).await;
            assert_eq!(status, StatusCode::OK);
            let mut paths: Vec<String> =
                body["paths"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            paths.sort();
            // dist/ 不在 .gitignore 里，ls-files 照列（遍历兜底才按 INDEX_SKIP_DIRS 跳）
            assert_eq!(paths, [".gitignore", "dist/out.js", "kept.rs"]);
        }
    }

    #[tokio::test]
    async fn file_preview_and_raw_bytes() {
        let app = TestApp::new();
        let wd = app.local_project("p 1");
        write(&wd.join("docs/index.html"), "<p>hi</p>");
        write(&wd.join("logo.png"), [0x89, b'P', b'N', b'G']);

        let (status, body) = json_of(app.get("/api/projects/p%201/file?path=docs/index.html").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["preview"], json!({ "kind": "text", "text": "<p>hi</p>", "size": 9, "truncated": false }));
        let raw_base = body["rawBase"].as_str().unwrap().to_string();
        assert!(raw_base.starts_with("/api/projects/p%201/raw/") && raw_base.ends_with('/'));

        let (status, body) = json_of(app.get("/api/projects/p%201/file?path=logo.png").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["preview"], json!({ "kind": "image", "mime": "image/png", "size": 4 }));
        let (status, body) = json_of(app.get("/api/projects/p%201/file").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少文件路径" })));
        let (status, body) = json_of(app.get("/api/projects/p%201/file?path=docs").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "这是一个文件夹" })));

        // 原始字节：按扩展名给 Content-Type，护栏头一个不少
        let res = app.get(&format!("{raw_base}docs/index.html")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header(&res, "content-type"), "text/html; charset=utf-8");
        assert_eq!(header(&res, "content-length"), "9");
        assert_eq!(header(&res, "cache-control"), "no-store");
        assert_eq!(header(&res, "x-content-type-options"), "nosniff");
        assert_eq!(
            header(&res, "content-security-policy"),
            "sandbox allow-scripts allow-forms allow-popups allow-modals"
        );
        assert_eq!(header(&res, "referrer-policy"), "no-referrer");
        assert_eq!(body_bytes(res).await, b"<p>hi</p>");

        let (status, body) = json_of(app.get(&format!("{raw_base}missing.css")).await).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "路径不存在或不可访问" })));
        let (status, body) = json_of(app.get(&raw_base).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少文件路径" })));
        let (status, body) = json_of(app.get(&format!("{raw_base}a/..%2F..%2Fx")).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不合法" })));

        // 超过 16MB 回 413，不给截断的字节（稀疏文件，不真占盘）
        let big = std::fs::File::create(wd.join("big.bin")).unwrap();
        big.set_len(falcon_proto::WORKSPACE_RAW_CAP + 1).unwrap();
        let (status, body) = json_of(app.get(&format!("{raw_base}big.bin")).await).await;
        assert_eq!((status, body), (StatusCode::PAYLOAD_TOO_LARGE, json!({ "error": "文件超过 16MB" })));
    }

    #[tokio::test]
    async fn raw_token_is_scoped_when_login_is_required() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        app.local_project("p2");
        write(&wd.join("a.css"), "x{}");
        assert!(app.state.auth.set_password("secret1", None));
        let login = app.state.auth.login("secret1").unwrap();

        // 没登录：普通 /api 401，原始字节没令牌也 401
        let (status, _) = json_of(app.get("/api/projects/p1/file?path=a.css").await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = json_of(app.get("/api/projects/p1/raw/bogus/a.css").await).await;
        assert_eq!((status, body), (StatusCode::UNAUTHORIZED, json!({ "error": "未认证" })));

        let req = Request::get("/api/projects/p1/file?path=a.css")
            .header("cookie", format!("falcon_token={login}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = json_of(app.send(req).await).await;
        assert_eq!(status, StatusCode::OK);
        let raw_base = body["rawBase"].as_str().unwrap().to_string();

        // 令牌（没有 cookie）能读这个项目
        let res = app.get(&format!("{raw_base}a.css")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header(&res, "content-type"), "text/css; charset=utf-8");
        // 换个项目就不认
        let token = raw_base.trim_end_matches('/').rsplit('/').next().unwrap();
        let (status, _) = json_of(app.get(&format!("/api/projects/p2/raw/{token}/a.css")).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // 登录 cookie 也认（新标签页直接打开原始地址）
        let req = Request::get("/api/projects/p1/raw/bogus/a.css")
            .header("cookie", format!("falcon_token={login}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.send(req).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn download_streams_with_attachment_headers() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        write(&wd.join("产物 \"v1\".bin"), &content);
        let res = app.get("/api/projects/p1/download?path=%E4%BA%A7%E7%89%A9%20%22v1%22.bin").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header(&res, "content-type"), "application/octet-stream");
        assert_eq!(header(&res, "content-length"), "200000");
        assert_eq!(header(&res, "cache-control"), "no-store");
        assert_eq!(header(&res, "x-content-type-options"), "nosniff");
        assert_eq!(
            header(&res, "content-disposition"),
            "attachment; filename=\"__ _v1_.bin\"; filename*=UTF-8''%E4%BA%A7%E7%89%A9%20%22v1%22.bin"
        );
        assert_eq!(body_bytes(res).await, content);

        let (status, body) = json_of(app.get("/api/projects/p1/download?path=nope.bin").await).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "路径不存在或不可访问" })));
        std::fs::create_dir(wd.join("dir")).unwrap();
        let (status, body) = json_of(app.get("/api/projects/p1/download?path=dir").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "这是一个文件夹" })));
        let (status, body) = json_of(app.get("/api/projects/p1/download").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少文件路径" })));
    }

    fn put(uri: &str, content_type: Option<&str>, body: impl Into<Body>, len: Option<usize>) -> Request<Body> {
        let mut req = Request::put(uri);
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        if let Some(len) = len {
            req = req.header("content-length", len.to_string());
        }
        req.body(body.into()).unwrap()
    }

    #[tokio::test]
    async fn upload_writes_checks_and_maps_errors() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        std::fs::create_dir(wd.join("sub")).unwrap();
        let oct = Some("application/octet-stream");
        let send = |req: Request<Body>| app.send(req);

        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?path=sub&name=a.txt", oct, "hello", Some(5))).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "path": "sub/a.txt", "size": 5 })));
        assert_eq!(std::fs::read(wd.join("sub/a.txt")).unwrap(), b"hello");

        // 同名：没带 overwrite 是 409，带了覆盖
        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?path=sub&name=a.txt", oct, "bye", Some(3))).await).await;
        assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "同名文件已存在" })));
        let uri = "/api/projects/p1/upload?path=sub&name=a.txt&overwrite=1";
        let (status, _) = json_of(send(put(uri, oct, "bye", Some(3))).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(std::fs::read(wd.join("sub/a.txt")).unwrap(), b"bye");

        // 声明的字节数与实收不符：报中断，不留临时文件、不动原文件
        let (status, body) = json_of(send(put(uri, oct, "xy", Some(10))).await).await;
        assert_eq!(
            (status, body),
            (StatusCode::BAD_REQUEST, json!({ "error": "上传中断，收到的字节数与声明的不一致" }))
        );
        assert_eq!(std::fs::read(wd.join("sub/a.txt")).unwrap(), b"bye");
        let leftovers: Vec<_> = std::fs::read_dir(wd.join("sub")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");

        // 空文件可以不带 Content-Type
        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?name=empty", None, Body::empty(), Some(0))).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "path": "empty", "size": 0 })));

        let (status, body) = json_of(send(put("/api/projects/p1/upload?path=sub", oct, "x", Some(1))).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少文件名" })));
        let (status, body) = json_of(send(put("/api/projects/p1/upload?name=a%2Fb", oct, "x", Some(1))).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "文件名不合法" })));
        let (status, body) = json_of(send(put("/api/projects/p1/upload?name=sub", oct, "x", Some(1))).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "这是一个文件夹" })));
        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?path=nope&name=x", oct, "x", Some(1))).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不存在或不可访问" })));
        // chunked（没有 Content-Length）
        let stream = futures::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"x"))]);
        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?name=c", oct, Body::from_stream(stream), None)).await).await;
        assert_eq!((status, body), (StatusCode::LENGTH_REQUIRED, json!({ "error": "缺少 Content-Length" })));
        // JSON 不是这条路由要的；Fastify 没有解析器的类型在进路由之前就 415
        let (status, body) =
            json_of(send(put("/api/projects/p1/upload?name=j", Some("application/json"), "{}", Some(2))).await).await;
        assert_eq!(
            (status, body),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, json!({ "error": "请求体必须是 application/octet-stream" }))
        );
        let (status, body) =
            json_of(send(put("/api/projects/zz/upload?name=j", Some("video/mp4"), "xx", Some(2))).await).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(body["code"], "FST_ERR_CTP_INVALID_MEDIA_TYPE");
        let (status, body) = json_of(send(put("/api/projects/zz/upload?name=j", oct, "xx", Some(2))).await).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "项目不存在" })));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mkdir_rename_remove() {
        let app = TestApp::new();
        let wd = app.local_project("p1");
        let url = |op: &str| format!("/api/projects/p1/{op}");

        let (status, body) =
            json_of(app.post_json(&url("mkdir"), json!({ "path": "a/b", "recursive": true })).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "path": "a/b" })));
        assert!(wd.join("a/b").is_dir());
        // 递归时已存在的目录算成功；非递归是 409
        let (status, _) =
            json_of(app.post_json(&url("mkdir"), json!({ "path": "a/b", "recursive": true })).await).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = json_of(app.post_json(&url("mkdir"), json!({ "path": "a/b" })).await).await;
        assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "同名文件已存在" })));
        let (status, body) = json_of(app.post_json(&url("mkdir"), json!({ "path": "x/y" })).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不存在或不可访问" })));
        let (status, body) = json_of(app.post_json(&url("mkdir"), json!({})).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少路径" })));

        write(&wd.join("a/f.txt"), "f");
        let (status, body) =
            json_of(app.post_json(&url("rename"), json!({ "path": "a/f.txt", "name": "g.txt" })).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "path": "a/g.txt" })));
        assert!(wd.join("a/g.txt").is_file());
        let (status, body) =
            json_of(app.post_json(&url("rename"), json!({ "path": "a/g.txt", "name": "b" })).await).await;
        assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "同名文件已存在" })));
        let (status, body) = json_of(app.post_json(&url("rename"), json!({ "path": "a/g.txt" })).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少文件名" })));
        let (status, body) =
            json_of(app.post_json(&url("rename"), json!({ "path": "a/g.txt", "name": "../x" })).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "文件名不合法" })));

        // 删除：子孙收进祖先、工作目录本身丢掉、不存在的单独报错，部分失败仍 200
        let outside = app.root.join("outside.txt");
        write(&outside, "keep");
        std::os::unix::fs::symlink(&outside, wd.join("link")).unwrap();
        let (status, body) =
            json_of(app.post_json(&url("remove"), json!({ "paths": ["a", "a/g.txt", "", "nope", "link"] })).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "removed": ["a", "link"], "errors": [{ "path": "nope", "error": "路径不存在或不可访问" }] })
        );
        assert!(!wd.join("a").exists() && wd.is_dir());
        // 符号链接只删链接本身
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        let (status, body) = json_of(app.post_json(&url("remove"), json!({ "paths": [] })).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "缺少路径" })));
        let (status, body) = json_of(app.post_json(&url("remove"), json!({ "paths": ["a", 1] })).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不合法" })));
        let (status, body) = json_of(app.post_json(&url("remove"), json!({ "paths": ["/"] })).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "removed": [], "errors": [] })));
        assert!(wd.is_dir());
    }
}
