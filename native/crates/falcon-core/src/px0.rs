//! px0 在 falcon 源下的挂载前缀（ADR 0017）。对应 shared 的 `px0BasePath`。
//!
//! 原生客户端没有嵌 px0：菜单项把 `<服务地址><前缀>` 交给系统浏览器。浏览器没登录过
//! falcon 时，服务端会把它送去 `/?next=<前缀>` 登录；登录成功后由浏览器版按 [`login_next`]
//! 跳回（falcon-web 的 `Platform::redirect_after_login`），旧 React 版是 lib/loginNext.ts。

use crate::js::encode_uri_component;

/// `/px0/<项目 id>/`。服务端起 px0 时的 `-base-path`、反代路由、客户端菜单里开的地址都是它
pub fn px0_base_path(project_id: &str) -> String {
    format!("/px0/{}/", encode_uri_component(project_id))
}

/// 登录后回跳的地址：页面查询串（`location.search`，带不带 `?` 都行）里的 `next`。
///
/// 只认站内的 px0 入口：`/px0/` 开头保证是同源路径，挡掉 `//evil.example`、
/// `/\evil.example`、`javascript:` 这类开放跳转。别的地址一律当没有，照常进首页。
pub fn login_next(search: &str) -> Option<String> {
    let next = search_param(search, "next")?;
    (next.starts_with("/px0/") && !next.contains('\\')).then_some(next)
}

/// `new URLSearchParams(search).get(name)`：去掉开头的 `?`，按 `&` 切、在第一个 `=` 处分键值，
/// 取第一个同名的。
fn search_param(search: &str, name: &str) -> Option<String> {
    let query = search.strip_prefix('?').unwrap_or(search);
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (form_decode(key) == name).then(|| form_decode(value))
    })
}

/// application/x-www-form-urlencoded 解码：`+` 是空格，`%XX` 按字节还原，坏的 `%` 原样保留
/// （URLSearchParams 不抛错，与 decodeURIComponent 不同），最后按 UTF-8 宽松解。
fn form_decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_shared_px0_base_path() {
        assert_eq!(px0_base_path("d7697ab3-292d-4ef9-a8de-b10c4ebd962a"), "/px0/d7697ab3-292d-4ef9-a8de-b10c4ebd962a/");
        assert_eq!(px0_base_path("a/b c"), "/px0/a%2Fb%20c/");
    }

    // 以下两条移植自 React 版的 lib/loginNext.test.ts

    #[test]
    fn login_next_returns_the_px0_entry_the_server_sent_us_away_from() {
        assert_eq!(login_next("?next=%2Fpx0%2Fp1%2F").as_deref(), Some("/px0/p1/"));
        assert_eq!(login_next("?next=%2Fpx0%2Fp1%2F%3Fretry%3D1").as_deref(), Some("/px0/p1/?retry=1"));
    }

    #[test]
    fn login_next_ignores_anything_that_could_leave_the_site_or_is_not_px0() {
        for next in ["//evil.example/px0/", "/\\evil.example", "https://evil.example/px0/", "javascript:alert(1)", "/api/sessions", "/"] {
            assert_eq!(login_next(&format!("?next={}", encode_uri_component(next))), None, "{next}");
        }
        assert_eq!(login_next(""), None);
    }

    // 以下是 Rust 侧补的：URLSearchParams 的解析细节

    #[test]
    fn search_params_decode_like_url_search_params() {
        // 不带 ? 也行；取第一个同名的；别的参数不干扰
        assert_eq!(login_next("a=1&next=/px0/x/&next=/px0/y/").as_deref(), Some("/px0/x/"));
        // 服务端用 encodeURIComponent 编的完整路径 + 查询能原样还原
        let full = format!("/px0/{}/?q=a b&r=1", encode_uri_component("p/1"));
        assert_eq!(login_next(&format!("?next={}", encode_uri_component(&full))).as_deref(), Some(full.as_str()));
        // + 是空格，坏的 % 原样保留，不报错
        assert_eq!(search_param("?k=a+b%2", "k").as_deref(), Some("a b%2"));
        assert_eq!(search_param("?k=%zz%E5%9B%BE", "k").as_deref(), Some("%zz图"));
        assert_eq!(search_param("?k", "k").as_deref(), Some(""));
        assert_eq!(search_param("?other=1", "k"), None);
    }
}
