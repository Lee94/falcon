//! 原始字节地址（ADR 0007）。对应 web 的 `lib/rawUrl.ts`。
//!
//! `raw_base` 来自 `GET /api/projects/:id/file` 的响应（`/api/projects/<id>/raw/<token>/`），
//! 后面接工作目录相对路径。HTML 预览的 WebView、图片查看都从这里拿地址。

use crate::js::encode_uri_component;

/// 逐段 `encodeURIComponent` 而不是整串：`/` 是路径分隔符得留着，而 `#` / `?` / `%`
/// 与空格出现在文件名里时必须编码，否则服务端拿到的是被截断或解错的路径。空段丢掉。
pub fn raw_url(raw_base: &str, path: &str) -> String {
    let encoded = path.split('/').filter(|s| !s.is_empty()).map(encode_uri_component).collect::<Vec<_>>().join("/");
    format!("{raw_base}{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/api/projects/p1/raw/tok/";

    #[test]
    fn keeps_slashes_and_encodes_each_segment() {
        assert_eq!(raw_url(BASE, "docs/index.html"), format!("{BASE}docs/index.html"));
        assert_eq!(raw_url(BASE, "a b/c#1?.png"), format!("{BASE}a%20b/c%231%3F.png"));
        assert_eq!(raw_url(BASE, "图/片.png"), format!("{BASE}%E5%9B%BE/%E7%89%87.png"));
    }

    #[test]
    fn drops_empty_segments() {
        assert_eq!(raw_url(BASE, "/a//b"), format!("{BASE}a/b"));
    }
}
