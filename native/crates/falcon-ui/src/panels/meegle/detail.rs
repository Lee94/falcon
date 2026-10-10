//! 工作项详情页（web 的 `ItemDetail`）：状态 / 优先级 / 编号药丸、基本字段、描述、角色，
//! 底部一键跳去飞书。编号旁的抓手可拖到侧栏源项目上预填派生表单，右键复制 Key / 上下文。
//!
//! 描述是飞书富文本编辑器导出的 Markdown 方言；面板窄，按纯文本保留换行就够看个大概
//! （后端已把图片换成 `[图片]`、注释剥掉）。

use falcon_core::meegle_drill::Drill;
use falcon_proto::{MeeglePinInput, MeeglePinKind, MeegleWorkItemDetail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, CursorStyle, Div, FontWeight, IntoElement, Render, SharedString, Window, div, relative};
use rust_i18n::t;

use super::common::{Ctx, cached, copy_menu, date_only, draggable, external_link, pin_button, sub_header};
use super::widgets::{hint, hint_detail, pill};
use crate::menus::to_popup;
use crate::theme::Ui;
use crate::ui::icon;
use crate::zoom::zpx;

pub struct ItemDetail {
    ctx: Ctx,
    space_key: String,
    space_name: Option<String>,
    type_key: Option<String>,
    url: Option<String>,
    id: String,
    title: String,
    detail: Option<MeegleWorkItemDetail>,
    error: Option<String>,
    generation: u64,
}

impl ItemDetail {
    pub(super) fn new(ctx: Ctx, drill: Drill, cx: &mut Context<Self>) -> Self {
        let Drill::Item { space_key, space_name, type_key, url, id, title } = drill else {
            unreachable!("ItemDetail 只接工作项层");
        };
        let mut this = Self { ctx, space_key, space_name, type_key, url, id, title, detail: None, error: None, generation: 0 };
        this.enter(false, cx);
        this
    }

    fn cache_key(&self) -> String {
        format!("item:{}:{}", self.space_key, self.id)
    }

    fn enter(&mut self, force: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let my = self.generation;
        let key = self.cache_key();
        let hit = self.ctx.cache.peek::<MeegleWorkItemDetail>(&key);
        if hit.is_some() {
            self.error = None;
        }
        self.detail = hit.clone();
        cx.notify();
        if hit.is_some() && !force && !self.ctx.cache.stale(&key, None) {
            return;
        }
        let bust = force || hit.is_some();
        let fut = cached(&self.ctx.cache, &key, bust, self.ctx.client.meegle_work_item(&self.space_key, &self.id, force));
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != my {
                    return;
                }
                match res {
                    Ok(d) => {
                        this.detail = Some(d);
                        this.error = None;
                    }
                    Err(err) => this.error = Some(this.ctx.load_failed(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enter(true, cx);
    }
}

/// 详情里的一行"标签：值"；值空着写「无」
fn field(label: &str, value: impl IntoElement, label_w: f32, ui: &Ui) -> Div {
    div()
        .flex()
        .gap_3()
        .child(div().flex_none().w(zpx(label_w)).text_color(ui.muted_foreground).whitespace_nowrap().child(label.to_string()))
        .child(div().flex_1().min_w_0().child(value))
}

fn text_or_empty(s: String) -> String {
    if s.is_empty() { t!("meegle.d_empty").to_string() } else { s }
}

/// 标签列按最宽的那个量（web 是 `grid-cols-[auto_minmax(0,1fr)]`）：中文 12px 一个字 12px
fn label_width(labels: &[String]) -> f32 {
    labels.iter().map(|l| l.chars().map(|c| if c.is_ascii() { 7.5 } else { 12. }).sum::<f32>()).fold(0., f32::max).ceil() + 2.
}

fn section_title(text: String, ui: &Ui) -> Div {
    div().mt_3().mb_1().text_size(zpx(11.)).text_color(ui.muted_foreground).child(text)
}

impl ItemDetail {
    fn body(&self, d: &MeegleWorkItemDetail, name: &str, url: Option<String>, cx: &App) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let mut pills = div().mt(zpx(6.)).flex().flex_wrap().gap_1();
        if let Some(s) = d.status.as_ref().filter(|s| !s.is_empty()) {
            pills = pills.child(pill(s.clone(), false, cx));
        }
        if let Some(p) = d.priority.as_ref().filter(|s| !s.is_empty()) {
            pills = pills.child(pill(p.clone(), false, cx));
        }
        // 正文要能读，所以只让编号旁的抓手可拖（web 同理：整块详情不设 draggable）
        let tip: SharedString = t!("meegle.dragHint").to_string().into();
        let grip = div()
            .id("meegle-detail-grip")
            .flex()
            .items_center()
            .gap(zpx(2.))
            .cursor(CursorStyle::OpenHand)
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(icon(IconName::GripVertical).size(zpx(12.)).text_color(ui.muted_foreground))
            .child(pill(format!("#{}", d.id), true, cx));
        let grip = draggable(grip, &self.space_key, &self.id, format!("#{} {name}", self.id));
        let menu = copy_menu(&self.ctx, &self.space_key, &self.id);
        pills = pills.child(grip.context_menu(move |m, _, _| to_popup(m, menu.clone())));

        let mut labels = vec![t!("meegle.d_space").to_string(), t!("meegle.d_type").to_string(), t!("meegle.d_createdBy").to_string()];
        if d.template.is_some() {
            labels.push(t!("meegle.d_template").to_string());
        }
        if d.mode.is_some() {
            labels.push(t!("meegle.d_mode").to_string());
        }
        if !d.current_nodes.is_empty() {
            labels.push(t!("meegle.d_currentNode").to_string());
        }
        if !d.operators.is_empty() {
            labels.push(t!("meegle.d_operators").to_string());
        }
        let w = label_width(&labels);
        // 人名之间的顿号（web 是 `join("、")`）
        let sep = t!("native.meegle.listSep").to_string();
        let join = |parts: [Option<String>; 2]| parts.into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");

        let mut dl = div().mt_3().flex().flex_col().gap(zpx(6.)).text_size(zpx(12.));
        dl = dl.child(field(&labels[0], text_or_empty(d.space_name.clone().unwrap_or_default()), w, &ui));
        dl = dl.child(field(&labels[1], text_or_empty(d.type_name.clone().unwrap_or_default()), w, &ui));
        if let Some(tpl) = d.template.clone().filter(|s| !s.is_empty()) {
            dl = dl.child(field(&t!("meegle.d_template"), tpl, w, &ui));
        }
        if let Some(mode) = d.mode.clone().filter(|s| !s.is_empty()) {
            dl = dl.child(field(&t!("meegle.d_mode"), mode, w, &ui));
        }
        if !d.current_nodes.is_empty() {
            let mut nodes = div().flex().flex_col();
            for n in &d.current_nodes {
                let mut line = div().flex().flex_wrap().child(n.name.clone());
                if !n.owners.is_empty() {
                    line = line.child(div().text_color(ui.muted_foreground).child(format!(" · {}", n.owners.join(&sep))));
                }
                nodes = nodes.child(line);
            }
            dl = dl.child(field(&t!("meegle.d_currentNode"), nodes, w, &ui));
        }
        if !d.operators.is_empty() {
            dl = dl.child(field(&t!("meegle.d_operators"), d.operators.join(&sep), w, &ui));
        }
        dl = dl.child(field(
            &t!("meegle.d_createdBy"),
            text_or_empty(join([d.created_by.clone(), d.created_at.as_deref().map(date_only)])),
            w,
            &ui,
        ));
        dl = dl.child(field(
            &t!("meegle.d_updatedBy"),
            text_or_empty(join([d.updated_by.clone(), d.updated_at.as_deref().map(date_only)])),
            w,
            &ui,
        ));

        let mut body = div()
            .px_3()
            .py(zpx(10.))
            .flex()
            .flex_col()
            .child(div().text_size(zpx(13.)).font_weight(FontWeight::MEDIUM).line_height(relative(1.375)).child(name.to_string()))
            .child(pills)
            .child(dl);
        if let Some(desc) = d.description.clone().filter(|s| !s.is_empty()) {
            body = body
                .child(section_title(t!("meegle.d_description").to_string(), &ui))
                .child(div().text_size(zpx(12.)).line_height(relative(1.6)).child(desc));
        }
        if !d.roles.is_empty() {
            let rw = label_width(&d.roles.iter().map(|r| r.name.clone()).collect::<Vec<_>>());
            let mut roles = div().flex().flex_col().gap_1().text_size(zpx(12.));
            for r in &d.roles {
                roles = roles.child(field(&r.name, text_or_empty(r.members.join(&sep)), rw, &ui));
            }
            body = body.child(section_title(t!("meegle.d_roles").to_string(), &ui)).child(roles);
        }
        if let Some(url) = url {
            body = body.child(
                div().mt_4().child(
                    Button::new("meegle-detail-open")
                        .outline()
                        .w_full()
                        .icon(IconName::ExternalLink)
                        .label(t!("meegle.openExternal").to_string())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            );
        }
        body.into_any_element()
    }
}

impl Render for ItemDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app: &App = cx;
        let name = self
            .detail
            .as_ref()
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| Some(self.title.clone()).filter(|t| !t.is_empty()))
            .unwrap_or_else(|| t!("meegle.untitled", id = self.id).to_string());
        let url = self.detail.as_ref().and_then(|d| d.url.clone()).or_else(|| self.url.clone());
        let pin = MeeglePinInput {
            kind: MeeglePinKind::WorkItem,
            space_key: self.space_key.clone(),
            space_name: self.detail.as_ref().and_then(|d| d.space_name.clone()).or_else(|| self.space_name.clone()),
            target_id: self.id.clone(),
            type_key: self.detail.as_ref().map(|d| d.type_key.clone()).filter(|k| !k.is_empty()).or_else(|| self.type_key.clone()),
            label: name.clone(),
            url: url.clone(),
        };
        let mut actions = vec![pin_button(&self.ctx, pin, app)];
        if let Some(url) = url.clone() {
            actions.push(external_link("meegle-detail-ext", url, app));
        }
        let meta = self.detail.as_ref().and_then(|d| d.type_name.clone());
        let header = sub_header(&self.ctx, name.clone(), meta, actions, app);

        let content: AnyElement = if let Some(err) = &self.error {
            hint_detail(t!("meegle.detailFailed").to_string(), err.clone(), app).into_any_element()
        } else if let Some(d) = &self.detail {
            self.body(d, &name, url, app)
        } else {
            hint(t!("meegle.loadingList").to_string(), app).into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(div().id("meegle-detail-scroll").flex_1().min_h_0().overflow_y_scroll().child(content))
    }
}
