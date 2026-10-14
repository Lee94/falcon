# 自研会话守护进程：holder 持 PTY，终端状态交给 libghostty-vt

状态：**已实施**（2026-10-13）。**一次替换 Zellij，不兼容旧会话、Windows 不阻塞切换**（决定七）。Windows 部分只做过交叉编译与零警告检查，没在真机上验证。详细设计（协议、布局、接入、验证结果）见 [docs/design/session-daemon.md](../design/session-daemon.md)；实施中与本 ADR 初稿不同的地方见文末后记。

持久会话不再由宿主机上的 Zellij 包装，改由 falcon 自己的会话守护进程 `falcon-sessiond` 承担。这个名字是为了避开 CONTEXT.md 里单独说「Host」会撞名的问题。

每个会话在宿主机上有两个进程：
- **holder**：持有 PTY，把输出原样转给接入方；
- **VT 进程**：吃一份输出副本，在 libghostty-vt 里维护终端状态。

接回时，从 VT 进程导出 VT 字节重建画面。思路来自 zmx（每会话一个守护进程，用 libghostty-vt 恢复状态），但用 Rust 重写，覆盖 Windows，并把「持 PTY」和「跑 VT 状态机」拆开。

本 ADR 推翻 ADR 0001 里「自研 session host 否决」那一条，并让 ADR 0019（Zellij 滚动插件）整份退役。ADR 0001 的其余结论照旧：持久会话的语义、降级时要标注具体原因、只管 falcon 自己建的会话。

## 背景：ADR 0001 的三条否决理由，现在各自的情况

1. **用户失去 `zellij attach` 自救的入口**：守护进程自带 `attach` 子命令（zmx 就是这么用的），falcon 不可信时，用户照样能 ssh 上去接回自己的会话。已消除。
2. **要在服务端跑 VT 状态机，等于重写 tmux 核心**：libghostty-vt 现在有 C API，二进制快照（`snapshot.h`）和 VT / HTML / 纯文本导出（`formatter.h`）都是现成的。zmx 证明了剩下的 Unix 部分只有 4–5k 行（v0.8.1 去掉测试、补全脚本、搬来的 posix 封装之后）。基本消除。
3. **Windows 的平台细节**（ConPTY resize、`CREATE_BREAKAWAY_FROM_JOB`、socket 权限）：仍然存在，是真正的工作量。但 Zellij 在 Windows 上同样踩这些坑，而且只能等上游修：Job Object 连坐（PR #5195 未合）、#5052（pwsh 下 cwd 失效）、没有 ARM64 产物。自研之后，这些坑由我们自己修。

还有几条理由，是写 ADR 0001 时还不存在的：

- **Zellij 吞掉它不认识的 OSC。** `grid.rs` 的 `osc_dispatch` 只处理 4/7/8/9/12/50/52/99/104/110–112/133/777，其余记一条 `Unhandled osc` 就丢掉。falcon 收到的又是 Zellij 重画后的画面，程序发出的原始序列根本到不了 falcon。OSC 7501（Program Status Protocol，见决定六）就卡在这里，而本机的 Claude Code 2.1.296 启动时已经在探测它：二进制里有 `probe: no reply to OSC 7501 ; ?` 和 `probe: OSC 7501 reply` 两条字符串。
- **ADR 0019 那一整套。** 滚动发生在宿主机的 Zellij 里，滚动条位置要靠 wasm 插件加 `zellij pipe` 问出来，还因此分出新旧两套配置、要记 `sessions.scroll_plugin`，0.45 起还得设 `scroll_mode_sync false`。字节原样透传之后，客户端的 alacritty 本地就有 scrollback，这些都可以退役。
- **`dump-screen` 重建历史有缺陷**：必须先 `list-panes` 找到 terminal pane 再带 `--pane-id`；有 #5311（全屏程序 resize 后带出陈旧行）；行数只能靠 `--scroll-buffer-size` 把 buffer 本身设小来限制。

## 决定一：每会话一对进程，holder 持 PTY、VT 进程跑 libghostty-vt

- **holder**：纯 Rust，不调用 libghostty-vt。它只做这些事：PTY 读写、socket / 命名管道收发、字节转发、保存程序状态记录、探测前台命令、拉起并看护 VT 进程。代码量目标 2k 行以内，协议只增不改。它是会话生命周期的锚：holder 活着，会话就活着。
- **VT 进程**：同一个二进制的 `vt` 子命令，由 holder 拉起，经管道通信。持有 libghostty-vt 终端，负责回答终端查询、导出快照。
- **为什么要拆**：Zig 的 panic 是 trap，跨 FFI 接不住。如果持 PTY 的进程跟着中止，shell 收到 SIGHUP，会话就死了，这正是 falcon 的核心承诺。拆开之后：
  - VT 进程挂了，holder 立即重启它，并用自留的 256 KiB 原始输出尾巴重建画面，最坏情况只丢更早的历史；
  - 60 秒内连续崩 3 次就降级为纯转发。
  - 拆开还带来一个附带好处：VT 进程可以热升级（用二进制快照交接），升级 ghostty 不必等存量会话自然消亡。服务端接回会话时发现它的 VT 进程不是这次部署的 ghostty，就让 holder 换掉。
- **Unix**：`fork` + `setsid` 脱离，PTY 用 portable-pty。
- **Windows**：ConPTY，`CREATE_BREAKAWAY_FROM_JOB` 脱离调用方。Job 不允许脱离时走 WMI `Win32_Process.Create`（远端 Zellij 现在就是这么脱离 sshd 的），再不行就标为非持久。
- **子命令**：`spawn`、`bridge`（给 falcon-server 用）、`attach`（给人用的逃生通道）、`list`、`kill`、`history`、`version`。
- 守护进程不做窗口、tab、分屏、快捷键。这些在 Zellij 里都是要关掉的东西：ADR 0001 的 locked 模式、`keybinds clear-defaults`、no-plugins layout、`--session-serialization false` 整类消失。

## 决定二：终端状态用 libghostty-vt，不用 alacritty_terminal

两者都评估过（客户端 falcon-term 用的就是 vendor 的 alacritty_terminal）。宿主机这一侧选 libghostty-vt：

| | libghostty-vt | alacritty_terminal |
|---|---|---|
| 接回时导出状态 | 快照 + VT 导出现成 | 没有，要自写序列化器 |
| OSC 7 / 133 / 7501 | 都认，7501 有按规范校验的解析和查询回复 | vte 0.15 只认 0/2、4、8、10–12、50、52、104、110–112 |
| 每个 cell | 8 字节（`packed struct(u64)`），样式在页内去重 | ≤ 24 字节（`cell.rs` 断言） |
| 测试 | AFL++ 模糊测试（osc / parser / stream 三个目标）+ 约 9.7 万行测试 | Alacritty / Zed 长期在用 |

代价与对策：

- **构建要 Zig 0.16，C API 不稳定**（头文件原话 "definitely going to change"）。钉住 ghostty 的 commit，升级当成有计划的动作来做（同 gpui-kit）。守护进程是唯一的使用者，影响面只到这一个二进制。
- **出 bug 时进程直接中止。** 由决定一的两进程模型兜住。静态库用 `ReleaseSafe` 编：`ReleaseFast` 连越界检查都没有，而第三方 libghostty-rs 在 release 下默认就是它。
- **宿主机与客户端引擎不同。** 实时流是原始字节，由客户端自己解释，不受影响。只有接回那一刻的重放要走「ghostty 导出 → alacritty 解释」，差异见决定三。

**绑定自己维护，不用第三方的 libghostty-rs。** 它钉在 ghostty `3425025`（2026-10-04），早于 OSC 7501 进 main；它的 build.rs 还会在构建时 `git clone` ghostty。我们的做法：

- ghostty 钉到 `05dcf88`（2026-10-12，写这份 ADR 时的 main）。升级的流程是：改 pin、重编静态库、重新生成绑定，再跑一遍接回比对语料和 7501 用例。
- 静态库由 xtask 用 Zig 编：`zig build -Demit-lib-vt -Doptimize=ReleaseSafe`。不经任何 build.rs 联网。
- 绑定用 bindgen 从同一份头文件生成（allowlist `ghostty_.*` / `Ghostty.*` / `GHOSTTY_.*`，枚举生成常量，保留布局断言），**生成结果入库**，日常构建不需要 libclang。只在升 pin 时重新生成，diff 就是这次升级的 ABI 变化。安全封装只覆盖用到的部分。
- 链接时 `.a` 要单独放一个目录：同目录里有 `.dylib` 时，macOS 的链接器会挑 dylib，运行时报 `Library not loaded: @rpath/libghostty-vt.dylib`。

## 决定三：接回 = 导出 VT 字节，回放给新 Viewer 也用它

原型实测（2026-10-13，macOS arm64，release，用的是 libghostty-rs 钉的 `3425025`；换到 `05dcf88` 后已作为语料测试复跑，见设计文档 15）。同一段输出分两路喂：一路直接喂给 alacritty，另一路喂给 libghostty-vt、导出 VT、再喂给一个新的 alacritty。两边逐格比对。

- **一致**：
  - 文字、宽字符、emoji、ZWJ 组合 emoji、国旗、组合字符；
  - 光标位置；
  - bracketed paste、SGR 鼠标、拖拽上报、备用屏；
  - 软折行标记。**前提是必须开 `unwrap`**，否则格式化器会在折行处写 `\r\n`，客户端的 WRAPLINE 全丢，复制和 resize 重排都会断行。
- **在备用屏里接回**：格式化器只导当前活动屏。做法是：
  1. 编码一份快照，解到一个副本里；
  2. 在副本上写 `CSI ?1049l` 退出备用屏，导出主屏（含历史）；
  3. 拼上 `CSI ?1049h`，再接当前备用屏的导出。

  实测退出备用屏后，主屏 306 行历史也对得上。
- **已知差异**（照单接受）：
  - 0–15 号色被写成 `38;5;N`，alacritty 存成 `Indexed`，「粗体用亮色」会失效。导出后要做一次改写，这是一个纯函数。
  - ~~正文里的 OSC 8 链接不导出：格式化器的 hyperlink 选项只恢复光标当前的链接状态（`formatter.zig`）。~~ 已由补丁解决，见后记（2026-10-14）：给 ghostty 打补丁。
  - 钉的 commit 导出 OSC 7 时会多一个 NUL。我们不导出 pwd，所以无害，但要报给上游。
  - `CSI 2J` 清掉的那一屏，ghostty 不推进 scrollback。
  - 从没写过的空格会带着前一段的前景色，不可见。
- **耗时**：1 万行历史时，快照 306 KB、约 0.1 ms；VT 导出 121 KB、约 0.5 ms。

落到 falcon-server 上：
- `captured_history`、`normalize_captured` 和 `dump-screen` 那条路径一起退役。
- 守护进程会话的 Viewer 回放不再用服务端的 4 MB RingBuffer 加 `term_modes` 前缀，改为向 holder 要快照（`Sync`）。holder 在这条连接上把快照之后的输出先扣下、快照发出后再放，所以连接上 `Synced` 之前的输出都已含在快照里、之后的都在快照之后，服务端按到达顺序处理即可，不用拼接。快照切点落在半截转义序列上时，libghostty-vt 的 continuation API 给出补齐用的原始字节（设计文档 5.5）。
- 格式化器总是省掉末尾的空行，有历史时重放会少滚几行（整屏上移、光标压在内容行上），导出时要补回来（设计文档 6.2，语料实测发现）。
- `OscColorGate` 对这类会话停用（见决定五）。
- 客户端协议不变：仍是一个 replay 帧，接若干 output 帧。

## 决定四：服务端与守护进程之间的协议；升级不杀会话

- **帧格式**：`u8 kind | u32 len | payload`。字节类载荷是原始字节，控制类载荷是 JSON。`Output` 帧带单调递增的字节偏移。协议类型放在 `falcon-proto::sessiond`。
- **本机会话**：falcon-server 直接连 socket / 命名管道，不再在 PTY 里跑一个 `zellij attach` 再解析它画出来的东西。
- **SSH 会话**：exec `falcon-sessiond bridge`，stdin / stdout 透传。每会话仍是一条 SSH channel，与现在相同。不用 `direct-streamlocal` 转发 socket，因为 Windows 的 OpenSSH 没有它，exec 桥两边都能用。
- **升级不能杀会话**（zmx 的已知问题：IPC 一变，所有会话全死）：
  - 二进制按版本命名，旧版不删；
  - bridge 只搬字节、不解析帧，所以跟版本无关；
  - 握手发生在 falcon-server 与 holder 之间，协商协议版本区间；
  - holder 协议只增不改，新版 falcon-server 总能和旧 holder 说话；
  - VT 进程可以热升级（决定一）。
- **二进制由后端推到宿主机**，不让宿主机自己下载（同 px0）：文件名带内容哈希（`falcon-sessiond-<版本>-<哈希>`），"在不在"就是"对不对"，推完用 `version --json` 握手；POSIX 经 stdin 推，Windows 走 ADR 0008 的 base64 行协议。这是第一次有二进制推到 Windows 远端（px0 直接报不支持）。
- **bridge 的 stdout 先写一个前导标记**再透传帧：SSH 上 bridge 由登录 shell 拉起，rc 文件往 stdout 打的字（motd、echo）会混进帧流，服务端读到标记之前的字节一律丢掉。连不上 holder 时 bridge 用退出码说原因：3 = 不在了、5 = 已正常结束、4 = 暂时连不上。
- **Windows 上不能经 PowerShell 的 `&` 跑 bridge**：PowerShell 会把原生命令的 stdout 当文本行收下再重新编码写出，二进制帧流全毁。改用 .NET 的 Process 起它、不重定向任何流（继承 sshd 的管道）。

## 决定五：守护进程会话的终端查询，由 VT 进程一处回答

- **为什么这么定**：
  - 没有 Viewer 时（agent 在后台启动、服务端重启后）也得有人答，否则程序会卡在等回复上；
  - 有多个 Viewer 时，今天每个客户端都会各答一遍，全部写进 PTY；
  - 客户端的回复还可能卡在队列里，直到下一段输出到来。
- **客户端怎么做**：对持久会话（`durable == true`，切换之后持久会话一定是守护进程会话），客户端一律不发 `PtyWrite`。非持久会话没有 VT 进程，照旧由客户端回答。
- **口径**：
  - DA、DSR、DECRQM、OSC 7501 `?`：用 libghostty-vt 的默认回答；
  - XTVERSION：改成 `falcon <版本>`，不冒充 ghostty；
  - OSC 10/11/12 与 `CSI ? 996 n`：按服务端下发的 `Appearance` 回答；
  - XTWINOPS：v1 不答；
  - **`CSI ? u` 的回复要过滤掉**：libghostty-vt 默认回 `?0u`，等于声称支持 kitty 键盘，而 `keys.rs` 只有传统编码。等客户端实现 CSI u 编码之后再放开。
- 深浅翻转时的 2031 通知改由 holder 发出。

## 决定六：OSC 7501 由 VT 进程解析，holder 保存

[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status) 目前是草案（0.1 发布于 2026-09-28，0.4 发布于 2026-10-10）。程序用它报告自己处于 idle / working / done / blocked / error；blocked 时还说明要的是授权、回答还是凭据。

- **在守护进程里解析**，不在客户端：最有用的恰好是没人在看的会话（后台的 agent 变成 blocked、等授权）。
- **记录存在 holder**：存储是纯 Rust 的 `ProgramStatusStore`，放在 falcon-proto。这样 VT 进程重启也不丢记录。
- **推到界面**：经 WS 推 `{type:"program_status"}`，`Session` 上带一个摘要（同级时根记录优先），侧栏显示徽标、终端窗口的标题栏显示展开的状态（状态、进度、子任务、计时，带动画；最初是窗口底部常驻的一条状态栏，见后记），blocked 时 `ToastExt` 提醒，进入 done / error 时响一声提示音（可在设置里关）。细节在设计文档第 8 节。
- **生命周期**：
  - OSC 133 `A`（新提示符）时清掉 working / blocked。
  - 再用前台命令兜底：agent 会话里 CLI 退出后是 `exec` 回登录 shell（ADR 0013），只看 133 不够。
  - done / error 在会话窗口获得焦点后清掉。
- **显示到终端之外要消毒、要署名**：`title` / `message` 是程序给的不可信文本。头文件要求去掉方向覆盖这类不可见格式字符，并标明来自哪个会话。
- **Windows 上收不到**：ConPTY 会吞掉它不认识的 OSC（wmx 的 README 原话 "ConPTY drops unknown OSCs"）。

实测（自维护的绑定 + `05dcf88` 静态库）：
- 按 Claude 的顺序喂 `OSC 7501 ; ?`、`CSI c`、`CSI > q`，依次得到 7501 的支持声明、DA、XTVERSION，顺序正确。
- working 带进度、blocked 带 kind 与 base64 文本、分层 id、`clear` 都正确；`state=bogus` 整条丢弃；OSC 133 `A` 会触发 `PROMPT_START`。
- 有一处与规范 0.4 的文字不符：msg 的 base64 坏掉时，ghostty 保留这条报告、只把 msg 置空。规范还在草案期，以 ghostty 的实现为准。

## 决定七：一次替换 Zellij，不兼容旧会话

用户明确不需要旧会话的兼容（2026-10-13）。所以不做「新旧并存 + 回退开关」，而是在一个版本里整体换掉：

- **不区分会话类型**：持久会话就是守护进程会话，非持久会话就是普通 PTY（本机 portable-pty / SSH 的 pty channel，与今天相同）。`sessions` 表不加 `backend` 列，客户端与服务端都按已有的 `durable` 分支。
- **部署失败不再退回 Zellij**：守护进程推不上、跑不起来时，按 ADR 0001 标为非持久并写明原因（`dir-not-writable`、`verify-failed` 等）。没有 `FALCON_SESSION_BACKEND` 这类开关。
- **升级时的旧会话**：数据库里的旧行在接回时找不到 holder，自然落到 `dead(SessionGone)`，由用户在界面上关掉，不写专门的迁移。宿主机上的 Zellij 进程、二进制和配置都不清理：会话里的程序还活着，用户想救可以自己 `zellij attach`。发版说明提前写明「升级会结束全部会话」。
- **主机授权沿用**：`zellij_hosts` 换成 `session_hosts`，只把 `authorized`（允许 falcon 往这台机器上放东西）抄过去一次，Zellij 的版本、下载镜像、Windows 断线验证结果都不要。授权不是会话，用户已经同意过的主机不该再问一遍。
- **Zellij 相关代码在切换的版本里全部删除**：`zellij/`、`sessions/ssh_zellij.rs`、`sessions/scroll_plugin.rs`、`native/zellij-plugin` 与 `assets/falcon-scroll.wasm`、WS 的 `scroll` 消息、`captured_history` / `dump-screen`、xtask 的 `zellij-update` / `zellij-plugin`、安装对话框里的下载镜像。被复用的部分（四种执行环境的拼装、远端探测、WMI 拉起）先挪进 `sessiond/` 再删。清单见设计文档第 14 节。
- **Windows 不阻塞切换**（用户要求，2026-10-13）：没有 Zellij 可以退了，Windows 上验证不过的子路径标非持久——守护进程拉不起来时会话退成非持久并写明原因，远端还有一次真实断线验证，不会静默失效。上线前按设计文档 15 的清单在真机上走一遍。
- **孤儿清理**：今天不存在。补上之后只清 `owner` 是本实例、不在 DB 里、存在超过 1 小时的会话。同一台远端可能被多个 falcon 共用。
- **非持久会话的老路径保留**：服务端的 RingBuffer、`term_modes` 回放前缀、`OscColorGate`，客户端对终端查询的回答，都只剩非持久会话在用，不删。

## 构建与分发

- `native/sessiond/` 是一个独立的 Cargo workspace，不进 `native/` 工作区。所以 `cargo check --workspace` 和日常开发都不需要 Zig。
- xtask 新增三个任务：
  - `ghostty`：管钉死版本的 Zig 和 ghostty 源码，编静态库；
  - `ghostty-bindgen`：重新生成绑定；
  - `sessiond`：用 cargo-zigbuild 交叉编译 5 个目标。
- 5 个目标：x86_64 / aarch64 的 linux-musl、x86_64 / aarch64 的 macOS、x86_64 的 Windows（gnu）。服务端发布版用 `embed-sessiond` 把它们压缩后内嵌，因为 Mac 上的后端也会连 Linux arm64 的远端。
- **已验证**：Zig 0.16.0 编 `05dcf88` 的 libghostty-vt，5 个目标全部通过；Rust 一侧经 cargo-zigbuild 链接也全部通过，**不需要 libc++**（ghostty 的 C++ 依赖以 no-libcxx 模式编进了静态库；原型时以为要 `-lc++` 是误判）。产物 2.5–3.7 MB，Linux 是静态链接（alpine 容器里两种架构跑通），macOS 只依赖 libSystem。

## 客户端

- 不换引擎，继续用 alacritty_terminal。换成 libghostty-vt 等于重写渲染（照 Zed 抄的 `terminal/element.rs`）、按键编码（`keys.rs`）和浏览器版的 wasm 编译链，这些与会话持久化无关。
- 要做的事：
  - 持久会话不发终端回复（决定五）；
  - 滚动条一律读本地 Grid，ADR 0019 的探测、定时器与 WS `scroll` 删除；
  - 滚轮补上 ?1007 的判断；
  - replay 之后保持阅读位置；
  - 显示程序状态。
- 另外修一个与守护进程无关的问题：`take_events` 只在脏位 0→1 时运行，终端回复可能卡在队列里（非持久会话仍由客户端回答，这个修复照样要做）。
- kitty 键盘（CSI u 编码）不在本期。

## Considered Options

- **继续用 Zellij**：现状。「背景」一节的问题都留着，OSC 7501 做不了。
- **与 Zellij 并存、按会话记 `backend`、留回退开关**：设计初稿就是这样。没选：用户不需要旧会话的兼容；并存期间回放、查询代答、滚动条、前台命令、退出判定都要两套路径一起维护，回退开关又只对新会话有效，换来的保险不值这个复杂度。
- **直接用 zmx**：没有 Windows；升级 IPC 会杀掉所有会话（官网的已知问题至今还挂着）；用 Zig 写的，falcon 没法往里加自己的协议（快照、程序状态、前台命令）。设计照搬，代码不用。
- **自研守护进程 + alacritty_terminal**：纯 Rust、零新工具链、与客户端同一份实现、panic 能被 `catch_unwind` 接住。没选，是因为序列化器和 OSC 7 / 133 / 7501 的解析都要自己写，每会话内存约是 ghostty 的 3 倍；而它最大的优势（崩溃隔离）已经由两进程模型拿到了。如果 P0 的交叉链接打不通，这是退路：服务端协议传的是 VT 字节，不绑定引擎，换引擎只动 VT 进程。
- **单进程 + `ReleaseSafe`**：结构最简单，但 libghostty-vt 的任何一次中止都会带走会话。
- **shpool**：Rust 写的，思路相同（用 vt100 的 fork 做接回重绘），但只支持 Linux / macOS，而且是一个独立的守护服务，不是可嵌入的库。
- **mosh**：只解决网络漫游，不解决后端重启（ADR 0001 已否）。

## 后记（2026-10-13）：实施

按本 ADR 一天内做完了全部分期（设计文档第 16 节），验证结果在设计文档第 15 节。与初稿不同、或初稿没料到的地方：

- **快照怎么接上**：初稿是"按字节偏移在客户端拼接"，实施时改成 holder 在这条连接上扣住快照之后的输出（决定三）。拼接规则因此从 falcon-proto 里消失，服务端只要按到达顺序处理；顺带让多个 Viewer 可以搭同一班快照。
- **libghostty-vt 有 continuation API**：快照切点落在半截序列 / 半个 UTF-8 字符上时，导出补齐用的原始字节。服务端还要去掉其中与自己 UTF-8 解码器重复的那半个字符（设计文档 5.5）。
- **格式化器省掉末尾空行**、**把模式放在正文之前**：语料比对时发现，导出时补回空行、把关自动换行等几个会改变排版的模式挪到正文之后（设计文档 6.2）。
- **程序状态的"进程退出"**：初稿是"前台从 CLI 变回 shell 就清"，用例发现 shell 的内建命令与命令之间的空档里前台也是 shell，改成每秒看一次、连续两次都在 shell 手里才清（设计文档 8）。
- **二进制按内容哈希命名**，不用 `.sha256` 旁文件（决定四）；本机也拷一份再跑，开发时重编不会换掉正在跑的 holder 的二进制。
- **bridge 的前导标记、退出码 5、Windows 上用 .NET Process 跑 bridge**（决定四）。
- **不需要 libc++**（构建与分发）。
- **ghostty 的构建脚本会被外层的 falcon 仓库带偏**：它用 `git describe` 取版本号，找到 falcon 的 tag 后直接 panic；xtask 用 `GIT_CEILING_DIRECTORIES` 挡住（设计文档 13.3）。
- **Windows 不阻塞切换**（决定七）。

## 后记（2026-10-14）：状态栏并进标题栏

终端窗口底部那条常驻状态栏去掉了，内容并进窗口标题栏（`falcon-ui/src/terminal/title_status.rs`）：名字后面一颗状态胶囊（图标、两个字、进度、计时，悬停是完整的一段），再是子任务数、程序那半句，标题栏底边一条线随状态变（进度线 / 扫光 / 呼吸的警示线 / 闪一下）。状态栏当初常驻是为了不让终端行数跟着状态变，标题栏本来就在，这个顾虑一起没了，终端还多出一行。

动画（转圈、扫光、呼吸、换状态时弹 / 抖）走 `falcon-ui/src/motion.rs` 自己的动画元素，**不看系统的「减少动态效果」**：GPUI 的 `with_animation` 在那个开关打开时整个定格，实测开着它的机器上运行中的图标一动不动、看不出程序还活着；而 macOS 自己的进度指示在减少动态效果下也照转。持续动画限到 30 帧（`motion::FPS`）。动画重画的是发起它的视图：标题栏里会动的胶囊与底边线因此各是一个小视图，不让整块画布逐帧重排（画布重画一次 release 下约 1.2–1.5ms）；侧栏的转圈直接画在侧栏里，实测只多约 0.1ms。

## 后记（2026-10-14）：给 ghostty 打补丁——正文里的 OSC 8

决定三把"正文里的 OSC 8 链接不导出"记成了已知差异，实际代价比当时想的大：持久会话一接回，屏幕与历史里的链接全丢，⌘+点击打不开；而让 Claude Code 等认 `supports-hyperlinks` 的 CLI 发 OSC 8（服务端给 POSIX 宿主机的会话写 `FORCE_HYPERLINK=1`，`term_env.rs`）之后，Markdown 链接可能只剩链接文字、地址全在 OSC 8 里——接回就等于地址没了。上游 main 还没支持（`formatter.zig` 的注释："In the future we can support emitting OSC 8 hyperlinks for VT output as well"）。

做法：**钉死的 commit 不变，在上面打补丁**（`native/sessiond/ghostty-patches/`，`ghostty.pin` 的 `patches=` 记补丁集的哈希，xtask 解包后 `patch -p1`）。补丁只改 `formatter.zig`：VT 与 HTML 走同一条写链接的路，结束的链接先收尾再补空白格（设计文档 6.2 第 6 条）。

- 静态库的标识变成 `<commit>+falcon.<补丁哈希前 12 位>`，VT 进程报给服务端的 ghostty 版本也是它。服务端照决定四的规则看到版本不同就热升级存量会话的 VT 进程，旧会话下次接回就用上新的格式化器，不用等它们自然消亡（实测）。二进制快照的格式没变（同一个 commit），交接走快照、不必退回原始尾巴。
- 头文件不动，绑定不用重生；holder 的协议不变。
- 升 commit 时逐个核对补丁：还要不要（上游支持了就删掉补丁与 `patches=`）、还打不打得上。

## 后记（2026-10-14）：自动标题借 OSC 7501 与终端标题

会话的自动标题原先就是探到的前台命令，两个毛病：macOS 上 Claude Code 原生安装版的进程名是版本号（可执行文件叫 `versions/2.1.299`，`proc_name` 拿到 `2.1.299`），几个 Claude 会话又都叫同一个名字；探测只在有 Viewer 时做，后台会话的标题停在最后一次有人看的时候。

- **holder 存终端标题**（OSC 0 / 2）并发 `Event::Title`（协议里早有这个变体，之前收到就丢）。shell 自己写的不收：标题到达那一刻前台是 shell（`fg::at_shell`）就当没有——只有 holder 看得到这一刻，服务端隔着一来一回的探测分不清标题是程序起来之前还是之后写的。
- **服务端拼标题**（`sessions/auto_title.rs`，经原来的 `Session.title` / WS `{type:"title"}` 下发，客户端不用改）：报了 7501 的程序用它设的终端标题（Claude Code 写的是会话主题），其次是它自报的 `app`，其余程序照旧是前台命令。终端标题只在程序报过 7501 时采信。探测看到 shell 时，此前到的素材作废（防 kill -9 留下的记录冒名）。
- **没人看着也探**：持久会话的探测是守护进程连接上的一个小帧，不再是宿主机上的一条命令，只在有 Viewer 时探的理由没了。仍不定时轮询，只跟着输出、程序状态、程序标题点火。

细节在设计文档第 9 节。0.3.2 及之前的 holder 不发标题，那些会话只有 `app` 与前台命令。
