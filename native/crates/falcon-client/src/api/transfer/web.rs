//! 浏览器里的落盘 / 读本机文件：没有本机路径可言，两条都直接报错。
//!
//! 签名与原生（native.rs）一致，界面层不用按 target 分支：下载由平台层判断成"交给浏览器"
//! （`<a download>`），根本走不到这里；上传要等浏览器版的 `<input type=file>` + XHR
//! （docs/design/rust-unification.md 附录 B，C3），在那之前选不出文件，也走不到这里。

use std::future::Future;
use std::path::PathBuf;

use falcon_proto::UploadResult;

use crate::client::FalconClient;
use crate::error::{ApiError, ApiResult};
use crate::runtime::MaybeSend;

const NO_LOCAL_FILES: &str = "浏览器里没有本机文件";

impl FalconClient {
    pub fn download_to_file(
        &self,
        _project_id: &str,
        _path: &str,
        _dest: PathBuf,
        _progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
    ) -> impl Future<Output = ApiResult<u64>> + MaybeSend + 'static {
        std::future::ready(Err(ApiError::internal(NO_LOCAL_FILES)))
    }

    pub fn upload_file(
        &self,
        _project_id: &str,
        _dir: &str,
        _name: &str,
        _src: PathBuf,
        _overwrite: bool,
        _progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
    ) -> impl Future<Output = ApiResult<UploadResult>> + MaybeSend + 'static {
        std::future::ready(Err(ApiError::internal(NO_LOCAL_FILES)))
    }
}
