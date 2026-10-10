//! 文件面板（ADR 0009）、文件查看与原始字节路由（ADR 0007）。
//!
//! 这里的 `path` 一律是**工作目录相对**、`/` 分隔的路径——Windows 远端也一样。
//! 拼宿主机绝对路径另走 falcon-core 移植的 `lib/filePath.ts`，不用 `std::path`。

use std::future::Future;

use bytes::Bytes;
use falcon_proto::{FileOpResult, FileRemoveResult, WorkspaceFile, WorkspaceIndex, WorkspaceListing};
use reqwest::Method;
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;

use super::{COMPONENT, Query, seg};
use crate::client::{FalconClient, Req, ReqBody, json_body};
use crate::error::{ApiError, ApiResult};
use crate::runtime::MaybeSend;

/// [`FalconClient::raw_bytes`] 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RawBytes {
    /// 服务端按文件名猜的类型（`mimeOf`）
    pub content_type: Option<String>,
    pub bytes: Bytes,
}

/// 原始字节地址：`raw_base`（`GET …/file` 响应里的 `rawBase`，形如
/// `/api/projects/<id>/raw/<token>/`）后面接工作目录相对路径。移植自 web 的
/// `lib/rawUrl.ts`。
///
/// 逐段 `encodeURIComponent` 而不是整串：`/` 是路径分隔符得留着，而 `#` / `?` / `%`
/// 与空格出现在文件名里时必须编码，否则服务端拿到的是被截断或解错的路径。
pub fn raw_url(raw_base: &str, path: &str) -> String {
    let encoded: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| percent_encoding::utf8_percent_encode(s, COMPONENT).to_string())
        .collect();
    format!("{raw_base}{}", encoded.join("/"))
}

impl FalconClient {
    /// `GET /api/projects/:id/files`：工作目录里的一层，`path` 缺省为工作目录本身。
    /// 与 git 无关——未跟踪、被 ignore 的文件同样在里面。读失败是 400。
    pub fn list_files(
        &self,
        project_id: &str,
        path: Option<&str>,
    ) -> impl Future<Output = ApiResult<WorkspaceListing>> + MaybeSend + 'static {
        let q = Query::new().nonempty("path", path).finish();
        self.get(format!("/api/projects/{}/files{q}", seg(project_id)))
    }

    /// `GET /api/projects/:id/files/index`：Quick Open 的文件路径清单（git 仓库走 ls-files）。
    pub fn index_files(&self, project_id: &str) -> impl Future<Output = ApiResult<WorkspaceIndex>> + MaybeSend + 'static {
        self.get(format!("/api/projects/{}/files/index", seg(project_id)))
    }

    /// `GET /api/projects/:id/file?path=`：读一个文件供查看。二进制与超大文件也是 200，
    /// 形状里写清是什么。响应里的 `rawBase` 配 [`raw_url`] 就是图片 / HTML 预览的地址。
    pub fn read_file(&self, project_id: &str, path: &str) -> impl Future<Output = ApiResult<WorkspaceFile>> + MaybeSend + 'static {
        let q = Query::new().push("path", path).finish();
        self.get(format!("/api/projects/{}/file{q}", seg(project_id)))
    }

    /// `POST /api/projects/:id/mkdir`。`recursive` 给文件夹上传用：中间层已存在算成功；
    /// 已有同名文件仍是 409。
    pub fn mkdir(
        &self,
        project_id: &str,
        path: &str,
        recursive: bool,
    ) -> impl Future<Output = ApiResult<FileOpResult>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a str,
            recursive: bool,
        }
        self.json(
            Method::POST,
            format!("/api/projects/{}/mkdir", seg(project_id)),
            json_body(&Body { path, recursive }),
        )
    }

    /// `POST /api/projects/:id/rename`：只改最后一段名字，不挪目录。同名已存在是 409。
    pub fn rename_file(
        &self,
        project_id: &str,
        path: &str,
        name: &str,
    ) -> impl Future<Output = ApiResult<FileOpResult>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a str,
            name: &'a str,
        }
        self.json(
            Method::POST,
            format!("/api/projects/{}/rename", seg(project_id)),
            json_body(&Body { path, name }),
        )
    }

    /// `POST /api/projects/:id/remove`：每条路径单独试，部分失败仍是 200，细节在
    /// `errors` 里。文件夹递归删；工作目录本身删不掉。
    pub fn remove_files(
        &self,
        project_id: &str,
        paths: &[String],
    ) -> impl Future<Output = ApiResult<FileRemoveResult>> + MaybeSend + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            paths: &'a [String],
        }
        self.json(
            Method::POST,
            format!("/api/projects/{}/remove", seg(project_id)),
            json_body(&Body { paths }),
        )
    }

    /// 取原始字节（图片预览用）。`url_path` 由服务端给的 `rawBase` 拼出来（见
    /// [`raw_url`]），已经带着作用域令牌；这里另外带上登录 cookie——令牌过期了
    /// cookie 照样认（服务端 `isAuthenticated || rawTokenValid`）。
    ///
    /// 也接受以本客户端基址开头的完整 URL；别的主机一律拒绝，免得把登录 cookie
    /// 送给不该拿到的地方。超过 16MB 是 413（原始字节路由的上限，ADR 0007）。
    pub fn raw_bytes(&self, url_path: &str) -> impl Future<Output = ApiResult<RawBytes>> + MaybeSend + 'static {
        let path = if url_path.starts_with('/') {
            Ok(url_path.to_owned())
        } else if let Some(rest) = url_path.strip_prefix(self.base_url()) {
            Ok(if rest.starts_with('/') { rest.to_owned() } else { format!("/{rest}") })
        } else {
            Err(ApiError::internal(format!("不是这台服务端的地址：{url_path}")))
        };
        self.call(move |inner| async move {
            let req = Req::new(Method::GET, path?, ReqBody::Empty);
            let resp = inner.execute(&req).await?;
            let status = resp.status().as_u16();
            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let bytes = resp.bytes().await.map_err(|e| ApiError::network(&e))?;
            if !(200..300).contains(&status) {
                return Err(ApiError::http(status, &bytes));
            }
            Ok(RawBytes { content_type, bytes })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::raw_url;

    #[test]
    fn raw_url_encodes_each_segment() {
        let base = "/api/projects/p1/raw/tok/";
        assert_eq!(raw_url(base, "docs/a b#1?.png"), "/api/projects/p1/raw/tok/docs/a%20b%231%3F.png");
        assert_eq!(raw_url(base, "/x//y/"), "/api/projects/p1/raw/tok/x/y");
        assert_eq!(raw_url(base, "图/100%.svg"), "/api/projects/p1/raw/tok/%E5%9B%BE/100%25.svg");
    }
}
