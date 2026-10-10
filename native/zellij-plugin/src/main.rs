//! falcon 的滚动位置插件（ADR 0019）。
//!
//! zellij 自己知道「视口下方还有多少行」，但只画在面板边框的 `SCROLL: n/m` 里，
//! CLI 没有任何查询口子：list-panes 没有这个字段，dump-screen --full 只给视口及以上
//! （源码注释原话），会话序列化同样不含视口下方。0.45 起插件能订阅
//! `ActivePaneScroll(position, length)`——就是边框上那两个数，变化时推送——还能按
//! pane id 滚动。这个插件把两者包成 CLI 管道上的一问一答，falcon 后端按需
//! `zellij pipe` 过来。
//!
//! 加载方式是会话配置里的 `load_plugins`（后台插件，没有 pane），**不能**靠
//! `zellij pipe --plugin` 现拉：那样拉起的是一个浮动 pane（只是浮动层恰好隐藏着），
//! 实测那样拉起的实例上管道还会一直挂着不退出，拉起它的第一条消息也会因为早于
//! 授权结果而落空。随会话启动还有个好处：从第一刻起就在收 ActivePaneScroll，手上的
//! 值永远是新的（事件只在值变化时才发）。
//!
//! 协议（payload 是一行空格分隔的文本，pane 是 terminal pane 的数字 id）：
//! - `get <pane>`：回一行当前值。
//! - `seek <pane> <position>`：把 pane 滚到「视口下方还剩 position 行」处，等滚完后的
//!   那次事件到了再回一行新值（滚不动、值没变时由定时器兜底回当前值）。
//!
//! 回话一行：`<position> <length>\n`。position = 视口下方的行数（0 = 在底部），
//! length = 视口上方的显示行数 + position，两者与边框上的 `SCROLL: n/m` 同口径。
//! 都为 0 = 没有可滚的历史（备用屏里的全屏程序也是这样）。

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

/// seek 之后等事件的上限（秒）。滚动指令在 screen 线程排队执行、下一帧渲染时才发
/// 事件，正常是几毫秒；目标与当前相同、或 pane 根本滚不动时永远等不到，靠它收尾
const SEEK_SETTLE_SECS: f64 = 0.15;

#[derive(Default)]
struct State {
    granted: bool,
    /// 权限结果到达之前收到的管道，授权后一律放行，见 [`State::pipe`]
    pending: Vec<String>,
    /// 最近一次 ActivePaneScroll。None = 加载以来还没收到过，见 [`initial`]
    last: Option<(usize, usize)>,
    /// 等滚完回话的 seek 管道
    seeking: Vec<String>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        // 权限由后端预写进 zellij 的 permissions.kdl，这里的请求会直接从缓存里批下来，
        // 不会弹授权界面（后台插件弹了也没人能点）
        request_permission(&[
            PermissionType::ReadApplicationState,   // ActivePaneScroll、get_pane_info
            PermissionType::ChangeApplicationState, // scroll_*_in_pane_id
            PermissionType::ReadCliPipes,           // 回话与放行管道
            PermissionType::ReadPaneContents,       // 算初值用的 get_pane_scrollback
        ]);
        subscribe(&[
            EventType::ActivePaneScroll,
            EventType::PermissionRequestResult,
            EventType::Timer,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                for pipe in std::mem::take(&mut self.pending) {
                    unblock_cli_pipe_input(&pipe);
                }
            },
            Event::ActivePaneScroll(scroll) => {
                // position 与 length 都为 0 时 zellij 发的是 None
                self.last = Some(scroll.unwrap_or((0, 0)));
                self.settle_seeks();
            },
            Event::Timer(_) => self.settle_seeks(),
            _ => {},
        }
        false
    }

    /// 后台插件的管道在 pipe() 返回后由 zellij 自动放行（CLI 随即退出），除非这里
    /// 显式 block——seek 就是这样挂住管道、等滚完再回话的。
    fn pipe(&mut self, msg: PipeMessage) -> bool {
        let PipeSource::Cli(pipe) = &msg.source else {
            return false;
        };
        if !self.granted {
            // 授权结果到达之前 output / block 都调不了（会话刚启动的一瞬间），
            // 后端拿到空回话当作「暂时不知道」。记下来只为授权后补一次放行，以防万一
            self.pending.push(pipe.clone());
            return false;
        }
        let payload = msg.payload.as_deref().unwrap_or("");
        let mut words = payload.split_whitespace();
        let op = words.next();
        let pane = words.next().and_then(|w| w.parse::<u32>().ok());
        match (op, pane) {
            (Some("get"), Some(pane)) => {
                let scroll = self.current(pane);
                cli_pipe_output(pipe, &format_line(scroll));
                unblock_cli_pipe_input(pipe);
            },
            (Some("seek"), Some(pane)) => {
                let Some(target) = words.next().and_then(|w| w.parse::<usize>().ok()) else {
                    unblock_cli_pipe_input(pipe);
                    return false;
                };
                let current = self.current(pane);
                seek(pane, current, target);
                block_cli_pipe_input(pipe);
                self.seeking.push(pipe.clone());
                set_timeout(SEEK_SETTLE_SECS);
            },
            _ => unblock_cli_pipe_input(pipe),
        }
        false
    }
}

impl State {
    fn current(&mut self, pane: u32) -> (usize, usize) {
        *self.last.get_or_insert_with(|| initial(pane))
    }

    fn settle_seeks(&mut self) {
        let Some(scroll) = self.last else {
            return;
        };
        let line = format_line(scroll);
        for pipe in self.seeking.drain(..) {
            cli_pipe_output(&pipe, &line);
            unblock_cli_pipe_input(&pipe);
        }
    }
}

fn format_line((position, length): (usize, usize)) -> String {
    format!("{position} {length}\n")
}

/// 没收到过事件时自己算一遍，口径照抄 zellij 的 `Grid::scrollback_position_and_length`：
/// position = lines_below 的条数；length = 视口上方每条逻辑行折成的显示行数之和 + position
/// （上方存的是未折行的整行，`recalculate_scrollback_buffer_count` 按 pane 宽度折算）。
/// 随会话加载时这里基本用不上——会话刚建时还没有历史，(0, 0) 本来就对。
fn initial(pane: u32) -> (usize, usize) {
    let id = PaneId::Terminal(pane);
    let cols = get_pane_info(id).map(|p| p.pane_content_columns).unwrap_or(0);
    let Ok(contents) = get_pane_scrollback(id, true) else {
        return (0, 0);
    };
    let position = contents.lines_below_viewport.len();
    let above: usize = contents
        .lines_above_viewport
        .iter()
        .map(|line| display_rows(UnicodeWidthStr::width(line.as_str()), cols))
        .sum();
    (position, above + position)
}

/// zellij 的 `calculate_row_display_height`
fn display_rows(width: usize, cols: usize) -> usize {
    if cols == 0 || width <= cols {
        1
    } else {
        width.div_ceil(cols)
    }
}

/// 从 current 滚到 target（都是「视口下方的行数」）。两端直接跳；中间先整页后逐行，
/// 拖动滚动条跨几千行时少发几千条指令。整页的步长照抄 zellij 的
/// `scroll_terminal_page_up`：pane 行数减一（留一行上下文）。万一算偏了也无所谓，
/// 滚完的真实位置会回给后端，前端以那个为准。
fn seek(pane: u32, (current, length): (usize, usize), target: usize) {
    let id = PaneId::Terminal(pane);
    if target == 0 {
        scroll_to_bottom_in_pane_id(id);
        return;
    }
    if target >= length {
        scroll_to_top_in_pane_id(id);
        return;
    }
    let page = get_pane_info(id)
        .map(|p| p.pane_rows.saturating_sub(1))
        .filter(|rows| *rows > 0)
        .unwrap_or(1);
    if target > current {
        let n = target - current;
        for _ in 0..n / page {
            page_scroll_up_in_pane_id(id);
        }
        for _ in 0..n % page {
            scroll_up_in_pane_id(id);
        }
    } else {
        let n = current - target;
        for _ in 0..n / page {
            page_scroll_down_in_pane_id(id);
        }
        for _ in 0..n % page {
            scroll_down_in_pane_id(id);
        }
    }
}
