# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 命令

整个仓库只剩 Rust：`native/` 是一个 Cargo workspace，服务端 `falcon-server` 与客户端（界面 `falcon-ui`，原生入口 `falcon-desktop`、浏览器入口 `falcon-web`）都在里面。构建 / 打包 / 资源生成任务在 `native/xtask`（`cargo xtask`，别名在 `native/.cargo/config.toml`；不在 `native/` 下就 `cargo run --manifest-path native/Cargo.toml -p xtask -- <命令>`），仓库里没有 Node / JS 依赖。

```bash
cd native
cargo xtask --help          # 全部任务
cargo run -p falcon-server  # 开发服务端（4923，默认数据目录 ~/.falcon——那是日常在用的实例，开发另起要带 --port / --data-dir）
cargo xtask meegle          # 开发构建要用飞书项目面板时先取一次锁定版本的 meegle CLI（落在 native/.cache/meegle/，服务端自己找得到）
cargo xtask web             # 浏览器版客户端（GPUI → wasm），产物 native/target-wasm/dist；开发构建的服务端缺省就托管它
cargo xtask server          # 服务端发布单文件（先 web，再把浏览器版与 meegle CLI 编进去），产物 release/falcon-v<版本>-<平台>
cargo build --release -p falcon-desktop   # 原生客户端 release 构建
cargo xtask pkg             # macOS 安装包：Falcon.app = 原生客户端 + Resources 里的服务端
cargo xtask win             # Windows 安装包（Inno Setup）：只有原生客户端，没有本机服务；须在 Windows 上打
cargo xtask sessiond        # 会话守护进程 falcon-sessiond（ADR 0020，独立工作区 native/sessiond），产物 native/target-sessiond/<三元组>/release/；开发构建的服务端从这里拿，`--targets all` 交叉编 5 个目标，`--check` 零警告检查（含 Windows），`--test` 跑它的测试
cargo xtask ghostty         # 钉死版本的 Zig 编 libghostty-vt 静态库（sessiond 会自动调），源码先打上 `native/sessiond/ghostty-patches/` 的补丁（改补丁要同步 ghostty.pin 的 patches=）；`ghostty-bindgen` 只在升 ghostty 的 pin 时跑
```

xtask 调的外部工具都不是 JS：rsvg-convert（图标、pkg 的 AppIcon）、macOS 的 pkgbuild / iconutil / codesign、Inno Setup 的 ISCC、curl（meegle tarball、Zig 与 ghostty 源码包，都钉了哈希）、patch（给 ghostty 源码打补丁）、`native/scripts/build-web.sh`（bash，要 rustup 的 wasm32 target 与同版本 wasm-bindgen-cli）、`cargo xtask sessiond` 要 rustup 装好 5 个目标（x86_64 / aarch64 的 linux-musl 与 apple-darwin、x86_64-pc-windows-gnu）与 `cargo install cargo-zigbuild`（Zig 本身由 xtask 下到 `native/.cache/zig/`）。发布版本号就是 `native/Cargo.toml` 的工作区版本。

### 门禁：零警告编译 + 单元测试

没有 clippy / rustfmt 要求，但保持零警告：

```bash
cd native
cargo check --workspace --tests
cargo test --workspace     # 与单独 -p 跑都要绿：GPUI 会经 feature 合并打开 serde_json 的 preserve_order，别断言 Map 的遍历顺序
cargo test -p falcon-server --lib sessions::manager   # 单个模块；--test-name-pattern 那套换成 cargo 的名字过滤
```

服务端的测试除了纯函数层（命令构造、路径运算、模式跟踪、DB 行合并），还有用真 PTY 跑的 SessionManager 用例（持久会话的那几个起真的守护进程，没跑过 `cargo xtask sessiond` 时跳过）、`oneshot` 打进 axum 的路由用例、进程内 russh 服务端上的传输用例。守护进程自己的测试在它的工作区里（`cargo xtask sessiond --test`：真 PTY 的 holder 用例、VT 进程崩溃 / 降级 / 热升级、接回比对语料 `tests/corpus/`）。真机链路另有几道：`FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored`（起真服务端 + 守护进程，走设密码、自动重登、两次重启接回）；SSH 上的守护进程会话对**隔离的** sshd 容器跑 `FALCON_SSH_TEST=host:port:user:key cargo test -p falcon-server --lib ssh_durable -- --ignored`（设计文档 `docs/design/session-daemon.md` 第 15 节）；`cargo xtask fixtures` 重生协议 fixture（配 `cargo xtask compare-fixtures` 可在改服务端前后比形状）。Windows 远端、浏览器里的真实交互（IME、剪贴板、拖放）没有自动化测试，改动那些要在真机上验。

## 架构

协议类型的唯一真相来源是 `native/crates/falcon-proto`，服务端与客户端用同一份；`tests/fixtures/` 是 `cargo xtask fixtures` 从真服务端落盘的响应，改协议就重生 fixture。VT 模式跟踪 `falcon-proto::term_modes` 也是两边共用（服务端回放前缀与客户端鼠标上报）。原 TS 实现（`packages/server`、`packages/web`、`packages/shared`）已删除，ADR 与注释里提到的 `*.ts` 在提交 `9c9d045`（`packages/web`、`packages/shared`）与 `fd9022a`（`packages/server`）里。注释里的「React 版」「旧 React 版」就是指 `packages/web`；现在说「web / 浏览器版」指的是 `falcon-web`（同一份 GPUI 编成 wasm）。

### 数据流

浏览器 / 原生客户端 ↔ `/ws/sessions/:id`（WebSocket）↔ `SessionManager` ↔ `Backend` ↔ shell。持久会话的 Backend 是宿主机上的会话守护进程 falcon-sessiond（本机直连它的 socket / 命名管道，SSH 经 exec 的 `bridge`）；非持久会话是后端自己开的 PTY / SSH 的 pty channel。

WS 上是混合协议：**终端字节走二进制帧**（1 字节类型头 `TERM_FRAME_OUTPUT` / `TERM_FRAME_REPLAY` + UTF-8 载荷），state / reconnecting / error 等控制消息走 JSON 文本帧。REST（`/api/*`）只管项目、主机、git、转发、会话的增删改查。

### server（`native/crates/falcon-server`，路径都相对 `src/`）

由 Node 版逐文件移植（ADR 与注释里提到的 `*.ts` 都在提交 `fd9022a` 的 `packages/server/src/` 里，Rust 文件名换成 snake_case），方案与实测记录在 `docs/design/rust-unification.md` §11。

- **执行模型**：会话核心（SessionManager、SshLink、中转、px0）跑在 `engine.rs` 的一条专用线程上——current_thread runtime + LocalSet，状态全是 `Rc` / `RefCell`，与 Node 事件循环同一个语义（同步段不会被打断，只在 await 处让出）。axum 处理器（多线程）经 `EngineHandle::call` 投闭包进去拿结果，WS 的 input / resize 经 `send` 保序投递。**别把这些状态改成 `Arc<Mutex>`**；RefCell 的借用只在同步小段里持有，不跨 await、不在借着时调别的方法。阻塞的 PTY 读写各开线程，SQLite 照旧同步调。HTTP 处理器被丢掉（客户端断开）不会打断引擎里在跑的写操作。
- `sessions/manager.rs` 是核心：`LiveEntry` 持有 backend、多个 Viewer（`viewer.rs`：发送端 + 未写出字节计数，即 bufferedAmount，背压 4MB / 256KB 两条水位）、`ViewerArbiter`（多 Viewer 时尺寸取最小、深浅跟最近操作的那端）、16ms 输出合并窗口、SSH 断线的指数退避重连。回放分两种：**持久会话**向守护进程要快照（`Sync`），等快照的 Viewer 标 `syncing`、期间不收输出，快照按连接上到达的顺序回来（设计文档 5.5，`on_backend_event`）；**非持久会话**用 RingBuffer（4MB）+ VT 模式跟踪的前缀。程序状态（OSC 7501）由守护进程推来，存在 entry 上、经 WS 的 `program_status` 推给界面、在会话列表上带摘要。会话状态机 `active / unverified / dead` 与 `Detach` / `Terminate` 的区分见 CONTEXT.md，别自己发明语义。
- `sessions/backend.rs` 是共同接口（守护进程才有的能力——快照、前台命令、外观、程序状态、终止、VT 热升级——是带缺省实现的方法，事件走 `BackendCallbacks.on_event`）；`local.rs`（本机：守护进程的 spawn 与直连、非持久会话的 portable-pty）/ `ssh.rs`（russh 传输层）+ `ssh_host.rs`（远端探测、守护进程的部署与 Windows 断线验证、bridge 附着、非持久的 pty channel）各实现一边。数据目录在配置解析时就转成绝对路径：portable-pty 只在 PATH 里找相对路径的程序。
- `sessiond/` 是服务端这一侧的会话守护进程（ADR 0020，设计文档 `docs/design/session-daemon.md`）：`command.rs` 纯函数（目标三元组、按内容哈希命名的二进制、各子命令 argv、Windows 上**不能经 PowerShell 的 `&` 跑 bridge**——它会改写二进制输出，见 `windows_bridge_command`）、`bin.rs`（各目标的二进制：`FALCON_SESSIOND_DIR` > 编进发布版的 gzip（`embed-sessiond`）> `native/target-sessiond`）、`deploy.rs`（推到宿主机、`version --json` 握手；POSIX 经 stdin，Windows 走 base64 行协议）、`backend.rs`（帧协议、bridge 的前导标记、快照拼 continuation）。守护进程本体在 `native/sessiond/`（独立工作区：`ghostty-vt-sys` 入库的绑定、`ghostty-vt` 安全封装、`falcon-sessiond` 的 holder / VT 进程 / spawn / bridge / attach），协议类型在 `falcon-proto::sessiond`，**holder 的协议只增不改**（旧 holder 的会话要能被新 falcon 接回）。
- `git/` 的分层（`sessiond/` 照它写）：
  - `command.rs` —— **纯函数，零 I/O，只产出 argv 数组与 env**，绝不拼命令行字符串（远端 POSIX 与远端 Windows 转义规则不同）；
  - `host.rs`（`src/host.rs`，git / 文件 / px0 / 守护进程共用）—— 抹平四种执行环境（本地 Unix / 本地 Windows / SSH POSIX / SSH Windows）为 `posix` / `windows` 两类，负责路径构造、命令行拼装与远端探测；
  - `exec.rs`（`Exec` trait）/ `git/repo.rs` —— 执行与错误分类。**执行器的铁律：非零退出码是正常返回值，绝不当 `Err`**，判错一律显式查 `res.code`；只有执行本身起不来才算链路故障。
- `git/path.rs` **不用 `std::path`**：后端跑在 Windows 上也可能在为 Linux 远端构造路径。同理后端**永远不 chdir 进 worktree**，git 命令一律带 `-C <dir>`。
- 终端查询（DA / DSR / DECRQM / OSC 10–12 …）：持久会话由宿主机的 VT 进程**一处**回答，客户端对 `durable` 会话不发 `PtyWrite`（多个 Viewer 会各答一遍、没人在看时没人答）；非持久会话照旧由客户端答，OSC 10/11/12 由服务端的 `OscColorGate` 代答。kitty 键盘的查询（`CSI ? u`）不答——客户端的按键编码只有传统 xterm 那套。滚动在客户端本地（alacritty 有真正的 scrollback），ADR 0019 的 zellij 插件已随 Zellij 一起删除。
- `git/remove.rs` 是删除**附属项目 worktree 目录**的唯一入口，护栏（静态断言 + 动态取证）在改动前先整份读完 `docs/adr/0002-worktree-derived-projects.md`。工作目录内部的文件删除在 `files.rs`（`resolve_inside` 挡在项目工作目录里，Windows 宿主机上的反斜杠 `..` 也挡），两件事不要合成一条路径。
- `sessions/agent.rs` 是 agent 会话（开场直接跑 claude / codex / grok，ADR 0013）：纯函数产启动脚本与写入命令，脚本落在宿主机 `<falcon 根>/agents/`，会话的 argv 就是它；CLI 退出后 `exec` 回登录 shell，会话不跟着结束。脚本里的登录 shell 是**生成时写死的绝对路径**，绝不读 `$SHELL`（Windows 远端那条路径上 `SHELL` 就是脚本自己，会递归）。
- `cloudflared/` 是公网发布（ADR 0014）：`command.rs` 纯函数产 argv 与解析 Quick Tunnel URL / `/quicktunnel` JSON（**只在 falcon 后端本机 spawn `cloudflared`，不往远端装**）；`bin.rs` 按需把锁定版本下到 `<dataDir>/bin/cloudflared`（`FALCON_CLOUDFLARED_BIN` 优先，PATH 兜底）；`sessions/share.rs` 管进程生命周期，远端目标先在本机 `listen(0)` 再 `forward_out`。规则在 `host_shares` 表，公网 URL 是运行时事实不入库。v1 只做 Quick Tunnel + HTTP。
- 中转（端口转发 `sessions/forward.rs` + 公网发布 `sessions/share.rs`，两者共用的监听 / 桥接在 `sessions/relay.rs`，ADR 0016）**按机器挂，不挂项目**：转发挂 SSH Host（`host_forwards`），发布挂本机或 SSH Host（`host_shares.host_id` 为 null = 本机）。隧道走主机链路 `SessionManager::get_host_link`（与「浏览远端目录」共用），断线由 `schedule_host_reconnect` 重连，**不走项目链路**。同端口可存多条、同时只一条生效：启用时先同步落库把同槽位的其它规则置 disabled，再异步停旧起新；槽位口径（本地转发跨主机比端口、远端转发与发布在同一台机器内比）在纯函数 `sessions/relay_spec.rs`，web 的「同端口」徽标照同一口径算。
- `meegle/` 是右侧「飞书项目」面板的后端（ADR 0010）：`command.rs` 纯函数产 argv 与归一化 CLI 输出（**只在 falcon 后端本机 spawn `meegle`，不经 shell、不跟项目走**），`client.rs` 起进程 / TTL 缓存（待办 / 搜索 / 详情默认 5 分钟，`?fresh=1` 与 `POST /api/meegle/cache/clear` 打穿）/ 按类型扇出 / device-code 登录进程（它是 `Send + Sync` 的，挂在 AppState 上，不进引擎），`api/meegle.rs` 挂 `/api/meegle/*`（含粘贴链接解析 `resolve-url`——走 CLI 的 `url decode`，别自己拆路径——与固定列表 CRUD，固定项存 `db.rs` 的 `meegle_pins` 表），`bin.rs` 定位可执行文件：`FALCON_MEEGLE_BIN`（文件得真在）> 编进发布二进制的那份（`embed-meegle`，首次用时释放到 `<dataDir>/bin/meegle-<哈希>`）> `native/.cache/meegle/bin/` 里 `cargo xtask meegle` 取下的锁定版本（开发构建；版本与 sha512 锁在 `xtask/src/meegle.rs`）> PATH。CLI 的脾气（错误信封在 stderr、未登录时一切命令都是 unknown command、视图只能按关键字搜、待办没名字要 MQL 补、view search 限 5 qps）全写在 `command.rs` 顶部注释，改动前先读。
- `px0/` 是「用 px0 审阅」（ADR 0017）：在项目宿主机上按需拉起 px0，经 falcon **同源**反代到 `/px0/<项目 id>/`（多仓库项目按成员各起一个，挂 `/px0/<项目 id>/<成员序号>/`，前缀本身是选成员页——px0 只认一个仓库；`api/px0.rs`，hyper 逐请求开连接、流式转发；鉴权就是登录 cookie，与 `/api/` 同一口径）。`command.rs` 纯函数（资产映射、**钉死的 sha256**、argv、端口解析、远端安装 / 启动命令），`bin.rs` 在后端本机下载校验、SSH 项目再经 stdin 推到远端（不让远端自己下），`manager.rs` 管实例——**本地与远端都挂在 pty 上**，后端死了 px0 跟着挂断，别改回普通 spawn / 无 pty 的 exec；远端连接直接走 forward_out，本机不另开监听端口；反代的租约随响应体释放，空闲回收据此计数。`proxy.rs` 的头过滤有讲究：剥 falcon 的 cookie、只替同源请求改写 Origin（px0 的 localPost 要 Origin == Host）、去 set-cookie。同源意味着 px0 的前端能调 falcon 全部接口——**只在本机 / 内网可接受，公网访问前先挪到独立源**。原生客户端不嵌 px0，菜单项把地址交给系统浏览器；没登录的浏览器被送去 `/?next=<px0 地址>`，登录成功后由浏览器版的 `Platform::redirect_after_login` 跳回（只认 `/px0/` 开头，规则在 falcon-core 的 `px0::login_next`）。
- `api/` 是 HTTP 面（axum）：路由路径、请求 / 响应体、状态码、错误形状（`{ error }` 与 Fastify 的 500 形状，`api/error.rs`）都照原 Node 版。请求体用 `LenientJson` 收、按 JS 口径逐字段取（`api/input.rs`），不先反序列化成强类型——那会把"某个字段类型不对"变成整条请求 400。`/api/*` 与 `/px0/*` 的登录检查在 `require_login`，各自验身份的（原始字节令牌、应用图标取图、askpass helper）挂在 `public_router`。前端产物：`FALCON_WEB_DIST`（要真有 index.html）> 编进二进制的那份（`embed-web`）> 开发默认目录。
- `db.rs` 用 rusqlite（bundled SQLite），与 Node 版同一个 `falcon.db` / `secret.key` 格式。外键约束显式关闭，级联在应用层手写；`migrate()` 是幂等的 `CREATE TABLE IF NOT EXISTS` + 加列，没有版本号迁移表——改表结构就往这套里加。
- Windows 远端的所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。
- 测 SSH 只用隔离的 sshd（Docker 容器、或 `SetEnv HOME=` 指到临时短路径），别对自己真实的家目录跑 probe / 部署；开发实例端口用 4940–4999、数据目录用短路径（守护进程的 socket 上限 104 字节，超了会落到 `/tmp/falcon-sd-<uid>-<哈希>/`），从 falcon 终端里起要 `env -u FALCON_WEB_DIST -u FALCON_MEEGLE_BIN -u FALCON_SESSION_ID`。开发构建的服务端用 `native/target-sessiond` 里的守护进程：改了守护进程要重跑 `cargo xtask sessiond`（远端用的那几个目标加 `--targets all`），否则宿主机上跑的还是旧的（同版本号不会触发重推，文件名按内容哈希，所以新编的会被当成新文件推上去）。

### client（`native/crates/falcon-ui` / `falcon-platform` / `falcon-desktop` / `falcon-web` 及其下层 crate）

Rust + GPUI 写的客户端，连 falcon 服务端（REST + `/ws/sessions/:id` + 登录 cookie）。一套界面两种产物（设计文档决定六，C1 拆层）：界面全在 `falcon-ui`，**不写 target cfg**——凡是"这台机器上有没有 / 怎么做"的事（偏好与工作区存哪儿、服务端从哪儿来、访问密码、本机服务、字体来源、下载、HTML 预览视图、应用图标、是不是 mac 键位）都问 `falcon-platform` 的 `Platform` trait（`falcon_platform::get(cx)`）；原生实现与入口在 `falcon-desktop`（数据目录 JSON——macOS 上记住的访问密码也在这，**不用钥匙串**（ad-hoc 签名每个版本都要重新授权，原因见 `passwords.rs`）；Windows 的密码走凭据管理器、launchd 托管、wry、AppKit Dock、Windows 资源），浏览器实现与 wasm 入口在 `falcon-web`（localStorage、页面的源、`<a download>`，DOM 胶水在 `src/dom.rs`，服务端托管产物）。新的平台差异先加进 trait，别在 falcon-ui 里写 `cfg(target_family/target_os)`；执行器层面的差异（`block_in_place`、`MaybeSend`）在 falcon-client 里。定下来的做法与踩过的坑在 ADR 0015，提案原文在 `docs/design/gpui-client.md`，浏览器版在 `docs/design/rust-unification.md`（附录 B 是浏览器端还没补齐的缺口），改动前先读。

```bash
cd native
cargo run -p falcon-desktop                     # 连本机服务（按已装 LaunchAgent 的端口，没装是 4923）；FALCON_LOCAL_URL 可改指别的实例
./scripts/build-web.sh                          # 浏览器版，产物 target-wasm/dist
```

- **crates.io 走 `native/.cargo/config.toml` 里的 rsproxy 镜像**：这台机器上 Clash 的 fake-ip 把 index.crates.io 解析坏了。网络正常的机器删掉那一段即可。
- 分层：`falcon-proto`（协议类型）→ `falcon-client`（REST / WS，与执行器无关的 future，原生上自带 tokio 运行时，浏览器上走 fetch / `web_sys::WebSocket`）→ `falcon-term`（alacritty_terminal + 从 Zed 抄的按键编码 + 鼠标 / 滚轮 / 模式跟踪）→ `falcon-theme`（主题系统：Ghostty 主题文件即数据模型、浅深双槽位、整套界面色由一套主题派生，ADR 0006 / 0011）→ `falcon-core`（与 GPUI 无关的纯逻辑：列式工作区排布 `layout.rs`、窗口 key `pane_key.rs`、会话标题 `session_title.rs`、快捷键、文件路径 / 搜索 / 树、git 图等；`tests/vectors/` 是回归向量）→ `falcon-platform` / `falcon-ui` / `falcon-desktop` / `falcon-web`（只有这四个依赖 GPUI）。**纯逻辑一律放下层 crate**，GPUI 升级只波及这一层。
- GPUI 走 `gpui-kit` 总包（`=` 精确锁版本）；Zed 的终端代码按文件抄（`terminal/element.rs`、`falcon-term/src/keys.rs`，文件头注明出处 commit），不按 crate 依赖——会带进第二份 gpui。`native/` 因此是 GPL-3.0-or-later。gpui-kit 开着 `gpui-fast`（Longbridge 的 GPUI Fast：保留式视图 / 布局 / 文字排版，`gpui_kit::*` 导出的是它；上游 gpui-pre 一族照样参与编译）。`native/vendor/` 下的 alacritty_terminal、gpui-fast（文字排版跨帧沿用补了字体代数，浏览器版后到的中文字体才生效）、gpui-fast-web（IME 补丁）与 gpui-{fast,pre}-{web,platform}（关默认的 `multithreaded`）都是带补丁的 fork，改动清单在各自 Cargo.toml 顶部，升级 gpui-kit 时重放（docs/design/rust-unification.md 决定八）。
- 文案：`t!("key")`，**界面上不允许硬编码中文**。文案只有 `falcon-ui/locales/zh-CN.json` 一份（v1 只有中文，直接改它），原生 / 平台独有的挂在 `native.<区域>` 下。
- 界面色**只准用 falcon-theme 派生出的语义 token**，不许写死颜色；新增语义色去 `falcon-theme/src/derive.rs` 加。派生规则有冻结的金标准 fixture（`tests/derive_golden.rs`），有意改规则要连 fixture 一起改并写清楚。**深浅按主题底色亮度切，不按系统明暗**。内置主题 = Falcon 两套 + Ghostty 全部 463 套（`falcon-theme/data/`，`cargo xtask vendor-themes` 从本机 Ghostty.app 或 GitHub 重新生成）。
- 界面骨架是**浮动岛**（ADR 0011）：窗口底上浮着侧栏 / 主区 / 右面板几块圆角面板，之间只有一道缝，**不要加分栏边框**；岛里再嵌一块用"借窗口底色"的下沉块，也不要用边框。主区是**列式工作区**（ADR 0012）：窗口排成列、每列可叠多扇、列分在一块块画布上、不横向滚动，排布与拖拽落点的纯函数在 `falcon-core/src/layout.rs`，改之前先读 ADR 0012。
- 会话**默认不起名**（`name` 空串），界面显示的是自动标题：手起的名字 → 前台命令 → agent 的 CLI 名 → shell 命令名（`falcon-core/src/session_title.rs`）。列表里各处必须叫同一个名字；窗口标题栏例外——它右边就是完整工作目录，没标题时那一格空着，别编占位名。自动标题由服务端拼好经 WS 的 `{type:"title"}` 推来（`sessions/auto_title.rs`）：报了 OSC 7501 的程序用它设的终端标题（OSC 0/2，守护进程只收前台不是 shell 时写的）、再是它自报的 `app`，其余程序是探到的前台命令；没人看着也探，但只跟着输出 / 程序状态 / 程序标题点火、2.5s 节流。
- 字体：`falcon-ui/build.rs` 把 `native/assets/fonts/` 里的 woff2（`cargo xtask vendor-fonts <berkeley|ioskeley|maple|nerd>` 从官方发行包生成、已进仓库；要 woff2_compress 与 hb-subset）解成 TTF：原生嵌进二进制，浏览器版只嵌正文字体、其余由 build-web.sh 拷进产物按需拉（`Platform::font_source`；GPUI 不认 WOFF2）。正文是 Berkeley Mono TX-02（商业字体，zip 不进仓库），缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
- 应用图标（ADR 0018）：选择存在服务端 settings 表；内置图标的图形在 `native/xtask/src/icons.rs`，`cargo xtask icons` 出浏览器版的 `native/web/icons/`（服务端的 `/api/app-icon/favicon` 跳到这里；PWA 已删，ADR 0018 后记）与原生的 `falcon-ui/assets/app-icons/`；增删图标要同时改 `xtask/src/icons.rs` 与 falcon-core 的 `APP_ICON_IDS`（服务端与客户端都用它，falcon-ui 有测试对账）。
- **验证界面靠 Metal 回读截图**（锁屏 / 远程也能用）：`cargo build -p falcon-desktop --features snapshot`，再用 `FALCON_AUTOMATE="ready;select:<项目>;new-terminal;type:ls\r;wait:1000;snap:/tmp/a.png;quit"` 驱动（步骤全表见 `falcon-ui/src/automation.rs`，`type:` 里的 `;` 写成 `\x3b`），配 `FALCON_NATIVE_DATA_DIR=<临时目录>`（别写进用户真实的 ~/Library/Application Support/Falcon）与 `FALCON_LOCAL_URL`（指向测试服务端，别用 4923）。点击坐标是窗口逻辑像素（截图 PNG 是 2 倍）。锁屏时显示链路不走，`snap` 自己会先画两帧，别把"截图是空的 / 旧的"当成界面 bug。浏览器版用 Chrome DevTools 驱动：canvas 上的合成指针事件在 DPR 2 时坐标要乘 2，键盘输入用真实按键（`type_text`）而不是合成 KeyboardEvent。
- **压测用 `--features automation --release`**（不带 snapshot 的 test-support）：`frames:<ms>` 以 60Hz 手动画并打印帧耗时 p50 / p95，`frames:<ms>:refresh` 无视视图缓存。侧栏与右侧面板是 `cached` 视图（ADR 0015），新加的大块视图照此办理。
- **浏览器版**：几条硬规矩：时间一律 `web_time::{Instant, SystemTime}`（std 的在 wasm 上一调就 panic）；future 的约束写 `MaybeSend`（falcon-client / falcon-core 各有一份），别写死 `Send`；static 里放不了在飞的 future，wasm 上用 thread_local。wasm 构建走 rustup 的 stable + `wasm32-unknown-unknown`，wasm-bindgen-cli 与 Cargo.lock 同版本（PATH 上排前面的 Homebrew rustc 没有 wasm 标准库，脚本里处理了），**不要 nightly**。宿主页是 `native/web/index.html`（首帧主题脚本、favicon；没有 PWA）。
- HTML 预览：界面只管摆位置，网页视图由平台给（`Platform::html_view`）；原生是 falcon-desktop 默认开启的 `webview` feature（wry，`--no-default-features` 退成"在浏览器中打开"），浏览器版的 iframe 叠层还没做（附录 B，排在 C3）；字节一律走原始字节路由 `/api/projects/:id/raw/<token>/<path>`（ADR 0007），沙箱不给 allow-same-origin、凭据是只能读该项目文件的作用域令牌，别为了"方便"放宽。
- 程序状态（OSC 7501，ADR 0020）在客户端的几处出口：侧栏 / 标题栏徽标、blocked 的 toast、进入 done / error 的提示音（`chime.rs`，判定在 falcon-core 的 `FinishChimes`，声音是 `Platform::play_chime`）、终端窗口标题栏上展开的状态胶囊与底边线（`terminal/title_status.rs`，计时是 falcon-core 的 `StateClock`）。**别再往终端窗口里加随状态出没的行**（原先底部的状态栏就是为此常驻、后来并进了标题栏）：一出一没终端就差一行，PTY 跟着 resize，正在干活的 TUI 整屏重画。状态指示的动画走 `motion.rs`（不看系统的「减少动态效果」，GPUI 的 `with_animation` 遇到它整个定格；持续动画限 30 帧），别换回 `with_animation`；标题栏里会动的两块是独立小视图（`StatusSlot`），别挪回画布里画——动画逐帧重画的是发起它的整个视图。
- **通知一律走 `falcon-ui/src/toasts.rs` 的 `ToastExt`**，不用组件库的 `push_notification`（它的通知层会被对话框盖住，见 ADR 0015）。
- **界面尺寸写 `zoom::zpx(..)`，不写 `px(..)`**：界面缩放（⌘+ / ⌘−）= rem 与 zpx 一起乘倍数；`px` 只留给画布几何、终端画面、窗口外框这类真实像素（`zoom.rs` 顶部有清单）。`theme.font_size` 就是 rem，必须是 16 × 倍数，别再拿它当正文字号。

## 约定

- 术语以 [CONTEXT.md](./CONTEXT.md) 为准，包括 _Avoid_ 列表——那里写的不只是命名偏好，Detach/Terminate、源项目/附属项目、宿主机/远端主机这些区分直接对应代码里的分支。
- 注释解释的是"为什么"和踩过的坑（多半是实测出来、文档里查不到的），密度偏高是刻意的；改动附近代码时保持同样的说明力度，注释与代码不符时先修注释。
- 架构决策写在 `docs/adr/`：0001 是持久会话的语义与当初为什么选 Zellij（已被 0020 取代，后记写了哪些结论仍有效），0002 是附属项目与删除护栏，0003 是多仓库项目与批量派生（含回滚与包围盒断言），0005 是 rio 引擎的自持装配层与 WebGPU 渲染器（已随 React 前端删除，只剩历史参考；后记写了哪些输入口径移植进了 falcon-term），0006 是主题系统（Ghostty 主题格式、双槽位、界面色派生规则、首帧策略），0007 是原始字节路由与 HTML / 图片预览沙箱（路径形状路由、作用域令牌、响应头护栏、图片缩放模型），0008 是文件下载 / 上传（流式 exec 通道、Windows 的 base64 行协议、临时文件 + 字节数核对的落盘规则），0009 是文件面板目录浏览器（平铺当前目录、mkdir / rename / remove、文件夹上传），0010 是飞书项目面板（宿主机上的 meegle CLI 当数据源、实测的 CLI 输出约定、device-code 登录、两层 TTL 缓存），0011 是浮动岛骨架（窗口底与圆角面板、圆角阶梯、窗口底只压亮度不洗色度与色域收缩、哪些地方刻意没改），0012 是列式工作区（列 → 窗口的排布模型、为什么必须绝对定位、落点坐标的两套下标、把手与持久化、多画布与自动另起一块），0013 是 agent 会话（开场 CLI 的启动脚本、为什么不能读 $SHELL、CLI 缺失时的退路），0014 是公网发布（Cloudflare Quick Tunnel、只在后端本机跑 cloudflared、远端经 SSH 桥），0015 是原生客户端（GPUI 薄客户端、crate 分层、Zed 代码按文件抄、appearance 先于 resize、一台服务端一个窗口、测量数据与踩坑清单），0016 是中转按机器挂（端口转发与公网发布从项目 / 右侧栏搬到主机 / 设置页、同端口互斥的槽位口径、旧规则迁移与删主机级联），0017 是用 px0 审阅（宿主机上跑 px0、同源反代的代价与前提、二进制钉哈希后由后端推到远端、pty 收尸、Origin 改写），0018 是应用图标（按服务端存的选择、公开的取图路由、客户端规整自定义图、一份定义出四种形状、原生运行时换 Dock 图标及其限制），0019 是终端滚动条（已随 Zellij 退役，只剩历史参考），0020 是自研会话守护进程 falcon-sessiond（已实施，取代 0001 的 Zellij：holder 持 PTY + libghostty-vt 的 VT 进程两进程模型、自维护绑定与钉死的 ghostty、带偏移的帧协议与快照、终端查询由 VT 进程一处代答、OSC 7501 程序状态、一个版本里整体替换 Zellij、不兼容旧会话、不设回退开关；详细设计在 `docs/design/session-daemon.md`）。做相关改动前先读对应 ADR。
