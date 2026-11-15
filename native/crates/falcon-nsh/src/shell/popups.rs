use std::rc::Rc;

use crate::{
    fonts::FONT_FAMILY_MONO,
    shell::action_owner::ActionOwner,
    store::WorkspaceStore,
    theme::{rgb_to_hsla, rgba_to_hsla},
    zoom::zpx,
};
use falcon_theme::ResolvedTheme;
use nsh_core::prelude::*;

/// Binds popup actions to the server that produced their resource IDs. URL checks reject A→B,
/// but cannot detect A→B→A; the parent connection-epoch integration is needed for that case.
#[derive(Clone)]
pub(crate) struct ServerUrlGuard {
    store: WorkspaceStore,
    origin_url: String,
}

impl ServerUrlGuard {
    pub(crate) fn capture(store: &WorkspaceStore) -> Self {
        let origin_url = normalized_server_url(&store.server_url.get()).to_owned();
        Self {
            store: store.clone(),
            origin_url,
        }
    }

    pub(crate) fn is_current(&self) -> bool {
        normalized_server_url(&self.store.server_url.get()) == self.origin_url.as_str()
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

#[derive(Clone)]
pub(crate) struct PopupAction {
    label: SharedString,
    disabled: bool,
    danger: bool,
    server: ServerUrlGuard,
    activate: Rc<dyn Fn()>,
}

impl PopupAction {
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
}

pub(crate) fn overflow_menu(
    trigger: impl IntoElement,
    actions: Vec<PopupAction>,
    theme: &ResolvedTheme,
) -> impl IntoElement {
    let ui = &theme.ui;
    let popover = rgb_to_hsla(ui.popover);
    let foreground = rgb_to_hsla(ui.popover_foreground);
    let border = rgba_to_hsla(ui.border);
    nsh_base::menu()
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
        .children(popup_items(actions, theme))
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
        .children(popup_items(actions, theme))
        .into_element()
}

fn popup_items(
    actions: Vec<PopupAction>,
    theme: &ResolvedTheme,
) -> impl Iterator<Item = impl IntoElement> {
    let foreground = rgb_to_hsla(theme.ui.popover_foreground);
    let muted = rgb_to_hsla(theme.ui.muted);
    let danger_color = rgb_to_hsla(theme.ui.destructive);
    let disabled_color = rgb_to_hsla(theme.ui.muted_foreground);
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
        let currently_disabled = initially_disabled || !disabled_server.is_current();
        let color = if currently_disabled {
            disabled_color
        } else if is_danger {
            danger_color
        } else {
            foreground
        };
        let mut item = nsh_base::menu_item()
            .disabled(move || initially_disabled || !disabled_server.is_current())
            .hover(move |style| style.bg(muted))
            .focus(move |style| style.bg(muted))
            .disabled_style(|style| style.opacity(0.55))
            .on_select(move || {
                if !initially_disabled && selection_server.is_current() {
                    let callback = callback.clone();
                    action_owner.run(|| callback());
                }
            });
        *item.style() = nsh_core::element::StyleRefinement::default()
            .px(zpx(10.0))
            .py(zpx(7.0))
            .rounded(zpx(6.0))
            .text_color(color);
        item.child(text(label).text_xs())
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
) -> impl IntoElement {
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
