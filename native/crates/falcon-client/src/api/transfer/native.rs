//! 原生的下载落盘与流式上传（读写本机文件）。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use falcon_proto::UploadResult;
use futures::StreamExt;
use reqwest::Method;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::{Query, Ticker, seg};
use crate::ProgressFn;
use crate::client::{FalconClient, Req, ReqBody};
use crate::error::{ApiError, ApiResult};
use crate::runtime::MaybeSend;

/// 读上传文件的块大小。
const UPLOAD_CHUNK: usize = 64 * 1024;

/// 上传的请求体：按块读文件，顺手报进度（报的是"已交给 HTTP 层"的字节数）。
pub(crate) fn file_body(file: tokio::fs::File, len: u64, progress: Option<Arc<ProgressFn>>) -> reqwest::Body {
    let mut ticker = progress.map(|f| Ticker::new(f, Some(len)));
    if let Some(t) = &mut ticker {
        if len == 0 { t.finish(0) } else { t.tick(0) }
    }
    let mut sent = 0u64;
    let stream = ReaderStream::with_capacity(file, UPLOAD_CHUNK).map(move |chunk| {
        if let (Ok(b), Some(t)) = (&chunk, &mut ticker) {
            sent += b.len() as u64;
            if sent >= len { t.finish(sent) } else { t.tick(sent) }
        }
        chunk
    });
    reqwest::Body::wrap_stream(stream)
}

/// 下载中的临时文件：没走到改名那一步就删掉（出错、被取消都算）。
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// `dest` 同目录下的临时文件名：同目录才能原子改名（跨卷的 rename 会失败）；点开头
/// 在 Finder 里默认看不见，下到一半不会被用户当成下好的文件双击打开。
fn temp_path_for(dest: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = format!(
        ".{name}.{}-{}.falcon-download",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(tmp),
        _ => PathBuf::from(tmp),
    }
}

impl FalconClient {
    /// 把工作目录里的一个文件流式下载到本机的 `dest`，返回字节数。
    ///
    /// 先写 `dest` 同目录下的隐藏临时文件，收满（字节数与 `Content-Length` 对上）并
    /// fsync 之后才改名成 `dest`；出错或 future 被丢掉（= 取消）都会删掉临时文件。
    /// `dest` 已存在会被覆盖——系统存储对话框已经问过用户了。
    ///
    /// 下载没有超时：几百 MB 的文件在慢链路上要传很久，任何固定超时都会误伤。
    pub fn download_to_file(
        &self,
        project_id: &str,
        path: &str,
        dest: PathBuf,
        progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
    ) -> impl Future<Output = ApiResult<u64>> + MaybeSend + 'static {
        let url = self.download_path(project_id, path);
        let progress: Arc<ProgressFn> = Arc::new(progress);
        self.call(move |inner| async move {
            let req = Req::new(Method::GET, url, ReqBody::Empty);
            let resp = inner.execute(&req).await?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let body = resp.bytes().await.map_err(|e| ApiError::network(&e))?;
                return Err(ApiError::http(status, &body));
            }
            let total = resp.content_length();
            let mut ticker = Ticker::new(progress, total);
            ticker.tick(0);

            let mut tmp = TempFile { path: temp_path_for(&dest), armed: true };
            let file = tokio::fs::File::create(&tmp.path)
                .await
                .map_err(|e| ApiError::io("创建下载临时文件失败", &e))?;
            let mut out = tokio::io::BufWriter::with_capacity(256 * 1024, file);
            let mut stream = resp.bytes_stream();
            let mut done = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| ApiError::network(&e))?;
                out.write_all(&chunk).await.map_err(|e| ApiError::io("写入下载文件失败", &e))?;
                done += chunk.len() as u64;
                ticker.tick(done);
            }
            // 远端文件在下载过程中被改写时，实际字节数会和响应头对不上（ADR 0008
            // 已知取舍）：宁可报失败，也不留一个悄悄截断的文件
            if let Some(total) = total
                && done != total
            {
                return Err(ApiError::network_msg(format!("下载不完整：收到 {done} / {total} 字节")));
            }
            out.flush().await.map_err(|e| ApiError::io("写入下载文件失败", &e))?;
            let file = out.into_inner();
            file.sync_all().await.map_err(|e| ApiError::io("写入下载文件失败", &e))?;
            drop(file);
            tokio::fs::rename(&tmp.path, &dest)
                .await
                .map_err(|e| ApiError::io("下载文件改名失败", &e))?;
            tmp.armed = false;
            ticker.finish(done);
            Ok(done)
        })
    }

    /// 把本机文件 `src` 流式上传到工作目录里的 `dir`（工作目录相对，`""` 是根），
    /// 落盘名为 `name`。
    ///
    /// 请求体按流发，带准确的 `Content-Length`（服务端据此核对收满了没有）。同名文件
    /// 已存在且没带 `overwrite` 时是 409（`is_conflict()`）：原样交回，app 问过用户再
    /// 带 `overwrite = true` 重发——文件会从头再传一遍（第一次的请求体已被服务端读掉
    /// 丢弃）。401 自动重登后的重放同样会重新打开文件、进度从 0 重报。
    pub fn upload_file(
        &self,
        project_id: &str,
        dir: &str,
        name: &str,
        src: PathBuf,
        overwrite: bool,
        progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
    ) -> impl Future<Output = ApiResult<UploadResult>> + MaybeSend + 'static {
        let q = Query::new().push("path", dir).push("name", name).flag("overwrite", overwrite).finish();
        let url = format!("/api/projects/{}/upload{q}", seg(project_id));
        let progress: Arc<ProgressFn> = Arc::new(progress);
        self.call(move |inner| async move {
            let req = Req::new(Method::PUT, url, ReqBody::File { path: src, progress: Some(progress) });
            inner.fetch_json(&req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_sits_next_to_dest() {
        let t = temp_path_for(Path::new("/Users/x/Downloads/report.pdf"));
        assert_eq!(t.parent(), Some(Path::new("/Users/x/Downloads")));
        let name = t.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".report.pdf.") && name.ends_with(".falcon-download"), "{name}");
        // 两次不撞名
        assert_ne!(t, temp_path_for(Path::new("/Users/x/Downloads/report.pdf")));
        assert_eq!(temp_path_for(Path::new("a.txt")).parent(), Some(Path::new("")));
    }

    #[test]
    fn ticker_throttles_but_reports_start_and_end() {
        let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let c = calls.clone();
        let mut t = Ticker::new(Arc::new(move |d, total| c.lock().push((d, total))), Some(10));
        t.tick(0);
        for i in 1..10 {
            t.tick(i);
        }
        t.finish(10);
        let calls = calls.lock();
        assert_eq!(calls.first(), Some(&(0, Some(10))));
        assert_eq!(calls.last(), Some(&(10, Some(10))));
        assert_eq!(calls.len(), 2, "紧挨着的 tick 被节流掉");
    }
}
