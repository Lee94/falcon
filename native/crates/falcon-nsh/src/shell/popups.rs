use std::{cell::{Cell, RefCell}, rc::Rc};

use crate::{
    fonts::FONT_FAMILY_MONO,
    shell::action_owner::ActionOwner,
    store::WorkspaceStore,
    theme::{rgb_to_hsla, rgba_to_hsla},
    zoom::zpx,
};
use falcon_theme::ResolvedTheme;
use nsh_core::prelude::*;
use nsh_core::element::desktop::MenuItem;

/// Binds popup actions to the connection that produced their resource IDs.
#[derive(Clone)]
pub(crate) struct ServerUrlGuard {
    store: WorkspaceStore,
    origin_url: String,
    connection_epoch: u64,
}

impl ServerUrlGuard {
    pub(crate) fn capture(store: &WorkspaceStore) -> Self {
        let origin_url = normalized_server_url(&store.server_url.get()).to_owned();
        let connection_epoch = store.connection_generation();
        Self {
            store: store.clone(),
            origin_url,
            connection_epoch,
        }
    }

    pub(crate) fn is_current(&self) -> bool {
        connection_scope_matches(
            &self.origin_url,
            self.connection_epoch,
            &self.store.server_url.get(),
            self.store.connection_generation(),
        )
    }

    pub(crate) fn matches_url(&self, url: &str) -> bool {
        normalized_server_url(url) == self.origin_url.as_str()
    }

    pub(crate) fn origin_url(&self) -> &str {
        &self.origin_url
    }
}

fn normalized_server_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

fn connection_scope_matches(
    origin_url: &str,
    origin_epoch: u64,
    current_url: &str,
    current_epoch: u64,
) -> bool {
    origin_epoch == current_epoch
        && normalized_server_url(origin_url) == normalized_server_url(current_url)
}

#[cfg(test)]
mod tests {
    use super::connection_scope_matches;

    #[test]
    fn connection_scope_rejects_returning_to_same_url_after_switch() {
        assert!(connection_scope_matches("http://server-a/", 7, "http://server-a", 7));
        assert!(!connection_scope_matches("http://server-a", 7, "http://server-a/", 9));
    }
}

#[derive(Clone)]
pub(crate) struct PopupAction {
    label: SharedString,
    kbd: Option<SharedString>,
    icon: Option<falcon_proto::SessionAgent>,
    terminal_icon: bool,
    checked: Option<bool>,
    separated: bool,
    disabled: bool,
    danger: bool,
    server: ServerUrlGuard,
    activate: Rc<dyn Fn()>,
}

impl PopupAction {
    /// Native popup items are built when the context menu opens, so selection-dependent
    /// enabled state does not require rebuilding the terminal surface.
    pub(crate) fn into_menu_item(self) -> MenuItem {
        let disabled = self.disabled || !self.server.is_current();
        MenuItem::new(self.label.to_string())
            .disabled(disabled)
            .on_select(move || {
                if !disabled && self.server.is_current() {
                    (self.activate)();
                }
            })
    }

    pub(crate) fn new(
        server: ServerUrlGuard,
        label: impl Into<SharedString>,
        disabled: bool,
        danger: bool,
        activate: impl Fn() + 'static,
    ) -> Self {
        let action_server = server.clone();
        Self {
            label: label.into(),
            kbd: None,
            icon: None,
            terminal_icon: false,
            checked: None,
            separated: false,
            disabled,
            danger,
            server,
            activate: Rc::new(move || {
                if action_server.is_current() {
                    activate();
                }
            }),
        }
    }

    pub(crate) fn kbd(mut self, chord: impl Into<SharedString>) -> Self {
        self.kbd = Some(chord.into());
        self
    }

    pub(crate) fn session_icon(mut self, agent: Option<falcon_proto::SessionAgent>) -> Self {
        self.icon = agent;
        self.terminal_icon = agent.is_none();
        self
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub(crate) fn sep(mut self) -> Self {
        self.separated = true;
        self
    }
}

pub(crate) fn overflow_menu(
    trigger: impl Element + Interactive,
    actions: Vec<PopupAction>,
    theme: &ResolvedTheme,
) -> impl IntoElement {
    let ui = &theme.ui;
    let popover = rgb_to_hsla(ui.popover);
    let foreground = rgb_to_hsla(ui.popover_foreground);
    let border = rgba_to_hsla(ui.border);
    let popup_token = ActionOwner::current().popup_token();
    let pending_action = Rc::new(RefCell::new(None));
    let trigger_focused = Rc::new(Cell::new(false));
    let popup_focused = Rc::new(Cell::new(false));
    let focus_returns_to_trigger = is_desktop();
    nsh_base::menu()
        .trigger(popup_action_trigger(
            trigger,
            pending_action.clone(),
            trigger_focused.clone(),
        ))
        .offset(zpx(4.0))
        .margin(zpx(8.0))
        .popup_style(move |style| {
            style
                .bg(popover)
                .text_color(foreground)
                .border_1()
                .border_color(border)
                .rounded(zpx(8.0))
                .p(zpx(4.0))
                .shadow([popup_shadow(border)])
        })
        .on_open_change(move |event: &nsh_base::MenuEvent| {
            if let nsh_base::MenuEvent::OpenChange(open, _) = event {
                popup_token.set_open(*open);
            }
        })
        .children(popup_items(
            actions,
            theme,
            Some((
                pending_action,
                trigger_focused,
                popup_focused,
                focus_returns_to_trigger,
            )),
        ))
}

pub(crate) fn context_menu(
    trigger: impl IntoElement,
    actions: Vec<PopupAction>,
    theme: &ResolvedTheme,
) -> AnyElement {
    let ui = &theme.ui;
    let popover = rgb_to_hsla(ui.popover);
    let foreground = rgb_to_hsla(ui.popover_foreground);
    let border = rgba_to_hsla(ui.border);
    let popup_token = ActionOwner::current().popup_token();
    nsh_base::context_menu()
        .trigger(trigger)
        .offset(zpx(4.0))
        .margin(zpx(8.0))
        .popup_style(move |style| {
            style
                .bg(popover)
                .text_color(foreground)
                .border_1()
                .border_color(border)
                .rounded(zpx(8.0))
                .p(zpx(4.0))
                .shadow([popup_shadow(border)])
        })
        .on_open_change(move |event: &nsh_base::MenuEvent| {
            if let nsh_base::MenuEvent::OpenChange(open, _) = event {
                popup_token.set_open(*open);
            }
        })
        .children(popup_items(actions, theme, None))
        .into_element()
}

type PendingPopupAction = Rc<RefCell<Option<Box<dyn Fn()>>>>;

fn popup_action_trigger(
    trigger: impl Element + Interactive,
    pending_action: PendingPopupAction,
    trigger_focused: Rc<Cell<bool>>,
) -> impl Element + Interactive {
    let action_owner = ActionOwner::current();
    let focus_pending = pending_action.clone();
    let focus_state = trigger_focused.clone();
    let trigger = trigger.on_focus(move || {
        focus_state.set(true);
        let callback = focus_pending.borrow_mut().take();
        if let Some(callback) = callback {
            action_owner.run(|| callback());
        }
    });
    trigger.on_blur(move || trigger_focused.set(false))
}

fn popup_items(
    actions: Vec<PopupAction>,
    theme: &ResolvedTheme,
    deferred_dispatch: Option<(PendingPopupAction, Rc<Cell<bool>>, Rc<Cell<bool>>, bool)>,
) -> impl Iterator<Item = impl IntoElement> {
    let foreground = rgb_to_hsla(theme.ui.popover_foreground);
    let hover_background = rgb_to_hsla(theme.ui.tint);
    let hover_foreground = rgb_to_hsla(theme.ui.tint_foreground);
    let focus_background = rgb_to_hsla(theme.ui.tint);
    let focus_foreground = rgb_to_hsla(theme.ui.tint_foreground);
    let danger_color = rgb_to_hsla(theme.ui.destructive);
    let disabled_color = rgb_to_hsla(theme.ui.muted_foreground);
    let badge_border = rgba_to_hsla(theme.ui.border);
    let badge_background = rgb_to_hsla(theme.ui.muted);
    // Menu items are lazy children; retain FalconApp's owner for work that outlives the overlay.
    let owner = ActionOwner::current();
    actions.into_iter().map(move |action| {
        let initially_disabled = action.disabled;
        let disabled_server = action.server.clone();
        let selection_server = action.server.clone();
        let label = action.label;
        let is_danger = action.danger;
        let callback = action.activate;
        let action_owner = owner.clone();
        let deferred_dispatch = deferred_dispatch.clone();
        let popup_focus_state = deferred_dispatch
            .as_ref()
            .map(|(_, _, popup_focused, _)| popup_focused.clone());
        let currently_disabled = initially_disabled || !disabled_server.is_current();
        let color = if currently_disabled {
            disabled_color
        } else if is_danger {
            danger_color
        } else {
            foreground
        };
        let hover_server = disabled_server.clone();
        let focus_server = disabled_server.clone();
        let mut item = nsh_base::menu_item()
            .disabled(move || initially_disabled || !disabled_server.is_current())
            .disabled_style(|style| style.opacity(0.55))
            .on_select(move || {
                if !initially_disabled && selection_server.is_current() {
                    let callback = callback.clone();
                    if let Some((
                        pending_action,
                        trigger_focused,
                        popup_focused,
                        focus_returns_to_trigger,
                    )) = deferred_dispatch.clone()
                    {
                        let callback_server = selection_server.clone();
                        *pending_action.borrow_mut() = Some(Box::new(move || {
                            if callback_server.is_current() {
                                callback();
                            }
                        }));
                        if !focus_returns_to_trigger
                            || trigger_focused.get()
                            || !popup_focused.get()
                        {
                            let pending_for_answer = pending_action.clone();
                            let answer_owner = action_owner.clone();
                            action_owner.run(|| {
                                after_answer(move || {
                                    let callback = pending_for_answer.borrow_mut().take();
                                    if let Some(callback) = callback {
                                        answer_owner.run(|| callback());
                                    }
                                })
                            });
                        }
                    } else {
                        action_owner.run(|| callback());
                    }
                }
            });
        *item.style() = nsh_core::element::StyleRefinement::default()
            .px(zpx(10.0))
            .py(zpx(7.0))
            .rounded(zpx(6.0))
            .text_color(color);
        if !initially_disabled {
            let hover_foreground = if is_danger { danger_color } else { hover_foreground };
            let focus_foreground = if is_danger { danger_color } else { focus_foreground };
            item = item
                .hover(move |style| {
                    if hover_server.is_current() {
                        style.bg(hover_background).text_color(hover_foreground)
                    } else {
                        style
                    }
                })
                .focus(move |style| {
                    if focus_server.is_current() {
                        style.bg(focus_background).text_color(focus_foreground)
                    } else {
                        style
                    }
                });
        }
        let mut content = view().w_full().flex_row().items_center().gap(zpx(8.0));
        if action.terminal_icon || action.icon.is_some() {
            content = content.child(super::widgets::session_icon(action.icon));
        }
        if let Some(checked) = action.checked {
            content = content.child(view().w(zpx(15.0)).when(checked, |v| {
                v.child(super::widgets::glyph(super::widgets::Glyph::Check))
            }));
        }
        content = content.child(text(label).flex_1().text_xs());
        if let Some(kbd) = action.kbd.filter(|kbd| !kbd.is_empty()) {
            content = content.child(view().px(zpx(5.0)).py(zpx(1.0))
                .rounded(zpx(4.0)).border_1().border_color(badge_border)
                .bg(badge_background).text_color(disabled_color)
                .font_family(FONT_FAMILY_MONO).child(text(kbd).text_xs()));
        }
        let item = item.child(content);
        let item = view().when(action.separated, |v| {
            v.border_t_1().border_color(badge_border).pt(zpx(4.0)).mt(zpx(4.0))
        }).child(item);
        if let Some(popup_focused) = popup_focus_state {
            let focus_ref = element_ref();
            let focus_in = popup_focused.clone();
            let focus_out = popup_focused;
            focus_ref.on_focus_in(move || focus_in.set(true));
            focus_ref.on_focus_out(move || focus_out.set(false));
            view().element_ref(&focus_ref).child(item).into_element()
        } else {
            item.into_element()
        }
    })
}

pub(crate) fn themed_tooltip(
    content: impl IntoElement,
    shortcut: Option<SharedString>,
    theme: &ResolvedTheme,
) -> impl IntoTooltip {
    // nsh_base supplies placement and hover/focus behavior; this child provides the app surface.
    let ui = &theme.ui;
    let background = rgb_to_hsla(ui.popover);
    let foreground = rgb_to_hsla(ui.popover_foreground);
    let border = rgba_to_hsla(ui.border);
    let muted = rgb_to_hsla(ui.muted);
    let open_popups = ActionOwner::current().open_popups();
    let mut body = view()
        .max_w(zpx(320.0))
        .flex_row()
        .items_center()
        .gap(zpx(8.0))
        .child(view().flex_1().min_w_0().child(content));
    if let Some(shortcut) = shortcut {
        body = body.child(
            view()
                .flex_none()
                .px(zpx(5.0))
                .py(zpx(2.0))
                .rounded(zpx(4.0))
                .bg(muted)
                .child(text(shortcut).text_xs()),
        );
    }
    let surface = view()
        .max_w(zpx(320.0))
        .p(zpx(8.0))
        .text_xs()
        // NSH currently maps both FONT_SANS and FONT_MONO to this TX-02 face.
        .font_family(FONT_FAMILY_MONO)
        .bg(background)
        .text_color(foreground)
        .border_1()
        .border_color(border)
        .rounded(zpx(8.0))
        .shadow([popup_shadow(border)])
        .child(body);
    nsh_base::tooltip()
        .disabled(move || !open_popups.get().is_empty())
        .offset(zpx(6.0))
        .margin(zpx(8.0))
        .child(surface)
}

pub(crate) fn tooltip_path_text(path: impl AsRef<str>) -> SharedString {
    const MAX_ASCII_RUN: usize = 24;

    // Zero-width spaces add Unicode line-break opportunities without changing the source path.
    let path = path.as_ref();
    let mut wrapped = String::with_capacity(path.len() + path.len() / MAX_ASCII_RUN);
    let mut ascii_run = 0;
    for ch in path.chars() {
        wrapped.push(ch);
        if matches!(ch, '/' | '\\') {
            wrapped.push('\u{200b}');
            ascii_run = 0;
        } else if ch.is_ascii_graphic() {
            ascii_run += 1;
            if ascii_run == MAX_ASCII_RUN {
                wrapped.push('\u{200b}');
                ascii_run = 0;
            }
        } else {
            ascii_run = 0;
        }
    }
    wrapped.into()
}

pub(crate) fn menu_trigger(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    theme: &ResolvedTheme,
) -> impl Element + Interactive {
    let id = id.into();
    let title = title.into();
    let muted = rgb_to_hsla(theme.ui.muted);
    let foreground = rgb_to_hsla(theme.ui.muted_foreground);
    view()
        .label(id)
        .focusable(true)
        .w(zpx(26.0))
        .h(zpx(26.0))
        .flex_none()
        .flex_row()
        .items_center()
        .justify_center()
        .rounded(zpx(6.0))
        .cursor_pointer()
        .text_color(foreground)
        .hover(move |style| style.bg(muted))
        .tooltip(themed_tooltip(text(title), None, theme))
        .child(text("⋯").text_sm())
}

pub(crate) fn popup_shadow(mut color: Hsla) -> BoxShadow {
    color.a *= 0.45;
    BoxShadow {
        offset_x: zpx(0.0),
        offset_y: zpx(5.0),
        blur: zpx(14.0),
        spread: zpx(0.0),
        color: color.into(),
        inset: false,
    }
}
