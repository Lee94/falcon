//! REST 方法，按功能域分文件，与 `packages/web/src/api.ts` 逐条对应（名字是它的
//! snake_case）。路径、方法、query、请求体以服务端 `packages/server/src/routes.ts`
//! （飞书项目在 `meegle/routes.ts`）为准，api.ts 有遗漏的地方照服务端补。
//!
//! 所有方法都返回 `impl Future<Output = ApiResult<T>> + Send + 'static`：参数在调用时
//! 就拷成自有值、拼好路径，工作在第一次 poll 时才 spawn 到网络运行时（见 crate 文档）。

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

pub(crate) mod app_icon;
pub(crate) mod auth;
pub(crate) mod files;
pub(crate) mod forwards;
pub(crate) mod git;
pub(crate) mod hosts;
pub(crate) mod meegle;
pub(crate) mod projects;
pub(crate) mod sessions;
pub(crate) mod system;
pub(crate) mod transfer;

/// `encodeURIComponent` 不编码的那些字符之外全部百分号编码。
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// 路径里的一段，照 `encodeURIComponent` 编码。
///
/// web 只给飞书项目的 key 编了码，项目 / 会话 id 直接拼——它们是 UUID，编不编码
/// 一个样。这里一律编：`/` 出现在段里时必须编掉，否则就换了一条路由。
pub(crate) fn seg(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// query 串，编码规则同浏览器的 `URLSearchParams`（空格编成 `+`），服务端的
/// querystring 解析认这一套。
pub(crate) struct Query {
    ser: url::form_urlencoded::Serializer<'static, String>,
    any: bool,
}

impl Query {
    pub(crate) fn new() -> Self {
        Query { ser: url::form_urlencoded::Serializer::new(String::new()), any: false }
    }

    pub(crate) fn push(mut self, key: &str, value: &str) -> Self {
        self.ser.append_pair(key, value);
        self.any = true;
        self
    }

    /// 有值才带，空串也带（`listDir` 的 `path=""` 是 Windows 盘符列表，与缺省不同）。
    pub(crate) fn opt(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.push(key, v),
            None => self,
        }
    }

    /// 有值且非空才带：对应 web 里 `if (x) q.set(...)` 这种按真值判断的写法。
    pub(crate) fn nonempty(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) if !v.is_empty() => self.push(key, v),
            _ => self,
        }
    }

    pub(crate) fn flag(self, key: &str, on: bool) -> Self {
        if on { self.push(key, "1") } else { self }
    }

    /// `""` 或 `?a=b&c=d`。
    pub(crate) fn finish(mut self) -> String {
        if self.any { format!("?{}", self.ser.finish()) } else { String::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seg_matches_encode_uri_component() {
        assert_eq!(seg("6f1c-a2_b.c!~*'()"), "6f1c-a2_b.c!~*'()");
        assert_eq!(seg("a/b c"), "a%2Fb%20c");
        assert_eq!(seg("空间#1?"), "%E7%A9%BA%E9%97%B4%231%3F");
    }

    #[test]
    fn query_like_url_search_params() {
        assert_eq!(Query::new().finish(), "");
        let q = Query::new()
            .push("path", "src/a b.ts")
            .opt("origPath", None)
            .nonempty("repo", Some(""))
            .flag("untracked", true)
            .finish();
        assert_eq!(q, "?path=src%2Fa+b.ts&untracked=1");
        // path="" 要带上（Windows 盘符列表）
        assert_eq!(Query::new().opt("path", Some("")).finish(), "?path=");
    }
}
