//! 脚本化驱动（`--features automation` 编进来；`snapshot` 包含它并额外提供 `snap` 截图）：锁屏 / 远程时验证界面的唯一手段。
//!
//! `FALCON_AUTOMATE` 是一串用 `;` 分隔的步骤，按顺序在第一个服务端窗口上执行：
//!
//! | 步骤 | 作用 |
//! |---|---|
//! | `wait:<ms>` | 等待 |
//! | `ready` | 等工作区登录完、数据拉回来（最多 15s） |
//! | `select:<项目名>` | 在侧栏选中项目 |
//! | `new-terminal[:claude]` | 在当前项目新建终端（可带 agent） |
//! | `type:<文本>` | 往活动终端送输入，支持 `\r` `\n` `\t` `\e` `\xHH` 转义（`;` 写成 `\x3b`） |
//! | `key:<键>` | 派发一次按键（GPUI 写法，如 `cmd-shift-p`、`enter`） |
//! | `action:<名字>` | 派发动作（如 `falcon::ToggleFilesPanel`） |
//! | `click:<x>,<y>` / `rclick:<x>,<y>` | 在窗口逻辑坐标处左 / 右击 |
//! | `move:<x>,<y>` | 鼠标移动 |
//! | `scroll:<x>,<y>,<dx>,<dy>` | 在该点滚一次像素滚轮（dy 为负往下翻） |
//! | `drag:<x1>,<y1>,<x2>,<y2>` | 左键按住从 1 拖到 2（中间插 8 个移动事件） |
//! | `frames:<ms>[:refresh]` | 按 60Hz 手动画这么久（锁屏时模拟显示链路，压测要算上渲染开销），打印 p50 / p95 / 最慢；带 `:refresh` 时每帧无视视图缓存 |
//! | `toast:<文本>` | 弹一条通知（验证通知层与对话框的上下关系） |
//! | `snap:<路径.png>` | 截图（Metal 回读，锁屏也能拿到） |
//! | `quit` | 退出 |
//!
//! 例：`FALCON_AUTOMATE="ready;select:mojito;new-terminal;wait:2500;type:echo 你好\r;wait:800;snap:/tmp/t.png;quit"`

use std::time::Duration;

use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Entity, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Point, px,
};

use crate::workspace::{ActiveView, AuthPhase, Workspace};

pub fn start(window: AnyWindowHandle, ws: Entity<Workspace>, cx: &mut App) {
    let Ok(script) = std::env::var("FALCON_AUTOMATE") else {
        return;
    };
    let steps: Vec<String> = script.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    cx.spawn(async move |cx| {
        for step in steps {
            log::info!("automate: {step}");
            if let Err(err) = run_step(&step, window, &ws, cx).await {
                log::error!("automate 步骤失败 `{step}`：{err}");
            }
        }
    })
    .detach();
}

/// `\r` `\n` `\t` `\e` 与 `\xHH`（控制键如 Ctrl-U = `\x15`；步骤分隔符 `;` 写成 `\x3b`）
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) => out.push(b as char),
                    Err(_) => {
                        out.push_str("\\x");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn point(arg: &str) -> Option<Point<gpui_kit::Pixels>> {
    let (x, y) = arg.split_once(',')?;
    Some(Point::new(px(x.trim().parse().ok()?), px(y.trim().parse().ok()?)))
}

async fn run_step(step: &str, window: AnyWindowHandle, ws: &Entity<Workspace>, cx: &mut AsyncApp) -> Result<(), String> {
    let (cmd, arg) = step.split_once(':').unwrap_or((step, ""));
    match cmd {
        "wait" => {
            let ms: u64 = arg.parse().map_err(|_| "wait 要毫秒数".to_string())?;
            cx.background_executor().timer(Duration::from_millis(ms)).await;
        }
        "ready" => {
            for _ in 0..150 {
                let ready = cx.update(|cx| {
                    let w = ws.read(cx);
                    w.auth_phase == AuthPhase::Ready && (w.system.is_some() || !w.projects.is_empty())
                });
                if ready {
                    return Ok(());
                }
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
            return Err("15s 内没就绪".into());
        }
        "select" => {
            let found = cx.update(|cx| {
                let id = ws.read(cx).projects.iter().find(|p| p.name == arg).map(|p| p.id.clone());
                if let Some(id) = &id {
                    ws.update(cx, |w, cx| w.select_project(id, cx));
                }
                id.is_some()
            });
            if !found {
                return Err(format!("没有叫 {arg} 的项目"));
            }
        }
        "new-terminal" => {
            let agent = match arg {
                "" => None,
                a => falcon_proto::SESSION_AGENTS.iter().copied().find(|x| x.as_str() == a),
            };
            cx.update(|cx| {
                if let Some(pid) = ws.read(cx).current_project_id() {
                    ws.update(cx, |w, cx| w.new_terminal(&pid, agent, None, cx));
                }
            });
        }
        "type" => {
            let text = unescape(arg);
            let ok = cx.update(|cx| {
                let view = match &ws.read(cx).state.active {
                    ActiveView::Terminal { session_id: id } => ws.read(cx).terminals.get(id).cloned(),
                    _ => None,
                };
                if let Some(view) = &view {
                    view.update(cx, |v, cx| v.automation_input(&text, cx));
                }
                view.is_some()
            });
            if !ok {
                return Err("没有活动终端".into());
            }
        }
        "key" => {
            let ks = Keystroke::parse(arg).map_err(|e| format!("{e:#}"))?;
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_keystroke(ks, cx);
            });
        }
        "action" => {
            let _ = window.update(cx, |_, window, cx| match cx.build_action(arg, None) {
                Ok(action) => window.dispatch_action(action, cx),
                Err(err) => log::error!("动作 {arg} 不存在：{err:#}"),
            });
        }
        "click" | "rclick" => {
            let p = point(arg).ok_or("click 要 x,y")?;
            let button = if cmd == "rclick" { MouseButton::Right } else { MouseButton::Left };
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
            });
            // 命中测试用的是上一帧的 hitbox：hover 才出现的按钮（行尾 ＋ / ⋯）要等 move 之后
            // 重画一帧才有，紧跟着按下会落空
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent { button, position: p, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent { button, position: p, modifiers: Modifiers::default(), click_count: 1 }),
                    cx,
                );
            });
        }
        "drag" => {
            let v: Vec<f32> = arg.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() != 4 {
                return Err("drag 要 x1,y1,x2,y2".into());
            }
            let (a, b) = (Point::new(px(v[0]), px(v[1])), Point::new(px(v[2]), px(v[3])));
            let m = Modifiers::default();
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: a, pressed_button: None, modifiers: m }), cx);
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: a, modifiers: m, click_count: 1, first_mouse: false }),
                    cx,
                );
            });
            for i in 1..=8 {
                let t = i as f32 / 8.0;
                let p = Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                let _ = window.update(cx, |_, window, cx| {
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: Some(MouseButton::Left), modifiers: m }),
                        cx,
                    );
                });
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: b, modifiers: m, click_count: 1 }),
                    cx,
                );
            });
        }
        "scroll" => {
            // 触控板式的像素滚动；dy 为负是往下翻（与 macOS 的自然滚动方向一致）
            let v: Vec<f32> = arg.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() != 4 {
                return Err("scroll 要 x,y,dx,dy".into());
            }
            let position = Point::new(px(v[0]), px(v[1]));
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::ScrollWheel(gpui_kit::ScrollWheelEvent {
                        position,
                        delta: gpui_kit::ScrollDelta::Pixels(Point::new(px(v[2]), px(v[3]))),
                        modifiers: Modifiers::default(),
                        touch_phase: gpui_kit::TouchPhase::Moved,
                    }),
                    cx,
                );
            });
        }
        "move" => {
            let p = point(arg).ok_or("move 要 x,y")?;
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
            });
        }
        #[cfg(feature = "snapshot")]
        "snap" => {
            // render_to_image 截的是上一次画好的帧。锁屏 / 窗口被遮住时显示链路不走，
            // notify 过的内容一直没真正画出来（截出来就是一扇只有光标的空终端），先手动画。
            // 画两帧：画布这类靠 prepaint 量出自己尺寸再排版的视图，第一次出现的那帧是空的
            // （真机上就是晚一帧，看不出来）
            let result = window.update(cx, |_, window, cx| {
                window.draw(cx).clear(cx);
                window.draw(cx).clear(cx);
                window.render_to_image()
            });
            match result {
                Ok(Ok(img)) => img.save(arg).map_err(|e| e.to_string())?,
                Ok(Err(e)) => return Err(format!("{e:#}")),
                Err(e) => return Err(format!("{e:#}")),
            }
        }
        #[cfg(not(feature = "snapshot"))]
        "snap" => return Err("snap 要 --features snapshot（它需要 test-support 的 render_to_image）".into()),
        "frames" => {
            // 锁屏时显示链路不走、根本不渲染，压测量到的只有解析。这里按 60Hz 手动画，模拟
            // 显示链路：排版、字形排布、场景构建的 CPU 都算进来（GPU 提交不在内）
            // `frames:<ms>:refresh` 每帧先 refresh：无视视图缓存，量整棵树重排的开销（悬停、换主题时就是这样）
            let (ms, refresh) = match arg.split_once(':') {
                Some((ms, "refresh")) => (ms, true),
                _ => (arg, false),
            };
            let ms: u64 = ms.parse().map_err(|_| "frames 要毫秒数".to_string())?;
            let until = std::time::Instant::now() + Duration::from_millis(ms);
            let mut took: Vec<Duration> = Vec::new();
            while std::time::Instant::now() < until {
                let t = std::time::Instant::now();
                let _ = window.update(cx, |_, window, cx| {
                    if refresh {
                        window.refresh();
                    }
                    window.draw(cx).clear(cx)
                });
                took.push(t.elapsed());
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            took.sort();
            let at = |q: f64| took.get(((took.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or_default();
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            log::info!(
                "frames: {} 帧，p50 {:.2}ms · p95 {:.2}ms · 最慢 {:.2}ms",
                took.len(),
                ms(at(0.5)),
                ms(at(0.95)),
                ms(at(1.0))
            );
        }
        "toast" => {
            let _ = window.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt;
                window.push_notification(gpui_kit::component::notification::Notification::info(arg.to_string()), cx);
            });
        }
        "quit" => cx.update(|cx| cx.quit()),
        other => return Err(format!("不认识的步骤 {other}")),
    }
    Ok(())
}
