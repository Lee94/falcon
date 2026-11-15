pub mod app_icon;

use falcon_core::window_background::{APP_BACKGROUND_PREF_KEY, AppBackground};
use nsh_core::platform::Idiom;
use nsh_core::prelude::{Signal, effect, idiom, signal};

use crate::store::WorkspaceStore;

#[derive(Clone)]
pub struct AppearanceState {
    pub app_icon: app_icon::AppIconController,
    pub window_background: Signal<AppBackground>,
}

impl AppearanceState {
    pub fn new() -> Self {
        Self {
            app_icon: app_icon::AppIconController::new(),
            window_background: signal(AppBackground::parse(
                crate::theme::load_pref_item(APP_BACKGROUND_PREF_KEY).as_deref(),
            )),
        }
    }

    pub fn set_window_background(&self, background: AppBackground) {
        self.window_background.set(background);
        crate::theme::save_pref_item(APP_BACKGROUND_PREF_KEY, Some(background.as_str()));
    }
}

pub fn supports_background(background: AppBackground, desktop: bool) -> bool {
    if background == AppBackground::Opaque {
        return true;
    }
    if !desktop {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        // NSH maps Window to Mica, not blur; the DWM backdrop silently fails before Windows 11 22H2, and live surface transparency changes are ignored.
        false
    }
    #[cfg(target_os = "linux")]
    {
        background == AppBackground::Transparent
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn effective_background(background: AppBackground, desktop: bool) -> AppBackground {
    if supports_background(background, desktop) {
        background
    } else {
        AppBackground::Opaque
    }
}

pub fn install_effects(store: WorkspaceStore) {
    app_icon::install_operations(store.clone());
    let background = store.appearance.window_background;
    effect(move || {
        let requested = background.get();
        crate::theme::save_pref_item(APP_BACKGROUND_PREF_KEY, Some(requested.as_str()));
        apply_window_background(requested, idiom() == Idiom::Desktop);
    });
}

fn apply_window_background(background: AppBackground, desktop: bool) {
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let _ = background;

    if !desktop {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    let effective = effective_background(background, desktop);

    #[cfg(target_os = "macos")]
    {
        use nsh_core::desktop::{Backdrop, WindowAttributes};
        use nsh_core::element::desktop::main_window;

        main_window().set(WindowAttributes {
            transparent: Some(effective != AppBackground::Opaque),
            backdrop: Some(if effective == AppBackground::Blurred {
                Backdrop::Window
            } else {
                Backdrop::None
            }),
            ..Default::default()
        });
    }

    #[cfg(target_os = "windows")]
    {
        use nsh_core::desktop::{Backdrop, WindowAttributes};
        use nsh_core::element::desktop::main_window;

        main_window().set(WindowAttributes {
            transparent: Some(false),
            backdrop: Some(if effective == AppBackground::Blurred {
                Backdrop::Window
            } else {
                Backdrop::None
            }),
            ..Default::default()
        });
    }

    #[cfg(target_os = "linux")]
    {
        use nsh_core::desktop::{Backdrop, WindowAttributes};
        use nsh_core::element::desktop::main_window;

        main_window().set(WindowAttributes {
            transparent: Some(effective == AppBackground::Transparent),
            backdrop: Some(Backdrop::None),
            ..Default::default()
        });
    }
}
