# Agent 浏览器：看 agent 操控的浏览器，一键接管

状态：提议（2026-11-21），未实施。

在 falcon 的终端里跑 Claude Code / Codex，agent 经常要开浏览器：查文档、点后台、走一遍登录。浏览器在宿主机 B 上无界面地跑，用户看不到它在干什么，卡在验证码 / 二次验证 / 登录页时也帮不上手，只能等 agent 自己放弃。

这份 ADR 定的是：agent 一开浏览器，falcon 就在终端旁边显示它的实时画面和操作记录；用户点一下就能接管，用完交还给 agent。原则是**用户什么都不用配，界面上只有一个按钮**。

## 用户看到的

1. **不用配置。** 在 falcon 的终端里跑 agent，agent 用 agent-browser 打开网页即可。B 上只要装了 agent-browser。
2. **自动出现。** 终端窗口标题栏多一个「浏览器」标记（显示当前页面的域名）。这个会话**第一次**出现浏览器时，falcon 自动在终端旁边打开浏览器窗口；用户关掉后不再自动弹，点标记再打开。
3. **浏览器窗口**：中间是画面；顶条是地址、标签页数、状态和**一个按钮**；底部是可展开的「记录」。
4. **接管**：点「接管」，或者直接在画面上点一下（这一下只用来接管，不送进页面，免得误点）。窗口描边变强调色，顶条写「你在操作」。此时 agent 的浏览器命令会被拦下，它只能看、不能动。
5. **交还**：点「交还」。关掉浏览器窗口也算交还（有 30 秒宽限，网络断一下不会立刻交还）。
6. **记录**：按时间列出 agent 的每条命令、结果、控制台报错、页面跳转，以及谁在什么时候接管 / 交还。点一条可以看那一刻的画面缩略图。

| 状态 | 顶条 | 按钮 | 画面上的点击 / 按键 |
|---|---|---|---|
| Agent 在操作（默认） | 地址 · 「Agent 在操作」 | 接管 | 第一下点击 = 接管；按键不送 |
| 你在操作 | 地址 · 「你在操作 · 3 分钟」，窗口描边强调色 | 交还 | 全部送进页面 |
| 浏览器已关闭 | 「浏览器已关闭」，保留最后一帧 | 无 | 无 |

设置里只有一个开关：「agent 打开浏览器时自动弹出窗口」，默认开。

## 决定一：只对接 agent-browser 的 stream 协议

[agent-browser](https://github.com/vercel-labs/agent-browser)（vercel-labs）为每个浏览器会话起一个守护进程。这个守护进程**会自动**在 `127.0.0.1` 上监听一个系统分配的端口，提供 WebSocket stream 服务。一条连接上既有画面（JPEG 帧及尺寸信息），也有事件（`command` / `result` / `console` / `page_error` / `url` / `tabs` / `status`），还能注入输入（`input_mouse` / `input_keyboard` / `input_touch`）。它还支持按 ack 限速，帧不会在慢链路上堆积。它是一个 CLI，不挑 agent：Claude Code、Codex 或者 shell 脚本，只要调用 `agent-browser` 就都能接上。

没有选的方案：

- **Playwright MCP / 直接用 CDP**：每个 agent 接法都不一样。CDP 只能看到底层协议消息，看不到"agent 执行了 click @e3"这一层，记录也就没有意义。
- **browser-use 等框架**：它们自己带一套 agent 循环（Python），Claude Code / Codex 不会走它。
- **VNC / noVNC**：传的是整个桌面，B 上要有 Xvfb 和有界面的浏览器，带宽更大，而且不知道哪一步是哪条命令做的。

最低版本是 v0.43（以 stream 协议带 `seq` / ack pacing 为准），这个值定在一个常量里。版本低于它的会话不显示，在服务端日志里记一条。

## 决定二：在拥有会话的那台 falcon-server 上实现，只连本机回环

ADR 0027 之后，B 上的终端会话由 B 上的 falcon 节点持有，agent-browser 也跑在 B 上。所以这项功能**全部写在 falcon-server 里，并且只连 `ws://127.0.0.1:<端口>`**：

- 客户端直连 B 时，就是本机连接。
- 客户端经 A 的网关访问时，`/ws/browser/...` 也是 `/ws/` 开头的路径，网关会原样转给节点（`nodes/gateway.rs` 的 WebSocket 转发），**网关一行代码都不用改**，也不用自己去写 `forward_out`。
- B 不开端口、不改防火墙，也不用起 agent-browser 的面板。

B 上要准备的只有：装好 agent-browser 和它的浏览器（Linux 上用 `agent-browser install --with-deps` 装系统库）。不需要 Xvfb。0027 之前建的、不经节点的旧 SSH 会话不支持这项功能。

## 决定三：浏览器会话与终端会话一一对应，靠启动时注入的环境变量

**每个**终端会话都注入下面这些变量（不只是 agent 会话：用户也可能在普通 shell 里手动起 claude）。注入点和 `FALCON_SESSION_ID` 放在一起（`askpass/install.rs`）：

| 变量 | 值 | 作用 |
|---|---|---|
| `AGENT_BROWSER_SESSION` | `falcon-<会话 id>` | 这个终端里开的浏览器都归这个会话。名字只含字母、数字和 `-`，符合 agent-browser 对会话名的限制 |
| `AGENT_BROWSER_SOCKET_DIR` | `<数据目录>/agent-browser/run` | 固定守护进程的文件目录，falcon 才能找到它（见下） |
| `AGENT_BROWSER_ACTION_POLICY` | `<数据目录>/agent-browser/policy/<会话 id>.json` | 接管用的锁（决定五） |
| `AGENT_BROWSER_STREAM_QUALITY` | `60` | 控制带宽；默认是 80 |
| `AGENT_BROWSER_STREAM_MAX_WIDTH` | `1280` | 只限制编码后的帧宽，不改页面尺寸，所以坐标不受影响 |

- **为什么要固定 socket 目录**：agent-browser 默认的目录取决于 `XDG_RUNTIME_DIR`（优先级：`AGENT_BROWSER_SOCKET_DIR` > `XDG_RUNTIME_DIR/agent-browser` > `~/.agent-browser`）。falcon 作为 systemd / launchd 服务启动时，看到的环境和终端里的登录环境不一样，猜不准目录。固定下来，发现才有确定的结果。代价是：在 falcon 终端外执行 `agent-browser session list`，看不到这些会话。
- **Unix socket 路径有长度上限**（macOS 是 104 字节）。默认的 `~/.falcon/agent-browser/run/falcon-<uuid>.sock` 不会超。数据目录太长时不注入这一组变量，功能关闭，并在日志里记一条。
- **用户自己设过的变量不覆盖**：登录环境里已经有同名变量时，以用户的为准，相应的能力关闭（见决定五最后一条）。
- **会话重开（ADR 0024）沿用同一个 id**，浏览器会话名也就不变，agent 的浏览器状态可以接上。

## 决定四：被动发现浏览器；服务端只开一条上游连接，扇出给所有窗口

**发现**：监听 `run/` 目录（notify，另每 2 秒扫一遍兜底）。看到 `falcon-<id>.stream`（内容是端口号），且对应的 `.pid` 进程还活着、`.version` 不低于最低版本，就认定这个会话有浏览器。**不调用 CLI**：`stream status` 之类的命令可能顺手拉起守护进程。这套文件布局是 agent-browser 的内部实现，所以解析只放在 `agent_browser/discovery.rs` 一处，并用固定的样例文件做测试；布局变了，最多是发现不了，不影响安全。

**上游连接**：发现浏览器后，服务端开**一条**上游连接 `ws://127.0.0.1:<端口>/?pacing=ack&maxFps=10`，浏览器在多久就保持多久。`command` / `result` 只从 stream 里来，这条连接要一直开着才记得到。所有 falcon 窗口共用它，服务端负责分发。

**帧**：

- 服务端把 base64 解成字节，用二进制 WS 消息发给客户端（头部带 seq 和尺寸信息，后面是 JPEG），比原样转发省约四分之一。
- 每个窗口只保留最新一帧，旧帧直接被覆盖。只要有一个**看得见的**窗口回了 ack，服务端就向上游 ack。"看得见"沿用 ADR 0022 的定义，由客户端经 `visibility` 上报。
- 没有看得见的窗口时，服务端不再 ack，上游也就不再发帧；只在每次 `result` 之后放行一帧，留作记录的缩略图。
- agent-browser 文档要求经代理时转发渲染端的 ack。这里虽然多了一跳，但服务端只在下游回 ack 之后才向上游 ack，等价于原样转发：SSH 链路上同时最多只有一帧在路上，不会拖慢同一条连接上的终端回显。

**在场信息**：会话摘要加一个字段 `browser: Option<BrowserBrief { url, title, control }>`，经现有的会话列表推送发给客户端，终端标题栏的「浏览器」标记和自动弹出窗口都靠它。

## 决定五：接管就是"人和 agent 之间的一把锁"，用 action policy 实现

翻了 agent-browser 的源码：守护进程**每执行一条命令前都会重新读一遍 policy 文件**（`native/actions.rs` 中命令入口处的 `policy.reload()`）；而 stream 连接注入的输入直接走 CDP，**不经过 policy**。所以只要改写这个文件，就能拦住 agent，同时不影响人的操作，也不挑 agent：

- 平时：`{"default":"allow"}`
- 接管时：`{"default":"deny","allow":["snapshot","get","read","wait"]}`。agent 还能看页面、等待（比如用 `wait --url` 等用户登录完），但不能点击、输入、跳转、执行脚本，也不能关浏览器。

规则：

- **原子写**（临时文件 + rename）。即使读到半个文件也是安全的：agent-browser 读不出合法 policy 时，除 `close` 外一律拒绝。
- **会话启动前就要写好平时用的文件**：环境变量指向一个不存在的文件，会让 agent 的所有浏览器命令都失败。
- 服务端启动时把所有 policy 重置成 allow，重启后不会留下卡住的锁。会话删除时一并删掉这个文件。
- **锁只区分人和 agent，人和人之间不分**：任何客户端的任何窗口都能接管、操作、交还。锁的状态存在服务端，按会话记，并推给所有窗口。没在接管状态时，窗口发来的输入服务端直接丢弃。
- **何时交还**：用户点「交还」；这个会话的浏览器窗口全部关闭或断线满 30 秒；服务端重启；浏览器关了。**空闲不自动交还**：用户可能正停下来看页面，这时交还，agent 会接着操作。所以顶条上显示已接管多久，提醒用户。
- **agent 那边看到什么**：被拦时，agent 收到的是 `Action 'click' denied by policy: Action 'click' is not in the allow list`，并不知道原因。第三阶段给 falcon 启动的 Claude Code（ADR 0013 的启动脚本）加一个 PreToolUse 钩子，在用户接管期间拦下 `agent-browser` 命令，并告诉它"用户正在 falcon 里操作这个浏览器，请等用户交还"。Codex 有没有同类机制，待查。
- **锁失效的情况**：如果用户自己设了 `AGENT_BROWSER_ACTION_POLICY`（决定三不会覆盖它），或者 agent 用了 `--action-policy` 参数（命令行参数优先于环境变量），或者会话的 `<session>.settings` 里存了另一份 policy（守护进程重启后会优先用它），锁就不起作用。这时仍然可以接管和操作，但「接管」按钮旁会提示"agent 可能同时在操作"。至于 `~/.agent-browser/config.json` 里的 `actionPolicy`，环境变量优先，不受影响。

## 决定六：输入

- **坐标**：客户端发的是相对帧图像的归一化坐标（0..1）和当时那一帧的 seq。服务端用那一帧的尺寸信息（`deviceWidth` / `deviceHeight` / `pageScaleFactor` / `offsetTop`）换算成页面坐标，再发 `input_mouse`。agent-browser 的协议细节只在服务端这一处，客户端不用了解。
- **键盘**：GPUI 的按键转成 `keyDown` / `keyUp`（`key` / `code` / modifiers：1 Alt、2 Ctrl、4 Meta、8 Shift）。文字输入和输入法上屏的内容转成 `char`。窗口获得焦点后，除 falcon 的窗口管理快捷键外，按键全部送进页面，与终端窗口的规则一致。
- **粘贴**：Cmd/Ctrl+V 把本机剪贴板的内容按 `char` 逐字送进去（浏览器在 B 上，读不到本机剪贴板）。
- **滚轮**转成 `mouseWheel`。
- **不按窗口大小调整浏览器视口**：agent 的视口和 snapshot 里的元素引用要保持稳定。窗口和画面比例不一致时，空出的部分露出窗口底色（ADR 0011）。

## 决定七：记录存在拥有会话的那台节点上

服务端按会话保存：`command`（action 与参数）、`result`（成功与否、耗时）、`console` 中的 warn / error、`page_error`、`url`，以及接管 / 交还的标记（记下是哪台设备）。

- 存放位置：`<数据目录>/agent-browser/log/<会话 id>.jsonl`，权限 0600。最多 5000 条，超出后丢最早的。缩略图取每次 `result` 之后的那一帧，每个会话最多 300 张。会话删除时一并删除。
- agent-browser 的文档写明了 console 事件不能当审计日志用（客户端跟不上时会丢）。这里的上游连接在本机回环上，又是一直开着的，基本不会丢；但不保证一条不丢，所以也不把它当审计日志宣传。
- 客户端打开窗口时，先用 `GET /api/sessions/:id/browser/log?before=` 拉最近 200 条，之后跟着 WS 增量更新。
- `fill` / `type` 的文本可能是密码：界面上默认打码，点开才显示。磁盘上原样保存，敏感程度和终端回滚内容相同。

## 协议

新增 `falcon_proto::browser`。WebSocket 路径是 `/ws/browser/:sessionId`，鉴权和终端 WS 相同。

- 服务端 → 客户端：`Hello { control, status, url, tabs }`、二进制帧、`Event(BrowserEvent)`、`Control { holder: Agent | Human, since }`、`Status`、`Gone`。
- 客户端 → 服务端：`Ack { seq }`、`Visibility { visible }`、`Takeover`、`Release`、`Pointer { kind, x, y, button, clicks, modifiers, seq }`、`Wheel { x, y, dx, dy, seq }`、`Key { kind, key, code, modifiers }`、`Text { text }`。
- 双方遇到不认识的消息一律忽略（`#[serde(other)]`），不升协议版本。

工作区新增一种窗口 `PaneItem::Browser { key, session_id }`，它的落点和持久化规则与其他窗口相同（ADR 0012）。自动弹出时，放在终端所在列的右边。

## 分阶段

1. **观看与记录**：环境变量注入、发现、上游连接、WS、浏览器窗口、标题栏标记、自动弹出、记录。
2. **接管**：policy 锁、输入、交还规则。
3. **补充**：Claude Code 钩子；Android 客户端；仿照 px0（ADR 0017）钉住 agent-browser 版本，由后端推送二进制，实现免安装；agent 主动请人接管（比如遇到验证码时，在标题栏亮起「Agent 在等你」）。

## 代价与限制

- 只支持 agent-browser。agent 自己用 `--session` 换了会话名，falcon 就看不到这个浏览器；用 `--action-policy` 时锁不住（界面会提示）。
- 浏览器存在期间，始终开着一条 stream 连接。没人看时不 ack，上游不出帧，但 Chrome 那边的 screencast 仍然开着，会占多少 CPU 待测。
- 依赖 agent-browser 的内部文件布局（`.stream` / `.pid` / `.version`）。
- 没有鼠标指针形状，不能把页面里的内容复制回本机剪贴板，也弹不出文件选择框（接管期间上传不了文件，交给 agent 的 `upload` 命令）。
- 不支持 0027 之前的旧 SSH 会话。

## 实施前要核实的

- 在一台真实的 Linux 远端上，经网关测帧延迟、帧率，以及打开画面时终端回显是否变慢。
- 先在 `.settings` 里存一份 policy、再改环境变量后，守护进程实际用的是哪一份。
- 没有消费端时 screencast 占多少 CPU；有必要的话，无人看时先断开上游连接，只在有命令执行时再连上。
- `--with-deps` 在 Debian / Ubuntu / RHEL 上是否真能装齐依赖。
- `.version` 文件的格式，以及最低版本 v0.43 是否准确。
