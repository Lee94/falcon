//! 「待办」页（web 的 `TodoSection`）：mywork 的四个列表（待办 / 本周 / 逾期 / 已办），
//! 一页最多 100 条（两个 CLI 50 条页），只对当前页分组、筛选。
//!
//! 缓存的是**最后一次成功的那一页**，不是累加的前缀：翻页失败时可见的条目与页码都不动。

use std::rc::Rc;

use falcon_core::meegle_groups::filter_meegle_items;
use falcon_proto::{MeeglePage, MeegleTodoAction, MeegleTodoItem};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, ScrollHandle, Subscription, Window, div, point, px};
use rust_i18n::t;

use super::common::{Ctx, Fold, date_only, tr, item_list, item_row, search_box, toolbar};
use super::widgets::segmented;

const ACTIONS: [MeegleTodoAction; 4] =
    [MeegleTodoAction::Todo, MeegleTodoAction::ThisWeek, MeegleTodoAction::Overdue, MeegleTodoAction::Done];

pub struct TodoSection {
    ctx: Ctx,
    action: MeegleTodoAction,
    items: Vec<MeegleTodoItem>,
    page: u32,
    has_more: bool,
    total: Option<u64>,
    loading: bool,
    error: Option<String>,
    filter: Entity<InputState>,
    /// 每发一次请求 +1；回来时对不上就是过时的结果（换了分组 / 又翻了一页）
    generation: u64,
    /// 最近一次请求的页码："重试"要重试的是失败的目标页，不是当前显示的那页
    requested: u32,
    scroll: ScrollHandle,
    fold: Entity<Fold>,
    _subs: Vec<Subscription>,
}

impl TodoSection {
    pub(super) fn new(ctx: Ctx, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            action: MeegleTodoAction::Todo,
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
        format!("todo:{}", self.action.as_str())
    }

    fn apply(&mut self, res: MeeglePage<MeegleTodoItem>, cx: &mut Context<Self>) {
        // 页码只在请求成功后改变；加载中或翻页失败时不打断原来的阅读位置
        if res.page != self.page {
            self.scroll.set_offset(point(px(0.), px(0.)));
            self.fold.update(cx, |f, cx| f.reset(cx));
        }
        self.items = res.items;
        self.page = res.page;
        self.has_more = res.has_more;
        self.total = res.total;
    }

    /// 进入一个分组（或刷新）：有缓存先画，30s 内不打网络；`force` 是顶栏刷新
    fn enter(&mut self, force: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let hit = self.ctx.cache.peek::<MeeglePage<MeegleTodoItem>>(&self.cache_key());
        let page = hit.as_ref().map(|h| h.page).unwrap_or(1);
        match hit.clone() {
            Some(h) => {
                self.apply(h, cx);
                self.error = None;
            }
            None => {
                // 列表清空再重拉（刷新 / 没缓存）：web 那边分组树随之卸载，回来时全部展开
                self.fold.update(cx, |f, cx| f.reset(cx));
                self.items.clear();
                self.page = 1;
                self.has_more = false;
                self.total = None;
            }
        }
        if hit.is_some() && !force && !self.ctx.cache.stale(&self.cache_key(), None) {
            self.loading = false;
            cx.notify();
            return;
        }
        self.load(page, force, cx);
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enter(true, cx);
    }

    fn set_action(&mut self, action: MeegleTodoAction, cx: &mut Context<Self>) {
        if self.action == action {
            return;
        }
        self.action = action;
        self.fold.update(cx, |f, cx| f.reset(cx));
        self.enter(false, cx);
    }

    fn load(&mut self, page: u32, fresh: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let my = self.generation;
        self.requested = page;
        self.loading = true;
        self.error = None;
        cx.notify();
        let key = self.cache_key();
        let fut = self.ctx.client.meegle_todo(self.action, page, fresh);
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

/// 待办行的 meta：空间 · 类型 · 节点（或状态）· 截止 / 完成于
fn todo_meta(item: &MeegleTodoItem, action: MeegleTodoAction) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = item.space_name.as_ref().filter(|s| !s.is_empty()) {
        parts.push(s.clone());
    }
    if let Some(s) = item.type_name.as_ref().filter(|s| !s.is_empty()) {
        parts.push(s.clone());
    }
    if let Some(n) = item.node_name.as_ref().filter(|s| !s.is_empty()) {
        parts.push(format!("{} {n}", t!("meegle.node")));
    } else if let Some(s) = item.state_name.as_ref().filter(|s| !s.is_empty()) {
        parts.push(format!("{} {s}", t!("meegle.state")));
    } else if let Some(s) = item.status.as_ref().filter(|s| !s.is_empty()) {
        parts.push(s.clone());
    }
    if action == MeegleTodoAction::Done && item.finished_at.as_ref().is_some_and(|s| !s.is_empty()) {
        parts.push(format!("{} {}", t!("meegle.finishedAt"), item.finished_at.as_deref().unwrap_or("")));
    } else if let Some(end) = item.schedule_end.as_ref().filter(|s| !s.is_empty()) {
        parts.push(format!("{} {}", t!("meegle.due"), date_only(end)));
    }
    parts.join(" · ")
}

impl Render for TodoSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let bar = toolbar(cx)
            .child(segmented(
                "meegle-todo-action",
                self.action,
                ACTIONS.iter().map(|a| (*a, tr(&format!("meegle.action_{}", a.as_str())))).collect(),
                move |a, _, cx| {
                    let _ = this.update(cx, |s, cx| s.set_action(a, cx));
                },
                cx,
            ))
            .child(search_box(&self.filter, false));

        let q = self.filter.read(cx).value().to_string();
        let shown = filter_meegle_items(&self.items, &q);
        let action = self.action;
        let (a, b) = (cx.weak_entity(), cx.weak_entity());
        let ctx = self.ctx.clone();
        let app: &gpui_kit::App = cx;
        let row = |it: &MeegleTodoItem| item_row(&ctx, &it.item, todo_meta(it, action), app);
        let (on_page, on_retry) = {
            (
                Rc::new(move |p: u32, _: &mut Window, cx: &mut gpui_kit::App| {
                    let _ = a.update(cx, |s, cx| s.load(p, false, cx));
                }) as super::common::OnPage,
                Rc::new(move |_: &mut Window, cx: &mut gpui_kit::App| {
                    let _ = b.update(cx, |s, cx| s.load(s.requested, false, cx));
                }) as Rc<dyn Fn(&mut Window, &mut gpui_kit::App)>,
            )
        };
        let list = item_list(
            "meegle-todo-list",
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
        div().flex_1().min_h_0().flex().flex_col().child(bar).child(list)
    }
}
