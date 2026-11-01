# 兼容 herdr-core：会话守护进程内置 hc 协议网关，每台机器新增「无项目」分组

`falcon-sessiond`（ADR 0020）和 `herdr-core`（`hc` CLI 与 `herdr-app` 背后的终端内核）在底层共享大量设计：一会话一 holder、libghostty-vt 状态机、OSC 7501 / 133、接回时的 VT 快照。为了能用 `hc` 命令行、Agent 技能（`hc ls` / `hc read` / `hc send` / `hc wait` / `hc attach`）以及 `herdr-app` 桌面端直接查看、操作乃至新建 falcon 所管机器上的会话，需要让 falcon 兼容 `herdr-core`。

详细设计见 [`docs/design/herdr-core-compat.md`](../design/herdr-core-compat.md)。

## 决定一：方向是「让 falcon 会话对 hc / herdr-app 可见可控」，不是用 hc 替换 falcon-sessiond

兼容有两个方向：

- **反向（让 falcon-server 改调 `hc`）**：需要把 `herdr-core` 的发布二进制连同 `falcon-sessiond` 一起打进 5 个目标的分发包或要求远端联网安装；Windows 宿主机的 ConPTY / WMI 脱离、VT 热升级（`UpgradeVt`）、`StatusSeen` 已读状态等 `falcon-sessiond` 已有语义都要往 `hc` 里迁；还得在宿主机上多跑一层进程。为兼容外部工具把自己的持久底座换掉，得不偿失。
- **正向（`falcon-sessiond` 直接说 `hc` 协议）**：PTY 仍由 `falcon-sessiond hold` 持有，宿主机上只需多跑一个轻量的 `falcon-sessiond hc serve` 网关，把该机器 `<root>/sessiond/sessions/` 下的全部会话按 `herdr-core` 的 JSON-RPC / Framed 协议暴露出去。对 `hc` CLI 和 `herdr-app` 而言，这台机器就是一个标准的 `herdr-core` 实例（或远端 machine）。

所以选正向：内部协议与 holder 不动，外部工具零改动直接连上来。

## 决定二：协议网关放在宿主机侧的 `falcon-sessiond hc` 里，而不是 `falcon-server` 里

如果只在 `falcon-server` 上开一个 `hc` socket：

- 用户在远端机器上直接敲 `HC_SESSION=falcon hc ls` 用不了；
- `herdr-app` 或本机 `hc` 想经标准 SSH（`hc machine add box --host user@box --via '~/.falcon/bin/falcon-sessiond hc connect --'`）直连某台远端宿主机时，如果网关在 `falcon-server` 上就必须绕经 `falcon-server`，没法直接走 SSH；
- `falcon-server` 重启期间，本机和远端的 `hc` 连接会全部断开——这违背了会话守护进程「服务端不在时会话照常可接」的初衷。

把网关做成 `falcon-sessiond` 的子命令（`hc serve` / `hc connect` / `hc status`）：

- 本机与每台 POSIX 远端宿主机用的是同一个二进制，随 `deploy.rs` 一起到位；
- 同时维护固定的符号链接 `<root>/bin/falcon-sessiond` 指向当前哈希命名的二进制，让用户的 `--via` 命令在 falcon 升级后无需改动；
- 网关挂了只影响 `hc` 视图，holder 里的会话毫发无伤。

代价是 `falcon-sessiond` 引入了 `tokio`（`current_thread` 运行时，只给 `hc` 子命令用，`hold` 与 `vt` 仍保持原有的同步线程模型不变），二进制体积增加约 300–500 KB。

## 决定三：本机默认占独立会话名 `falcon`，绝不触碰用户的 `default` herdr-core

同一台开发机上用户往往同时跑着 `herdr-app`（占着 `~/.local/state/herdr-core/default/default.sock` 与 `core.lock`）。如果 falcon 去抢 `default`：

- 先启动的一方拿着 `core.lock`，后启动的一方要么起不来、要么把对方的 socket 删掉；
- 停止或重启网关时会误杀用户在 `herdr-app` 里的本机终端。

因此：

- `falcon-sessiond hc` 的默认会话名定为 **`falcon`**（当 `--root` 不是默认的 `~/.falcon` 时，用 `falcon-<fnv8(root)>` 区分多个实例），socket 落在 `$XDG_STATE_HOME/herdr-core/falcon/falcon.sock`（或 `$XDG_RUNTIME_DIR/herdr-core/falcon.sock`）；
- 网关启动时对同目录的 `core.lock` 取非阻塞排他锁（`flock(LOCK_EX | LOCK_NB)`）并写入自己的 socket 路径，防止真 `herdr-core` 把 socket 删掉；若锁已被真 `herdr-core` 占住，直接报错提示，绝不强抢；
- 用户既可以用 `HC_SESSION=falcon hc ls` 直接看，也可以用 `hc machine add falcon --via '~/.falcon/bin/falcon-sessiond hc connect --'` 把它挂进自己的 `default` core，在 `herdr-app` 里和本机终端同屏使用。

## 决定四：一期做齐三档 `hc` 能力（查看 / 操控 / 新建），首帧与查询过滤对齐 hc 规范

外部消费者分两类：`herdr-app`（走 Framed 协议：`hello`、`events.subscribe`、`terminal.list/attach/kill/rm/run`、`doc.get/set`）与 Coding Agent 的 `hc` 技能（走 `terminal.list/get/read/send/keys/interrupt/wait/label/keep/run`）。只做半截会让 Agent 技能在 `hc send` 或 `hc wait` 时报错，因此三档能力一次做齐：

- **连接与发现**：Plain JSON-RPC 与 Framed 双模式（`[channel u32 LE][kind u8][len u32 LE][payload]`），256 KiB 初始信用窗口、4 MiB 积压丢弃并重绘首帧、15 秒事件流心跳（防 `hc` 的 45 秒静默断线）；`hello` 报 `features: ["framed", "attach", "events", "status", "shell"]`。
- **首帧与防双答**：`terminal.attach` 首帧按 `hc` 规范拼装（`0x18` + 复位前导 + VT 快照 + OSC 2 标题 + `?9001l` + 重编码的当前 OSC 7501 记录 + continuation）；实时输出流经过查询过滤器（剥掉已被 VT 进程答过的 DA / DSR / DECRQM / OSC 4/10/11/12 / XTVERSION / Kitty 键盘查询），避免下方真终端再答一遍变成乱码输入。
- **Agent 操控**：`terminal.send` 实现完整的 3 秒输出静默检测、blocked 状态拒绝（除非 `force`）、按 holder 的模式位自动包裹 bracketed paste（`?2004h`）、200 ms 后补发回车；`terminal.keys` 按 DECCKM（`?1`）状态查表转换命名按键；`terminal.wait` 监听状态 / turn / 退出变化。
- **默认合成 `herdr.space` / `herdr.tab` 标签**：让 falcon 的项目与会话在 `herdr-app` 里直接显示为对应的 Space 与 Tab（可在设置「Herdr」页关掉）。在 `herdr-app` 里关掉 pane 会调 `terminal.kill`，对应结束该 falcon 会话。

## 决定五：每台机器新增内置「无项目」分组，统一承载独立终端与 hc 新建的会话

过去 falcon 要求所有普通会话都挂在某个项目文件夹下（只有 AI 助手线程用了隐藏的 `machine:local` / `machine:host:<id>` 合成 owner）。这带来两个问题：

1. 用户只想在某台机器上随手开一个不绑项目的终端（比如看看日志、配个环境），却非得先建一个项目；
2. 用户从 `hc run` 或 `herdr-app` 在这台机器上新建的终端（`terminal.run` / `terminal.put`）没有对应的 falcon 项目，falcon-server 不知道该把它挂在哪。

做法是在每台机器（本机与每台已保存的 SSH Host）下内置一个**「无项目」分组**，使用合成项目 ID `standalone:local` 与 `standalone:host:<hostId>`：

- **服务端与协议零分叉**：`sessions/machine.rs` 的 `owner_row` 同时解析 `standalone:*`（工作目录指向该机器家目录 `~`）。建会话照走 `POST /api/projects/{id}/sessions`，浏览文件照走 `/api/projects/{id}/files`（自动看该机器家目录），只有 Git 面板对 `standalone:*` 直接返回 `NoWorkingDir`（不把整个 `$HOME` 当仓库扫）。
- **侧栏直接一层展开**：在每台机器的分组下画一行「无项目」（不套多余的 `文件夹 → 分支` 两层，因为它没有 Git 分支），带 `+` 按钮可直接新建终端或 Agent 会话。
- **自动收养外部会话**：从 `hc` / `herdr-app` 经 `terminal.run` 新建的 holder 会话，`owner` 记为 `hc:<core-id>`（避免被 `falcon-server` 的孤儿清理误杀）。`falcon-server` 在列出会话时发现尚未入库的 `hc:*` 会话，若其标签带有效的 `falcon.project` 则归入该项目，否则自动收养进该机器的 `standalone:*`「无项目」分组，两边实时互通。

## 决定六：远端 SSH 会话自动在本机 `<data_dir>/sessiond/` 下做 Unix Socket 代理

当用户的 `herdr-app` 或 `hc` 只连接了运行 `falcon-server` 的这台机器（例如 `hc machine add falcon`），用户希望无需把 `falcon-server` 管理的每台 SSH 服务器再逐一配进 `hc machine`，就能在 Herdr 里直接看到、挂接乃至在这些远端 SSH 主机上新建会话。

做法是在 `falcon-server`（Unix 平台）挂接远端 SSH 持久会话时，在本机 `<data_dir>/sessiond/` 下为该会话投影一个本地代理：

- **本地代理 Socket 与元数据**：`falcon-server` 绑定 `<data_dir>/sessiond/run/<sid>.sock`，写出 `<sid>.json`（`pid = falcon-server pid`，`shell_pid = 0` 标识为远端代理）与 `<sid>.labels.json`（含 `falcon.remote = "true"`、`falcon.host`、`falcon.host_name`）。本机的 `falcon-sessiond hc serve` 扫描目录时天然发现它，把 `where` 合成为 `<host_name> · <project_name> / <session_name>`。
- **按需桥接**：当 `falcon-sessiond` 连上本地 `<sid>.sock`（无论是后台状态监控还是 `terminal.attach`），`falcon-server` 在已有的 SSH 连接上开一条运行远端 `falcon-sessiond bridge --sid <sid>` 的通道，剥掉 `FALCONBR\n` 前导后在本地 UnixStream 与远端 SSH Channel 之间双向透传字节。
- **远端新建会话委托（`server-hc.sock`）**：`falcon-server` 监听 `<data_dir>/sessiond/server-hc.sock`。当 `hc` 调 `terminal.run` 并通过标签指定了远端主机或项目（`falcon.host`、`falcon.host_name`、`falcon.project`、`herdr.space = falcon.<id>`）时，本机 `falcon-sessiond` 将创建请求委托给 `server-hc.sock`，由 `SessionManager::create_session` 在目标 SSH 主机上拉起持久会话并同步建立本地代理。

## 已知的不足

- **只支持 macOS 与 Linux 宿主机**：Windows 宿主机上 ConPTY 吞掉 OSC 7501，`falcon-sessiond hc` 在 Windows 上直接报不支持。
- **不做 machine 级联与文件 RPC**：`machine.*`、`file.*` 回 `unknown_method`，`server.stop` / `server.restart` 回 `refused`（防止 `hc server stop` 把网关关掉导致真 `hc` 抢占 socket）。
- **忽略 viewer 的 `FACTS` 颜色上报**：会话的深浅与默认色仍以 falcon 主题为准，避免外部 `hc attach` 一接上就把 falcon 画布里的程序刷成别的配色。
