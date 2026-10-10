//! 路由测试的脚手架：临时数据目录 + 真的会话引擎 + 整个 axum 应用，请求经
//! `tower::ServiceExt::oneshot` 直接喂进去，不开端口、不碰用户的数据目录。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use serde_json::Value;
use tower::ServiceExt as _;

use super::AppState;
use crate::askpass::hub::AskpassHub;
use crate::config::ServerConfig;
use crate::crypto::SecretBox;
use crate::db::{Db, ProjectRow};
use crate::engine::{self, EngineDeps};

pub struct TestApp {
    pub state: AppState,
    pub router: Router,
    pub data_dir: PathBuf,
    /// 测试用的工作目录根（项目都建在这下面）
    pub root: PathBuf,
    _dir: tempfile::TempDir,
}

impl TestApp {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("临时目录");
        // canonicalize：macOS 的 /var 是 /private/var 的软链，返回的路径要和服务端拼出来的对得上
        let base = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let data_dir = base.join("data");
        let root = base.join("ws");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let db = Arc::new(Db::open(&data_dir).expect("开库"));
        let secrets = Arc::new(SecretBox::open(&data_dir).expect("密钥"));
        let askpass = Arc::new(AskpassHub::new());
        let (engine, _thread) = engine::spawn(
            EngineDeps {
                db: db.clone(),
                secrets: secrets.clone(),
                data_dir: data_dir.clone(),
                askpass: askpass.clone(),
            },
            |_| {},
        )
        .expect("起引擎");
        let config = ServerConfig { host: "127.0.0.1".into(), port: 0, data_dir: data_dir.clone() };
        let state = AppState::new(config, db, secrets, askpass, engine);
        let router = super::app(state.clone());
        TestApp { state, router, data_dir, root, _dir: dir }
    }

    /// 建一个本地项目，工作目录是 `root/<name>`
    pub fn local_project(&self, id: &str) -> PathBuf {
        let wd = self.root.join(id);
        std::fs::create_dir_all(&wd).unwrap();
        self.state.db.insert_project(&ProjectRow {
            id: id.into(),
            name: id.into(),
            project_type: "local".into(),
            working_dir: Some(wd.to_string_lossy().into_owned()),
            created_at: 1,
            ..Default::default()
        });
        wd
    }

    pub async fn send(&self, req: Request<Body>) -> Response<Body> {
        self.router.clone().oneshot(req).await.expect("路由不会失败")
    }

    pub async fn get(&self, uri: &str) -> Response<Body> {
        self.send(Request::get(uri).body(Body::empty()).unwrap()).await
    }

    pub async fn post_json(&self, uri: &str, body: Value) -> Response<Body> {
        self.send(
            Request::post(uri).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap(),
        )
        .await
    }
}

pub async fn body_bytes(res: Response<Body>) -> Vec<u8> {
    axum::body::to_bytes(res.into_body(), usize::MAX).await.expect("读响应体").to_vec()
}

pub async fn json_of(res: Response<Body>) -> (StatusCode, Value) {
    let status = res.status();
    let bytes = body_bytes(res).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

pub fn header<'a>(res: &'a Response<Body>, name: &str) -> &'a str {
    res.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// 测试里写文件的小工具
pub fn write(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}
