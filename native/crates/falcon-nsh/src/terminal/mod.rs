//! `FalconTerminal` 跨六端统一终端组件。
//!
//! - 桌面 / Android / Web：`canvas(Painter)` 批量网格绘制 + 锚定在光标处的 `1pt` 透明 `input()` 接收器
//!   （承载中文 / 日文 IME 组合输入、系统软键盘唤醒与原生粘贴）；
//! - TUI（`Idiom::Terminal`）：自动切换为字符网格投影 + `Enter` / `Ctrl+\` 直通输入切换条；
//! - 原生扩展槽位：预留 `native("falcon.terminal_surface")` 钩子，供特定宿主需要接管底层纹理时挂载。

pub mod painter;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use falcon_term::keyroute::{KeyRoute, route_key};
use falcon_term::keys::to_esc_str;
use falcon_term::keystroke::{Keystroke as TermKeystroke, Modifiers as TermModifiers};
use falcon_term::{DeltaMode, MouseAction, MouseButton as TermMouseButton, MouseReportEvent};
use nsh_core::element::input;
use nsh_core::keymap::When;
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

pub use painter::{TerminalViewport, paint_snapshot, render_tui_lines};
pub use session::{EffectiveGrid, SessionRuntimeMeta, TerminalSession};

use crate::i18n::t;
use crate::theme::{rgb_to_hsla, use_theme};
use crate::zoom::{pt_px, zpx};

/// `FalconTerminal` 组件属性。
#[derive(Clone, PartialEq, Props)]
pub struct FalconTerminalProps {
    /// 终端会话唯一标识。
    pub session_id: String,
    /// 当前面板是否拥有工作区焦点。
    pub focused: bool,
    /// TUI 宿主下是否处于全按键直通模式（`Enter` 开启，`Ctrl+\` 退出）。
    pub tui_passthrough: bool,
    /// 可选：启用宿主注册的 `"falcon.terminal_surface"` 原生表面组件。
    pub use_native_surface: bool,
}

/// 检查快捷键字符串在当前平台是否可安全注册到 `on_key_with`（过滤系统保留键，避免 NSH 内部断言 panic）。
pub fn is_safe_nsh_keystroke(spec: &str) -> bool {
    nsh_core::keymap::Keystroke::parse_with(spec, apple_keys())
        .ok()
        .filter(|ks| ks.reserved(os()).is_none())
        .is_some()
}

/// 在当前作用域安全绑定非保留快捷键。
pub fn bind_safe_key(spec: &str, repeat: bool, handler: impl Fn() + 'static) {
    bind_safe_key_when(spec, repeat, When::Always, handler);
}

pub fn bind_safe_key_when(spec: &str, repeat: bool, when: When, handler: impl Fn() + 'static) {
    if is_safe_nsh_keystroke(spec) {
        on_key_with(
            spec,
            KeyOptions {
                when,
                repeat,
                pass_through: false,
            },
            move |_: &str| handler(),
        );
    }
}

pub fn terminal_shortcuts_active(
    focused: bool,
    modal_open: bool,
    overview_open: bool,
    tui_passthrough: bool,
    terminal_idiom: bool,
    terminal_input_focused: bool,
) -> bool {
    focused
        && !modal_open
        && !overview_open
        && if terminal_idiom {
            tui_passthrough
        } else {
            terminal_input_focused
        }
}

pub fn tui_exit_key_active(
    focused: bool,
    modal_open: bool,
    overview_open: bool,
    tui_passthrough: bool,
    terminal_idiom: bool,
) -> bool {
    focused && !modal_open && !overview_open && tui_passthrough && terminal_idiom
}

/// 跨六端统一终端视图。
#[component]
pub fn FalconTerminal(props: FalconTerminalProps) -> impl IntoElement {
    let theme_ctrl = use_theme();
    let store = crate::store::use_workspace();
    let session_id = props.session_id.clone();
    let session = store.ensure_terminal_session(&session_id);

    let pane_bounds = signal((800.0_f32, 480.0_f32));
    let ime_value = signal(String::new());
    let ime_preedit = signal(String::new());
    let frame_tick = signal(session.frame_seq());
    let sink_ref = element_ref();
    let terminal_input_focused = signal(false);
    let focused_signal = signal(props.focused);
    focused_signal.set(props.focused);
    let tui_passthrough_signal = signal(props.tui_passthrough);
    tui_passthrough_signal.set(props.tui_passthrough);

    // 16ms 输出轮询仅推进脏帧；光标闪烁另用低频计时器，不依赖终端输出。
    {
        let session_poll = session.clone();
        set_interval(Duration::from_millis(16), move || {
            let latest = session_poll.frame_seq();
            if frame_tick.get() != latest {
                if let Some(clip) = session_poll.take_clipboard_pending() {
                    set_clipboard(clip);
                }
                frame_tick.set(latest);
            }
        });
    }

    // 主题切换时同步深浅线索到会话连接（ADR 0015）
    {
        let session_theme = session.clone();
        effect(move || {
            let resolved = theme_ctrl.resolved();
            session_theme.sync_appearance(&resolved);
        });
    }

    // 焦点终端非字符控制键路由（经 `is_safe_nsh_keystroke` 守卫排除系统保留键）
    let store_keys = store.clone();
    let session_keys = session.clone();
    let focused_keys = focused_signal.clone();
    let tui_passthrough_keys = tui_passthrough_signal.clone();
    let terminal_input_focus = terminal_input_focused.clone();
    let terminal_idiom = idiom() == Idiom::Terminal;
    effect(move || {
        let focused = focused_keys.get();
        let tui_passthrough = tui_passthrough_keys.get();
        let modal_open = store_keys.active_dialog.get() != crate::store::ActiveDialog::None;
        let overview_open = store_keys.overview_open.get();
        let terminal_keys_active = terminal_shortcuts_active(
            focused,
            modal_open,
            overview_open,
            tui_passthrough,
            terminal_idiom,
            terminal_input_focus.get(),
        );
        if terminal_keys_active {
            let terminal_special_keys: &[(&str, &str, TermModifiers)] = &[
                ("escape", "escape", TermModifiers::default()),
                ("tab", "tab", TermModifiers::default()),
                (
                    "shift+tab",
                    "tab",
                    TermModifiers {
                        shift: true,
                        ..Default::default()
                    },
                ),
                ("up", "up", TermModifiers::default()),
                ("down", "down", TermModifiers::default()),
                ("left", "left", TermModifiers::default()),
                ("right", "right", TermModifiers::default()),
                ("home", "home", TermModifiers::default()),
                ("end", "end", TermModifiers::default()),
                ("pageup", "pageup", TermModifiers::default()),
                ("pagedown", "pagedown", TermModifiers::default()),
                (
                    "ctrl+c",
                    "c",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+d",
                    "d",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+z",
                    "z",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+l",
                    "l",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+a",
                    "a",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+e",
                    "e",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+u",
                    "u",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+w",
                    "w",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
                (
                    "ctrl+r",
                    "r",
                    TermModifiers {
                        control: true,
                        ..Default::default()
                    },
                ),
            ];

            for &(nsh_spec, key_name, mods) in terminal_special_keys {
                let sess = session_keys.clone();
                let key_owned = key_name.to_string();
                bind_safe_key(nsh_spec, true, move || {
                    let ks = TermKeystroke {
                        modifiers: mods,
                        key: key_owned.clone(),
                        key_char: None,
                    };
                    match route_key(&ks, false, sess.core().has_selection()) {
                        KeyRoute::Copy => {
                            if let Some(sel) = sess.core().selection_text() {
                                set_clipboard(sel);
                            }
                        }
                        KeyRoute::Paste => {}
                        KeyRoute::Global => {}
                        KeyRoute::Terminal => {
                            if let Some(esc) = to_esc_str(&ks, sess.core().mode(), true) {
                                sess.send_input(&esc);
                            }
                        }
                    }
                });
            }

            // TUI 直通模式下按 `Ctrl+\` 退出直通返回工作台导航
        }

        if tui_exit_key_active(
            focused,
            modal_open,
            overview_open,
            tui_passthrough,
            terminal_idiom,
        ) {
            let store_exit = store_keys.clone();
            bind_safe_key("ctrl+\\", false, move || {
                store_exit.set_tui_passthrough(false);
            });
        }
    });

    // 若宿主显式开启了原生表面扩展组件（如自定义 GPU 表面），直接挂接 `<native>` 槽位
    if props.use_native_surface {
        let sid = session_id.clone();
        return view()
            .size_full()
            .child(
                native("falcon.terminal_surface")
                    .size_full()
                    .prop("sessionId", serde_json::Value::String(sid)),
            )
            .into_element();
    }

    // TUI 宿主环境（`Idiom::Terminal`）：使用等宽字符行网格渲染，省去像素画布往返
    if idiom() == Idiom::Terminal {
        let sess_tui = session.clone();
        let passthrough = props.tui_passthrough;
        let sid_enter = session_id.clone();
        let store_tui = store.clone();
        return view()
            .size_full()
            .flex_col()
            .bg(move || rgb_to_hsla(theme_ctrl.resolved().terminal.background))
            .on_layout({
                let sess_layout = session.clone();
                move |b: &Bounds<Points>| {
                    let cols = (b.size.width.0.floor() as usize).max(20);
                    let rows = (b.size.height.0.floor() as usize).saturating_sub(1).max(4);
                    sess_layout.report_viewport_grid(cols, rows);
                    pane_bounds.set((b.size.width.0, b.size.height.0));
                }
            })
            .child(
                view()
                    .flex_1()
                    .flex_col()
                    .overflow_hidden()
                    .p(zpx(4.0))
                    .child(dynamic(move || {
                        let _ = frame_tick.get();
                        let (w, h) = pane_bounds.get();
                        let snap = sess_tui.snapshot();
                        let lines = render_tui_lines(
                            &snap,
                            (w as usize).max(40),
                            (h as usize).saturating_sub(2).max(6),
                        );
                        let fg = rgb_to_hsla(theme_ctrl.resolved().terminal.foreground);
                        view()
                            .flex_col()
                            .children(lines.into_iter().map(move |line| {
                                text(if line.is_empty() {
                                    " ".to_string()
                                } else {
                                    line
                                })
                                .text_color(fg)
                                .number_of_lines(1)
                            }))
                    })),
            )
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(zpx(8.0))
                    .py(zpx(2.0))
                    .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.muted))
                    .child(
                        text(move || {
                            if passthrough {
                                t!("native.nsh.tuiHintPassthrough")
                            } else {
                                t!("native.nsh.tuiHintNavigate")
                            }
                        })
                        .text_xs()
                        .text_color(move || rgb_to_hsla(theme_ctrl.resolved().ui.muted_foreground)),
                    )
                    .child(
                        view()
                            .px(zpx(6.0))
                            .py(zpx(2.0))
                            .rounded(zpx(4.0))
                            .cursor_pointer()
                            .bg(move || {
                                let ui = &theme_ctrl.resolved().ui;
                                if passthrough {
                                    rgb_to_hsla(ui.tint)
                                } else {
                                    rgb_to_hsla(ui.secondary)
                                }
                            })
                            .on_press(move || {
                                store_tui.focus_session(&sid_enter);
                                store_tui.set_tui_passthrough(!passthrough);
                            })
                            .child(
                                text(move || {
                                    if passthrough {
                                        t!("native.nsh.tuiExitPassthrough")
                                    } else {
                                        t!("native.nsh.tuiEnterPassthrough")
                                    }
                                })
                                .text_xs()
                                .text_color(move || {
                                    rgb_to_hsla(theme_ctrl.resolved().ui.foreground)
                                }),
                            ),
                    ),
            )
            .into_element();
    }

    // 桌面 / Android / Web：高性能矢量 + 文本批次画布 + 跟随光标的隐藏 IME 输入汇
    let term_pref = store.term_prefs();
    let font_size = term_pref.font_size as f32;
    let cell_height = (font_size * term_pref.line_height as f32).round().max(14.0);
    let cell_width = (font_size * 0.60).max(6.5);

    let sess_canvas = session.clone();
    let sess_layout = session.clone();
    let sess_press = session.clone();
    let sess_wheel = session.clone();
    let sess_ime_change = session.clone();
    let sess_ime_submit = session.clone();
    let sess_ime_paste = session.clone();
    let sess_anchor = session.clone();
    let sid_focus = session_id.clone();
    let is_focused = props.focused;

    let term_preferences = store.term_prefs;
    let cursor_visible = signal(true);
    set_interval(Duration::from_millis(500), move || {
        cursor_visible.update(|v| *v = !*v)
    });
    let compute_vp = move |snap: &falcon_term::Snapshot, sess: &Arc<TerminalSession>| {
        let pref = term_preferences.get();
        let font_size = pref.font_size as f32;
        let cell_width = (font_size * 0.60).max(6.5);
        let cell_height = (font_size * pref.line_height as f32).round().max(14.0);
        let (pw, ph) = pane_bounds.get();
        let eff = sess.effective_grid();
        TerminalViewport::compute(
            pw,
            ph,
            cell_width,
            cell_height,
            font_size,
            eff.owner,
            is_focused,
            snap,
        )
    };

    {
        let session = session.clone();
        effect(move || {
            let pref = term_preferences.get();
            let (width, height) = pane_bounds.get();
            let cols = (width / (pref.font_size as f32 * 0.60).max(6.5)).floor() as usize;
            let rows = (height
                / (pref.font_size as f32 * pref.line_height as f32)
                    .round()
                    .max(14.0))
            .floor() as usize;
            if cols >= 2 && rows >= 1 {
                session.report_viewport_grid(cols, rows);
            }
        });
    }

    let terminal_view = view()
        .size_full()
        .relative()
        .overflow_hidden()
        .test_id(&format!("terminal-view-{session_id}"))
        .bg(move || rgb_to_hsla(theme_ctrl.resolved().terminal.background))
        .on_layout(move |b: &Bounds<Points>| {
            let pw = b.size.width.0.max(16.0);
            let ph = b.size.height.0.max(16.0);
            pane_bounds.set((pw, ph));
            let cols = (pw / cell_width).floor() as usize;
            let rows = (ph / cell_height).floor() as usize;
            if cols >= 2 && rows >= 1 {
                sess_layout.report_viewport_grid(cols, rows);
            }
        })
        .on_press({
            let sink_focus = sink_ref.clone();
            let store_press = store.clone();
            move |e: &PressEvent| {
                if store_press.focused_pane_key.get() != falcon_core::pane_key::term_key(&sid_focus)
                {
                    store_press.focus_session(&sid_focus);
                }
                sink_focus.focus();
                let snap = sess_press.snapshot();
                let vp = compute_vp(&snap, &sess_press);
                let (col, row) = vp.pixel_to_cell(
                    e.position.x.0,
                    e.position.y.0,
                    snap.size.cols,
                    snap.size.rows,
                );
                let _ = sess_press.handle_mouse(MouseReportEvent {
                    action: MouseAction::Down,
                    button: TermMouseButton::Left,
                    col: col as i32,
                    row: row as i32,
                    x: e.position.x.0.max(0.0) as i32,
                    y: e.position.y.0.max(0.0) as i32,
                    shift: e.modifiers.shift,
                    alt: e.modifiers.alt,
                    ctrl: e.modifiers.ctrl,
                });
                let _ = sess_press.handle_mouse(MouseReportEvent {
                    action: MouseAction::Up,
                    button: TermMouseButton::Left,
                    col: col as i32,
                    row: row as i32,
                    x: e.position.x.0.max(0.0) as i32,
                    y: e.position.y.0.max(0.0) as i32,
                    shift: e.modifiers.shift,
                    alt: e.modifiers.alt,
                    ctrl: e.modifiers.ctrl,
                });
            }
        })
        .on_wheel(move |ev: &WheelEvent| {
            let snap = sess_wheel.snapshot();
            let vp = compute_vp(&snap, &sess_wheel);
            let (col, row) = vp.pixel_to_cell(
                ev.position.x.0,
                ev.position.y.0,
                snap.size.cols,
                snap.size.rows,
            );
            let (mode, dy) = match ev.delta {
                WheelDelta::Lines(pt) => (DeltaMode::Line, -pt.y as f64),
                WheelDelta::Points(pt) => (DeltaMode::Pixel, -pt.y.0 as f64),
            };
            sess_wheel.handle_wheel(
                mode,
                dy,
                cell_height as f64,
                col as i32,
                row as i32,
                ev.modifiers.shift,
            );
        })
        .child(
            canvas(move || {
                let _ = frame_tick.get();
                let resolved = theme_ctrl.resolved();
                let pref = term_preferences.get();
                let mut snap = sess_canvas.snapshot();
                let vp = compute_vp(&snap, &sess_canvas);
                if pref.cursor_blink && !cursor_visible.get() {
                    snap.cursor = None;
                }
                let preedit = ime_preedit.get();
                let mut families = falcon_core::term::primary_family(&pref)
                    .into_iter()
                    .collect::<Vec<_>>();
                families.extend(falcon_core::term::fallback_families(&pref));
                let mut stack = families
                    .iter()
                    .map(|f| format!("\"{}\"", f.replace('\\', "\\\\").replace('"', "\\\"")))
                    .collect::<Vec<_>>()
                    .join(", ");
                if !stack.is_empty() {
                    stack.push_str(", ");
                }
                stack.push_str(crate::fonts::FONT_STACK_MONO);
                painter::paint_snapshot_with_preferences(
                    &snap,
                    &resolved,
                    vp,
                    (!preedit.is_empty()).then_some(preedit.as_str()),
                    &stack,
                    Some(pref.cursor_style),
                )
            })
            .size_full(),
        )
        // 锚定在当前光标位置的 1pt 极低透明度原生 Input：承接平台输入法候选框、剪贴板粘贴与移动端软键盘
        .child(
            input()
                .test_id(&format!("terminal-input-{session_id}"))
                .element_ref(&sink_ref)
                .on_focus({
                    let terminal_input_focused = terminal_input_focused.clone();
                    move || terminal_input_focused.set(true)
                })
                .on_blur({
                    let terminal_input_focused = terminal_input_focused.clone();
                    move || terminal_input_focused.set(false)
                })
                .absolute()
                .left({
                    let sess_left = sess_anchor.clone();
                    move || {
                        let _ = frame_tick.get();
                        let snap = sess_left.snapshot();
                        let vp = compute_vp(&snap, &sess_left);
                        pt_px(vp.cursor_anchor_px(&snap).0)
                    }
                })
                .top({
                    let sess_top = sess_anchor;
                    move || {
                        let _ = frame_tick.get();
                        let snap = sess_top.snapshot();
                        let vp = compute_vp(&snap, &sess_top);
                        pt_px(vp.cursor_anchor_px(&snap).1)
                    }
                })
                .w(pt_px(2.0))
                .h(pt_px(2.0))
                .opacity(0.02)
                .value(move || ime_value.get())
                .caret(false)
                .auto_correct(false)
                .auto_capitalize(AutoCapitalize::None)
                .paste_behavior(PasteBehavior::Report)
                .submit_behavior(SubmitBehavior::Submit)
                .on_composition_change(move |comp: &Composition| {
                    ime_preedit.set(comp.text.to_string());
                })
                .on_paste(move |ev: &PasteEvent| {
                    if !ev.text.is_empty() {
                        sess_ime_paste.send_paste(&ev.text);
                    }
                })
                .on_change({
                    let store_ime = store.clone();
                    move |txt: &str| {
                        if !txt.is_empty() && ime_preedit.get().is_empty() {
                            let committed = store_ime.apply_extra_modifiers(txt);
                            sess_ime_change.send_input(&committed);
                            ime_value.set(String::new());
                        } else {
                            ime_value.set(txt.to_string());
                        }
                    }
                })
                .on_submit(move |_: &str| {
                    sess_ime_submit.send_input("\r");
                    ime_value.set(String::new());
                })
                .on_key_press({
                    let sess_kp = session.clone();
                    move |k: &str| {
                        if k == "Backspace" && ime_value.get().is_empty() {
                            sess_kp.send_input("\x7f");
                        }
                    }
                }),
        );

    #[cfg(test)]
    let terminal_view = {
        let session = session.clone();
        let frame_tick = frame_tick.clone();
        terminal_view.child(dynamic(move || {
            let _ = frame_tick.get();
            view()
                .absolute()
                .left(pt_px(1.0))
                .top(pt_px(1.0))
                .test_id("terminal-input-count")
                .child(text(format!(
                    "terminal-input-count-{}",
                    session.test_input_count()
                )))
                .into_element()
        }))
    };

    terminal_view.into_element()
}

#[cfg(test)]
mod shortcut_scope_tests {
    use super::{terminal_shortcuts_active, tui_exit_key_active};

    #[test]
    fn terminal_controls_do_not_route_outside_active_terminal_context() {
        assert!(terminal_shortcuts_active(
            true, false, false, false, false, true
        ));
        assert!(!terminal_shortcuts_active(
            true, false, false, false, false, false
        ));
        assert!(!terminal_shortcuts_active(
            false, false, false, false, false, true
        ));
        assert!(!terminal_shortcuts_active(
            true, true, false, false, false, true
        ));
        assert!(!terminal_shortcuts_active(
            true, false, true, false, false, true
        ));
        assert!(!terminal_shortcuts_active(
            true, false, false, false, true, false
        ));
        assert!(terminal_shortcuts_active(
            true, false, false, true, true, false
        ));
        assert!(tui_exit_key_active(true, false, false, true, true));
        assert!(!tui_exit_key_active(true, true, false, true, true));
    }
}
