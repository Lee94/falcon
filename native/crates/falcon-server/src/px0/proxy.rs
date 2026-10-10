//! 移植自 `packages/server/src/px0/proxy.ts`（含 `proxy.test.ts` 的全部用例）。
//!
//! px0 反代的纯函数：请求 / 响应头过滤，以及「正在启动」页（ADR 0017 决定六、七）。
//!
//! 反代只改下面几处，其余原样透传——px0 本来就认 `/px0/<id>/` 这个前缀，路径不用改写：
//! - 请求去掉 cookie / authorization：falcon 的登录令牌不该送进宿主机上的第三方进程
//!   （px0 开 -verbose 会把请求打到终端上）。
//! - 请求的 Host / Origin 换成 px0 认的样子。px0 会改东西的 POST（派 agent 编辑、
//!   commit / push、写会话、PR 评论、装语言服务器）都过它的 localPost：Host 必须是
//!   IP 或 localhost，且 Origin 的 host 必须等于 Host。经反代后 Host 是 127.0.0.1，
//!   浏览器的 Origin 却是 falcon 的地址（内网里可能是主机名），原样转发一律 403。
//!   所以**只有同源请求**（Origin 的 host 等于浏览器发来的 Host）才把 Origin 改成
//!   `http://127.0.0.1`；跨源的 Origin 原样转过去，让 px0 照旧拒掉——falcon 的 cookie
//!   是 SameSite=Lax，同站不同端口的页面仍能带着它发 POST，这道检查还有用。
//! - 响应里 CSP 的 `frame-ancestors 'none'` 改成 `'self'`（main 分支已带上，下个版本起
//!   生效），留出以后嵌进工作区窗口的余地；去掉 set-cookie——px0 不用 cookie，
//!   而它和 falcon 同源，放行等于让它能改写 falcon 的登录 cookie。
//!
//! # 移植约定
//!
//! - 头用 `http::HeaderMap`（axum / hyper 的那个）：头名本身就大小写不敏感、存成小写，
//!   同名头可以有多个值。Node 的 IncomingHttpHeaders 把大部分同名头用 ", " 并成一个
//!   字符串（set-cookie 是数组、host 只留第一个）；这里同名的多个值原样逐个转发，
//!   语义等价（RFC 9110 §5.3）。要当字符串看的几处（Connection、Origin、CSP）照 Node
//!   的规矩先并起来再处理。
//! - 头的值按 latin1 解码成字符串（Node 也是这么解的），非 ASCII 字节不会让比较失败。
//! - Origin 的 host 用 `url` crate 取，与 `new URL(origin).host` 同一套 WHATWG 解析
//!   （默认端口省略、主机名转小写、IPv6 带方括号）。
//!
//! 不在这里的：`routes.ts` 的 `registerPx0Routes`、`forward`（真正的反代与 SSE 透传）随 api 层
//! 的路由一起移植；上游连接从 [`super::manager::Px0Manager::connect`] 拿。

use std::collections::HashSet;
use std::sync::LazyLock;

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use regex::Regex;

/// 逐跳头：只对一跳有意义，反代两侧各自管理（RFC 9110 §7.6.1）
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 头的值按 latin1 解成字符串（Node 的做法）
fn latin1(v: &HeaderValue) -> String {
    v.as_bytes().iter().map(|&b| char::from(b)).collect()
}

/// 同名头的全部值用 `sep` 并成一个字符串；一个都没有时是 None（TS 的 undefined）
fn joined(headers: &HeaderMap, name: &HeaderName, sep: &str) -> Option<String> {
    let values: Vec<String> = headers.get_all(name).iter().map(latin1).collect();
    (!values.is_empty()).then(|| values.join(sep))
}

/// `Connection: foo, bar` 里点名的头也是逐跳的
fn connection_tokens(headers: &HeaderMap) -> HashSet<String> {
    // Array.isArray(raw) ? raw.join(",") : (raw ?? "")
    let text = joined(headers, &header::CONNECTION, ",").unwrap_or_default();
    text.split(',').map(|s| js::trim(s).to_lowercase()).filter(|s| !s.is_empty()).collect()
}

fn is_hop_by_hop(name: &str, named: &HashSet<String>) -> bool {
    HOP_BY_HOP.contains(&name) || named.contains(name)
}

/// Origin 的 host[:port]；不是合法 URL（含字面量 "null"）返回 None
fn origin_host(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    let host = url.host_str()?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    // new URL(origin).host || null
    (!host.is_empty()).then_some(host)
}

/// upstream_host 必须是 IP 或 localhost（px0 的 localPost 只认这两种 Host），
/// 这里固定传 127.0.0.1。
pub fn upstream_request_headers(headers: &HeaderMap, upstream_host: &str) -> HeaderMap {
    let named = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let k = name.as_str();
        if is_hop_by_hop(k, &named) {
            continue;
        }
        if k == "host" || k == "cookie" || k == "authorization" {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    // upstream_host 是 IP / localhost，总是合法的头值；万一不是，宁可不带 Host 也不 panic
    if let Ok(v) = HeaderValue::from_str(upstream_host) {
        out.insert(header::HOST, v);
    }
    // headers.host：Node 只留第一个 Host
    let host = headers.get(header::HOST).map(latin1).filter(|h| !h.is_empty());
    if let (Some(origin), Some(host)) = (joined(headers, &header::ORIGIN, ", "), host)
        && origin_host(&origin) == Some(host.to_lowercase())
        && let Ok(v) = HeaderValue::from_str(&format!("http://{upstream_host}"))
    {
        out.insert(header::ORIGIN, v);
    }
    out
}

pub fn rewrite_csp(value: &str) -> String {
    // /frame-ancestors\s+'none'/gi
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!("{}{}+'{}'", js::ci("frame-ancestors"), js::WS, js::ci("none"))).unwrap());
    RE.replace_all(value, "frame-ancestors 'self'").into_owned()
}

pub fn downstream_response_headers(headers: &HeaderMap) -> HeaderMap {
    let named = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for name in headers.keys() {
        let k = name.as_str();
        if is_hop_by_hop(k, &named) {
            continue;
        }
        if name == header::SET_COOKIE {
            continue;
        }
        if name == header::CONTENT_SECURITY_POLICY {
            // 多条 CSP 头与逗号拼成一条等价（各条策略取交集）
            let csp: Vec<String> = headers.get_all(name).iter().map(|v| rewrite_csp(&latin1(v))).collect();
            // 改写只换 ASCII，字节仍在 latin1 范围内，按原样编回去
            let bytes: Vec<u8> = csp.join(", ").chars().map(|c| c as u8).collect();
            if let Ok(v) = HeaderValue::from_bytes(&bytes) {
                out.insert(name.clone(), v);
            }
            continue;
        }
        for value in headers.get_all(name) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// 入口页的样子：正在起（带阶段），或起不来（带原因）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Px0PageState {
    Starting { stage: Px0Stage },
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Px0Stage {
    Preparing,
    Downloading,
    Installing,
    Launching,
}

impl Px0Stage {
    /// TS 的 `STAGE_TEXT`
    pub fn text(self) -> &'static str {
        match self {
            Px0Stage::Preparing => "正在连接宿主机…",
            Px0Stage::Downloading => "正在下载 px0…",
            Px0Stage::Installing => "正在把 px0 装到宿主机…",
            Px0Stage::Launching => "正在启动 px0…",
        }
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// px0 还没起来时入口页回的 HTML。没有脚本：起着就 meta refresh 每秒刷一次，
/// 起好了刷新落到 px0 本体；起不来就停在这里显示原因，重试链接带 `?retry=1`。
///
/// 文案写死中文：这一页由服务端直接吐出、不经 web 的 i18n，与服务端的错误文案同一口径。
/// 颜色只用系统色（Canvas / CanvasText），跟着系统明暗走——这里拿不到 falcon 的主题。
pub fn px0_status_page(state: &Px0PageState, base_path: &str, project_name: &str) -> String {
    let title = escape_html(&format!("px0 · {project_name}"));
    let home = escape_html(base_path);
    let refresh = match state {
        Px0PageState::Starting { .. } => format!("<meta http-equiv=\"refresh\" content=\"1;url={home}\">"),
        Px0PageState::Error { .. } => String::new(),
    };
    let body = match state {
        Px0PageState::Starting { stage } => format!(
            "<p class=\"lead\">{}</p><p class=\"hint\">首次打开要下载并安装 px0，可能要半分钟。</p>",
            escape_html(stage.text())
        ),
        Px0PageState::Error { message } => format!(
            "<p class=\"lead\">px0 没能启动</p><pre>{}</pre><p><a href=\"{home}?retry=1\">重试</a></p>",
            escape_html(message)
        ),
    };
    let title_line = format!("<title>{title}</title>");
    let body_line = format!("<body><main>{body}</main></body>");
    [
        "<!doctype html>",
        "<html lang=\"zh-CN\">",
        "<head>",
        "<meta charset=\"utf-8\">",
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
        &refresh,
        &title_line,
        "<style>",
        ":root{color-scheme:light dark}",
        "body{margin:0;min-height:100vh;display:grid;place-items:center;background:Canvas;color:CanvasText;",
        "font:14px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace}",
        "main{max-width:640px;padding:24px}",
        ".lead{font-size:16px;margin:0 0 8px}",
        ".hint{opacity:.6;margin:0}",
        "pre{white-space:pre-wrap;word-break:break-all;opacity:.8}",
        "a{color:LinkText}",
        "</style>",
        "</head>",
        &body_line,
        "</html>",
    ]
    .iter()
    // .filter(Boolean)：起不来时没有 refresh 那一行
    .filter(|s| !s.is_empty())
    .copied()
    .collect::<Vec<&str>>()
    .join("\n")
}

/// 照抄 TS 语义要用到的几处 JavaScript 行为（与 meegle/command.rs 的 `js` 同源；
/// falcon-server 还没有公共的落脚点，先各带一份）。
mod js {
    /// `String.prototype.trim` 的空白集（ECMAScript WhiteSpace + LineTerminator）
    pub(super) fn trim(s: &str) -> &str {
        s.trim_matches(|c| {
            matches!(
                c,
                '\u{9}'..='\u{d}'
                    | ' '
                    | '\u{a0}'
                    | '\u{1680}'
                    | '\u{2000}'..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
            )
        })
    }

    /// 正则里的 `\s`
    pub(super) const WS: &str =
        r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

    /// 不带 `u` 标志的 `/…/i` 的源码 → Rust 正则：ASCII 字母展开成 `[xX]`（Rust 的 `(?i)` 还会
    /// 让开尔文符号、长 s 配上 k / s）。只用在纯字面量片段上。
    pub(super) fn ci(literal: &str) -> String {
        literal
            .chars()
            .map(|c| {
                if c.is_ascii_alphabetic() {
                    format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
                } else {
                    regex::escape(&c.to_string())
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    /// upstreamRequestHeaders · drops falcon credentials and hop-by-hop headers, rewrites host
    #[test]
    fn upstream_drops_credentials_and_hop_by_hop() {
        let out = upstream_request_headers(
            &headers(&[
                ("host", "localhost:4923"),
                ("cookie", "falcon_token=secret"),
                ("authorization", "Bearer x"),
                ("connection", "keep-alive, x-custom-hop"),
                ("x-custom-hop", "1"),
                ("keep-alive", "timeout=5"),
                ("transfer-encoding", "chunked"),
                ("accept", "text/event-stream"),
                ("content-type", "application/json"),
                ("content-length", "12"),
            ]),
            "127.0.0.1",
        );
        assert_eq!(
            out,
            headers(&[
                ("host", "127.0.0.1"),
                ("accept", "text/event-stream"),
                ("content-type", "application/json"),
                ("content-length", "12"),
            ])
        );
    }

    // px0 的 localPost：Origin 的 host 必须等于 Host，且 Host 是 IP 或 localhost

    /// upstreamRequestHeaders · Origin · maps a same-origin POST onto px0's own origin
    #[test]
    fn same_origin_post_maps_onto_px0_origin() {
        let out = upstream_request_headers(
            &headers(&[("host", "mac-mini.local:6789"), ("origin", "http://mac-mini.local:6789")]),
            "127.0.0.1",
        );
        assert_eq!(out.get(header::HOST).unwrap(), "127.0.0.1");
        assert_eq!(out.get(header::ORIGIN).unwrap(), "http://127.0.0.1");
    }

    /// upstreamRequestHeaders · Origin · forwards a cross-origin Origin untouched so px0 still rejects it
    #[test]
    fn cross_origin_forwarded_untouched() {
        for origin in ["http://mac-mini.local:7777", "https://evil.example", "null"] {
            let out =
                upstream_request_headers(&headers(&[("host", "mac-mini.local:6789"), ("origin", origin)]), "127.0.0.1");
            assert_eq!(out.get(header::ORIGIN).unwrap(), origin);
        }
    }

    /// upstreamRequestHeaders · Origin · adds no Origin when the browser sent none
    #[test]
    fn no_origin_added_when_none_sent() {
        let out = upstream_request_headers(&headers(&[("host", "localhost:6789")]), "127.0.0.1");
        assert_eq!(out.get(header::ORIGIN), None);
    }

    /// downstreamResponseHeaders · rewrites frame-ancestors and drops set-cookie / hop-by-hop
    #[test]
    fn downstream_rewrites_frame_ancestors_drops_set_cookie() {
        let out = downstream_response_headers(&headers(&[
            ("content-type", "text/html"),
            ("content-encoding", "gzip"),
            ("content-security-policy", "default-src 'self'; frame-ancestors 'none'; form-action 'none';"),
            ("set-cookie", "falcon_token=evil; Path=/"),
            ("connection", "close"),
            ("transfer-encoding", "chunked"),
        ]));
        assert_eq!(
            out,
            headers(&[
                ("content-type", "text/html"),
                ("content-encoding", "gzip"),
                ("content-security-policy", "default-src 'self'; frame-ancestors 'self'; form-action 'none';"),
            ])
        );
    }

    /// downstreamResponseHeaders · leaves the rest of the CSP alone
    #[test]
    fn rewrite_csp_leaves_rest_alone() {
        let csp = "default-src 'self'; script-src 'self'; connect-src 'self'";
        assert_eq!(rewrite_csp(csp), csp);
    }

    /// px0StatusPage · refreshes back to the entry while starting
    #[test]
    fn status_page_refreshes_while_starting() {
        let html = px0_status_page(&Px0PageState::Starting { stage: Px0Stage::Installing }, "/px0/p1/", "app");
        assert!(html.contains(r#"http-equiv="refresh" content="1;url=/px0/p1/""#));
        assert!(html.contains("正在把 px0 装到宿主机"));
    }

    /// px0StatusPage · stops refreshing on error, escapes the message, offers a retry
    #[test]
    fn status_page_error_escapes_and_offers_retry() {
        let html =
            px0_status_page(&Px0PageState::Error { message: "<script>alert(1)</script>".into() }, "/px0/p1/", "a<b");
        assert!(!html.contains(r#"http-equiv="refresh""#));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("px0 · a&lt;b"));
        assert!(html.contains(r#"href="/px0/p1/?retry=1""#));
    }

    // ---- 以下是 Rust 侧补的 ----

    #[test]
    fn origin_compare_follows_url_host_rules() {
        let rewritten = |host: &str, origin: &str| {
            upstream_request_headers(&headers(&[("host", host), ("origin", origin)]), "127.0.0.1")
                .get(header::ORIGIN)
                .is_some_and(|v| v == "http://127.0.0.1")
        };
        // 默认端口省略、主机名小写、IPv6 带方括号——与 new URL(origin).host 一致
        assert!(rewritten("example.com", "http://EXAMPLE.com:80"));
        assert!(rewritten("Example.COM:8080", "http://example.com:8080"));
        assert!(rewritten("[::1]:6789", "http://[::1]:6789"));
        assert!(!rewritten("example.com", "http://example.com:8080"));
        // 浏览器发来的 Host 是空串：不改
        assert!(!rewritten("", "http://example.com"));
    }

    #[test]
    fn multiple_values_and_case() {
        // 多条 CSP 并成一条；Connection 点名的头不分大小写
        let out = downstream_response_headers(&headers(&[
            ("content-security-policy", "frame-ancestors 'none'"),
            ("content-security-policy", "FRAME-ANCESTORS\t'NONE'; img-src *"),
            ("connection", "X-Trace"),
            ("x-trace", "1"),
            ("vary", "accept"),
            ("vary", "origin"),
        ]));
        assert_eq!(
            out.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "frame-ancestors 'self', frame-ancestors 'self'; img-src *"
        );
        assert_eq!(out.get("x-trace"), None);
        assert_eq!(out.get_all(header::VARY).iter().count(), 2);
    }
}
