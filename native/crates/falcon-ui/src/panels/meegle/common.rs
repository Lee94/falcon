//! 飞书项目面板各页共用的东西：面板上下文（客户端、缓存、回到面板的弱引用）、工作项行、
//! 三级分组、翻页、下钻页顶栏、复制菜单与拖拽载荷。对应旧 React 版 `MeeglePanel.tsx` 底部的
//! "小件" 与 `useMeegleCopyMenu.ts`。

use std::collections::HashSet;
use std::future::Future;
use std::rc::Rc;

use falcon_client::{ApiError, ApiResult, FalconClient};
use falcon_core::meegle_cache::MeegleCache;
use falcon_core::meegle_context::format_meegle_context;
use falcon_core::meegle_drag::MeegleWorkItemDragPayload;
use falcon_core::meegle_groups::{ItemGroup, MeegleRow, group_meegle_items, meegle_page};
use falcon_core::meegle_key::meegle_display_key;
use falcon_core::ttl_cache::LoadError;
use falcon_proto::{MeeglePinInput, MeegleWorkItem};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, CursorStyle, Div, ElementId, Entity,
    EventEmitter, FontWeight, IntoElement, Render, ScrollHandle, SharedString, Stateful, WeakEntity,
    Window, div,
};
use rust_i18n::t;

use super::MeeglePanel;
use super::widgets::{icon_button, tiny_button};
use crate::menus::{MenuItemSpec, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;
use falcon_core::maybe_send::MaybeSend;

/// 各页都要的：客户端、这台服务端的前端缓存、回到面板的弱引用（下钻、固定、409 重问状态）
#[derive(Clone)]
pub(super) struct Ctx {
    pub ws: Entity<Workspace>,
    pub client: FalconClient,
    pub panel: WeakEntity<MeeglePanel>,
    pub cache: MeegleCache,
}

impl Ctx {
    /// 业务请求失败的统一出口（React 版的 `handleApiError` + `onUnavailable`）：401 回登录页；
    /// 409 说明 CLI 没装 / 登录态中途没了，让面板重新问一次状态、自己切到对应提示。
    pub fn failed(&self, err: &ApiError, cx: &mut App) {
        self.ws.update(cx, |w, cx| w.handle_error(err, cx));
        if err.is_conflict() {
            let _ = self.panel.update(cx, |p, cx| p.on_unavailable(cx));
        }
    }

    /// 走缓存的那条路失败时：错误被包进了 `anyhow`，认得出是 REST 错误才走上面的出口
    pub fn load_failed(&self, err: &LoadError, cx: &mut App) -> String {
        if let Some(api) = err.downcast_ref::<ApiError>() {
            self.failed(api, cx);
            return api.message.clone();
        }
        err.to_string()
    }

    pub fn toast(&self, kind: ToastKind, title: String, body: Option<String>, cx: &mut App) {
        self.ws.update(cx, |w, cx| w.toast(kind, title, body, cx));
    }

    pub fn open_item(&self, item: &MeegleWorkItem, cx: &mut App) {
        let item = item.clone();
        let _ = self.panel.update(cx, |p, cx| p.open_item(&item, cx));
    }

    pub fn pop(&self, cx: &mut App) {
        let _ = self.panel.update(cx, |p, cx| p.pop(cx));
    }
}

/// 列表 / 详情走前端缓存：命中给缓存，否则并入同 key 在飞的那次，再否则发请求（React 版的
/// `loadMeegleCache(key, () => api.xxx(…), bust)`）。
pub(super) fn cached<T, Fut>(cache: &MeegleCache, key: &str, bust: bool, fut: Fut) -> impl Future<Output = Result<T, LoadError>> + MaybeSend + 'static
where
    T: Clone + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<T>> + MaybeSend + 'static,
{
    cache.load::<T, _, _>(key, move || async move { fut.await.map_err(anyhow::Error::from) }, bust)
}

pub(super) fn is_url(s: &str) -> bool {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && s.len() > if lower.starts_with("https") { 8 } else { 7 }
        && !s.chars().any(char::is_whitespace)
}

/// 飞书给的时间有 ISO（带时区）也有 `YYYY-MM-DD HH:mm`，面板里只看到天
pub(super) fn date_only(s: &str) -> String {
    s.chars().take(10).collect()
}

/// 运行时拼出来的 key（`meegle.action_${a}` 这类）
pub(super) fn tr(key: &str) -> String {
    t!(key).to_string()
}

pub(super) fn untitled(item: &MeegleWorkItem) -> String {
    if item.name.is_empty() { t!("meegle.untitled", id = item.id).to_string() } else { item.name.clone() }
}

// ---------------- 复制菜单（React 版的 useMeegleCopyMenu） ----------------

/// 工作项行 / 工作项固定项 / 详情里编号上的右键菜单
pub(super) fn copy_menu(ctx: &Ctx, space_key: &str, id: &str) -> Vec<MenuItemSpec> {
    let (c1, s1, i1) = (ctx.clone(), space_key.to_string(), id.to_string());
    let (c2, s2, i2) = (ctx.clone(), space_key.to_string(), id.to_string());
    vec![
        MenuItemSpec::new(t!("meegle.copyKey").to_string(), move |_, cx| copy_key(&c1, &s1, &i1, cx)),
        MenuItemSpec::new(t!("meegle.copyContext").to_string(), move |_, cx| copy_context(&c2, &s2, &i2, cx)),
    ]
}

/// 编号要带模板前缀（`g-7105690993`），前缀只有详情里的模板名才认得出，所以先取详情
fn copy_key(ctx: &Ctx, space_key: &str, id: &str, cx: &mut App) {
    let fut = ctx.client.meegle_work_item(space_key, id, false);
    let ctx = ctx.clone();
    cx.spawn(async move |cx| {
        let res = fut.await;
        cx.update(|cx| match res {
            Ok(detail) => {
                cx.write_to_clipboard(ClipboardItem::new_string(meegle_display_key(&detail)));
                ctx.toast(ToastKind::Success, t!("meegle.copyKeyDone").to_string(), None, cx);
            }
            Err(err) => {
                ctx.failed(&err, cx);
                ctx.toast(ToastKind::Danger, t!("meegle.copyFailed").to_string(), None, cx);
            }
        });
    })
    .detach();
}

/// 复制 AI 修复上下文：明确打穿缓存取最新详情（评论 / 附件要全），不拿列表摘要冒充
fn copy_context(ctx: &Ctx, space_key: &str, id: &str, cx: &mut App) {
    ctx.toast(ToastKind::Info, t!("meegle.copyContextLoading").to_string(), None, cx);
    let fut = ctx.client.meegle_work_item(space_key, id, true);
    let ctx = ctx.clone();
    cx.spawn(async move |cx| {
        let res = fut.await;
        cx.update(|cx| match res {
            Ok(detail) => {
                let text = format_meegle_context(&detail, "", |key| t!(key).to_string());
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                ctx.toast(ToastKind::Success, t!("meegle.copyContextDone").to_string(), None, cx);
            }
            Err(err) => {
                ctx.failed(&err, cx);
                ctx.toast(ToastKind::Danger, t!("meegle.copyContextFailed").to_string(), None, cx);
            }
        });
    })
    .detach();
}

// ---------------- 拖拽 ----------------
// React 版的 meegleDrag 走私有 MIME；这里是 GPUI 的类型化载荷

/// 拖动时跟着指针的小标签。落点只认 [`MeegleWorkItemDragPayload`] 这个类型，标签只是给人看的
pub(super) struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx);
        div()
            .max_w(zpx(260.))
            .px_2()
            .py_1()
            .rounded(radius::SM)
            .bg(ui.popover)
            .text_color(ui.popover_foreground)
            .border_1()
            .border_color(ui.border)
            .shadow_md()
            .text_size(zpx(12.))
            .truncate()
            .child(self.label.clone())
    }
}

/// 标识符合法才可拖（它们之后会原样进 CLI 的 argv），拖出去的是 `MeegleWorkItemDragPayload`
pub(super) fn draggable<E: StatefulInteractiveElement>(el: E, space_key: &str, id: &str, label: String) -> E {
    match MeegleWorkItemDragPayload::new(id, space_key) {
        Some(payload) => {
            let label: SharedString = label.into();
            el.on_drag(payload, move |_, _, _, cx| {
                let label = label.clone();
                cx.new(|_| DragPreview { label })
            })
        }
        None => el,
    }
}

// ---------------- 行 ----------------

/// 工作项行（React 版的 `ItemRow`）：两行标题 + 一行 meta；整行可点下钻、可拖、右键复制；
/// hover 时右上角冒出外链
pub(super) fn item_row(ctx: &Ctx, item: &MeegleWorkItem, meta: String, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let name = untitled(item);
    let key = format!("{}/{}/{}", item.space_key, item.type_key, item.id);
    let open_ctx = ctx.clone();
    let open_item = item.clone();
    let tip: SharedString = t!("meegle.dragHint").to_string().into();
    let body = div()
        .id(ElementId::Name(format!("mi-{key}").into()))
        .w_full()
        .pl_3()
        .pr_8()
        .py(zpx(6.))
        .cursor(CursorStyle::OpenHand)
        .hover(|s| s.bg(ui.accent.opacity(0.5)))
        .child(div().text_size(zpx(12.)).line_clamp(2).text_ellipsis().child(name.clone()))
        .when(!meta.is_empty(), |d| {
            d.child(div().mt(zpx(2.)).text_size(zpx(11.)).text_color(ui.muted_foreground).truncate().child(meta))
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |_, _, cx| open_ctx.open_item(&open_item, cx));
    let body = draggable(body, &item.space_key, &item.id, format!("#{} {name}", item.id));
    let menu = copy_menu(ctx, &item.space_key, &item.id);
    let body = body.context_menu(move |m, _, _| to_popup(m, menu.clone()));

    let mut row = div().relative().group("meegle-row").border_b_1().border_color(ui.border).child(body);
    if let Some(url) = item.url.clone() {
        row = row.child(
            div().absolute().top(zpx(6.)).right(zpx(6.)).opacity(0.).group_hover("meegle-row", |s| s.opacity(1.)).child(
                tiny_button(
                    ElementId::Name(format!("mi-ext-{key}").into()),
                    IconName::ExternalLink,
                    t!("meegle.openExternal").to_string(),
                    20.,
                    cx,
                    move |_, _, cx| cx.open_url(&url),
                ),
            ),
        );
    }
    row.into_any_element()
}

// ---------------- 三级分组 ----------------

/// 分组的折叠状态（React 版用 `<details open>`，翻页时整棵树重挂、全部重新展开）
#[derive(Default)]
pub(super) struct Fold {
    closed: HashSet<String>,
}

impl EventEmitter<()> for Fold {}

impl Fold {
    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.closed.remove(key) {
            self.closed.insert(key.to_string());
        }
        cx.notify();
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if !self.closed.is_empty() {
            self.closed.clear();
            cx.notify();
        }
    }
}

const LEVELS: [&str; 3] = ["business", "type", "status"];

/// 业务线 → 工作项类型 → 当前状态（ADR 0010「工作项分组与复制上下文」）
pub(super) fn grouped<T: MeegleRow>(rows: &[T], fold: &Entity<Fold>, row: &dyn Fn(&T) -> AnyElement, cx: &App) -> Vec<AnyElement> {
    let groups = group_meegle_items(rows);
    let closed = fold.read(cx).closed.clone();
    let ui = Ui::global(cx).clone();
    render_nodes(&groups, 0, "", fold, &closed, row, &ui)
}

fn render_nodes<T: MeegleRow>(
    nodes: &[ItemGroup<'_, T>],
    depth: usize,
    path: &str,
    fold: &Entity<Fold>,
    closed: &HashSet<String>,
    row: &dyn Fn(&T) -> AnyElement,
    ui: &Ui,
) -> Vec<AnyElement> {
    let level = LEVELS[depth.min(2)];
    nodes
        .iter()
        .map(|node| {
            let gkey = format!("{path}\u{1f}{}", node.key);
            let open = !closed.contains(&gkey);
            let label = node.label.clone().unwrap_or_else(|| tr(&format!("meegle.unknown_{level}")));
            let (fold2, k2) = (fold.clone(), gkey.clone());
            let summary = div()
                .id(ElementId::Name(format!("mg-{gkey}").into()))
                .flex()
                .items_start()
                .gap_1()
                .px_2()
                .py(zpx(6.))
                .text_size(zpx(12.))
                .cursor_pointer()
                .hover(|s| s.bg(ui.accent.opacity(0.5)))
                .child(
                    div()
                        .flex_none()
                        .pt(zpx(1.))
                        .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(zpx(12.)).text_color(ui.muted_foreground)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .gap_x_1()
                        .child(div().text_color(ui.muted_foreground).child(format!("{} ·", tr(&format!("meegle.group_{level}")))))
                        .child(div().child(label))
                        .child(div().ml_1().text_color(ui.muted_foreground).child(node.count.to_string())),
                )
                .on_click(move |_, _, cx| fold2.update(cx, |f, cx| f.toggle(&k2, cx)));
            let mut el = div().flex().flex_col().border_b_1().border_color(ui.border);
            if depth > 0 {
                el = el.ml_2().border_l_1();
            }
            el = el.child(summary);
            if open {
                if let Some(children) = &node.children {
                    el = el.children(render_nodes(children, depth + 1, &gkey, fold, closed, row, ui));
                } else if let Some(items) = &node.items {
                    el = el.children(items.iter().map(|it| row(it)));
                }
            }
            el.into_any_element()
        })
        .collect()
}

pub(super) type OnPage = Rc<dyn Fn(u32, &mut Window, &mut App)>;

/// 翻页（React 版的 `PageControls`）
pub(super) fn page_controls(id: &str, page: u32, count: usize, total: Option<u64>, has_more: bool, loading: bool, on_page: OnPage, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let mut summary = t!("meegle.pageSummary", page = page, count = count).to_string();
    if let Some(total) = total {
        summary.push_str(&format!(" · {}", t!("meegle.totalItems", count = total)));
    }
    let (prev, next) = (on_page.clone(), on_page);
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_1()
        .px_2()
        .py_2()
        .border_t_1()
        .border_color(ui.border)
        .text_size(zpx(11.))
        .text_color(ui.muted_foreground)
        .child(div().child(summary))
        .child(
            div()
                .flex()
                .gap_1()
                .child(
                    Button::new(ElementId::Name(format!("{id}-prev").into()))
                        .ghost()
                        .small()
                        .label(t!("meegle.previousPage").to_string())
                        .disabled(loading || page <= 1)
                        .on_click(move |_, window, cx| prev(page.saturating_sub(1), window, cx)),
                )
                .child(
                    Button::new(ElementId::Name(format!("{id}-next").into()))
                        .ghost()
                        .small()
                        .label(t!("meegle.nextPage").to_string())
                        .disabled(loading || !has_more)
                        .on_click(move |_, window, cx| next(page + 1, window, cx)),
                ),
        )
}

/// 待办 / 视图两种列表共用的正文：空态、错误、筛不到、翻页（React 版的 `ItemList`）
#[allow(clippy::too_many_arguments)]
pub(super) fn item_list<T: MeegleRow>(
    id: &str,
    items: &[T],
    shown: &[&T],
    loading: bool,
    error: Option<&str>,
    page: u32,
    has_more: bool,
    total: Option<u64>,
    scroll: &ScrollHandle,
    fold: &Entity<Fold>,
    row: &dyn Fn(&T) -> AnyElement,
    on_page: OnPage,
    on_retry: Rc<dyn Fn(&mut Window, &mut App)>,
    cx: &App,
) -> Stateful<Div> {
    use super::widgets::{hint, hint_detail};
    let ui = Ui::global(cx).clone();
    let mut list = div().id(SharedString::from(id.to_string())).flex_1().min_h_0().overflow_y_scroll().track_scroll(scroll);
    list = if let Some(err) = error.filter(|_| items.is_empty()) {
        list.child(hint_detail(t!("meegle.loadFailed").to_string(), err.to_string(), cx))
    } else if loading && items.is_empty() {
        list.child(hint(t!("meegle.loadingList").to_string(), cx))
    } else if items.is_empty() {
        list.child(hint(t!("meegle.empty").to_string(), cx))
    } else if shown.is_empty() {
        list.child(hint(t!("meegle.noResults").to_string(), cx))
    } else {
        let deref = |r: &&T| row(r);
        list.children(grouped(shown, fold, &deref, cx))
    };
    list = list.child(
        div().px_3().py_1().text_size(zpx(11.)).text_color(ui.muted_foreground).child(t!("meegle.groupCurrentPage").to_string()),
    );
    if loading && !items.is_empty() {
        list = list.child(hint(t!("meegle.loadingList").to_string(), cx));
    }
    if let Some(err) = error {
        list = list.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .px_3()
                .py_2()
                .text_size(zpx(12.))
                .text_color(ui.destructive)
                .child(div().max_w_full().child(err.to_string()))
                .child(
                    Button::new(SharedString::from(format!("{id}-retry")))
                        .ghost()
                        .small()
                        .label(t!("meegle.retry").to_string())
                        .disabled(loading)
                        .on_click(move |_, window, cx| on_retry(window, cx)),
                ),
        );
    }
    list.child(page_controls(id, page, shown.len(), total, has_more, loading, on_page, cx))
}

/// 只对已返回结果做本地分页（每页 100 条）再分组：最近列表与搜索结果
/// （React 版的 `LocalGroupedItems`）
pub(super) fn local_grouped<T: MeegleRow>(id: &str, items: &[T], requested: u32, fold: &Entity<Fold>, row: &dyn Fn(&T) -> AnyElement, on_page: OnPage, cx: &App) -> Vec<AnyElement> {
    let ui = Ui::global(cx);
    let current = meegle_page(items, requested as f64);
    let mut out = vec![
        div().px_3().py_1().text_size(zpx(11.)).text_color(ui.muted_foreground).child(t!("meegle.groupReturnedPage").to_string()).into_any_element(),
    ];
    out.extend(grouped(current.items, fold, row, cx));
    out.push(
        page_controls(id, current.page as u32, current.items.len(), Some(items.len() as u64), current.has_more, false, on_page, cx)
            .into_any_element(),
    );
    out
}

// ---------------- 顶栏小件 ----------------

/// 带放大镜与清空键的搜索框（React 版的 `SearchBox`）
pub(super) fn search_box(state: &Entity<InputState>, disabled: bool) -> Input {
    Input::new(state).prefix(icon(IconName::Search).size(zpx(14.))).cleanable(true).disabled(disabled)
}

/// 下钻页顶栏（React 版的 `SubHeader`）：返回、标题、meta、右侧动作
pub(super) fn sub_header(ctx: &Ctx, title: String, meta: Option<String>, actions: Vec<AnyElement>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let back = ctx.clone();
    div()
        .h(zpx(32.))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .border_b_1()
        .border_color(ui.border)
        .child(tiny_button("meegle-back", IconName::ArrowLeft, t!("meegle.back").to_string(), 24., cx, move |_, _, cx| back.pop(cx)))
        .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).font_weight(FontWeight::MEDIUM).child(title))
        .when_some(meta.filter(|m| !m.is_empty()), |d, m| {
            d.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(m))
        })
        .children(actions)
}

/// 下钻页顶栏的图钉：已固定就取消，没固定就固定（React 版的 `PinButton`）
pub(super) fn pin_button(ctx: &Ctx, input: MeeglePinInput, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let panel = ctx.panel.upgrade();
    let (loaded, pinned) = panel
        .as_ref()
        .map(|p| {
            let p = p.read(cx);
            (p.pins.is_some(), p.find_pin(input.kind, &input.space_key, &input.target_id).is_some())
        })
        .unwrap_or((false, false));
    let label = if pinned { t!("meegle.unpin") } else { t!("meegle.pin") }.to_string();
    let weak = ctx.panel.clone();
    let glyph = icon(IconName::Pin).size(zpx(14.)).when(pinned, |i| i.text_color(ui.primary));
    icon_button("meegle-pin", glyph, label, 24., !loaded, cx, move |_, _, cx| {
        let input = input.clone();
        let _ = weak.update(cx, |p, cx| p.toggle_pin(input, cx));
    })
    .into_any_element()
}

/// 顶栏的外链（React 版的 `ExternalIconLink`）
pub(super) fn external_link(id: &str, url: String, cx: &App) -> AnyElement {
    tiny_button(ElementId::Name(id.to_string().into()), IconName::ExternalLink, t!("meegle.openExternal").to_string(), 24., cx, move |_, _, cx| {
        cx.open_url(&url)
    })
    .into_any_element()
}

/// 页头下面那条放分段 / 筛选框的带子（React 版的 `shrink-0 border-b px-2 py-1.5`）
pub(super) fn toolbar(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div().flex_none().flex().flex_col().gap(zpx(6.)).px_2().py(zpx(6.)).border_b_1().border_color(ui.border)
}

