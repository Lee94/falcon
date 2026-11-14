# AGENTS.md

This file provides guidance to Codex (claude.ai/code) when working with code in this repository.

## 命令

整个仓库只剩纯 Rust：`native/` 是一个 Cargo workspace，服务端 `falcon-server` 与客户端（NSH 跨端界面 `falcon-nsh`，六端统一入口 `falcon-app`：macOS / Windows / Linux GTK4 / Android / Web WASM / TUI）都在里面。构建 / 打包 / 资源生成任务在 `native/xtask`（`cargo xtask`，别名在 `native/.cargo/config.toml`；不在 `native/` 下就 `cargo run --manifest-path native/Cargo.toml -p xtask -- <命令>`），仓库里没有 Node / JS / Gradle / Kotlin 依赖。

```bash
cd native
cargo xtask --help          # 全部任务
cargo run -p falcon-server  # 开发服务端（4923，默认数据目录 ~/.falcon——那是日常在用的实例，开发另起要带 --port / --data-dir）
cargo xtask server          # 服务端发布单文件（把 sessiond 编进去），产物 release/falcon-v<版本>-<平台>
cargo build --release -p falcon-app       # 原生客户端 release 构建（macOS / Windows / TUI；Linux 桌面加 --features linux-desktop）
cargo xtask tui             # 在当前终端启动 falcon-app TUI 工作台（nsh-platform-tui）
cargo xtask pkg             # macOS 安装包：Falcon.app = NSH 原生客户端 + Resources 里的服务端
cargo xtask win             # Windows 安装包（Inno Setup）：NSH 原生客户端 + 同目录服务端（装好注册计划任务）；须在 Windows 上打
cargo xtask linux           # Linux 服务端安装文件：.deb / .rpm / install.sh（只含服务端 + systemd 服务）
cargo xtask linux-desktop   # Linux GTK4 原生桌面客户端安装包：Falcon-v<版本>-<平台>.deb / .tar.gz
cargo xtask android         # Android 安装包（纯 Rust NSH + Android SDK d8/aapt2/apksigner，零 Gradle）
cargo xtask web             # Web 静态站点打包（纯 Rust wasm32-unknown-unknown + nsh.js，产物 release/web/）
cargo xtask nsh-check       # 局部检查 falcon-nsh 与 falcon-app（含测试目标）
cargo xtask sessiond        # 会话守护进程 falcon-sessiond（ADR 0020，独立工作区 native/sessiond），产物 native/target-sessiond/<三元组>/release/；开发构建的服务端从这里拿，`--targets all` 交叉编 5 个目标，`--check` 零警告检查（含 Windows），`--test` 跑它的测试
cargo xtask ghostty         # 钉死版本的 Zig 编 libghostty-vt 静态库（sessiond 会自动调），源码先打上 `native/sessiond/ghostty-patches/` 的补丁（改补丁要同步 ghostty.pin 的 patches=）；`ghostty-bindgen` 只在升 ghostty 的 pin 时跑
```

xtask 调的外部工具都不是 JS：rsvg-convert（图标、pkg 的 AppIcon）、macOS 的 pkgbuild / iconutil / codesign、Inno Setup 的 ISCC、Android SDK 的 javac / d8 / aapt2 / zipalign / apksigner、curl（Zig 与 ghostty 源码包，都钉了哈希）、patch（给 ghostty 源码打补丁）、`cargo xtask sessiond` 要 rustup 装好 5 个目标（x86_64 / aarch64 的 linux-musl 与 apple-darwin、x86_64-pc-windows-gnu）与 `cargo install cargo-zigbuild`（Zig 本身由 xtask 下到 `native/.cache/zig/`）。发布版本号就是 `native/Cargo.toml` 的工作区版本。

### 验证：本地局部检查，GitHub 发版执行全量检查

本地提交与 push 只执行和改动相关的局部测试或编译检查，不要求运行全工作区的 `cargo check` / `cargo test`，也不默认打各端安装包。Rust 按修改的 crate / 模块检查（客户端改动可运行 `cargo xtask nsh-check` 与 `cargo test -p falcon-nsh` / `cargo test -p falcon-app`）；仅修改文档或工作流时检查差异格式与工作流语法。用户明确要求全量验证，或局部检查发现需要扩大排查的问题时，再执行额外检查。

全量编译检查与单元测试由 `.github/workflows/package.yml` 在 GitHub 发版流程中执行，失败则禁止发布 GitHub Release；手动触发与定时运行同样执行。主工作区检查使用 release profile，与安装包构建共用依赖缓存；独立的 sessiond 工作区经 xtask 检查全部五个目标并在 runner 本机执行测试。没有 clippy / rustfmt 要求，源码保持零警告。

```bash
cd native
cargo check -p falcon-server --tests                # 本地：检查改动涉及的 crate
cargo test -p falcon-server --lib sessions::manager   # 本地：只测试相关模块；--test-name-pattern 那套换成 cargo 的名字过滤
cargo check --release --workspace --tests --locked  # GitHub 发版：全量编译检查
cargo test --release --workspace --locked           # GitHub 发版：全量测试；别断言 serde_json::Map 的遍历顺序
```

服务端的测试除了纯函数层（命令构造、路径运算、模式跟踪、DB 行合并），还有用真 PTY 跑的 SessionManager 用例（持久会话的那几个起真的守护进程，没跑过 `cargo xtask sessiond` 时跳过）、`oneshot` 打进 axum 的路由用例、进程内 russh 服务端上的传输用例。守护进程自己的测试在它的工作区里（`cargo xtask sessiond --test`：真 PTY 的 holder 用例、VT 进程崩溃 / 降级 / 热升级、接回比对语料 `tests/corpus/`）。真机链路另有几道：`FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored`（起真服务端 + 守护进程，不设访问密码，两次重启后接回）；SSH 上的守护进程会话对**隔离的** sshd 容器跑 `FALCON_SSH_TEST=host:port:user:key cargo test -p falcon-server --lib ssh_durable -- --ignored --test-threads=1`（设计文档 `docs/design/session-daemon.md` 第 15 节）；`cargo xtask fixtures` 重生协议 fixture（配 `cargo xtask compare-fixtures` 可在改服务端前后比形状）。Windows 远端、浏览器里的真实交互（IME、剪贴板、拖放）没有自动化测试，改动那些要在真机上验。

## 架构

协议类型的唯一真相来源是 `native/crates/falcon-proto`，服务端与客户端用同一份；`tests/fixtures/` 是 `cargo xtask fixtures` 从真服务端落盘的响应，改协议就重生 fixture。VT 模式跟踪 `falcon-proto::term_modes` 也是两边共用（服务端回放前缀与客户端鼠标上报）。原 TS 实现（`packages/server`、`packages/web`、`packages/shared`）与浏览器版（`falcon-web`）已删除，ADR 与注释里提到的 `*.ts` 在提交 `9c9d045`（`packages/web`、`packages/shared`）与 `fd9022a`（`packages/server`）里。注释里的「React 版」「旧 React 版」就是指 `packages/web`。

### 数据流

原生客户端 ↔ `/ws/sessions/:id`（WebSocket）↔ `SessionManager` ↔ `Backend` ↔ shell。持久会话的 Backend 是宿主机上的会话守护进程 falcon-sessiond（本机直连它的 socket / 命名管道，SSH 经 exec 的 `bridge`）；非持久会话是后端自己开的 PTY / SSH 的 pty channel。

WS 上是混合协议：**终端字节走二进制帧**（1 字节类型头 `TERM_FRAME_OUTPUT` / `TERM_FRAME_REPLAY` + UTF-8 载荷），state / reconnecting / error 等控制消息走 JSON 文本帧。REST（`/api/*`）只管项目、主机、git、转发、会话的增删改查。

### 宿主机节点（ADR 0027）

项目与新终端会话由**所属宿主机的节点数据库与会话引擎**管理。A、C 经 SSH 连接 B 时，A/C 只聚合列表和转发业务，B 保存项目与会话；客户端布局仍按设备保存。`falcon-server/src/nodes/` 包含节点引导、SSH 部署、项目迁移和请求网关，节点身份是数据库的 `instance_id`，远端资源 ID 为 `node:<nodeId>:<base64url原ID>`。普通 REST、文件、Git 与终端 WebSocket 经网关交给所属节点，B 只把这些资源当本地项目执行；原有 SSH 执行器仍用于探测、端口转发和过渡路径。

`host-projects.json` 只在首次迁移时导入，不再双向同步。迁移先备份、保持全部项目属性与附属关系；旧 terminal 不要求迁移但不能借迁移主动结束仍在运行的任务。旧 SSH 项目保留为迁移缓存，存档清扫不能再拿这些缓存删除远端目录。含 worktree 元数据的导入端点只接受 SSH 可读取的节点令牌，普通项目编辑仍不可写删除护栏字段。

`falcon node ensure --data-dir <绝对目录>` 按需启动并复用回环节点；常规启动在打开数据库前取得节点进程锁，bind 后写 `node.json`。节点数据目录必须隔离，测试不得探测或部署到真实家目录。`falcon version --json` / `--version` 只打印版本与协议，不启动服务。

服务端升级是 `/api/system/update` 与 `/api/system/update/status`，桌面应用更新仍按 ADR 0026。SSH 节点允许由入口下载、校验后传入节点；独立帮手完成替换与健康确认。候选服务先在维护状态完成迁移，开放业务写入之后不得恢复旧数据库覆盖新写入。项目创建、会话创建与派生请求的 `Idempotency-Key` 在节点持久化；执行中断留下 pending 时返回结果未确认，不重复执行。

### server（`native/crates/falcon-server`，路径都相对 `src/`）

由 Node 版逐文件移植（ADR 与注释里提到的 `*.ts` 都在提交 `fd9022a` 的 `packages/server/src/` 里，Rust 文件名换成 snake_case），方案与实测记录在 `docs/design/rust-unification.md` §11。

- **执行模型**：会话核心（SessionManager、SshLink、中转、px0）跑在 `engine.rs` 的一条专用线程上——current_thread runtime + LocalSet，状态全是 `Rc` / `RefCell`，与 Node 事件循环同一个语义（同步段不会被打断，只在 await 处让出）。axum 处理器（多线程）经 `EngineHandle::call` 投闭包进去拿结果，WS 的 input / resize 经 `send` 保序投递。**别把这些状态改成 `Arc<Mutex>`**；RefCell 的借用只在同步小段里持有，不跨 await、不在借着时调别的方法。阻塞的 PTY 读写各开线程，SQLite 照旧同步调。HTTP 处理器被丢掉（客户端断开）不会打断引擎里在跑的写操作。
- `sessions/manager.rs` 是核心：`LiveEntry` 持有 backend、多个 Viewer（`viewer.rs`：发送端 + 未写出字节计数，即 bufferedAmount，背压 4MB / 256KB 两条水位）、`ViewerArbiter`（多 Viewer 时尺寸跟最近真操作过的、看得见的那扇窗口——客户端经 WS 报 `visibility`，生效尺寸经 `size` 推回、不定尺寸的窗口按它建网格再缩放 / 裁剪着画，ADR 0022；深浅跟最近操作的那端）、16ms 输出合并窗口、SSH 断线的指数退避重连。回放分两种：**持久会话**向守护进程要快照（`Sync`），等快照的 Viewer 标 `syncing`、期间不收输出，快照按连接上到达的顺序回来（设计文档 5.5，`on_backend_event`）；**非持久会话**用 RingBuffer（4MB）+ VT 模式跟踪的前缀。程序状态（OSC 7501）由守护进程推来，存在 entry 上、经 WS 的 `program_status` 推给界面、在会话列表上带摘要。会话状态机 `active / unverified / dead` 与 `Detach` / `Terminate` 的区分见 CONTEXT.md，别自己发明语义。
- `sessions/backend.rs` 是共同接口（守护进程才有的能力——快照、前台命令、外观、程序状态、终止、VT 热升级——是带缺省实现的方法，事件走 `BackendCallbacks.on_event`）；`local.rs`（本机：守护进程的 spawn 与直连、非持久会话的 portable-pty）/ `ssh.rs`（russh 传输层）+ `ssh_host.rs`（远端探测、守护进程的部署与 Windows 断线验证、bridge 附着、非持久的 pty channel）各实现一边。数据目录在配置解析时就转成绝对路径：portable-pty 只在 PATH 里找相对路径的程序。
- `sessiond/` 是服务端这一侧的会话守护进程（ADR 0020，设计文档 `docs/design/session-daemon.md`）：`command.rs` 纯函数（目标三元组、按内容哈希命名的二进制、各子命令 argv、Windows 上**不能经 PowerShell 的 `&` 跑 bridge**——它会改写二进制输出，见 `windows_bridge_command`）、`bin.rs`（各目标的二进制：`FALCON_SESSIOND_DIR` > 编进发布版的 gzip（`embed-sessiond`）> `native/target-sessiond`）、`deploy.rs`（推到宿主机、`version --json` 握手；POSIX 经 stdin，Windows 走 base64 行协议）、`backend.rs`（帧协议、bridge 的前导标记、快照拼 continuation）。守护进程本体在 `native/sessiond/`（独立工作区：`ghostty-vt-sys` 入库的绑定、`ghostty-vt` 安全封装、`falcon-sessiond` 的 holder / VT 进程 / spawn / bridge / attach），协议类型在 `falcon-proto::sessiond`，**holder 的协议只增不改**（旧 holder 的会话要能被新 falcon 接回）。
- `git/` 的分层（`sessiond/` 照它写）：
  - `command.rs` —— **纯函数，零 I/O，只产出 argv 数组与 env**，绝不拼命令行字符串（远端 POSIX 与远端 Windows 转义规则不同）；
  - `host.rs`（`src/host.rs`，git / 文件 / px0 / 守护进程共用）—— 抹平四种执行环境（本地 Unix / 本地 Windows / SSH POSIX / SSH Windows）为 `posix` / `windows` 两类，负责路径构造、命令行拼装与远端探测；
  - `exec.rs`（`Exec` trait）/ `git/repo.rs` —— 执行与错误分类。**执行器的铁律：非零退出码是正常返回值，绝不当 `Err`**，判错一律显式查 `res.code`；只有执行本身起不来才算链路故障。
- `git/path.rs` **不用 `std::path`**：后端跑在 Windows 上也可能在为 Linux 远端构造路径。同理后端**永远不 chdir 进 worktree**，git 命令一律带 `-C <dir>`。
- 终端查询（DA / DSR / DECRQM / OSC 10–12 …）：持久会话由宿主机的 VT 进程**一处**回答，客户端对 `durable` 会话不发 `PtyWrite`（多个 Viewer 会各答一遍、没人在看时没人答）；非持久会话照旧由客户端答，OSC 10/11/12 由服务端的 `OscColorGate` 代答。kitty 键盘的查询（`CSI ? u`）不答——客户端的按键编码只有传统 xterm 那套。滚动在客户端本地（alacritty 有真正的 scrollback），ADR 0019 的 zellij 插件已随 Zellij 一起删除。
- `git/remove.rs` 是删除**附属项目 worktree 目录**的唯一入口，护栏（静态断言 + 动态取证）在改动前先整份读完 `docs/adr/0002-worktree-derived-projects.md`。工作目录内部的文件删除在 `files.rs`（`resolve_inside` 挡在项目工作目录里，Windows 宿主机上的反斜杠 `..` 也挡），两件事不要合成一条路径。
- `sessions/agent.rs` 是 agent 会话（开场直接跑 Claude Code / Codex 等 CLI，ADR 0013）：纯函数产启动脚本与写入命令，脚本落在宿主机 `<falcon 根>/agents/`，会话的 argv 就是它；CLI 退出后 `exec` 回登录 shell，会话不跟着结束。脚本里的登录 shell 是**生成时写死的绝对路径**，绝不读 `$SHELL`（Windows 远端那条路径上 `SHELL` 就是脚本自己，会递归）。本地菜单只显示 `sessions/agent_detect.rs` 在交互登录 shell 的 PATH 中检测到的可执行文件（`GET /api/agents`，30 秒缓存与轮询）；SSH 菜单通过同一接口的 `hostId` / `projectId` 在目标宿主机检测，只显示实际存在的 CLI。新增 CLI 时同步 `SessionAgent` 与 `agent.<标识>` 文案。
- `cloudflared/` 是公网发布（ADR 0014）：`command.rs` 纯函数产 argv 与解析 Quick Tunnel URL / `/quicktunnel` JSON（**只在 falcon 后端本机 spawn `cloudflared`，不往远端装**）；`bin.rs` 按需把锁定版本下到 `<dataDir>/bin/cloudflared`（`FALCON_CLOUDFLARED_BIN` 优先，PATH 兜底）；`sessions/share.rs` 管进程生命周期，远端目标先在本机 `listen(0)` 再 `forward_out`。规则在 `host_shares` 表，公网 URL 是运行时事实不入库。v1 只做 Quick Tunnel + HTTP。
- 中转（端口转发 `sessions/forward.rs` + 公网发布 `sessions/share.rs`，两者共用的监听 / 桥接在 `sessions/relay.rs`，ADR 0016）**按机器挂，不挂项目**：转发挂 SSH Host（`host_forwards`），发布挂本机或 SSH Host（`host_shares.host_id` 为 null = 本机）。隧道走主机链路 `SessionManager::get_host_link`（与「浏览远端目录」共用），断线由 `schedule_host_reconnect` 重连，**不走项目链路**。同端口可存多条、同时只一条生效：启用时先同步落库把同槽位的其它规则置 disabled，再异步停旧起新；槽位口径（本地转发跨主机比端口、远端转发与发布在同一台机器内比）在纯函数 `sessions/relay_spec.rs`，web 的「同端口」徽标照同一口径算。
- `plugins/` 是右侧栏插件（ADR 0025，作者手册 `docs/plugins.md`）：插件交的是**声明式面板文档**（`falcon-proto::plugin` 的 `PluginView`：固定词汇的块 + 语义色调），客户端用自己的组件画，**别给插件开渲染代码的口子**（HTML / WASM 的取舍写在 ADR 决定一）。命令插件是 `<数据目录>/plugins/<id>/plugin.json`，**只在后端本机 spawn、不经 shell、argv 只来自磁盘上的清单**（不经 API 安装），请求经 stdin JSON + `FALCON_PLUGIN_*` / `FALCON_PARAM_*` / `FALCON_PROJECT_*` 环境变量交过去；`manifest.rs`（清单与白名单）、`view.rs`（逐块解析 + 裁剪，插件输出不可信，内置插件也过这一道）是纯函数，`runner.rs` 限时 / 限量、Unix 上整个进程组 SIGKILL；同一份渲染请求单飞、动作不合并。内置插件看哪台机器共用 `plugins/machine.rs`（跟随项目 / 本机 / SSH Host，远端经项目或主机链路 `remote_exec`）。系统监控在 `plugins/sysmon/`：本机走 sysinfo（macOS 上进程 CPU 要第三次刷新才有值，模块注释里有实测），Linux 远端走 `sysmon/linux.rs` 的 /proc 探测脚本（`sh -c` 跑），macOS / Windows 远端如实说不支持；采样跟着请求走、没有后台定时器。Docker 在 `plugins/docker/`：只跑那台机器上的 `docker` 命令行（`cli.rs` 纯函数：argv、POSIX 远端找 docker 的脚本、失败分类、动作白名单；inspect 模板**不取 Env**），`docker stats` 要两秒、不在画面板的路上采（`kick_stats`）。`PluginHost` 是 `Send + Sync` 的，挂在 AppState 上，不进引擎（看远端时才借引擎拿链路）。
- `px0/` 是「用 px0 审阅」（ADR 0017）：在项目宿主机上按需拉起 px0，经 falcon **同源**反代到 `/px0/<项目 id>/`（多仓库项目按成员各起一个，挂 `/px0/<项目 id>/<成员序号>/`，前缀本身是选成员页——px0 只认一个仓库；`api/px0.rs`，hyper 逐请求开连接、流式转发；与 `/api/` 同一口径，能连上就允许使用）。`command.rs` 纯函数（资产映射、**钉死的 sha256**、argv、端口解析、远端安装 / 启动命令），`bin.rs` 在后端本机下载校验、SSH 项目再经 stdin 推到远端（不让远端自己下），`manager.rs` 管实例——**本地与远端都挂在 pty 上**，后端死了 px0 跟着挂断，别改回普通 spawn / 无 pty 的 exec；远端连接直接走 forward_out，本机不另开监听端口；反代的租约随响应体释放，空闲回收据此计数。`proxy.rs` 的头过滤有讲究：剥 falcon 的 cookie、只替同源请求改写 Origin（px0 的 localPost 要 Origin == Host）、去 set-cookie。同源意味着 px0 的前端能调 falcon 全部接口——**只在本机 / 内网可接受，公网访问前先挪到独立源**。原生客户端不嵌 px0，菜单项把地址交给系统浏览器。
- `sessions/machine.rs` 解析会话 owner（`owner_row`）：真项目查库；无项目会话是合成 id（本机 `standalone:local`，SSH Host `standalone:host:<id>`），合成一条 ProjectRow。会话路径上要 owner 一律走它，别直接 `db.get_project`。旧库里可能还留着已删除的 AI 助手对话终端（owner 前缀 `machine:`），`is_machine_owner` 把它们挡在会话列表和文件接口外面。设置里的「网络」（tsnet / NetBird sidecar）也已删除。
- `api/` 是 HTTP 面（axum）：路由路径、请求 / 响应体、状态码、错误形状（`{ error }` 与 Fastify 的 500 形状，`api/error.rs`）都照原 Node 版。请求体用 `LenientJson` 收、按 JS 口径逐字段取（`api/input.rs`），不先反序列化成强类型——那会把"某个字段类型不对"变成整条请求 400。`/api/*` 与 `/px0/*` 仍经过 `require_login`，但账户门禁已去掉，请求直接放行。各自验身份的（原始字节令牌、应用图标取图、askpass helper）挂在 `public_router`。
- `db.rs` 用 rusqlite（bundled SQLite），与 Node 版同一个 `falcon.db` / `secret.key` 格式。外键约束显式关闭，级联在应用层手写；`migrate()` 是幂等的 `CREATE TABLE IF NOT EXISTS` + 加列，没有版本号迁移表——改表结构就往这套里加。
- Windows 远端的所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。
- 测 SSH 只用隔离的 sshd（Docker 容器、或 `SetEnv HOME=` 指到临时短路径），别对自己真实的家目录跑 probe / 部署；开发实例端口用 4940–4999、数据目录用短路径（守护进程的 socket 上限 104 字节，超了会落到 `/tmp/falcon-sd-<uid>-<哈希>/`），从 falcon 终端里起要 `env -u FALCON_SESSION_ID`。开发构建的服务端用 `native/target-sessiond` 里的守护进程：改了守护进程要重跑 `cargo xtask sessiond`（远端用的那几个目标加 `--targets all`），否则宿主机上跑的还是旧的（同版本号不会触发重推，文件名按内容哈希，所以新编的会被当成新文件推上去）。

### client（`native/crates/falcon-nsh` / `falcon-platform` / `falcon-app` 及其下层 crate）

纯 Rust + **Native Shell (NSH, `longbridge/nsh`)** 写的六端统一客户端（ADR 0028：macOS AppKit、Windows Direct2D、Linux GTK4、Android 原生视图、Web WASM、终端 TUI），连 falcon 服务端（REST + `/ws/sessions/:id`）。能连上就允许使用，没有登录页。界面全在 `falcon-nsh`，平台宿主能力（偏好与工作区持久化、本机服务地址与 launchd / schtasks 生命周期、钥匙串 / 凭据管理器密码、应用图标、提示音、深链 `falcon://`、应用内更新）通过无框架依赖的 `falcon-platform` `Platform` trait（`falcon_platform::current()`）提供；平台实现与各端编译入口在 `falcon-app`（含旧版 Android `falcon_android_prefs.xml` 首启无损迁移）。别把访问密码或登录页加回去。应用更新见 ADR 0026：设置「关于」与菜单栏检查 `Lee94/falcon` 的 GitHub Release；macOS 替换 `Falcon.app` 后按原参数 `service install`，再经正常退出重新打开新版本（没保存的文件修改会先问）。版本比较与换入在 `falcon-core` 的 `app_update.rs`。

```bash
cd native
cargo run -p falcon-app                         # 桌面端连本机服务（按已装 LaunchAgent 的端口，没装是 4923）；FALCON_LOCAL_URL 可改指别的实例
cargo xtask tui                                 # 在当前终端以 TUI 模式启动同一套工作台
```

- **crates.io 走 `native/.cargo/config.toml` 里的 rsproxy 镜像**：这台机器上 Clash 的 fake-ip 把 index.crates.io 解析坏了。网络正常的机器删掉那一段即可。
- 分层：`falcon-proto`（协议类型）→ `falcon-client`（REST / WS，与执行器无关的 future，自带 tokio 运行时）→ `falcon-term`（alacritty_terminal + 从 Zed 抄的按键编码 + 鼠标 / 滚轮 / 模式跟踪）→ `falcon-theme`（主题系统：Ghostty 主题文件即数据模型、浅深双槽位、整套界面色由一套主题派生，ADR 0006 / 0011）→ `falcon-core`（与 UI 框架无关的纯逻辑：列式工作区排布 `layout.rs`、窗口 key `pane_key.rs`、会话标题 `session_title.rs`、快捷键、文件路径 / 搜索 / 树、git 图等；`tests/vectors/` 是回归向量）→ `falcon-platform`（零 UI 依赖的宿主服务抽象）→ `falcon-nsh` / `falcon-app`（只有这两个依赖 NSH）。**纯逻辑一律放下层 crate**，渲染层升级只波及最外两层。
- 主应用全纯 Rust（`nsh-engine` 关闭 `js-core`，主路径零 QuickJS 开销）；Zed 的终端按键编码（`falcon-term/src/keys.rs`）与 `native/vendor/alacritty_terminal`（裁掉 46MB 测试数据的入库副本，关闭 `tty` 支持 `wasm32`）保留原样，`native/` 因此是 GPL-3.0-or-later。
- 文案：`t!("key")`，**界面上不允许硬编码中文**。文案统一在 `falcon-nsh/locales/zh-CN.json` 一份（v1 只有中文，直接改它），原生 / 平台独有的挂在 `native.<区域>` 下。
- 界面色**只准用 falcon-theme 派生出的语义 token**，不许写死颜色；新增语义色去 `falcon-theme/src/derive.rs` 加。派生规则有冻结的金标准 fixture（`tests/derive_golden.rs`），有意改规则要连 fixture 一起改并写清楚。**深浅按主题底色亮度切，不按系统明暗**。内置主题 = Falcon 两套 + Ghostty 全部 463 套（`falcon-theme/data/`，`cargo xtask vendor-themes` 从本机 Ghostty.app 或 GitHub 重新生成）。
- 界面骨架：桌面端与大屏是**浮动岛**（ADR 0011：窗口底上浮着侧栏 / 主区 / 右面板几块圆角面板，之间只有一道缝，**不要加分栏边框**；岛里再嵌一块用"借窗口底色"的下沉块，也不要用边框）+ **列式工作区**（ADR 0012：窗口排成列、每列可叠多扇、列分在一块块画布上、不横向滚动，排布与拖拽落点的纯函数在 `falcon-core/src/layout.rs`，改之前先读 ADR 0012）；手机端按 `ViewportLayout::Mobile` 自适应为移动导航栈与单键直达的扩展按键栏（`Esc` / `Tab` / `Ctrl` / `Alt` / 方向键等）；TUI 端按终端字符网格自适应渲染。
- 会话**默认不起名**（`name` 空串），界面显示的是自动标题：手起的名字 → 前台命令 → agent 的 CLI 名 → shell 命令名（`falcon-core/src/session_title.rs`）。列表里各处必须叫同一个名字；窗口标题栏例外——它右边就是完整工作目录，没标题时那一格空着，别编占位名。自动标题由服务端拼好经 WS 的 `{type:"title"}` 推来（`sessions/auto_title.rs`）：报了 OSC 7501 的程序用它设的终端标题（OSC 0/2，守护进程只收前台不是 shell 时写的）、再是它自报的 `app`，其余程序是探到的前台命令；没人看着也探，但只跟着输出 / 程序状态 / 程序标题点火、2.5s 节流。
- 字体：`falcon-nsh/build.rs` 把 `native/assets/fonts/` 里的 woff2（`cargo xtask vendor-fonts <berkeley|ioskeley|maple|nerd>` 从官方发行包生成、已进仓库；要 woff2_compress 与 hb-subset）解成 TTF 嵌进二进制。正文是 Berkeley Mono TX-02（商业字体，zip 不进仓库），缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
- 应用图标（ADR 0018）：选择存在服务端 settings 表；内置图标的图形在 `native/xtask/src/icons.rs`，`cargo xtask icons` 出 `falcon-nsh/assets/app-icons/` 与 `falcon-app/assets/icon-512.png`；增删图标要同时改 `xtask/src/icons.rs` 与 falcon-core 的 `APP_ICON_IDS`（服务端与客户端都用它，`falcon-nsh` 有测试对账）。
- **组件与端到端界面回归测试**：直接使用 `nsh-testing` 的 `Headless` 测试平台驱动桌面 / 移动 / TUI 三套布局与终端画布（`cargo test --release -p falcon-nsh`）；30 万行高吞吐帧耗时基准使用 `cargo xtask nsh-bench`（门禁 p95 < 4.0ms）。
- HTML 预览：网页预览使用 NSH 原生 `web_view(url)`（沙箱隔离）；字节一律走原始字节路由 `/api/projects/:id/raw/<token>/<path>`（ADR 0007），沙箱不给 allow-same-origin、凭据是只能读该项目文件的作用域令牌，别为了"方便"放宽。
- 程序状态（OSC 7501，ADR 0020）在客户端的几处出口：侧栏 / 标题栏徽标、blocked 的 toast、进入 done / error 的提示音（判定在 falcon-core 的 `FinishChimes`，声音是 `Platform::play_chime`）、终端窗口标题栏上展开的状态胶囊与底边线（计时是 falcon-core 的 `StateClock`）。**别再往终端窗口里加随状态出没的行**（原先底部的状态栏就是为此常驻、后来并进了标题栏）：一出一没终端就差一行，PTY 跟着 resize，正在干活的 TUI 整屏重画。
- 右侧栏插件（ADR 0025 + ADR 0028 双轨）：`RightPanelId::Plugin(id)`，持久化成 `plugin:<id>`。第一轨保留 `falcon-proto::plugin` 的声明式 `PluginView`（在 `falcon-nsh/src/plugins.rs` 用语义色调渲染）；第二轨支持经宿主机 `falcon-server` Ed25519 根公钥签名校验（`nsh-package` `.nshpkg`，信任根来自 `/api/plugins/trust`，包字节来自 `/api/plugins/:id/package`）的受控富交互插件包。
- **界面尺寸写 `zoom::zpx(..)`**：界面缩放（⌘+ / ⌘−）= rem 与 zpx 一起乘倍数；真实像素只留给画布几何、终端画面、窗口外框。

## 约定

- 术语以 [CONTEXT.md](./CONTEXT.md) 为准，包括 _Avoid_ 列表——那里写的不只是命名偏好，Detach/Terminate、源项目/附属项目、宿主机/远端主机这些区分直接对应代码里的分支。
- 注释解释的是"为什么"和踩过的坑（多半是实测出来、文档里查不到的），密度偏高是刻意的；改动附近代码时保持同样的说明力度，注释与代码不符时先修注释。
- 架构决策写在 `docs/adr/`：0001 是持久会话的语义与当初为什么选 Zellij（已被 0020 取代，后记写了哪些结论仍有效），0002 是附属项目与删除护栏，0003 是多仓库项目与批量派生（含回滚与包围盒断言），0005 是 rio 引擎的自持装配层与 WebGPU 渲染器（已随 React 前端删除，只剩历史参考；后记写了哪些输入口径移植进了 falcon-term），0006 是主题系统（Ghostty 主题格式、双槽位、界面色派生规则、首帧策略），0007 是原始字节路由与 HTML / 图片预览沙箱（路径形状路由、作用域令牌、响应头护栏、图片缩放模型），0008 是文件下载 / 上传（流式 exec 通道、Windows 的 base64 行协议、临时文件 + 字节数核对的落盘规则），0009 是文件面板目录浏览器（平铺当前目录、mkdir / rename / remove、文件夹上传），0010 是飞书项目面板（已删除；历史决定见该 ADR），0011 是浮动岛骨架（窗口底与圆角面板、圆角阶梯、窗口底只压亮度不洗色度与色域收缩、哪些地方刻意没改），0012 是列式工作区（列 → 窗口的排布模型、为什么必须绝对定位、落点坐标的两套下标、把手与持久化、多画布与自动另起一块），0013 是 agent 会话（开场 CLI 的启动脚本、为什么不能读 $SHELL、CLI 缺失时的退路），0014 是公网发布（Cloudflare Quick Tunnel、只在后端本机跑 cloudflared、远端经 SSH 桥），0015 是旧版 GPUI 原生客户端（渲染层已被 ADR 0028 的 NSH 六端统一架构取代，其中 crate 分层、appearance 先于 resize、一台服务端一个窗口等经验仍保留在 `falcon-nsh` / `falcon-app`），0016 是中转按机器挂（端口转发与公网发布从项目 / 右侧栏搬到主机 / 设置页、同端口互斥的槽位口径、旧规则迁移与删主机级联），0017 是用 px0 审阅（宿主机上跑 px0、同源反代的代价与前提、二进制钉哈希后由后端推到远端、pty 收尸、Origin 改写），0018 是应用图标（按服务端存的选择、公开的取图路由、客户端规整自定义图、一份定义出四种形状、原生运行时换 Dock 图标及其限制），0019 是终端滚动条（已随 Zellij 退役，只剩历史参考），0021 是 AI 助手（已删除；历史决定见该 ADR 与 `docs/design/ai-assistant.md`），0022 是多客户端的终端尺寸（取代「所有 Viewer 取最小」：跟最近真操作过的、看得见的那扇窗口，后台画布 / 收起的面板 / 最小化或被盖住的窗口不算、都看不见时退回全部；生效尺寸经 WS 的 `size` 推给每扇窗口，不定尺寸的按它建网格，放不下先缩字到一半再裁剪跟光标，比窗口小就露窗口底并在标题栏提示），0023 是文件窗口的编辑（只有未截断的合法 UTF-8 文本能改、`PUT …/file` 带内容哈希判冲突、原地覆盖保住权限位与链接、换行与末尾换行跟原文件走、换文件 / 关窗口 / 退出 / 关标签页前拦没存的修改），0024 是宿主机上的会话没了就原地重开（holder 被杀 / 宿主机重启时接回判 Gone，用同一个会话 id 再起一个而不是记 dead；Codex / grok 开场钉对话 id = 会话 id、重开时 `--resume`，codex 等退成普通 shell；exited 不重开），0025 是右侧栏插件（插件交声明式面板文档、客户端用语义色画；命令插件只在后端本机跑、不经 shell、不经 API 安装；输出逐块解析并裁剪；交互只有参数与动作（`setParam` 进出详情、文档的 `params` 改写参数、列表行的「⋯」菜单）、状态在插件那边；系统监控与 Docker 是走同一条路的内置插件：系统监控本机 sysinfo、Linux 远端 /proc 探测，Docker 跑那台机器上的 docker 命令行；`RightPanelId::Plugin` 持久化成 `plugin:<id>`），0026 是应用内更新（检查 GitHub Release，macOS 替换 Falcon.app 后按原参数 service install，再经正常退出重新打开；没保存的文件修改会先问），0020 是自研会话守护进程 falcon-sessiond（已实施，取代 0001 的 Zellij：holder 持 PTY + libghostty-vt 的 VT 进程两进程模型、自维护绑定与钉死的 ghostty、带偏移的帧协议与快照、终端查询由 VT 进程一处代答、OSC 7501 程序状态、一个版本里整体替换 Zellij、不兼容旧会话、不设回退开关；详细设计在 `docs/design/session-daemon.md`），0028 是 Native Shell（NSH）六端客户端渲染统一（`falcon-nsh` + `falcon-app` 统一 macOS / Windows / Linux GTK4 / Android / Web WASM / TUI，淘汰 GPUI 与 Android Gradle/Kotlin，双轨插件支持宿主机 Ed25519 自签发 `.nshpkg`）。做相关改动前先读对应 ADR。
