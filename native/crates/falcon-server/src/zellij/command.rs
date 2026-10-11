//! Zellij 命令构造。移植自 `packages/server/src/zellij/command.ts`。
//!
//! 一律返回 argv 数组与 env 列表，由执行层（本地 pty spawn / SSH exec）
//! 负责转义与注入——远端 Unix 与远端 Windows 的转义规则不同，不能在这里拼字符串。
//!
//! env 是**有序**的 `(名, 值)` 列表（TS 的 `Object.entries` 顺序），理由见 [`super::host`]。
//!
//! 这一层没有需要执行器的函数，全部移完；没有留到 S3 的。

use std::sync::LazyLock;

use regex::Regex;

use super::version::ZELLIJ_VERSION;

/// 远端 scrollback 深度。dump-screen 没有 `-S -2000` 这类行数参数，
/// 只能全量或仅当前屏；把 buffer 本身设小，`--full` 就天然限行了。
///
/// 注意这是断线重连后用户能上翻行数的**硬上限**：replay 会整体替换前端缓冲
/// （等同 reset 再写），前端积累的历史保不住，能翻多少全看 dump 有多少。
/// 取 zellij 默认值 10000，并与客户端 scrollback（falcon-term 的 `TermOptions`）、服务端 RingBuffer
/// 容量保持匹配（三者取最小生效）。
const SCROLL_BUFFER: u32 = 10000;

/// sessionId（UUID）→ Zellij session 名。
///
/// 截断到 16 个 hex 是为了绕开 socket 路径长度限制：socket 路径为
/// `$ZELLIJ_SOCKET_DIR/contract_version_1/<name>`，而上限在 macOS 只有 104 字符
/// （Linux 108 / Windows 256）。完整 `falcon-<uuid>` 是 43 字符，叠加 home 路径后
/// 在用户名稍长的 macOS 上就会触顶。64 bit 随机在单个 socket 目录内足够。
///
/// 单向派生：总是从 sessionId 算出会话名，不做反解。
///
/// 前缀保持 `mj-`：产品曾名 Mojito，改前缀会让已有持久会话接不回去。
pub fn zellij_session_name(session_id: &str) -> String {
    let stripped = session_id.replace('-', "");
    format!("mj-{}", slice_utf16(&stripped, 16))
}

/// 判断一个 Zellij session 名是否由 falcon 创建（孤儿会话检测用）。
/// 即 `/^mj-[0-9a-f]{16}$/`
pub fn is_falcon_session(name: &str) -> bool {
    name.strip_prefix("mj-")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// zellij 在宿主机上用到的路径。[`super::host::HostLayout`] `Deref` 到它
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZellijPaths {
    /// 二进制完整路径
    pub bin: String,
    /// ~/.falcon/zellij/config/config.kdl —— 见 CONFIG_BODY
    pub config_file: String,
    /// ~/.falcon/zellij/sock
    pub socket_dir: String,
    /// ~/.falcon/zellij/config —— 保持为空目录即可保证零配置文件运行
    pub config_dir: String,
    /// ~/.falcon/zellij/data
    pub data_dir: String,
    /// ~/.falcon/zellij/cache —— 仅 Linux 生效（XDG_CACHE_HOME）
    pub cache_dir: String,
    /// ~/.falcon/zellij/layouts/falcon.kdl —— 见 LAYOUT_BODY
    pub layout_file: String,
    /// ~/.falcon/zellij/config/scroll.kdl —— 带滚动位置插件的会话用的配置，见 scroll_config_body
    pub scroll_config_file: String,
    /// ~/.falcon/zellij/plugins/falcon-scroll.wasm —— 滚动位置插件（ADR 0019）
    pub scroll_plugin_file: String,
}

/// 我们自己的 layout。
///
/// `layout` 单独一行 = 无 tab bar、无 status bar、无 plugin pane，
/// 终端看起来就是个普通终端。这与内置的 no-plugins.kdl 相同，但**不能直接引用
/// 内置名字**：`options --default-layout` 走 PathBuf 解析，只在 layout_dir 里找
/// 文件，够不到编译进二进制的 assets（实测报 `IoError: The layout was not found`），
/// 而 `--layout-dir` 又不是顶层 flag。所以安装时写一份到宿主机，用绝对路径引用。
///
/// `keybinds clear-defaults=true` 是实测踩出来的关键一条：`--default-mode locked`
/// **只在新建会话时生效**，接回已存在的会话时模式退回 normal，用户的按键会被
/// Zellij 当成快捷键吃掉——表现为断线重连后终端"卡死"，而新建会话一切正常。
/// 清空全部键位后就不再依赖模式状态，所有按键无条件透传给 shell。
pub const LAYOUT_BODY: &str = "layout\n";

/// 我们独占的 config.kdl，写进 ZELLIJ_CONFIG_DIR。
///
/// 原本想让配置目录保持为空以实现"零配置文件"，但实测证明**必须有这个文件**：
/// `options --default-mode locked` 这类 CLI 参数只在**新建**会话时生效，attach
/// 到已存在的会话时会被会话自身的模式状态覆盖，而最后一个客户端断开后模式会
/// 回到 normal。症状是断线重连后终端像卡死一样不响应键盘（按键全被 Zellij 当
/// 快捷键吃掉），而新建会话完全正常——极难排查。
///
/// 配置文件里的 default_mode 每次客户端连接都会读，不受会话状态影响。
/// keybinds clear-defaults 是纵深防御：万一模式没设上，至少所有键位都是空的，
/// 用户按 Ctrl+p / Ctrl+n 不会莫名掉进 pane 模式。
///
/// scroll_mode_sync false 是 0.45 起必须的（#5299 引入、#5532 才给开关）：默认
/// 开着时滚轮上翻会把会话隐式切进 Scroll 模式，而我们清空了全部键位，Scroll 模式
/// 下没绑定的键**直接被吞掉**——实测（0.45.1）滚一下再敲 `echo x⏎`，shell 什么都
/// 没收到，视口也不回底，看上去就是终端卡死。关掉后与 0.44 一致：一敲键就回到底部、
/// 按键照常透传。0.44.3 读到这一行不报错（实测），新旧二进制共用同一份文件无碍。
///
/// 目录是 falcon 独占的，不会读到用户自己的 Zellij 配置。
///
/// （TS 里是模块加载时算好的常量；这里用 `LazyLock`，让 SCROLL_BUFFER 只写一处。）
pub static CONFIG_BODY: LazyLock<String> = LazyLock::new(|| config_body("pane_frames false", ""));

/// 带滚动位置插件的会话用的配置（ADR 0019），经 `--config` 指给 zellij，与 config.kdl
/// 只差两处：
///
/// - 边框样式是 titles 而不是 `pane_frames false`：插件要的 ActivePaneScroll 事件只在
///   titles 样式（且 tab 里只有一个 pane）时才发，`pane_frames false` 会把样式压成 None。
///   单 pane 时 titles 样式不画标题行、滚动时也不画 `SCROLL:` 指示（实测 0.45.1），
///   外观与关掉边框一致。
/// - `load_plugins` 让插件随会话以后台插件启动（没有 pane）。不能靠 `zellij pipe
///   --plugin` 现拉，理由见插件源码顶部。
///
/// **为什么是另一个文件而不是改 config.kdl**：升级前建的会话跑在 0.44.3 的 server 上，
/// 接回时客户端会把配置带过去，0.44.3 不认识 titles 样式、又没了 `pane_frames false`，
/// 实测会给老会话画上整圈边框。所以老会话照旧读 config.kdl，只有建会话时插件已就位的
/// 新会话才指向这份（sessions.scroll_plugin 记着），zellij 的配置热重载也各管各的文件。
pub fn scroll_config_body(plugin_file: &str) -> String {
    config_body(
        "pane_frame_style \"titles\"",
        &format!("load_plugins {{\n    {}\n}}\n", kdl_string(&format!("file:{plugin_file}"))),
    )
}

fn config_body(frames: &str, extra: &str) -> String {
    let scroll_buffer = format!("scroll_buffer_size {SCROLL_BUFFER}");
    [
        "default_mode \"locked\"",
        frames,
        "simplified_ui true",
        "session_serialization false",
        &scroll_buffer,
        "scroll_mode_sync false",
        "mouse_mode true",
        "show_startup_tips false",
        "show_release_notes false",
        "keybinds clear-defaults=true {",
        "}",
        extra,
    ]
    .join("\n")
}

/// KDL 字符串字面量：反斜杠与双引号转义（只给 POSIX 路径用，Windows 不走插件）
fn kdl_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 运行时环境变量。
///
/// 四处写入目标全部指进 ~/.falcon/zellij/，让"删掉 ~/.falcon 即完全卸载"尽量成立。
/// 注意 cache 只有 Linux 能靠 XDG_CACHE_HOME 收拢——macOS/Windows 的 cache 路径
/// 由 directories crate 硬编码，无法重定向，会有残留（已在 README 注明）。
///
/// 顺序与 TS 对象字面量一致：发到宿主机的命令串按这个顺序展开。
pub fn zellij_env(paths: &ZellijPaths) -> Vec<(String, String)> {
    vec![
        ("ZELLIJ_SOCKET_DIR".to_string(), paths.socket_dir.clone()),
        ("ZELLIJ_CONFIG_DIR".to_string(), paths.config_dir.clone()),
        ("XDG_CACHE_HOME".to_string(), paths.cache_dir.clone()),
    ]
}

fn global_args(paths: &ZellijPaths) -> Vec<String> {
    vec!["--data-dir".to_string(), paths.data_dir.clone()]
}

/// 会话用哪套配置。scroll = 带滚动位置插件（见 [`scroll_config_body`]），否则是 config.kdl。
///
/// TS 里三个字段都是可选的、按真假判断：`cwd` / `shell` 为空串与缺省同义，这里也一样。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionProfile {
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub scroll: bool,
}

/// attach 用的全局参数：带插件的会话另指配置文件
fn attach_global_args(paths: &ZellijPaths, opts: &SessionProfile) -> Vec<String> {
    let mut args = global_args(paths);
    if opts.scroll {
        args.push("--config".to_string());
        args.push(paths.scroll_config_file.clone());
    }
    args
}

/// 会话级 options。作为 `attach` 的子命令附加，CLI 值优先于配置文件。
///
/// - default-mode locked：**最关键的一条**。不设它 Zellij 会抢走 Ctrl+p / Ctrl+n /
///   Ctrl+o / Ctrl+t 做模式切换，用户在 shell 里按 Ctrl+p 调历史会掉进 pane 模式。
/// - session-serialization false：关掉复活序列化。开着的话被杀的 session 会以
///   `(EXITED - attach to resurrect)` 留在 `zellij ls` 里，污染存活判断。
fn session_options(paths: &ZellijPaths, opts: &SessionProfile) -> Vec<String> {
    // 边框样式两套配置各不相同，理由见 scroll_config_body。带插件的会话**不能**再传
    // --pane-frames false：它会把 titles 样式压成 None，插件就收不到滚动事件
    let frames: [&str; 2] = if opts.scroll { ["--pane-frame-style", "titles"] } else { ["--pane-frames", "false"] };
    let scroll_buffer = SCROLL_BUFFER.to_string();
    let mut args: Vec<String> = [
        "options",
        "--default-layout",
        // 必须是绝对路径，理由见 LAYOUT_BODY
        &paths.layout_file,
        "--default-mode",
        "locked",
        frames[0],
        frames[1],
        "--simplified-ui",
        "true",
        "--session-serialization",
        "false",
        "--scroll-buffer-size",
        &scroll_buffer,
        // 理由见 CONFIG_BODY；两处都写，与其它项同一口径（CLI 管新建，config 管接回）
        "--scroll-mode-sync",
        "false",
        // 鼠标上报**只能**由 config.kdl 的 mouse_mode true 开启，这里绝不能传
        // --mouse-mode。实测（0.44.3）：CLI 传 --mouse-mode true 时客户端反而
        // 永远不发 ?1000h（与传 false 同效，疑似上游 bug）；不传时每次 attach
        // 都按 config 开启。开鼠标的动机：Zellij 常驻备用屏（无 scrollback），
        // 客户端照 xterm 的 alternateScroll，把"备用屏 + 无鼠标上报"的滚轮降级成
        // ↑/↓ 方向键——在 shell 提示符下滚轮变成翻命令历史。开启后滚轮交给 Zellij
        // 滚它自己的 scroll buffer（上限 SCROLL_BUFFER 行）。代价：前端本地选区需按住
        // Shift 拖拽。
        // 这两条是实测踩出来的：Zellij 默认会先显示一屏 "Zellij Tip #N" 启动提示，
        // 挡在 shell 前面等用户按键关闭。表现为会话建成了、hasSession 为真、
        // 但屏幕空白且输入毫无反应——不关掉整个终端就是废的。
        "--show-startup-tips",
        "false",
        "--show-release-notes",
        "false",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(cwd) = opts.cwd.as_deref().filter(|s| !s.is_empty()) {
        args.push("--default-cwd".to_string());
        args.push(cwd.to_string());
    }
    // --default-shell 是 PathBuf，不接受参数（`bash -l` 会失败）。
    // UI 上已说明"需要参数请指向自己的脚本"。
    if let Some(shell) = opts.shell.as_deref().filter(|s| !s.is_empty()) {
        args.push("--default-shell".to_string());
        args.push(shell.to_string());
    }
    args
}

/// 建立或接回会话。
///
/// 始终带 `--create`：带它走精确匹配，不带则是**前缀匹配**——
/// 前缀歧义会命中错误的 session 或直接 exit(1)，程序化调用绝不能依赖。
pub fn attach_args(paths: &ZellijPaths, session_id: &str, opts: &SessionProfile) -> Vec<String> {
    let mut args = attach_global_args(paths, opts);
    args.push("attach".to_string());
    args.push(zellij_session_name(session_id));
    args.push("--create".to_string());
    args.extend(session_options(paths, opts));
    args
}

/// 后台建一个 detached 会话（无需 PTY）。Windows 远端专用：会话级 options 只在
/// 创建时生效，所以这里必须带全 session_options——后续 PTY attach 时的同名参数
/// 对已存在的会话是无效的，真正决定 layout / shell / cwd 的是这一条。
///
/// 不幂等：会话已存在时退出码为 1（实测），调用方要先用 hasSession 挡一道。
pub fn create_background_args(paths: &ZellijPaths, session_id: &str, opts: &SessionProfile) -> Vec<String> {
    let mut args = attach_global_args(paths, opts);
    args.push("attach".to_string());
    args.push(zellij_session_name(session_id));
    args.push("--create-background".to_string());
    args.extend(session_options(paths, opts));
    args
}

/// 列出会话。用 `--no-formatting` 而不是 `--short`：
/// short 模式**不区分死活**，只打名字；要判断存活必须看
/// `(EXITED - attach to resurrect)` 后缀。
///
/// 退出码：有 session → 0；一个都没有 → 1（stderr 输出 "No active zellij sessions found."）。
/// 所以非零退出码不代表出错。
pub fn list_args(paths: &ZellijPaths) -> Vec<String> {
    let mut args = global_args(paths);
    args.push("list-sessions".to_string());
    args.push("--no-formatting".to_string());
    args
}

/// 解析 `zellij ls --no-formatting` 的输出，返回存活（非 EXITED）的会话名
pub fn parse_live_sessions(stdout: &str) -> Vec<String> {
    let mut live = Vec::new();
    for line in stdout.split('\n') {
        let t = js_trim(line);
        if t.is_empty() {
            continue;
        }
        if t.contains("EXITED") {
            continue;
        }
        if let Some(name) = js_split_ws(t).into_iter().next().filter(|n| !n.is_empty()) {
            live.push(name.to_string());
        }
    }
    live
}

/// 列出会话里的 pane。用于找出终端 pane 的 id——见 [`dump_screen_args`]。
pub fn list_panes_args(paths: &ZellijPaths, session_id: &str) -> Vec<String> {
    let mut args = global_args(paths);
    args.extend(["--session".to_string(), zellij_session_name(session_id), "action".into(), "list-panes".into()]);
    args
}

/// 从 `list-panes` 输出里取第一个终端 pane 的 id。
///
/// 输出形如：
/// ```text
///   PANE_ID  TYPE  TITLE
///   plugin_0  plugin  (.) - zellij:link
///   terminal_0  terminal  Pane #1
/// ```
pub fn parse_terminal_pane_id(stdout: &str) -> Option<String> {
    for line in stdout.split('\n') {
        let cols = js_split_ws(js_trim(line));
        let id = cols.first().copied().unwrap_or("");
        if !id.is_empty() && cols.get(1).copied() == Some("terminal") {
            return Some(id.to_string());
        }
    }
    None
}

/// 列出已连接客户端与其聚焦 pane 的前台命令。用于关 tab 前判断"有没有程序在跑"。
pub fn list_clients_args(paths: &ZellijPaths, session_id: &str) -> Vec<String> {
    let mut args = global_args(paths);
    args.extend(["--session".to_string(), zellij_session_name(session_id), "action".into(), "list-clients".into()]);
    args
}

/// 从 `list-clients` 输出里取前台命令。
///
/// 输出形如：
/// ```text
///   CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND
///   1         terminal_0     sleep 300
/// ```
///
/// 实测（macOS，0.44.3）：空闲 shell 时 RUNNING_COMMAND 是字面量 "N/A"，
/// 有前台程序时是完整命令行；没有客户端连接时只有表头。命令行可能含空格，
/// 所以第三列起要整段拼回。聚焦在 plugin pane 上（理论上 falcon 的单 pane
/// layout 不会发生）没有可言的前台命令，同样按"不知道"处理。
pub fn parse_client_running_command(stdout: &str) -> Option<String> {
    for line in stdout.split('\n') {
        let t = js_trim(line);
        if t.is_empty() || t.starts_with("CLIENT_ID") {
            continue;
        }
        let cols = js_split_ws(t);
        if !cols.get(1).is_some_and(|pane_id| pane_id.starts_with("terminal")) {
            continue;
        }
        let command = cols.get(2..).map(|rest| rest.join(" ")).unwrap_or_default();
        if command.is_empty() || command == "N/A" {
            return None;
        }
        return Some(command);
    }
    None
}

/// 抓取历史用于重建 Scrollback。
/// `--ansi` 保留颜色（等价 tmux capture-pane -e），`--full` 含 scrollback。
///
/// **必须显式给 `--pane-id`**：不给的话抓的是"当前聚焦 pane"，而在没有客户端
/// 连接时（后端重启后接回，正是我们最需要它的时刻）焦点会落在 Zellij 后台的
/// plugin pane 上（`zellij:link` / `About Zellij` 即使 layout 里没写也会加载），
/// dump 出来是空的。实测：不给 pane-id 时 detach 状态下只能抓到一个 "\r\n"。
///
/// 已知缺陷 #5311：alternate-screen 程序（vim/htop）被 resize 后，
/// 输出会带重复的陈旧行，重建会有视觉瑕疵。暂无解。
pub fn dump_screen_args(paths: &ZellijPaths, session_id: &str, pane_id: &str) -> Vec<String> {
    let mut args = global_args(paths);
    args.extend([
        "--session".to_string(),
        zellij_session_name(session_id),
        "action".into(),
        "dump-screen".into(),
        "--ansi".into(),
        "--full".into(),
        "--pane-id".into(),
        pane_id.to_string(),
    ]);
    args
}

// ---------------- 滚动位置插件（ADR 0019） ----------------

/// 插件监听的管道名；插件源码在 packages/server/zellij-plugin
const SCROLL_PIPE: &str = "falcon-scroll";

/// 滚动位置：两个数与 zellij 边框上的 `SCROLL: position/length` 同口径。
///
/// 用 u32：与线上的 `ServerMessage::Scroll` 同宽（zellij 的显示行数，上限是
/// SCROLL_BUFFER 那个量级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollPosition {
    /// 视口下方的显示行数，0 = 在底部
    pub position: u32,
    /// 视口上方的显示行数 + position；0 = 没有可滚的历史（含备用屏里的全屏程序）
    pub length: u32,
}

/// 问插件要滚动位置（`get`），或让它滚到「视口下方还剩 position 行」处（`seek`，
/// 回的是滚完后的位置）。pane 是 terminal pane 的数字 id，见 [`parse_terminal_pane_number`]。
///
/// **按名字广播，不带 `--plugin`**：带上的话，会话里没有这个插件时（升级前建的老会话、
/// 插件没部署上）zellij 会现场拉起一个浮动 pane 实例，而且那条管道会挂着不退出（实测）。
/// 广播时没人认领就直接放行，回话为空。
///
/// 执行方必须给 stdin 一个 EOF：stdin 不是终端时 CLI 收到放行后还要把 stdin 读到头
/// 才退出（zellij-client 的 pipe_client），SSH exec 与默认的 spawn 都不会自己关。
///
/// `seek` 是 f64：WS 消息里的 seek 是任意 JSON 数（ws.ts 只保证有限且 ≥ 0），这里照
/// `Math.max(0, Math.round(seek))` 取整并夹到非负。
pub fn scroll_pipe_args(paths: &ZellijPaths, session_id: &str, pane: u32, seek: Option<f64>) -> Vec<String> {
    let payload = match seek {
        None => format!("get {pane}"),
        Some(seek) => format!("seek {pane} {}", js_non_negative_round(seek)),
    };
    let mut args = global_args(paths);
    args.extend([
        "--session".to_string(),
        zellij_session_name(session_id),
        "pipe".into(),
        "--name".into(),
        SCROLL_PIPE.into(),
        "--".into(),
        payload,
    ]);
    args
}

/// 插件的回话一行 `<position> <length>`；空（会话里没有插件）或认不出就是 None。
///
/// 即 TS 的 `/^(\d+) (\d+)$/`（JS 的 `\d` 只认 ASCII 数字）。数超出 u32 也当认不出。
pub fn parse_scroll_reply(stdout: &str) -> Option<ScrollPosition> {
    let (a, b) = js_trim(stdout).split_once(' ')?;
    let position = parse_ascii_u32(a)?;
    let length = parse_ascii_u32(b)?;
    (position <= length).then_some(ScrollPosition { position, length })
}

/// `terminal_3` → 3。[`scroll_pipe_args`] 要的是数字 id。即 TS 的 `/^terminal_(\d+)$/`
pub fn parse_terminal_pane_number(pane_id: &str) -> Option<u32> {
    parse_ascii_u32(pane_id.strip_prefix("terminal_")?)
}

/// 插件要的权限，与插件 load() 里 request_permission 的清单一致
const SCROLL_PERMISSIONS: [&str; 4] = ["ReadApplicationState", "ChangeApplicationState", "ReadCliPipes", "ReadPaneContents"];

/// 宿主机的系统，只分 permissions.kdl 位置要的两种（TS 的 `"linux" | "darwin"`）。
/// 由 `sessions::scroll_plugin::target_os` 从 zellij target 得出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    Darwin,
}

impl HostOs {
    pub fn as_str(self) -> &'static str {
        match self {
            HostOs::Linux => "linux",
            HostOs::Darwin => "darwin",
        }
    }
}

/// zellij 的插件授权缓存 permissions.kdl 在宿主机上的位置。
///
/// 位置由 directories crate 的 ProjectDirs 定：Linux 跟 XDG_CACHE_HOME 走（zellij_env 把它
/// 指进了 falcon 自己的 cache 目录），macOS 是写死的 ~/Library/Caches/… 改不了——会和
/// 用户自己的 zellij 共用这个文件，所以只追加、不覆盖。
pub fn scroll_permissions_file(paths: &ZellijPaths, os: HostOs, home: &str) -> String {
    match os {
        HostOs::Darwin => format!(
            "{}/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl",
            home.trim_end_matches('/')
        ),
        HostOs::Linux => format!("{}/zellij/permissions.kdl", paths.cache_dir),
    }
}

/// 预授权条目。后台插件请求权限时 zellij 先查这个缓存，命中就直接批；没命中会弹授权
/// 界面——后台插件没有 pane，弹了也没人能点，插件就永远拿不到权限。
///
/// 键是插件路径**本身**，不带 `file:` 前缀（RunPluginLocation::File 的 Display 就是
/// 路径；带前缀的话匹配不上，实测踩过）。
pub fn scroll_permissions_entry(plugin_file: &str) -> String {
    let body: String = SCROLL_PERMISSIONS.iter().map(|p| format!("    {p}\n")).collect();
    format!("{} {{\n{body}}}\n", kdl_string(plugin_file))
}

/// 彻底销毁会话（对应 Terminate 语义）。
///
/// 用 delete-session 而非 kill-session：后者保留复活数据，会在 `ls` 里留下
/// EXITED 条目。`--force` 表示还活着就先杀再删。
pub fn delete_session_args(paths: &ZellijPaths, session_id: &str) -> Vec<String> {
    let mut args = global_args(paths);
    args.extend(["delete-session".to_string(), zellij_session_name(session_id), "--force".into()]);
    args
}

/// 安装后的握手验证：跑得起来才算装好（能挡住 noexec 和架构不符）
pub fn version_args() -> Vec<String> {
    vec!["--version".to_string()]
}

/// TS 的 `/zellij\s+(\d+\.\d+\.\d+)/i`。几处 JS 语义要照搬：
/// - 没有 `u` 标志的 `/i` 只在 ASCII 字母间折叠大小写（Rust 的 `(?i)` 是 Unicode 折叠），
///   所以字母逐个写成 `[zZ]`；
/// - `\s` 是 ECMAScript 的空白集合（含 U+FEFF、不含 U+0085），与 Rust 的 `\s` 不同；
/// - `\d` 只认 ASCII 数字。
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"[zZ][eE][lL][lL][iI][jJ][\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]+([0-9]+\.[0-9]+\.[0-9]+)",
    )
    .expect("VERSION_RE")
});

/// `zellij --version` 输出形如 `zellij 0.44.3`
pub fn parse_version(stdout: &str) -> Option<String> {
    VERSION_RE.captures(stdout).map(|c| c[1].to_string())
}

pub fn version_matches(stdout: &str) -> bool {
    parse_version(stdout).as_deref() == Some(ZELLIJ_VERSION)
}

// ---------------- JS 语义的私有小工具 ----------------
//
// falcon-core 的 `js.rs` 有同样的东西，但它是 crate 私有的；falcon-server 里还没有
// 共用的位置（lib.rs / mod.rs 不归这一轮改），先各留一份。shells.rs 里也有一份。

/// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
/// （含 U+FEFF、不含 U+0085，与 Rust 的 `char::is_whitespace` 恰好相反）。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{a}'
            | '\u{b}'
            | '\u{c}'
            | '\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `String.prototype.trim`
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// 对**已 trim 过**的串做 `s.split(/\s+/)`。没有首尾空白时，按空白游程切等于按单个
/// 空白切再丢掉空段；唯一的差别是空串：JS 给 `[""]`，这里给 `[]`——调用方都把
/// "第一列为空"与"没有第一列"同样处理。
fn js_split_ws(trimmed: &str) -> Vec<&str> {
    trimmed.split(is_js_whitespace).filter(|s| !s.is_empty()).collect()
}

/// `s.slice(0, max)`：按 UTF-16 码元截断。截断点落在代理对中间时 JS 会留下半个代理，
/// Rust 的 `str` 表示不了，这里整个字符不要（会话 id 是 UUID，碰不到）。
fn slice_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

/// 非空且全是 ASCII 数字（JS 的 `\d+`）再转 u32；超出 u32 给 None
fn parse_ascii_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `String(Math.max(0, Math.round(x)))`。
///
/// 负数（含 .5）四舍五入后再夹到 0，结果都是 0，所以只需关心非负数：那里 JS 的
/// "往 +∞ 舍"与 `f64::round` 的"远离零舍"一致。NaN / Infinity 照 JS 打印（调用方已挡掉，
/// 只为不在这里编出别的值）；≥ 1e21 时 JS 改用指数记法。
fn js_non_negative_round(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    let r = x.round();
    // 不用 f64::max：它对 ±0 不保证给哪个，而 Rust 会把 -0 打印成 "-0"
    let v = if r > 0.0 { r } else { 0.0 };
    if v.is_infinite() {
        "Infinity".to_string()
    } else if v < 1e21 {
        // 与 JS 的 Number#toString 同一套"最短往返位数 + 补零"：2^53 以上的整数
        // JS 打 123456789012345680000，不是精确值 123456789012345667584
        format!("{v}")
    } else {
        // Rust 的 `{:e}` 给 `1e21` / `1.5e22`，JS 是 `1e+21` / `1.5e+22`
        format!("{v:e}").replacen('e', "e+", 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zellij::host::{HostKind, HostLayout, host_layout};
    use crate::zellij::version::ZellijTarget;

    const SESSION_ID: &str = "0f0e0d0c-0b0a-0908-0706-050403020100";

    fn posix_layout() -> HostLayout {
        host_layout(HostKind::Posix, "/home/fay/.falcon", ZellijTarget::X86_64LinuxMusl)
    }

    /// `args[args.indexOf(flag) + 1]`
    fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let i = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag} not in {args:?}"));
        &args[i + 1]
    }

    fn index_of(args: &[String], s: &str) -> usize {
        args.iter().position(|a| a == s).unwrap_or_else(|| panic!("{s} not in {args:?}"))
    }

    fn has(args: &[String], s: &str) -> bool {
        args.iter().any(|a| a == s)
    }

    fn profile(shell: &str, scroll: bool) -> SessionProfile {
        SessionProfile { cwd: None, shell: Some(shell.into()), scroll }
    }

    // ---- parseTerminalPaneId ----

    /// skips the header and plugin panes
    #[test]
    fn parse_terminal_pane_id_skips_the_header_and_plugin_panes() {
        let stdout = [
            "PANE_ID  TYPE  TITLE",
            "plugin_0  plugin  (.) - zellij:link",
            "terminal_0  terminal  Chrome PWA Install App Feature Support - grok",
        ]
        .join("\n");
        assert_eq!(parse_terminal_pane_id(&stdout).as_deref(), Some("terminal_0"));
    }

    /// returns null when there is no terminal pane
    #[test]
    fn parse_terminal_pane_id_returns_null_when_there_is_no_terminal_pane() {
        assert_eq!(parse_terminal_pane_id("PANE_ID  TYPE  TITLE\n"), None);
        assert_eq!(parse_terminal_pane_id(""), None);
    }

    // ---- parseClientRunningCommand ----

    const CLIENTS_HEADER: &str = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND";

    /// 命令行含空格时第三列起整段拼回
    #[test]
    fn parse_client_running_command_joins_columns_from_the_third_on() {
        let stdout = [CLIENTS_HEADER, "1         terminal_0     sleep 300"].join("\n");
        assert_eq!(parse_client_running_command(&stdout).as_deref(), Some("sleep 300"));
    }

    /// 空闲 shell 的 N/A 按'没有程序'处理
    #[test]
    fn parse_client_running_command_treats_idle_na_as_nothing() {
        let stdout = [CLIENTS_HEADER, "1         terminal_0     N/A"].join("\n");
        assert_eq!(parse_client_running_command(&stdout), None);
    }

    /// 没有客户端连接时只有表头
    #[test]
    fn parse_client_running_command_header_only_without_clients() {
        assert_eq!(parse_client_running_command(&format!("{CLIENTS_HEADER}\n")), None);
        assert_eq!(parse_client_running_command(""), None);
    }

    /// 聚焦在 plugin pane 上没有可言的前台命令
    #[test]
    fn parse_client_running_command_plugin_pane_has_no_foreground_command() {
        let stdout = [CLIENTS_HEADER, "1         plugin_3       zellij:configuration"].join("\n");
        assert_eq!(parse_client_running_command(&stdout), None);
    }

    // ---- createBackgroundArgs ----

    /// creates detached with the full session options
    #[test]
    fn create_background_args_creates_detached_with_the_full_session_options() {
        let layout = host_layout(HostKind::Windows, "C:\\Users\\fay\\.falcon", ZellijTarget::X86_64WindowsMsvc);
        let args = create_background_args(
            &layout,
            SESSION_ID,
            &SessionProfile {
                cwd: Some("C:\\code".into()),
                shell: Some("C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe".into()),
                scroll: false,
            },
        );

        assert!(has(&args, &zellij_session_name(SESSION_ID)));
        assert!(has(&args, "--create-background"));
        // 会话级 options 只在创建时生效，必须全量出现在这里（而不是只在 attach 时给）
        assert!(index_of(&args, "options") > index_of(&args, "--create-background"));
        assert_eq!(after(&args, "--default-layout"), layout.layout_file);
        assert_eq!(after(&args, "--default-cwd"), "C:\\code");
        assert_eq!(after(&args, "--default-shell"), "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    }

    // ---- 滚动位置插件的会话配置（ADR 0019） ----

    /// 老配置照旧关边框，不碰插件
    #[test]
    fn scroll_config_old_config_keeps_frames_off_and_no_plugin() {
        assert!(CONFIG_BODY.split('\n').any(|l| l == "pane_frames false"));
        assert!(!CONFIG_BODY.contains("load_plugins") && !CONFIG_BODY.contains("pane_frame_style"));
    }

    /// 带插件的配置用 titles 样式并随会话加载插件，其余与老配置相同
    #[test]
    fn scroll_config_uses_titles_and_loads_plugin_rest_same_as_old() {
        let layout = posix_layout();
        let body = scroll_config_body(&layout.scroll_plugin_file);
        // pane_frames false 会把样式压成 None，插件就收不到 ActivePaneScroll
        assert!(!body.contains("pane_frames"));
        assert!(body.split('\n').any(|l| l == "pane_frame_style \"titles\""));
        assert!(body.contains("load_plugins {\n    \"file:/home/fay/.falcon/zellij/plugins/falcon-scroll.wasm\"\n}"));
        // /pane_frame|load_plugins|falcon-scroll|^}$/
        let common = |b: &str| -> Vec<String> {
            b.split('\n')
                .filter(|l| {
                    !(l.contains("pane_frame") || l.contains("load_plugins") || l.contains("falcon-scroll") || *l == "}")
                })
                .map(str::to_string)
                .collect()
        };
        assert_eq!(common(&body), common(&CONFIG_BODY));
    }

    /// 插件路径里的引号与反斜杠按 KDL 转义
    #[test]
    fn scroll_config_escapes_quotes_and_backslashes_in_plugin_path() {
        assert!(scroll_config_body("/a \"b\"\\c.wasm").contains("\"file:/a \\\"b\\\"\\\\c.wasm\""));
    }

    /// 老会话 attach：关边框、读默认 config.kdl
    #[test]
    fn scroll_config_old_session_attach_frames_off_default_config() {
        let args = attach_args(&posix_layout(), SESSION_ID, &profile("/bin/zsh", false));
        assert!(!has(&args, "--config"));
        assert_eq!(after(&args, "--pane-frames"), "false");
        assert!(!has(&args, "--pane-frame-style"));
    }

    /// 带插件的会话 attach：--config 指 scroll.kdl 且在 attach 之前，titles 样式，不传 --pane-frames
    #[test]
    fn scroll_config_plugin_session_attach_points_config_before_attach() {
        let layout = posix_layout();
        let args = attach_args(&layout, SESSION_ID, &profile("/bin/zsh", true));
        assert_eq!(after(&args, "--config"), layout.scroll_config_file);
        assert!(index_of(&args, "--config") < index_of(&args, "attach"));
        assert_eq!(after(&args, "--pane-frame-style"), "titles");
        assert!(!has(&args, "--pane-frames"));
        // scroll_mode_sync 两套都要关（0.45 默认开着会吞按键）
        assert_eq!(after(&args, "--scroll-mode-sync"), "false");
    }

    // ---- scrollPipeArgs ----

    /// 按名字广播、不带 --plugin（没插件的会话不能被现拉一个浮动实例）
    #[test]
    fn scroll_pipe_args_broadcasts_by_name_without_plugin_flag() {
        let args = scroll_pipe_args(&posix_layout(), SESSION_ID, 0, None);
        assert!(!has(&args, "--plugin"));
        assert_eq!(after(&args, "--session"), zellij_session_name(SESSION_ID));
        assert_eq!(after(&args, "--name"), "falcon-scroll");
        assert_eq!(&args[args.len() - 2..], ["--", "get 0"]);
    }

    /// seek 取整并夹到非负
    #[test]
    fn scroll_pipe_args_seek_rounds_and_clamps_to_non_negative() {
        let layout = posix_layout();
        assert_eq!(scroll_pipe_args(&layout, SESSION_ID, 3, Some(120.6)).last().unwrap(), "seek 3 121");
        assert_eq!(scroll_pipe_args(&layout, SESSION_ID, 3, Some(-5.0)).last().unwrap(), "seek 3 0");
    }

    // ---- parseScrollReply ----

    /// 一行两个数
    #[test]
    fn parse_scroll_reply_two_numbers_on_a_line() {
        assert_eq!(parse_scroll_reply("97 173\n"), Some(ScrollPosition { position: 97, length: 173 }));
        assert_eq!(parse_scroll_reply("0 0\n"), Some(ScrollPosition { position: 0, length: 0 }));
    }

    /// 空回话（会话里没有插件）与乱码都是 null
    #[test]
    fn parse_scroll_reply_empty_and_garbage_are_null() {
        assert_eq!(parse_scroll_reply(""), None);
        assert_eq!(parse_scroll_reply("hello"), None);
        assert_eq!(parse_scroll_reply("1 2 3"), None);
    }

    /// position 不可能大于 length
    #[test]
    fn parse_scroll_reply_position_cannot_exceed_length() {
        assert_eq!(parse_scroll_reply("9 3"), None);
    }

    // ---- parseTerminalPaneNumber ----

    /// 只认 terminal pane
    #[test]
    fn parse_terminal_pane_number_only_terminal_panes() {
        assert_eq!(parse_terminal_pane_number("terminal_0"), Some(0));
        assert_eq!(parse_terminal_pane_number("terminal_12"), Some(12));
        assert_eq!(parse_terminal_pane_number("plugin_0"), None);
    }

    // ---- 插件预授权 ----

    /// Linux 跟 XDG_CACHE_HOME（falcon 自己的 cache 目录），macOS 是写死的 Library/Caches
    #[test]
    fn permissions_file_linux_follows_xdg_macos_is_library_caches() {
        let layout = posix_layout();
        assert_eq!(
            scroll_permissions_file(&layout, HostOs::Linux, "/home/fay"),
            "/home/fay/.falcon/zellij/cache/zellij/permissions.kdl"
        );
        assert_eq!(
            scroll_permissions_file(&layout, HostOs::Darwin, "/Users/fay/"),
            "/Users/fay/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl"
        );
    }

    /// 键是插件路径本身，不带 file: 前缀
    #[test]
    fn permissions_entry_key_is_the_plugin_path_itself() {
        let entry = scroll_permissions_entry("/p/falcon-scroll.wasm");
        assert_eq!(entry.split('\n').next().unwrap(), "\"/p/falcon-scroll.wasm\" {");
        for p in ["ReadApplicationState", "ChangeApplicationState", "ReadCliPipes", "ReadPaneContents"] {
            let line = format!("    {p}");
            assert!(entry.split('\n').any(|l| l == line), "{p} missing in {entry:?}");
        }
        assert!(entry.ends_with("}\n"));
    }

    // ---- 以下不在 command.test.ts 里：逐字节对照 TS 产出（老会话与宿主机上已有文件依赖它） ----

    #[test]
    fn extra_config_bodies_match_ts_byte_for_byte() {
        assert_eq!(
            *CONFIG_BODY,
            "default_mode \"locked\"\npane_frames false\nsimplified_ui true\nsession_serialization false\n\
             scroll_buffer_size 10000\nscroll_mode_sync false\nmouse_mode true\nshow_startup_tips false\n\
             show_release_notes false\nkeybinds clear-defaults=true {\n}\n"
        );
        assert_eq!(
            scroll_config_body("/p/falcon-scroll.wasm"),
            "default_mode \"locked\"\npane_frame_style \"titles\"\nsimplified_ui true\nsession_serialization false\n\
             scroll_buffer_size 10000\nscroll_mode_sync false\nmouse_mode true\nshow_startup_tips false\n\
             show_release_notes false\nkeybinds clear-defaults=true {\n}\n\
             load_plugins {\n    \"file:/p/falcon-scroll.wasm\"\n}\n"
        );
        assert_eq!(
            scroll_permissions_entry("/p/x.wasm"),
            "\"/p/x.wasm\" {\n    ReadApplicationState\n    ChangeApplicationState\n    ReadCliPipes\n    ReadPaneContents\n}\n"
        );
    }

    #[test]
    fn extra_attach_args_match_ts_byte_for_byte() {
        let layout = posix_layout();
        let args = attach_args(
            &layout,
            SESSION_ID,
            &SessionProfile { cwd: Some("/w".into()), shell: Some("/bin/zsh".into()), scroll: true },
        );
        let d = "/home/fay/.falcon/zellij";
        let (data, scroll_kdl, layout_kdl) =
            (format!("{d}/data"), format!("{d}/config/scroll.kdl"), format!("{d}/layouts/falcon.kdl"));
        let expected = [
            "--data-dir", &data, "--config", &scroll_kdl, "attach",
            "mj-0f0e0d0c0b0a0908", "--create", "options", "--default-layout", &layout_kdl,
            "--default-mode", "locked", "--pane-frame-style", "titles", "--simplified-ui", "true",
            "--session-serialization", "false", "--scroll-buffer-size", "10000", "--scroll-mode-sync", "false",
            "--show-startup-tips", "false", "--show-release-notes", "false", "--default-cwd", "/w",
            "--default-shell", "/bin/zsh",
        ];
        assert_eq!(args, expected);
        // 空串的 cwd / shell 与缺省同义（TS 按真假判断）
        let bare = attach_args(&layout, SESSION_ID, &SessionProfile { cwd: Some(String::new()), shell: None, scroll: false });
        assert!(!has(&bare, "--default-cwd") && !has(&bare, "--default-shell"));
        assert_eq!(
            zellij_env(&layout),
            vec![
                ("ZELLIJ_SOCKET_DIR".to_string(), format!("{d}/sock")),
                ("ZELLIJ_CONFIG_DIR".to_string(), format!("{d}/config")),
                ("XDG_CACHE_HOME".to_string(), format!("{d}/cache")),
            ]
        );
    }

    #[test]
    fn extra_session_names_and_parsers() {
        assert_eq!(zellij_session_name(SESSION_ID), "mj-0f0e0d0c0b0a0908");
        assert!(is_falcon_session("mj-0f0e0d0c0b0a0908"));
        assert!(!is_falcon_session("mj-0F0E0D0C0B0A0908"));
        assert!(!is_falcon_session("mj-0f0e0d0c0b0a0908\n"));
        assert!(!is_falcon_session("mj-0f0e0d0c0b0a090"));
        assert_eq!(
            parse_live_sessions("mj-aaaa [Created 1m ago] \nmj-bbbb [Created 2m ago] (EXITED - attach to resurrect)\n\n"),
            ["mj-aaaa"]
        );
        assert_eq!(parse_version("zellij 0.45.1\n").as_deref(), Some("0.45.1"));
        assert_eq!(parse_version("ZELLIJ\t0.44.3").as_deref(), Some("0.44.3"));
        assert_eq!(parse_version("zellij v0.44.3"), None);
        assert!(version_matches(&format!("zellij {ZELLIJ_VERSION}")));
        assert_eq!(
            scroll_pipe_args(&posix_layout(), SESSION_ID, 1, Some(-0.4)).last().unwrap(),
            "seek 1 0",
            "Math.max(0, -0) 打印成 0"
        );
        assert_eq!(js_non_negative_round(2.5), "3");
        assert_eq!(js_non_negative_round(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_non_negative_round(1.5e22), "1.5e+22");
        assert_eq!(js_non_negative_round(f64::NAN), "NaN");
        assert_eq!(parse_terminal_pane_number("terminal_"), None);
        assert_eq!(parse_terminal_pane_number("terminal_１"), None, "全角数字不是 \\d");
    }
}
