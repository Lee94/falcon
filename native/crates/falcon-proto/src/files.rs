//! 项目工作目录里的文件：文件面板（ADR 0009）、查看与原始字节路由（ADR 0007）、
//! 下载 / 上传（ADR 0008）。
//!
//! 这里的 `path` 一律是**工作目录相对**、以 `/` 分隔的路径——Windows 远端也是
//! 这样，平台分隔符只在后端拼绝对路径时才出现。客户端拼宿主机绝对路径另走
//! `falcon-core` 移植的 `lib/filePath.ts`，不用 `std::path`。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// [`WorkspaceEntry::kind`]。指向目录的符号链接算 dir——用户点它期待的是进去。
    pub enum WorkspaceEntryKind {
        File => "file",
        Dir => "dir",
    }
}

/// 项目工作目录里的一项。
///
/// `path` 是工作目录相对的 `/` 路径，前端拿它当列表 key 与展开状态的键，
/// 不需要知道宿主机是什么平台。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub path: String,
    pub kind: WorkspaceEntryKind,
    /// 字节数。目录不给（前端画成 —）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 最后修改时间，Unix **秒**（不是毫秒）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

/// 工作目录下某一层的列表（`GET /api/projects/:id/files`）。目录在前、文件在后，各自按名字排
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListing {
    /// 工作目录相对路径；`""` 是工作目录本身
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
    /// 条目数超过 WORKSPACE_LIST_CAP，只给了前面一批（node_modules 这种）
    pub truncated: bool,
}

/// 一层目录最多列多少条。超过就截断——几万条目的列表画出来也没人看
pub const WORKSPACE_LIST_CAP: usize = 2000;

/// ⌘P 文件索引的上限。git ls-files 对中型仓库很快，但把整份 node_modules
/// 塞进前端没有意义——Quick Open 也只会显示前几十条匹配。
pub const WORKSPACE_INDEX_CAP: usize = 8000;

/// 工作目录下的文件路径清单，给 Quick Open 用（`GET /api/projects/:id/files/index`）。路径一律 `/` 分隔
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIndex {
    pub paths: Vec<String>,
    pub truncated: bool,
}

/// 查看单个文件（文本）的上限。base64 传输会膨胀 1/3，2MB 的文件已经是 2.7MB 的响应，
/// 再大就不是"查看"而是"下载"了。
pub const WORKSPACE_FILE_CAP: u64 = 2 * 1024 * 1024;

/// 原始字节路由（`/api/projects/:id/raw/<token>/<path>`）单个文件的上限。
///
/// 图片与 HTML 预览的子资源走这条路，浏览器直接吃字节、不经 JSON / base64，
/// 所以上限可以比文本宽得多；再往上 SSH 端 base64 一次性攒在内存里就不合适了。
pub const WORKSPACE_RAW_CAP: u64 = 16 * 1024 * 1024;

/// 查看一个文件的结果。
///
/// 类型判定在后端做（要看字节）：text 直接给解好的 UTF-8 文本；image 只报 mime
/// 与大小，字节由前端拼 rawBase + path 自己去取（见 WorkspaceFile）；
/// 剩下的一律 binary，前端只报"这是二进制文件"。
///
/// Rust 侧加了 `Unknown` 兜底：新服务端多一种预览（pdf / video）时，老客户端
/// 退回"不能预览，请下载"，而不是整个查看请求解码失败。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind")]
pub enum FilePreview {
    #[serde(rename = "text")]
    Text {
        /// UTF-8 解码后的文本；超过上限时按字节截断（末尾可能缺半行）
        text: String,
        size: u64,
        truncated: bool,
    },
    #[serde(rename = "image")]
    Image { mime: String, size: u64 },
    #[serde(rename = "binary")]
    Binary { size: u64 },
    /// 超过上限且不是文本（文本会截断显示）：图片按 WORKSPACE_RAW_CAP 算，只报大小
    #[serde(rename = "too-large")]
    TooLarge { size: u64 },
    /// 本版本不认识的预览类型（服务端比客户端新）。只在反序列化时出现。
    #[serde(rename = "unknown", other)]
    Unknown,
}

/// `GET /api/projects/:id/file` 的响应。
///
/// rawBase 是这个项目原始字节路由的前缀（形如 `/api/projects/<id>/raw/<token>/`），
/// 把工作目录相对路径按段 URL 编码接在后面就是能直接取字节的地址。前缀里带一枚
/// **只对这个项目的原始读取有效**的令牌：HTML 预览跑在 opaque origin 的沙箱里，
/// 浏览器不会给它的子资源请求带登录 cookie，只能靠 URL 自带凭据；令牌泄给页面里的
/// 脚本也拿不到别的接口。令牌有时效，每次读文件都会给一枚新鲜的，不用缓存。
///
/// 原生客户端的 HTML 预览（WebView）同样只加载 `rawBase + path`，**不注入登录
/// cookie**——ADR 0007 的安全边界原样保留（设计文档 §4.4）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFile {
    pub preview: FilePreview,
    pub raw_base: String,
}

/// `PUT /api/projects/:id/upload` 的响应：落盘后的工作目录相对路径与字节数
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub path: String,
    pub size: u64,
}

/// mkdir / rename 成功后回相对路径，前端用来刷新那一层
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileOpResult {
    pub path: String,
}

/// 批量删除（`POST /api/projects/:id/remove`）。每条路径单独试，互不挡住——
/// 20 个里坏了 1 个，另外 19 个不该陪葬。前端按 removed / errors 分别 toast。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileRemoveResult {
    pub removed: Vec<String>,
    pub errors: Vec<FileRemoveError>,
}

/// [`FileRemoveResult::errors`] 的元素（TS 里是内联的匿名类型）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileRemoveError {
    pub path: String,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn listing() {
        let l = roundtrip::<WorkspaceListing>(
            r#"{"path":"src","truncated":false,"entries":[
                 {"name":"lib","path":"src/lib","kind":"dir","mtime":1758700000},
                 {"name":"main.rs","path":"src/main.rs","kind":"file","size":1234,"mtime":1758700001},
                 {"name":"broken","path":"src/broken","kind":"file"}]}"#,
        );
        assert_eq!(l.entries[0].kind, WorkspaceEntryKind::Dir);
        assert_eq!(l.entries[0].size, None);
        roundtrip::<WorkspaceListing>(r#"{"path":"","entries":[],"truncated":true}"#);
    }

    #[test]
    fn index() {
        roundtrip::<WorkspaceIndex>(r#"{"paths":["README.md","src/main.rs"],"truncated":false}"#);
    }

    #[test]
    fn file_preview_every_kind() {
        let raw = r#""rawBase":"/api/projects/p1/raw/tok123/""#;
        let text = roundtrip::<WorkspaceFile>(&format!(
            r#"{{"preview":{{"kind":"text","text":"fn main() {{}}\n","size":13,"truncated":false}},{raw}}}"#
        ));
        assert!(matches!(text.preview, FilePreview::Text { size: 13, .. }));
        roundtrip::<WorkspaceFile>(&format!(
            r#"{{"preview":{{"kind":"image","mime":"image/png","size":20480}},{raw}}}"#
        ));
        roundtrip::<WorkspaceFile>(&format!(r#"{{"preview":{{"kind":"binary","size":9}},{raw}}}"#));
        let big = roundtrip::<WorkspaceFile>(&format!(
            r#"{{"preview":{{"kind":"too-large","size":40000000}},{raw}}}"#
        ));
        assert_eq!(big.preview, FilePreview::TooLarge { size: 40_000_000 });
    }

    #[test]
    fn file_preview_unknown_kind() {
        let f = serde_json::from_str::<WorkspaceFile>(
            r#"{"preview":{"kind":"pdf","pages":3,"size":1},"rawBase":"/r/"}"#,
        )
        .unwrap();
        assert_eq!(f.preview, FilePreview::Unknown);
    }

    #[test]
    fn file_ops() {
        roundtrip::<UploadResult>(r#"{"path":"docs/a.pdf","size":1048576}"#);
        roundtrip::<FileOpResult>(r#"{"path":"docs/new"}"#);
        let r = roundtrip::<FileRemoveResult>(
            r#"{"removed":["a.txt","dir"],"errors":[{"path":"locked.db","error":"Permission denied"}]}"#,
        );
        assert_eq!(r.errors[0].path, "locked.db");
    }
}
