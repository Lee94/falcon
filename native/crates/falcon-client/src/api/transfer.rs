//! 下载 / 上传（ADR 0008）：流对流，不经内存攒整个文件，没有预览那个 16MB 上限。
//!
//! 下载先写同目录的临时文件、收满才改名到位；上传带准确的 `Content-Length`，服务端
//! 收满这个数才把它那边的临时文件改名到位。两头都不会留下悄悄截断的文件。
//!
//! 落盘 / 读本机文件的两条（[`FalconClient::download_to_file`]、
//! [`FalconClient::upload_file`]）只在原生上真有：浏览器里没有本机路径，下载交给
//! `<a download>`、上传读 `File`（docs/design/rust-unification.md 附录 B），wasm 上这两个
//! 方法照样在（签名一致、直接报错），界面层不用按 target 分支。

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

use super::{Query, seg};
use crate::ProgressFn;
use crate::client::FalconClient;

/// 进度回调的节流间隔。GPUI 那边每次回调多半要 notify 一次视图，逐块回调在快链路上
/// 就是每秒上千次重绘。
#[cfg_attr(target_family = "wasm", allow(dead_code))]
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

// 浏览器版的上传进度（C3，XHR upload.onprogress）也走它，在那之前 wasm 上没人用
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub(crate) struct Ticker {
    f: Arc<ProgressFn>,
    total: Option<u64>,
    last: Option<Instant>,
}

#[cfg_attr(target_family = "wasm", allow(dead_code))]
impl Ticker {
    pub(crate) fn new(f: Arc<ProgressFn>, total: Option<u64>) -> Self {
        Ticker { f, total, last: None }
    }

    /// 第一次必报，之后按时间节流。
    pub(crate) fn tick(&mut self, done: u64) {
        let now = Instant::now();
        if self.last.is_none_or(|t| now.duration_since(t) >= PROGRESS_EVERY) {
            self.last = Some(now);
            (self.f)(done, self.total);
        }
    }

    /// 结尾必报。
    pub(crate) fn finish(&mut self, done: u64) {
        self.last = Some(Instant::now());
        (self.f)(done, self.total);
    }
}

#[cfg(not(target_family = "wasm"))]
pub(crate) use native::file_body;

#[cfg(not(target_family = "wasm"))]
mod native;
#[cfg(target_family = "wasm")]
mod web;

impl FalconClient {
    /// React 版的 `downloadUrl`：`/api/projects/:id/download?path=` 这个路径（不含基址）。
    /// 原生客户端自己下载走 `download_to_file`；浏览器版把它交给 `<a download>`。
    pub fn download_path(&self, project_id: &str, path: &str) -> String {
        let q = Query::new().push("path", path).finish();
        format!("/api/projects/{}/download{q}", seg(project_id))
    }
}
