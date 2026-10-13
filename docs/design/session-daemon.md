# 自研会话守护进程 falcon-sessiond：详细设计

> 状态：**已实施**（2026-10-13）· 决定见 [ADR 0020](../adr/0020-session-daemon-libghostty.md)，本文给做法 · 范围：用 falcon 自己的会话守护进程替换宿主机上的 Zellij，覆盖本机 / SSH POSIX / 本机 Windows / SSH Windows 四种宿主机；**一个版本里整体切换，不兼容旧会话、Windows 不阻塞切换**（第 14 节）；客户端引擎不换 · Windows 部分只做过交叉编译与零警告检查，**没在真机上验证**（第 12、15 节）
>
> 术语沿用 [CONTEXT.md](../../CONTEXT.md)。「falcon 服务端」是跑 falcon-server 的机器，「宿主机」是会话 PTY 所在的机器（本机项目时二者相同）。本文的词：**守护进程**（`falcon-sessiond`，一个会话的全部宿主机侧进程的统称）、**holder**（持 PTY 的那个进程）、**VT 进程**（跑 libghostty-vt 的那个进程）。

---

## 0. 一句话

**每个会话在宿主机上有一对进程：holder 持有 PTY、转发字节、存程序状态，代码小且几乎不变；VT 进程吃一份输出副本、在 libghostty-vt 里维护终端状态、负责导出快照与回答终端查询，挂了只丢画面历史，会话不死。falcon-server 与 holder 之间是一套带偏移量的帧协议，本机直连 socket / 命名管道，SSH 经 `bridge` 子命令走 exec 通道的 stdio。**

代码：守护进程在 `native/sessiond/`（独立工作区），协议在 `falcon-proto::sessiond` 与 `falcon-proto::program_status`，服务端这一侧在 `falcon-server/src/sessiond/` 与 `sessions/{local,ssh_host,manager}.rs`。

---

## 1. 现状与目标

| | Zellij 时代（ADR 0001 / 0019） | 现在 |
|---|---|---|
| 谁持 PTY | 宿主机上的 Zellij server；falcon 在外面再套一个跑 `zellij attach` 的 PTY / SSH pty channel | holder 直接持 PTY；falcon 连 holder，不再有外层 PTY |
| 客户端看到的字节 | Zellij 重画后的画面，客户端常驻备用屏、鼠标上报常开 | 程序的原始输出，客户端 alacritty 有真正的 scrollback |
| 接回时的历史 | `list-panes` + `dump-screen --ansi --full --pane-id`，带 #5311 的陈旧行 | VT 进程用 libghostty-vt 导出，带模式、光标、备用屏下的主屏历史 |
| 回放给新 Viewer | 服务端 4MB RingBuffer + `term_modes` 前缀 | holder 给的快照，holder 保证快照前后的输出在连接上不重不漏（5.5） |
| 终端查询（DA / DSR / DECRQM / OSC 10–12 …） | Zellij 答一部分；客户端再答一遍（多个 Viewer 各答一遍）；服务端 `OscColorGate` 代答 OSC 10/11/12 | VT 进程一处答全部；客户端对持久会话不再回答 |
| 滚动条 | wasm 插件 + `zellij pipe` 一问一答，新旧两套配置 | 客户端读本地 grid |
| 前台命令 | `zellij action list-clients` 解析 RUNNING_COMMAND | holder 直接 `tcgetpgrp`（Windows 走进程树） |
| OSC 7501 / 133 | Zellij 丢弃 | VT 进程解析，holder 保存，服务端推给界面 |
| 部署 | 宿主机自己下载 Zellij（不校验哈希） | 后端推送，文件名带内容哈希，推完握手；Windows 走 base64 行协议 |
| 逃生通道 | `zellij attach mj-…` | `falcon-sessiond attach <SID>` |
| 与 Zellij 的关系 | — | 一次替换，不并存；升级后旧会话一律显示为已丢失（14） |

---

## 2. 进程模型

### 2.1 为什么一会话两个进程

libghostty-vt 有两个风险：Zig 的 panic 是 trap、跨 FFI 接不住；C API 不稳定、升级频繁。持 PTY 的进程一旦退出，shell 收到 SIGHUP，会话就死了——这正是 falcon 对用户的核心承诺。所以把「必须活着的」和「可能出事的」拆开：

- **holder**（`falcon-sessiond hold`）：纯 Rust，不调用任何 libghostty-vt 函数。只做 PTY 读写、socket / 命名管道收发、字节转发、程序状态记录、前台命令探测、拉起与看护 VT 进程。线程模型：PTY 读 / 写、监听、每条连接的读 / 写、VT 进程的读 / 写各一条线程，经一个 channel 汇到主循环，状态只在主循环里改。处理一个事件时 panic 只断开那条连接（`catch_unwind`）。
- **VT 进程**（`falcon-sessiond vt`）：holder 用自己的二进制拉起，经 stdin / stdout 上的内部帧通信（`vtproto.rs`，kind 从 0x81 起，与对外协议不重叠）。持有 libghostty-vt 终端，吃 holder 转来的输出副本，回答查询，按请求导出快照。

两者是同一个二进制的两个子命令，分发时只有一个文件。

### 2.2 VT 进程出事时

- holder 读到管道 EOF 即判定 VT 进程已死。holder 继续转发字节，程序与客户端都不受影响。
- 立即重启 VT 进程，并用 holder 自己留的**原始输出尾巴**（最近 256 KiB）喂给它重建画面。这段喂入标记为 `replay`：VT 进程在它期间**不回答查询、不报事件**——那些查询早就有人答过了，再答一遍就成了程序收到的多余输入。
- 在途的快照请求改基到当前偏移、重新发给新进程（5.5）。
- 60 秒内崩 3 次即进入**降级**：不再重启，holder 只转发。快照退化为原始尾巴（截断过就从第一个换行之后开始），查询无人回答。holder 发 `Event{vtDegraded}`。
- VT 管道积压超过 16 MiB（VT 进程卡住）同样当作崩溃：杀掉、重启。

### 2.3 VT 进程热升级

服务端接回会话时，若 holder 的 `Welcome.vtVersion`（它的 VT 进程用的 ghostty commit）与这次部署的二进制里的不同，就发 `UpgradeVt{binary}`。holder 让旧 VT 进程导出 libghostty-vt 的**二进制快照**，记下导出请求之后喂进去的字节，起新 VT 进程（新二进制）先 `Restore` 快照、再以 `replay` 方式补喂那段字节，最后关掉旧进程。在途的快照请求同样改基后转给新进程。新版本解不了旧快照（`Restored{ok:false}`）时退回原始尾巴重建；新二进制起来就崩则退回旧二进制。这样升级 ghostty 不必等存量会话自然消亡。

已有真 PTY 用例：备用屏状态经热升级后仍在快照里（15）。

### 2.4 生命周期

```
falcon-server ──exec──▶ falcon-sessiond-<版本>-<哈希> spawn / bridge --create
                              │ setsid（Windows：CREATE_BREAKAWAY_FROM_JOB，不行再 WMI）
                              ▼
                        holder ──openpty──▶ shell（会话 leader，进程组 = 前台判定基准）
                          │  └─ socket / 命名管道 ◀── falcon-server（本机直连 / SSH 经 bridge）
                          │                     ◀── 人手 `falcon-sessiond attach`
                          └─pipes─▶ VT 进程（libghostty-vt）
```

- **shell 退出**：holder 收到退出状态后最多再等 500 ms 读完 PTY 里的余下输出（后台进程占着 slave 时永远等不到 EOF，Windows 的 ConPTY 不关就不会 EOF），然后：丢掉 working / blocked 程序状态（规范要求），写 `<sid>.exit`（`code` / `signal` / `at`），向所有连接发 `Exit`，删 meta 与 socket，关掉 VT 进程，最多等 2 秒把 `Exit` 帧送出去，退出。
- **Terminate**：holder 向 shell 的进程组发 SIGHUP（shell 来得及收尾、存历史），2 秒后还在就补 SIGKILL（Windows 结束整棵进程树）。连不上 holder 时 `falcon-sessiond kill` 按 meta 里的 pid 做同样的事。
- **holder 自身**：忽略 SIGHUP（没有控制终端，保险起见）；SIGPIPE Rust 运行时已经忽略。shell 不继承这些：portable-pty 在 exec 前把 SIGHUP 等重置为默认，std 的 spawn 重置 SIGPIPE。stdio 指到空设备，日志写 `log/<sid>.log`，满 1 MiB 轮转一次。

---

## 3. 宿主机上的布局与命名

`<root>` 即 falcon 根：本机是 `--data-dir`，远端是探测到的 home 下的 `.falcon`（`src/host.rs` 的 `remote_root`，含从 `.mojito` 迁移）。

```
<root>/bin/falcon-sessiond-<版本>-<内容哈希前 8 位>[.exe]   按内容命名，旧的不覆盖、不删
<root>/sessiond/sessions/<sid>.json     holder 起来后写入（SessionMeta：pid、shell_pid、版本、owner、端点、argv、cwd、created_at）
<root>/sessiond/sessions/<sid>.exit     shell 退出后写入，`list` 时清掉 7 天前的
<root>/sessiond/sessions/<sid>.spec     spawn 交给 holder 的启动参数（含整份 env），holder 读完即删
<root>/sessiond/run/<sid>.sock          Unix socket（见下方长度规则）
<root>/sessiond/log/<sid>.log
```

- `<sid>` 是会话 uuid 去掉横线后的前 16 个 hex（`falcon_proto::sessiond::sid_of`），与 Zellij 时代的 `mj-<16hex>` 同源。只认 16 位小写 hex，拼进路径前校验。
- **文件名带内容哈希**：开发构建版本号不变而内容在变，同名覆盖会换掉正在跑的 holder 的二进制（Linux 上 `/proc/self/exe` 变成 "(deleted)"，VT 进程重启就起不来）。按内容命名，"在不在"就是"对不对"，不需要 `.sha256` 旁文件。本机也拷一份到 `<dataDir>/bin/` 再跑，不直接跑 `target-sessiond` 里那个。
- **socket 路径长度**：macOS 上限 104 字节，Linux 108 字节。纯函数 `socket_path(root, sid, uid)`：`<root>/sessiond/run/<sid>.sock` 不超过 100 字节就用它；否则用 `/tmp/falcon-sd-<uid>-<root 的 FNV 短哈希>/<sid>.sock`。这个目录创建时 0700，已存在时核对属主（别人预建的目录可以用来劫持 socket）。meta 里记实际端点，连接时以 meta 为准。
- **Windows 命名管道**：`\\.\pipe\falcon-sd-<root 的短哈希>-<sid>`，DACL 只给当前用户 SID（`D:P(A;;GA;;;<SID>)`），带 `PIPE_REJECT_REMOTE_CLIENTS`；第一个实例带 `FILE_FLAG_FIRST_PIPE_INSTANCE`（不和同名的另一个 holder 抢）。
- **owner**：falcon 实例的随机 id（settings 表的 `instance_id`）。同一台远端可能同时被两台机器上的 falcon 用，清理孤儿时只动 owner 是自己的会话（10.6）。
- 权限：`sessiond/` 及子目录 0700，socket 0600，meta / spec / exit 文件 0600；accept 后再核对对端 uid（Linux `SO_PEERCRED`，macOS `getpeereid`）。

---

## 4. 子命令

| 子命令 | 谁用 | 做什么 |
|---|---|---|
| `spawn --root R --id SID --spec B64` | 服务端（本机、Windows 断线验证） | 写 spec 文件、脱离调用方拉起 holder，等 meta 出现且端点连得上（10 秒）后打印 `SpawnResult` JSON 退出。已存在且存活时 `existing: true`。起不来时把 holder 日志最后几行带进错误 |
| `bridge --root R --id SID [--create B64]` | 服务端（SSH） | 连上 holder 后先往 stdout 写前导标记 `BRIDGE_PREAMBLE`，再把 stdin / stdout 与连接双向透传，**不解析帧**（与 holder 版本无关）。stdin 读到 EOF 就整个退出（Windows 的管道不能只关一半）。连不上时用退出码说原因：3 = 不在了、5 = 已正常结束（有 `.exit`）、4 = 还在但暂时连不上；带 `--create` 时先 spawn 再连 |
| `attach SID [--force]` | 人 | 逃生通道：清屏、拿快照、进 raw 模式、透传；`Ctrl+\` 断开（`FALCON_SESSIOND_DETACH_KEY` 可改成 `^X` 形式或 `none`）。在会话里（环境里有 `FALCON_SESSIOND_ID`）拒绝嵌套，除非 `--force`。终端尺寸靠 250 ms 轮询（不装 SIGWINCH 处理器，Windows 同理）。断开时把会话可能开着的鼠标上报、备用屏等模式关掉 |
| `list [--json]` | 人 / 服务端 | 读 `sessions/*.json` 与 `*.exit`，按 pid 且进程名是 falcon-sessiond 判断活着（防 pid 复用） |
| `kill SID…` | 人 / 服务端 | 连得上就发 `Terminate` 等 `Exit`；连不上按 meta 的 pid 动手 |
| `history SID [--vt\|--plain\|--html]` | 人 | 导出历史（请求 VT 进程格式化） |
| `version [--json]` | 服务端握手 | `{version, protoMin, protoMax, ghostty, target}` |
| `hold` / `vt` | 内部 | holder 与 VT 进程的入口 |

人用的子命令缺省的根目录是二进制所在 `bin/` 的上一层（`~/.falcon/bin/falcon-sessiond-…` → `~/.falcon`），`--root` 或 `FALCON_SESSIOND_ROOT` 可改。

`--spec` 是 base64url（无填充）编码的 JSON（`SpawnSpec`）：`{sessionId, argv, env: [[k,v]…], envClear, cwd, cols, rows, scrollbackLines, owner, appearance}`。env 是有序列表（同服务端 `merge_env` 的顺序要求）。`envClear`：本机为 true（服务端已经解析好整份登录环境），SSH 为 false（在 holder 继承的登录 shell 环境上叠加）。holder 自己补 `FALCON_SESSIOND_ID=<sid>`。spec 经文件交给 holder，不走命令行：里面有整份环境变量，Windows 的命令行与 WMI 都有长度上限，也不该出现在进程列表里。cwd 不存在（项目被删 / 盘没挂）时退回家目录，不至于起不来。

---

## 5. 协议

协议类型放在 `falcon-proto::sessiond`（协议的唯一真相来源在 falcon-proto），holder、`attach`、falcon-server 共用。**holder 的协议只增不改**：二进制按版本命名、旧版不删，新版 falcon-server 必须能和它可能遇到的所有旧 holder 说话——字段只加不删，未知字段忽略，未知帧按长度跳过。

### 5.1 帧

```
u8 kind | u32 LE len | payload
```

- 客户端 → holder 的 kind 在 0x01–0x3f，holder → 客户端在 0x41–0x7f，VT 进程的内部协议从 0x81 起：抓包时一眼看得出方向。
- 字节类的帧载荷是原始字节加几个定长头字段；控制类的帧载荷是 JSON（字段 camelCase）。
- 上限：`Synced` / `Exported` 64 MiB，`Input` / `Output` 1 MiB，其余 1 MiB。超限即断开（宁可重连，也不能让一个畸形的长度字段把内存吃光）。

### 5.2 握手

客户端先发 `Hello{protoMin, protoMax, role: "server"|"attach", client}`，holder 回 `Welcome{proto, version, vtVersion, vtDegraded, cols, rows, pid, startedAt, sessionId, offset}`，紧接着推一次程序状态全集 `Status`。holder 取双方区间交集的最大值，没有交集就回 `Error` 并断开。第一帧不是 Hello、或 10 秒内不握手的连接直接断开。bridge 不参与握手，只透传字节。

### 5.3 客户端 → holder

| 帧 | 说明 |
|---|---|
| `Input(bytes)` | 写进 PTY（写线程里写，前台程序不读输入时不卡主循环） |
| `Resize{cols, rows}` | 见 5.6 |
| `Sync{req}` | 请求一份 VT 快照，见 5.5 |
| `Export{req, format: vt\|plain\|html}` | `history` 子命令用 |
| `Appearance{scheme, foreground, background, cursor}` | 转给 VT 进程设默认色与深浅（OSC 10/11/12、`CSI ? 996 n` 的回答依据）；深浅真的翻转、且输出流里见过 DECSET 2031 订阅时，holder 往 PTY 写 `CSI ? 997 ; 1\|2 n`（没人订阅时绝不注入，会被前台程序当键盘输入） |
| `Foreground{req}` | 前台命令，见 9 |
| `StatusSeen{ids}` | 用户看过了，清掉 done / error，见 8 |
| `Terminate` | 见 2.4 |
| `Ping` | 回 `Pong` |
| `UpgradeVt{req, binary}` | 热升级 VT 进程，见 2.3 |

### 5.4 holder → 客户端

| 帧 | 说明 |
|---|---|
| `Output{offset: u64, bytes}` | `offset` 是这块字节在会话输出流里的起始位置（holder 启动时为 0，单调递增） |
| `Synced{req, offset, snapshot, continuation}` | `snapshot` 反映流中 `[0, offset)`；`continuation` 是解析器停在半截序列 / 半个 UTF-8 字符时要补在后面的原始字节（多半为空） |
| `Exported{req, bytes}` | |
| `Status{records}` | 程序状态全集（变化时推，握手后先推一次） |
| `ForegroundIs{req, name}` | |
| `Event{type, …}` | `vtRestarted{atOffset}`、`vtDegraded`（`title` / `bell` / `prompt` 预留，暂不发） |
| `Exit{code, signal}` | shell 退出，随后 holder 关闭连接 |
| `Pong` / `Error{message}` / `Upgraded{req, ok, message}` | |

### 5.5 快照与增量怎么接上

holder 往 VT 进程管道里写的字节，与发给连接的字节是同一个顺序。收到某条连接的 `Sync` 时，holder 记下此刻的偏移 N、在 VT 管道里排一个快照请求，并**把这条连接之后的输出先扣下**；VT 进程按顺序处理，导出的快照恰好对应 `[0, N)`。快照回来，holder 发 `Synced{offset: N}`，再把扣下的（全都 `≥ N`）放出去。同一条连接上有多个在途的 Sync 时按 FIFO 处理，每放一次只放到下一个切点之前。

于是在这条连接上，`Synced` 之前的输出都 `< N`、之后的都 `≥ N`，**接收方不用拼接**：

- falcon-server（一条连接服务多个 Viewer）：等回放的 Viewer 标 `syncing`，合并窗口 flush 时跳过它们；`Synced` 一到（按连接上的到达顺序、同一个同步段里），先把攒着的输出发给已经在看的 Viewer，再把快照发给等回放的、清掉标记。等回放的 Viewer 都搭同一班 Sync。
- `attach`：`Synced` 之前的 `Output` 全丢，之后照常写。

**continuation 与 UTF-8**：服务端把输出按块做 UTF-8 解码再给 Viewer，半个字符压在解码器里、等后半截到了作为实时输出整个吐出来。continuation 可能也以这半个字符结尾——回放里再带上它就成了一个 U+FFFD 加一个完整字符，所以服务端从 continuation 里去掉解码器压着的那几个字节（`sessiond/backend.rs` 的 `replay_text`，带单测）。

**VT 进程重启 / 热升级时**在途的快照请求改基到当前偏移重新发：扣下的输出都已含在新快照里，丢掉。

### 5.6 尺寸

holder 记每条连接最后一次请求的尺寸，生效尺寸取**最近有过输入或 resize 的那条连接**的请求（与服务端 ViewerArbiter「深浅跟最近操作的那端」同一思路）。falcon-server 自己的多 Viewer 仍按 arbiter 取最小后作为一条连接提交。人手 `attach` 断开后，生效尺寸回到 falcon-server 的请求。

### 5.7 慢连接

PTY 读取绝不因为连接慢而阻塞：每条连接一个发送队列，积压超过 8 MiB 就断开该连接（服务端会重连、重新 Sync）。VT 管道积压超过 16 MiB 按 2.2 处理。

---

## 6. 终端状态与接回

### 6.1 VT 进程的配置

- 尺寸跟随 holder；scrollback 上限 **10 000 行**（与客户端 `TermOptions` 的 `scrollback` 一致），另设字节上限 32 MiB 兜底。
- continuation 跟踪上限 1 MiB（在写入第一个字节前就开，否则之前的半截序列重建不了）。超了（巨大的 kitty 图片 APC 卡在半截）就只能接受切点处的瑕疵。
- 回调：`WRITE_PTY`（过滤后回 PTY）、`PROGRAM_STATUS`、`SEMANTIC_PROMPT`、`TITLE_CHANGED`、`RESET`（RIS 清空程序状态）、`COLOR_SCHEME`、`XTVERSION`。
- 静态库用 `ReleaseSafe` 编。

### 6.2 导出回放用的 VT

原型与语料实测得出的做法（`vt.rs` 的 `replay_bytes`）：

1. 格式化选项：`emit = VT`，**`unwrap = true`**（否则软折行处写成 `\r\n`，客户端的 WRAPLINE 全丢），`modes`、`scrolling_region`、`tabstops`、`cursor`、`style`、`hyperlink`、`protection`、`charsets` 开；`kitty_keyboard`、`keyboard` 关（客户端不支持）；`pwd`、`palette` 关（客户端不认 OSC 7，钉的 commit 导出 pwd 还多一个 NUL；调色板归客户端主题管）。
2. **补回末尾空行**：格式化器总是省掉末尾的空行（formatter.zig 原话 "Trailing blank lines are always trimmed"）。有历史时这会让重放少滚几行：整屏上移、光标压到内容行上（语料实测：less 退出后的主屏差一行；`seq 100` 之后敲两下回车的屏差两行）。做法：单独导一份纯正文，拿同尺寸的临时终端重放它、比对历史行数，差几行就在正文与附加状态之间补几个 `\r\n`（补之前先 `SGR 0`，免得新滚出来的行沾上最后一段的背景色）。单独导的正文末尾多一个 `SGR 0`，在整份导出里定位正文时先去掉它。
3. **几个模式挪到正文之后**：格式化器把模式放在正文**之前**。关自动换行（`?7l`）、插入模式（`4h`）、换行模式（`20h`）会改变正文的排版（正文里软折行的长行是靠自动换行折回去的），挪到最后。
4. 当前在备用屏：先编码二进制快照、解到一个副本里，在副本上写 `CSI ? 1049 l` 退出备用屏，导出副本的正文（主屏 + 历史，同样补回末尾空行），再把光标放回主屏进备用屏时存下的位置（退出备用屏时 1049 恢复的就是它）。然后拼上 `CSI ? 1049 h CSI H CSI 2 J`，最后接当前终端的完整导出（备用屏 + 模式）。
5. 后处理：`38;5;N`（N < 16）改写成 `3N` / `9N`（`48;5;N` 改成 `4N` / `10N`，冒号形式同理），否则客户端存成 `Indexed`，「粗体用亮色」失效。跑在字节上的纯函数，带单测。

已知差异（照单接受，语料比对时归一掉）：正文里的 OSC 8 链接不导出（格式化器只恢复光标当前的链接状态）；`CSI 2 J` 清掉的那一屏不进历史；从没写过的空格带着前一段的前景色（不可见）。

### 6.3 服务端何时要快照

- Viewer 进来（`add_viewer` → `send_replay`）；
- 接回成功（`mark_active(replay=true)`，所有 Viewer 一起）；
- lagged 的 Viewer 排空后需要重新对齐。

1 万行历史的快照约 120 KB，导出约 0.5 ms。

---

## 7. 终端查询的代答

**原则：持久会话的所有终端查询，都由 VT 进程一处回答；客户端对 `durable` 会话的 `PtyWrite` 一律不发。** 理由：没有 Viewer 时（agent 在后台启动、服务端重启后）总得有人答，否则程序卡在等回复上；多个 Viewer 时客户端每个都答一遍；客户端的回复还可能卡在队列里。

| 查询 | 回答方式 |
|---|---|
| DA1 / DA2 / DA3 | libghostty-vt 默认值：`?62;22c`、`>1;0;0c`、`DCS !\|00000000 ST` |
| DSR 5n / 6n（光标位置） | 默认（与客户端画面一致：同一字节流、同一尺寸） |
| DECRQM | 默认 |
| XTVERSION | `falcon <版本>`，不冒充 ghostty（程序会据此打开 ghostty 专有特性，而客户端画画的是 alacritty） |
| `CSI ? u`（kitty 键盘） | **在回 PTY 前把 `CSI ? <flags> u` 过滤掉**。libghostty-vt 默认回 `?0u`，等于声称支持，程序就会改发 kitty 编码；而客户端的 `keys.rs` 只有传统 xterm 编码。不回答时程序先收到 DA 就判定不支持（真 PTY 用例验证：先问 `CSI ? u` 再问 DA，第一个到的是 DA）。等客户端实现 CSI u 编码后再放开 |
| OSC 10 / 11 / 12 | 由 `Appearance` 设的默认色回答（没设时不答）。服务端的 `OscColorGate` 只对非持久会话起作用 |
| OSC 4 调色板 | libghostty-vt 默认调色板 |
| `CSI ? 996 n`（深浅） | `COLOR_SCHEME` 回调，按 `Appearance.scheme` |
| XTWINOPS 14/16/18/19 t | 不答（程序取尺寸主要走 `TIOCGWINSZ`） |
| OSC 7501 `?` | 默认回答（声明支持的 states / kinds） |
| OSC 52 读剪贴板 | 不答 |

非持久会话照旧由客户端回答。回放解析期间客户端本来就静音（`muted`），不受影响。

**新旧版本错配**：旧客户端连新服务端时，仍会对持久会话回答 DA / DSR，程序会多收到一份回复。只有原生客户端连别的机器上的服务端时才会碰到（macOS 包里两者同版本、浏览器版由服务端托管），发版说明提醒同时升级，不在服务端做过滤（CPR 与 Shift+F3、焦点事件与回复同形，滤不干净）。

---

## 8. 程序状态（OSC 7501）

[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)（草案 0.4）。

- **解析**：VT 进程的 `PROGRAM_STATUS` 回调（libghostty-vt 已按规范校验、解码 base64），转成 `ProgramStatusReport` 交给 holder。
- **存储在 holder**：纯 Rust 的 `ProgramStatusStore`（`falcon-proto::program_status`，带单测），VT 进程重启也不丢。规则照规范：报告整条替换同 id 的记录；`kind` 只对 blocked、`progress` 只对 working / blocked 有意义；`clear` 删掉该 id 及其下全部（空 id 清空全部，`build` 不会误伤 `buildx`）；`app` 缺省时从最近的、带 app 的祖先继承；上限 256 条，满了淘汰最久没更新的；RIS 清空全部。
- **生命周期**：
  - OSC 133 `A`（新提示符）时清掉 working / blocked；
  - 附着的进程退出：holder 在有 working / blocked 记录时每秒看一次前台是不是回到了 shell（前台进程组就是 shell 的、而且 shell 没有子进程），**连续两次**都在 shell 手里才清。只看一次不行：shell 的内建命令（`printf`）、两条命令之间的空档里前台也是 shell 自己，刚报上来的状态会被立刻清掉（用例实测）。只看进程组也不行：agent 会话的启动脚本是 `sh -c`，CLI 和脚本可能在同一个进程组里；
  - shell 退出时清掉 working / blocked；
  - done / error 在用户看过会话后清（客户端在窗口获得焦点、或在窗口里操作时发 `{type:"status_seen"}`，服务端转成 `StatusSeen`）。
- **推到界面**：holder 发 `Status{records}`；服务端存进 entry、经 WS 广播 `{type:"program_status", records}`，并在 `GET /api/sessions` 的 `Session.programStatus` 上带一个摘要（blocked > error > working > done，同级时根记录优先、再取最近更新的，全 idle 时没有），没人在看的会话也能在列表上显示。根记录优先是因为 Claude Code 把每个后台子 agent 报成一条 working：只按时间挑，摘要会在根记录的「正在干什么 + 进度」与子任务描述之间来回跳。
- **显示规则**（客户端，falcon-core 的 `program_status.rs`）：侧栏会话行与窗口标题栏显示徽标（blocked 写明「授权 / 回答 / 凭据」，working 可带进度，done 绿勾、error 红三角）；某个会话进入 blocked 时用 `ToastExt` 提醒一次（带会话显示名与「切过去」按钮，同一会话原地替换，不卡了自动撤掉）；**进入** done / error 时响一声提示音（`FinishChimes`：停在 done 里只换 msg 不再响——Claude Code 跑完一轮报 done 后会再补一次本轮摘要；人正看着那个会话时不响，「看着」= 窗口在前台 + 活动视图是它 + 键盘焦点在它的终端上；声音由 `Platform::play_chime` 给：macOS 是系统音 Glass，Windows 是 `MessageBeep`，浏览器用 Web Audio 合成、要等页面上有过一次手势；设置 → 通知里可以关）。每扇终端窗口底部常驻一条**状态栏**（`terminal/status_bar.rs`）：状态 + 进度条 + 程序那半句，右边是子任务数（悬停列出）、计时（已运行 / 已等待 / 用时，`StateClock` 只认本客户端亲眼看到的状态切换，不编开始时间）与程序名；没有程序状态时显示前台命令或「空闲」。常驻是为了不让终端的行数跟着状态变——一出一没就是一次 resize，正在干活的 TUI 整屏重画。`title` / `message` 是程序给的不可信文本：过 `sanitize_display_text`（去掉控制字符、方向覆盖、零宽格式字符，按字符数截断），并且总是带上会话的显示名。
- **Windows**：ConPTY 会吞掉它不认识的 OSC，收不到。

---

## 9. 前台命令

- **Unix**：holder 用 `tcgetpgrp(master)` 拿前台进程组 leader，取名字：macOS 用 `proc_name`，Linux 读 `/proc/<pid>/comm`（15 个字符截断）。
- **Windows**：没有前台进程组。holder 用 Toolhelp 快照沿 shell 的子孙往下找最深的那个（同深取 pid 大的），跳过 conhost / OpenConsole，去掉 `.exe`。启发式，但比 Zellij 时代恒为空要好。
- 服务端的节流（2.5 秒、只在有 Viewer 时探测）与 `is_shell_command` 过滤不变，数据源换成 `Foreground` 帧（2 秒超时）。SSH 会话不再每次 exec 一条命令。

---

## 10. falcon-server 接入

分层照 `git/`：`falcon-server/src/sessiond/` 下依次是

- `command.rs`：纯函数（目标三元组识别、按内容哈希命名、各子命令 argv、SSH 上两类宿主机的命令行），零 I/O；
- `bin.rs`：各目标的二进制从哪来；
- `deploy.rs`：推到宿主机、握手、重试；
- `backend.rs`：`SessiondBackend`（帧协议、bridge 前导标记、快照 + continuation）。

四种执行环境的拼装、远端探测、根目录迁移原在 `zellij/host.rs`，挪成了 `src/host.rs`（git / 文件 / px0 / 守护进程共用）。会话附着在 `sessions/local.rs`（本机）与 `sessions/ssh_host.rs`（SSH，原 `ssh_zellij.rs`）。

### 10.1 数据库与协议

- **`sessions` 表不加列**。持久与否已经有 `durable`：`durable = 1` 就是守护进程会话，`durable = 0` 就是普通 PTY。`scroll_plugin` 列不再读写（新库不再建这一列，老库里的留着）。
- **`session_hosts` 表**取代 `zellij_hosts`：`host, port, username, authorized, sessiond_version, verified_durable, updated_at`。建表时从 `zellij_hosts` 抄一次授权（`INSERT OR IGNORE … WHERE authorized IS NOT NULL`），Zellij 的版本、下载镜像、Windows 断线验证结果都不要。`sessiond_version` 记 falcon 的版本号（客户端拿它与 `requiredVersion` 比，对不上就弹出部署进度）。
- settings 表存 `instance_id`（`Db::instance_id`，首次用到时生成）。
- `falcon-proto`：`zellij.rs` 改名 `install.rs`（`InstallStage`：probing / uploading / verifying；`InstallFailure`：arch-unsupported / dir-not-writable / probe-failed / upload-failed / verify-failed / cancelled / sessiond-unavailable；`NonDurableReason` 是它加上 not-authorized / windows-job-object；`HostInstallStatus` 去掉了 baseUrl / defaultBaseUrl）；`Session` 加 `programStatus`；`ServerMessage::ProgramStatus`；`ClientMessage::StatusSeen`；WS 的 `scroll` 两向都删了（老服务端发来的 `scroll` 在新客户端上是 `Unknown`）。Zellij 时代留在库里的非持久原因（no-tar 之类）读出来是 `Unknown`。
- 协议 fixture 用真服务端重生过（`cargo xtask fixtures`）：与旧 fixture 的形状差异只有主机状态少了下载镜像字段、本机部署不再分阶段报进度（只是拷文件）。

### 10.2 Backend 接口

`Backend` trait 原有四个方法不变，守护进程才有的能力是带缺省实现的方法，非持久的 `LocalPtyBackend` 与 SSH pty channel 不用改：

```rust
fn request_sync(&self) -> bool { false }   // true = 快照会经 on_event(Synced) 按流的顺序回来
fn foreground(&self) -> Option<LocalBoxFuture<'static, Option<String>>> { None }
fn set_appearance(&self, _hint: &OscColorHint) {}
fn status_seen(&self, _ids: &[String]) {}
fn terminate(&self) -> bool { false }      // true = 由 backend 自己结束会话
fn upgrade_vt(&self, _binary: &str) {}
```

`BackendCallbacks` 加 `on_event: Rc<dyn Fn(BackendEvent)>`，`BackendEvent` 有 `Synced(String)`、`ProgramStatus(records)`、`Exit(status)`、`VtRestarted`、`VtDegraded`、`BridgeExit(code)`。`on_data(String)` 不变（`SessiondBackend` 内部做 UTF-8 解码）。

### 10.3 manager 的改动

`LiveEntry` 按 `durable` 分两条路：非持久会话原样（RingBuffer、`term_modes` 前缀、`OscColorGate`）；持久会话：

- **创建**：`prepare` 先部署守护进程（10.5），成功则 `durable = true`。附着失败（守护进程拉不起来：Windows 的 Job 不让脱离、WMI 也被禁）时**退成非持久**并写明原因（本机且后端在 Job 里 → `windows-job-object`，其余 → `verify-failed`），会话照样开得出来。没有退回 Zellij 这条路。
- **输出路径**：只做 16ms 合并与背压，不过 `OscColorGate`、不跟踪模式、不进 RingBuffer。
- **回放**：见 5.5。
- **接回**：不再有 `captured_history`。连上 holder 即视为存活，`mark_active(replay=true)` 之后所有 Viewer 搭一班 Sync。接回时 entry 还没有部署信息（后端重启后重建的 entry）就重新 prepare：链路一类的失败（probe / upload / cancelled / unknown）返回可重试的错误，稳定的环境事实（没有这个架构的构建、没授权）说明这台机器上不可能有守护进程会话，记 `SessionGone`。接回时如果 VT 进程的 ghostty 与这次部署的不同，发 `UpgradeVt`（2.3）。
- **退出判定**（替换 Zellij 的 `has_session`）：
  - 收到过 `Exit` → `dead(exited)`；
  - 本机连接断开：有 `<sid>.exit` → `dead(exited)`；meta 里的 pid 还活着 → `unverified`（自动接回）；否则 `dead(session-gone)`；
  - SSH：bridge 退出码 3 → `dead(session-gone)`，5 → `dead(exited)`，其余（链路断了、bridge 被杀）→ `unverified`。
  - 接回时连不上同理：本机看文件（`local_session_state`），SSH 看 bridge 的退出码（`ConnectError::{Gone, Exited, Unavailable}`）。
- **终止**：`backend.terminate()` 发 `Terminate`；要等（删 worktree 前）时最多 3 秒等 `Exit`。连不上 holder 时 exec `falcon-sessiond kill`。
- **标题 / 前台**：走 `backend.foreground()`。
- **外观**：`set_appearance` 同时调 `backend.set_appearance`；2031 通知由 holder 发。
- **滚动**：WS 的 `scroll` 整个删除，滚动全在客户端本地。

### 10.4 argv 与 env

- **本机**：env = 登录环境探测的结果（`base_env`）+ term env（`TERM=xterm-256color`、`COLORTERM=truecolor`、`TERM_PROGRAM=falcon`、深浅相关的三项）+ askpass（`PATH` 前置、`FALCON_SESSION_ID`）+ `FALCON_SESSIOND_ID`，`envClear = true`。argv = `[shell]`（交互式非登录 shell）；agent 会话的 argv = `[启动脚本]`。cwd 直接交给 spawn（容器项目落在虚拟目录）。
- **SSH POSIX**：`exec '<探测到的登录 shell>' -l -c 'PATH=<askpass>:$PATH exec <bin> bridge --root … --id … [--create <spec>]'`。套一层登录 shell，让 holder 继承登录环境（ADR 0001：在最外层套，一次修到底）；rc 文件往 stdout 打的字由前导标记挡掉（隔离 sshd 用例验证：`.profile` 里的 echo 没有混进帧流）。spec 的 env 只有 term env、`FALCON_SESSION_ID`、`FALCON_SESSIOND_ID`，`envClear = false`。
- **SSH Windows**：PowerShell 里**不能**用 `& exe` 跑 bridge——PowerShell 会把原生命令的 stdout 当文本行收下再按控制台编码重新写出，二进制帧流全毁。用 .NET 的 `Process` 起它、不重定向任何流（子进程直接继承 powershell 的 stdin / stdout，也就是 sshd 的管道），再把退出码透传出去。参数按 `CommandLineToArgvW` 的规则转义（`windows_quote_arg`，带单测）。外层仍是 `powershell -EncodedCommand`，远端 DefaultShell 是 cmd 还是 PowerShell 都不影响。
- env 在会话创建时冻结（同 Zellij server，`login_env.rs` 的注释）。

### 10.5 部署

| | 本机 | SSH POSIX | SSH Windows |
|---|---|---|---|
| 探测 | 不需要 | `RemoteProbe`：home、`uname -sm`、登录 shell → 目标三元组 | `PROCESSOR_ARCHITECTURE`（ARM64 跑 x64 构建） |
| 二进制从哪来 | `bin.rs`（见下） | 同左，取对应目标 | 同左 |
| 是否已在 | — | `[ -x <bin> ]`，在就直接握手 | `Test-Path` 同理 |
| 推送 | 拷到 `<dataDir>/bin/` | `exec_with_input`：`mkdir -p; chmod 700; cat > f.partial; chmod 755; mv -f`（同 px0） | base64 行协议（`transfer.rs` 的编码器），核对字节数，`Move-Item` |
| 握手 | `version --json`：版本、目标、协议区间都要对得上 | 同左 | 同左 |
| 失败原因 | `dir-not-writable` / `verify-failed` / `sessiond-unavailable` | 另有 `probe-failed` / `upload-failed`（自动重试 3 次） / `arch-unsupported`；noexec 挂载卡在握手（隔离 sshd 用例验证：报 `verify-failed`，详情是远端的 Permission denied） | 同左；另有真实断线验证（12） |

阶段经 `/ws/install/{project_id}` 上报：`probing` → `verifying`（已在）或 `uploading` → `verifying`。本机只是拷文件，不报阶段。

**二进制定位**（`bin.rs`，照 `meegle/bin.rs` 的优先级链）：

1. `FALCON_SESSIOND_DIR`：一个目录，按 `<三元组>/release/falcon-sessiond[.exe]` 或 `<三元组>/falcon-sessiond[.exe]` 放；
2. 编进发布版的那份：`embed-sessiond` feature，5 个目标各一份 gzip（`cargo xtask server` 准备在 `target-sessiond/embed/`）；
3. 开发构建：`native/target-sessiond/<三元组>/release/falcon-sessiond`（`cargo xtask sessiond` 的产物）；
4. 都没有：非持久，原因 `sessiond-unavailable`（只会发生在没编那个目标的开发构建上）。

开发时改了守护进程要重跑 `cargo xtask sessiond`（远端目标加 `--targets all`）：同版本号不会触发重推，但文件名按内容哈希，新编的会被当成新文件推上去；没重编的话宿主机上跑的还是旧的（实测踩过：加 bridge 前导标记后没重编，服务端一直等那个标记直到握手超时）。

### 10.6 孤儿清理

服务端第一次准备好某台宿主机（本机或某个 `user@host:port`）时扫一次：`list --json`，找出 owner 是本实例、还活着、不在库里、起来超过 1 小时的会话，`kill`。1 小时的宽限避开「holder 刚起来、会话行还没落库」的竞态（`create_session` 在附着成功之后才 `insert_session`）。owner 不是自己的一律不动。

### 10.7 没有回退开关

不设 `FALCON_SESSION_BACKEND` 一类的开关（ADR 0020 决定七）。守护进程出问题时的退路是降级到上一个 falcon 版本，代价见 14。

---

## 11. 客户端改动

以 `Session.durable` 区分（持久 = 守护进程会话，非持久 = 普通 PTY）：

1. **不发终端回复**：`terminal/view.rs` 处理 `PtyWrite` 时，持久会话直接丢弃（7）。恢复的窗口可能比会话列表先建出来、还不知道 `durable`，这期间的回复先攒着（最多 64 条），拿到列表后再决定发掉还是丢掉。另外修掉回复可能卡在队列里的问题：每段输出解析完都取一次事件（原来只在脏位 0→1 时取），非持久会话还靠客户端回答。
2. **滚动条读本地**：`ScrollState::from_grid`（display_offset / history_size / screen_lines）每次渲染从快照推；拖动滑块时换算出目标再调 `TermCore::scroll_to(offset)`。WS `scroll`、位置探测、刷新定时器一起删除。滚轮在本地滚动时点亮滚动条。
3. **滚轮**：falcon-term 的 `route_wheel`：鼠标上报 → 备用屏且 `ALTERNATE_SCROLL`（?1007）开着才转方向键 → 本地历史。程序关掉 ?1007 时不再发方向键。
4. **回放后保持阅读位置**：`replace_with_replay` 同一把锁里记下原来的 `display_offset`，换上新状态后按它恢复（历史不够就停在最顶上）；在底部就留在底部。
5. **程序状态**：见 8 的显示规则；`ServerMessage::ProgramStatus` 变成会话事件，工作区自己算摘要写回 `Session.programStatus`（窗口开着时以推来的为准），侧栏与标题栏只读这一个字段。
6. **安装对话框**：去掉下载镜像输入框与「手动安装」段（守护进程没有公开的下载地址），阶段文案「探测 / 上传 / 校验」，失败原因按新枚举出文案（有单测拿协议枚举全集对账 `zh-CN.json`）。弹出条件：没问过授权，或宿主机上的版本与 `requiredVersion` 对不上（已授权的直接开始部署，升级时看得到进度）。
7. **kitty 键盘**：本期不做。`keys.rs` 补上 CSI u 编码之后，再放开 7 节里对 `CSI ? u` 的过滤，两件事必须同一个版本发。

不需要改的：回放帧格式、`muted`、OSC 8 链接点击、同步输出超时兜底。

---

## 12. Windows

**没有在 Windows 真机上验证过。** 已做的：全部代码 `cargo xtask sessiond --check --targets all` 零警告（含 `x86_64-pc-windows-gnu`），release 产物能编出来。真机清单见 15。

- **ConPTY**：holder 用 portable-pty 的 ConPTY；resize 走 `ResizePseudoConsole`。shell 退出后 ConPTY 不关就不会 EOF，holder 靠 500 ms 的宽限收尾（2.4）。
- **脱离调用方**（守护进程的 `spawn.rs`）：`CreateProcess` 带 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`；Job 不允许脱离时返回拒绝访问，改用 PowerShell 的 `Invoke-CimMethod Win32_Process Create`（WMI 提供者进程不在调用方的 Job 里，Zellij 时代实测能熬过 SSH 断开）。WMI 拉起的进程没有我们的句柄，就绪判定靠轮询 meta 与命名管道。两条都失败时 spawn 报错，服务端把会话退成非持久（本机且后端在 Job 里时原因是 `windows-job-object`）。
- **命名管道**：同步句柄上的 I/O 会被系统串行化（一条线程阻塞在 ReadFile 上，另一条的 WriteFile 也得等），所以一律重叠 I/O、每次操作自带事件；从外面断开连接用 `CancelIoEx` + `DisconnectNamedPipe`。服务端本机连接时对 `ERROR_PIPE_BUSY` 重试。
- **SSH**：bridge 经 .NET Process 继承句柄（10.4）；`spawn` 在 sshd 的 Job 里跑，靠上面的 breakaway / WMI 脱离。
- **首次真实断线验证**：建一个后台会话（`spawn`，跑 `Start-Sleep 600`）→ 断开 SSH → 重连 → `list` 看它还活着没 → `kill`。结果存进 `session_hosts.verified_durable`，只对当前版本有效。验证不过就是非持久（原因 `verify-failed`，详情写明）。
- **前台命令**：进程树启发（9）。**OSC 7501**：收不到（8）。**推送**：base64 行协议（10.5）。
- **attach**：控制台切到 VT 输入 / 输出、UTF-8 代码页，尺寸从 `GetConsoleScreenBufferInfo` 取。

---

## 13. 构建与分发

### 13.1 目录

```
native/sessiond/                      独立的 Cargo 工作区，不进 native/ 工作区（主工作区 exclude 了它）
  Cargo.toml                          release：opt-level 3、lto、strip，panic 保持 unwind（holder 靠 catch_unwind 隔离连接）
  ghostty.pin                         钉死的 ghostty commit 与 GitHub 源码包的 sha256（xtask 与 build.rs 共用）
  crates/ghostty-vt-sys/              bindgen 生成的 bindings.rs（入库，含布局断言）+ build.rs（只设链接路径）
  crates/ghostty-vt/                  安全封装：Terminal、回调、格式化、二进制快照、continuation
  crates/falcon-sessiond/             二进制：hold / vt / spawn / bridge / attach / list / kill / history / version
    tests/holder.rs                   真 PTY 用例（Unix），另有 #[ignore] 的语料录制器
    tests/corpus/*.bin                录下来的 PTY 原始输出（100×30）
  tools/bindgen/                      绑定生成器（要 libclang，只由 xtask 调）
```

`cargo check --workspace` 不涉及 sessiond，日常开发不需要 Zig。falcon-proto 以 path 依赖进 sessiond 工作区，继承的字段照旧从 native/Cargo.toml 解析——`FALCON_VERSION` 就是 falcon-proto 的包版本，即 native 工作区的版本。

### 13.2 钉死的东西

| 什么 | 钉在哪 | 值 |
|---|---|---|
| Zig | `xtask/src/sessiond.rs`：版本 + 四个宿主平台 tarball 的 sha256 | 0.16.0 |
| ghostty | `native/sessiond/ghostty.pin`：commit + 源码包 sha256 | `05dcf887ef51ea27821f20979493306ec3a19149` |
| 绑定 | `ghostty-vt-sys/src/bindings.rs`，文件头注明 commit | 随 pin 一起更新 |

### 13.3 xtask 任务

- `cargo xtask ghostty [--targets host|all|<三元组>,…] [--force]`：确保 Zig（下载到 `native/.cache/zig/`，比对 sha256）和 ghostty 源码（`native/.cache/ghostty/<commit>/src`，源码包比对 sha256）就绪，按目标 `zig build -Demit-lib-vt -Doptimize=ReleaseSafe -Dtarget=…`，静态库拷到 `native/.cache/ghostty/<commit>/<三元组>/libghostty-vt.a`（目录里只有静态库：同目录有 dylib 时 macOS 链接器会挑 dylib；Windows 上 zig 出的静态库叫 `ghostty-vt-static.lib`，拷成同一个名字）。Zig 的包缓存指到 `native/.cache/zig-global`。两个实测的坑：
  - ghostty 的 build.zig 用 `git describe` 取版本号，而 `.cache` 在 falcon 自己的仓库里——git 一路往上找到 falcon 的仓库和它的 tag，ghostty 当成"打了 tag 的发布版"去核对版本号直接 panic。用 `GIT_CEILING_DIRECTORIES` 挡在源码目录的上一层；
  - macOS 的 Zig 目标带上最低系统版本（`aarch64-macos.11.0`、`x86_64-macos.10.15`），与 Rust 的部署目标对齐，否则链接器对每个目标文件报 "built for newer macOS version"。
- `cargo xtask ghostty-bindgen`：用 bindgen（macOS 用 CommandLineTools 自带的 libclang）从 pin 对应的头文件重新生成 `bindings.rs`。只在升 pin 时跑，diff 就是 ABI 变化。
- `cargo xtask sessiond [--targets …] [--check] [--test]`：本机目标用系统链接器（macOS），其余用 cargo-zigbuild。用 rustup 的 stable 工具链（PATH 上排前面的 Homebrew rustc 没有交叉目标的标准库）。`--check` 每个目标 `cargo check --all-targets` 加 `-D warnings`（单独的 target 目录，不冲掉正常构建的缓存）——cfg(windows) 的代码只有交叉 check 才看得到；`--test` 跑本机目标的测试。产物 `native/target-sessiond/<三元组>/release/`。
- `cargo xtask server`：先 `sessiond --targets all`，gzip 到 `target-sessiond/embed/`，再带 `embed-sessiond` 编服务端。CI（`.github/workflows/package.yml`）装好 5 个 rustup 目标与 cargo-zigbuild。

### 13.4 实测（2026-10-13，macOS arm64 构建）

- Zig 0.16.0 编 `05dcf88` 的 libghostty-vt：每个目标 25–65 秒，五个目标约 3 分钟。
- **不需要链 libc++**：ghostty 的 C++ 依赖（simdutf / highway）以 no-libcxx 模式编进了静态库，pkg-config 的 `Libs.private` 为空。（原型时以为要 `-lc++`，是误判。）
- 5 个目标都链接通过：x86_64 / aarch64 linux-musl 静态链接（3.7 / 3.2 MB），x86_64 / aarch64 macOS 只依赖 libSystem（2.9 / 2.5 MB），x86_64 Windows-gnu（3.7 MB）。musl 产物在 alpine 容器里（两种架构）跑 `version` 成功。
- zig 带的 lld 对 rustc 缺省传的 `-Wl,-O1` 回一句 "ignoring deprecated linker optimization setting"，rustc 报成警告：二进制入口处 `#![allow(linker_messages)]`。

---

## 14. 切换（不兼容旧会话）

用户明确不需要旧会话的兼容（2026-10-13），并要求**无论 Windows 验证与否都删掉 Zellij**。所以没有并存期，一个版本里整体换掉；Windows 上验证不过的子路径标非持久（不再有 Zellij 可退）。

- **升级后的旧会话**：库里的旧行（Zellij 建的）照常走接回，找不到 `<sid>.json` 与 `.exit`（本机）或 bridge 以退出码 3 结束（SSH），落到 `dead(session-gone)`，由用户在界面上关掉。不写专门的迁移代码（有用例：库里有一行 Zellij 时代的持久会话，接回后是 `dead(session-gone)`）。
- **宿主机上的 Zellij 残留不清理**：旧会话里的程序还活着，`<root>/bin/` 下的 zellij 二进制与配置目录都留着，用户想救可以自己 `zellij attach mj-<16hex>`，用完自己结束。
- **发版说明**写明：升级会结束全部会话（进程还在宿主机上）；原生客户端要与服务端同时升级（7 的版本错配）；降级的后果（见下）。
- **降级到旧版 falcon**：守护进程会话被当成 Zellij 会话去接回并失败，显示为 `dead(session-gone)`。进程仍在宿主机上，可以用 `falcon-sessiond attach` 自救。
- **已删除**：服务端的 `zellij/`、`sessions/ssh_zellij.rs`（改写成 `ssh_host.rs`）、`sessions/scroll_plugin.rs`、`assets/falcon-scroll.wasm`、`captured_history` / `normalize_captured`、Zellij 的安装通道与下载镜像、`zellij_hosts` 的读写；客户端的 WS `scroll`、滚动位置探测与定时器、安装对话框的镜像输入框与手动安装段；`native/zellij-plugin`、xtask 的 `zellij-update` / `zellij-plugin`；ADR 0001 与 0019 加了后记；CONTEXT.md、CLAUDE.md、README 里相关的段落已改；e2e 改走守护进程。
- **保留**：非持久会话的 RingBuffer、`term_modes` 回放前缀、`OscColorGate`、客户端的查询回答。

---

## 15. 验证

| 层 | 内容 | 结果 |
|---|---|---|
| falcon-proto 纯函数 | 帧编解码往返（逐字节喂）、未知帧与未知字段跳过、超长帧拒收、握手协商、spec 的 base64url、sid 校验、`socket_path` 的长度规则、管道名；`ProgramStatusStore` 按规范逐条（整条替换、kind / progress 的适用范围、子树清除不误伤前缀、app 继承、256 上限淘汰、提示符清 working / blocked、seen、RIS）、摘要排序、显示文本消毒 | 通过 |
| ghostty-vt 封装 | 查询经 WRITE_PTY 回答且顺序对（7501 `?` → DA → XTVERSION）、深浅与默认色、7501 / 133 / 标题 / RIS 事件、continuation 重建半截序列与半个 UTF-8、快照往返与备用屏 | 通过（macOS、alpine aarch64） |
| sessiond 单测 | `CSI ? u` 回复过滤、16 色改写、内部帧往返、`replay` 期间不答查询、备用屏导出、spec 文件只读一次 | 通过 |
| sessiond 真 PTY 用例 | spawn → Sync → 输入 → 退出（`Exit` 带退出码、`.exit` 留下、bridge 退出码 5 / 3）；没有客户端时 VT 进程替程序答 DA；kitty 查询不答、DA 先到；程序状态存储、新连接拿全集、133 A 清 blocked、StatusSeen；`kill -9` VT 进程后会话存活、快照里崩溃前后的输出都在；60 秒内崩 3 次进入降级、降级后照常转发；热升级换掉 VT 进程且备用屏状态还在；最近操作的连接决定尺寸、断开后回落；`kill` 子命令；bridge 的前导标记、`--create`、stdin 关闭后会话还在 | 10 个全部通过（macOS；alpine aarch64 容器里同样通过） |
| 接回比对语料 | `tests/corpus/`（shell 颜色与 CJK / emoji / ZWJ / 国旗 / 组合字符 / 真彩下划线 / 250 字符软折行 / OSC 8、vim 备用屏带鼠标模式、less、git log --graph、top、清屏、bracketed paste 等模式）各自「直接喂 alacritty」与「VT 导出后再喂」逐行比较文字、软折行、样式、光标、模式，备用屏下再比主屏历史；另有切点落在每一个字节上的续接用例、末尾空行与关自动换行的用例 | 通过（补回末尾空行之前 less / top 两份对不上，见 6.2） |
| falcon-server | 原有 669 个用例；新增守护进程会话：从快照回放给第二个 Viewer、退出判定、后端重启后接回且历史还在、终止后宿主机上不再有活的 holder、Zellij 时代的行接回后 `dead(session-gone)`、程序状态推给 Viewer 并进列表摘要；`session_hosts` 的部分更新、授权只抄一次 | 通过（守护进程那几个在没编守护进程时跳过） |
| 端到端 | `FALCON_E2E=1` 的 falcon-client e2e：起服务端 → 建会话 → 回显与 resize → 设密码 → 两次重启（REST 401 / socket 4401 各自动重登一次）后接回 → Terminate | 通过（2 秒） |
| SSH | 隔离的 sshd 容器（alpine aarch64、bash 登录 shell，`.profile` / `.bashrc` 往 stdout 打字）：未授权非持久 → 授权后探测 / 上传 / 握手 → 建会话（尺寸、TERM、rc 的噪音没混进帧流）→ 「重启后端」后接回且历史还在 → 前台探测 → 退出 → 终止后远端没有活的会话；另一个容器把 `~/.falcon` 挂成 noexec：报 `verify-failed`，会话照样建（非持久） | 通过（`FALCON_SSH_TEST` / `FALCON_SSH_TEST_NOEXEC` 的 #[ignore] 用例） |
| 界面 | 原生客户端 Metal 截图：持久会话的终端正常；发 OSC 7501 blocked 后侧栏与标题栏出现「授权」徽标、弹出带会话名与消息的提醒 | 看过 |
| Windows | 真机手测清单：本机后端在 Job 内外、breakaway 与 WMI 两条路、SSH 推送（base64 行）与 bridge（.NET Process 继承句柄、二进制不被改写）、首次断线验证、ConPTY resize、前台命令、命名管道权限与 PIPE_BUSY、attach 的控制台模式 | **未做**：只有交叉编译与零警告检查 |

录语料：`cargo test -p falcon-sessiond --test holder -- --ignored record_corpus`（在 sessiond 工作区里，`CARGO_TARGET_DIR=../target-sessiond`）。

实测踩到、写进用例的坑：

- alpine 的 `/bin/sh` 是 busybox ash：画完提示符会发 `CSI 6n` 问光标位置，等回答期间到达的输入会被它吞掉（"echo hello" 整段没了）。回答由 VT 进程给、往返要一点时间——测试里等提示符后再留 300 ms。真人打字不会这么快，不影响使用；也顺带证明了 VT 进程在替程序答 CPR。
- 测试脚本里用 `printf` 发 OSC 7501 后，前台立刻回到 shell，holder 按规范把 blocked 清掉了——发报告的程序得一直占着前台。

---

## 16. 分期与现状

| 期 | 内容 | 现状 |
|---|---|---|
| **P0 工具链** | xtask 的 `ghostty` / `ghostty-bindgen` / `sessiond`；`ghostty-vt-sys` 与 `ghostty-vt`；5 个目标交叉编译链接 | 完成 |
| **P1 守护进程（Unix）** | holder / VT 进程 / spawn / bridge / attach / list / kill / history / version；协议；接回比对语料 | 完成 |
| **P2 守护进程（Windows）** | ConPTY、命名管道与 DACL、breakaway → WMI、进程树前台、attach 的控制台 | 代码完成、零警告；**真机未验证** |
| **P3 切换** | 服务端接入四类宿主机、客户端改造、删除 Zellij | 完成（Windows 两类走的是未验证的 P2） |
| **P4 程序状态** | 8 节全部，客户端徽标与提醒 | 完成 |
| **P5 VT 热升级** | 2.3 | 守护进程侧完成并有用例；服务端在接回时按 ghostty commit 触发 |

客户端的 kitty 键盘（CSI u 编码）不在本计划内。

---

## 17. 风险

| 风险 | 对策 |
|---|---|
| libghostty-vt 的 C API 升级时变动 | pin 加入库的 bindings，升级是一次有意的、可 diff 的动作；安全封装只覆盖用到的部分；交叉编译时布局断言逐目标核对 |
| libghostty-vt 崩溃 | 两进程模型（2.1），最坏情况只丢画面历史 |
| 接回保真度 | 语料比对作为回归门禁；6.2 的已知差异清单 |
| 不回答 `CSI ? u` 让 kitty 键盘用不了 | 与 Zellij 时代的体验一致；等客户端实现 CSI u 后放开 |
| 同一台远端被多个 falcon 共用 | owner 隔离，只清理自己的孤儿 |
| SSH 的 `MaxSessions` | 每个会话仍是一条 channel，与 Zellij 时代相同；前台探测不再额外 exec，反而少了 |
| 升级即结束全部会话 | 用户已接受；发版说明提前写明；宿主机上的进程还在，可以 `zellij attach` 自救（14） |
| 降级到旧版 falcon | 会话显示为 dead，但进程还在，可以 `falcon-sessiond attach` 自救 |
| 没有回退开关 | 守护进程出问题只能降版本；所以守护进程的真 PTY 用例与语料、服务端的守护进程用例、e2e、隔离 sshd 用例是硬门禁 |
| **Windows 未经真机验证** | 切换不等它（用户要求）；验证不过的子路径会被标成非持久而不是静默失效（spawn 失败 → 非持久，远端还有一次真实断线验证）。上线前按 15 的清单在真机上走一遍 |
| 开发时守护进程与服务端版本号相同、内容不同 | 文件名按内容哈希，新编的会被推上去；但没重编就还是旧的——改了守护进程记得 `cargo xtask sessiond` |

---

## 附录 A：原型记录（2026-10-13，macOS arm64，release）

原型代码不进仓库，结论都已写进上文：

- **接回往返**（libghostty-rs 钉的 `3425025`）：开启 `unwrap` 后文字、软折行、光标、bracketed paste、SGR 鼠标、拖拽上报、备用屏全部一致；退出备用屏后主屏的 306 行历史一致；ZWJ 组合 emoji、国旗、组合字符一致；差异见 6.2。实施时在 `05dcf88` 上用语料复跑，另外发现了末尾空行被省掉的问题（6.2 第 2 条）。
- **规模**：1 万行历史时，二进制快照 306 KB、约 0.1 ms；VT 导出 121 KB、约 0.5 ms；二进制快照解码约 0.25 ms。
- **OSC 7501**（自维护的绑定 + `05dcf88`）：按 Claude Code 的顺序发 `OSC 7501;?`、`CSI c`、`CSI > q`，依次得到 7501 的支持声明、DA、XTVERSION；working 带进度、blocked 带 kind 与 base64 文本、分层 id、`clear` 都正确；`state=bogus` 整条丢弃；OSC 133 `A` 触发 `PROMPT_START`；msg 的 base64 坏掉时 ghostty 保留这条报告、只把 msg 置空（规范 0.4 写的是整条丢弃）。
- **默认查询回复**（未装回调）：

  | 查询 | 回复 |
  |---|---|
  | DSR 6n | `CSI 3;5R` |
  | DSR 5n | `CSI 0n` |
  | DECRQM 2004 / 2026 | `?…;2$y` |
  | `CSI ? u` | `?0u`（因此必须过滤） |
  | DA2 | `>1;0;0c` |
  | DA3 | `DCS !\|00000000 ST` |
  | OSC 4;1 | 默认调色板 |
  | OSC 10 / 11 | 不答；设了默认色后才答 |
  | `CSI ? 996 n`、XTWINOPS、XTGETTCAP | 不答 |

- **continuation API**（`ghostty_terminal_continuation_*`）：实施时才发现的现成能力，导出解析器停在半截序列 / 半个 UTF-8 字符时补齐用的字节，省掉了自己写转义状态机。
