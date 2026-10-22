//! Markdown 文档里链接目标的解析。对应旧 React 版的 `lib/mdLink.ts`，供 Markdown 预览用。

use crate::js::{decode_uri_component, js_trim};

/// 能直接交给系统浏览器的地址。
///
/// 白名单而不是黑名单：`javascript:` 只是最有名的那个，`data:text/html` 同样能
/// 执行脚本。认不出的一律降级成纯文本，代价只是少一个可点的链接。
pub fn external_href(href: &str) -> Option<String> {
    let s = js_trim(href);
    let lower_head = |n: usize| s.get(..n).map(str::to_ascii_lowercase);
    let ok = lower_head(7).as_deref() == Some("http://")
        || lower_head(8).as_deref() == Some("https://")
        || lower_head(7).as_deref() == Some("mailto:");
    ok.then(|| s.to_string())
}

/// 文档里的相对链接 → 与 dir 同一种身份的路径（项目相对，或宿主机绝对），可直接喂给查看窗口。
///
/// 项目相对目录不能弹出项目；绝对目录不能弹出文件系统根、盘符或 UNC 共享根。raw 图片
/// 仍传 token 范围内的相对目录，不会因为支持绝对文件导航而越出预览范围。
/// 锚点与 query 一并丢掉——查看窗口里没有它们的语义。
///
/// href 是 URL 而不是路径：Markdown 里带空格的文件名只能写成 `img%20dir/a.png`
/// （或用 <> 包起来），解析器原样给出。每段 decodeURIComponent 还原成真实文件名，
/// 之后再交给 falcon-client 的 `raw_url` 编码或喂给查看窗口才不会二次编码成 `%2520`。
/// 解不动的（孤零零的 `%`）按原文保留——那多半本来就是文件名的一部分。
pub fn resolve_rel(dir: &str, href: &str) -> Option<String> {
    let no_hash = href.split('#').next().unwrap_or("");
    let clean = js_trim(no_hash.split('?').next().unwrap_or(""));
    if clean.is_empty() {
        return None;
    }
    // 绝对路径指的是宿主机的根，不在工作目录里，给不出来
    if clean.starts_with('/') {
        return None;
    }
    let mut segs: Vec<String> =
        if dir.is_empty() { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).map(String::from).collect() };
    // 文件面板也能打开项目外文件：链接保留宿主机绝对路径前缀；盘符和 UNC 共享根不能被 .. 弹掉。
    let prefix = if dir.starts_with("//") { "//" } else if dir.starts_with('/') { "/" } else { "" };
    let drive = dir.as_bytes().get(1) == Some(&b':');
    let root_depth = if prefix == "//" { 2 } else { usize::from(drive) };
    for raw in clean.split('/') {
        let part = decode_uri_component(raw).unwrap_or_else(|| raw.to_string());
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if segs.len() <= root_depth {
                return None;
            }
            segs.pop()?;
            continue;
        }
        segs.push(part);
    }
    if segs.is_empty() { None } else {
        let path = format!("{prefix}{}", segs.join("/"));
        Some(if drive && segs.len() == 1 { format!("{path}/") } else { path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_href_only_lets_http_s_and_mailto_through() {
        assert_eq!(external_href("https://a.b/c").as_deref(), Some("https://a.b/c"));
        assert_eq!(external_href("mailto:x@y.z").as_deref(), Some("mailto:x@y.z"));
        assert_eq!(external_href("javascript:alert(1)"), None);
        assert_eq!(external_href("data:text/html,hi"), None);
        assert_eq!(external_href("docs/a.md"), None);
        // 以下是 Rust 侧补的：大小写不敏感、首尾空白先剥掉
        assert_eq!(external_href("  HTTP://a.b ").as_deref(), Some("HTTP://a.b"));
        assert_eq!(external_href("http:/a"), None);
    }

    #[test]
    fn resolve_rel_resolves_against_the_document_directory() {
        assert_eq!(resolve_rel("docs", "./a.md").as_deref(), Some("docs/a.md"));
        assert_eq!(resolve_rel("docs/x", "../b.md").as_deref(), Some("docs/b.md"));
        assert_eq!(resolve_rel("", "README.md").as_deref(), Some("README.md"));
    }

    #[test]
    fn resolve_rel_refuses_to_escape_the_workspace_or_use_host_absolute_paths() {
        assert_eq!(resolve_rel("docs", "../../etc/passwd"), None);
        assert_eq!(resolve_rel("docs", "/etc/passwd"), None);
    }

    #[test]
    fn resolve_rel_preserves_absolute_file_locations() {
        assert_eq!(resolve_rel("/tmp/docs", "./a.md").as_deref(), Some("/tmp/docs/a.md"));
        assert_eq!(resolve_rel("/tmp/docs", "../a.md").as_deref(), Some("/tmp/a.md"));
        assert_eq!(resolve_rel("C:/docs", "../a.md").as_deref(), Some("C:/a.md"));
        assert_eq!(resolve_rel("C:/", "../a.md"), None);
        assert_eq!(resolve_rel("//host/share/docs", "../a.md").as_deref(), Some("//host/share/a.md"));
        assert_eq!(resolve_rel("//host/share", "../a.md"), None);
    }

    #[test]
    fn resolve_rel_drops_anchors_and_queries() {
        assert_eq!(resolve_rel("docs", "a.md#top").as_deref(), Some("docs/a.md"));
        assert_eq!(resolve_rel("docs", "a.md?x=1").as_deref(), Some("docs/a.md"));
        assert_eq!(resolve_rel("docs", "#top"), None);
    }

    #[test]
    fn resolve_rel_decodes_percent_encoded_segments_so_the_path_names_the_real_file() {
        assert_eq!(resolve_rel("", "img%20dir/logo.png").as_deref(), Some("img dir/logo.png"));
        assert_eq!(resolve_rel("docs", "%E5%9B%BE/%E7%89%87.png").as_deref(), Some("docs/图/片.png"));
        // 解不动的按原文保留
        assert_eq!(resolve_rel("", "100%/a.png").as_deref(), Some("100%/a.png"));
        // 编码过的 `..` 同样是上跳
        assert_eq!(resolve_rel("docs", "%2E%2E/b.md").as_deref(), Some("b.md"));
    }
}
