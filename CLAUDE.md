# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 命令

```bash
pnpm install
pnpm build          # shared → server → web，顺序不能反：workspace:* 指向 shared/dist
pnpm dev:server     # tsx watch，4923
pnpm dev:web        # vite，5173，/api 与 /ws 代理到 4923
pnpm build:bin      # Node SEA 单文件，产物在 release/（详见 README）
pnpm build:native   # 原生客户端（native/，Rust + GPUI）release 构建
pnpm build:pkg      # macOS 安装包：Falcon.app = 原生客户端 + Resources 里的 SEA 服务
```

### 门禁：typecheck + 单元测试

仓库没有 ESLint / Prettier / Biome，唯一的静态门禁是 `tsc`：

```bash
pnpm --filter @falcon/server typecheck
pnpm --filter @falcon/web typecheck     # web 的 build 也会先跑一遍 --noEmit
```

测试用 `node:test`，但**必须经 tsx 跑**——源码里的相对 import 一律带 `.js` 后缀，`node --test` 不会把 `./x.js` 解析到 `x.ts`，会 `ERR_MODULE_NOT_FOUND`。只有 server 声明了 tsx 依赖，从它那里一次跑全仓：

```bash
pnpm --filter @falcon/server exec tsx --test "src/**/*.test.ts" \
  "../shared/src/**/*.test.ts" "../web/src/**/*.test.ts"

# 单文件
pnpm --filter @falcon/server exec tsx --test src/zellij/command.test.ts
# 单用例
pnpm --filter @falcon/server exec tsx --test --test-name-pattern "dump-screen" src/zellij/command.test.ts
```

测试只覆盖纯函数层（命令构造、路径运算、模式跟踪、DB 行合并）。会话 / SSH / Zellij 的端到端路径没有自动化测试，改动那些要在真机上验。

## 架构

三个 workspace 包，`packages/shared` 是类型与协议的唯一真相来源，server 与 web 都从它取（含 VT 模式跟踪 `termModes.ts`：服务端回放前缀与 rio 鼠标上报共用一个跟踪器）。

### 数据流

浏览器 ↔ `/ws/sessions/:id`（WebSocket）↔ `SessionManager` ↔ `Backend`（本地 PTY 或 SSH channel）↔ 宿主机上的 Zellij ↔ shell。

WS 上是混合协议：**终端字节走二进制帧**（1 字节类型头 `TERM_FRAME_OUTPUT` / `TERM_FRAME_REPLAY` + UTF-8 载荷），state / reconnecting / error 等控制消息走 JSON 文本帧。REST（`/api/*`）只管项目、主机、git、转发、会话的增删改查。

### server

- `sessions/manager.ts` 是核心，约一千行：`LiveEntry` 持有 backend、RingBuffer（4MB 输出环形缓冲，供 Viewer 重连回放）、多个 Viewer、输出合并窗口、VT 模式跟踪、SSH 断线的指数退避重连。会话状态机 `active / unverified / dead` 与 `Detach` / `Terminate` 的区分见 CONTEXT.md，别自己发明语义。
- `sessions/backend.ts` 是本地 PTY 与 SSH channel 的共同接口；`local.ts` / `ssh.ts` 各实现一边。
- `zellij/` 与 `git/` 是同一套分层，改一边时照另一边的样子写：
  - `command.ts` —— **纯函数，零 I/O，只产出 argv 数组与 env**，绝不拼命令行字符串（远端 POSIX 与远端 Windows 转义规则不同）；
  - `host.ts` —— 抹平四种执行环境（本地 Unix / 本地 Windows / SSH POSIX / SSH Windows）为 `posix` / `windows` 两类，负责路径构造与命令行拼装；
  - `exec.ts` / `repo.ts` —— 执行与错误分类。**ExecFn 的铁律：非零退出码是正常返回值，绝不 reject**，判错一律显式查 `res.code`；只有 exec 本身 reject 才算链路故障。
- `git/path.ts` **不用 `node:path`**：后端跑在 Windows 上也可能在为 Linux 远端构造路径。同理后端**永远不 `process.chdir` 进 worktree**，git 命令一律带 `-C <dir>`。
- `git/remove.ts` 是删除**附属项目 worktree 目录**的唯一入口，护栏（静态断言 + 动态取证）在改动前先整份读完 `docs/adr/0002-worktree-derived-projects.md`。工作目录内部的文件删除在 `files.ts`（`resolveInside` 挡在项目工作目录里），两件事不要合成一条路径。
- `sessions/agent.ts` 是 agent 会话（开场直接跑 claude / codex / grok，ADR 0013）：纯函数产启动脚本与写入命令，脚本落在宿主机 `<falcon 根>/agents/`，Zellij 的 `--default-shell` 指向它；CLI 退出后 `exec` 回登录 shell，会话不跟着结束。脚本里的登录 shell 是**生成时写死的绝对路径**，绝不读 `$SHELL`（Windows 远端那条路径上 `SHELL` 就是脚本自己，会递归）。
- `cloudflared/` 是公网发布（ADR 0014）：`command.ts` 纯函数产 argv 与解析 Quick Tunnel URL / `/quicktunnel` JSON（**只在 falcon 后端本机 spawn `cloudflared`，不往远端装**）；`bin.ts` 按需把锁定版本下到 `<dataDir>/bin/cloudflared`（`FALCON_CLOUDFLARED_BIN` 优先，PATH 兜底）；`sessions/share.ts` 管进程生命周期，远端目标先在本机 `listen(0)` 再 `forwardOut`。规则在 `host_shares` 表，公网 URL 是运行时事实不入库。v1 只做 Quick Tunnel + HTTP。
- 中转（端口转发 `sessions/forward.ts` + 公网发布 `sessions/share.ts`，ADR 0016）**按机器挂，不挂项目**：转发挂 SSH Host（`host_forwards`），发布挂本机或 SSH Host（`host_shares.host_id` 为 null = 本机）。隧道走主机链路 `manager.getHostLink`（与「浏览远端目录」共用），断线由 `scheduleHostReconnect` 重连，**不走项目链路**。同端口可存多条、同时只一条生效：启用时先同步落库把同槽位的其它规则置 disabled，再异步停旧起新；槽位口径（本地转发跨主机比端口、远端转发与发布在同一台机器内比）在纯函数 `relaySpec.ts`，web 的「同端口」徽标照同一口径算。
- `meegle/` 是右侧「飞书项目」面板的后端（ADR 0010）：`command.ts` 纯函数产 argv 与归一化 CLI 输出（**只在 falcon 后端本机 spawn `meegle`，不经 shell、不跟项目走**），`client.ts` 起进程 / TTL 缓存（待办 / 搜索 / 详情默认 5 分钟，`?fresh=1` 与 `POST /api/meegle/cache/clear` 打穿）/ 按类型扇出 / device-code 登录进程，`routes.ts` 挂 `/api/meegle/*`（含粘贴链接解析 `resolve-url`——走 CLI 的 `url decode`，别自己拆路径——与固定列表 CRUD，固定项存 `db.ts` 的 `meegle_pins` 表），`bin.ts` 定位可执行文件（**CLI 随服务内置**：`@lark-project/meegle` 锁定版本依赖，npm 包自带各平台静态二进制，直接 spawn 本平台那个；单文件发布由 `scripts/build-binary.mjs` 封进 SEA、bootstrap 释放后经 `FALCON_MEEGLE_BIN` 指入；PATH 上的 `meegle` 只是兜底）。CLI 的脾气（错误信封在 stderr、未登录时一切命令都是 unknown command、视图只能按关键字搜、待办没名字要 MQL 补、view search 限 5 qps）全写在 `command.ts` 顶部注释，改动前先读。
- `px0/` 是「用 px0 审阅」（ADR 0017）：在项目宿主机上按需拉起 px0，经 falcon **同源**反代到 `/px0/<项目 id>/`（鉴权就是登录 cookie，onRequest 钩子与 `/api/` 同一口径）。`command.ts` 纯函数（资产映射、**钉死的 sha256**、argv、端口解析、远端安装 / 启动命令），`bin.ts` 在后端本机下载校验、SSH 项目再经 stdin 推到远端（不让远端自己下），`manager.ts` 管实例——**本地与远端都挂在 pty 上**，后端死了 px0 跟着挂断，别改回普通 spawn / 无 pty 的 exec；远端连接直接走 forwardOut，本机不另开监听端口。`proxy.ts` 的头过滤有讲究：剥 falcon 的 cookie、只替同源请求改写 Origin（px0 的 localPost 要 Origin == Host）、去 set-cookie。同源意味着 px0 的前端能调 falcon 全部接口——**只在本机 / 内网可接受，公网访问前先挪到独立源**。原生客户端不嵌 px0，菜单项把地址交给系统浏览器；没登录的浏览器被送去 `/?next=<px0 地址>`，登录后由 web 的 `lib/loginNext.ts` 跳回（只认 `/px0/` 开头）。
- `db.ts` 用的是 **`node:sqlite` 的 `DatabaseSync`**（README 的结构图里写的 better-sqlite3 已过时）。外键约束显式关闭，级联在应用层手写；`migrate()` 是幂等的 `CREATE TABLE IF NOT EXISTS` + 加列，没有版本号迁移表——改表结构就往这套里加。
- Windows 远端的所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。

### web

- 界面骨架是**浮动岛**（ADR 0011）：桌面最外层铺 `--app`（窗口底），侧栏 / 主区内容 / 右面板都是浮在它上面的圆角面板（`styles.css` 的 `.island`），之间只有一道 6px 的缝——**不要再加分栏边框**；岛里再嵌一块（统计卡、表格、代码块、分段控件的槽）用 `.sunken`（借窗口底色），**也不要用边框**。`.island` 的面板底一律 `--background`，终端要的就是主题原底色，面板与它同色圆角边缘才不露色差。画布上一列是一座岛，列里每扇窗口自己裁对应的角。圆角走 `--radius`（12px）减出来的阶梯（sm 8 / md 10 / lg 12 / xl 16），别写死半径。「当前选中」用 `--tint`（主题 ANSI 蓝推的弱着色面），hover 用灰，两者都是内缩的圆角块。
- 主区是**列式工作区**（ADR 0012）：`WorkCanvas.tsx` 把窗口（终端 / 文件 / 差异）排成从左到右的列，每列可叠多扇，列宽与窗口高度都能拖，抓标题栏能拖到别的列或另起一列。**不横向滚动**：列分在一块块画布上（`ColumnLayout.canvas`），一次显示一块、铺满视口，新列排不下就自动另起一块（store 订阅里的 `settleCanvases` → `assignCanvases`），两块以上时顶上出画布条（每块画一张布局缩略图，`canvasThumb`）；固定列在每块上都有。**没有顶部 tab 栏**——新建入口在侧栏每个 checkout 行的 ＋（菜单含各家 CLI），"开着哪些会话"列在侧栏树里（**会话行默认收起**，checkout 行尾巴上只摆一个计数徽标 + 最重的异常状态；展开状态存 `store.sessionsOpen`，新建 / 打开会话时自动摊开那个 checkout）。排布模型在 `store.columns`，纯函数在 `lib/layout.ts`（增删移 / 与数据源对账 `syncColumns` / 按项目过滤 / 拖拽落点 / 像素几何 / 固定列——`pinned` 的列永远是最后一列，插列与拖拽落点一律夹到 `pinEdge()` 之前），窗口的 key 约定在 `lib/paneKey.ts`。**每扇窗口都绝对定位、DOM 顺序恒定**：xterm 的画布换过父节点渲染尺寸就毁（整屏空白 + 字被拉大），改排布只准改 left/top/width/height，改之前先读 ADR 0012。
- `store.ts` 是单个 zustand store，含全部 UI 态与持久化的工作区布局（含列排布）；`lib/useActions.ts` 集中所有菜单项 / 命令面板动作。
- `components/ui/` 是 shadcn 生成物，`components/common/` 是本项目封装（Menu / ConfirmDialog / Field 等），业务组件在 `components/` 顶层。
- `TerminalView.tsx` 只面向 `lib/termAdapter.ts` 接口，底下是 xterm.js（默认）或 rioterm（实验性，Rust VT 核心编译成 WASM，动态 import）。
- `lib/rio/` 是 rio 引擎的自持装配层（ADR 0005）：`open.ts` 复刻了 rioterm 的 `open()`（键鼠 / IME / 滚轮 / 剪贴板接线，换字体只换渲染器、Terminal 不动），`renderer.ts` 是渲染器契约，`webgpu/` 是自研 WebGPU 渲染器（不可用时回落 rioterm 自带 canvas），`mouse.ts` 合成鼠标按键报文（rioterm 没有这个 API），输出一律经 `handle.write()` 进来才有协议 / 编码可查。**rioterm 锁定精确版本**，升级前按 ADR 核对它的 open()/canvas/keys/core。纯函数层（度量、颜色、图集分配、行构建、脏行、sprite 几何、鼠标报文）都有单测；GPU 与 DOM 只在真机验。排查用 `localStorage["falcon.rio.renderer"] = "canvas" | "webgpu"` 强制渲染器，DEV 下控制台看 `__rioHandles`（`rendererKind` / `fallbackReason` / `renderer.stats`）。WebGPU 画布 present 后回读是空的，看像素只能页面截图。
- `lib/theme/` 是主题系统（ADR 0006）：数据模型就是 Ghostty 主题文件（`ghostty.ts` 解析 / 补默认 / 序列化，含 `theme = X` 覆盖与 cell-foreground 特殊值），浅色 / 深色各一个槽位（`pref.ts`，存的是颜色**副本**，启动不等目录），整套 shadcn 语义色、语法高亮色（shiki css-variables 主题）、终端 ITheme 都由 `derive.ts` 从一套主题推出来并写在 `<html>` 内联 style 上（`apply.ts`）。**`.dark` 按主题底色亮度切，不按明暗模式**。内置目录 = Falcon 两套 + Ghostty 全部 463 套（`assets/themes/ghostty-themes.ts`，`pnpm vendor-ghostty-themes` 从本机 Ghostty.app 或 GitHub 重新生成，懒加载）。界面色**只准用语义 token**，不许写死颜色；新增语义色去 `derive.ts` 加，不要回到 `styles.css` 写两套。
- 文件下载 / 上传（ADR 0008）：服务端 `transfer.ts` 走**流对流**——本地是 fs 流，远端是 `SshLink.execStream` 交出来的 ssh2 通道（POSIX `cat` / `cat >`，Windows 走每行独立可解的 base64 行），没有预览那个 16MB 上限；上传先写同目录临时文件、收满 `Content-Length` 才改名到位（中途断开不留截断文件），同名文件回 409 让前端问过再带 `overwrite=1` 重发。鉴权就是登录 cookie（下载是 `<a download>` 同源导航，上传是 XHR），别再发一种令牌。进度走 sonner toast，顺序确认用 `lib/confirmAsync.ts`。
- 文件面板（ADR 0009）：`FilesPanel.tsx` 是当前目录的平铺浏览器（路径栏 + 图标工具栏 + 多选表），不是树。mkdir / rename / remove 在 `files.ts`，命令构造是纯函数；删除只作用于 `resolveInside` 之后的工作目录内部，文件夹递归删、不能删工作目录本身。文件夹上传是 mkdir -p 再逐个走 0008 的 PUT。前端拼宿主机路径用 `lib/filePath.ts`。
- 中转页（`RelaysPane.tsx`，ADR 0016）在设置里，不在右侧栏：按机器分块（本机只有公网发布），开着才 3s 轮询；任何写操作之后重拉整张 `/api/relays`——启用一条会顺手停掉同端口的其它规则，只替换这一行会显示错。
- 飞书项目面板（`MeeglePanel.tsx`，ADR 0010）：不随焦点项目切换，三页（待办 / 空间 / 固定）加面板内下钻栈，粘贴飞书项目链接可直接打开视图 / 全景视图 / 工作项并固定；所有数据走 `api.meegle*`，前端不认识 CLI 原始字段；业务请求撞上 409 就重新拉 `/api/meegle/status` 让面板自己切到安装 / 登录提示。列表 / 详情走 `lib/meegleCache.ts`（与服务端共用 `TtlCache`）：卸载后再开立刻画出上次的结果，30s 内不打网络，顶栏刷新清缓存。右侧栏开着时切走不卸载（`App.tsx` 里 `hidden`），关掉右侧栏才卸。
- 文件查看（`FileView.tsx`）：图片与 HTML 预览的字节不经 JSON，走原始字节路由 `/api/projects/:id/raw/<token>/<path>`（ADR 0007）。HTML 在**没有 allow-same-origin** 的沙箱 iframe 里渲染，凭据是 URL 里只能读该项目文件的作用域令牌（`Auth.rawToken`），响应头带 CSP `sandbox` + nosniff；别为了"方便"给 iframe 加 allow-same-origin 或把登录 cookie 塞进 URL。前端拼地址一律用 `lib/rawUrl.ts`。
- 默认字体是内嵌的 Berkeley Mono TX-02（`lib/berkeley-mono.css`，family 名 `TX-02`）：界面 `--font-sans` / `--font-mono` 和终端默认正文都用它。商业字体，zip 不进仓库，woff2 由 `pnpm vendor-berkeley-mono` 从官方发行包抽出。缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
- 会话**默认不起名**（`name` 空串），界面显示的是自动标题：手起的名字 → 前台命令 → agent 的 CLI 名 → shell 命令名，纯函数在 `lib/sessionTitle.ts`（React 里用 `lib/useSessionLabel.ts`，store 内部直接调 `sessionLabel`）。列表里四个地方（侧栏 / 总览 / 命令面板 / 移动端切换）必须叫同一个名字；窗口标题栏例外——它右边就是完整工作目录，没标题时那一格空着，别编占位名填进去。前台命令由服务端探测后经 WS 的 `{type:"title"}` 推来（`manager.scheduleTitleProbe`：只在有 Viewer 时探、2.5s 节流、后台会话的标题会陈旧），改这条链路前先读那两处注释。
- 应用图标（ADR 0018）：选择存在服务端 settings 表（所有设备同一个），自定义图由客户端规整成 512 PNG 再传（服务端不解码图片）。标签页图标 / apple-touch 走 `/api/app-icon/*` 的跳转、PWA 清单由服务端现出，这几条取图路由不要登录（路由上的 `config.publicAsset`）。内置图标的图形在 `scripts/app-icons.mjs`，`pnpm gen-icons` 出 web 与原生的全部 PNG；增删图标要同时改 shared 与 falcon-core 的 `APP_ICON_IDS`（有测试对账）。
- 界面上**不允许硬编码中文**，一律走 `i18n.ts` 的 key（v1 只有中文资源）。
- 重组件（终端、命令面板、各种表单、设置）都在 `App.tsx` 里 `lazy()` 加载，新增浮层沿用这个做法。
- `vite.config.ts` 里的 `build.target: es2022` 和 `optimizeDeps.exclude: ["rioterm"]` 都是绕具体 bug 的，注释写了症状，别顺手删。

### native（原生客户端）

`native/` 是独立的 Cargo workspace（不进 pnpm workspace），Rust + GPUI 写的桌面客户端，**连现有 falcon 服务端**（同一套 REST + `/ws/sessions/:id` + 登录 cookie），功能与 web 桌面端对齐。定下来的做法与踩过的坑在 ADR 0015，提案原文在 `docs/design/gpui-client.md`，改动前先读。

```bash
cd native
cargo check -p falcon-app                       # 门禁之一（没有 clippy 要求，但保持零警告）
cargo test --workspace                          # 单测 + 共享测试向量 + 协议 fixture（与单独 -p 跑都要绿：
                                                # GPUI 会经 feature 合并打开 serde_json 的 preserve_order，别断言 Map 的遍历顺序）
FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored   # 起真服务端 + zellij 的 e2e
cargo run -p falcon-app                         # 连本机服务（按已装 LaunchAgent 的端口，没装是 4923）；FALCON_LOCAL_URL 可改指别的实例
```

- **crates.io 走 `native/.cargo/config.toml` 里的 rsproxy 镜像**：这台机器上 Clash 的 fake-ip 把 index.crates.io 解析坏了。网络正常的机器删掉那一段即可。
- 分层：`falcon-proto`（shared 的 serde 镜像，shared 仍是真相来源；`tests/fixtures/` 是 `native/scripts/gen-fixtures.mjs` 从真服务端落盘的响应）→ `falcon-client`（REST / WS，与执行器无关的 future，内部自带 tokio 运行时）→ `falcon-term`（alacritty_terminal + 从 Zed 抄的按键编码 + web 移植的鼠标 / 滚轮 / 模式跟踪）→ `falcon-theme`（lib/theme 的移植，派生结果与 web 逐字节一致）→ `falcon-core`（web lib/*.ts 与 store.ts 纯函数部分的移植，`tests/vectors/` 是与 TS 共用口径的测试向量）→ `falcon-app`（唯一依赖 GPUI 的 crate）。**纯逻辑一律放下层 crate**，GPUI 升级只波及 falcon-app。
- GPUI 走 `gpui-kit` 总包（`=` 精确锁版本）；Zed 的终端代码按文件抄（`terminal/element.rs`、`falcon-term/src/keys.rs`，文件头注明出处 commit），不按 crate 依赖——会带进第二份 gpui。`native/` 因此是 GPL-3.0-or-later。
- 文案：`t!("key")`，key 与 web 的 `i18n.ts` 同名（`native/scripts/export-i18n.mjs` 导出到 `falcon-app/locales/`）；原生独有的放 `falcon-app/i18n-native/<区域>.json`，挂在 `native.<区域>` 下。改了 web 的 i18n.ts 要重跑导出。
- 字体：`falcon-app/build.rs` 把 web 已 vendor 的 woff2 解成 TTF 嵌进二进制（GPUI 不认 WOFF2）。主题数据：`native/scripts/export-ghostty-themes.mjs`。
- **验证界面靠 Metal 回读截图**（锁屏 / 远程也能用）：`cargo build -p falcon-app --features snapshot`，再用 `FALCON_AUTOMATE="ready;select:<项目>;new-terminal;type:ls\r;wait:1000;snap:/tmp/a.png;quit"` 驱动（步骤全表见 `falcon-app/src/automation.rs`，`type:` 里的 `;` 写成 `\x3b`），配 `FALCON_NATIVE_DATA_DIR=<临时目录>`（别写进用户真实的 ~/Library/Application Support/Falcon）与 `FALCON_LOCAL_URL`（指向测试服务端，别用 4923）。点击坐标是窗口逻辑像素（截图 PNG 是 2 倍）。锁屏时显示链路不走，`snap` 自己会先画两帧，别把"截图是空的 / 旧的"当成界面 bug。
- **压测用 `--features automation --release`**（不带 snapshot 的 test-support）：`frames:<ms>` 以 60Hz 手动画并打印帧耗时 p50 / p95，`frames:<ms>:refresh` 无视视图缓存。侧栏与右侧面板是 `cached` 视图（ADR 0015），新加的大块视图照此办理。
- HTML 预览的 WebView 在默认开启的 `webview` feature 上（`--no-default-features` 退成"在浏览器中打开"）。
- **界面尺寸写 `zoom::zpx(..)`，不写 `px(..)`**：界面缩放（⌘+ / ⌘−）= rem 与 zpx 一起乘倍数；`px` 只留给画布几何、终端画面、窗口外框这类真实像素（`zoom.rs` 顶部有清单）。`theme.font_size` 就是 rem，必须是 16 × 倍数，别再拿它当正文字号。

## 约定

- 相对 import 一律带 `.js` 后缀（server / shared 是 NodeNext 的硬要求，web 也保持同一风格；web 另有 `@/` 指向 `src/`）。
- 术语以 [CONTEXT.md](./CONTEXT.md) 为准，包括 _Avoid_ 列表——那里写的不只是命名偏好，Detach/Terminate、源项目/附属项目、宿主机/远端主机这些区分直接对应代码里的分支。
- 注释解释的是"为什么"和踩过的坑（多半是实测出来、文档里查不到的），密度偏高是刻意的；改动附近代码时保持同样的说明力度，注释与代码不符时先修注释。
- 架构决策写在 `docs/adr/`：0001 是持久会话为什么选 Zellij 及一长串实现要点，0002 是附属项目与删除护栏，0003 是多仓库项目与批量派生（含回滚与包围盒断言），0005 是 rio 引擎的自持装配层与 WebGPU 渲染器（含踩坑清单），0006 是主题系统（Ghostty 主题格式、双槽位、界面色派生规则、首帧策略），0007 是原始字节路由与 HTML / 图片预览沙箱（路径形状路由、作用域令牌、响应头护栏、图片缩放模型），0008 是文件下载 / 上传（流式 exec 通道、Windows 的 base64 行协议、临时文件 + 字节数核对的落盘规则），0009 是文件面板目录浏览器（平铺当前目录、mkdir / rename / remove、文件夹上传），0010 是飞书项目面板（宿主机上的 meegle CLI 当数据源、实测的 CLI 输出约定、device-code 登录、两层 TTL 缓存），0011 是浮动岛骨架（窗口底与圆角面板、圆角阶梯、窗口底只压亮度不洗色度与色域收缩、哪些地方刻意没改），0012 是列式工作区（列 → 窗口的排布模型、为什么必须绝对定位、落点坐标的两套下标、把手与持久化、多画布与自动另起一块），0013 是 agent 会话（开场 CLI 的启动脚本、为什么不能读 $SHELL、CLI 缺失时的退路），0014 是公网发布（Cloudflare Quick Tunnel、只在后端本机跑 cloudflared、远端经 SSH 桥），0015 是原生客户端（GPUI 薄客户端、crate 分层、Zed 代码按文件抄、appearance 先于 resize、一台服务端一个窗口、测量数据与踩坑清单），0016 是中转按机器挂（端口转发与公网发布从项目 / 右侧栏搬到主机 / 设置页、同端口互斥的槽位口径、旧规则迁移与删主机级联），0017 是用 px0 审阅（宿主机上跑 px0、同源反代的代价与前提、二进制钉哈希后由后端推到远端、pty 收尸、Origin 改写），0018 是应用图标（按服务端存的选择、公开的取图路由、客户端规整自定义图、一份定义出四种形状、原生运行时换 Dock 图标及其限制）。做相关改动前先读对应 ADR。
