//! 下载 / 上传（ADR 0008）：流对流，不经内存攒整个文件，没有预览那个 16MB 上限。
//!
//! 下载先写同目录的临时文件、收满才改名到位；上传带准确的 `Content-Length`，服务端
//! 收满这个数才把它那边的临时文件改名到位。两头都不会留下悄悄截断的文件。

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

use super::{Query, seg};
use crate::ProgressFn;
use crate::client::FalconClient;

/// 进度回调的节流间隔。GPUI 那边每次回调多半要 notify 一次视图，逐块回调在快链路上
/// 就是每秒上千次重绘。
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

pub(crate) struct Ticker {
    f: Arc<ProgressFn>,
    total: Option<u64>,
    last: Option<Instant>,
}

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

pub(crate) use native::file_body;

mod native;

impl FalconClient {
    /// React 版的 `downloadUrl`：`/api/projects/:id/download?path=` 这个路径（不含基址）。
    pub fn download_path(&self, project_id: &str, path: &str) -> String {
        let q = Query::new().push("path", path).finish();
        format!("/api/projects/{}/download{q}", seg(project_id))
    }
}
