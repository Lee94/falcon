# AI 助手：一段对话 = 一个终端 + 一段 AI 上下文

> 状态：**已实施**（2026-10-15，同日从「弹窗 + Shell / Agent 两页」改成现在的 Warp 式面板，见 ADR 0021 后记）· 决定见 [ADR 0021](../adr/0021-ai-assistant.md)，本文给做法 · 范围：侧栏「设置」旁边一个入口，点开是浮在主区左下角的面板。面板里一段**对话**就是某台机器（本机或一台 SSH Host）上的一个终端加一段 AI 上下文：你在终端里敲命令，跑完的命令记成时间线上的命令块；⌘⇧I 切到 Agent 输入，把任务交给 AI，AI 的命令打进同一个终端。AI 有两种来源：falcon 内置的 agent（直连 Claude API，任一台机器），以及经 ACP（Agent Client Protocol）接入的本地 agent（Claude Code / Codex / Gemini CLI 等，只在本机）· **没验证过的**：真的 Claude API（只对模拟的 SSE 服务端跑过，见第 11 节）、真的 Claude Code / Codex / Gemini ACP 适配器（只对按协议写的替身跑过）、SSH Host 上的对话（终端、命令块、文件工具）、浏览器版里的交互、fish 上的命令块
>
> 术语沿用 [CONTEXT.md](../../CONTEXT.md)。「本机」= falcon 后端所在的机器（与「中转」同一个口径）；「机器」= 本机或一台已保存的 SSH Host。

---

## 0. 一句话

**助手是一个浮动面板，面板里是一段段对话；一段对话 = 一台机器上的一个持久会话 + 一段 AI 上下文（像 Warp 的一个标签）。时间线从上往下是：你敲完的命令块、你对 agent 说的话、AI 的话与工具卡片，最底下是这个终端的实时区域——只露出当前提示符到光标那几行。falcon 给对话终端注入了 shell integration，从输出流上的 OSC 133 / 633;E 知道一条命令的命令行、输出、退出码，服务端据此记命令块。agent 在服务端跑（收起面板不打断、浏览器版同样能用、密钥不下发），内置 agent 与 ACP agent 在服务端被抹平成同一种条目流，界面只认这一种。内置 agent 的命令打进这段对话的终端，也看得见你上一句话之后在终端里敲过什么。**

代码：

| 层 | 位置 |
|---|---|
| 协议 | `falcon-proto/src/assistant.rs`（条目、WS 消息、对话摘要、设置）；会话 WS 多了 `shell` 消息（`ws.rs`） |
| 纯逻辑 | `falcon-core/src/assistant.rs`（机器清单、`用户@机器:~/目录`、终端输出转纯文本 / 带样式的行、行级 diff、命令行拆分）、`falcon-core/src/shell_marks.rs`（OSC 133 / 633;E / 7 扫描，服务端与客户端共用） |
| 服务端 · 终端 | `sessions/machine.rs`（按机器挂的会话）、`sessions/shell_integration.rs`（注入脚本）、`sessions/shell_marks.rs`（状态机、命令块、PROMPT_SP）、`sessions/manager/machine_run.rs`（往对话的终端里跑命令） |
| 服务端 · 对话 | `assistant/hub.rs`（对话、观看者、持久化、审批、两种 agent 的一轮、命令块入时间线）、`assistant/claude.rs`（Messages API 流式客户端）、`assistant/tools.rs`（四个工具）、`assistant/acp.rs`（ACP 客户端）、`assistant/settings.rs`；路由 `api/assistant.rs` |
| 客户端 | `falcon-client/src/api/assistant.rs`、`ws/assistant.rs`；`falcon-ui/src/assistant/`（`mod.rs` 面板与对话列表、`pane.rs` 一段对话、`blocks.rs` 各种条目的画法）、`terminal/view.rs` 的实时模式（`TerminalView::new_live`）、`dialogs/settings/assistant.rs` |

---

## 1. 范围

做了的：

- 侧栏底部「设置」旁的「助手」按钮、⌘J（非 mac `Ctrl+Shift+J`，浏览器另有 `Alt+J`）、命令面板、原生菜单「视图」里各一个入口，都是开 / 收面板；
- 面板浮在主区左下角、贴着侧栏；⤢ 铺满主区，— 收起。不是模态的：开着时照样能点画布里的终端；
- 「＋」新对话 = 在某台机器上开一个新终端 + 一段新的 AI 上下文（只有本机时一点就开，有 SSH Host 时先挑机器）；标题 ▾ 列出全部对话（跨机器，带机器名），切回旧对话它的终端还活着；删对话 = 终止它的终端；
- 一段对话两种输入，⌘⇧I 切换（浏览器另有 `Alt+I`）：Shell（键盘在终端里）/ Agent（底部输入框）；
- 收起面板不结束任何东西：终端只是 Detach，在跑的 agent 继续跑，再打开原样接着（视图常驻，WS 不断）；
- 内置 agent：Claude Messages API，流式，四个工具，命令与写文件默认要批准；
- ACP：预设 Claude Code / Codex / Gemini CLI，可自定义；只在本机起；一段对话随时能换 agent；
- 设置里的「AI 助手」页：API key（加密存服务端、只显示掩码）、Base URL、模型、思考强度、「不再询问」、ACP agent 列表。

不做的：

- 把 AI 的话画进终端网格里（ADR 0021 决定二）；
- 别家模型 API 的直连：用别家模型就走 ACP；
- ACP registry 的在线安装、MCP server 的下发（`mcpServers` 恒为空）、ACP agent 的终端能力（`terminal/*`，没有 agent 用它）；
- Windows 宿主机上的 shell integration、命令块与文件工具（写得出来，但没有真机能验）；
- 对话的上下文压缩：长到上限前提示另开一段；
- 面板收着时「有卡片在等批准」的提醒（侧栏按钮上的点）；
- 命令块的重新运行、复制、分享（Warp 有）。

---

## 2. 界面

### 2.1 面板（`assistant/mod.rs`）

画在工作区窗口主区（画布那一格）里，绝对定位在左下角，离边 8px；平常 560 × 680（放不下就缩到主区），⤢ 铺满主区。面板底下多半就是终端，两者同一个底色，岛的「只靠底色分块、不画边框」在这里分不出轮廓（试过，看不出边），所以它像弹出层一样带一圈细边（`input` token）与阴影。`occlude`：点面板不会点穿到画布。

视图（`AssistantPanel`）挂在 `Workspace::assistant` 上常驻：打开过的对话各有一个 `ThreadPane`，切走不卸载，WS 与终端一直连着。第一次打开时拉对话列表与设置；一段对话都没有就在本机开一段（打开助手默认就是本机的一个终端）；全删光了显示空状态与各台机器的按钮。

头部：左边「机器图标 + 对话标题 ▾」（标题是第一条命令或第一句话的前 40 字，还没有时是「新对话」；菜单里每段带机器名，在跑的带 ●，末尾「删除这段对话」）；右边 ＋、⚙（设置的 AI 助手页）、⤢、—。

收起时焦点还给工作区里活动的那扇终端。⌘J 走工作区窗口上那一份（面板不是对话框，动作冒泡得到）。

### 2.2 一段对话（`assistant/pane.rs`）

一条时间线 + 底下一条小底栏。时间线要像一个终端：最底下那一行就是「命令行」，Shell 与 Agent 都在这一行上输入，**不另起一个输入框**——你对 agent 说的话与你敲的命令在同一个位置、同一条时间线上。

时间线（`blocks.rs`），从上往下：

| 块 | 画法 |
|---|---|
| 命令块 | 画得和终端里一样，不加底色：`目录 命令行`（目录 `~` 缩写），失败的右边红字「退出码 N」；输出按终端习惯处理 `\r`、退格与 SGR 颜色（`styled_output`，16 / 256 / 真彩、粗斜下划线、反显），末尾空行不画；超过 20 行折起来只留最后 20 行，「…前面还有 N 行，点开看全部」。你敲的与内置 agent 打进去的（`by_agent`）同一种画法 |
| 你对 agent 说的话 | 也画成命令行上的一行：`目录 ✦ 这句话`（目录是说的时候终端在哪，服务端记在条目上） |
| AI 的一轮 | 左边一道竖线，里面一列：思考、回复、每一步。打头那一项前面一个 ✦。agent 跑完的命令块把一轮切成两段——那是终端里的东西，不归 AI 那一栏（与参考图一样：卡片 → 终端输出 → 下一段思考） |
| 思考 | **直接画出来**（淡色），超过 600 字只露开头、「展开」看全部。用户要看得见 AI 为什么这么做 |
| AI 的话 | gpui-component 的 markdown `TextView`，可选中，链接用系统浏览器开 |
| 一步 | 上面一行「这一步要做什么」（内置 agent 的 `run_command` 必带 `description`，6.3），下面是卡片 |
| 工具卡片 | 头一行是状态：已执行 ✓ / 已读取 / 已修改 / 执行中 / 没有完成；等批准时换成一句问话（「是否同意执行这条命令？」）+ 右边的按钮。下面是 `$ 命令`（等宽、强调色）或路径，改文件的画行级 diff（改动前后各留 3 行，最多 60 行）。内置 agent 的命令输出不在卡片上，在卡片后面的命令块里；ACP agent 的命令在它自己的进程里跑，实时输出画在卡片上 |
| 审批 | 按钮：执行 ⌘↵（内置 agent 的命令卡片上「允许」叫「执行」；ACP 的选项名原样用）、本对话内总是允许、修改、拒绝。⌘↵ / ⌥⌘Y / ⌥⌘Z（非 mac `Ctrl+↵` / `Ctrl+Alt+Y` / `Ctrl+Alt+Z`）答最上面那张 |
| 修改 | 只给内置 agent 的单行命令（多行命令的标题只有第一行，改不全）：卡片上的命令变成输入框，按钮只剩「执行 ⌘↵ / 取消」，Enter 或 ⌘↵ 按改过的跑。服务端按改过的命令执行、更新卡片标题，并在回给模型的结果前面说一句「用户把命令改成了 X」——不然模型以为跑的是自己那条 |
| 计划 | ACP 的 `plan`：一列带完成标记的条目 |
| 系统的话 | 一行小字：出错（红）、被拒 / 被截断（黄）、已停止（灰） |
| 在跑 | 一轮没结束时末尾一行：竖线 + 转圈 + 「AI 正在思考… / 正在执行命令… / 等你批准」+ 红字「终止」 |

**实时区域**（命令行那一行在 Shell 输入时就是它）：这段对话的 `TerminalView`（实时模式），外面一层只露出「当前提示符到光标」那几行：

- `TerminalView` 在输出流上自己认 OSC 133（falcon-core 的 `MarkScanner`，按标记切开喂给 alacritty），`A` 到时记下光标的绝对行，`C` 之后算「在跑」；`live_slice()` 给出要露的屏幕行（首行 = 提示符的绝对行 − 当前历史行数，夹在 0 到光标之间）；
- 终端本身按面板的高度排版（PTY 的格子数跟着它），外层 `overflow_hidden`、高 = 行数 × 行高，里层上移 `留白 + 首行 × 行高`（网格画在元素里 6px 留白之下，`element.rs` 的 `PADDING_Y`）；命令块、提示符行的左边对齐同一道留白（`PADDING_X`），字体字号与终端一样；
- 改尺寸时 alacritty 重排折行，行会在历史与屏幕之间挪，绝对行就对不上：改之前记下提示符在光标上面几行，改完按光标还原；
- 备用屏（vim、claude、top 这类全屏程序）或输出多过一屏：整块露出来；
- 实时模式不画滚动条，滚轮交给外面的时间线（备用屏里照旧交给程序）；
- Agent 输入时 shell 闲着就不露（那一行换成 Agent 的输入），命令在跑时照样露着——内置 agent 的命令就在这里看得见；
- **切 Shell / Agent 不改终端的高度**：PTY 一 resize，正在跑的全屏程序就整屏重画；
- 终端结束了（`exit`、被终止）不自动另开（rc 文件有问题时 shell 一起来就退，自动另开会停不下来），显示「终端已结束」+「新终端」；命令块与 AI 上下文都还在。

**Agent 输入那一行**：`✦ 目录 ▌`，字体字号同终端，输入框没有边框、没有内边距（xsmall），自动长高 1–8 行；占位字写着「让 AI 在这台机器上做点什么 · Enter 发送 · ⌘⇧I 回到 Shell」。Enter 发送、⇧Enter 换行、⌘↵ 批准最上面那张卡片（输入框把 ⌘↵ 变成 `Enter { secondary }`，这一行在捕获阶段拦下来转成批准）。一轮在跑时 Enter 不发（末尾那一行有「终止」）。内置 agent 没有 API key 时这一行上面一条提示与「去设置」。

重画：一轮在跑时会动的只有转圈与输入框的光标。两样都是**单独的小视图**（`blocks::Spinner`、`blocks::InputView`）：持续动画、光标闪烁重画的是读过那份状态的视图，画在对话里的话每一帧都是整条时间线（markdown、命令块、卡片）重排；实时终端也只在「要露的行」变了时才通知对话（`TerminalViewEvent::LiveChanged`，光标闪烁不算）。实测：只把转圈拆出去时，等批准的三秒里对话视图还重画了三四十次（输入框与终端的光标）；三样都处理完是 0。

人停在底部时跟着往下滚（时间线新条目、实时区域长了都算）；往上翻着看的时候不拽回去。Shell 输入时点时间线的空白处，键盘回到终端。

底栏：`[Shell|Agent]` 小分段、`用户@机器:~/目录`（来自 shell integration，4.2）；Shell 时右边淡字「⌘⇧I 切换到 Agent，交给 AI 完成任务」，Agent 时右边是这段对话交给哪个 agent（内置；本机再加上设置里开着的 ACP agent；在跑时不能换）。

### 2.3 设置

「设置 → AI 助手」：
- 内置 agent：API key（只写；显示「已设置 · sk-ant-…1a2b」，留空保存 = 不改，「清除」才删）、模型（四个常用的一点即填，也能手填）、Base URL（留空 = 官方）、保存；
- 行为：思考强度（低 / 中 / 高 / 很高 / 最高）、命令与改文件不再询问；
- ACP agent：每个一行（名称、一行命令、开关；自定义的能删），「保存命令」，下面一行加自定义的（名称 + 命令）。命令行按 shell 的习惯拆（单双引号、反斜杠），不经 shell、不展开变量。

存完把新设置推给面板，各段对话的「缺 key」提示与可选的 agent 跟着变。

---

## 3. 对话的终端：按机器挂的会话

会话一直属于某个项目。对话要的是「某台机器上的一个终端」，不属于任何项目，也不该出现在侧栏、会话总览、命令面板里。

**沿用 `sessions` 表与 SessionManager 的全部机制，只把 owner 换成合成 id**（`sessions/machine.rs`）：

- 本机 `machine:local`，SSH Host `machine:host:<hostId>`。故意不用 `host_as_project` 给的 `host:<id>`：那是中转与浏览目录共用的主机链路的 key，这里走的是项目链路那一套（断线重连并接回会话），两套语义不同，不共用一条连接；
- 会话路径上要 owner 的地方（创建、接回、终止、前台探测、断线重连、贴图、部署授权 WS）一律走 `owner_row`：真项目查库，`machine:local` 合成一条本机 ProjectRow（工作目录 = 家目录，shell 留空让下游挑默认），`machine:host:<id>` 是 `host_as_project` 换个 id；
- **不加列**：`machine:` 前缀本身就是标记，`GET /api/sessions` 据此滤掉；
- 一台机器上可以有多个（一段对话一个），对话记着自己的 `session_id`（`assistant_threads.session_id`）。`POST /api/assistant/machines/:machine/threads` 新建对话时连终端一起建；`POST /api/assistant/threads/:id/shell` 取这段对话的终端：活着（active / unverified）就交出去，已丢失的删掉、在同一台机器上另开一个、记回对话。按对话排队，连点两下不会建出两个；
- 删对话：在跑的一轮先停，它的终端终止；删 SSH Host：那台机器上的对话连同终端一起删，链路拆掉；
- 持久性与普通会话一样：本机的守护进程、SSH 上的授权与部署照旧（没授权的 SSH Host 上是非持久会话）。

---

## 4. Shell integration、命令块与「往 shell 里跑命令」

### 4.1 注入（`sessions/shell_integration.rs`）

- **只给对话的终端注入**，项目里的普通会话不动；
- 照 agent 启动脚本（ADR 0013）：脚本写在宿主机 `<falcon 根>/shell-integration/`，每次附着都重写；会话的 argv 是启动器 `falcon-shell.sh`，它 `exec` 用户的 shell（路径生成时写死，不读 `$SHELL`）；
- 与普通会话同一个口径：守护进程起的是**交互式非登录** shell，这里也只模拟交互式非登录：

| shell | 注入 | 钩子 |
|---|---|---|
| zsh | `ZDOTDIR` 指到我们的目录；我们的 `.zshenv` / `.zshrc` 先替用户读他自己的（他的 `.zshenv` 改了 `ZDOTDIR` 也跟着），读完把 `ZDOTDIR` 还给他（原来没设就 unset） | `precmd`（排在**最前**，排在主题后面 `$?` 早被改了）打 `D;$?`、`A`、OSC 7；`preexec`（排在最后）打 `633;E;<命令行>` 与 `C`，命令行取 `$1` |
| bash | `bash --init-file <脚本>`，脚本先读 `~/.bashrc` | `PROMPT_COMMAND` 最前面打 `D;$?`、`A`、OSC 7 并 `return` 原值；≥ 4.4 用 `PS0` 打 `E` 与 `C`；更老的（macOS 的 3.2）用 DEBUG trap，而且只在用户自己没设 DEBUG trap 时才设（bash-preexec 一类占着它）。命令行取 `HISTTIMEFORMAT= history 1` 去掉序号（`fc -ln -1` 在 3.2 上报 out of range） |
| fish | `fish --init-command 'source <脚本>'` | `fish_preexec`（`$argv[1]` 是命令行）/ `fish_postexec` / `fish_prompt` 事件 |
| 其它、Windows | 不注入，原样 exec | 没有命令块；`run_command` 退成独立进程（4.5） |

- `633;E` 是 VS Code 的写法：命令行里的 `\` 写成 `\\`、`;` 与控制字符写成 `\xNN`，后面可以跟 `;nonce`（我们不打）；
- **每个提示符前都打 `D`**（空回车也打）：服务端只在「运行中」或正在替 agent 收输出时认它，多余的无害；省掉了「上一条到底跑没跑」这个在 bash 里要靠子 shell 外的标志才记得住的状态；
- **不打 `B`、不改 PS1**：p10k、starship 每个 precmd 都重设提示符，加进去的会被冲掉；
- 测试在真 PTY 上跑 zsh 与 bash（含 macOS 自带的 bash 3.2），断言 C…D 之间正好是命令输出、`false` 的 D 带 1、E 带回原样的命令行；bash 5 的 PS0 路径在 alpine 容器里手验过。

### 4.2 扫描、状态机与命令块（`sessions/shell_marks.rs`）

- 服务端在 `on_backend_data`（持久与非持久会话都经过它）上旁观输出，不改转发出去的字节（alacritty 不认这些 OSC；持久会话的 VT 进程还要靠 133 清程序状态）。守护进程的回放快照不经这里，不会把旧标记再认一遍；
- `MarkScanner`（falcon-core）把输出切成文字与标记，跨块拼序列（不全的尾巴留到下一块，上限 8KB）；OSC 7 的路径百分号解码；
- `ShellTracker`：`unknown → at_prompt ⇄ running`，记当前目录；
  - 替 agent 收输出：上膛之后，`C` 之后的文字算输出、`D` 带回退出码；shell 没打 `C` 时从上膛起收（会带上回显的那一行）——宁可多一行，不能一直等不到；
  - 不是 agent 的命令（你自己敲的）：`C` 开一个命令块（命令行取刚到的 `E`，目录取当时的 OSC 7），`D` 收尾（没等到 `D` 就又到了 `A` 也收尾，退出码空着）。输出存原始字节（带 ANSI），头 64KB + 尾 192KB，中间掐掉标一行「省略 N 字节」；
  - **PROMPT_SP**：zsh 在 precmd **之前**打 PROMPT_EOL_MARK（缺省反显的 `%`）+ 补满一行的空格 + `\r` + 与标记等宽的空格 + `\r`，靠终端的自动折行把提示符挤到下一行。它落在 `D` 之前，算进了输出；命令块不知道终端多宽、不会折行，原样画的话输出的头几个字被盖掉、每块末尾多一行空白。`strip_prompt_sp` 认出这个形状（fish 的 `⏎` 多一个 `ESC[K`）整段去掉，命令块与回给 agent 的输出都过它；
- 状态或目录变了就推会话 WS 的 `shell { cwd, busy, user, home }`，连上时也推一次。`user` / `home` 是附着时服务端填的（本机：`$USER` 与家目录；SSH：主机的用户名与探测到的 home），不来自输出流；
- 命令块交给 hub（`SessionManager::set_block_listener`）：按 `session_id` 找到对话，记成 `Entry::Command`；对话还没名字就用命令行起名，并推 `thread` 消息让面板的列表改名。

### 4.3 agent 看得见你在终端里干了什么

每句话发给 agent 之前，取「上一句话之后」时间线上**你自己敲的**命令块（agent 跑的 `by_agent` 不算：它知道自己跑了什么，结果也已经在它的历史里；最多最近 5 条，每条输出转纯文本后头尾各留一半、最多 3000 字），拼成一段 `<terminal_activity>` 随这句话带上：内置 agent 并进这条 user 消息，ACP agent 并进 prompt 的文字。原来的「附带终端最近输出」勾选框（取屏幕最后 80 行）去掉了：命令块有命令行、目录、退出码，比一屏文字准，也不用人记得去勾。

### 4.4 往对话的终端里跑一条命令（`sessions/manager/machine_run.rs`）

1. 同一个终端上一次只跑一条（按会话排队）；
2. 取这段对话的终端（3 节），`ensure_attached(force)`；
3. 等 shell 停在提示符上：标记说「在提示符上」就走；还没见过标记（刚接回）就问守护进程前台是不是 shell；前台被别的命令 / 全屏程序占着就等，卡片上提示一句，60 秒还占着就报错让模型换办法；
4. 上膛，写 `Ctrl-E Ctrl-U` 清掉输入行上可能有的半截输入，再写命令 + 回车。多行命令包在括号粘贴里（zsh / bash ≥ 5.1 / fish 认），只回车一次；
5. `D` 带回退出码。卡片上只有命令与状态；跑完的输出记成卡片后面的一个命令块（`by_agent`，原始字节带颜色，与你自己敲的一样画），shell 那边不再另记一个「你敲的」。超时的也记：到那一刻为止的输出，退出码空着。**超时不杀**（命令跑在你的终端里，你看得见、停得了），把已有输出交回模型；**停止 = 往 shell 送 Ctrl-C**。

### 4.5 没有集成的 shell

`integrated` 在附着时按同一个口径算（认得出的 shell、不是 Windows）。为 false 时没有命令块，命令改成独立进程：本机 `/bin/sh -c`（登录环境、当前目录跟着 shell 报的走），SSH 走 exec 通道；拿不到实时输出，跑完一次给，卡片上注明。

---

## 5. Agent 的整体架构

```
 客户端（面板）                     服务端（引擎线程）                          机器
┌───────────────┐  WS /ws/assistant/threads/:id ┌──────────────────────┐
│ 一段对话        │ ◀─ snapshot / entry / append ──│ AssistantHub           │
│  时间线         │    status / thread             │  └ Thread（对话）      │
│  审批按钮       │ ── prompt / cancel / ─────────▶│    ├ 终端 ◀─ 命令块 ───┼── shell integration
│  实时区域 ◀─────┼─ /ws/sessions/:id ─────────────┼────┤                  │
└───────────────┘                                 │    ├ 内置：Claude API ──┼─ run_command ─▶ 这段对话的终端（4.4）
                                                  │    │          文件工具 ─┼─ 本机 fs / SSH exec
                                                  │    └ ACP：子进程 stdio ─┼─▶ 本机上的 agent 进程
                                                  └──────────────────────┘
```

- **跑在服务端的引擎线程上**（`Rc` / `RefCell`，与 SessionManager 同一个执行模型）：命令要进对话的终端，那些状态本来就在这条线程上。收起面板、断网、换设备都不打断在跑的一轮；浏览器版起不了子进程，也不该拿到 API key；
- **一种条目流**：条目照 ACP `session/update` 的形状定义在 falcon-proto（`User / Command / Agent / Thought / ToolCall / Plan / Notice`，`ToolKind` / `ToolStatus` / `PermissionKind` 的取值与 ACP 一致；`Command` 是 falcon 自己的，带 `byAgent`；`User` 带说话时的 `cwd`；`ToolCall` 带 `description`）。客户端答审批的 `permission` 可以带 `command`（「修改」过的命令，只有内置 agent 的 run_command 认）。新加的字段都是缺省可省的，旧数据、旧客户端照样读。没直接用 ACP 的 schema crate：一周三个大版本、一堆 unstable 变体，客户端协议要稳、要编进 wasm；映射只在服务端。每个带标签的枚举都有 `Unknown` 兜底，服务端比客户端新时安静忽略。Entry 的标签字段叫 `type`——叫 `kind` 会被 ToolCall 自己的 `kind` 摊平后盖掉；
- **快照 + 增量**：WS 连上先收 `snapshot`（对话全集），再收 `entry`（新条目或同 id 整条替换）、`append`（同 id 的文字追加：Agent / Thought 是正文，ToolCall 是终端输出）、`status`、`thread`（摘要变了：起了名、换了 agent、换了终端）。断线重连后新的快照就是全集。客户端断线期间发的消息**排队**（不像终端输入那样丢：这里发的是想清楚了的一句话、一次批准）；
- **对话**（`assistant_threads` 表）：机器、agent（可换）、标题（第一条命令或第一句话的前 40 字）、终端会话 id、界面条目、agent 自己的状态（内置：API 的原始 `messages`；ACP：`acpSessionId`）。落库时机：每轮开始与结束、每次等批准、每个命令块、流式文字每 2 秒至多一次。读回来时，上次进程留下的「等批准」与「运行中」的卡片标成失败——那一轮已经没了；
- **换 agent**（`PATCH /api/assistant/threads/:id`）：之后的话交给新的那个，终端与时间线不动；ACP 的进程与会话丢掉，内置 agent 的 `messages` 留着（换回来接着用）。

---

## 6. 内置 agent

### 6.1 请求（`assistant/claude.rs`）

- `POST {base}/v1/messages`，`stream: true`；Rust 没有官方 SDK，走 reqwest（TLS 与 px0 下载同一套），**不设整体超时**，逐块计空闲超时 5 分钟；
- `thinking: {type: "adaptive", display: "summarized"}`（Opus 5.5 不能关思考，summarized 才有看得见的摘要），`output_config.effort` 显式给（Opus 5.5 的缺省是 medium，falcon 缺省 high），`max_tokens: 64000`；
- **只在官方地址上**带 `eager_input_streaming`（工具参数边生成边到）与 `fallbacks: "default"`（beta `server-side-fallback-2026-07-01`，被安全分类器拒了自动换模型）：网关可能不认这两个字段；
- 缓存：system 块上一个断点（渲染顺序 tools → system → messages，一个就盖住工具与系统提示词），顶层再一个自动断点盖住历史。system 里只放不变的（机器事实、做事的规矩）；当前目录与 `<terminal_activity>` 每轮随用户消息带；
- 限流 / 过载 / 5xx 在还没开始收的时候退避重试三次；API 的错误原话（`HTTP 401：invalid x-api-key`）写成对话里的一条红字。

### 6.2 流与历史

- `Accumulator` 把内容块**原样**攒成 JSON（thinking 带签名；不认识的块照存），同时吐出界面要的增量；
- 工具参数严格解析：坏的不执行，原文以 `{"INVALID_JSON": …}` 回给模型；
- 中途换了模型（`fallback` 块）：回传时分界点之前只留 text 块，那些 tool_use 不执行、卡片标失败；
- `stop_reason`：`tool_use` 就跑工具；`refusal` 不进历史、工具一个不跑；`max_tokens` 不执行可能被截断的工具；一句话里最多来回 60 轮；
- 历史只追加：被拒绝 / 被停止那一轮留下的 tool_use，在下一句话之前补上 `is_error` 的「没有执行」结果；下一句话并进同一条 user 消息（角色要交替）。

### 6.3 工具（`assistant/tools.rs`）

| 工具 | 做法 | 审批 |
|---|---|---|
| `run_command {command, description, timeout_secs?}` | 打进这段对话的终端（4.4），超时缺省 120s、最多 600s；回给模型的是 `exit code: N` + 纯文本输出（头尾各留一半，最多 3 万字）。`description` 是一句给人看的「这一步要做什么」（用用户的语言），画在卡片上面；schema 里必填，解析时宽松（没给不算错）。你在卡片上改过命令时按改过的跑，结果前面加一句 `(The user edited the command before approving it. What actually ran: …)` | 要 |
| `read_file {path, offset?, limit?}` | 带行号，缺省 2000 行；大于 5MB、二进制、目录报错 | 不要 |
| `write_file {path, content}` | 建父目录；**原地覆盖内容**（`cat 临时 > 目标`），不换 inode，执行位与属主留着；远端先落临时文件、核对字节数 | 要，卡片带 diff |
| `edit_file {path, old_string, new_string, replace_all?}` | `old_string` 唯一命中才换 | 要，卡片带 diff |

- 相对路径按终端的当前目录（OSC 7）解析，`~` 是家目录；
- 文件工具：本机直接读写，SSH POSIX 主机走 exec 通道，Windows 宿主机报错让模型改用 `run_command`；
- 审批：「允许」「本对话内总是允许」（按工具记）「拒绝」；拒绝 = 这一轮停下，等你下一句话；设置里的「不再询问」打开时都不问。

---

## 7. ACP agent（`assistant/acp.rs`）

- **只在本机**：Claude / Codex 的 ACP 适配器都在自己的进程里执行命令、改文件（读过它们的 JS：不调客户端的 `terminal/*` / `fs/*`），agent 跑在哪台机器上就只看得到哪台机器；SSH Host 上的对话只能选内置 agent。它的命令不进对话的终端（不会出现在实时区域），实时输出画在卡片上；
- **自己写 JSON-RPC**：stdio 上一行一条。`agent-client-protocol` 3.x 的处理器要求 `Send`，跟引擎线程合不来；schema crate 也没用，按 JSON 宽松地读用得上的字段（某个字段长得不对，不该让整条更新作废）。收发层 `AcpConn` 对任意读写流都成立，测试里用内存管道；
- 起进程：登录 shell 解析出的环境（npx / nvm 在 PATH 上）+ 配置里的额外环境变量，工作目录是这段对话终端的当前目录，`kill_on_drop`；一段对话一个进程，删对话、换 agent 或服务端退出时收掉（没有空闲回收）；
- `initialize`：协议 1；`fs.readTextFile/writeTextFile`（Gemini 用）、不声明 `terminal`、`auth.terminal`、`_meta.terminal_output` + `terminal_output_delta`（Claude / Codex 经这个扩展把命令输出按增量流给客户端：`terminal_info` / `terminal_output_delta {terminal_id, data}` / `terminal_exit {exit_code}`，卡片的实时输出靠它）；
- `session/new`（`cwd`、`mcpServers: []`）；对话里存过会话 id 时：agent 声明 `loadSession` 就 `session/load`（期间 agent 重放的历史丢掉，界面上本来就有），否则开新会话并插一条「上下文已重置」；
- `session/new` 报 auth_required（-32000）：把 `authMethods` 里终端类的拼成一条命令（agent 自己的命令 + 方法的 args），让人在终端里跑；
- `session/update` → 条目（`UpdateMapper`）：文字块流进同一条，遇到工具调用另起；`tool_call` / `tool_call_update` 改卡片（内容整份替换时终端已收的输出留着；标题去掉包着的反引号）；`plan` 整份替换；其它（`available_commands_update`、`usage_update`……）忽略；
- 反向请求：`session/request_permission` → 卡片上的按钮（选项原样用），停止时答 `cancelled`；「不再询问」打开时替你选 `allow_once`；`fs/read_text_file`（`line` / `limit`）与 `fs/write_text_file` 直接读写本机；其余回「没有这个方法」；
- 停止：发 `session/cancel`，等它回 `stopReason: cancelled`，10 秒不理人就不等了。

---

## 8. 协议

REST（`require_login` 之后）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/assistant/threads` | 全部对话（跨机器），新的在前 |
| POST | `/api/assistant/machines/:machine/threads` | 新对话 `{agent?, appearance?, background?, foreground?}`：连终端一起建（配色深浅同新建终端，写 COLORFGBG 等）；主机不存在 404 |
| POST | `/api/assistant/threads/:id/shell` | 这段对话的终端，丢了另开，回 `Session`；对话不存在 404 |
| PATCH | `/api/assistant/threads/:id` | `{agent}`：换 agent（在跑时不行） |
| DELETE | `/api/assistant/threads/:id` | 删对话（在跑的先停、ACP 进程收掉、终端终止） |
| GET / PUT | `/api/assistant/settings` | 设置；GET 里 API key 只给掩码，PUT 只改带了的字段 |

`:machine` 是 `local` 或 SSH Host 的 id。WS `/ws/assistant/threads/:id` 自己验身份（同 `/ws/sessions/:id`），应用层心跳 `{"type":"ping"}` 直接回 pong。对话的终端就是普通会话，连接走 `/ws/sessions/:id`。

---

## 9. 安全

- AI 能在你的机器上跑任意命令：默认每条命令、每次写文件都问；「不再询问」是显式的全局开关；
- API key 只在服务端：`SecretBox` 加密落库、不进日志、不下发；没填时退回服务端进程的 `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL`；
- **你在对话终端里敲的命令与输出会随下一句话发给模型**（4.3）。这是这个终端的用途（像 Warp），但别在对话的终端里 `cat` 密钥再去问 AI；项目里的普通终端不受影响；
- 公网发布 falcon 时（ADR 0014）助手等于一个远程执行入口，与终端本身同级，靠的仍是登录密码。

---

## 10. 自动化与截图

`FALCON_AUTOMATE` 的助手步骤：`assistant`（开 / 收）、`assistant-new:<机器>`（开新对话）、`assistant-expand`（⤢ 开合）、`assistant-agent:<agent>`（当前对话换 agent）、`assistant-prompt:<文字>`（切到 Agent 输入、填字、发送）；面板开着且在 Shell 输入时 `type:` 打进当前对话的终端。

---

## 11. 验证

自动化测试：
- 纯函数：机器清单与标签、路径截尾、`plain_output` / `styled_output`（颜色、改写、去掉末尾空行）、行级 diff、命令行拆分、扫描器（跨块切开的每一个位置都拼得回来，633;E 的转义）、状态机与命令块（头尾截断）、PROMPT_SP（zsh 两种情形、fish、对不上的原样）、工具参数校验、路径解析、Claude 请求体（官方 / 网关）、SSE 切行、内容块累积（thinking 签名、坏 JSON、fallback 分界）、悬空 tool_use 补结果、ACP 映射与认证提示；
- 真 PTY：注入后的 zsh / bash 打出的标记与命令行；你敲的命令变成命令块、对话起了名并推给在看的、下一句话带上了它；agent 的命令跑在对话的终端里、输出记成卡片后面唯一的一个 `by_agent` 命令块、卡片带着 `description`、下一句话不把它当成你敲的带回去；在卡片上改过的命令按改过的跑、卡片标题跟着变、模型被告知；
- 模拟 Messages API（本机 axum 按顺序回放 SSE）：读文件的往返、写文件要批准 → 拒绝停下 → 下一句话并进同一条 user 消息 → 总是允许后真的写了、401 变成红字；
- ACP：内存管道上的请求 / 应答 / 反向请求 / 断线；测试二进制扮演 agent（`FALCON_TEST_ACP_AGENT`）跑完一轮：消息、命令卡片、权限往返、终端输出增量与退出码、会话 id 落库；命令不存在时的报错。

手动（原生客户端截图，隔离的服务端 + 模拟 API）：面板的位置与轮廓、新对话、命令块（颜色、失败的退出码、没以换行结尾的输出、`top -l` 一类刷屏的输出、`less`）、实时区域只露提示符那一行、⌘⇧I、Agent 的一轮（审批 → 命令在同一个终端里跑 → 结果）、对话菜单、⤢、⌘J 收起。
