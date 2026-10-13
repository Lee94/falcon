//! 「跑完了」的提示音（`Platform::play_chime`）：Web Audio 现场合成一声"叮—咚"（两个正弦音
//! 各带一个八度泛音、指数衰减），不带音频文件。
//!
//! 自动播放限制：AudioContext 只有用户在页面里有过手势之后才放得出声。Chrome 认"粘性激活"
//! （按过一次键，之后建的 context 直接就是 running）；Safari 更严，要在手势的调用栈里 resume
//! 过一次，这个 context 才算解锁。所以页面一起来就在捕获阶段挂上 keydown / pointerdown：
//! 在那里把 context 建好并 resume，之后程序什么时候跑完都响得出来。用户还没在页面里按过任何键
//! 就有程序跑完——不响，这是浏览器的规矩（此时的 resume 只会挂着，等下一次手势才兑现，规范
//! 如此，不会抛 unhandled rejection）。
//!
//! context 建好就一直跑着，不挂起：不出声时浏览器不会因为它拿电源锁、也不显示"正在播放"，
//! 挂起再恢复反倒要在 Safari 上重新碰一次手势规则。监听也一直挂着（跟页面同寿），context
//! 已经在跑时每次只是读一下状态。

use std::cell::RefCell;

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{AudioContext, AudioContextState, OscillatorType};

thread_local! {
    static CONTEXT: RefCell<Option<AudioContext>> = const { RefCell::new(None) };
}

fn context() -> Option<AudioContext> {
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = AudioContext::new().ok();
        }
        c.clone()
    })
}

/// 启动时调一次
pub fn unlock_on_gesture() {
    let Some(window) = web_sys::window() else { return };
    let unlock = Closure::<dyn FnMut()>::new(|| {
        let Some(ctx) = context() else { return };
        if ctx.state() != AudioContextState::Running {
            let _ = ctx.resume();
        }
    });
    for event in ["keydown", "pointerdown"] {
        let _ = window.add_event_listener_with_callback_and_bool(event, unlock.as_ref().unchecked_ref(), true);
    }
    unlock.forget();
}

pub fn play() {
    let Some(ctx) = context() else { return };
    if ctx.state() != AudioContextState::Running {
        // 还没等到手势：排一次 resume（挂着等下一次手势），这一声不放——排进挂起的 context 的音
        // 会在解锁那一刻冒出来，那时早就对不上号了
        let _ = ctx.resume();
        return;
    }
    if let Err(err) = ding(&ctx) {
        log::warn!("提示音没放出来：{err:?}");
    }
}

fn ding(ctx: &AudioContext) -> Result<(), JsValue> {
    let t = ctx.current_time() + 0.01;
    // A5 → E6（纯五度往上），第二声稍长
    tone(ctx, 880.0, t, 0.45, 0.16)?;
    tone(ctx, 1318.51, t + 0.13, 0.6, 0.14)
}

/// 一个音：基音 + 轻一点、衰减更快的八度泛音（只有正弦太"电子"，带一点泛音像敲铃）
fn tone(ctx: &AudioContext, freq: f32, start: f64, dur: f64, peak: f32) -> Result<(), JsValue> {
    partial(ctx, freq, start, dur, peak)?;
    partial(ctx, freq * 2.0, start, dur * 0.5, peak * 0.3)
}

fn partial(ctx: &AudioContext, freq: f32, start: f64, dur: f64, peak: f32) -> Result<(), JsValue> {
    let osc = ctx.create_oscillator()?;
    osc.set_type(OscillatorType::Sine);
    osc.frequency().set_value(freq);
    let gain = ctx.create_gain()?;
    let g = gain.gain();
    // 8ms 起音免得"啪"一声；指数衰减不能到 0，落到 -80dB
    g.set_value_at_time(0.0, start)?;
    g.linear_ramp_to_value_at_time(peak, start + 0.008)?;
    g.exponential_ramp_to_value_at_time(0.0001, start + dur)?;
    osc.connect_with_audio_node(&gain)?;
    gain.connect_with_audio_node(&ctx.destination())?;
    osc.start_with_when(start)?;
    osc.stop_with_when(start + dur + 0.05)
}
