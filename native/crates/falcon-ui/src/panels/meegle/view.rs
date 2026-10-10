//! 视图下钻页（web 的 `ViewItems`）：普通视图与全景视图共用一页，只是取数接口不同。
//!
//! 顶上是筛选框，底下是分组树。飞书那边视图左侧的分类树与分组 CLI 一条都不给，所以这里的
//! 层级是就手上的条目自己推的（ADR 0010）；全景视图跨空间，行上多带空间与类型。

use std::rc::Rc;

use falcon_core::meegle_drill::Drill;
use falcon_core::meegle_groups::filter_meegle_items;
use falcon_proto::{MeeglePage, MeeglePinInput, MeeglePinKind, MeegleWorkItem};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, ScrollHandle, Subscription, Window, div, point, px};
use rust_i18n::t;

use super::common::{Ctx, Fold, OnPage, date_only, external_link, item_list, item_row, pin_button, search_box, sub_header, toolbar};

pub struct ViewItems {
    ctx: Ctx,
    space_key: String,
    space_name: Option<String>,
    type_key: Option<String>,
    type_name: Option<String>,
    url: Option<String>,
    view_id: String,
    label: String,
    multi: bool,
    items: Vec<MeegleWorkItem>,
    page: u32,
    has_more: bool,
    total: Option<u64>,
    loading: bool,
    error: Option<String>,
    filter: Entity<InputState>,
    generation: u64,
    requested: u32,
    scroll: ScrollHandle,
    fold: Entity<Fold>,
    _subs: Vec<Subscription>,
}

impl ViewItems {
    pub(super) fn new(ctx: Ctx, drill: Drill, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Drill::View { space_key, space_name, type_key, url, view_id, label, multi, type_name } = drill else {
            unreachable!("ViewItems 只接视图层");
        };
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("meegle.filterPagePlaceholder").to_string()));
        let fold = cx.new(|_| Fold::default());
        let subs = vec![
            cx.subscribe(&filter, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe(&fold, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            ctx,
            space_key,
            space_name,
            type_key,
            type_name,
            url,
            view_id,
            label,
            multi,
            items: Vec::new(),
            page: 1,
            has_more: false,
            total: None,
            loading: true,
            error: None,
            filter,
            generation: 0,
            requested: 1,
            scroll: ScrollHandle::new(),
            fold,
            _subs: subs,
        };
        this.enter(false, cx);
        this
    }

    fn cache_key(&self) -> String {
        format!("{}:{}:{}", if self.multi { "mview" } else { "view" }, self.space_key, self.view_id)
    }

    fn apply(&mut self, res: MeeglePage<MeegleWorkItem>, cx: &mut Context<Self>) {
        if res.page != self.page {
            self.scroll.set_offset(point(px(0.), px(0.)));
            self.fold.update(cx, |f, cx| f.reset(cx));
        }
        self.items = res.items;
        self.page = res.page;
        self.has_more = res.has_more;
        self.total = res.total;
    }

    fn enter(&mut self, force: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let key = self.cache_key();
        let hit = self.ctx.cache.peek::<MeeglePage<MeegleWorkItem>>(&key);
        let page = hit.as_ref().map(|h| h.page).unwrap_or(1);
        self.error = None;
        match hit.clone() {
            Some(h) => self.apply(h, cx),
            None => {
                // 列表清空再重拉（刷新 / 没缓存）：web 那边分组树随之卸载，回来时全部展开
                self.fold.update(cx, |f, cx| f.reset(cx));
                self.items.clear();
                self.page = 1;
                self.has_more = false;
                self.total = None;
            }
        }
        if hit.is_some() && !force && !self.ctx.cache.stale(&key, None) {
            self.loading = false;
            cx.notify();
            return;
        }
        self.load(page, force, cx);
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enter(true, cx);
    }

    fn load(&mut self, page: u32, fresh: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let my = self.generation;
        self.requested = page;
        self.loading = true;
        self.error = None;
        cx.notify();
        let key = self.cache_key();
        let client = &self.ctx.client;
        let fut = if self.multi {
            futures::future::Either::Left(client.meegle_multi_view_items(&self.space_key, &self.view_id, page, fresh))
        } else {
            futures::future::Either::Right(client.meegle_view_items(&self.space_key, &self.view_id, page, fresh))
        };
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != my {
                    return;
                }
                this.loading = false;
                match res {
                    Ok(res) => {
                        this.ctx.cache.write(&key, res.clone());
                        this.apply(res, cx);
                    }
                    Err(err) => {
                        this.ctx.failed(&err, cx);
                        this.error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ViewItems {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app: &App = cx;
        let meta = self.type_name.clone().or_else(|| self.multi.then(|| t!("meegle.kind_multiProjectView").to_string()));
        let pin = MeeglePinInput {
            kind: if self.multi { MeeglePinKind::MultiProjectView } else { MeeglePinKind::View },
            space_key: self.space_key.clone(),
            space_name: self.space_name.clone(),
            target_id: self.view_id.clone(),
            type_key: self.type_key.clone(),
            label: self.label.clone(),
            url: self.url.clone(),
        };
        let mut actions = vec![pin_button(&self.ctx, pin, app)];
        if let Some(url) = self.url.clone() {
            actions.push(external_link("meegle-view-ext", url, app));
        }
        let header = sub_header(&self.ctx, self.label.clone(), meta, actions, app);

        let q = self.filter.read(app).value().to_string();
        let shown = filter_meegle_items(&self.items, &q);
        let multi = self.multi;
        let ctx = self.ctx.clone();
        let row = |it: &MeegleWorkItem| {
            let parts: Vec<String> = [
                it.space_name.clone().filter(|_| multi),
                it.type_name.clone().filter(|_| multi),
                it.status.clone(),
                it.updated_at.as_deref().map(date_only),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
            item_row(&ctx, it, parts.join(" · "), app)
        };
        let (a, b) = (cx.weak_entity(), cx.weak_entity());
        let on_page: OnPage = Rc::new(move |p, _, cx| {
            let _ = a.update(cx, |s, cx| s.load(p, false, cx));
        });
        let on_retry: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
            let _ = b.update(cx, |s, cx| s.load(s.requested, false, cx));
        });
        let list = item_list(
            "meegle-view-list",
            &self.items,
            &shown,
            self.loading,
            self.error.as_deref(),
            self.page,
            self.has_more,
            self.total,
            &self.scroll,
            &self.fold,
            &row,
            on_page,
            on_retry,
            app,
        );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(toolbar(app).child(search_box(&self.filter, false)))
            .child(list)
    }
}
