//! 画布上的文件查看窗口（旧 React 版的 components/FileView.tsx，ADR 0007）。
//!
//! 目标由排布给（canvas 调 [`FileView::set_target`]）而不是跟着焦点走：文件窗口是列里的一扇，
//! 焦点在别处时它照样要显示自己的内容。打开 / 换文件 / 手动刷新时拉一次，不轮询——正看着的
//! 文件在眼皮底下换掉，比看到旧内容更让人困惑（要新的按刷新就是了）。
//!
//! 能原样写回的文本（服务端给了内容版本：没截断、是合法 UTF-8）在源码视图里可以直接改，
//! ⌘S / Ctrl+S 或工具栏的保存按钮写回去（`PUT …/file`，带着打开时的版本）。只做"顺手改两行"
//! 的分量：没有自动保存、没有 LSP，大改还是终端里的编辑器。几条规矩：
//! - 编辑器常驻在 [`FileView::code`] 上，切到预览不扔（Markdown 预览照着没存的内容渲染，HTML
//!   预览加载的是磁盘上的，工具栏上会说）；换文件 / 刷新才扔；
//! - 有没存的修改时，换文件、关窗口、刷新都先确认（拦在 [`Workspace::open_file`] /
//!   [`Workspace::close_file`] 上，`file_dirty` 就是给它们看的）；改名跟过去不丢修改；
//! - 磁盘上的文件在这期间被别人改了（终端里的编辑器、git、agent），服务端回 409，问过用户才覆盖；
//! - 换行风格与末尾换行跟原文件走（falcon-core 的 `file_text`）。
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

use std::rc::Rc;

use falcon_client::raw_url;
use falcon_core::file_text::{self, TextShape};
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

use crate::actions::file_view::Save;
use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{ActiveView, ConfirmSpec, ToastKind, Workspace};

use self::code::{CodePane, CodePaneEvent};
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

/// 能编辑的文件：打开时（或上一次保存后）磁盘上是什么。脏标记与保存都对着它
struct EditBase {
    /// 编辑器里的初始内容（磁盘上的文本吞掉末尾换行之后，`file_text::for_editor`）
    body: String,
    shape: TextShape,
    /// 内容版本，保存时带回去让服务端比对
    version: String,
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
    /// 文本的编辑器：第一次显示源码时建，切到预览也留着——没存的修改在它身上
    code: Option<Entity<CodePane>>,
    _code_sub: Option<Subscription>,
    /// 能编辑时才有（服务端给了内容版本）
    base: Option<EditBase>,
    /// 编辑器里的内容与 `base.body` 不同
    dirty: bool,
    saving: bool,
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
            code: None,
            _code_sub: None,
            base: None,
            dirty: false,
            saving: false,
            _subs: subs,
        };
        this.load(cx);
        this
    }

    /// 同一扇窗口换内容（预览语义：点另一个文件不新开窗口）。没存的修改在这之前已经由
    /// [`Workspace::open_file`] 问过用户了
    pub fn set_target(&mut self, project_id: String, path: String, cx: &mut Context<Self>) {
        self.project_id = project_id;
        self.path = path;
        self.load(cx);
    }

    /// 文件改了名（文件面板里改的，[`Workspace::follow_file_rename`]）：内容没变，有没存的修改
    /// 就留着、只换路径（版本照旧，保存写到新路径上）；没有就照常重读
    pub fn rename_to(&mut self, path: String, cx: &mut Context<Self>) {
        self.path = path;
        if !self.dirty {
            self.load(cx);
            return;
        }
        // 预览凭据可能只授权旧文件（文件位于盘符根时），不能仅改 URL 的末段。重取 raw 元数据，
        // 不替换 preview / 编辑器 / 内容版本，没存的修改和保存冲突判定都留着。
        let path = self.path.clone();
        let generation = self.generation;
        let fut = self.ws.read(cx).client.read_file(&self.project_id, &path);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.path != path || this.generation != generation {
                    return;
                }
                if let (Ok(next), Some(current)) = (result, this.result.as_mut()) {
                    current.raw_base = next.raw_base;
                    current.raw_path = next.raw_path;
                    this.body = Body::None;
                    this.body_dirty = true;
                    cx.notify();
                }
            }).ok();
        }).detach();
        self.body = Body::None;
        self.body_dirty = true;
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.result = None;
        self.error = None;
        self.body = Body::None;
        self.body_dirty = false;
        self.code = None;
        self._code_sub = None;
        self.base = None;
        self.saving = false;
        self.set_dirty(false, cx);
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
                        if let FilePreview::Text { text, version: Some(version), .. } = &file.preview {
                            let (body, shape) = file_text::for_editor(text);
                            this.base = Some(EditBase { body: body.to_string(), shape, version: version.clone() });
                        }
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
        if self.dirty {
            let view = cx.weak_entity();
            self.confirm_discard(Rc::new(move |_, cx| {
                view.update(cx, |v, cx| {
                    v.busy = true;
                    v.load(cx);
                })
                .ok();
            }), cx);
            return;
        }
        self.busy = true;
        self.load(cx);
    }

    fn confirm_discard(&self, on_confirm: Rc<dyn Fn(&mut Workspace, &mut Context<Workspace>)>, cx: &mut Context<Self>) {
        let name = file_name(&self.path).to_string();
        self.ws.update(cx, |w, cx| w.confirm(discard_spec(&name, on_confirm), cx));
    }

    /// 脏标记变了才动：工作区上那一份是给"换文件 / 关窗口前确认"看的，不触发重画
    fn set_dirty(&mut self, dirty: bool, cx: &mut Context<Self>) {
        if self.dirty == dirty {
            return;
        }
        self.dirty = dirty;
        self.ws.update(cx, |w, _| w.set_file_dirty(dirty));
        cx.notify();
    }

    fn recheck_dirty(&mut self, cx: &mut Context<Self>) {
        let dirty = match (&self.code, &self.base) {
            (Some(code), Some(base)) => !code.read(cx).is(&base.body, cx),
            _ => false,
        };
        self.set_dirty(dirty, cx);
    }

    /// 写回去。`overwrite` = 用户看过冲突提示、决定覆盖：不带版本，服务端不比对
    fn save(&mut self, overwrite: bool, cx: &mut Context<Self>) {
        if self.saving || !(self.dirty || overwrite) {
            return;
        }
        let (Some(code), Some(base)) = (&self.code, &self.base) else { return };
        let body = code.read(cx).text(cx);
        let disk = file_text::for_disk(&body, base.shape);
        let version = (!overwrite).then(|| base.version.clone());
        let fut = self.ws.read(cx).client.save_file(&self.project_id, &self.path, disk, version.as_deref());
        self.saving = true;
        let generation = self.generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.saving = false;
                match result {
                    Ok(res) => {
                        if let Some(base) = &mut this.base {
                            base.body = body;
                            base.version = res.version;
                        }
                        // 存的这一会儿里又敲了字的，照样算没存
                        this.recheck_dirty(cx);
                        this.ws.update(cx, |w, cx| w.refresh_changes(cx));
                    }
                    Err(err) if err.is_conflict() => this.confirm_overwrite(err.message, cx),
                    Err(err) => this.ws.update(cx, |w, cx| {
                        w.handle_error(&err, cx);
                        w.toast(ToastKind::Danger, t!("files.saveFailed").to_string(), Some(err.to_string()), cx);
                    }),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 磁盘上的文件在打开之后变了（或没了）：覆盖要用户点头。不覆盖就什么也不做——修改还在
    /// 编辑器里，复制走之后刷新就能看到别人改成了什么
    fn confirm_overwrite(&self, reason: String, cx: &mut Context<Self>) {
        let view = cx.weak_entity();
        let name = file_name(&self.path).to_string();
        self.ws.update(cx, |w, cx| {
            w.confirm(
                ConfirmSpec {
                    title: t!("files.saveConflictTitle", name = name).to_string(),
                    body: t!("files.saveConflictBody", reason = reason).to_string(),
                    footnote: None,
                    confirm_label: t!("files.saveOverwrite").to_string(),
                    danger: true,
                    on_confirm: Rc::new(move |_, cx| {
                        view.update(cx, |v, cx| v.save(true, cx)).ok();
                    }),
                },
                cx,
            )
        });
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
        Some(format!("{}{}", self.ws.read(cx).client.base_url().trim_end_matches('/'), raw_url(&result.raw_base, result.raw_path.as_deref().unwrap_or(&self.path))))
    }

    fn build_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.body_dirty = false;
        let Some(result) = self.result.clone() else { return };
        let client = self.ws.read(cx).client.clone();
        let previewing = self.previewing();
        self.body = match &result.preview {
            FilePreview::Image { .. } => {
                let (url, path) = (raw_url(&result.raw_base, result.raw_path.as_deref().unwrap_or(&self.path)), self.path.clone());
                Body::Image(cx.new(|cx| ImagePane::new(client, url, path, cx)))
            }
            // 空文件的预览没什么可看的（body() 里说"这是一个空文件"）；源码视图照样给编辑器，好往里写
            FilePreview::Text { text, .. } if previewing && text.is_empty() && self.code.is_none() => Body::None,
            FilePreview::Text { .. } if previewing && is_html(&self.path) => {
                // 加载的是磁盘上的字节：没存的修改看不到（工具栏上有一句提示）
                let (Some(url), Some(prefix)) = (self.raw_absolute(cx), self.raw_prefix(cx)) else { return };
                Body::Html(cx.new(|cx| HtmlPane::new(url, prefix, window, cx)))
            }
            FilePreview::Text { text, .. } if previewing && is_markdown(&self.path) => {
                let dir = dir_of(result.raw_path.as_deref().unwrap_or(&self.path)).to_string();
                let raw_base = result.raw_base.clone();
                // 图片相对 raw 令牌的目录解析，点击链接仍用宿主机路径打开，不能把不同成员的
                // README.md 都变成同一个项目相对路径。
                let on_link = markdown::link_handler(&self.ws, self.project_id.clone(), dir_of(&self.path).to_string());
                // 编辑过的照着编辑器里的渲染：改完切过来看效果，不必先存
                let text = match &self.code {
                    Some(code) => code.read(cx).text(cx),
                    None => text.clone(),
                };
                Body::Markdown(cx.new(|cx| MarkdownPane::new(&text, dir, client, move |p| raw_url(&raw_base, p), on_link, cx)))
            }
            FilePreview::Text { text, .. } => Body::Code(self.ensure_code(text, window, cx)),
            _ => Body::None,
        };
        self.sync_shown(cx);
    }

    fn ensure_code(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<CodePane> {
        if let Some(code) = &self.code {
            return code.clone();
        }
        let editable = self.base.is_some();
        let (body, _) = file_text::for_editor(text);
        let path = self.path.clone();
        let code = cx.new(|cx| CodePane::new(body, &path, editable, window, cx));
        self._code_sub = Some(cx.subscribe(&code, |this, _, _: &CodePaneEvent, cx| this.recheck_dirty(cx)));
        self.code = Some(code.clone());
        code
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
        let fg = ui.foreground;

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
            .child(div().min_w_0().truncate().text_xs().child(self.path.clone()))
            // 有没存的修改：路径后面一个实心点（各家编辑器的标签页都这么标）
            .when(self.dirty, |b| b.child(div().flex_none().size(zpx(6.)).rounded_full().bg(fg)));
        let note = |text: String| div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(text);
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
                bar = bar.child(note(format_bytes(*size)));
            }
            // 不是合法 UTF-8（显示的是替换过的 U+FFFD）：存回去会改坏原字节，只读
            Some(FilePreview::Text { text, version: None, truncated: false, .. }) if text.contains('\u{FFFD}') => {
                bar = bar.child(note(t!("files.readOnlyNotUtf8").to_string()));
            }
            _ => {}
        }
        if self.dirty && previewing && is_html(&self.path) {
            bar = bar.child(note(t!("files.previewSaved").to_string()));
        }
        bar = bar.child(div().flex_1());
        if self.dirty || self.saving {
            bar = bar.child(
                bar_button("fv-save", IconName::Save, t!("files.save"), false, self.saving, &ui)
                    .on_click(cx.listener(|this, _, _, cx| this.save(false, cx))),
            );
        }
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
            FilePreview::Text { text, .. } if text.is_empty() && self.previewing() && self.code.is_none() => {
                hint(t!("files.viewEmpty").to_string(), None)
            }
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
            .key_context("FileView")
            .on_action(cx.listener(|this, _: &Save, _, cx| this.save(false, cx)))
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
    match path.rfind('/') {
        Some(0) => "/",
        Some(2) if path.as_bytes().get(1) == Some(&b':') => &path[..3],
        Some(i) => &path[..i],
        None => "",
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 有没存的修改时要离开（换文件 / 关窗口 / 刷新）的确认
pub(crate) fn discard_spec(name: &str, on_confirm: Rc<dyn Fn(&mut Workspace, &mut Context<Workspace>)>) -> ConfirmSpec {
    ConfirmSpec {
        title: t!("files.discardTitle").to_string(),
        body: t!("files.discardBody", name = name).to_string(),
        footnote: None,
        confirm_label: t!("files.discardConfirm").to_string(),
        danger: true,
        on_confirm,
    }
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
        assert_eq!(dir_of("/c.md"), "/");
        assert_eq!(dir_of("C:/c.md"), "C:/");
        assert_eq!(file_name("a/b/c.md"), "c.md");
        assert_eq!(file_name("c.md"), "c.md");
    }

    #[test]
    fn bytes_like_web() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
