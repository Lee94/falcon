//! 「跑完了」提示音（ADR 0020 决定六）：程序经 OSC 7501 报告 done / error 时响一声。
//!
//! 什么时候算"跑完了"、晚到的轮询怎么不重响，规则在 falcon-core 的 `FinishChimes`；人正看着
//! 那个会话时不响，由工作区判断（`Workspace::chime_finished`）；声音由平台给
//! （`Platform::play_chime`）。这里只有开关（设置 → 通知）与播放入口。
//!
//! 开关存在偏好的 `falcon.finishSound` 下，缺省是开：只有存了 `off` 才算关——没这个键的老用户
//! 升级上来直接就有提示音。

use gpui_kit::App;

use crate::prefs::Prefs;

const PREF_KEY: &str = "falcon.finishSound";

pub fn enabled(cx: &App) -> bool {
    Prefs::global(cx).get(PREF_KEY) != Some("off")
}

pub fn set_enabled(on: bool, cx: &mut App) {
    cx.global_mut::<Prefs>().set(PREF_KEY, if on { "on" } else { "off" }.to_string());
}

pub fn play(cx: &App) {
    falcon_platform::get(cx).play_chime();
}
