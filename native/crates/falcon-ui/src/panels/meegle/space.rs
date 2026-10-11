//! 「空间」页（旧 React 版的 `SpaceSection`）：选一个空间，按关键字搜视图与工作项。
//!
//! - CLI 没有"列出全部视图"的接口，视图只能搜（`view search` 关键字必填、按租户限 5 qps，
//!   所以输入框防抖 350ms 再发）；
//! - 有关键字就两路并行搜视图与工作项（某个类型失败只记一条错误，其余照常出结果）；没关键字
//!   但选了类型就列该类型最近 30 条；都没有就只给提示；
//! - 贴进来的是飞书项目链接就直接打开，不当关键字搜。
//!
//! 空间列表只有"最近访问过的"（CLI 的限制）；记住的空间不在里面就退回第一个。

use std::rc::Rc;
use std::time::Duration;

use falcon_proto::{MeegleSearchResult, MeegleSpace, MeegleWorkItem, MeegleWorkItemType};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, IntoElement, Render, ScrollHandle, SharedString, Subscription, Task,
    Window, div, point, px,
};
use rust_i18n::t;

use super::common::{Ctx, Fold, OnPage, cached, date_only, is_url, item_row, local_grouped, search_box, toolbar};
use super::widgets::{group_title, hint, hint_detail};
use super::pref_key;
use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::zoom::zpx;

const SPACE_KEY: &str = "falcon.meegle.space";
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);

/// 空间下拉的一项：名字 + 等宽的 simple_name
#[derive(Clone)]
struct SpaceOption {
    key: SharedString,
    name: SharedString,
    simple: SharedString,
}

impl SearchableListItem for SpaceOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let ui = Ui::global(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(div().truncate().child(self.name.clone()))
            .child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(self.simple.clone()))
    }

    fn value(&self) -> &SharedString {
        &self.key
    }

    fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.name.to_lowercase().contains(&q) || self.simple.to_lowercase().contains(&q)
    }
}

type SpaceSelect = SelectState<SearchableVec<SpaceOption>>;

pub struct SpaceSection {
    ctx: Ctx,
    spaces: Option<Vec<MeegleSpace>>,
    spaces_error: Option<String>,
    space_key: String,
    select: Entity<SpaceSelect>,
    /// 空间列表 / 选中项变了，下一帧（拿得到 Window 时）同步给下拉
    select_dirty: bool,
    types: Option<Vec<MeegleWorkItemType>>,
    type_key: String,
    query: Entity<InputState>,
    /// 防抖后的关键字（已 trim）
    keyword: String,
    /// 链接打开成功后清空输入框（同样要等拿得到 Window）
    clear_query: bool,
    debounce: Option<Task<()>>,
    result: Option<MeegleSearchResult>,
    recent: Option<Vec<MeegleWorkItem>>,
    loading: bool,
    error: Option<String>,
    search_gen: u64,
    spaces_gen: u64,
    types_gen: u64,
    /// 搜索结果 / 最近列表的本地分页
    local_page: u32,
    scroll: ScrollHandle,
    fold: Entity<Fold>,
    _subs: Vec<Subscription>,
}

impl SpaceSection {
    pub(super) fn new(ctx: Ctx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<SpaceOption>::new()), None, window, cx));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("meegle.searchPlaceholder").to_string()));
        let fold = cx.new(|_| Fold::default());
        let space_key = Prefs::global(cx).get(&pref_key(&ctx.ws, SPACE_KEY, cx)).unwrap_or("").to_string();
        let subs = vec![
            cx.subscribe(&select, |this, _, ev: &SelectEvent<SearchableVec<SpaceOption>>, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else { return };
                if v.as_ref() != this.space_key {
                    this.space_key = v.to_string();
                    this.on_space_changed(cx);
                }
            }),
            cx.subscribe(&query, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.schedule_search(cx);
                    cx.notify();
                }
            }),
            cx.observe(&fold, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            ctx,
            spaces: None,
            spaces_error: None,
            space_key,
            select,
            select_dirty: true,
            types: None,
            type_key: String::new(),
            query,
            keyword: String::new(),
            clear_query: false,
            debounce: None,
            result: None,
            recent: None,
            loading: false,
            error: None,
            search_gen: 0,
            spaces_gen: 0,
            types_gen: 0,
            local_page: 1,
            scroll: ScrollHandle::new(),
            fold,
            _subs: subs,
        };
        this.spaces = this.ctx.cache.peek::<Vec<MeegleSpace>>("spaces");
        this.load_types(false, cx);
        this.load_spaces(false, cx);
        this
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_spaces(true, cx);
        self.load_types(true, cx);
        self.run_search(true, cx);
    }

    // ---------------- 空间 ----------------

    fn load_spaces(&mut self, force: bool, cx: &mut Context<Self>) {
        let hit = self.ctx.cache.peek::<Vec<MeegleSpace>>("spaces");
        if let Some(h) = hit.clone() {
            self.spaces = Some(h);
            self.fix_space_key(cx);
        }
        if hit.is_some() && !force && !self.ctx.cache.stale("spaces", None) {
            return;
        }
        let bust = force || hit.is_some();
        self.spaces_gen += 1;
        let my = self.spaces_gen;
        let fut = cached(&self.ctx.cache, "spaces", bust, self.ctx.client.meegle_spaces(None, force));
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if this.spaces_gen != my {
                    return;
                }
                match res {
                    Ok(list) => {
                        this.spaces = Some(list);
                        this.spaces_error = None;
                    }
                    Err(err) => {
                        this.spaces_error = Some(this.ctx.load_failed(&err, cx));
                        this.spaces = Some(Vec::new());
                    }
                }
                this.fix_space_key(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 记住的空间不在最近列表里就退回第一个
    fn fix_space_key(&mut self, cx: &mut Context<Self>) {
        self.select_dirty = true;
        let Some(spaces) = &self.spaces else { return };
        if spaces.iter().any(|s| s.key == self.space_key) {
            cx.notify();
            return;
        }
        let next = spaces.first().map(|s| s.key.clone()).unwrap_or_default();
        if next != self.space_key {
            self.space_key = next;
            self.on_space_changed(cx);
        }
    }

    fn on_space_changed(&mut self, cx: &mut Context<Self>) {
        self.type_key.clear();
        self.select_dirty = true;
        self.load_types(false, cx);
        self.run_search(false, cx);
    }

    // ---------------- 类型 ----------------

    fn load_types(&mut self, force: bool, cx: &mut Context<Self>) {
        self.types_gen += 1;
        if self.space_key.is_empty() {
            self.types = None;
            cx.notify();
            return;
        }
        let pk = pref_key(&self.ctx.ws, SPACE_KEY, cx);
        cx.global_mut::<Prefs>().set(&pk, self.space_key.clone());
        let key = format!("types:{}", self.space_key);
        let hit = self.ctx.cache.peek::<Vec<MeegleWorkItemType>>(&key);
        self.types = hit.as_ref().map(|h| h.iter().filter(|t| !t.disabled).cloned().collect());
        cx.notify();
        if hit.is_some() && !force && !self.ctx.cache.stale(&key, None) {
            return;
        }
        let bust = force || hit.is_some();
        let my = self.types_gen;
        let fut = cached(&self.ctx.cache, &key, bust, self.ctx.client.meegle_types(&self.space_key, force));
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if this.types_gen != my {
                    return;
                }
                match res {
                    Ok(list) => this.types = Some(list.into_iter().filter(|t| !t.disabled).collect()),
                    Err(err) => {
                        this.ctx.load_failed(&err, cx);
                        this.types = Some(Vec::new());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_type(&mut self, key: String, cx: &mut Context<Self>) {
        if self.type_key != key {
            self.type_key = key;
            self.run_search(false, cx);
        }
    }

    // ---------------- 搜索 ----------------

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                let kw = this.query.read(cx).value().trim().to_string();
                if kw != this.keyword {
                    this.keyword = kw;
                    this.run_search(false, cx);
                }
            })
            .ok();
        }));
    }

    fn show_result(&mut self, result: Option<MeegleSearchResult>, recent: Option<Vec<MeegleWorkItem>>, cx: &mut Context<Self>) {
        self.result = result;
        self.recent = recent;
        self.local_page = 1;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.fold.update(cx, |f, cx| f.reset(cx));
    }

    /// 贴进来的是链接就直接开；其余：有关键字就搜，没关键字但选了类型就看最近
    fn run_search(&mut self, force: bool, cx: &mut Context<Self>) {
        self.search_gen += 1;
        let my = self.search_gen;
        let keyword = self.keyword.clone();
        cx.notify();

        if is_url(&keyword) {
            self.show_result(None, None, cx);
            self.error = None;
            self.loading = true;
            let Ok(task) = self.ctx.panel.update(cx, |p, cx| p.open_url(keyword, cx)) else { return };
            cx.spawn(async move |this, cx| {
                let err = task.await;
                this.update(cx, |this, cx| {
                    if this.search_gen != my {
                        return;
                    }
                    this.loading = false;
                    match err {
                        Some(e) => this.error = Some(e),
                        None => {
                            this.clear_query = true;
                            this.keyword.clear();
                            this.run_search(false, cx);
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }

        if self.space_key.is_empty() || (keyword.is_empty() && self.type_key.is_empty()) {
            self.show_result(None, None, cx);
            self.error = None;
            self.loading = false;
            return;
        }

        let space = self.space_key.clone();
        if !keyword.is_empty() {
            let key = format!("search:{space}:{keyword}:{}", self.type_key);
            let hit = self.ctx.cache.peek::<MeegleSearchResult>(&key);
            if hit.is_some() {
                self.error = None;
            }
            self.show_result(hit.clone(), None, cx);
            if hit.is_some() && !force && !self.ctx.cache.stale(&key, None) {
                self.loading = false;
                return;
            }
            let bust = force || hit.is_some();
            self.loading = true;
            let ty = Some(self.type_key.as_str()).filter(|t| !t.is_empty());
            let fut = cached(&self.ctx.cache, &key, bust, self.ctx.client.meegle_search(&space, &keyword, ty, force));
            cx.spawn(async move |this, cx| {
                let res = fut.await;
                this.update(cx, |this, cx| {
                    if this.search_gen != my {
                        return;
                    }
                    this.loading = false;
                    match res {
                        Ok(r) => this.show_result(Some(r), None, cx),
                        Err(err) => this.error = Some(this.ctx.load_failed(&err, cx)),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }

        let key = format!("recent:{space}:{}", self.type_key);
        let hit = self.ctx.cache.peek::<Vec<MeegleWorkItem>>(&key);
        if hit.is_some() {
            self.error = None;
        }
        self.show_result(None, hit.clone(), cx);
        if hit.is_some() && !force && !self.ctx.cache.stale(&key, None) {
            self.loading = false;
            return;
        }
        let bust = force || hit.is_some();
        self.loading = true;
        let fut = cached(&self.ctx.cache, &key, bust, self.ctx.client.meegle_recent(&space, &self.type_key, force));
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if this.search_gen != my {
                    return;
                }
                this.loading = false;
                match res {
                    Ok(r) => this.show_result(None, Some(r), cx),
                    Err(err) => this.error = Some(this.ctx.load_failed(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_local_page(&mut self, page: u32, cx: &mut Context<Self>) {
        self.local_page = page;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.fold.update(cx, |f, cx| f.reset(cx));
        cx.notify();
    }

    // ---------------- 画 ----------------

    fn sync_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_dirty {
            self.select_dirty = false;
            let options: Vec<SpaceOption> = self
                .spaces
                .iter()
                .flatten()
                .map(|s| SpaceOption { key: s.key.clone().into(), name: s.name.clone().into(), simple: s.simple_name.clone().into() })
                .collect();
            let key: SharedString = self.space_key.clone().into();
            self.select.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(options), window, cx);
                if key.is_empty() {
                    s.set_selected_index(None, window, cx);
                } else {
                    s.set_selected_value(&key, window, cx);
                }
            });
        }
        if self.clear_query {
            self.clear_query = false;
            self.query.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    fn type_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let types = self.types.as_ref().filter(|t| !t.is_empty())?;
        let ui = Ui::global(cx).clone();
        let mut chips = div().flex().flex_wrap().gap_1();
        let all = std::iter::once((String::new(), t!("meegle.typeAll").to_string()));
        for (key, name) in all.chain(types.iter().map(|t| (t.key.clone(), t.name.clone()))) {
            let on = self.type_key == key;
            let k2 = key.clone();
            let mut chip = div()
                .id(ElementId::Name(format!("meegle-type-{key}").into()))
                .h(zpx(20.))
                .px_2()
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .text_size(zpx(11.))
                .whitespace_nowrap()
                .cursor_pointer()
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| this.set_type(k2.clone(), cx)));
            chip = if on {
                chip.border_color(ui.primary).bg(ui.primary).text_color(ui.primary_foreground)
            } else {
                chip.border_color(ui.border)
                    .text_color(ui.muted_foreground)
                    .hover(|s| s.bg(ui.accent).text_color(ui.accent_foreground))
            };
            chips = chips.child(chip);
        }
        Some(chips.into_any_element())
    }

    fn results(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let this = cx.weak_entity();
        let on_page: OnPage = Rc::new(move |p, _, cx| {
            let _ = this.update(cx, |s, cx| s.set_local_page(p, cx));
        });
        let panel = self.ctx.panel.clone();
        let space_key = self.space_key.clone();
        let space_name = self.spaces.iter().flatten().find(|s| s.key == self.space_key).map(|s| s.name.clone());
        let ui = Ui::global(cx).clone();
        let app: &App = cx;
        let ctx = self.ctx.clone();

        if self.spaces.as_ref().is_some_and(|s| s.is_empty()) {
            let text = self.spaces_error.clone().unwrap_or_else(|| t!("meegle.noSpaces").to_string());
            return vec![hint(text, app).into_any_element()];
        }
        if self.space_key.is_empty() {
            return vec![hint(t!("meegle.loadingList").to_string(), app).into_any_element()];
        }
        if let Some(err) = self.error.as_ref().filter(|_| self.result.is_none() && self.recent.is_none()) {
            return vec![hint_detail(t!("meegle.loadFailed").to_string(), err.clone(), app).into_any_element()];
        }
        if let Some(result) = &self.result {
            let mut out = Vec::new();
            if result.views.is_empty() && result.items.is_empty() && result.errors.is_empty() {
                out.push(hint(t!("meegle.noResults").to_string(), app).into_any_element());
            }
            if !result.views.is_empty() {
                out.push(
                    group_title(app)
                        .child(t!("meegle.views").to_string())
                        .child(div().opacity(0.7).child(result.views.len().to_string()))
                        .into_any_element(),
                );
                for v in &result.views {
                    let (panel, space_key, space_name, view) = (panel.clone(), space_key.clone(), space_name.clone(), v.clone());
                    out.push(
                        div()
                            .id(ElementId::Name(format!("meegle-view-{}/{}", v.type_key, v.id).into()))
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .px_3()
                            .py(zpx(6.))
                            .border_b_1()
                            .border_color(ui.border)
                            .cursor_pointer()
                            .hover(|s| s.bg(ui.accent.opacity(0.5)))
                            .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).child(v.name.clone()))
                            .child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(v.type_name.clone()))
                            .on_click(move |_, _, cx| {
                                let _ = panel.update(cx, |p, cx| p.open_view(&space_key, &view, space_name.clone(), cx));
                            })
                            .into_any_element(),
                    );
                }
            }
            if !result.items.is_empty() {
                out.push(
                    group_title(app)
                        .child(t!("meegle.items").to_string())
                        .child(div().opacity(0.7).child(result.items.len().to_string()))
                        .into_any_element(),
                );
                let row = |it: &MeegleWorkItem| {
                    let meta: Vec<String> = [it.type_name.clone(), it.status.clone(), it.updated_at.as_deref().map(date_only)]
                        .into_iter()
                        .flatten()
                        .filter(|s| !s.is_empty())
                        .collect();
                    item_row(&ctx, it, meta.join(" · "), app)
                };
                out.extend(local_grouped("meegle-search-items", &result.items, self.local_page, &self.fold, &row, on_page, app));
            }
            if !result.errors.is_empty() {
                let mut errs = div()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .text_size(zpx(11.))
                    .text_color(ui.destructive)
                    .child(t!("meegle.partialFailed").to_string());
                for e in &result.errors {
                    errs = errs.child(div().truncate().child(e.clone()));
                }
                out.push(errs.into_any_element());
            }
            return out;
        }
        if let Some(recent) = &self.recent {
            let type_name = self.types.iter().flatten().find(|t| t.key == self.type_key).map(|t| t.name.clone()).unwrap_or_default();
            let mut out = vec![group_title(app).child(t!("meegle.recentOf", "type" => type_name).to_string()).into_any_element()];
            if recent.is_empty() {
                out.push(hint(t!("meegle.empty").to_string(), app).into_any_element());
            } else {
                let row = |it: &MeegleWorkItem| {
                    let meta: Vec<String> =
                        [it.status.clone(), it.updated_at.as_deref().map(date_only)].into_iter().flatten().filter(|s| !s.is_empty()).collect();
                    item_row(&ctx, it, meta.join(" · "), app)
                };
                out.extend(local_grouped("meegle-recent-items", recent, self.local_page, &self.fold, &row, on_page, app));
            }
            return out;
        }
        let text = if self.loading {
            if is_url(&self.keyword) { t!("meegle.resolving") } else { t!("meegle.searching") }
        } else if space_name.is_some() {
            t!("meegle.searchHint")
        } else {
            t!("meegle.loadingList")
        };
        vec![hint(text.to_string(), app).into_any_element()]
    }
}

impl Render for SpaceSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_widgets(window, cx);
        let placeholder = if self.spaces.is_some() { t!("meegle.spacePlaceholder") } else { t!("meegle.loadingList") };
        let disabled = self.spaces.as_ref().is_none_or(|s| s.is_empty());
        let chips = self.type_chips(cx);
        let bar = toolbar(cx)
            .child(Select::new(&self.select).w_full().placeholder(placeholder.to_string()).disabled(disabled))
            .child(search_box(&self.query, self.space_key.is_empty()))
            .children(chips);
        let results = self.results(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(div().id("meegle-space-results").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).children(results))
    }
}
