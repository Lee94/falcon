//! 会话总览（web 的 `components/SessionOverview.tsx`）：主区在 ActiveView::Overview 时盖在
//! 画布上。舰队视图——先看健康度（三张状态卡兼作筛选器），再按需下钻到某一台机器上的某一个会话。
//!
//! 筛选 / 勾选状态在 Workspace 上（`overview_filter` / `overview_project` / `selected`）：
//! 项目菜单的"在总览中筛选"要能从外面把总览设到某个项目上。改了这几个字段要 `cx.notify()`，
//! 与 web 的 setFilter / setProjectFilter 一样，换筛选清空勾选。
//!
//! 与 web 的差别：web 的行本身不可点，只能点"打开"；这里点行（不在勾选框 / 按钮上）也打开
//! 那个会话——已丢失的除外，它只剩一条记录。空闲时间的 hover（web 用 toLocaleString 给
//! 绝对时间）没做：app 没有带时区的日期库。

use falcon_core::reason::durability_hint;
use falcon_proto::{SessionState, SessionWithProject};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, Context, Entity, FontWeight, IntoElement, Render, SharedString, Window, div,
};
use rust_i18n::t;

use crate::labels::{idle_text, local_word, session_label};
use crate::menus::{self, to_popup};
use crate::theme::{Ui, radius};
use crate::ui::{Mark, icon, status_mark};
use crate::workspace::Workspace;
use crate::zoom::zpx;

const CARD_STATES: [SessionState; 3] = [SessionState::Active, SessionState::Unverified, SessionState::Dead];

// 表格列宽（web 的 w-9 / w-23 / w-30 / w-22 / w-33），会话与位置两列平分剩下的
const COL_CHECK: f32 = 36.;
const COL_STATE: f32 = 92.;
const COL_DURABLE: f32 = 120.;
const COL_IDLE: f32 = 88.;
const COL_ACTIONS: f32 = 132.;

fn state_label(state: SessionState) -> String {
    Mark::from(state).label()
}

/// falcon-core 的翻译回调：`t(key, {k: v})`。rust-i18n 的参数名要编译期写死，这里按
/// `%{k}` 手工替换
fn tr(key: &str, params: &[(&str, &str)]) -> String {
    let mut s = t!(key).to_string();
    for (k, v) in params {
        s = s.replace(&format!("%{{{k}}}"), v);
    }
    s
}

pub struct SessionOverview {
    ws: Entity<Workspace>,
}

impl SessionOverview {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        Self { ws }
    }

    fn set_filter(&self, filter: Option<SessionState>, cx: &mut Context<Self>) {
        self.ws.update(cx, |w, cx| {
            w.overview_filter = filter;
            w.selected.clear();
            cx.notify();
        });
    }

    fn reset_filter(&self, cx: &mut Context<Self>) {
        self.ws.update(cx, |w, cx| {
            w.overview_filter = None;
            w.overview_project = None;
            w.selected.clear();
            cx.notify();
        });
    }

    fn render_cards(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let filter = w.overview_filter;
        let mut cards = div().mb_5().flex().gap_3();
        for state in CARD_STATES {
            let count = w.sessions.iter().filter(|s| s.state == state).count();
            let on = filter == Some(state);
            let hover = ui.tint.opacity(0.6);
            cards = cards.child(
                crate::ui::sunken(cx)
                    .id(SharedString::from(format!("overview-card-{}", state.as_str())))
                    .w(zpx(176.))
                    .px(zpx(14.))
                    .py_3()
                    .cursor_pointer()
                    .map(|d| if on { d.bg(ui.tint) } else { d.hover(move |s| s.bg(hover)) })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_filter(if on { None } else { Some(state) }, cx)
                    }))
                    .child(
                        div()
                            .mb(zpx(6.))
                            .flex()
                            .items_center()
                            .gap(zpx(6.))
                            .text_xs()
                            .text_color(ui.muted_foreground)
                            .child(status_mark(Mark::from(state), zpx(12.), cx))
                            .child(state_label(state)),
                    )
                    .child(
                        div()
                            .font_family(crate::fonts::BERKELEY)
                            .text_size(zpx(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(count.to_string()),
                    ),
            );
        }
        cards.into_any_element()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let project = w.overview_project.as_deref().and_then(|id| w.project(id)).map(|p| p.name.clone());
        let mut badge = match w.overview_filter {
            None => t!("overview.filterAll").to_string(),
            Some(state) => state_label(state),
        };
        if let Some(name) = project {
            badge = format!("{badge} · {name}");
        }
        let selected = w.selected.clone();
        let ws = self.ws.clone();
        let mut bar = div()
            .mb(zpx(10.))
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("overview.filter").to_string()))
            .child(
                div()
                    .h(zpx(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(zpx(6.))
                    .bg(ui.secondary)
                    .text_color(ui.secondary_foreground)
                    .text_xs()
                    .child(badge),
            )
            .child(
                Button::new("overview-all")
                    .ghost()
                    .small()
                    .label(t!("overview.all").to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.reset_filter(cx))),
            )
            .child(div().flex_1());
        if !selected.is_empty() {
            let ws = ws.clone();
            let n = selected.len();
            bar = bar.child(
                Button::new("overview-terminate")
                    .danger()
                    .small()
                    .label(t!("overview.terminateSelected", n = n).to_string())
                    .on_click(move |_, window, cx| menus::terminate_many(&ws, selected.clone(), window, cx)),
            );
        }
        bar.child(
            Button::new("overview-clear-dead")
                .secondary()
                .small()
                .label(t!("overview.clearAllDead").to_string())
                .on_click(move |_, _, cx| menus::clear_all_dead(&ws, cx)),
        )
        .into_any_element()
    }

    fn render_table(&self, rows: &[SessionWithProject], cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let all_selected = !rows.is_empty() && rows.iter().all(|r| w.selected.contains(&r.id));
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let head = |text: String| div().child(text);
        let header = div()
            .h(zpx(40.))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .border_b_1()
            .border_color(ui.border)
            .child(
                div().w(zpx(COL_CHECK)).flex_none().child(
                    Checkbox::new("overview-select-all")
                        .checked(all_selected)
                        .tooltip(t!("overview.selectAll").to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let ids = ids.clone();
                            this.ws.update(cx, |w, cx| {
                                w.selected = if all_selected { Vec::new() } else { ids };
                                cx.notify();
                            });
                        })),
                ),
            )
            .child(head(t!("overview.state").to_string()).w(zpx(COL_STATE)).flex_none())
            .child(head(t!("overview.session").to_string()).flex_1().min_w_0())
            .child(head(t!("overview.where").to_string()).flex_1().min_w_0())
            .child(head(t!("overview.durability").to_string()).w(zpx(COL_DURABLE)).flex_none())
            .child(head(t!("overview.idle").to_string()).w(zpx(COL_IDLE)).flex_none())
            .child(div().w(zpx(COL_ACTIONS)).flex_none());
        let n = rows.len();
        let picked = w.selected.clone();
        let mut table = crate::ui::sunken(cx).min_w(zpx(800.)).px_2().flex().flex_col().child(header);
        for (i, session) in rows.iter().enumerate() {
            table = table.child(self.render_row(session, picked.contains(&session.id), i + 1 < n, cx));
        }
        table.into_any_element()
    }

    fn render_row(&self, session: &SessionWithProject, selected: bool, sep: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let label = session_label(session, &w.projects);
        let host = falcon_core::host_color::host_label(w.project(&session.project_id), &local_word());
        let dead = session.state == SessionState::Dead;
        let id = session.id.clone();
        let items = menus::session_menu_items(&self.ws, session, cx);
        let ctx_items = items.clone();

        // 按状态给一个主操作，其余进 ⋯ ——一行铺四个按钮时危险操作也被平铺了
        let ws = self.ws.clone();
        let sid = id.clone();
        let primary = match session.state {
            SessionState::Unverified => Button::new(SharedString::from(format!("ov-primary-{id}")))
                .warning()
                .small()
                .label(t!("session.reattach").to_string())
                .on_click(move |_, _, cx| menus::reattach(&ws, &sid, cx)),
            SessionState::Dead => Button::new(SharedString::from(format!("ov-primary-{id}")))
                .ghost()
                .small()
                .label(t!("session.clear").to_string())
                .on_click(move |_, _, cx| menus::clear_dead(&ws, &sid, cx)),
            _ => Button::new(SharedString::from(format!("ov-primary-{id}")))
                .outline()
                .small()
                .label(t!("session.open").to_string())
                .on_click(move |_, _, cx| ws.update(cx, |w, cx| w.open_session(&sid, cx))),
        };

        let hint = durability_hint(&tr, session.durable, session.non_durable_reason, None);
        let durable = div()
            .id(SharedString::from(format!("ov-durable-{id}")))
            .h(zpx(24.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded(radius::SM)
            .text_xs()
            .map(|d| {
                if session.durable {
                    d.text_color(ui.muted_foreground).child(icon(IconName::ShieldCheck).size(zpx(14.)))
                } else {
                    d.bg(ui.warning.opacity(0.1)).text_color(ui.warning).child(icon(IconName::TriangleAlert).size(zpx(14.)))
                }
            })
            .child(if session.durable { t!("session.durable") } else { t!("session.nonDurable") }.to_string())
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx));

        let toggle_id = id.clone();
        let open_id = id.clone();
        let label_tip = label.clone();
        div()
            .id(SharedString::from(format!("ov-row-{id}")))
            .min_h(zpx(48.))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(radius::SM)
            .when(sep, |d| d.border_b_1().border_color(ui.border.opacity(0.5)))
            .hover(|s| s.bg(ui.background.opacity(0.7)))
            .when(!dead, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    this.ws.update(cx, |w, cx| w.open_session(&open_id, cx));
                }))
            })
            .child(
                // 勾选框吃掉点击：点它是勾选，不是打开
                div()
                    .id(SharedString::from(format!("ov-check-wrap-{id}")))
                    .w(zpx(COL_CHECK))
                    .flex_none()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(SharedString::from(format!("ov-check-{id}")))
                            .checked(selected)
                            .tooltip(t!("overview.selectOne").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let id = toggle_id.clone();
                                this.ws.update(cx, |w, cx| {
                                    if let Some(at) = w.selected.iter().position(|x| *x == id) {
                                        w.selected.remove(at);
                                    } else {
                                        w.selected.push(id);
                                    }
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .child(
                div()
                    .w(zpx(COL_STATE))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(zpx(6.))
                    .text_xs()
                    .text_color(ui.muted_foreground)
                    .child(status_mark(Mark::from(session.state), zpx(12.), cx))
                    .child(state_label(session.state)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("ov-name-{id}")))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .when(dead, |d| d.text_color(ui.muted_foreground).line_through())
                    .tooltip(move |window, cx| Tooltip::new(label_tip.clone()).build(window, cx))
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_sm()
                    .text_color(ui.muted_foreground)
                    .child(div().min_w_0().truncate().child(session.project_name.clone()))
                    // 分隔点用透明度而不是 border 色：后者在浅色下是实色浅灰，白底上看不见
                    .child(div().flex_none().text_color(ui.muted_foreground.opacity(0.4)).child("·"))
                    .child(div().flex_none().font_family(crate::fonts::BERKELEY).text_xs().child(host)),
            )
            .child(div().w(zpx(COL_DURABLE)).flex_none().flex().child(durable))
            .child(div().w(zpx(COL_IDLE)).flex_none().text_xs().text_color(ui.muted_foreground).child(idle_text(session.last_active_at)))
            .child(
                div()
                    .w(zpx(COL_ACTIONS))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .gap(zpx(6.))
                    .child(primary)
                    .child(
                        Button::new(SharedString::from(format!("ov-menu-{id}")))
                            .ghost()
                            .small()
                            .icon(IconName::Ellipsis)
                            .tooltip(t!("session.moreActions").to_string())
                            // 按钮贴着右边：菜单右对齐往左展开，别溢出窗口（web 的 align end）
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| to_popup(menu, items.clone())),
                    ),
            )
            .context_menu(move |menu, _, _| to_popup(menu, ctx_items.clone()))
            .into_any_element()
    }

    /// 空状态不是"没有会话"这四个字：一句说明 + 一个主操作
    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let has_projects = !w.projects.is_empty();
        let has_sessions = !w.sessions.is_empty();
        let title = if !has_projects {
            t!("overview.emptyNoProjectTitle")
        } else if has_sessions {
            t!("overview.emptyFilteredTitle")
        } else {
            t!("overview.emptyNoSessionTitle")
        };
        let body = if !has_projects { t!("overview.emptyNoProjectBody") } else { t!("overview.emptyNoSessionBody") };
        let mut out = div()
            .py_12()
            .flex()
            .flex_col()
            .items_center()
            .gap(zpx(10.))
            .text_color(ui.muted_foreground)
            .child(icon(IconName::Terminal).size(zpx(28.)).text_color(ui.muted_foreground.opacity(0.5)))
            .child(div().text_size(zpx(15.)).text_color(ui.foreground).child(title.to_string()))
            .child(div().max_w(zpx(460.)).text_center().text_size(zpx(12.5)).line_height(zpx(20.)).child(body.to_string()));
        if !has_projects {
            let ws = self.ws.clone();
            out = out.child(
                div().mt(zpx(6.)).child(
                    Button::new("overview-new-project")
                        .primary()
                        .label(t!("sidebar.newProject").to_string())
                        .on_click(move |_, window, cx| {
                            crate::dialogs::project_form::open(&ws, None, Default::default(), window, cx)
                        }),
                ),
            );
        } else if has_sessions {
            out = out.child(
                div().mt(zpx(6.)).child(
                    Button::new("overview-empty-all")
                        .outline()
                        .label(t!("overview.all").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.reset_filter(cx))),
                ),
            );
        }
        out.into_any_element()
    }
}

impl Render for SessionOverview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let w = self.ws.read(cx);
        let filter = w.overview_filter;
        let project = w.overview_project.clone();
        let rows: Vec<SessionWithProject> = w
            .sessions
            .iter()
            .filter(|s| filter.is_none_or(|f| s.state == f) && project.as_ref().is_none_or(|p| &s.project_id == p))
            .cloned()
            .collect();
        let content = if rows.is_empty() { self.render_empty(cx) } else { self.render_table(&rows, cx) };
        crate::ui::island(cx).size_full().child(
            div()
                .id("session-overview")
                .size_full()
                .overflow_y_scroll()
                .px_6()
                .pt_6()
                .pb_8()
                .child(
                    div()
                        .mb_4()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("overview.title").to_string()),
                )
                .child(self.render_cards(cx))
                .child(self.render_toolbar(cx))
                .child(content),
        )
    }
}
