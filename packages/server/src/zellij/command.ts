/**
 * Zellij 命令构造。
 *
 * 一律返回 argv 数组与 env 映射，由执行层（本地 pty.spawn / SSH exec）
 * 负责转义与注入——远端 Unix 与远端 Windows 的转义规则不同，不能在这里拼字符串。
 */

import { ZELLIJ_VERSION } from "./version.js";

/**
 * 远端 scrollback 深度。dump-screen 没有 `-S -2000` 这类行数参数，
 * 只能全量或仅当前屏；把 buffer 本身设小，`--full` 就天然限行了。
 *
 * 注意这是断线重连后用户能上翻行数的**硬上限**：replay 会 term.reset()
 * 整体替换前端缓冲，前端积累的历史保不住，能翻多少全看 dump 有多少。
 * 取 zellij 默认值 10000，并与前端 xterm scrollback、服务端 RingBuffer
 * 容量保持匹配（三者取最小生效）。
 */
const SCROLL_BUFFER = 10000;

/**
 * sessionId（UUID）→ Zellij session 名。
 *
 * 截断到 16 个 hex 是为了绕开 socket 路径长度限制：socket 路径为
 * `$ZELLIJ_SOCKET_DIR/contract_version_1/<name>`，而上限在 macOS 只有 104 字符
 * （Linux 108 / Windows 256）。完整 `falcon-<uuid>` 是 43 字符，叠加 home 路径后
 * 在用户名稍长的 macOS 上就会触顶。64 bit 随机在单个 socket 目录内足够。
 *
 * 单向派生：总是从 sessionId 算出会话名，不做反解。
 *
 * 前缀保持 `mj-`：产品曾名 Mojito，改前缀会让已有持久会话接不回去。
 */
export function zellijSessionName(sessionId: string): string {
  return `mj-${sessionId.replace(/-/g, "").slice(0, 16)}`;
}

/** 判断一个 Zellij session 名是否由 falcon 创建（孤儿会话检测用） */
export function isFalconSession(name: string): boolean {
  return /^mj-[0-9a-f]{16}$/.test(name);
}

export interface ZellijPaths {
  /** 二进制完整路径 */
  bin: string;
  /** ~/.falcon/zellij/config/config.kdl —— 见 CONFIG_BODY */
  configFile: string;
  /** ~/.falcon/zellij/sock */
  socketDir: string;
  /** ~/.falcon/zellij/config —— 保持为空目录即可保证零配置文件运行 */
  configDir: string;
  /** ~/.falcon/zellij/data */
  dataDir: string;
  /** ~/.falcon/zellij/cache —— 仅 Linux 生效（XDG_CACHE_HOME） */
  cacheDir: string;
  /** ~/.falcon/zellij/layouts/falcon.kdl —— 见 LAYOUT_BODY */
  layoutFile: string;
  /** ~/.falcon/zellij/config/scroll.kdl —— 带滚动位置插件的会话用的配置，见 scrollConfigBody */
  scrollConfigFile: string;
  /** ~/.falcon/zellij/plugins/falcon-scroll.wasm —— 滚动位置插件（ADR 0019） */
  scrollPluginFile: string;
}

/**
 * 我们自己的 layout。
 *
 * `layout` 单独一行 = 无 tab bar、无 status bar、无 plugin pane，
 * 终端看起来就是个普通终端。这与内置的 no-plugins.kdl 相同，但**不能直接引用
 * 内置名字**：`options --default-layout` 走 PathBuf 解析，只在 layout_dir 里找
 * 文件，够不到编译进二进制的 assets（实测报 `IoError: The layout was not found`），
 * 而 `--layout-dir` 又不是顶层 flag。所以安装时写一份到宿主机，用绝对路径引用。
 *
 * `keybinds clear-defaults=true` 是实测踩出来的关键一条：`--default-mode locked`
 * **只在新建会话时生效**，接回已存在的会话时模式退回 normal，用户的按键会被
 * Zellij 当成快捷键吃掉——表现为断线重连后终端"卡死"，而新建会话一切正常。
 * 清空全部键位后就不再依赖模式状态，所有按键无条件透传给 shell。
 */
export const LAYOUT_BODY = `layout
`;

/**
 * 我们独占的 config.kdl，写进 ZELLIJ_CONFIG_DIR。
 *
 * 原本想让配置目录保持为空以实现"零配置文件"，但实测证明**必须有这个文件**：
 * `options --default-mode locked` 这类 CLI 参数只在**新建**会话时生效，attach
 * 到已存在的会话时会被会话自身的模式状态覆盖，而最后一个客户端断开后模式会
 * 回到 normal。症状是断线重连后终端像卡死一样不响应键盘（按键全被 Zellij 当
 * 快捷键吃掉），而新建会话完全正常——极难排查。
 *
 * 配置文件里的 default_mode 每次客户端连接都会读，不受会话状态影响。
 * keybinds clear-defaults 是纵深防御：万一模式没设上，至少所有键位都是空的，
 * 用户按 Ctrl+p / Ctrl+n 不会莫名掉进 pane 模式。
 *
 * scroll_mode_sync false 是 0.45 起必须的（#5299 引入、#5532 才给开关）：默认
 * 开着时滚轮上翻会把会话隐式切进 Scroll 模式，而我们清空了全部键位，Scroll 模式
 * 下没绑定的键**直接被吞掉**——实测（0.45.1）滚一下再敲 `echo x⏎`，shell 什么都
 * 没收到，视口也不回底，看上去就是终端卡死。关掉后与 0.44 一致：一敲键就回到底部、
 * 按键照常透传。0.44.3 读到这一行不报错（实测），新旧二进制共用同一份文件无碍。
 *
 * 目录是 falcon 独占的，不会读到用户自己的 Zellij 配置。
 */
export const CONFIG_BODY = configBody("pane_frames false");

/**
 * 带滚动位置插件的会话用的配置（ADR 0019），经 `--config` 指给 zellij，与 config.kdl
 * 只差两处：
 *
 * - 边框样式是 titles 而不是 `pane_frames false`：插件要的 ActivePaneScroll 事件只在
 *   titles 样式（且 tab 里只有一个 pane）时才发，`pane_frames false` 会把样式压成 None。
 *   单 pane 时 titles 样式不画标题行、滚动时也不画 `SCROLL:` 指示（实测 0.45.1），
 *   外观与关掉边框一致。
 * - `load_plugins` 让插件随会话以后台插件启动（没有 pane）。不能靠 `zellij pipe
 *   --plugin` 现拉，理由见插件源码顶部。
 *
 * **为什么是另一个文件而不是改 config.kdl**：升级前建的会话跑在 0.44.3 的 server 上，
 * 接回时客户端会把配置带过去，0.44.3 不认识 titles 样式、又没了 `pane_frames false`，
 * 实测会给老会话画上整圈边框。所以老会话照旧读 config.kdl，只有建会话时插件已就位的
 * 新会话才指向这份（sessions.scroll_plugin 记着），zellij 的配置热重载也各管各的文件。
 */
export function scrollConfigBody(pluginFile: string): string {
  return configBody(
    `pane_frame_style "titles"`,
    `load_plugins {\n    ${kdlString(`file:${pluginFile}`)}\n}\n`
  );
}

function configBody(frames: string, extra = ""): string {
  return `default_mode "locked"
${frames}
simplified_ui true
session_serialization false
scroll_buffer_size ${SCROLL_BUFFER}
scroll_mode_sync false
mouse_mode true
show_startup_tips false
show_release_notes false
keybinds clear-defaults=true {
}
${extra}`;
}

/** KDL 字符串字面量：反斜杠与双引号转义（只给 POSIX 路径用，Windows 不走插件） */
function kdlString(s: string): string {
  return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

/**
 * 运行时环境变量。
 *
 * 四处写入目标全部指进 ~/.falcon/zellij/，让"删掉 ~/.falcon 即完全卸载"尽量成立。
 * 注意 cache 只有 Linux 能靠 XDG_CACHE_HOME 收拢——macOS/Windows 的 cache 路径
 * 由 directories crate 硬编码，无法重定向，会有残留（已在 README 注明）。
 */
export function zellijEnv(paths: ZellijPaths): Record<string, string> {
  return {
    ZELLIJ_SOCKET_DIR: paths.socketDir,
    ZELLIJ_CONFIG_DIR: paths.configDir,
    XDG_CACHE_HOME: paths.cacheDir,
  };
}

function globalArgs(paths: ZellijPaths): string[] {
  return ["--data-dir", paths.dataDir];
}

/** 会话用哪套配置。scroll = 带滚动位置插件（见 scrollConfigBody），否则是 config.kdl */
export interface SessionProfile {
  cwd?: string;
  shell?: string;
  scroll?: boolean;
}

/** attach 用的全局参数：带插件的会话另指配置文件 */
function attachGlobalArgs(paths: ZellijPaths, opts: SessionProfile): string[] {
  return opts.scroll
    ? [...globalArgs(paths), "--config", paths.scrollConfigFile]
    : globalArgs(paths);
}

/**
 * 会话级 options。作为 `attach` 的子命令附加，CLI 值优先于配置文件。
 *
 * - default-mode locked：**最关键的一条**。不设它 Zellij 会抢走 Ctrl+p / Ctrl+n /
 *   Ctrl+o / Ctrl+t 做模式切换，用户在 shell 里按 Ctrl+p 调历史会掉进 pane 模式。
 * - session-serialization false：关掉复活序列化。开着的话被杀的 session 会以
 *   `(EXITED - attach to resurrect)` 留在 `zellij ls` 里，污染存活判断。
 */
function sessionOptions(paths: ZellijPaths, opts: SessionProfile): string[] {
  const args = [
    "options",
    "--default-layout",
    // 必须是绝对路径，理由见 LAYOUT_BODY
    paths.layoutFile,
    "--default-mode",
    "locked",
    // 边框样式两套配置各不相同，理由见 scrollConfigBody。带插件的会话**不能**再传
    // --pane-frames false：它会把 titles 样式压成 None，插件就收不到滚动事件
    ...(opts.scroll ? ["--pane-frame-style", "titles"] : ["--pane-frames", "false"]),
    "--simplified-ui",
    "true",
    "--session-serialization",
    "false",
    "--scroll-buffer-size",
    String(SCROLL_BUFFER),
    // 理由见 CONFIG_BODY；两处都写，与其它项同一口径（CLI 管新建，config 管接回）
    "--scroll-mode-sync",
    "false",
    // 鼠标上报**只能**由 config.kdl 的 mouse_mode true 开启，这里绝不能传
    // --mouse-mode。实测（0.44.3）：CLI 传 --mouse-mode true 时客户端反而
    // 永远不发 ?1000h（与传 false 同效，疑似上游 bug）；不传时每次 attach
    // 都按 config 开启。开鼠标的动机：Zellij 常驻备用屏（无 scrollback），
    // xterm.js 对"备用屏 + 无鼠标上报"的滚轮会降级成 ↑/↓ 方向键——在 shell
    // 提示符下滚轮变成翻命令历史。开启后滚轮交给 Zellij 滚它自己的 scroll
    // buffer（上限 SCROLL_BUFFER 行）。代价：前端本地选区需按住 Shift 拖拽。
    // 这两条是实测踩出来的：Zellij 默认会先显示一屏 "Zellij Tip #N" 启动提示，
    // 挡在 shell 前面等用户按键关闭。表现为会话建成了、hasSession 为真、
    // 但屏幕空白且输入毫无反应——不关掉整个终端就是废的。
    "--show-startup-tips",
    "false",
    "--show-release-notes",
    "false",
  ];
  if (opts.cwd) args.push("--default-cwd", opts.cwd);
  // --default-shell 是 PathBuf，不接受参数（`bash -l` 会失败）。
  // UI 上已说明"需要参数请指向自己的脚本"。
  if (opts.shell) args.push("--default-shell", opts.shell);
  return args;
}

/**
 * 建立或接回会话。
 *
 * 始终带 `--create`：带它走精确匹配，不带则是**前缀匹配**——
 * 前缀歧义会命中错误的 session 或直接 exit(1)，程序化调用绝不能依赖。
 */
export function attachArgs(
  paths: ZellijPaths,
  sessionId: string,
  opts: SessionProfile
): string[] {
  return [
    ...attachGlobalArgs(paths, opts),
    "attach",
    zellijSessionName(sessionId),
    "--create",
    ...sessionOptions(paths, opts),
  ];
}

/**
 * 后台建一个 detached 会话（无需 PTY）。Windows 远端专用：会话级 options 只在
 * 创建时生效，所以这里必须带全 sessionOptions——后续 PTY attach 时的同名参数
 * 对已存在的会话是无效的，真正决定 layout / shell / cwd 的是这一条。
 *
 * 不幂等：会话已存在时退出码为 1（实测），调用方要先用 hasSession 挡一道。
 */
export function createBackgroundArgs(
  paths: ZellijPaths,
  sessionId: string,
  opts: SessionProfile
): string[] {
  return [
    ...attachGlobalArgs(paths, opts),
    "attach",
    zellijSessionName(sessionId),
    "--create-background",
    ...sessionOptions(paths, opts),
  ];
}

/**
 * 列出会话。用 `--no-formatting` 而不是 `--short`：
 * short 模式**不区分死活**，只打名字；要判断存活必须看
 * `(EXITED - attach to resurrect)` 后缀。
 *
 * 退出码：有 session → 0；一个都没有 → 1（stderr 输出 "No active zellij sessions found."）。
 * 所以非零退出码不代表出错。
 */
export function listArgs(paths: ZellijPaths): string[] {
  return [...globalArgs(paths), "list-sessions", "--no-formatting"];
}

/** 解析 `zellij ls --no-formatting` 的输出，返回存活（非 EXITED）的会话名 */
export function parseLiveSessions(stdout: string): string[] {
  const live: string[] = [];
  for (const line of stdout.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    if (t.includes("EXITED")) continue;
    const name = t.split(/\s+/)[0];
    if (name) live.push(name);
  }
  return live;
}

/**
 * 列出会话里的 pane。用于找出终端 pane 的 id——见 dumpScreenArgs。
 */
export function listPanesArgs(paths: ZellijPaths, sessionId: string): string[] {
  return [
    ...globalArgs(paths),
    "--session",
    zellijSessionName(sessionId),
    "action",
    "list-panes",
  ];
}

/**
 * 从 `list-panes` 输出里取第一个终端 pane 的 id。
 *
 * 输出形如：
 *   PANE_ID  TYPE  TITLE
 *   plugin_0  plugin  (.) - zellij:link
 *   terminal_0  terminal  Pane #1
 */
export function parseTerminalPaneId(stdout: string): string | null {
  for (const line of stdout.split("\n")) {
    const [id, type] = line.trim().split(/\s+/);
    if (id && type === "terminal") return id;
  }
  return null;
}

/**
 * 列出已连接客户端与其聚焦 pane 的前台命令。用于关 tab 前判断"有没有程序在跑"。
 */
export function listClientsArgs(paths: ZellijPaths, sessionId: string): string[] {
  return [
    ...globalArgs(paths),
    "--session",
    zellijSessionName(sessionId),
    "action",
    "list-clients",
  ];
}

/**
 * 从 `list-clients` 输出里取前台命令。
 *
 * 输出形如：
 *   CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND
 *   1         terminal_0     sleep 300
 *
 * 实测（macOS，0.44.3）：空闲 shell 时 RUNNING_COMMAND 是字面量 "N/A"，
 * 有前台程序时是完整命令行；没有客户端连接时只有表头。命令行可能含空格，
 * 所以第三列起要整段拼回。聚焦在 plugin pane 上（理论上 falcon 的单 pane
 * layout 不会发生）没有可言的前台命令，同样按"不知道"处理。
 */
export function parseClientRunningCommand(stdout: string): string | null {
  for (const line of stdout.split("\n")) {
    const t = line.trim();
    if (!t || t.startsWith("CLIENT_ID")) continue;
    const cols = t.split(/\s+/);
    const paneId = cols[1];
    if (!paneId?.startsWith("terminal")) continue;
    const command = cols.slice(2).join(" ");
    if (!command || command === "N/A") return null;
    return command;
  }
  return null;
}

/**
 * 抓取历史用于重建 Scrollback。
 * `--ansi` 保留颜色（等价 tmux capture-pane -e），`--full` 含 scrollback。
 *
 * **必须显式给 `--pane-id`**：不给的话抓的是"当前聚焦 pane"，而在没有客户端
 * 连接时（后端重启后接回，正是我们最需要它的时刻）焦点会落在 Zellij 后台的
 * plugin pane 上（`zellij:link` / `About Zellij` 即使 layout 里没写也会加载），
 * dump 出来是空的。实测：不给 pane-id 时 detach 状态下只能抓到一个 "\r\n"。
 *
 * 已知缺陷 #5311：alternate-screen 程序（vim/htop）被 resize 后，
 * 输出会带重复的陈旧行，重建会有视觉瑕疵。暂无解。
 */
export function dumpScreenArgs(
  paths: ZellijPaths,
  sessionId: string,
  paneId: string
): string[] {
  return [
    ...globalArgs(paths),
    "--session",
    zellijSessionName(sessionId),
    "action",
    "dump-screen",
    "--ansi",
    "--full",
    "--pane-id",
    paneId,
  ];
}

// ---------------- 滚动位置插件（ADR 0019） ----------------

/** 插件监听的管道名；插件源码在 packages/server/zellij-plugin */
const SCROLL_PIPE = "falcon-scroll";

/** 滚动位置：两个数与 zellij 边框上的 `SCROLL: position/length` 同口径 */
export interface ScrollPosition {
  /** 视口下方的显示行数，0 = 在底部 */
  position: number;
  /** 视口上方的显示行数 + position；0 = 没有可滚的历史（含备用屏里的全屏程序） */
  length: number;
}

/**
 * 问插件要滚动位置（`get`），或让它滚到「视口下方还剩 position 行」处（`seek`，
 * 回的是滚完后的位置）。pane 是 terminal pane 的数字 id，见 parseTerminalPaneNumber。
 *
 * **按名字广播，不带 `--plugin`**：带上的话，会话里没有这个插件时（升级前建的老会话、
 * 插件没部署上）zellij 会现场拉起一个浮动 pane 实例，而且那条管道会挂着不退出（实测）。
 * 广播时没人认领就直接放行，回话为空。
 *
 * 执行方必须给 stdin 一个 EOF：stdin 不是终端时 CLI 收到放行后还要把 stdin 读到头
 * 才退出（zellij-client 的 pipe_client），SSH exec 与默认的 spawn 都不会自己关。
 */
export function scrollPipeArgs(
  paths: ZellijPaths,
  sessionId: string,
  pane: number,
  seek?: number
): string[] {
  const payload = seek == null ? `get ${pane}` : `seek ${pane} ${Math.max(0, Math.round(seek))}`;
  return [
    ...globalArgs(paths),
    "--session",
    zellijSessionName(sessionId),
    "pipe",
    "--name",
    SCROLL_PIPE,
    "--",
    payload,
  ];
}

/** 插件的回话一行 `<position> <length>`；空（会话里没有插件）或认不出就是 null */
export function parseScrollReply(stdout: string): ScrollPosition | null {
  const m = stdout.trim().match(/^(\d+) (\d+)$/);
  if (!m) return null;
  const position = Number(m[1]);
  const length = Number(m[2]);
  return position <= length ? { position, length } : null;
}

/** `terminal_3` → 3。scrollPipeArgs 要的是数字 id */
export function parseTerminalPaneNumber(paneId: string): number | null {
  const m = paneId.match(/^terminal_(\d+)$/);
  return m ? Number(m[1]) : null;
}

/** 插件要的权限，与插件 load() 里 request_permission 的清单一致 */
const SCROLL_PERMISSIONS = [
  "ReadApplicationState",
  "ChangeApplicationState",
  "ReadCliPipes",
  "ReadPaneContents",
];

/**
 * zellij 的插件授权缓存 permissions.kdl 在宿主机上的位置。
 *
 * 位置由 directories crate 的 ProjectDirs 定：Linux 跟 XDG_CACHE_HOME 走（zellijEnv 把它
 * 指进了 falcon 自己的 cache 目录），macOS 是写死的 ~/Library/Caches/… 改不了——会和
 * 用户自己的 zellij 共用这个文件，所以只追加、不覆盖。
 */
export function scrollPermissionsFile(
  paths: ZellijPaths,
  os: "linux" | "darwin",
  home: string
): string {
  return os === "darwin"
    ? `${home.replace(/\/+$/, "")}/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl`
    : `${paths.cacheDir}/zellij/permissions.kdl`;
}

/**
 * 预授权条目。后台插件请求权限时 zellij 先查这个缓存，命中就直接批；没命中会弹授权
 * 界面——后台插件没有 pane，弹了也没人能点，插件就永远拿不到权限。
 *
 * 键是插件路径**本身**，不带 `file:` 前缀（RunPluginLocation::File 的 Display 就是
 * 路径；带前缀的话匹配不上，实测踩过）。
 */
export function scrollPermissionsEntry(pluginFile: string): string {
  const body = SCROLL_PERMISSIONS.map((p) => `    ${p}\n`).join("");
  return `${kdlString(pluginFile)} {\n${body}}\n`;
}

/**
 * 彻底销毁会话（对应 Terminate 语义）。
 *
 * 用 delete-session 而非 kill-session：后者保留复活数据，会在 `ls` 里留下
 * EXITED 条目。`--force` 表示还活着就先杀再删。
 */
export function deleteSessionArgs(paths: ZellijPaths, sessionId: string): string[] {
  return [
    ...globalArgs(paths),
    "delete-session",
    zellijSessionName(sessionId),
    "--force",
  ];
}

/** 安装后的握手验证：跑得起来才算装好（能挡住 noexec 和架构不符） */
export function versionArgs(): string[] {
  return ["--version"];
}

/** `zellij --version` 输出形如 `zellij 0.44.3` */
export function parseVersion(stdout: string): string | null {
  const m = stdout.match(/zellij\s+(\d+\.\d+\.\d+)/i);
  return m ? m[1] : null;
}

export function versionMatches(stdout: string): boolean {
  return parseVersion(stdout) === ZELLIJ_VERSION;
}
