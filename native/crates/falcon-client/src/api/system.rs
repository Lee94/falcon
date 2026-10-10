//! 系统信息、后端机器（或 SSH 远端）上的目录浏览与 shell 侦测。

use std::future::Future;

use falcon_proto::{FsListing, FsValidateResult, ShellsInfo, SystemInfo};
use reqwest::Method;
use serde::Serialize;

use super::Query;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

/// 目录浏览 / shell 侦测在哪台机器上做。
///
/// FolderPicker 列的是**宿主机**上的目录，不能换成本机的系统文件夹选择器（设计文档
/// §4.5）：连的是远处的 falcon 服务端时，"后端本机"是那台机器，不是你面前这台 Mac。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProbeTarget {
    /// falcon 后端所在的机器
    #[default]
    Backend,
    /// 一台已保存的远端主机（SSH Host），带它的 id。新建 SSH 项目的表单用
    SshHost(String),
    /// 一个 SSH 项目的远端，带项目 id。编辑已有 SSH 项目时用
    Project(String),
}

impl ProbeTarget {
    fn query(&self, q: Query) -> Query {
        match self {
            ProbeTarget::Backend => q,
            ProbeTarget::SshHost(id) => q.nonempty("hostId", Some(id)),
            ProbeTarget::Project(id) => q.nonempty("projectId", Some(id)),
        }
    }
}

impl FalconClient {
    /// `GET /api/system`：服务端平台、版本、本地会话能否持久（`null` = 还没探测过）。
    pub fn system(&self) -> impl Future<Output = ApiResult<SystemInfo>> + MaybeSend + 'static {
        self.get("/api/system".to_owned())
    }

    /// `POST /api/fs/validate`：后端本机上这个路径是不是一个能进的文件夹。
    /// 不是也是 200 + `ok: false`。
    pub fn validate_path(&self, path: &str) -> impl Future<Output = ApiResult<FsValidateResult>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a str,
        }
        self.json(Method::POST, "/api/fs/validate".to_owned(), json_body(&Body { path }))
    }

    /// `GET /api/fs/list`：列一层子目录。`dir` 缺省为家目录；**空串**是 Windows 的
    /// 盘符列表（与缺省不同，照样带上）。路径不存在、SSH 连不上是 400。
    pub fn list_dir(
        &self,
        dir: Option<&str>,
        target: &ProbeTarget,
    ) -> impl Future<Output = ApiResult<FsListing>> + MaybeSend + 'static {
        let q = target.query(Query::new().opt("path", dir)).finish();
        self.get(format!("/api/fs/list{q}"))
    }

    /// `GET /api/shells`：侦测宿主机上可用的 shell（项目表单的 shell 选择）。
    /// 探测命令失败不算错（至少有默认项），连不上远端才是 400。
    pub fn list_shells(&self, target: &ProbeTarget) -> impl Future<Output = ApiResult<ShellsInfo>> + MaybeSend + 'static {
        let q = target.query(Query::new()).finish();
        self.get(format!("/api/shells{q}"))
    }
}
