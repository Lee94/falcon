//! `falcon-android`：Falcon 安卓原生客户端的 Rust JNI 核心库。
//!
//! 职责：
//! - 复用 [`falcon_term`]（`alacritty_terminal` VT 解析、按键编码、Bracketed Paste、滚轮与鼠标上报、历史回滚与选区）；
//! - 复用 [`falcon_proto`]（`/ws/sessions/:id` 二进制帧解码、控制帧序列化、会话与项目模型）；
//! - 复用 [`falcon_theme`]（Falcon 与 Ghostty 463 套主题、OKLab 界面语义色派生、终端 256 色与 OSC 覆盖结算）；
//! - 复用 [`falcon_core`]（项目与附属项目分组树、会话自动标题、OSC 7501 程序状态徽标）。

pub mod dashboard;
pub mod term;

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jboolean, jdouble, jint, jintArray, jlong, jstring};

use crate::dashboard::{build_dashboard_view, export_theme_tokens, list_available_themes};
use crate::term::{
    create_session, destroy_session, get_session, parse_server_control_message,
};

fn jstr_to_string(env: &mut JNIEnv<'_>, s: &JString<'_>) -> String {
    if s.is_null() {
        return String::new();
    }
    env.get_string(s).map(Into::into).unwrap_or_default()
}

fn string_to_jstr(env: &mut JNIEnv<'_>, s: &str) -> jstring {
    env.new_string(s)
        .map(|o| o.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_createSession(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    cols: jint,
    rows: jint,
    durable: jboolean,
    theme_name: JString<'_>,
) -> jlong {
    let theme = jstr_to_string(&mut env, &theme_name);
    create_session(
        cols.clamp(2, 500) as u16,
        rows.clamp(1, 300) as u16,
        durable != 0,
        &theme,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_destroySession(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    destroy_session(handle);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_setSessionDurable(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    durable: jboolean,
) {
    if let Some(session) = get_session(handle) {
        session.set_durable(durable != 0);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_setSessionTheme(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    theme_name: JString<'_>,
) -> jstring {
    let theme = jstr_to_string(&mut env, &theme_name);
    let out = get_session(handle)
        .map(|s| s.set_theme(&theme))
        .unwrap_or_default();
    string_to_jstr(&mut env, &out)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_handshakeMessages(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    cols: jint,
    rows: jint,
    visible: jboolean,
) -> jstring {
    let msgs = get_session(handle)
        .map(|s| {
            s.handshake_messages(
                cols.clamp(2, 500) as u16,
                rows.clamp(1, 300) as u16,
                visible != 0,
            )
        })
        .unwrap_or_default();
    let json = serde_json::to_string(&msgs).unwrap_or_else(|_| "[]".to_string());
    string_to_jstr(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_onViewportResize(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    cols: jint,
    rows: jint,
) -> jstring {
    let msg = get_session(handle)
        .and_then(|s| {
            s.on_viewport_resize(cols.clamp(2, 500) as u16, rows.clamp(1, 300) as u16)
        })
        .unwrap_or_default();
    string_to_jstr(&mut env, &msg)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_onServerSize(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    cols: jint,
    rows: jint,
    owner: jboolean,
) {
    if let Some(session) = get_session(handle) {
        session.on_server_size(
            cols.clamp(2, 500) as u16,
            rows.clamp(1, 300) as u16,
            owner != 0,
        );
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_onBinaryFrame(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    frame: JByteArray<'_>,
) -> jstring {
    let bytes = env.convert_byte_array(&frame).unwrap_or_default();
    let Some(session) = get_session(handle) else {
        return string_to_jstr(&mut env, r#"{"dirty":false,"effects":[]}"#);
    };
    let result = session.on_binary_frame(&bytes);
    let json = serde_json::to_string(&result)
        .unwrap_or_else(|_| r#"{"dirty":false,"effects":[]}"#.to_string());
    string_to_jstr(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_packRenderSnapshot(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jintArray {
    let data = get_session(handle)
        .map(|s| s.pack_render_snapshot())
        .unwrap_or_default();
    match env.new_int_array(data.len() as i32) {
        Ok(arr) => {
            let _ = env.set_int_array_region(&arr, 0, &data);
            arr.into_raw()
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_encodeKey(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    key: JString<'_>,
    ctrl: jboolean,
    alt: jboolean,
    shift: jboolean,
) -> jstring {
    let key_str = jstr_to_string(&mut env, &key);
    let chunks = get_session(handle)
        .map(|s| s.encode_key(&key_str, ctrl != 0, alt != 0, shift != 0))
        .unwrap_or_default();
    let json = serde_json::to_string(&chunks).unwrap_or_else(|_| "[]".to_string());
    string_to_jstr(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_encodeText(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    text: JString<'_>,
) -> jstring {
    let text_str = jstr_to_string(&mut env, &text);
    let chunks = get_session(handle)
        .map(|s| s.encode_text(&text_str))
        .unwrap_or_default();
    let json = serde_json::to_string(&chunks).unwrap_or_else(|_| "[]".to_string());
    string_to_jstr(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_encodePaste(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    text: JString<'_>,
) -> jstring {
    let text_str = jstr_to_string(&mut env, &text);
    let chunks = get_session(handle)
        .map(|s| s.encode_paste(&text_str))
        .unwrap_or_default();
    let json = serde_json::to_string(&chunks).unwrap_or_else(|_| "[]".to_string());
    string_to_jstr(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_onScrollGesture(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    delta_y_pixels: jdouble,
    cell_height: jdouble,
    row: jint,
    col: jint,
) -> jstring {
    let msg = get_session(handle)
        .and_then(|s| {
            s.on_scroll_gesture(
                delta_y_pixels,
                cell_height,
                row.max(0) as usize,
                col.max(0) as usize,
            )
        })
        .unwrap_or_default();
    string_to_jstr(&mut env, &msg)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_scrollToBottom(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if let Some(session) = get_session(handle) {
        session.core.scroll_to_bottom();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_onMouseEvent(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    action: jint,
    row: jint,
    col: jint,
    pixel_x: jint,
    pixel_y: jint,
) -> jstring {
    let msg = get_session(handle)
        .and_then(|s| s.on_mouse_event(action, row, col, pixel_x, pixel_y))
        .unwrap_or_default();
    string_to_jstr(&mut env, &msg)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_startSelection(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    kind: jint,
    row: jint,
    col: jint,
    right_half: jboolean,
) {
    if let Some(session) = get_session(handle) {
        session.start_selection(kind, row.max(0) as usize, col.max(0) as usize, right_half != 0);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_updateSelection(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    row: jint,
    col: jint,
    right_half: jboolean,
) {
    if let Some(session) = get_session(handle) {
        session.update_selection(row.max(0) as usize, col.max(0) as usize, right_half != 0);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_clearSelection(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if let Some(session) = get_session(handle) {
        session.clear_selection();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_selectionText(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    let text = get_session(handle)
        .and_then(|s| s.selection_text())
        .unwrap_or_default();
    string_to_jstr(&mut env, &text)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_parseServerMessage(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    raw_json: JString<'_>,
) -> jstring {
    let s = jstr_to_string(&mut env, &raw_json);
    let out = parse_server_control_message(&s);
    string_to_jstr(&mut env, &out)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_buildDashboardView(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    projects_json: JString<'_>,
    sessions_json: JString<'_>,
    hosts_json: JString<'_>,
) -> jstring {
    let p = jstr_to_string(&mut env, &projects_json);
    let s = jstr_to_string(&mut env, &sessions_json);
    let h = jstr_to_string(&mut env, &hosts_json);
    let out = build_dashboard_view(&p, &s, &h);
    string_to_jstr(&mut env, &out)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_exportThemeTokens(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    theme_name: JString<'_>,
) -> jstring {
    let name = jstr_to_string(&mut env, &theme_name);
    let out = export_theme_tokens(&name);
    string_to_jstr(&mut env, &out)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_falcon_android_bridge_FalconNative_listAvailableThemes(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    query: JString<'_>,
) -> jstring {
    let q = jstr_to_string(&mut env, &query);
    let out = list_available_themes(&q);
    string_to_jstr(&mut env, &out)
}
