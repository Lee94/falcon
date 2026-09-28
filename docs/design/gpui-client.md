# Falcon 原生客户端（GPUI）设计方案

> 状态：**已实现**（2026-09-24）。定下来的做法、与本提案的出入、踩过的坑与测量数据见 [ADR 0015](../adr/0015-gpui-native-client.md)；本文保留为提案原文。原提案：范围已拍板，见 §10 · 范围：新增 `native/`（Rust + GPUI 桌面客户端），**功能与 web 桌面端对齐**；server 与 web 不动（§8 的小缺口除外） · 平台：先 macOS / Apple Silicon，Windows 预留 · 许可：`native/` 为 GPL-3.0-or-later（抄了 Zed 的代码，个人使用）
>
> 术语一律沿用 [CONTEXT.md](../../CONTEXT.md)：Project / Terminal Session / Window / Viewer / Detach / Terminate / 宿主机 / 远端主机。本文不新造词。

---

## 0. 一句话

**用 GPUI 写一个功能与 web 桌面端对齐的原生客户端，能连本机与任意一台 falcon 服务端；后端、协议、会话语义一行不改。**

原生客户端是 falcon server 的又一个 Viewer，和浏览器平起平坐：同一套 REST（`/api/*`）、同一条 `/ws/sessions/:id`、同一个登录 cookie。web 端继续存在——手机与临时借用的电脑仍然靠它（移动壳 `MobileShell` 不进原生）。

"falcon 服务端"指跑着 falcon server 的那台机器，与 CONTEXT.md 里的**宿主机**（会话实际跑在哪）、**远端主机**（SSH Host）是三件事：连到一台远处的 falcon 服务端，它的"本地项目"在**那台**机器上，不在你面前这台 Mac 上。

---

## 1. 要快在哪里（先量，再做）

"提升性能"必须落到可测的指标上，否则重写一个前端的成本没有对照物。

### 1.1 web 端的结构性开销（现状，带出处）

| # | 开销 | 出处 | 原生能不能消掉 |
|---|---|---|---|
| 1 | **不在场的窗口不卸载**：挪到 `left:-10000` + `visibility:hidden`，WS、xterm 实例、输出解析都照跑。开过的会话越多，常驻的终端实例越多 | `WorkCanvas.tsx:955-966`，ADR 0012 | 能：解析照跑（状态必须跟上），但**不画**的视图在 GPUI 里零渲染成本；Term 状态放在 Entity 里，与视图位置无关 |
| 2 | **每扇终端一个 WebGL / WebGPU 上下文**：WebGL 被回收时回落 DOM 渲染；WebGPU 路径跨终端不共享图集 | `termAdapter.ts:141-150`，ADR 0005 "未做" | 能：一个窗口一个 GPU 场景，所有终端共用字形图集 |
| 3 | **xterm 的 write 是异步的**：TUI 高频重绘时积压，40 帧/tick 积压约 1.2s | ADR 0005 基准表 | 能：解析在后台线程同步完成，UI 每帧只取最新快照 |
| 4 | **单个 zustand store + 约 130 个 selector**：resize 每帧进来，值不变都得拦住；`CommitRow` 不 memo 方向键就掉帧 | `App.tsx:134`，`GitPanel.tsx:1010` | 能：GPUI 按 Entity 粒度 `notify`，没有全局 selector 扫描 |
| 5 | **xterm 画布不能换父节点**：窗口必须绝对定位、DOM 顺序恒定，拖窗口本质是一次 resize | ADR 0012 | 问题本身消失（见 §4.2） |
| 6 | **字体异步加载**：Maple CJK 片约 5MB，终端等 `fonts.ready` / `loadingdone` 后重建图集，另有 12 帧 bump 兜底 | `TerminalView.tsx:378-400` | 能：字体随程序注册，启动即可用，没有"字到了再重画" |
| 7 | **回放解析在主线程**：重连 / 打开一个 4MB ring 的会话，整份回放在主线程解析；恢复工作区时 N 个会话同时回放 | `TerminalView.tsx:262-272` | 能：回放在后台线程解析到一个新 Term，完成后一次性换上（§3.1） |
| 8 | **浏览器保留键**：快捷键被迫配 Alt 别名 | `shortcuts.ts:9-12` | 能：原生菜单与 keymap 没有这个限制 |

### 1.2 GPUI 省不掉的部分

- **网络 + server + zellij 这一段两边完全一样**。按键先过 WS 到 Node，再进 zellij，zellij 重绘后才回来。原生只省客户端两端的开销；连远处的 falcon 服务端时，网络往返会压过一切客户端优化（不做 mosh 式本地回显预测）。
- **单终端吞吐 web 已经够了**：ADR 0005 的基准里，1× 全速下 xterm-webgl / rio-webgpu / rio-canvas 在 55MB/s 都满帧，只有 4× 节流才拉开差距。何况 zellij 自己会限制输出节奏，客户端收到的是 zellij 的重绘，不是程序的原始洪流。
- **GPUI 自己也有延迟账**：zed#26900 有人报告输入到上屏 ≥ 4 帧，2025-03 提出，至今未关。"原生一定更快"不能当前提，得量。

所以收益预期集中在：**多终端并发、回放 / 恢复工作区、常驻内存、拖动排布时的帧时间、冷启动**，以及长列表（git 历史、diff、文件表、飞书项目）不再需要 memo / 虚拟化这类手工优化。单终端打字手感是否更好，要看 M1 的实测。

### 1.3 指标

P0 先在 web 上测基线，再定目标。下表的"目标"是初定值，基线测完回填。全部在本机 falcon 服务端上测，排除网络。

| 指标 | 场景 | 测法（web / 原生） | 初定目标 |
|---|---|---|---|
| **M1 输入延迟** | 单终端，zellij 里跑 `cat` 回显 | 客户端段：keydown → `ws.send`、收到 OUTPUT → 该帧 present，两边打点；端到端：外部测（Typometer 类工具或 240fps 摄像） | 端到端**不差于** web；客户端段 p95 ≤ 1 帧 |
| **M2 并发输出** | 8 扇终端同时跑持续全屏重绘的 TUI（btop / 自造的 20 帧/tick 脚本） | Chrome Performance trace / GPUI 帧计时，外加客户端进程 CPU% | p95 帧时间 ≤ 8.3ms（120Hz），CPU ≤ web 的 1/2 |
| **M3 冷启动** | 启动 → 恢复 4 列 8 会话 → 第一个终端可输入 | 墙钟打点 | ≤ web 的 1/2 |
| **M4 回放** | 打开一个 ring 满 4MB 的会话；以及 8 个会话同时回放 | WS open → 首帧完整画面 | 单会话 ≤ 50ms；8 会话期间 UI 不掉帧 |
| **M5 常驻内存** | 8 / 16 个会话开着（一半不在场） | 活动监视器"内存"：Chrome 该 tab 的渲染进程 + GPU 进程分摊 / 原生进程 | ≤ web 的 1/3 |
| **M6 拖动排布** | 4 扇终端在输出时拖列宽 | 帧时间 p95 | ≤ 8.3ms，且终端 resize 不闪 |

### 1.4 Go / No-go

P0 结束时同时满足以下几条才进 P1：

- M2、M4、M5 至少两项明显优于 web（上表目标），M1 端到端不差于 web；
- macOS 系统拼音输入法能用：候选框跟着光标，组字串画在光标处，上屏正确；
- Berkeley Mono + Maple CJK 回退 + Nerd 图标渲染正确，盒线无缝。

不满足就停。把同样的精力投到 web 侧（跨终端共享 WebGPU 图集、默认 rio-webgpu、回放挪进 Worker 解析），账面上更划算。功能对齐是 P1–P3 的事，不影响 P0 的判断。

---

## 2. 总体架构

```
┌──────────────────────── Falcon.app（GPUI，单进程，每台 falcon 服务端一个窗口）───────┐
│ UI 线程（GPUI 前台执行器）                                                          │
│   ServerWindow ─ Sidebar │ WorkCanvas（列 → 窗口）│ RightBar │ Palette │ Dialogs     │
│                    └─ TerminalPane ──每帧取快照──▶ Arc<FairMutex<Term>>             │
│                                                        ▲                            │
│ 网络运行时（独立线程上的 tokio）                       │ OUTPUT：加锁 advance         │
│   ApiClient × 服务端（reqwest + rustls + cookie）      │ REPLAY：离锁解析后整体换     │
│   SessionSocket × N（tokio-tungstenite）───────────────┘ 置脏位 → 每帧至多唤醒一次    │
└───────────┬──────────────────────────────────────────┬──────────────────────────────┘
            │ 本机：http://127.0.0.1:4923              │ 远处：https://falcon.example（反代）
┌───────────▼──────────── falcon server（launchd）─┐ ┌──▼──── falcon server（别的机器）─┐
│ SessionManager · RingBuffer · PTY / SSH · Zellij │ │            同一份代码             │
└──────────────────────────────────────────────────┘ └───────────────────────────────────┘
```

### 决定一：只换前端，后端与协议不动

server 是 I/O 密集型，瓶颈不在 Node；会话持久性的全部承诺（Zellij、SSH 退避重连、回放前缀）都在后端，重写一遍只会引入回归。原生客户端对 server 来说就是一个会用 cookie 的 Viewer：WS 升级不检查 Origin，带上 `Cookie: falcon_token=…` 即可（`ws.ts:32`）。

`packages/shared` 仍是协议的唯一真相来源，Rust 侧只是它的消费者（决定五）。

### 决定二：VT 核心用 alacritty_terminal，隔在 `falcon-term` 接口后面

候选有三个，结论先写在这里，理由见 §3.3：

- **alacritty_terminal**（Apache-2.0）：纯 Rust，`Term` 可跨线程；字宽口径与 zellij 同源（都是 `unicode-width 0.2`）；Zed 的终端就是它加 GPUI，而 Zed 的终端代码现在可以直接抄（决定三）。**选它。**
- **libghostty-vt 0.2.1**（MIT/Apache）：自带按键 / 鼠标编码、字素簇、kitty 图形；但 API 明说不稳定，构建要 Zig 0.16 并联网拉源码，全部类型 `!Send`。**观望。**
- **rio-vt 0.5.28**（MIT，2026-07 才从 rio 抽出来）：字宽用 rio 自己的 Unicode 表 + 字素簇，同样有与 zellij 对不齐的风险，作为库的历史也太短。**观望。**

`falcon-term` 定义"喂字节 / 取快照 / 编码输入 / 事件回调"四件事（形状对应 web 的 `lib/termAdapter.ts`），换核心只换实现。

### 决定三：GPUI 走 gpui-kit 的锁定版本；Zed 的代码按文件抄，不按 crate 依赖

- 官方 `gpui` 在 crates.io 停在 0.2.2（2025-10），平台层 crate 全是 `publish = false`。能用的渠道是 zed 仓库 git rev，或 Longbridge 每周发的 `gpui-pre` 快照。
- **直接依赖 `gpui-kit` 总包 0.6.x**（开 component 与 assets 两个 feature），不自己拼单个 crate。它一个依赖就把 `gpui-pre`、`gpui-pre-platform`（窗口 / 应用启动器，光有 `gpui-pre` 起不来窗口）、`gpui-base`、`gpui-component`、默认资源锁在同一版本上，其中 `gpui-pre` 系列是 `=` 精确锁。自己分别依赖这五个 crate，只会多出"版本没对齐"这一类错误。组件层（`gpui-component`）提供 Input / TextArea / 代码编辑器（tree-sitter 高亮）、Tree、VirtualList、DataTable、Resizable、Popover / Menu / Dialog / Sheet、Tabs、Command、Markdown、图表；WebView 另加同版本的 `gpui-wry`。**升级 GPUI 就是升级 gpui-kit，是一次有意的发版动作**（与 rioterm 锁精确版本同理）。
- **Zed 的代码可以抄（个人使用，`native/` 整体声明 GPL-3.0-or-later），但只按文件抄，不当 crate 依赖**：Zed 的 `terminal` crate 牵着 `settings` / `theme` / `task` / `util` / `release_channel` 一串内部 crate，而且会带进第二份 `gpui`（zed git 版），与 gpui-component 用的 `gpui-pre` 是两个互不相认的 crate，类型对不上。所以：
  - 从 **`gpui-pre` 对应的那个 zed commit**（0.3.6 ↔ zed@bcf6582，升级时一起换）抄文件，API 天然对齐；
  - 抄来的文件保留原版权头，首行注明"出自 zed@<commit> 的 <路径>"，改动处照本仓库的注释习惯写"为什么"；
  - `alacritty_terminal` 跟着那个 commit 用 Zed 的 fork rev，省掉抄来的代码对上游 API 的适配。
- 要抄的清单与改法见 §3.4 / §3.5 / §3.6。**tty7**（l0ng-ai/tty7，Apache-2.0，gpui + gpui-component + alacritty_terminal 的终端工作台，形态与我们高度重合）同样值得逐段读。
- server 与 web 通过网络协议与 `native/` 通信，是独立程序，不受 GPL 影响；Berkeley Mono 的字体授权与此无关，仍照 web 的口径。

### 决定四：网络层与 GPUI 解耦

GPUI 有自己的执行器，不是 tokio。reqwest / tungstenite 跑在一个独立线程上的 tokio 运行时里，与 UI 之间只走 channel：UI 侧用 `cx.spawn` 等 channel，网络侧不认识任何 GPUI 类型。这样 `falcon-client` 可以脱离窗口单测，也能对真 server 跑 e2e（§6.3）。

### 决定五：协议类型手写 Rust 镜像 + 服务端产出的 fixture 做契约测试

- 镜像原生客户端用到的全部类型（功能对齐之后基本就是 `shared/src/index.ts` 的全集），serde 手写，保留 TS 里的注释。
- 由 server 产出一组真实响应的 JSON fixture（起临时数据目录的 server，建项目、建会话，把各 GET 的响应落盘），Rust 测试逐个反序列化。server 改字段名或删字段，重生 fixture 时 Rust 测试就红。
- 不走"TS → JSON Schema → typify"的生成路线：shared 里大量可辨识联合与"存在 ⇔ 某种项目"的可选字段，生成出来的 Rust 类型难读，注释也会丢。也不反过来让 Rust 当真相来源——那会推翻 CLAUDE.md 里"shared 是唯一真相来源"这条。

### 决定六：功能与 web 桌面端对齐，分期补齐；没做完的临时走浏览器

目标是 web 桌面端的**全部**功能：终端与画布、侧栏、命令面板、文件面板与预览（含 HTML）、修改 / 差异 / 历史、转发 / 公网发布、飞书项目、派生与各种表单、设置、会话总览、Zellij 安装、askpass。分期见 §5。

还没移植的功能，入口先放"在浏览器中打开"（打开同一台 falcon 服务端的 web 界面），**P3 结束时这些入口全部删掉**。不做的只有移动壳（`MobileShell` / `MobileSwitcher`）和 PWA 安装——那是另一种设备形态。

### 决定七：一台 falcon 服务端一个窗口；本机服务由 App 托管

- **服务端配置**（名称 + 基址 URL）存在客户端本地。本机配置自动存在，指向 launchd 托管的服务；远处的配置由用户添加（§4.7）。
- **一个窗口只连一台 falcon 服务端**，连多台就开多个窗口（GPUI 原生多窗口）。项目 id、会话 id、窗口 key（`lib/paneKey.ts`）都只在一台服务端内唯一，不给它们加服务端前缀，排布模型与持久化格式原样照搬；工作区状态按服务端配置分开存。
- **本机服务托管**照 `launcher.sh`：Resources 里带 SEA 服务程序，启动时 `service install`（幂等，兼作升级）、等 4923 起来。会话活在 launchd 服务里，**退出 App = 所有窗口 Detach**，永远不会 Terminate。这一段藏在 `LocalService` 接口后面，只有 macOS 实现（SEA 不支持 Windows 目标，见 §4.8）。

---

## 3. 终端链路

这是整个方案里最难的部分，也是性能收益的主要来源。

### 3.1 线程与数据流

- 每个会话一个 `Arc<FairMutex<Term>>`，由 `TerminalModel`（GPUI Entity）持有；视图只是它的一扇窗口。
- **OUTPUT 帧**（server 已按 16ms 合并，单帧很小）：网络线程加锁 `advance`，置脏位；脏位由 0 变 1 时才唤醒 UI，所以每帧至多 `notify` 一次。注意用 Zed 的 `write_raw_output` 路径，**不要**用 `write_output`——后者会做 LF → CRLF 归一化，那是给"不经 PTY 的文本"用的，我们的字节流本来就是完整的终端流。
- **REPLAY 帧**（最大 4MB，可能在连接中途任意时刻到达，`ws.ts:102-122` 的背压重同步）：在 `spawn_blocking` 里**新建一个 Term 离锁解析**，解析完加锁**整体替换**。语义就是 web 的"reset 再 write"，但 UI 线程既不会被 4MB 解析卡住，也不会看到半截画面。
- **回放期间产生的应答一律丢弃**：回放里带着 zellij attach 时发的查询（DA、DSR 之类），再答一次就是往 zellij 里敲一串垃圾（`termEnv.ts` 里 `OscColorGate` 的注释写过同样的坑）。web 端的 xterm 是否有这个问题，P0 顺带核对。
- **绘制**：UI 线程加锁，把可见区拷成一份快照（格子、光标、选区、模式位，对应 Zed 的 `last_content`），立即解锁，再用快照排版绘制。锁只持有一次拷贝的时间。
- **不在场的窗口**（别的项目的列）：Term 照常解析（状态必须跟上），视图不在树里就不画，成本只剩解析。

### 3.2 连接时序（必须与 web 一致）

照 `TerminalView.tsx` 的现有行为逐条对齐，这些都是踩过的坑：

1. 连上后**先发 `appearance`（带 background / foreground）**，server 的 `OscColorGate` 才会代答 OSC 10/11/12。原生客户端**不答**颜色查询（alacritty 的 `ColorRequest` 事件直接忽略），否则多个 Viewer 会各答一次。
2. **每条新 socket 都强制发一次已量好的 `resize`**，按 socket 去重。它是持久会话懒惰接回的信号（`sessions/termSize.ts`），不发就一直停在 Unverified。
3. **格子没量好之前不发 resize**。原生的字体启动即就绪，这一步从"等字体"变成"等首次布局"。
4. `state` / `reconnecting` / `error` / `title` / `askpass` 五种控制消息照 web 处理；标题以 server 推的 `title` 为准，走 `sessionTitle` 同一套规则（§4.3）。
5. 关闭码 **4401 = 未认证**，不重连，走重新登录（§4.7）；其他断开按 1s×2ⁿ 退避、封顶 15s；系统唤醒 / 网络恢复时立即重连，重连成功后补拉一次会话列表。
6. `input` 超过 1MB 要分片（server `maxPayload` 1MB，`index.ts:62`）。
7. **每 30s 发一次 WS ping**：远处的 falcon 服务端通常在反向代理后面，代理的 idle timeout 会悄悄掐掉安静的连接（server 用的 `ws` 库自动回 pong）。

### 3.3 VT 核心对比

| 维度 | alacritty_terminal 0.26 | libghostty-vt 0.2.1 | rio-vt 0.5.28 |
|---|---|---|---|
| 许可 | Apache-2.0 | MIT / Apache（绑定）；Ghostty 本体 MIT | MIT |
| 构建 | 纯 Rust | 需要 Zig 0.16；build.rs 默认联网拉固定 commit 的 Ghostty 源码 | 纯 Rust；默认带 PTY feature，要 `default-features = false` |
| 线程 | `Term: Send`，后台解析 + UI 加锁取快照 | 全部 `!Send` / `!Sync`，要么钉在专属线程再拷快照，要么全在主线程 | 未核实 |
| 字宽与 zellij 对齐 | `unicode-width 0.2`，与 zellij 的 `unicode-width 0.2.2` **同源** | 自带 Unicode 表 + 字素簇，ZWJ / VS16 emoji 可能与 zellij 算得不一样（整行错位）；能否切回老口径未查到 | `rio-unicode` 自带表 + UAX #29，同样的风险 |
| 输入编码 | 库本身不提供；**Zed 的 `mappings/keys.rs` / `mouse.rs` 可以直接抄** | 自带 KeyEncoder（含 kitty 键盘协议）、MouseEncoder、焦点、粘贴校验 | 未核实 |
| 其他能力 | scrollback / 重排、选区、正则搜索、vi 模式、OSC 8、事件（标题、OSC 52、颜色查询、PtyWrite）、damage | scrollback / 重排、带脏行的 RenderState、选区、导出、kitty 图形；OSC 52 / 标题 / 颜色查询回调文档未写 | grid、scrollback、选区、搜索、图片协议 |
| GPUI 先例 | Zed、tty7、zortax/gpui-terminal | 无（官方示例用 macroquad） | 无 |
| 稳定性 | 0.x，但 Zed 生产用了几年 | 作者明说会有破坏性变更 | 作为独立库只有两个月 |

**中间隔着 zellij，这张表的权重和一般终端不一样**：客户端只需忠实渲染 zellij 发出来的那部分转义序列，Ghostty 最强的协议覆盖面（kitty 图形等）大半用不上；反过来，"字宽算得跟 zellij 一样"比"字宽算得更正确"重要。能抄 Zed 之后，alacritty 唯一的短板（输入编码要自己写）也没了。

P0 做一次**差分测试**：从 RingBuffer 录几段真实的 zellij 输出（Claude Code 会话、btop、中文 + emoji 混排），分别喂给三个核心，比解析耗时与最终 grid。以下任一成立时再考虑换 libghostty-vt：API 稳定且确认支持 Windows；想让 web 也换成同一个核心（ghostty-web 提供 xterm.js 兼容 API 的 wasm 版本）；emoji / 复杂文字成了真实的用户抱怨。

### 3.4 输入编码（`falcon-term::encode`）

| 事项 | 做法 | 来源 |
|---|---|---|
| 按键 → 字节 | **抄** Zed `terminal/src/mappings/keys.rs`（442 行，按 `TermMode` 编 legacy 序列，含 macOS 的 option-as-meta）；把 `gpui::Keystroke` 之外的依赖剥掉 | zed |
| kitty 键盘协议 | **关**（`term::Config.kitty_keyboard = false`）。Zed 的 keys.rs 也不编 kitty；打开后 zellij 协商成功就会期待 kitty 格式的按键 | — |
| 鼠标上报 | **抄** Zed `mappings/mouse.rs`；Shift 绕过上报做本地选区（web 的 rio 路径同一口径，`lib/rio/mouse.ts` 的单测用例拿来对照） | zed + `mouse.ts` |
| 滚轮 | 移植 `WheelAccumulator`：程序接管鼠标时每个事件最多折成一次点击，本地 scrollback 按行推。滚动归 zellij（alt screen，本地无 scrollback），与 web 一致；Zed 的滚轮口径**不**照搬，它面对的不是 zellij | `lib/rio/wheel.ts` |
| 焦点事件 | `?1004h` 时发 `CSI I` / `CSI O` | — |
| 粘贴 | `?2004h` 时包 bracketed paste，并剔除载荷里的 `ESC[200~` / `ESC[201~`，防止粘贴内容逃出括号 | — |
| 全局键穿透 | 终端的 key context 只吃自己的键，全局快捷键照常冒泡；⌘C 仅在有选区时复制 | `lib/rio/keyRoute.ts` |

### 3.5 渲染（`TerminalElement`）

**以 Zed `terminal_view/src/terminal_element.rs` 为底本抄**，只留绘制与输入部分，剥掉 workspace / settings / 任务 / 路径跳转：

- `layout_grid`：逐行遍历快照，同样式的相邻格合成 `BatchedTextRun`，`shape_line` 排版后绘制；背景色合并成 `LayoutRect`。GPUI 的行排版缓存跨帧复用相同的行，没变的行几乎不花钱。
- `BlockElementLayoutRect`：块元素自己画成矩形，不走字形，行高与字体不一致时也不出缝。盒线字符同理补上（web 的 rio 渲染器用 sprite 解决同一个问题，ADR 0005）。
- 宽字符占两格，按格宽强制定位，不信字体给的 advance。
- 光标（块 / 竖线 / 下划线、失焦空心）、闪烁（窗口不可见时停）、选区高亮、OSC 8 链接下划线。
- 颜色从 `falcon-theme` 的 ANSI 16 色 + 前景 / 背景 / 光标 / 选区取，替换 Zed 的 `theme` 依赖。
- 行高、字号、字重设置沿用 web 的 `falcon.term` 字段语义。

### 3.6 IME

从 Zed 的终端元素里一并抄 `InputHandler` 那一段（`ime_cursor_bounds`、组字串绘制）：

- `bounds_for_range` 返回光标格的屏幕矩形，候选框跟着光标走；
- 组字串（marked text）画在光标处，覆盖在格子上，不进 PTY；
- 上屏文本走 `input`。

macOS 上 Zed 终端就是这么做的，路径可行。Windows 候选框位置的问题（zed#56149）刚在 2026-09-18 修掉，移植 Windows 时要按那时的 zed commit 重抄这一段。

### 3.7 剪贴板、图片粘贴、链接、选区

- **OSC 52**：`ClipboardStore` 事件 → 写系统剪贴板，只写不读（与 web 一致）。
- **图片粘贴**：剪贴板里只有位图时，`POST /api/sessions/:id/paste-image` 上传，把返回的宿主机路径粘进终端；同时有文本时贴文本（CONTEXT.md「Image Paste」）。拖入图片文件同理（GPUI 的外部文件拖放）。连的是远处的 falcon 服务端时这条路一样成立——图片本来就是传到宿主机的。
- **链接**：OSC 8 超链接加正则识别 URL，⌘+点击用系统浏览器打开。
- **选区**：松开鼠标即复制（与 web 一致）；右键菜单保留复制 / 粘贴 / 清屏。

### 3.8 字体

- 默认 Berkeley Mono TX-02。web 的字体栈是 Symbols Nerd Font Mono → Berkeley → Ioskeley → Maple CN → 系统 CJK / emoji（`term.ts:88-141`）。**图标字体排在主字体前面**是故意的：Maple 自带的 NF 图标是宽形，会被裁掉。GPUI 永远先查主字体，所以用 `FontFallbacks` 挂 Ioskeley → Maple → 系统 CJK / emoji，PUA 区（U+E000–F8FF）则在切 run 时按码位直接指定 Symbols 字体，不交给回退。
- **GPUI 不吃 WOFF2**（Windows / Linux 源码里没有 WOFF2 解包，macOS 未证实），所以 `vendor-*` 脚本要从官方发行包另抽一份 TTF / OTF，放在 gitignore 的目录里，构建时 `include_bytes!` 或打包进 Resources。
- 启动时 `add_fonts` 一次注册完。web 那套"等字体到了再重建图集"的逻辑整段不需要。

---

## 4. 界面

### 4.1 浮动岛与主题

- 骨架照 ADR 0011：窗口底 `app`，侧栏 / 画布列 / 右面板是圆角岛，岛间 6px 缝，不画分栏边框；岛内嵌块用 `sunken`；圆角走同一套阶梯（sm 8 / md 10 / lg 12 / xl 16）。
- `falcon-theme` 移植 `lib/theme/`：Ghostty 主题解析 / 序列化、浅色 / 深色双槽位、OKLab 派生。派生出的语义 token 是一个 Rust 结构体，同时喂给我们自己的组件和 gpui-component 的 Theme。**界面色只准用语义 token** 这条规则照搬。
- 明暗模式跟随系统：读 `window.appearance()` 并监听变化；`.dark` 语义仍按主题底色亮度判，不按明暗模式。
- 463 套 Ghostty 主题：`vendor-ghostty-themes` 额外产出一份 Rust 可读的数据文件，按需解析。主题选择器照 web：高亮到哪项就实时预览哪项。

### 4.2 列式画布

- 排布模型照搬 `store.columns`：`falcon-layout` 移植 `lib/layout.ts` 的纯函数（增删移、`syncColumns` 对账、按项目过滤、拖拽落点、像素几何、`pinned` 列永远在最后）。持久化的 JSON 形状与 web 一致，方便对照排查。
- **ADR 0012 的"绝对定位、DOM 顺序恒定"在这里不需要**：终端状态在 `TerminalModel` 里，`TerminalElement` 每帧按快照重画，挂在树的哪个位置都一样，拖窗口换列就是普通的重排。列宽 / 窗口高度只钉一边、双击还原这些交互规则保留。
- 拖动列缝时终端 resize 按帧合并，只在松手时写持久化（与 web 同样的节制）。
- 拖拽统一用 GPUI 的类型化拖拽：窗口标题栏、文件、飞书工作项（web 是自定义 dataTransfer 类型，`lib/meegleDrag.ts`）都是带类型的载荷，落点各自按类型认。

### 4.3 侧栏、命令面板、快捷键

- 侧栏树（服务器 → 文件夹 → checkout → 会话）移植 `lib/projectTree.ts`（这里的"服务器"是 SSH Host 分组，不是 falcon 服务端）；会话行默认收起，checkout 行尾只摆计数徽标 + 最重的异常状态，展开状态照 `sessionsOpen` 的语义存。
- **会话标题**移植 `lib/sessionTitle.ts`：手起的名字 → 前台命令 → agent 的 CLI 名 → shell 命令名。侧栏、命令面板、总览必须叫同一个名字——现在还要与 web 叫同一个名字（§6.3 的共享测试向量兜住）；窗口标题栏没标题时那一格空着。
- 命令面板用 gpui-component 的 Command，三种前缀（`@` 会话、`#` 项目、`>` 命令）与 web 一致；⌘P 文件搜索移植 `lib/fileSearch.ts` 的打分；动作集中在一处（对应 `useActions.ts`），菜单与面板共用。
- 快捷键移植 `shortcuts.ts` 的动作表，去掉为绕开浏览器而加的 Alt 别名，加上 macOS 菜单栏。
- **Detach / Terminate 语义照 CONTEXT.md**：关闭终端窗口 = Terminate，关之前 `GET /foreground` 问忙不忙（2s 超时）；Shift+关闭 = Detach；关掉 falcon 服务端的窗口或退出 App = 全部 Detach。

### 4.4 右侧栏与长尾面板

| 面板 | 要点 | 依赖的原生能力 |
|---|---|---|
| 文件（ADR 0009） | 平铺当前目录 + 路径栏 + 多选表（DataTable）；mkdir / rename / remove；宿主机路径一律用移植的 `lib/filePath.ts` 拼，**不用 `std::path`**（宿主机可能是 Linux，客户端将来可能是 Windows，同 `git/path.ts` 不用 `node:path` 的理由） | 系统存 / 选文件对话框 |
| 下载 / 上传（ADR 0008） | 下载：流式 GET 写到用户选的位置，进度 toast；上传：流式 PUT，带 `Content-Length`，同名 409 问过再 `overwrite=1`；文件夹上传 = mkdir -p 再逐个 PUT；从 Finder 拖进文件面板即上传 | reqwest 流式 body |
| 文件查看（ADR 0007） | 代码：gpui-component 的代码视图（tree-sitter），高亮色映射到 `derive.ts` 的语法色；图片：移植缩放 / 平移 / 锚点缩放的几何；Markdown：gpui-component 的 Markdown，相对链接照 `lib/mdLink.ts` 解析 | `img()`、tree-sitter |
| HTML 预览（ADR 0007） | 嵌 `gpui-wry` 的 WebView，加载 `rawBase + path`（URL 里只带作用域令牌）；**无痕 / 非持久数据存储，不注入登录 cookie**；只允许在同一 raw 前缀内导航，外链交给系统浏览器——ADR 0007 的安全边界原样保留。WebView 永远压在 GPUI 内容上面，所以**任何浮层（命令面板、菜单、对话框）打开时先把它隐藏**，关掉再显示 | gpui-wry |
| 修改 / 差异 / 历史 | 勾选提交、amend / push、restore、冲突处理、17 种 git op；diff 的 split / unified 用 `uniform_list`（不再需要"500 行以上才虚拟化"的分支）；提交图移植 `lib/gitGraph.ts` 的泳道算法，用 `canvas` + path 画 | `uniform_list`、`canvas` |
| 转发 / 公网发布（ADR 0014） | 表单增删改 + 状态轮询；文案照写"任何拿到链接的人都能访问" | — |
| 飞书项目（ADR 0010） | 三页（待办 / 空间 / 固定）+ 面板内下钻栈、粘贴链接解析、固定项、device-code 登录（打开浏览器 + 轮询状态）；移植 `lib/meegleCache.ts` 的 30s 缓存；右侧栏开着时切走不卸载 | 系统浏览器 |

### 4.5 弹层与表单

- askpass（WS 推送 + `GET /api/askpass/pending` 兜底）、Zellij 安装进度（`/ws/install/:projectId`）、确认框、重命名：P1 就做，它们都在"开 / 连终端"的主路径上。
- 项目 / 附属项目（含多仓库批量派生）/ 主机表单、FolderPicker（列的是**宿主机**上的目录，走 `/api/fs/list`，不能换成本机的系统文件夹选择器）、设置（外观 / 账户 / 关于 / 主机）、会话总览：P3。
- 表单一律用 gpui-component 的 Input / Select / Checkbox，校验与提交语义照 web 组件逐个对齐（附属项目删除前的脏文件 / ignored 文件确认框尤其要照 ADR 0002 原样呈现）。

### 4.6 文案

界面上不许硬编码中文。原生侧用独立的资源文件（`rust-i18n`，v1 只有中文），key 与 web 的 `i18n.ts` 同名，便于对照；两边不强行共用一份文件，免得为了原生去改 web 的 i18n 装载。

### 4.7 连接 falcon 服务端

- **服务端配置**：名称 + 基址（`http(s)://host:port`）。本机配置自动存在且不可删；远处的配置在"连接到…"里增删。
- **TLS**：rustls + `rustls-platform-verifier`，认系统信任库。自签证书的做法是把它加进钥匙串并设为信任，客户端不做自己的指纹固定。
- **明文 HTTP**：只在回环地址上静默允许；连非回环的 `http://` 时明确提示"访问密码与会话内容以明文传输"，用户确认后记住。
- **认证**：`POST /api/auth/login` 取 `falcon_token` cookie，之后 REST 与 WS 升级都带上。server 的登录 token 只在内存里（`auth.ts:16`），**server 一重启就全部失效**——远处的服务端升级、重启都会碰到。所以访问密码默认存钥匙串（按服务端配置分条），收到 401 / 4401 时自动重新登录一次，失败才弹登录框。这样不用改 server 去加一种长效令牌。
- **"本机"能力按配置开关**：在 Finder 中显示、用本机编辑器打开这类假设"宿主机文件就在我这台机器上"的动作，只在本机配置上出现；连的是远处的服务端时，文件一律走下载 / 上传。
- **窗口身份**：窗口标题栏写服务端名称；远处服务端的窗口在侧栏顶部显示它的基址，避免把命令敲错服务端。这和 ui-redesign 里"身份先于内容"是同一条原则。

### 4.8 Windows 预留（现在不做，但不堵路）

- 平台相关的东西只准出现在 `falcon-app` 的 `platform/` 模块里：钥匙串走 `keyring` crate（macOS Keychain / Windows 凭据管理器），数据目录走 `directories`，TLS 用 rustls 不用 native-tls，菜单栏在 Windows 上换成 gpui-component 的 TitleBar 菜单。
- **Windows 上没有"本机服务"**：SEA 单文件不支持 Windows 目标（README），`LocalService` 在 Windows 上没有实现，客户端只连远处的 falcon 服务端——这正好就是"连接其他 falcon 服务端"这条路。
- 移植时要重新核对：IME 候选框（zed#56149）、DirectWrite 内存字体、WS 与 rustls 在公司代理后面的表现。

### 4.9 本地状态

web 存在 localStorage 的那些（`falcon.workspace` / `falcon.term` / `falcon.theme` / `falcon.diffView` / `falcon.fileView` / `falcon.meegle.*` …）改存 `~/Library/Application Support/Falcon/`：全局偏好（主题、终端字体）一份，工作区（列排布、侧栏展开、`sessionsOpen`）按服务端配置各一份。只在松手 / 关窗时落盘，内容相同跳过。

---

## 5. 分期

| 功能 | P0 spike | P1 可日用 | P2 | P3 |
|---|---|---|---|---|
| 登录、本机服务托管 | ✓ | ✓ | | |
| 服务端配置、连远处的服务端、钥匙串自动重登、多窗口 | | ✓ | | |
| 单终端（连接、回放、输入、IME、字体） | ✓ | ✓ | | |
| 侧栏树、会话新建 / Terminate / Detach / 接回 / 清除 | 只读列表 | ✓ | | |
| 列式画布、拖窗口、列宽 / 窗高、持久化 | | ✓ | | |
| agent 会话（＋ 菜单里的各家 CLI） | | ✓ | | |
| askpass、Zellij 安装、busy 确认 | | ✓ | | |
| 主题（双槽位 + 目录 + 自定义）、外观设置 | 固定一套 | ✓ | | |
| 命令面板、快捷键、菜单栏 | | ✓ | | |
| 图片粘贴 / 拖入、OSC 52、链接 | | ✓ | | |
| 文件面板、⌘P、下载 / 上传 | | 浏览器 | ✓ | |
| 文件查看：代码高亮、图片、Markdown | | 浏览器 | ✓ | |
| 修改 / 差异 / 历史（git） | | 浏览器 | ✓ | |
| HTML 预览（WebView） | | 浏览器 | 浏览器 | ✓ |
| 转发 / 公网发布 | | 浏览器 | 浏览器 | ✓ |
| 飞书项目面板 | | 浏览器 | 浏览器 | ✓ |
| 项目 / 附属项目 / 主机表单、FolderPicker、账户设置 | | 浏览器 | 浏览器 | ✓ |
| 会话总览 | | 浏览器 | 浏览器 | ✓ |
| 删掉所有"在浏览器中打开" | | | | ✓ |
| Windows | | | | P4 |
| 移动端 | web 独有 | | | |

粗估（一人全职）：P0 约 2 周，P1 约 4–6 周，P2 约 6–8 周，P3 约 6–8 周，合计约 5–6 个月。抄 Zed 的终端代码主要压缩的是 P0 / P1；P2 / P3 的体量来自表单与面板本身（web 这部分约 1.2 万行 TSX），没有捷径。

**P0 交付物**：`native/` 骨架；登录；只读项目 / 会话列表；一扇真终端（回放、输入、resize、IME、Berkeley Mono）；M1–M6 的 web 基线与原生数据；三个 VT 核心的差分测试结果；一页 go / no-go 结论。

**P1 验收**：只用原生客户端完成"连本机与一台远处的 falcon 服务端 → 开项目 → 开几个 Claude / 终端 → 排好列 → 关 App 再开，排布与会话原样回来 → 远处的服务端重启后自动重登接回"。

---

## 6. 工程结构

### 6.1 目录与 crate

`native/` 是独立的 Cargo workspace，不进 pnpm workspace，根目录放 GPL-3.0 的 LICENSE。**GPUI 依赖只出现在 `falcon-app`**，逻辑尽量放在不依赖 GPUI 的 crate 里——GPUI 每次破坏性升级都只波及一层。

| crate | 职责 | 对应的 TS | 依赖 GPUI |
|---|---|---|---|
| `falcon-proto` | 协议类型镜像、WS 帧编解码 | `packages/shared/src/index.ts` | 否 |
| `falcon-client` | REST（reqwest + rustls + cookie）、SessionSocket（重连 / 退避 / 强制 resize / 分片 / ping）、安装 WS、流式上传下载、服务端配置与重登 | `api.ts`、`TerminalView.tsx` 的连接部分、`lib/fileTransfer.ts` | 否 |
| `falcon-term` | VT 核心接口 + alacritty 实现、快照、输入编码（抄自 Zed 的 mappings）、滚轮累加、选区 | `termAdapter.ts`、`lib/rio/mouse.ts` / `wheel.ts` / `keyRoute.ts`、`termInput.ts` | 仅 `Keystroke` 等少数类型 |
| `falcon-theme` | Ghostty 解析 / 序列化、派生、目录 | `lib/theme/*` | 否 |
| `falcon-core` | 列排布、paneKey、项目树、会话标题、宿主机路径、文件搜索打分、提交图泳道、飞书项目的分组 / 下钻 / 缓存 | `lib/layout.ts`、`paneKey.ts`、`projectTree.ts`、`sessionTitle.ts`、`filePath.ts`、`fileSearch.ts`、`gitGraph.ts`、`meegle*.ts` | 否 |
| `falcon-app` | GPUI 视图与元素（终端元素抄自 Zed）、快捷键、i18n、本地持久化、`platform/`（launchd、钥匙串、菜单栏） | `components/*`、`store.ts`、`useActions.ts` | 是 |

### 6.2 依赖锁定

- `gpui-kit` 与 `gpui-wry` 同版本一起升级（`gpui-pre` 的版本由 gpui-kit 决定，不单独写）；**同时**按新的 zed commit 重新比对抄来的文件（`git diff` 两个 commit 之间的那几个路径），再把 §3 的终端路径与 IME 在真机上过一遍。
- `alacritty_terminal` 用抄的那个 zed commit 所锁的 fork rev。
- macOS 构建开 `runtime_shaders`，免得 CI 机也得装 Xcode 的 Metal 编译器（本机有 Xcode，开发不受影响）。

### 6.3 门禁与测试

- 门禁：`cargo check` + `cargo test`（与 web 的 `tsc` 地位相同）。
- **共享测试向量（定了，做）**：每移植一个有 TS 单测的纯函数模块，就把它的用例抽成同目录的 `*.vectors.json`，TS 测试与 Rust 测试读同一份。两边实现一旦分叉，至少有一边会红。范围就是 `falcon-core` / `falcon-theme` / 滚轮与鼠标编码对应的那些 TS 模块；其余 TS 测试不动。会话标题这类"两个客户端必须叫同一个名字"的规则，只有这样才守得住。
- **协议 fixture**：见决定五。
- **e2e**：`falcon-client` 对真 server 跑"登录 → 建项目 → 建会话 → 连 WS → 输入 → 收到回显 → 杀掉 server 重启 → 自动重登接回 → Terminate"。这条会话链路目前没有任何自动化测试（CLAUDE.md），原生客户端顺手补上。数据目录要用短路径（zellij socket 路径 macOS 上限 104 字符）。
- GPU、IME、WebView 只能真机验，与 web 端 rio 的口径一致。

### 6.4 构建与打包

- `pnpm build:native` → `cargo build --release -p falcon-app`。
- `build-macos-pkg.mjs`：`CFBundleExecutable` 换成 GPUI 二进制，SEA 服务程序照旧放在 Resources，`launcher.sh` 的逻辑（`service install` → 等端口）挪进 `LocalService` 的 macOS 实现。其余（ad-hoc 签名、pkg、postinstall）不动。
- `vendor-berkeley-mono` 等字体脚本加一个 TTF 产物；`vendor-ghostty-themes` 加一份 Rust 数据产物。

---

## 7. 考虑过的方案

- **Tauri / Electron 套壳现有 web**：还是同一个浏览器引擎，§1.1 的开销一条都没消掉，只换来一个 Dock 图标。否决。
- **连 server 一起用 Rust 重写 / 学 tty7 自带守护进程替掉 Zellij**：性能瓶颈不在后端；会话持久性的全部承诺建在 Zellij 与现有 SessionManager 上（ADR 0001），重写等于重新踩一遍那些坑。否决。
- **原生壳 + WebView 承载除终端外的所有界面**：两套 UI 栈在一个进程里，WebView 永远压在 GPUI 上面，焦点与快捷键在两边来回抢。否决；只在 HTML 预览这一处用 WebView，并靠"浮层打开时隐藏"绕开遮挡。
- **长尾功能永久留在 web**（上一版的默认）：用户要求功能对齐，改为分期补齐。
- **直接依赖 Zed 的 `terminal` / `terminal_view` crate**：会带进第二份 gpui 与一串 Zed 内部 crate（settings、theme、task……），与 gpui-component 的 `gpui-pre` 类型不通。要么像 tty7 那样 fork gpui-component 锁 zed git rev，要么按文件抄。选后者：抄来的几个文件我们自己拥有，升级时只比对这几个路径。
- **直接 fork tty7**：它的模型是自带 daemon + 客户端直连 SSH（russh），与"falcon server 持有一切"的架构冲突。不 fork，但它的终端元素、IME、输入处理值得逐段读。
- **一个窗口同时挂多台 falcon 服务端**：id 只在一台服务端内唯一，要给项目 / 会话 / 窗口 key 全部加服务端前缀，排布模型与持久化格式都得改，还和 web 对不上。改成一台服务端一个窗口，零改动。
- **VT 核心用 libghostty-vt / rio-vt**：见 §3.3，隔在接口后面，满足条件再换。
- **不做原生、只优化 web**：这是 §1.4 no-go 时的退路。可做的事：跨终端共享 WebGPU 设备与图集、默认 rio-webgpu、回放放进 Worker 解析、不在场窗口降低解析优先级。P0 的基线数据同时也是这条路的输入。

---

## 8. 需要 server / web 配合的缺口

1. **协议 fixture 生成脚本**（决定五，P0 就要）：一个起临时 server、落盘各接口真实响应的脚本。
2. **共享测试向量**（§6.3，随移植进度做）：对应的 TS 单测改成读 `*.vectors.json`。
3. **全局事件推送**（P2 前评估）：现在会话列表 5s、askpass 1.5s、git 计数 8s 都靠轮询，项目 / 主机列表甚至不轮询——另一个客户端改了项目，这边看不到。原生与 web 同时开着、或者两台 Mac 连同一台 falcon 服务端时，这个缺口会被放大。一条 `/ws/events` 推 `sessions` / `projects` / `hosts` / `askpass` 的变化，两个前端都能去掉大部分轮询。P1 先照 web 的节奏轮询，真觉得"别处改了这边不知道"再做。

上一版里的"web 深链接"（给"在浏览器中打开"用）与"长效设备令牌"不再需要：前者随 P3 功能补齐而消失，后者由钥匙串存密码 + 自动重登代替。

---

## 9. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| GPUI 是 pre-1.0，走的是非官方快照（`gpui-pre`） | 升级有破坏性变更；上游修复要等快照 | 逻辑放在无 GPUI 依赖的 crate；精确锁版本；必要时临时切 zed git rev |
| 抄来的 Zed 代码与上游漂移 | 上游修了的 bug（IME、渲染）我们没有 | 升级 GPUI 时按新 commit 比对那几个路径（§6.2）；文件头记着出处 commit |
| 输入延迟（zed#26900，≥ 4 帧） | 直接打穿"提升性能"的前提 | P0 的 M1 端到端实测是 go / no-go 硬条件 |
| 两个前端长期并存 | 每个新功能做两遍、行为漂移 | 共享测试向量 + 协议 fixture；新功能先定协议与纯函数，再各画各的界面 |
| 工作量（约 5–6 人月） | P2 / P3 拖长，长期停在"一半原生一半浏览器" | 分期每一步都可独立日用；P1 之后按使用频率排 P2 / P3 内部的顺序 |
| WebView 压在 GPUI 上面 | HTML 预览窗口挡住浮层 | 浮层打开时隐藏 WebView；实在不行退回系统浏览器 |
| 远处服务端的明文 HTTP | 密码与终端内容被窃听 | 非回环 `http://` 明确提示；推荐反代 + TLS（README 已有此要求） |
| 无障碍（AccessKit 2026-05 才合入） | 读屏基本不可用 | 已知限制，跟随上游 |
| 调试工具弱（没有 DevTools 等价物） | 排查界面问题慢 | 开 inspector feature；快照 / 布局 / 编码都在纯函数层，靠单测兜住 |

---

## 10. 已拍板

| 问题 | 结论 | 来源 |
|---|---|---|
| 能否抄 GPL 代码 | 能。个人非商业使用，`native/` 声明 GPL-3.0-or-later；按文件抄，不按 crate 依赖（决定三） | 用户 |
| 长尾功能 | 全部原生化，P3 结束时删掉所有"在浏览器中打开"（决定六） | 用户 |
| 平台 | 先 macOS；Windows 以后可能做，现在只守住不堵路（§4.8） | 用户 |
| 连其他 falcon 服务端 | 要，P1 就做；一台服务端一个窗口（决定七、§4.7） | 用户 |
| 共享测试向量 | 做，限于移植的纯函数模块（§6.3） | 判断 |
| 重登方式 | 钥匙串存访问密码 + 自动重登，不改 server | 判断 |
| 自签证书 | 认系统信任库，不做客户端指纹固定 | 判断 |
| HTML 预览 | P3 嵌 WebView，浮层打开时隐藏 | 判断 |
| 全局事件推送 | P1 照 web 轮询，P2 前按实际体验评估 | 判断 |
| VT 核心 | alacritty_terminal，P0 用差分测试复核 | 判断 |
