//! 「固定」页（web 的 `PinsSection` / `PinRow`）：顶上一个链接输入框（粘贴完整链接即开，
//! 省一次回车），下面是固定过的视图 / 全景视图 / 工作项。
//!
//! 飞书自己的「快速访问」CLI 与 OpenAPI 都没开出来，所以让用户把常用的东西钉在 Falcon 里；
//! 列表存在后端 SQLite（登录态是整台机器一份，固定列表也跟着机器走）。名字可改——从链接
//! 打开的视图拿不到名字，默认就是 id。

use falcon_proto::{MeeglePin, MeeglePinKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, Entity, IntoElement, Render, ScrollHandle, SharedString, Subscription,
    Window, div,
};
use rust_i18n::t;

use super::common::{Ctx, copy_menu, draggable, is_url, toolbar, tr};
use super::widgets::{hint, tiny_button};
use crate::menus::to_popup;
use crate::theme::Ui;
use crate::ui::icon;
use crate::zoom::zpx;

pub struct PinsSection {
    ctx: Ctx,
    url: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    /// 打开成功后清空输入框（要等拿得到 Window）
    clear_url: bool,
    /// 正在就地改名的那一项
    editing: Option<(String, Entity<InputState>)>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
    _edit_subs: Vec<Subscription>,
}

impl PinsSection {
    pub(super) fn new(ctx: Ctx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(t!("meegle.urlPlaceholder").to_string()));
        let subs = vec![cx.subscribe(&url, |this, input, ev: &InputEvent, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let raw = input.read(cx).value().to_string();
                this.open(raw, cx);
            }
            InputEvent::Change => cx.notify(),
            _ => {}
        })];
        Self { ctx, url, busy: false, error: None, clear_url: false, editing: None, scroll: ScrollHandle::new(), _subs: subs, _edit_subs: Vec::new() }
    }

    fn open(&mut self, raw: String, cx: &mut Context<Self>) {
        let u = raw.trim().to_string();
        if u.is_empty() || self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let Ok(task) = self.ctx.panel.update(cx, |p, cx| p.open_url(u, cx)) else { return };
        cx.spawn(async move |this, cx| {
            let err = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match err {
                    Some(e) => this.error = Some(e),
                    None => this.clear_url = true,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_edit(&mut self, pin: &MeeglePin, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("meegle.renamePlaceholder").to_string()).default_value(pin.label.clone())
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self._edit_subs = vec![cx.subscribe(&input, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.save_edit(cx),
            _ => {}
        })];
        self.editing = Some((pin.id.clone(), input));
        cx.notify();
    }

    /// 回车 / 失焦保存；空名或没改就当取消
    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some((id, input)) = self.editing.take() else { return };
        self._edit_subs.clear();
        let label = input.read(cx).value().trim().to_string();
        let old = self.ctx.panel.upgrade().and_then(|p| p.read(cx).pins.iter().flatten().find(|p| p.id == id).map(|p| p.label.clone()));
        if !label.is_empty() && Some(&label) != old.as_ref() {
            let _ = self.ctx.panel.update(cx, |p, cx| p.rename_pin(id, label, cx));
        }
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self._edit_subs.clear();
        cx.notify();
    }

    fn pin_row(&self, pin: &MeeglePin, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let icon_name = match pin.kind {
            MeeglePinKind::View => IconName::Table2,
            MeeglePinKind::MultiProjectView => IconName::Layers,
            _ => IconName::FileText,
        };
        let row_icon = icon(icon_name).size(zpx(14.)).text_color(ui.muted_foreground);

        if let Some((_, input)) = self.editing.as_ref().filter(|(id, _)| *id == pin.id) {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py(zpx(6.))
                .border_b_1()
                .border_color(ui.border)
                .child(row_icon)
                .child(
                    div()
                        .flex_1()
                        .on_action(cx.listener(|this, _: &Escape, _, cx| this.cancel_edit(cx)))
                        .child(Input::new(input).small()),
                )
                .into_any_element();
        }

        let kind_label = tr(&format!("meegle.kind_{}", pin.kind.as_str()));
        let meta: Vec<String> = [Some(kind_label), pin.space_name.clone()].into_iter().flatten().filter(|s| !s.is_empty()).collect();
        let is_item = pin.kind == MeeglePinKind::WorkItem;
        let open_pin = pin.clone();
        let panel = self.ctx.panel.clone();
        let mut body = div()
            .id(ElementId::Name(format!("meegle-pin-{}", pin.id).into()))
            .flex()
            .items_start()
            .gap_2()
            .pl_3()
            .pr(zpx(80.))
            .py(zpx(6.))
            .cursor(if is_item { CursorStyle::OpenHand } else { CursorStyle::PointingHand })
            .hover(|s| s.bg(ui.accent.opacity(0.5)))
            .child(div().pt(zpx(2.)).child(row_icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(zpx(12.)).line_clamp(2).text_ellipsis().child(pin.label.clone()))
                    .child(div().text_size(zpx(11.)).text_color(ui.muted_foreground).truncate().child(meta.join(" · "))),
            )
            .on_click(move |_, _, cx| {
                let _ = panel.update(cx, |p, cx| p.open_pin(&open_pin, cx));
            });
        if is_item {
            let tip: SharedString = t!("meegle.dragHint").to_string().into();
            body = body.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
            body = draggable(body, &pin.space_key, &pin.target_id, format!("#{} {}", pin.target_id, pin.label));
        }
        let body: AnyElement = if is_item {
            let menu = copy_menu(&self.ctx, &pin.space_key, &pin.target_id);
            body.context_menu(move |m, _, _| to_popup(m, menu.clone())).into_any_element()
        } else {
            body.into_any_element()
        };

        let edit_pin = pin.clone();
        let remove_id = pin.id.clone();
        let panel = self.ctx.panel.clone();
        let mut actions = div()
            .absolute()
            .top(zpx(4.))
            .right(zpx(4.))
            .flex()
            .gap(zpx(2.))
            .opacity(0.)
            .group_hover("meegle-pin-row", |s| s.opacity(1.))
            .child(tiny_button(
                ElementId::Name(format!("meegle-pin-rename-{}", pin.id).into()),
                IconName::Pencil,
                t!("meegle.rename").to_string(),
                20.,
                cx,
                cx.listener(move |this, _, window, cx| this.start_edit(&edit_pin, window, cx)),
            ));
        if let Some(url) = pin.url.clone() {
            actions = actions.child(tiny_button(
                ElementId::Name(format!("meegle-pin-ext-{}", pin.id).into()),
                IconName::ExternalLink,
                t!("meegle.openExternal").to_string(),
                20.,
                cx,
                move |_, _, cx| cx.open_url(&url),
            ));
        }
        actions = actions.child(tiny_button(
            ElementId::Name(format!("meegle-pin-remove-{}", pin.id).into()),
            IconName::PinOff,
            t!("meegle.unpin").to_string(),
            20.,
            cx,
            move |_, _, cx| {
                let _ = panel.update(cx, |p, cx| p.remove_pin(remove_id.clone(), cx));
            },
        ));
        div().relative().group("meegle-pin-row").border_b_1().border_color(ui.border).child(body).child(actions).into_any_element()
    }
}

impl Render for PinsSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_url {
            self.clear_url = false;
            self.url.update(cx, |s, cx| s.set_value("", window, cx));
        }
        let ui = Ui::global(cx).clone();
        let this = cx.weak_entity();
        let url_state = self.url.clone();
        // 粘贴进来的是完整链接就直接开，省一次回车
        let input = Input::new(&self.url).on_paste(move |item, window, cx| {
            let Some(text) = item.text().map(|t| t.trim().to_string()).filter(|t| is_url(t)) else { return false };
            url_state.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            let _ = this.update(cx, |s, cx| s.open(text, cx));
            true
        });
        let empty = self.url.read(cx).value().trim().is_empty();
        let form = toolbar(cx)
            .child(
                div().flex().gap_1().child(div().flex_1().min_w_0().child(input)).child(
                    Button::new("meegle-open-url")
                        .primary()
                        .loading(self.busy)
                        .disabled(self.busy || empty)
                        .label(if self.busy { t!("meegle.resolving") } else { t!("meegle.open") }.to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let raw = this.url.read(cx).value().to_string();
                            this.open(raw, cx);
                        })),
                ),
            )
            .when_some(self.error.clone(), |d, e| d.child(div().text_size(zpx(11.)).text_color(ui.destructive).child(e)));

        let pins = self.ctx.panel.upgrade().and_then(|p| p.read(cx).pins.clone());
        let list: Vec<AnyElement> = match pins {
            None => vec![hint(t!("meegle.loadingList").to_string(), cx).into_any_element()],
            Some(list) if list.is_empty() => vec![hint(t!("meegle.pinsEmpty").to_string(), cx).into_any_element()],
            Some(list) => list.iter().map(|p| self.pin_row(p, cx)).collect(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(form)
            .child(div().id("meegle-pins-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).children(list))
    }
}
