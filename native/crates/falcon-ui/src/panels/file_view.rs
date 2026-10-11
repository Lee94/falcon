//! 画布上的文件查看窗口（旧 React 版的 components/FileView.tsx，ADR 0007）。
//!
//! 目标由排布给（canvas 调 [`FileView::set_target`]）而不是跟着焦点走：文件窗口是列里的一扇，
//! 焦点在别处时它照样要显示自己的内容。打开 / 换文件 / 手动刷新时拉一次，不轮询——正看着的
//! 文件在眼皮底下换掉，比看到旧内容更让人困惑（要新的按刷新就是了）。只读，没有编辑：改文件
//! 是终端里的事，这里不做半个编辑器。
//!
//! 内容按预览类型分四种：源码（tree-sitter 高亮，[`code`]）、图片（缩放 / 平移，[`image`]）、
//! Markdown（渲染，[`markdown`]）、HTML（WebView，[`html`]）。后两种有"预览 / 源码"切换，
//! 选择记在偏好里（`falcon.fileViewMode`，沿用 React 版的键）。图片与 HTML 的字节走原始字节路由
//! （`rawBase + path`，URL 里带只能读这个项目文件的作用域令牌），不经 JSON。

mod code;
mod html;
mod image;
/// 设置里上传自定义应用图标也按扩展名认格式（app_icon.rs）
pub(crate) use image::format_by_ext;
mod markdown;

use falcon_client::raw_url;
use falcon_proto::{FilePreview, WorkspaceFile};
use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, IntoElement, Render, SharedString, Stateful, Subscription,
    Window, div,
};
use rust_i18n::t;

use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{ActiveView, Workspace};

use self::code::CodePane;
use self::html::HtmlPane;
use self::image::ImagePane;
use self::markdown::MarkdownPane;
use crate::zoom::zpx;

const VIEW_KEY: &str = "falcon.fileViewMode";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Preview,
    Source,
}

enum Body {
    /// 还没建（或这种预览不需要子视图：二进制 / 太大 / 空文件）
    None,
    Code(Entity<CodePane>),
    Image(Entity<ImagePane>),
    Markdown(Entity<MarkdownPane>),
    Html(Entity<HtmlPane>),
}

pub struct FileView {
    ws: Entity<Workspace>,
    pub project_id: String,
    pub path: String,
    result: Option<WorkspaceFile>,
    error: Option<String>,
    busy: bool,
    /// 过期响应的闸门：换了文件之后，上一个文件的内容晚到了也不认
    generation: u64,
    mode: ViewMode,
    body: Body,
    /// 内容或模式变了、子视图要在下一次渲染时重建（建代码编辑器要 Window）
    body_dirty: bool,
    _subs: Vec<Subscription>,
}

impl FileView {
    pub fn new(ws: Entity<Workspace>, project_id: String, path: String, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe(&ws, |this, _, cx| {
            this.sync_shown(cx);
            cx.notify();
        })];
        let mode = match Prefs::global(cx).get(VIEW_KEY) {
            Some("source") => ViewMode::Source,
            _ => ViewMode::Preview,
        };
        let mut this = Self {
            ws,
            project_id,
            path,
            result: None,
            error: None,
            busy: false,
            generation: 0,
            mode,
            body: Body::None,
            body_dirty: false,
            _subs: subs,
        };
        this.load(cx);
        this
    }

    /// 同一扇窗口换内容（预览语义：点另一个文件不新开窗口）
    pub fn set_target(&mut self, project_id: String, path: String, cx: &mut Context<Self>) {
        self.project_id = project_id;
        self.path = path;
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.result = None;
        self.error = None;
        self.body = Body::None;
        self.body_dirty = false;
        let fut = self.ws.read(cx).client.read_file(&self.project_id, &self.path);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(file) => {
                        this.result = Some(file);
                        this.body_dirty = true;
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.load(cx);
    }

    fn switch_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        cx.global_mut::<Prefs>().set(VIEW_KEY, if mode == ViewMode::Source { "source" } else { "preview" }.to_string());
        self.body = Body::None;
        self.body_dirty = true;
        cx.notify();
    }

    fn previewing(&self) -> bool {
        (is_markdown(&self.path) || is_html(&self.path)) && self.mode == ViewMode::Preview
    }

    /// 这个项目原始字节路由的绝对前缀（基址 + rawBase），HTML 预览只准在它下面导航
    fn raw_prefix(&self, cx: &App) -> Option<String> {
        let result = self.result.as_ref()?;
        Some(format!("{}{}", self.ws.read(cx).client.base_url().trim_end_matches('/'), result.raw_base))
    }

    fn raw_absolute(&self, cx: &App) -> Option<String> {
        let result = self.result.as_ref()?;
        Some(format!("{}{}", self.ws.read(cx).client.base_url().trim_end_matches('/'), raw_url(&result.raw_base, &self.path)))
    }

    fn build_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.body_dirty = false;
        let Some(result) = self.result.clone() else { return };
        let client = self.ws.read(cx).client.clone();
        let previewing = self.previewing();
        self.body = match &result.preview {
            FilePreview::Image { .. } => {
                let (url, path) = (raw_url(&result.raw_base, &self.path), self.path.clone());
                Body::Image(cx.new(|cx| ImagePane::new(client, url, path, cx)))
            }
            FilePreview::Text { text, .. } if !text.is_empty() => {
                if previewing && is_html(&self.path) {
                    let (Some(url), Some(prefix)) = (self.raw_absolute(cx), self.raw_prefix(cx)) else { return };
                    let pane = cx.new(|cx| HtmlPane::new(url, prefix, window, cx));
                    self.body = Body::Html(pane);
                    self.sync_shown(cx);
                    return;
                } else if previewing && is_markdown(&self.path) {
                    let dir = dir_of(&self.path).to_string();
                    let raw_base = result.raw_base.clone();
                    let on_link = markdown::link_handler(&self.ws, self.project_id.clone(), dir.clone());
                    let text = text.clone();
                    Body::Markdown(cx.new(|cx| MarkdownPane::new(&text, dir, client, move |p| raw_url(&raw_base, p), on_link, cx)))
                } else {
                    let (text, path) = (text.clone(), self.path.clone());
                    Body::Code(cx.new(|cx| CodePane::new(&text, &path, window, cx)))
                }
            }
            _ => Body::None,
        };
    }

    /// HTML 预览的 WebView 是压在画面上的原生视图：这扇窗口不在画布上（换了项目 / 别的窗口
    /// 最大化 / 切到了总览）时，GPUI 不再摆它的位置，得明确藏起来
    fn sync_shown(&mut self, cx: &mut Context<Self>) {
        let Body::Html(pane) = &self.body else { return };
        let key = falcon_core::workspace::FileTabTarget { project_id: self.project_id.clone(), path: self.path.clone() }.key();
        let shown = {
            let ws = self.ws.read(cx);
            let visible = ws.visible_columns();
            let present = falcon_core::layout::find_pane(&visible, &key).is_some();
            let zoomed_away = ws.state.term_zoomed
                && ws.active_key().is_some_and(|a| a != key && falcon_core::layout::find_pane(&visible, &a).is_some());
            present && !zoomed_away && ws.state.active != ActiveView::Overview
        };
        pane.update(cx, |p, cx| p.set_shown(shown, cx));
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let ui = Ui::global(cx).clone();
        let preview = self.result.as_ref().map(|r| &r.preview);
        let renderable = is_markdown(&self.path) || is_html(&self.path);
        let previewing = self.previewing();
        let raw = matches!(preview, Some(FilePreview::Image { .. })) || (is_html(&self.path) && previewing);

        let mut bar = div()
            .h(zpx(34.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr(zpx(6.))
            .border_b_1()
            .border_color(ui.border)
            .child(icon(IconName::FileText).size(zpx(14.)).flex_none().text_color(ui.muted_foreground))
            .child(div().min_w_0().truncate().text_xs().child(self.path.clone()));
        match preview {
            Some(FilePreview::Text { size, truncated: true, .. }) => {
                bar = bar.child(
                    div()
                        .flex_none()
                        .text_size(zpx(11.))
                        .text_color(ui.warning)
                        .child(t!("files.viewTruncated", size = format_bytes(*size)).to_string()),
                );
            }
            Some(FilePreview::Image { size, .. } | FilePreview::Binary { size } | FilePreview::TooLarge { size }) => {
                bar = bar.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(format_bytes(*size)));
            }
            _ => {}
        }
        bar = bar.child(div().flex_1());
        if renderable {
            // 预览 / 源码只对 Markdown 与 HTML 有意义，别的文件类型不摆这两个按钮
            bar = bar
                .child(bar_button("fv-preview", IconName::Eye, t!("files.preview"), previewing, false, &ui).on_click(
                    cx.listener(|this, _, _, cx| this.switch_mode(ViewMode::Preview, cx)),
                ))
                .child(bar_button("fv-source", IconName::CodeXml, t!("files.source"), !previewing, false, &ui).on_click(
                    cx.listener(|this, _, _, cx| this.switch_mode(ViewMode::Source, cx)),
                ));
        }
        if raw {
            // 原始字节地址交给系统浏览器：路由带 CSP sandbox，顶层打开也跑在 opaque origin 里
            bar = bar.child(
                bar_button("fv-open-raw", IconName::ExternalLink, t!("native.files.openInBrowser"), false, false, &ui)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(url) = this.raw_absolute(cx) {
                            cx.open_url(&url);
                        }
                    })),
            );
        }
        bar.child(
            // 二进制 / 超大文件在这里"无法预览"，下载是它们唯一的出路，所以不看预览种类
            bar_button("fv-download", IconName::Download, t!("files.download"), false, false, &ui).on_click(cx.listener(
                |this, _, window, cx| {
                    let (ws, pid, path) = (this.ws.clone(), this.project_id.clone(), this.path.clone());
                    crate::panels::files::transfer::download(ws, pid, vec![path], window, cx);
                },
            )),
        )
        .child(
            bar_button("fv-refresh", IconName::RefreshCw, t!("files.refresh"), false, self.busy, &ui)
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        this.refresh(cx);
                    }
                })),
        )
    }

    fn body(&self, cx: &App) -> AnyElement {
        let ui = Ui::global(cx);
        let hint = |text: String, detail: Option<String>| {
            let mut d = div().px_4().py_4().text_xs().line_height(zpx(20.)).text_color(ui.muted_foreground).child(text);
            if let Some(detail) = detail {
                d = d.child(div().mt_1().text_size(zpx(11.)).child(detail));
            }
            d.into_any_element()
        };
        if let Some(err) = &self.error {
            return hint(t!("files.viewFailed").to_string(), Some(err.clone()));
        }
        let Some(result) = &self.result else {
            return hint(t!("files.viewLoading").to_string(), None);
        };
        match &result.preview {
            // 服务端比客户端新、多了一种预览：退回"不能预览，请下载"
            FilePreview::Binary { .. } | FilePreview::Unknown => hint(t!("files.viewBinary").to_string(), None),
            FilePreview::TooLarge { size } => hint(t!("files.viewTooLarge", size = format_bytes(*size)).to_string(), None),
            FilePreview::Text { text, .. } if text.is_empty() => hint(t!("files.viewEmpty").to_string(), None),
            _ => match &self.body {
                Body::Code(v) => v.clone().into_any_element(),
                Body::Image(v) => v.clone().into_any_element(),
                Body::Markdown(v) => v.clone().into_any_element(),
                Body::Html(v) => v.clone().into_any_element(),
                Body::None => hint(t!("files.viewLoading").to_string(), None),
            },
        }
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.body_dirty {
            self.build_body(window, cx);
        }
        let ui = Ui::global(cx).clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.background)
            .text_color(ui.foreground)
            .child(self.toolbar(cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(self.body(cx)))
    }
}

/// 工具栏上的小图标按钮（React 版的 ghost icon-xs）；按下态用 muted 底，`spinning` 时图标自己转
fn bar_button(id: &'static str, name: IconName, label: impl Into<SharedString>, pressed: bool, spinning: bool, ui: &Ui) -> Stateful<Div> {
    let label: SharedString = label.into();
    let fg = ui.foreground;
    let muted = ui.muted;
    let glyph: AnyElement = if spinning {
        Spinner::new().icon(name).with_size(zpx(14.)).into_any_element()
    } else {
        icon(name).size(zpx(14.)).into_any_element()
    };
    div()
        .id(id)
        .size(zpx(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(6.))
        .cursor_pointer()
        .text_color(if pressed { ui.foreground } else { ui.muted_foreground })
        .when(pressed, |d| d.bg(muted))
        .hover(move |s| s.bg(muted).text_color(fg))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(glyph)
}

fn is_markdown(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with(".md") || p.ends_with(".markdown") || p.ends_with(".mdx")
}

fn is_html(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with(".html") || p.ends_with(".htm") || p.ends_with(".xhtml")
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// React 版 FileView 的 formatBytes（与文件面板的 format_size 不同：只到 MB，一位小数）
fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.)
    } else {
        format!("{:.1} MB", n as f64 / 1024. / 1024.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert!(is_markdown("docs/ADR.MD"));
        assert!(is_markdown("a.mdx"));
        assert!(!is_markdown("md"));
        assert!(is_html("site/index.htm"));
        assert!(!is_html("a.html.txt"));
        assert_eq!(dir_of("a/b/c.md"), "a/b");
        assert_eq!(dir_of("c.md"), "");
    }

    #[test]
    fn bytes_like_web() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
