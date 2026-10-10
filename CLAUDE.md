# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 命令

整个仓库只剩 Rust：`native/` 是一个 Cargo workspace，服务端 `falcon-server` 与客户端 `falcon-app`（原生桌面 + 浏览器 wasm 两种产物）都在里面。构建 / 打包 / 资源生成任务在 `native/xtask`（`cargo xtask`，别名在 `native/.cargo/config.toml`；不在 `native/` 下就 `cargo run --manifest-path native/Cargo.toml -p xtask -- <命令>`），仓库里没有 Node / JS 依赖。

```bash
cd native
cargo xtask --help          # 全部任务
cargo run -p falcon-server  # 开发服务端（4923，默认数据目录 ~/.falcon——那是日常在用的实例，开发另起要带 --port / --data-dir）
cargo xtask meegle          # 开发构建要用飞书项目面板时先取一次锁定版本的 meegle CLI（落在 native/.cache/meegle/，服务端自己找得到）
cargo xtask web             # 浏览器版客户端（GPUI → wasm），产物 native/target-wasm/dist；开发构建的服务端缺省就托管它
cargo xtask server          # 服务端发布单文件（先 web，再把浏览器版与 meegle CLI 编进去），产物 release/falcon-v<版本>-<平台>
cargo build --release -p falcon-app   # 原生客户端 release 构建
cargo xtask pkg             # macOS 安装包：Falcon.app = 原生客户端 + Resources 里的服务端
cargo xtask win             # Windows 安装包（Inno Setup）：只有原生客户端，没有本机服务；须在 Windows 上打
cargo xtask zellij-plugin   # 重编滚动位置插件（ADR 0019），产物提交在 native/crates/falcon-server/assets/；只在改插件或升 zellij 时跑
```

xtask 调的外部工具都不是 JS：rsvg-convert（图标、pkg 的 AppIcon）、macOS 的 pkgbuild / iconutil / codesign、Inno Setup 的 ISCC、curl（meegle tarball 与 Zellij release 查询）、`native/scripts/build-web.sh`（bash，要 rustup 的 wasm32 target 与同版本 wasm-bindgen-cli）。发布版本号就是 `native/Cargo.toml` 的工作区版本。

### 门禁：零警告编译 + 单元测试

没有 clippy / rustfmt 要求，但保持零警告：

```bash
cd native
cargo check --workspace --tests
cargo test --workspace     # 与单独 -p 跑都要绿：GPUI 会经 feature 合并打开 serde_json 的 preserve_order，别断言 Map 的遍历顺序
cargo test -p falcon-server --lib sessions::manager   # 单个模块；--test-name-pattern 那套换成 cargo 的名字过滤
```

服务端的测试除了纯函数层（命令构造、路径运算、模式跟踪、DB 行合并），还有用真 PTY 跑的 SessionManager 用例、`oneshot` 打进 axum 的路由用例、进程内 russh 服务端上的传输用例。真机链路另有两道：`FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored`（起真服务端 + zellij，走设密码、自动重登、两次重启接回）；`cargo xtask fixtures` 重生协议 fixture（配 `cargo xtask compare-fixtures` 可在改服务端前后比形状）。SSH 远端、Windows 远端与浏览器里的真实交互（IME、剪贴板、拖放）没有自动化测试，改动那些要在真机上验。

## 架构

协议类型的唯一真相来源是 `native/crates/falcon-proto`，服务端与客户端用同一份；`tests/fixtures/` 是 `cargo xtask fixtures` 从真服务端落盘的响应，改协议就重生 fixture。VT 模式跟踪 `falcon-proto::term_modes` 也是两边共用（服务端回放前缀与客户端鼠标上报）。原 TS 实现（`packages/server`、`packages/web`、`packages/shared`）已删除，ADR 与注释里提到的 `*.ts` 在提交 `9c9d045`（`packages/web`、`packages/shared`）与 `fd9022a`（`packages/server`）里。

### 数据流

浏览器 / 原生客户端 ↔ `/ws/sessions/:id`（WebSocket）↔ `SessionManager` ↔ `Backend`（本地 PTY 或 SSH channel）↔ 宿主机上的 Zellij ↔ shell。

WS 上是混合协议：**终端字节走二进制帧**（1 字节类型头 `TERM_FRAME_OUTPUT` / `TERM_FRAME_REPLAY` + UTF-8 载荷），state / reconnecting / error 等控制消息走 JSON 文本帧。REST（`/api/*`）只管项目、主机、git、转发、会话的增删改查。

### server（`native/crates/falcon-server`，路径都相对 `src/`）

由 Node 版逐文件移植（ADR 与注释里提到的 `*.ts` 都在提交 `fd9022a` 的 `packages/server/src/` 里，Rust 文件名换成 snake_case），方案与实测记录在 `docs/design/rust-unification.md` §11。

- **执行模型**：会话核心（SessionManager、SshLink、中转、px0）跑在 `engine.rs` 的一条专用线程上——current_thread runtime + LocalSet，状态全是 `Rc` / `RefCell`，与 Node 事件循环同一个语义（同步段不会被打断，只在 await 处让出）。axum 处理器（多线程）经 `EngineHandle::call` 投闭包进去拿结果，WS 的 input / resize 经 `send` 保序投递。**别把这些状态改成 `Arc<Mutex>`**；RefCell 的借用只在同步小段里持有，不跨 await、不在借着时调别的方法。阻塞的 PTY 读写各开线程，SQLite 照旧同步调。HTTP 处理器被丢掉（客户端断开）不会打断引擎里在跑的写操作。
- `sessions/manager.rs` 是核心：`LiveEntry` 持有 backend、RingBuffer（4MB 输出环形缓冲，供 Viewer 重连回放）、多个 Viewer（`viewer.rs`：发送端 + 未写出字节计数，即 bufferedAmount，背压 4MB / 256KB 两条水位）、`ViewerArbiter`（多 Viewer 时尺寸取最小、深浅跟最近操作的那端）、16ms 输出合并窗口、VT 模式跟踪、SSH 断线的指数退避重连。会话状态机 `active / unverified / dead` 与 `Detach` / `Terminate` 的区分见 CONTEXT.md，别自己发明语义。
- `sessions/backend.rs` 是本地 PTY 与 SSH channel 的共同接口；`local.rs`（portable-pty）/ `ssh.rs`（russh 传输层）+ `ssh_zellij.rs`（远端探测、Zellij 安装与会话查询）各实现一边。数据目录在配置解析时就转成绝对路径：portable-pty 只在 PATH 里找相对路径的程序。
- `zellij/` 与 `git/` 是同一套分层，改一边时照另一边的样子写：
  - `command.rs` —— **纯函数，零 I/O，只产出 argv 数组与 env**，绝不拼命令行字符串（远端 POSIX 与远端 Windows 转义规则不同）；
  - `host.rs` —— 抹平四种执行环境（本地 Unix / 本地 Windows / SSH POSIX / SSH Windows）为 `posix` / `windows` 两类，负责路径构造与命令行拼装；
  - `exec.rs`（`Exec` trait）/ `git/repo.rs` —— 执行与错误分类。**执行器的铁律：非零退出码是正常返回值，绝不当 `Err`**，判错一律显式查 `res.code`；只有执行本身起不来才算链路故障。
- `git/path.rs` **不用 `std::path`**：后端跑在 Windows 上也可能在为 Linux 远端构造路径。同理后端**永远不 chdir 进 worktree**，git 命令一律带 `-C <dir>`。
- 终端滚动条（ADR 0019）：滚动发生在宿主机的 zellij 里，位置只有 zellij 的插件事件 `ActivePaneScroll` 给得出（CLI 问不到视口下方还有多少）。插件源码 `native/zellij-plugin`（独立的 wasm32-wasip1 crate，不进工作区；`zellij-tile` 与 `ZELLIJ_VERSION` 同步升级），产物 `assets/falcon-scroll.wasm` 编进服务端，由 `sessions/scroll_plugin.rs` 推到宿主机并预写 permissions.kdl，随会话以后台插件加载；查询是按名字广播的一次性 `zellij pipe`（**别加 `--plugin`**，stdin 要给 EOF）。带插件的会话用单独的 `config/scroll.kdl`（titles 边框样式），老会话照旧 config.kdl——**别把两套合并**，拿新配置接回跑在老 zellij 上的会话会画出整圈边框；会话用哪套记在 `sessions.scroll_plugin`。zellij 0.45 起两套配置都必须 `scroll_mode_sync false`，否则滚一下后按键全被吞。
- `git/remove.rs` 是删除**附属项目 worktree 目录**的唯一入口，护栏（静态断言 + 动态取证）在改动前先整份读完 `docs/adr/0002-worktree-derived-projects.md`。工作目录内部的文件删除在 `files.rs`（`resolve_inside` 挡在项目工作目录里，Windows 宿主机上的反斜杠 `..` 也挡），两件事不要合成一条路径。
- `sessions/agent.rs` 是 agent 会话（开场直接跑 claude / codex / grok，ADR 0013）：纯函数产启动脚本与写入命令，脚本落在宿主机 `<falcon 根>/agents/`，Zellij 的 `--default-shell` 指向它；CLI 退出后 `exec` 回登录 shell，会话不跟着结束。脚本里的登录 shell 是**生成时写死的绝对路径**，绝不读 `$SHELL`（Windows 远端那条路径上 `SHELL` 就是脚本自己，会递归）。
- `cloudflared/` 是公网发布（ADR 0014）：`command.rs` 纯函数产 argv 与解析 Quick Tunnel URL / `/quicktunnel` JSON（**只在 falcon 后端本机 spawn `cloudflared`，不往远端装**）；`bin.rs` 按需把锁定版本下到 `<dataDir>/bin/cloudflared`（`FALCON_CLOUDFLARED_BIN` 优先，PATH 兜底）；`sessions/share.rs` 管进程生命周期，远端目标先在本机 `listen(0)` 再 `forward_out`。规则在 `host_shares` 表，公网 URL 是运行时事实不入库。v1 只做 Quick Tunnel + HTTP。
- 中转（端口转发 `sessions/forward.rs` + 公网发布 `sessions/share.rs`，两者共用的监听 / 桥接在 `sessions/relay.rs`，ADR 0016）**按机器挂，不挂项目**：转发挂 SSH Host（`host_forwards`），发布挂本机或 SSH Host（`host_shares.host_id` 为 null = 本机）。隧道走主机链路 `SessionManager::get_host_link`（与「浏览远端目录」共用），断线由 `schedule_host_reconnect` 重连，**不走项目链路**。同端口可存多条、同时只一条生效：启用时先同步落库把同槽位的其它规则置 disabled，再异步停旧起新；槽位口径（本地转发跨主机比端口、远端转发与发布在同一台机器内比）在纯函数 `sessions/relay_spec.rs`，web 的「同端口」徽标照同一口径算。
- `meegle/` 是右侧「飞书项目」面板的后端（ADR 0010）：`command.rs` 纯函数产 argv 与归一化 CLI 输出（**只在 falcon 后端本机 spawn `meegle`，不经 shell、不跟项目走**），`client.rs` 起进程 / TTL 缓存（待办 / 搜索 / 详情默认 5 分钟，`?fresh=1` 与 `POST /api/meegle/cache/clear` 打穿）/ 按类型扇出 / device-code 登录进程（它是 `Send + Sync` 的，挂在 AppState 上，不进引擎），`api/meegle.rs` 挂 `/api/meegle/*`（含粘贴链接解析 `resolve-url`——走 CLI 的 `url decode`，别自己拆路径——与固定列表 CRUD，固定项存 `db.rs` 的 `meegle_pins` 表），`bin.rs` 定位可执行文件：`FALCON_MEEGLE_BIN`（文件得真在）> 编进发布二进制的那份（`embed-meegle`，首次用时释放到 `<dataDir>/bin/meegle-<哈希>`）> `native/.cache/meegle/bin/` 里 `cargo xtask meegle` 取下的锁定版本（开发构建；版本与 sha512 锁在 `xtask/src/meegle.rs`）> PATH。CLI 的脾气（错误信封在 stderr、未登录时一切命令都是 unknown command、视图只能按关键字搜、待办没名字要 MQL 补、view search 限 5 qps）全写在 `command.rs` 顶部注释，改动前先读。
- `px0/` 是「用 px0 审阅」（ADR 0017）：在项目宿主机上按需拉起 px0，经 falcon **同源**反代到 `/px0/<项目 id>/`（`api/px0.rs`，hyper 逐请求开连接、流式转发；鉴权就是登录 cookie，与 `/api/` 同一口径）。`command.rs` 纯函数（资产映射、**钉死的 sha256**、argv、端口解析、远端安装 / 启动命令），`bin.rs` 在后端本机下载校验、SSH 项目再经 stdin 推到远端（不让远端自己下），`manager.rs` 管实例——**本地与远端都挂在 pty 上**，后端死了 px0 跟着挂断，别改回普通 spawn / 无 pty 的 exec；远端连接直接走 forward_out，本机不另开监听端口；反代的租约随响应体释放，空闲回收据此计数。`proxy.rs` 的头过滤有讲究：剥 falcon 的 cookie、只替同源请求改写 Origin（px0 的 localPost 要 Origin == Host）、去 set-cookie。同源意味着 px0 的前端能调 falcon 全部接口——**只在本机 / 内网可接受，公网访问前先挪到独立源**。原生客户端不嵌 px0，菜单项把地址交给系统浏览器；没登录的浏览器被送去 `/?next=<px0 地址>`，登录后由 web 的 `lib/loginNext.ts` 跳回（只认 `/px0/` 开头）。
- `api/` 是 HTTP 面（axum）：路由路径、请求 / 响应体、状态码、错误形状（`{ error }` 与 Fastify 的 500 形状，`api/error.rs`）都照原 Node 版。请求体用 `LenientJson` 收、按 JS 口径逐字段取（`api/input.rs`），不先反序列化成强类型——那会把"某个字段类型不对"变成整条请求 400。`/api/*` 与 `/px0/*` 的登录检查在 `require_login`，各自验身份的（原始字节令牌、应用图标取图、askpass helper）挂在 `public_router`。前端产物：`FALCON_WEB_DIST`（要真有 index.html）> 编进二进制的那份（`embed-web`）> 开发默认目录。
- `db.rs` 用 rusqlite（bundled SQLite），与 Node 版同一个 `falcon.db` / `secret.key` 格式。外键约束显式关闭，级联在应用层手写；`migrate()` 是幂等的 `CREATE TABLE IF NOT EXISTS` + 加列，没有版本号迁移表——改表结构就往这套里加。
- Windows 远端的所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。
- 测 SSH 只用隔离的 sshd（`SetEnv HOME=` 指到临时短路径），别对自己真实的家目录跑 probe / 安装；开发实例端口用 4940–4999、数据目录用短路径（zellij socket 上限 104 字节），从 falcon 终端里起要 `env -u FALCON_WEB_DIST -u FALCON_MEEGLE_BIN -u FALCON_SESSION_ID`。

### client（`native/crates/falcon-app` 及其下层 crate）

Rust + GPUI 写的客户端，连 falcon 服务端（REST + `/ws/sessions/:id` + 登录 cookie）。一套代码两种产物：原生桌面（`src/main.rs` → `run_desktop`）与浏览器 wasm（`falcon-web` 薄壳 → `run_web`，服务端托管）。定下来的做法与踩过的坑在 ADR 0015，提案原文在 `docs/design/gpui-client.md`，浏览器版在 `docs/design/rust-unification.md`（附录 B 是浏览器端还没补齐的缺口），改动前先读。

```bash
cd native
cargo run -p falcon-app                         # 连本机服务（按已装 LaunchAgent 的端口，没装是 4923）；FALCON_LOCAL_URL 可改指别的实例
./scripts/build-web.sh                          # 浏览器版，产物 target-wasm/dist
```

- **crates.io 走 `native/.cargo/config.toml` 里的 rsproxy 镜像**：这台机器上 Clash 的 fake-ip 把 index.crates.io 解析坏了。网络正常的机器删掉那一段即可。
- 分层：`falcon-proto`（协议类型）→ `falcon-client`（REST / WS，与执行器无关的 future，原生上自带 tokio 运行时，浏览器上走 fetch / `web_sys::WebSocket`）→ `falcon-term`（alacritty_terminal + 从 Zed 抄的按键编码 + 鼠标 / 滚轮 / 模式跟踪）→ `falcon-theme`（主题系统：Ghostty 主题文件即数据模型、浅深双槽位、整套界面色由一套主题派生，ADR 0006 / 0011）→ `falcon-core`（与 GPUI 无关的纯逻辑：列式工作区排布 `layout.rs`、窗口 key `pane_key.rs`、会话标题 `session_title.rs`、快捷键、文件路径 / 搜索 / 树、git 图等；`tests/vectors/` 是回归向量）→ `falcon-app`（唯一依赖 GPUI 的 crate）。**纯逻辑一律放下层 crate**，GPUI 升级只波及 falcon-app。
- GPUI 走 `gpui-kit` 总包（`=` 精确锁版本）；Zed 的终端代码按文件抄（`terminal/element.rs`、`falcon-term/src/keys.rs`，文件头注明出处 commit），不按 crate 依赖——会带进第二份 gpui。`native/` 因此是 GPL-3.0-or-later。`native/vendor/` 下的 alacritty_terminal 与 gpui-pre-web 是带补丁的 fork，改动清单在各自 Cargo.toml 顶部，升级 gpui-kit 时重放。
- 文案：`t!("key")`，**界面上不允许硬编码中文**。共用文案在 `falcon-app/locales/zh-CN.json`（v1 只有中文），原生 / 平台独有的放 `falcon-app/i18n-native/<区域>.json`，挂在 `native.<区域>` 下。
- 界面色**只准用 falcon-theme 派生出的语义 token**，不许写死颜色；新增语义色去 `falcon-theme/src/derive.rs` 加。派生规则有冻结的金标准 fixture（`tests/derive_golden.rs`），有意改规则要连 fixture 一起改并写清楚。**深浅按主题底色亮度切，不按系统明暗**。内置主题 = Falcon 两套 + Ghostty 全部 463 套（`falcon-theme/data/`，`cargo xtask vendor-themes` 从本机 Ghostty.app 或 GitHub 重新生成）。
- 界面骨架是**浮动岛**（ADR 0011）：窗口底上浮着侧栏 / 主区 / 右面板几块圆角面板，之间只有一道缝，**不要加分栏边框**；岛里再嵌一块用"借窗口底色"的下沉块，也不要用边框。主区是**列式工作区**（ADR 0012）：窗口排成列、每列可叠多扇、列分在一块块画布上、不横向滚动，排布与拖拽落点的纯函数在 `falcon-core/src/layout.rs`，改之前先读 ADR 0012。
- 会话**默认不起名**（`name` 空串），界面显示的是自动标题：手起的名字 → 前台命令 → agent 的 CLI 名 → shell 命令名（`falcon-core/src/session_title.rs`）。列表里各处必须叫同一个名字；窗口标题栏例外——它右边就是完整工作目录，没标题时那一格空着，别编占位名。前台命令由服务端探测后经 WS 的 `{type:"title"}` 推来（只在有 Viewer 时探、2.5s 节流、后台会话的标题会陈旧）。
- 字体：`falcon-app/build.rs` 把 `native/assets/fonts/` 里的 woff2（`cargo xtask vendor-fonts <berkeley|ioskeley|maple|nerd>` 从官方发行包生成、已进仓库；要 woff2_compress 与 hb-subset）解成 TTF：原生嵌进二进制，浏览器版只嵌正文字体、其余由 build-web.sh 拷进产物按需拉（GPUI 不认 WOFF2）。正文是 Berkeley Mono TX-02（商业字体，zip 不进仓库），缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
- 应用图标（ADR 0018）：选择存在服务端 settings 表；内置图标的图形在 `native/xtask/src/icons.rs`，`cargo xtask icons` 出浏览器版的 `native/web/icons/`（服务端 `/api/app-icon/*` 与 PWA 清单跳到这里）与原生的 `falcon-app/assets/app-icons/`；增删图标要同时改 `xtask/src/icons.rs` 与 falcon-core 的 `APP_ICON_IDS`（服务端与客户端都用它，falcon-app 有测试对账）。
- **验证界面靠 Metal 回读截图**（锁屏 / 远程也能用）：`cargo build -p falcon-app --features snapshot`，再用 `FALCON_AUTOMATE="ready;select:<项目>;new-terminal;type:ls\r;wait:1000;snap:/tmp/a.png;quit"` 驱动（步骤全表见 `falcon-app/src/automation.rs`，`type:` 里的 `;` 写成 `\x3b`），配 `FALCON_NATIVE_DATA_DIR=<临时目录>`（别写进用户真实的 ~/Library/Application Support/Falcon）与 `FALCON_LOCAL_URL`（指向测试服务端，别用 4923）。点击坐标是窗口逻辑像素（截图 PNG 是 2 倍）。锁屏时显示链路不走，`snap` 自己会先画两帧，别把"截图是空的 / 旧的"当成界面 bug。浏览器版用 Chrome DevTools 驱动：canvas 上的合成指针事件在 DPR 2 时坐标要乘 2，键盘输入用真实按键（`type_text`）而不是合成 KeyboardEvent。
- **压测用 `--features automation --release`**（不带 snapshot 的 test-support）：`frames:<ms>` 以 60Hz 手动画并打印帧耗时 p50 / p95，`frames:<ms>:refresh` 无视视图缓存。侧栏与右侧面板是 `cached` 视图（ADR 0015），新加的大块视图照此办理。
- **浏览器版**：平台差异写 `cfg(target_family = "wasm")`，DOM 胶水放 `falcon-app/src/web.rs`。几条硬规矩：时间一律 `web_time::{Instant, SystemTime}`（std 的在 wasm 上一调就 panic）；future 的约束写 `MaybeSend`（falcon-client / falcon-core 各有一份），别写死 `Send`；static 里放不了在飞的 future，wasm 上用 thread_local。wasm 构建走 rustup 的 stable + `wasm32-unknown-unknown`，wasm-bindgen-cli 与 Cargo.lock 同版本（PATH 上排前面的 Homebrew rustc 没有 wasm 标准库，脚本里处理了），**不要 nightly**。宿主页是 `native/web/index.html`（首帧主题脚本、manifest、favicon）。
- HTML 预览：原生走默认开启的 `webview` feature（`--no-default-features` 退成"在浏览器中打开"）；字节一律走原始字节路由 `/api/projects/:id/raw/<token>/<path>`（ADR 0007），沙箱不给 allow-same-origin、凭据是只能读该项目文件的作用域令牌，别为了"方便"放宽。
- **通知一律走 `falcon-app/src/toasts.rs` 的 `ToastExt`**，不用组件库的 `push_notification`（它的通知层会被对话框盖住，见 ADR 0015）。
- **界面尺寸写 `zoom::zpx(..)`，不写 `px(..)`**：界面缩放（⌘+ / ⌘−）= rem 与 zpx 一起乘倍数；`px` 只留给画布几何、终端画面、窗口外框这类真实像素（`zoom.rs` 顶部有清单）。`theme.font_size` 就是 rem，必须是 16 × 倍数，别再拿它当正文字号。

## 约定

- 术语以 [CONTEXT.md](./CONTEXT.md) 为准，包括 _Avoid_ 列表——那里写的不只是命名偏好，Detach/Terminate、源项目/附属项目、宿主机/远端主机这些区分直接对应代码里的分支。
- 注释解释的是"为什么"和踩过的坑（多半是实测出来、文档里查不到的），密度偏高是刻意的；改动附近代码时保持同样的说明力度，注释与代码不符时先修注释。
- 架构决策写在 `docs/adr/`：0001 是持久会话为什么选 Zellij 及一长串实现要点，0002 是附属项目与删除护栏，0003 是多仓库项目与批量派生（含回滚与包围盒断言），0005 是 rio 引擎的自持装配层与 WebGPU 渲染器（含踩坑清单），0006 是主题系统（Ghostty 主题格式、双槽位、界面色派生规则、首帧策略），0007 是原始字节路由与 HTML / 图片预览沙箱（路径形状路由、作用域令牌、响应头护栏、图片缩放模型），0008 是文件下载 / 上传（流式 exec 通道、Windows 的 base64 行协议、临时文件 + 字节数核对的落盘规则），0009 是文件面板目录浏览器（平铺当前目录、mkdir / rename / remove、文件夹上传），0010 是飞书项目面板（宿主机上的 meegle CLI 当数据源、实测的 CLI 输出约定、device-code 登录、两层 TTL 缓存），0011 是浮动岛骨架（窗口底与圆角面板、圆角阶梯、窗口底只压亮度不洗色度与色域收缩、哪些地方刻意没改），0012 是列式工作区（列 → 窗口的排布模型、为什么必须绝对定位、落点坐标的两套下标、把手与持久化、多画布与自动另起一块），0013 是 agent 会话（开场 CLI 的启动脚本、为什么不能读 $SHELL、CLI 缺失时的退路），0014 是公网发布（Cloudflare Quick Tunnel、只在后端本机跑 cloudflared、远端经 SSH 桥），0015 是原生客户端（GPUI 薄客户端、crate 分层、Zed 代码按文件抄、appearance 先于 resize、一台服务端一个窗口、测量数据与踩坑清单），0016 是中转按机器挂（端口转发与公网发布从项目 / 右侧栏搬到主机 / 设置页、同端口互斥的槽位口径、旧规则迁移与删主机级联），0017 是用 px0 审阅（宿主机上跑 px0、同源反代的代价与前提、二进制钉哈希后由后端推到远端、pty 收尸、Origin 改写），0018 是应用图标（按服务端存的选择、公开的取图路由、客户端规整自定义图、一份定义出四种形状、原生运行时换 Dock 图标及其限制），0019 是终端滚动条（CLI 为什么问不到位置、zellij 插件按需一问一答、推插件与预授权、新旧会话两套配置、升 0.45.1 的 scroll_mode_sync 坑）。做相关改动前先读对应 ADR。
