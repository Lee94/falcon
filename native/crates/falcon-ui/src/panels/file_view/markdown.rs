//! Markdown 预览（旧 React 版的 components/Markdown.tsx）。README / CHANGELOG / ADR 这类文件
//! 在这里是主要内容，给一坨等宽源码不如直接渲染出来。
//!
//! 渲染交给 gpui-component 的 TextView（它不执行任何脚本，内联 HTML 也只是排版，没有 React 版
//! 那种"塞进 innerHTML 等于打开 XSS"的问题）。这里补三件事：
//! - **链接**：`http(s)` / `mailto` 交给系统浏览器（白名单，照 React 版 `lib/mdLink.ts`：
//!   `javascript:`、`data:` 一律不认）；指向仓库里另一个文件的相对链接在画布上打开；`#锚点` 不动；
//! - **相对图片**：TextView 自己只会把相对地址当成**这台 Mac 上的本地路径**去读——既读不到宿主机
//!   上的图，还会去碰本机同名文件。所以先把它们改写成链接（点开在查看窗口里看原图，React 版
//!   的虚线占位也是这个作用），再逐张经原始字节路由取回，内联成 `data:` 地址；
//! - **外链图片**：http(s) 的原样留给 TextView，由 `http.rs` 装给 GPUI 的外链客户端去取（不带
//!   falcon 的 cookie，与 React 版的 `<img src>` 一样直接从对方站点拿）。
//!
//! 代码块的高亮颜色与源码视图同一套映射（theme.rs 随主题装进组件库的那份）。

use std::collections::HashMap;
use std::sync::Arc;

use falcon_client::FalconClient;
use falcon_core::md_link::{external_href, resolve_rel};
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Window, div};

use crate::theme::Ui;
use crate::workspace::Workspace;
use crate::zoom::zpx;

/// 链接点击回调（TextView 要求 Send + Sync）
pub type LinkHandler = Arc<dyn Fn(&SharedString, &ClickEvent, &mut Window, &mut App) + Send + Sync>;

pub struct MarkdownPane {
    id: SharedString,
    source: SharedString,
    on_link: LinkHandler,
}

impl MarkdownPane {
    /// `dir` 是当前文件所在目录（工作目录相对），`url_for(path)` 给出某个工作目录相对路径的
    /// 原始字节地址（`rawBase` + 按段编码的路径）
    pub fn new(
        text: &str,
        dir: String,
        client: FalconClient,
        url_for: impl Fn(&str) -> String + 'static,
        on_link: LinkHandler,
        cx: &mut Context<Self>,
    ) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id: SharedString = format!("md-{}", SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)).into();

        // 先把所有图片都改成链接：取到字节之前（或取不到时）至少能点开
        let mut wanted: Vec<String> = Vec::new();
        let initial = rewrite_images(text, |href| {
            if let Some(url) = web_image(href) {
                return ImageRewrite::Url(url);
            }
            if external_href(href).is_none()
                && let Some(path) = resolve_rel(&dir, href)
                && !wanted.contains(&path)
            {
                wanted.push(path);
            }
            ImageRewrite::Link
        });

        if !wanted.is_empty() {
            let text = text.to_string();
            let fetches: Vec<(String, String)> = wanted.iter().map(|p| (p.clone(), url_for(p))).collect();
            cx.spawn(async move |this, cx| {
                let mut inline: HashMap<String, String> = HashMap::new();
                for (path, url) in fetches {
                    if let Ok(raw) = client.raw_bytes(&url).await
                        && let Some(mime) = image_mime(raw.content_type.as_deref(), &path)
                    {
                        inline.insert(path, format!("data:{mime};base64,{}", base64(&raw.bytes)));
                    }
                }
                if inline.is_empty() {
                    return;
                }
                let next = rewrite_images(&text, |href| {
                    if let Some(url) = web_image(href) {
                        return ImageRewrite::Url(url);
                    }
                    match resolve_rel(&dir, href).and_then(|p| inline.get(&p)) {
                        Some(data) if external_href(href).is_none() => ImageRewrite::Url(data.clone()),
                        _ => ImageRewrite::Link,
                    }
                });
                this.update(cx, |this, cx| {
                    this.source = next.into();
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        Self { id, source: initial.into(), on_link }
    }
}

/// 链接点击：外链交给系统浏览器，仓库内的相对链接在画布上打开
pub fn link_handler(ws: &Entity<Workspace>, project_id: String, dir: String) -> LinkHandler {
    let ws = ws.downgrade();
    Arc::new(move |href: &SharedString, _: &ClickEvent, _: &mut Window, cx: &mut App| {
        if let Some(url) = external_href(href) {
            cx.open_url(&url);
            return;
        }
        // 文档内锚点：这里没有目录也没做 id 映射，点了跳到别处反而更奇怪
        if href.starts_with('#') || href.starts_with("data:") {
            return;
        }
        if let Some(path) = resolve_rel(&dir, href) {
            ws.update(cx, |w, cx| w.open_file(&project_id, &path, cx)).ok();
        }
    })
}

impl Render for MarkdownPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let on_link = self.on_link.clone();
        let style = TextViewStyle {
            // theme.rs 随主题装好的那份，Arc 共享，不必每帧重建
            highlight_theme: gpui_kit::component::theme::Theme::global(cx).highlight_theme.clone(),
            ..Default::default()
        };
        div().id(SharedString::from(format!("{}-scroll", self.id))).size_full().overflow_y_scroll().child(
            // React 版：max-w-3xl 居中、px-6 py-5、text-sm
            div().mx_auto().max_w(zpx(768.)).px_6().py_5().text_size(zpx(14.)).text_color(ui.foreground).child(
                TextView::markdown(self.id.clone(), self.source.clone())
                    .style(style)
                    .selectable(true)
                    .on_link_click(move |href, e, window, cx| on_link(href, e, window, cx)),
            ),
        )
    }
}

/// http(s) 外链图片：原样交给 TextView（`mailto:` 之类不是图片，仍改写成链接）
fn web_image(href: &str) -> Option<String> {
    external_href(href).filter(|u| {
        let lower = u.to_ascii_lowercase();
        lower.starts_with("https://") || lower.starts_with("http://")
    })
}

enum ImageRewrite {
    /// 换成这个地址（`data:` 内联，或 http(s) 外链原样）
    Url(String),
    /// 改写成指向原地址的普通链接（alt 为空时用地址当文字）
    Link,
}

/// 找出 Markdown 源码里的每个 `![alt](href "title")`，按 `f(href)` 改写。
///
/// 围栏代码块（``` / ~~~）与行内代码（反引号）里的原样保留——那是在展示语法，不是图片。
/// 只认内联形式；引用式图片（`![alt][ref]`）少见，保持原样。
fn rewrite_images(src: &str, mut f: impl FnMut(&str) -> ImageRewrite) -> String {
    let mut out = String::with_capacity(src.len());
    let mut fence: Option<&str> = None;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(mark) = fence {
            if trimmed.starts_with(mark) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            out.push_str(line);
            continue;
        }
        rewrite_line(line, &mut f, &mut out);
    }
    out
}

fn rewrite_line(line: &str, f: &mut impl FnMut(&str) -> ImageRewrite, out: &mut String) {
    let b = line.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'`' => {
                // 行内代码：跳到等长的反引号串之后
                let run = b[i..].iter().take_while(|&&c| c == b'`').count();
                let close = "`".repeat(run);
                match line[i + run..].find(&close) {
                    Some(off) => i = i + run + off + run,
                    None => i += run,
                }
            }
            b'!' if b.get(i + 1) == Some(&b'[') => {
                if let Some(img) = parse_image(line, i) {
                    out.push_str(&line[copied..i]);
                    match f(&img.href) {
                        ImageRewrite::Url(url) => {
                            out.push_str(&format!("![{}]({url})", img.alt));
                        }
                        ImageRewrite::Link => {
                            let text = if img.alt.trim().is_empty() { img.href.clone() } else { img.alt.clone() };
                            out.push_str(&format!("[{text}](<{}>)", img.href));
                        }
                    }
                    i = img.end;
                    copied = i;
                } else {
                    i += 2;
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&line[copied..]);
}

struct ParsedImage {
    alt: String,
    href: String,
    /// 整个 `![…](…)` 之后的下标
    end: usize,
}

fn parse_image(line: &str, start: usize) -> Option<ParsedImage> {
    let b = line.as_bytes();
    // alt：到配对的 ]（允许一层嵌套的方括号）
    let mut depth = 0;
    let mut j = start + 2;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => break,
            b']' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    if j >= b.len() || b.get(j + 1) != Some(&b'(') {
        return None;
    }
    let alt = line[start + 2..j].to_string();
    let mut k = j + 2;
    while k < b.len() && b[k] == b' ' {
        k += 1;
    }
    let (href, mut k) = if b.get(k) == Some(&b'<') {
        let close = line[k + 1..].find('>')? + k + 1;
        (line[k + 1..close].to_string(), close + 1)
    } else {
        let s = k;
        let mut paren = 0;
        while k < b.len() {
            match b[k] {
                b'(' => paren += 1,
                b')' if paren == 0 => break,
                b')' => paren -= 1,
                b' ' | b'\t' => break,
                _ => {}
            }
            k += 1;
        }
        (line[s..k].to_string(), k)
    };
    // 标题（"…" / '…' / (…)）跳过，找收尾的 )
    let close = line[k..].find(')')? + k;
    k = close + 1;
    if href.is_empty() {
        return None;
    }
    Some(ParsedImage { alt, href, end: k })
}

/// 能被 TextView 当成图片解码的类型；认不出（avif 等）返回 None，保持链接
fn image_mime(content_type: Option<&str>, path: &str) -> Option<&'static str> {
    let from_header = content_type.map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase());
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let pick = |s: &str| -> Option<&'static str> {
        Some(match s {
            "image/png" | "png" => "image/png",
            "image/jpeg" | "image/jpg" | "jpg" | "jpeg" => "image/jpeg",
            "image/gif" | "gif" => "image/gif",
            "image/webp" | "webp" => "image/webp",
            "image/svg+xml" | "svg" => "image/svg+xml",
            "image/bmp" | "bmp" => "image/bmp",
            _ => return None,
        })
    };
    from_header.as_deref().and_then(pick).or_else(|| pick(&ext))
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(src: &str) -> String {
        rewrite_images(src, |_| ImageRewrite::Link)
    }

    #[test]
    fn images_become_links_outside_code() {
        assert_eq!(links("a ![logo](img/a.png) b\n"), "a [logo](<img/a.png>) b\n");
        assert_eq!(links("![](x.svg \"t\")"), "[x.svg](<x.svg>)");
        assert_eq!(links("`![x](y)` ![z](w)"), "`![x](y)` [z](<w>)");
        assert_eq!(links("```\n![x](y)\n```\n![z](w)"), "```\n![x](y)\n```\n[z](<w>)");
        assert_eq!(links("![a](<with space.png>)"), "[a](<with space.png>)");
        assert_eq!(links("not an image ![x] here"), "not an image ![x] here");
    }

    #[test]
    fn images_can_be_inlined() {
        let got = rewrite_images("![a](b.png)", |h| ImageRewrite::Url(format!("data:{h}")));
        assert_eq!(got, "![a](data:b.png)");
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn mime_from_header_or_ext() {
        assert_eq!(image_mime(Some("image/svg+xml; charset=utf-8"), "a.svg"), Some("image/svg+xml"));
        assert_eq!(image_mime(Some("application/octet-stream"), "a.PNG"), Some("image/png"));
        assert_eq!(image_mime(None, "a.avif"), None);
    }
}
