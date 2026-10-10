# Falcon 全面 Rust 化：Rust 服务端 + gpui-kit 统一客户端

> 状态：**C0 完成**（测量结果见 §9）；**S 线完成、Node 服务端已删除**（2026-10-10，见 §11；TS 原文在提交 `fd9022a`）；**React 前端与 shared 已提前删除**（2026-10-10，用户拍板，见 §12；TS 原文在提交 `9c9d045`），C1–C3 变成在浏览器版上补回功能 · 范围：`packages/server` 用 Rust 重写；`native/` 的 GPUI 客户端拆成一套代码、两种产物（原生桌面 + 桌面浏览器 wasm）；删除 React 前端（`packages/web`）与移动端；`packages/shared` 随之退役 · 方向由用户拍板（§10），本文给做法、分期与风险
>
> 术语一律沿用 [CONTEXT.md](../../CONTEXT.md)。"falcon 服务端"指跑着 falcon server 的那台机器，与**宿主机**、**远端主机**是三件事（同 [gpui-client.md](./gpui-client.md) §0）。

---

## 0. 一句话

**后端换成 Rust，协议与会话语义一个字节不改；客户端只剩一套 gpui-kit 代码，同时出原生桌面版与桌面浏览器版；移动端不再支持。**

这推翻了 [ADR 0015](../adr/0015-gpui-native-client.md) 的"决定一：只换前端，后端与协议不动"。当时的两条理由仍然成立，本方案逐条回应：

- "会话持久性的全部承诺都在后端，重写一遍只会重新踩坑" → 坑不靠记忆守，靠**机器对拍**：纯函数层由 TS 导出测试向量逐字节对拍（§5.1），会话层用"TS 建会话 → 停 → Rust 接回"做验收（§5.2），附录 A 是从源码与 ADR 摘出的必须原样保留的行为清单。
- "瓶颈不在 Node" → 这次不是为了快。收益在别处：协议与纯逻辑只剩一份 Rust 实现（`falcon-proto` / `falcon-core` 从"镜像"变成"真相"），去掉 Node SEA 打包与 node-pty 原生模块释放，单个原生二进制，以及给 Windows 服务端留出路（meegle 自带 Windows 二进制，`portable-pty` 有 ConPTY）。

---

## 1. 现状与目标

| | 现状 | 目标 |
|---|---|---|
| 服务端 | TypeScript 约 2.1 万行（非测试），Fastify + ssh2 + node-pty + `node:sqlite`，Node SEA 单文件 | Rust（axum + russh + portable-pty + rusqlite），单个原生二进制 |
| 协议真相 | `packages/shared`（TS），`falcon-proto` 是手写镜像 + fixture 契约测试 | `falcon-proto` 即真相；`packages/shared` 删除 |
| 桌面客户端 | `native/` GPUI 原生客户端 | 不变，拆出平台层后行为不变 |
| 浏览器客户端 | React + xterm.js / rio，约 3.3 万行 TS/TSX，含移动壳 | 同一套 gpui-kit 代码编到 `wasm32-unknown-unknown`，**只支持桌面浏览器** |
| 移动端 | `MobileShell` / `MobileSwitcher` / `lib/mobileNav.ts` / `lib/useIsMobile.ts` | 删除 |
| 仓库 | pnpm workspace（shared / server / web）+ 独立 Cargo workspace `native/` | 只剩 Cargo workspace（是否挪到仓库根见 §10 待定）；JS 只留构建 / vendor 脚本与浏览器版的宿主页 |

---

## 2. 关键决定

### 决定一：协议与会话语义不变，只换实现语言

REST 路径、请求 / 响应体、错误体形状（`{error}`，有的带 `reason` / `code`）、WS 二进制帧（1 字节类型头 + UTF-8）、JSON 控制消息、close code 4401、cookie 名与属性、rawToken 路径形状、`publicAsset` 路由、px0 同源反代——全部按现状搬。原生客户端在服务端切换前后**不改一行**就能连，这本身就是验收条件之一。

服务端对外面共 94 条路由（含 2 条 WS），分组与出处见附录 C。

### 决定二：一次性替换，不做按路由反代的渐进迁移

`SessionManager` 在内存里独占会话、链路、登录令牌，文件、px0、中转都依赖它；拆成"Rust 在前、Node 在后"两个进程收益很小、状态同步代价很大。两个版本**也不能同时开在同一个数据目录上**。

所以 Rust 服务端在旁边长到功能齐全（用独立的数据目录与端口开发，fixture 与 e2e 对拍），到 S7 一次切换。切换就是换二进制：同一个数据目录、同一个 `falcon.db`、同一个 `secret.key`，宿主机上的 Zellij 会话原样接回。

### 决定三：迁移期间协议冻结

从 S0 到 S7 切换完成，协议只修 bug、不加字段。确有必要的改动必须在 Node 与 Rust 两边同一个提交里落地，并重生 fixture。否则 TS 服务端、Rust 服务端、原生客户端、React 前端四方会对不上。

### 决定四：服务端选型

| 能力 | 选型 | 理由 / 必守的点 |
|---|---|---|
| SSH | **russh** | Falcon 只用了 ssh2 的一小部分：exec / pty / direct-tcpip / tcpip-forward / agent / TOFU，没用 SFTP、ProxyJump、ssh_config、`shell()`。libssh2（ssh2-rs）的 Session 不是线程安全的，一条连接上并发跑多路 PTY + 转发 + 探测会互相阻塞，否决。需实测：TOFU 指纹格式（`hex(sha256(主机公钥 blob))`，与 ssh2 一致，否则所有已存主机报指纹不符）、旧式加密 PEM（russh 只认 AES-128-CBC）与 SEC1 EC 私钥、Windows 命名管道 agent、开 legacy 算法的老设备 |
| 本地 PTY | **portable-pty** | Unix openpty、Windows ConPTY；读端阻塞，一个 PTY 一条线程。自己设 `TERM`、自己做 UTF-8 跨块拼接；前台进程名 macOS 走 libproc、Linux 读 `/proc/<pid>/comm` |
| HTTP / WS | **axum 0.8 + tower-http + axum-extra（CookieJar）** | 鉴权钩子改成"受保护 / raw / public / askpass"四组 Router；WS 没有 `bufferedAmount`，每个 Viewer 一条有界写队列 + 字节计数，自己做 4MB / 256KB 高低水位；`DefaultBodyLimit` 分路由（JSON 1MB、图片 20MB、上传不限） |
| 数据库 | **rusqlite（bundled）** | 保留 WAL + `synchronous=NORMAL`、外键关闭、`CREATE TABLE IF NOT EXISTS` + 加列的幂等迁移、`mojito.db` 回退、启动时 active → unverified / dead |
| 加密 | aes-gcm、scrypt、sha2、subtle | **与旧数据字节兼容**：存储格式是 `base64(iv12 ‖ tag16 ‖ ct)`，aes-gcm crate 输出 `ct ‖ tag`，要重排；scrypt 用 Node 默认 N=16384、r=8、p=1、keylen=32 |
| px0 反代 | hyper `client::conn::http1` 跑在 TcpStream / russh ChannelStream 上 | SSE 边收边发（`Body::from_stream`）；每请求一条新连接（远端每条连接是一条 forwardOut 通道）；头过滤逻辑照搬 `px0/proxy.ts` |
| 静态资源 | rust-embed | 浏览器版的宿主页 + wasm + 字体从内存直接服务；`FALCON_WEB_DIST` 覆盖保留，但要校验目录里真有 index.html |
| 第三方二进制 | meegle：vendor 脚本从 npm tarball 抽出、按 lock 里的 sha512 校验，build.rs `include_bytes!`；zellij 滚动插件 wasm：`include_bytes!` + build.rs 算 sha256 | 运行时释放规则照搬 SEA bootstrap（`<dataDir>/runtime/<hash>/bin/`、`.complete` 标记 + 原子 rename、清理旧版本） |
| 日志 | tracing | — |

### 决定五：服务端 crate 放进 Cargo workspace，复用现有下层 crate

新建 `falcon-server`（lib + bin），path 依赖 `falcon-proto`。`falcon-proto` 需要补齐只在 falcon-client 里有、只序列化的请求体，让服务端能反序列化。`ClientMessage` 的宽松语义要先解成 `Value` 再手判：cols=0 时静默忽略，seek 非法时当成"只问位置"。

`TermModeTracker` **单独搬一份**到不依赖 alacritty 的位置（比如 `falcon-proto` 或新 `falcon-vt`），不要让服务端依赖 `falcon-term`，否则会把 Zed 的 alacritty fork 一起拖进来。`termEnv` 的运行时部分（`OscColorGate`、`termPtyEnv`、`oscColorReplies`）目前没有 Rust 版，要补。

### 决定六：客户端一套代码、两种产物，靠平台 trait 而不是到处 `cfg`

```
falcon-proto / falcon-theme / falcon-core   不变（时间统一用 web-time）
falcon-term       自定 vte Timeout；alacritty fork 打补丁关掉 tty（§4.2）
falcon-client     对外 API 不变；内部 transport/{native,web} 按 target 选
falcon-platform   新：平台能力 trait
falcon-ui         现 falcon-app 去掉平台相关的约 2.6k 行，只认 falcon-platform
falcon-desktop    bin：main、本机服务托管、连接窗口与 profiles、钥匙串、wry、Dock、automation / snapshot、build.rs 嵌字体
falcon-web        cdylib：wasm 入口、localStorage、DOM 胶水（文件选择 / 下载 / 拖放 / iframe 叠层 / favicon）、字体异步加载
```

`falcon-platform` 的 trait 包括：

- KvStore：`prefs.json` 或 localStorage，**键名与现在 web 完全一致**，老用户的主题 / 终端设置直接继承；
- FilePicker；
- Upload / Download；
- HtmlOverlay：wry 或 `<iframe sandbox>`；
- AppIconSink：Dock 或 favicon；
- ServerSource：profiles 或 `location.origin`；
- Secrets：仅桌面；
- LocalService：仅桌面；
- PlatformInfo：`is_mac`、是否浏览器、是否安全上下文。

**C0 的实际做法（比上面轻）**：`falcon-app` 改成 lib + bin，新增很薄的 `falcon-web`（cdylib，只导出 `start()` 调 `falcon_app::run_web`）。平台差异先用 `cfg(target_family = "wasm")` 落在各自模块里：prefs / profiles / workspace/persist 走 localStorage，fonts 只嵌正文字体、其余运行时拉，files/transfer 的下载走 `<a download>`，window 不托管本机服务，DOM 胶水集中在 `falcon-app/src/web.rs`。这样改动面约 15 个文件，原生行为不变。是否再抽出 falcon-platform / falcon-ui / falcon-desktop，留到 C1 按 cfg 的实际分布再定——cfg 不多就不拆。

切分规则有两条：

- **物理上不存在的能力用 `cfg(target_family = "wasm")` 加 target 专属依赖**：tokio 的 rt-multi-thread / net / fs、rustls、tungstenite、keyring、directories、wry、objc2。
- **产品取舍用 feature**：webview、automation、snapshot、syntax-highlight，只在桌面入口打开。注意 gpui-kit 的 `tree-sitter-languages` feature 必须挂在 `[target.'cfg(not(target_family = "wasm"))'.dependencies]` 上，否则 wasm 编不过。

### 决定七：浏览器版只做桌面浏览器、单线程、不做无障碍

- **单线程**（`gpui_platform::single_threaded_web`）：不需要 nightly 和 build-std，也不需要 COOP/COEP。COEP 会要求 HTML 预览 iframe、px0、外链图片都带 CORP 头，代价太大。后果是后台任务全在主线程上，4MB 回放解析要分块让出主线程，这和现在 web 在主线程解析回放是同一个水平。
- **只支持桌面浏览器**：Chrome / Edge / Safari / Firefox 的近两个大版本，WebGPU 优先、WebGL2 回落。手机浏览器打开时给一句"请用桌面浏览器"，不做任何适配。
- **无障碍**：gpui-web 没有无障碍树（GPUI core 有 AccessKit，web 平台没接），随"全部用 gpui-kit"的决定接受。
- **浏览器原生能力随之失去**：⌘F 页面查找、任意 DOM 文本选中、翻译。需要的地方（文件、差异）用应用内查找补。

### 决定八：gpui-pre-web 的缺口在 fork 里补，按文件记出处

gpui-kit 0.7.1 的 Web 后端就是 Zed `crates/gpui_web` 的重发（`gpui-pre-web` 0.3.8，快照点 zed@279fe07）。gpui-kit 文档把 WebAssembly 标为 "Showcase only — not to ship applications"。Falcon 会是第一个在上面跑的完整应用，平台层缺什么就在 fork 里补。

做法：`native/vendor/` 下放 fork，workspace 用 `[patch]` 指过去，改动清单写在各自 Cargo.toml 顶部，升级 gpui-kit 时对着新快照重放。能回馈上游的（IME 候选框定位）提 PR。具体缺口见附录 B。C0 已落的两份：

- `vendor/alacritty_terminal`（Zed fork @ 4c12966）：加默认开启的 `tty` feature，event_loop / thread / tty 与 polling、rustix 等挂在它下面，falcon-term 关掉它。
- `vendor/gpui-pre-web`（0.3.8）：① default features 去掉 `multithreaded`（wasm_thread 只能 nightly 编）；② 实现 `update_ime_position`，并在 `compositionstart` 时主动向 input handler 拉光标格（`ime_candidate_bounds`）——浏览器只有"推"，macOS 是输入法弹出时向应用"拉"，终端只在提交后才推，不补这一拉候选框就还在左上角。

---

## 3. 服务端分期（S 线）

| 期 | 内容 | 验收 |
|---|---|---|
| **S0 对拍基建** | `falcon-server` 骨架；TS 向量导出脚本（照 `export-ghostty-themes.mjs` 的路子，用 tsx 跑 TS 纯函数落盘 JSON）；`gen-fixtures.mjs` 与 native e2e 的服务端入口参数化（Node / Rust 二选一） | 向量导出跑通，Rust 侧有读向量的测试骨架 |
| **S1 纯函数层** | zellij command / host、git path / command、termEnv、relay / forward / share spec、viewerArbiter、termSize、agent、askpass scripts、shells、virtualdir、paste、cloudflared / px0 / meegle 的 command、px0 头过滤、base64 行编解码、appIcon | 约 30 个测试文件、380 个以上用例的向量**逐字节**通过：发到宿主机的命令串、kdl、启动脚本差一个字节，老会话就可能接不回 |
| **S2 基础与 HTTP 骨架** | config、crypto、auth、db；axum 鉴权分组、静态托管、错误形状；auth / system / app-icon / hosts / projects 的增删改查 | **Rust 打开现有 `falcon.db` + `secret.key` 并解密出主机凭据**；对应路由 fixture 一致 |
| **S3 执行层** | localExec（ExecFn 语义：非零退出码是正常返回值）；SshLink（russh：connect、TOFU、exec、stdin EOF、流式通道、两个方向的转发）；probe；zellij install / prepare；`/ws/install` | §2 决定四列的 SSH 四项实测通过 |
| **S4 会话核心** | 本地 PTY / SSH 两种 Backend、manager（RingBuffer、16ms 合并、replay 前缀、Viewer 仲裁、退避重连、标题探测、滚动查询）、`/ws/sessions` | native e2e 全绿；React 前端手测；**TS 建会话 → 停 → Rust 接回**（本地持久、SSH POSIX、SSH Windows 各一次） |
| **S5 文件与 git** | fs、files、transfer、paste、virtualdir；git 全部路由、worktree 派生、删除护栏（`git/remove.ts` 的向量先对拍完）、存档清扫 | fixture 一致；上传中途断开不留截断文件；删除护栏矩阵全绿 |
| **S6 外围** | 中转（转发 + cloudflared 发布）、askpass、px0（pty 收尸、SSE、Origin 改写）、meegle（进程、TTL 缓存、5 qps 退避） | 各功能真机走一遍 |
| **S7 打包与切换** | 内嵌 web 产物、meegle、滚动插件；`falcon service install`（launchd / systemd）；macOS pkg 的 Resources 里换成 Rust 二进制；删 `packages/server`、SEA 脚本 | 在用户真实数据目录上切换，所有会话接回；之后 fixture 改由 Rust 服务端生成 |

粗估（一人全职）：S0–S1 2–3 周，S2–S4 4–6 周，S5–S6 4–5 周，S7 1–2 周，合计约 3 个月。最大的不确定在 S3（russh 与各类密钥、Windows 远端）和 S4（并发状态机、背压）。

---

## 4. 客户端分期（C 线）

| 期 | 内容 | 验收 |
|---|---|---|
| **C0 wasm spike** ✅ | 不重构，用最小的 cfg 把"登录 → 一扇终端"编到 wasm 并跑起来：单线程 gpui-web、web-sys WebSocket、alacritty 补丁、web-time | 量出 wasm 体积（原始 / brotli，不含 CJK）、冷 / 热启动时间、键入回显延迟；中文 IME 在 Chrome / Safari 上能上屏；WebGPU 与 WebGL2 都能画。**结果写进本文 §9**，体积或 IME 有硬伤就先补 fork 再往下走。实际做到了整个 falcon-app 编到 wasm，见 §9 |
| **C1 拆层** | 拆出 falcon-platform / falcon-ui / falcon-desktop；时间统一 web-time；键位改为运行时调 `falcon_core::shortcuts`（删掉三处 `cfg(target_os = "macos")`） | 桌面版行为不变：`cargo test --workspace`、e2e、截图自动化全绿 |
| **C2 falcon-client 双传输** | 执行器胶水、`MaybeSend`、按 target 选 HTTP 实现、浏览器 cookie 罐模式（`token()` 恒为 None，401 直接推 LoginRequired）、web-sys WebSocket、会话 Driver 改写成 futures + Timer（浏览器不能发 ping，改用 online / visibilitychange 触发重连）、上传走 XHR（有进度）、下载走 `<a download>` | wasm-bindgen-test 加 headless 浏览器跑通登录、建会话、收发帧 |
| **C3 falcon-web 功能对齐** | 附录 B 的全部缺口；宿主页（首帧主题脚本、加载页、manifest）；字体异步加载；外链图片的服务端代理路由；Web 版语法高亮（§10 待定） | 按 [gpui-client.md](./gpui-client.md) §5 的功能表，在桌面浏览器上逐项走通 |
| **C4 删除** | 删 `packages/web`、移动端、`packages/shared`、pnpm workspace 里随之失效的部分；README / CLAUDE.md / CONTEXT.md / ADR 更新 | 仓库里不再有 React；`pnpm build` 只剩构建脚本，或改成 cargo / just |

粗估：C0 1–2 周，C1 1.5 周，C2 2–2.5 周，C3 4–6 周（含三家浏览器真机验证），C4 0.5 周，合计约 2–2.5 个月。

### 4.1 两条线的顺序

```
C0（先打穿最大的未知数）
  → S0 → S1 → S2 → S3 → S4 → S5 → S6 → S7（切换，删 Node）
  → C1 → C2 → C3 → C4（删 React）
```

- **C0 放最前**：服务端重写是已知量；浏览器版能不能以可接受的体积和 IME 体验跑起来是最大的未知数，先花一两周量清楚。
- **S 线整体先于 C1–C3**：服务端切换影响所有现存会话，越早稳定越好；客户端重构期间 React 前端照常可用。C1 不依赖服务端，若 S 线卡在 SSH 实测上，可以穿插做 C1。
- **"去掉移动端"不单独排期**：React 前端在 C4 整体删掉，移动壳跟着走；在那之前不再修移动端的 bug。如果想早点清掉，`MobileShell` / `MobileSwitcher` / `lib/mobileNav.ts` / `lib/useIsMobile.ts` 和 `App.tsx` 里的分支可以随时单独删。

---

## 5. 验证

### 5.1 共享测试向量

Server 与 shared 现有 38 个测试文件，约 436 个用例。

- **约 30 个文件、380 个以上用例是"输入 → 字符串 / argv / JSON"的纯函数**，由 TS 导出为 JSON 向量，Rust 读同一份向量对拍。价值最高的是 `git/command.test`（56）、`virtualdir.test`（25）、`zellij/command.test`（20）、`files.test`（19）、`meegle/command.test`（19）、`viewerArbiter.test`（18）和 `git/remove.test`（15，删除护栏）。
- **db、auth、askpass hub、ttlCache、config、cloudflared/bin** 在 Rust 侧原生重写测试。

导出脚本在 S7 之后退役，向量 JSON 留在仓库里当回归基线。

### 5.2 契约与端到端

- **fixture**：`native/scripts/gen-fixtures.mjs` 对 Node 与 Rust 服务端各跑一遍，落盘的响应必须一致（排除时间戳、随机 id 等字段）。
- **native e2e**（`FALCON_E2E=1`）的服务端入口参数化，两个后端各跑一遍。
- **接回测试**：持久会话本来就设计成能扛过后端重启。用 TS 版建会话，停掉，再在同一个数据目录上启动 Rust 版，会话原样接回、回放正确、输入正常。这是对 Zellij 命令、会话名、配置文件、DB 兼容性最强的一次性检验。
- 会话 / SSH / Zellij 仍然只能真机验，和现在的口径一致。

---

## 6. 工程结构

- Cargo workspace 新增 `falcon-server`、`falcon-platform`、`falcon-ui`、`falcon-desktop`、`falcon-web`。`falcon-app` 在 C1 拆完后消失。
- `native/.cargo/config.toml` 的 rsproxy 镜像照用。wasm 需要 `rustup target add wasm32-unknown-unknown`，还要一份与 lock 文件版本一致的 `wasm-bindgen-cli`。
- 服务端进入 GPL-3.0-or-later 的 workspace，整个产品随之 GPL（个人使用，与 `native/` 的既有立场一致）。
- 门禁：`cargo check` / `cargo test --workspace` 保持零警告；新增 `cargo check -p falcon-web --target wasm32-unknown-unknown`。React 删除之前，`tsc` 门禁照旧。

---

## 7. 考虑过的方案

- **只重写后端，浏览器端保留 React**：风险最低，但两套界面长期并存，与"统一客户端技术方案"的目标相悖。否决（用户）。
- **浏览器端只把纯逻辑编成 wasm 给 React 用**：能消掉一部分重复实现，界面仍是两套。否决。
- **GPUI-web 只管桌面、手机继续用 React 壳**：仍是两套界面。随"去除移动端"一起否决（用户）。
- **服务端 SSH 用 libssh2（ssh2-rs）**：Session 不是线程安全的，一条连接上多路并发要靠全局锁，否决（§2 决定四）。
- **绞杀式迁移（Rust 在前按路由反代 Node）**：见 §2 决定二，否决。
- **浏览器版开多线程**：要 nightly、build-std 和 COOP/COEP，后者波及 HTML 预览、px0、外链图片。先单线程；只有 C0 / C3 量出主线程卡顿是真实问题时再评估。

---

## 8. 风险

| 风险 | 后果 | 对策 |
|---|---|---|
| gpui-pre-web 不成熟（上游定位 "basic" / "Showcase only"，Falcon 是第一个完整应用） | 浏览器版的 bug 只能自己修；每次升级 gpui-kit 两端一起验 | fork 补丁按文件记出处（决定八）；C0 先量；能回馈的提 PR |
| 中文 IME 候选框不跟光标（`update_ime_position` 是空函数，隐藏 textarea 固定在页面左上角） | 中文输入体验差 | C0 就在 fork 里实现：把隐藏 textarea 移到 `bounds_for_range` 给出的光标位置 |
| wasm 首屏体积（gpui-kit 自己的演示页 42MB 原始 / 10MB gzip；Maple 中文字体单个 TTF 21MB） | 首次打开慢 | CJK 与 Nerd Font 运行时异步拉取，到位前走 Canvas 回落；`wasm-opt -Oz` + brotli；服务端带 `application/wasm` 与长缓存头；C0 定体积预算 |
| GPU 设备丢失不能恢复（gpui-web 停止渲染，只提示刷新） | 浏览器切后台久了可能白屏 | 监听后自动 reload 并恢复工作区（工作区布局本来就持久化） |
| russh 与现有密钥 / 老设备不兼容 | 部分 SSH Host 连不上 | S3 用用户真实的主机清单逐台实测；不行的密钥格式在 Host 表单里给出明确报错和转换提示 |
| 附录 A 的某条行为没对齐 | 老会话接不回、Windows 远端命令失败、删错目录 | 向量逐字节对拍 + 接回测试；删除护栏（ADR 0002）的向量最先对拍 |
| 数据兼容（加密字节序、scrypt 参数、TOFU 指纹格式） | 存着的主机密码、访问密码、已信任主机全部失效 | S2 的第一个验收就是 Rust 解密现有库 |
| 工作量（S 线约 3 个月 + C 线约 2–2.5 个月） | 长期停在"一半 Node 一半 Rust" | 两条线都是每期可独立日用；S7 之前 Node 服务端照常是线上版本 |
| 失去浏览器原生能力（查找、选中、无障碍、翻译） | 浏览器版不如现在的 React 版"像网页" | 已接受（决定七）；文件 / 差异视图补应用内查找 |

---

## 9. C0 测量结果

2026-10-10，macOS（Apple M4）上的 Chrome，测试服务端是现有 Node 后端（独立数据目录，端口 4961）。构建：`native/scripts/build-web.sh`（profile `web`：opt-level s + LTO + strip，单线程 gpui-web）。

**能用的部分**：整个 falcon-app（不只是一扇终端）编到 wasm 并在浏览器里跑起来——项目树、会话总览、新建持久会话、终端回放与接回（刷新页面后原内容还在）、英文与中文输入、shell 着色、CJK 两格对齐、图标、右侧活动栏。REST 走 fetch（同源 cookie），终端走 `web_sys::WebSocket`。

**体积**

| 产物 | 原始 | gzip -9 | brotli -11 |
|---|---|---|---|
| wasm，字体全部内嵌（同原生） | 44.2 MB | 18.0 MB | 12.9 MB |
| wasm，只嵌 TX-02（现行） | 18.8 MB | 6.3 MB | 4.2 MB |
| 上一行再过 wasm-opt -Os / -Oz / -O3 | 16.9 / 16.2 / 17.1 MB | 6.4–6.5 MB | 4.4 MB |
| JS 胶水（falcon_web.js） | 0.18 MB | | |
| 运行时拉的字体（TTF）：Nerd 图标 / Ioskeley / Maple CN | 2.6 / 0.65 / 21.3 MB | | |

- wasm-opt 是负收益：原始体积小 10–14%，压缩后反而大 0.1–0.2MB（rustc 的 opt-level s + LTO 已经做完了；它的改写打乱了字节分布）。构建脚本默认不跑，`FALCON_WASM_OPT=1` 才开。
- 现在的 Node 静态托管不压缩，浏览器版上线前服务端要给 wasm / 字体发预压缩的 brotli（S 线 rust-embed 时一并做）。Maple 21MB 要么子集化、要么按 web 现在的 unicode-range 拆片懒加载，否则首次出现中文要等它。

**启动**

- 本机回环：fetch + 实例化 38–55 ms，`start()`（到开出窗口）3–7 ms；CPU 降速 4×：85 ms + 7 ms。编译不是瓶颈（Chrome 的 Liftoff），**首屏时间 ≈ 下载 4.2MB 的时间**：50 Mbps 约 0.7s，10 Mbps 约 3.4s，之后走 HTTP 缓存。
- 回退字体到位（本机）：Ioskeley 7–9 ms、Nerd 11–13 ms、Maple 51–62 ms，到了就 `refresh_windows`，没观察到跳字。

**输入延迟**（给 WebSocket 打桩取时间戳，真实 CDP 键盘事件，逐键）

- keydown → 发出 input 帧：约 1 ms（客户端开销）。
- 发出 → 收到回显帧：约 30 ms，是服务端 16 ms 输出合并 + zellij 的往返，与 web / 原生同一条链路。

**渲染**：WebGPU（Chrome，Metal）与 WebGL2（把 `navigator.gpu` 藏掉后自动回落，ANGLE Metal）画面一致。

**中文输入**：fork 补了候选框定位后（决定八），`compositionstart` 时隐藏 textarea 移到光标格（实测 left 545.2 / top 134 / 高 16，即提示符后那一格），预编辑文本带下划线画在光标处，`compositionend` 提交"你好"只上屏一次。自动化只能发合成的 composition 事件，**真实系统输入法弹窗的位置还要人工在 Chrome / Safari 上确认一次**。

**还没覆盖**：Safari / Firefox；设了访问密码的实例（cookie 罐模式的登录、4401 后重新登录）；断网 / 休眠后的重连；上传（C3）；HTML 预览（C3）；下载（代码已接 `<a download>`，没实测）。

**发现的问题，排进 C1 / C3**

- 键位：wasm 上 `target_os` 不是 macos，菜单里显示 `Ctrl+Shift+T`。要按运行时判断（gpui-web 的 `WebWindowInner` 里已经有 `is_mac`），并给浏览器保留键注册 BrowserAlias。
- 空状态文案里的全角加号（U+FF0B）渲染成了别的字形，疑似回落链上的字形映射错位，C3 查。
- gpui-kit-assets 的 wasm 加载器在图标到位前每帧打一条 "Wasm assets loading" 的 error 日志（上游行为，只是噪音）。
- 驱动 canvas 的合成指针事件在 DPR = 2 时 `offsetX/Y` 只有 `clientX/Y` 的一半（Chrome 行为，只影响自动化脚本，坐标要乘 DPR）。
- 原生的截图自动化会被钥匙串授权弹窗挡住（新编的二进制签名变了，"本机"那条已存密码要重新授权），C1 回归时要给自动化一个跳过钥匙串的开关。

**结论**：没有阻断性的硬伤，C 线按计划往下走。体积（brotli 4.2MB）比 React 版大一截但可接受；IME 的关键缺口已在 fork 里补上。

---

## 10. 已拍板与待定

| 问题 | 结论 | 来源 |
|---|---|---|
| 后端技术栈 | Rust 重写 | 用户 |
| 客户端技术栈 | 原生与浏览器都用 gpui-kit，一套代码 | 用户 |
| 移动端 | 去除 | 用户 |
| 无障碍、浏览器原生查找 / 选中 | 随 gpui-web 现状接受失去 | 随上两条 |
| SSH 库 | russh | 判断 |
| 迁移方式 | 一次性替换，同数据目录兼容 | 判断 |
| 迁移期间协议 | 冻结；必要改动两边同一提交落地 | 判断 |
| 浏览器版线程模型 | 单线程 | 判断 |
| 登录令牌 | 保持只存内存（重启后重新登录）；改成持久化属于行为变更，另议 | 判断 |
| **待定**：Cargo workspace 是否在 C4 后挪到仓库根（`native/` 这个名字届时已不准确） | 倾向挪，C4 时定 | — |
| **待定**：浏览器版的语法高亮（tree-sitter 编不到 wasm） | 倾向 C3 用 syntect 的纯 Rust 正则后端；首版可以无高亮 | — |
| wasm-opt | 不用：压缩后反而变大（§9） | 实测 |
| 浏览器版工具链 | rustup stable + wasm32-unknown-unknown，wasm-bindgen-cli 与 Cargo.lock 同版本；不需要 nightly | 实测 |
| **待定**：PWA | 倾向删掉安装入口，保留服务端的 manifest / 图标路由（标签页图标仍要用） | — |

---

## 11. S 线进展（2026-10-10）

分支 `rust-server`。`native/crates/falcon-server` 已覆盖 Node 版的全部路由，`cargo test -p falcon-server` 689 个用例，零警告。

**执行模型（S4 定下来的）**：会话核心不改写成多线程，而是保留 Node 事件循环的语义——`engine.rs` 在一条专用线程上起 current_thread runtime + LocalSet，SessionManager、SshLink、中转、px0 全是 `Rc` / `RefCell`；axum 处理器经 `EngineHandle::call` 把闭包投进引擎、拿 oneshot 等结果，WS 的 input / resize 经 `send` 保序投递。阻塞的 PTY 读写各开线程；SQLite 照 Node 版直接同步调。好处是 manager.ts 里那些依赖"同步段不会被打断"的状态机（单飞、`attaching`、重连登记在册检查）可以逐行照搬；HTTP 处理器被丢掉（客户端断开）不会打断引擎里在跑的写操作。

**实测过的**

| 场景 | 结果 |
|---|---|
| Node 建的本地持久会话 → 停 Node → 同一数据目录起 Rust | 自动接回（unverified → active），回放里有 Node 时期的输出，之后输入正常 |
| 浏览器版 GPUI 客户端连 Rust 服务端 | 打开会话、回放、中英文输入正常 |
| React 前端连 Rust 服务端 | 新建终端、中文输入、侧栏改动徽标、修改面板提交、历史面板、文件面板与文件查看；控制台与服务端日志都没有报错 |
| 原生 e2e（`FALCON_E2E_SERVER=rust`） | 同一套断言两边都过：设密码、REST 401 与 WS 4401 自动重登、两次重启后接回、Terminate |
| 协议 fixture 对拍（`gen-fixtures.mjs` 对 Rust 跑 + `compare-fixtures.mjs`） | 98 个响应的结构（键集合、值类型、数组长度）全部一致；剩下的只有少数类型的 JSON 键序（serde 按 falcon-proto 的字段序写，客户端不依赖键序） |
| SSH（隔离环境：测试 sshd 用 `SetEnv HOME` 把远端家目录换成 `native/target/h`） | 主机试连、Zellij 远端安装（`/ws/install` 分阶段）、建持久会话、杀掉 SSH 连接 → 数秒内自动重连接回且历史还在、重启 Rust 服务端 → 启动即接回、本地端口转发经隧道可用并随停用拆掉 |
| 附属项目（临时 git 仓库） | 派生、目标占用 409、禁止二级派生、删除前预检看得到脏文件、存档 / 恢复、删除时 worktree 目录随之清掉 |
| px0 | 首次打开下载钉哈希的二进制、状态页、启动后经反代可用；服务端 SIGTERM 时子进程被收掉 |
| 发布构建 `pnpm build:server` | 40 MB 单文件（内嵌 React 产物与 meegle CLI），独立跑起来 UI、meegle、`service` 子命令都正常 |

SSH Windows 远端没有环境，未测。

**与 Node 版有意的出入**（其余逐字节照搬）

- 数据目录在配置解析时就转成绝对路径：portable-pty 只在 PATH 里找相对路径的程序，node-pty 的 execvp 按 cwd 解析，相对的 `--data-dir` 会让 Zellij 起不来。
- 旧链路的 channel 在新附着之后才报关闭时，不再把新 backend 置空（Node 版的竞态）。
- WS 关闭时补完关闭握手（Node 版 1005，之前 Rust 版 1006）。
- 陈旧的 `FALCON_MEEGLE_BIN` / `FALCON_WEB_DIST`（从旧版 falcon 终端继承来的）指向不存在的路径时忽略，不挡路。
- 修掉的 TS 缺陷：`files.ts` 在 Windows 宿主机上反斜杠 `..` 能逃出工作目录（**线上 Node 版仍有，Windows 宿主机受影响**）；`repo.ts` 的 drop / squash / reword 在 `rev-parse HEAD` 失败时把任何提交当成 HEAD（会 `reset --hard` 掉未提交的改动）；中转的若干竞态（停用时等长连接、并发启停漏杀 cloudflared）；meegle 登录超时在设备码出现前触发会失效。

**删除 Node 版（2026-10-10，用户拍板）**：`packages/server`、SEA 脚本（`build-binary.mjs`）、esbuild / postject 依赖一并删掉；最后一个带 TS 原文的提交是 `fd9022a`。随之挪动的：滚动插件源码 → `native/zellij-plugin`（不进 Cargo 工作区），产物 → `native/crates/falcon-server/assets/`；`@lark-project/meegle` 与 `tsx` → 仓库根的 devDependencies；`pnpm build:bin` 改跑 `build-server.mjs`，`build:pkg` 随之打进 Rust 服务端；版本号改取仓库根 `package.json`；原生 e2e 与 `gen-fixtures.mjs` 只认 Rust 服务端，协议 fixture 已由它重新生成。工作区里 `packages/server` 那组未提交的 viewerArbiter 改动（逻辑已在 Rust 版里）删除前收进了 `git stash`。

**剩下的**

1. 在真实数据目录上切换（用新 pkg 或 `falcon service install` 换掉 `~/.falcon` 上在跑的旧服务）：要用户点头。
2. 已知风险：russh 按到达而不是按消费放大通道窗口、每条通道的队列有界，慢的下载客户端可能拖住整条 SSH 链路（终端也在上面）；Node 的 ssh2 是逐通道流控。大文件下载在弱网上要实测。

---

## 12. 提前删除 React 前端（2026-10-10）

用户拍板：不等 C1–C3 补齐，直接删 `packages/web`、`packages/shared` 与移动端（C4 提前）。仓库里从此只剩 Rust，浏览器访问拿到的就是浏览器版 GPUI（`native/scripts/build-web.sh` 的产物，`pnpm build:bin` 编进服务端）。

**跟着挪走的**：字体 → `native/assets/fonts/`（Maple 为 React 按 unicode-range 拆的拉丁 / CJK 两片与 `subset-maple-mono.mjs` 一并删掉，原生与浏览器版都用整份 Regular）；内置应用图标 → `native/web/icons/`（build-web.sh 拷进产物的 `/icons/`，服务端的 `/api/app-icon/*` 与 PWA 清单跳到这里）；Ghostty 主题 vendoring（`pnpm vendor-ghostty-themes`）直接写 `falcon-theme/data/`，产物与原来逐字节相同；`export-i18n.mjs` / `export-ghostty-themes.mjs` 退役——`falcon-app/locales/zh-CN.json` 与 `falcon-theme/tests/fixtures/` 从此是真相来源 / 冻结的回归基线；pnpm 不再是 workspace，只剩 meegle CLI 与 subset-font 两个打包用依赖。

**代价（浏览器端相对 React 版的倒退，排进 C3）**：附录 B 里标 C3 的缺口现在都是线上缺口——文件上传 / 下载的选择框、拖入文件、浏览器剪贴板粘贴、HTML 预览（iframe 浮层）、外链图片代理、浏览器保留快捷键、GPU 设备丢失后自动 reload；Safari / Firefox 没测；wasm 没有预压缩（18.8 MB 原始，服务端还不发 brotli），中文字体 Maple 21 MB 要首次出现中文时才拉。原生客户端不受影响。

---

## 附录 A：重写时必须原样保留的运行时行为

本附录从源码注释与 ADR 摘出，S 线每期动手前对照。行号是 2026-10-10 的工作区。

**执行与命令层**

- ExecFn 返回 `{code, stdout, stderr}`：**非零退出码是正常返回值，绝不 reject**；spawn 失败也收成 `code: null`。只有链路故障才算错（`zellij/exec.ts`、`git/repo.ts:4-5`）。
- exec 输出先攒成整块字节，结束时一次解码，不逐块转字符串（`zellij/exec.ts:26-32`、`sessions/ssh.ts:341-356`）。
- Windows 远端一律走 `powershell -NoProfile -NonInteractive -EncodedCommand <UTF-16LE base64>`，前面加 `$ProgressPreference` 和 UTF-8 的 OutputEncoding；先设 `$LASTEXITCODE = 127` 当哨兵，最后显式 `exit`（`zellij/host.ts:144-199`）。
- 清继承来的环境变量用 `env -u` 或 `$env:X=$null`，**绝不设成空串**（`zellij/host.ts:153-168`、`git/command.ts:44-61`）。
- Windows 持久会话：用 WMI `Win32_Process.Create` 把 Zellij server 生在 sshd 的 Job 之外。内层用 `-Command`，不能再套一层 EncodedCommand（会膨胀约 7 倍，撞 cmd 的 8191 字符上限）。是否建成以 hasSession 为准，不信退出码（`zellij/host.ts:269-300`）。
- Windows 本地：进程在 Job 里就如实降级为非持久（`zellij/exec.ts:64-97`）。
- Node 的 `spawn(shell: true)` 在 Windows 上实际执行的是 `cmd.exe /d /s /c "<cmd>"`，要用 `CommandExt::raw_arg` 复刻；windowsHide 对应 `CREATE_NO_WINDOW`。
- `git/path.ts` 不用平台路径库（后端可能跑在 Windows 上为 Linux 远端算路径）；git 永远用 `-C <dir>`，不 chdir。
- git 环境锁 `LC_ALL=C`、`GIT_TERMINAL_PROMPT=0`；只读查询加 `GIT_OPTIONAL_LOCKS=0`；所有调用带 `core.quotepath=false`；不用 `-z`（`git/command.ts:24-68`）。
- 仓库锁的键是"宿主机 key + `::` + canonKey(仓库根)"，不是项目 id（`git/lock.ts`）。

**会话与终端**

- RingBuffer 上限 4MB、按块淘汰。容量是按 dump-screen 的 10000 行算的，两个数要一起改（`ringbuffer.ts`、`zellij/command.ts:19`）。
- 输出 16ms 合并一次，所有 Viewer 共用同一个序列化好的帧；发控制消息前先把待发输出冲掉（`sessions/manager.ts`）。
- replay = VT 模式前缀 + RingBuffer 快照，否则重连后滚轮和 bracketed paste 失效。
- PTY 输出做 UTF-8 跨块拼接，每一帧和每个 RingBuffer 块都是完整的 UTF-8，非法字节替换成 U+FFFD。
- 背压：Viewer 写队列超过 4MB 丢帧并标记落后，降到 256KB 以下后整体 replay。
- 重连退避：1s 起翻倍，封顶 30s。项目链路、主机链路、本地持久会话各一套（本地首次 250ms）。
- 自动标题只在有 Viewer 时探测，2.5s 节流，同一时刻只跑一次。
- 多 Viewer 时 PTY 尺寸取最小格子；深浅跟最近操作的那个 Viewer；Viewer 没报格子前不 attach（不拿 80×24 抢跑）（`viewerArbiter.ts`、`termSize.ts`）。
- 启动时 active 的持久会话改为 unverified，非持久的改为 dead，然后自动接回。
- agent 启动脚本里的登录 shell 是生成时写死的绝对路径，**绝不读 `$SHELL`**（Windows 远端上会递归调用自己）。
- 三种登录 shell 写法刻意不统一：PTY 包装用 `<shell> -l -c`，agent 和 px0 用 `-i -l -c`，loginEnv 用 `-i -l -c env -0`。
- 非持久 SSH 会话不能用 `shell()`，要走 exec 加 env 前缀（`shell()` 塞不进 COLORFGBG 等变量）。
- 本地 PTY 的基底环境是登录环境，不是服务端进程自己的环境。顺带一提：Rust 版不改写自身环境，SEA 把 `FALCON_*` 漏进会话的问题自然消失。

**Zellij**

- 会话名是 `mj-` 加 16 位 hex，**前缀不能改**，改了老会话就接不回。
- 用 `attach <名> --create`（不带 `--create` 是前缀匹配）；销毁用 `delete-session`。
- 独占的 config.kdl：locked 模式、`clear-defaults`、`session_serialization false`、`mouse_mode true`；**CLI 绝不能传 `--mouse-mode`**。
- 0.45 起 config 和 CLI 两处都要 `scroll_mode_sync false`，否则滚一下滚轮后敲的字全被吞。
- config.kdl 与 scroll.kdl **两套配置不能合并**，会话用哪套记在 `sessions.scroll_plugin`（ADR 0019）。
- `zellij pipe` 按名字广播，**不加 `--plugin`**；stdin 必须给 EOF，超时 3s。
- 接回时先 `list-panes` 找 terminal pane，再 `dump-screen --pane-id`，结果的 `\n` 统一成 `\r\n`。
- `--default-shell` 是路径，不能带参数。

**文件与传输**

- 远端**不走 SFTP**：POSIX 下载用 `cat`，上传用 `cat >`。Windows 用 base64 行协议：每 48KB 输入字节编成一行，每行可以独立解码，解码时忽略 `\r`（ADR 0008）。
- 上传先写同目录下的临时文件 `.<名>.<随机>.falcon-upload`，实收字节数等于 Content-Length 才改名到位，否则报 ESHORT；EEXIST 最后检查，回 409；本地用 create_new 打开。
- 流式 exec 的调用方必须把 stdout 读起来，否则通道不 close。
- 原始字节路由是路径形状（`raw/:token/*`），响应头带 `CSP: sandbox allow-scripts allow-forms allow-popups allow-modals`、nosniff、no-store、no-referrer；超过 16MB 回 413，不回截断的内容。MIME 用手写的映射表，不换 mime crate（ADR 0007）。
- Windows 写文件不用 Set-Content（会写成 GBK 或带 BOM，内联内容还会撞 8191 上限），走 stdin base64 + `WriteAllBytes`。
- 删除：`git/remove.ts` 是删附属项目 worktree 目录的唯一入口，vetoRemoval 七条静态断言 + 动态取证在 `worktree remove` 之前；DB 行无条件删，回 200 带 warnings（ADR 0002）。工作目录内部的删除走 `resolveInside`，两件事不合成一条路径。

**链路、中转与第三方**

- TOFU：首次连接记下 sha256 hex 指纹，存在 DB 的 known_hosts 表；不符就报 HostKeyMismatch。
- 远端转发要先登记 acceptor 再 forwardIn；入站连接按 destPort 分发，不按 destIP。
- 中转同端口互斥：先同步落库禁用同槽位的其它规则，再异步停旧起新；用代数作废还在途的启动尝试（ADR 0016）。
- cloudflared 的 URL 两路取、谁先到用谁：stderr 正则加 metrics `/quicktunnel`；回环目标的 Host 头改写成 localhost。
- 后端退出时必须杀掉 cloudflared 和 px0 子进程。
- px0 必须挂在 pty 上，后端死了它跟着挂断；`-no-update`；空闲 15 分钟停；只有入口页会拉起，其它路径在它没跑时回 503（ADR 0017）。
- meegle：stdout 和 stderr 都要当 JSON 试（业务错误在 stderr）；未登录时一切命令都报 unknown command；用户输入一律 `--flag=value`；view search 限 5 qps，撞上退避 700ms；加 `MEEGLE_NO_UPDATE_CHECK=1`（`meegle/command.ts` 顶部注释）。
- askpass helper 读 conf，不读 `SUDO_ASKPASS`（Claude Code 会把它从子进程环境里删掉）。

**数据兼容**

- 数据目录回退 `~/.mojito`，库文件回退 `mojito.db`。
- `secret.key` 必须是 32 字节；凭据存成 `base64(iv12 ‖ tag16 ‖ ct)`；密码哈希是 scrypt 的 `salthex:hashhex`。
- 登录令牌只存内存，30 天滑动过期，cookie 属性是 httpOnly、SameSite=Lax、path=/；rawToken 寿命 6 小时，剩不到一半就换新，logout 时整批作废。

---

## 附录 B：浏览器版的平台缺口与补法

gpui-pre-web 0.3.8 的现状出自源码（`gpui-pre-web-0.3.8/src/`）与 gpui-kit 文档，C0 时以实测为准。

| 缺口 | gpui-web 现状 | 补法 | 期 |
|---|---|---|---|
| IME 候选框位置 | `update_ime_position` 空实现，隐藏 textarea 固定在左上角 | fork：按 `bounds_for_range` 把 textarea 移到光标处 | C0 |
| 终端 IME 兼容 | 镜像 textarea 依赖 `selected_text_range` | Falcon 的终端 handler 恒返回 `Some(0..0)`，正好兼容，保持即可 | — |
| 键盘 | 只认 `event.key`、布局固定 us；mac 上 Option 不能当 Meta | 终端的 Option-as-Meta 在应用层按 `event.code` 补（需要 fork 透出 code）；浏览器保留键（⌘T / ⌘W 等）走 `falcon_core::shortcuts` 的 BrowserAlias | C0 / C1 |
| WebSocket | 无 | falcon-client 的 web 传输用 `web_sys::WebSocket`，`binaryType = arraybuffer` | C0 / C2 |
| HTTP | `fetch_http_client` 没有上传流式与进度，丢弃 future 不中断请求 | 普通请求走 fetch（同源 credentials，挂 AbortController）；上传走 XHR | C2 |
| 登录态 | cookie 是 httpOnly，脚本读不到也设不了 | cookie 罐模式；401 / close 4401 推 LoginRequired；钥匙串自动重登在浏览器里没有 | C2 |
| 文件选择 / 下载 | `prompt_for_paths` 返回错误 | DOM `<input type=file>`（含 `webkitdirectory`）+ `<a download>` | C3 |
| 拖入文件 | drop 事件只 preventDefault | falcon-web 自己挂 DOM drop 监听，读 `File.arrayBuffer` | C3 |
| 剪贴板读 | 同步读恒为 None | 终端粘贴改走 paste 事件 → `InputHandler::paste`；非安全上下文（明文 http 非回环）下没有剪贴板写，fork 里先探测再写 | C3 |
| HTML 预览 | 无 WebView | `<iframe sandbox>` 绝对定位叠在 canvas 上，复用 `html.rs` 现有的"浮层打开时隐藏、按可视区裁切"逻辑；ADR 0007 的沙箱与令牌规则不变 | C3 |
| 外链图片（飞书头像、Markdown 的 https 图） | fetch 受 CORS 限制 | 服务端加图片代理路由（S6 或 C3 时加） | C3 |
| 字体 | 启动时字体库为空，只认 TTF / OTF | 正文字体随 wasm 加载；CJK、Nerd Font 运行时 fetch 后 `add_fonts`；字体文件由服务端以 TTF 提供（构建期由 woff2 转） | C0 / C3 |
| 本地设置 | 无文件系统 | localStorage，键名与现 web 一致 | C3 |
| 时间 | `Instant` / `SystemTime::now` 在 wasm 上 panic；chrono 不开 `wasmbind` 时本地时区静默变成 UTC | 全仓换 web-time；chrono 在 wasm 上开 `wasmbind` | C0 / C1 |
| vte 同步输出 | `StdSyncHandler` 用 `Instant::now`，遇到 `CSI ?2026h` 会 panic | 自定一个基于 web-time 的 `Timeout` | C0 |
| alacritty fork | 无条件依赖 polling（wasm 上 `compile_error`），非 Windows 下编 unix tty | fork 加默认开启的 `tty` feature，把 event_loop / tty / polling / rustix 设成可选；workspace 用 `[patch]` 指过去 | C0 |
| 多窗口 | 只能一个顶层窗口 | 浏览器版的服务端就是当前源，没有连接窗口、profiles | C1 |
| 弹窗拦截 | `open_url` 是 `window.open`，在 await 之后调用会被拦 | 打开外链只在同步点击回调里做（px0、飞书链接已是这样，个别异步路径要改） | C3 |
| GPU 设备丢失 | 停止渲染 | 检测到后自动 reload（§8） | C3 |
| favicon | 无 | 改 `<link rel=icon>`，走现有 `/api/app-icon/*` | C3 |

---

## 附录 C：服务端对外面（S 线搬迁清单）

| 分组 | 数量 | 出处（`packages/server/src/`） |
|---|---|---|
| `/api/auth/*` | 4 | `routes.ts` |
| `/api/askpass*`（helper 用 Bearer） | 3 | `routes.ts` |
| `/api/system`、`/api/fs/*`、`/api/shells` | 4 | `routes.ts` |
| `/api/projects` 增删改查、repo / repos、host | 8 | `routes.ts` |
| `/api/hosts` 增删改查与测试 | 6 | `routes.ts` |
| git | 12 | `routes.ts` |
| 文件（含 `raw/:token/*`、上传下载） | 9 | `routes.ts` |
| worktree | 4 | `routes.ts` |
| 中转（relays / forwards / shares） | 7 | `routes.ts` |
| 会话 | 8 | `routes.ts` |
| `/api/meegle/*` | 17 | `meegle/routes.ts` |
| 应用图标与 manifest | 8 | `appIcon.ts` |
| `/px0/:projectId[/*]` | 2 | `px0/routes.ts` |
| WS：`/ws/sessions/:id`、`/ws/install/:projectId` | 2 | `ws.ts` |
