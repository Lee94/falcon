//! 把 [`falcon_core::window_background::AppBackground`] 接到 GPUI 窗口上。
//!
//! `WindowBackgroundAppearance` 由 GPUI 落到各平台：macOS 的模糊是垫在内容下面的
//! `NSVisualEffectView`（材质 `UnderWindowBackground`，自带的染色被 GPUI 去掉，只留模糊），
//! Windows 走 acrylic，Wayland 走合成器的 blur 协议。不支持模糊的环境里，模糊和透明看起来一样，
//! 窗口仍透出桌面。
//!
//! 只把窗口根上的那层 `app` 降到 [`AppBackground::fill_alpha`]。岛、对话框、岛里的下沉块继续用
//! 实色：下沉块垫在岛的实底上，跟着变透明会在面板里开一个洞。

use falcon_core::window_background::{APP_BACKGROUND_PREF_KEY, AppBackground};
use gpui_kit::component::Root;
use gpui_kit::{AnyView, App, Context, Hsla, Styled as _, Window, WindowBackgroundAppearance, transparent_black};

use crate::prefs::Prefs;
use crate::theme::Ui;

pub fn current(cx: &App) -> AppBackground {
    AppBackground::parse(Prefs::global(cx).get(APP_BACKGROUND_PREF_KEY))
}

pub fn appearance(cx: &App) -> WindowBackgroundAppearance {
    to_gpui(current(cx))
}

/// 窗口根要铺的那层颜色。实色时就是 `app` 本身
pub fn fill(cx: &App) -> Hsla {
    Ui::global(cx).app.opacity(current(cx).fill_alpha())
}

/// 把当前偏好写到这扇系统窗口上：透明 / 模糊时 GPUI 把图层标成非不透明，
/// 模糊再垫一层 `NSVisualEffectView`。窗口底的颜色另由 [`fill`] 画。
pub fn apply(window: &mut Window, cx: &App) {
    window.set_background_appearance(appearance(cx));
}

/// 包一层 [`Root`]。gpui-component 的 Root 插件每帧给整窗铺主题 `background`
/// （Falcon Dark 是实色 `#0a0a0a`）。它画在窗口底的半透明填充下面，quad 的 alpha
/// 混合是 source One、dest One，底下这层把 alpha 饱和成 1，透明和模糊都只剩颜色
/// 变深。窗口底由各窗口根自己铺，这里把插件那一层盖成全透明，`a = 0` 的填充不会画。
pub fn mount_root(view: impl Into<AnyView>, window: &mut Window, cx: &mut Context<Root>) -> Root {
    Root::new(view, window, cx).bg(transparent_black())
}

/// 改偏好并让已经开着的窗口换成新效果。
///
/// 调用发生在某一扇窗口的 update 里，当场再 `update` 同一扇会借不到。先改手头这扇，其余的
/// 推到这轮分派结束再设，设完再刷新——刷新若早于系统窗口的透明标记，半透明色会合成到不透明的
/// 窗口底上，看起来只是变深了一点。
pub fn set(next: AppBackground, window: &mut Window, cx: &mut App) {
    if current(cx) == next {
        return;
    }
    cx.global_mut::<Prefs>().set(APP_BACKGROUND_PREF_KEY, next.as_str().to_string());
    apply(window, cx);
    cx.defer(|cx| {
        let appearance = to_gpui(current(cx));
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, _| window.set_background_appearance(appearance));
        }
        cx.refresh_windows();
    });
}

fn to_gpui(background: AppBackground) -> WindowBackgroundAppearance {
    match background {
        AppBackground::Opaque => WindowBackgroundAppearance::Opaque,
        AppBackground::Transparent => WindowBackgroundAppearance::Transparent,
        AppBackground::Blurred => WindowBackgroundAppearance::Blurred,
    }
}
