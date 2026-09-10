# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 命令

```bash
pnpm install
pnpm build          # shared → server → web，顺序不能反：workspace:* 指向 shared/dist
pnpm dev:server     # tsx watch，4923
pnpm dev:web        # vite，5173，/api 与 /ws 代理到 4923
pnpm build:bin      # Node SEA 单文件，产物在 release/（详见 README）
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
- `meegle/` 是右侧「飞书项目」面板的后端（ADR 0010）：`command.ts` 纯函数产 argv 与归一化 CLI 输出（**只在 falcon 后端本机 spawn `meegle`，不经 shell、不跟项目走**），`client.ts` 起进程 / 缓存 / 按类型扇出 / device-code 登录进程，`routes.ts` 挂 `/api/meegle/*`（含粘贴链接解析 `resolve-url`——走 CLI 的 `url decode`，别自己拆路径——与固定列表 CRUD，固定项存 `db.ts` 的 `meegle_pins` 表），`bin.ts` 定位可执行文件（**CLI 随服务内置**：`@lark-project/meegle` 锁定版本依赖，npm 包自带各平台静态二进制，直接 spawn 本平台那个；单文件发布由 `scripts/build-binary.mjs` 封进 SEA、bootstrap 释放后经 `FALCON_MEEGLE_BIN` 指入；PATH 上的 `meegle` 只是兜底）。CLI 的脾气（错误信封在 stderr、未登录时一切命令都是 unknown command、视图只能按关键字搜、待办没名字要 MQL 补、view search 限 5 qps）全写在 `command.ts` 顶部注释，改动前先读。
- `db.ts` 用的是 **`node:sqlite` 的 `DatabaseSync`**（README 的结构图里写的 better-sqlite3 已过时）。外键约束显式关闭，级联在应用层手写；`migrate()` 是幂等的 `CREATE TABLE IF NOT EXISTS` + 加列，没有版本号迁移表——改表结构就往这套里加。
- Windows 远端的所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。

### web

- `store.ts` 是单个 zustand store，含全部 UI 态与持久化的工作区布局；`lib/useActions.ts` 集中所有菜单项 / 命令面板动作。
- `components/ui/` 是 shadcn 生成物，`components/common/` 是本项目封装（Menu / ConfirmDialog / Field 等），业务组件在 `components/` 顶层。
- `TerminalView.tsx` 只面向 `lib/termAdapter.ts` 接口，底下是 xterm.js（默认）或 rioterm（实验性，Rust VT 核心编译成 WASM，动态 import）。
- `lib/rio/` 是 rio 引擎的自持装配层（ADR 0005）：`open.ts` 复刻了 rioterm 的 `open()`（键鼠 / IME / 滚轮 / 剪贴板接线，换字体只换渲染器、Terminal 不动），`renderer.ts` 是渲染器契约，`webgpu/` 是自研 WebGPU 渲染器（不可用时回落 rioterm 自带 canvas），`mouse.ts` 合成鼠标按键报文（rioterm 没有这个 API），输出一律经 `handle.write()` 进来才有协议 / 编码可查。**rioterm 锁定精确版本**，升级前按 ADR 核对它的 open()/canvas/keys/core。纯函数层（度量、颜色、图集分配、行构建、脏行、sprite 几何、鼠标报文）都有单测；GPU 与 DOM 只在真机验。排查用 `localStorage["falcon.rio.renderer"] = "canvas" | "webgpu"` 强制渲染器，DEV 下控制台看 `__rioHandles`（`rendererKind` / `fallbackReason` / `renderer.stats`）。WebGPU 画布 present 后回读是空的，看像素只能页面截图。
- `lib/theme/` 是主题系统（ADR 0006）：数据模型就是 Ghostty 主题文件（`ghostty.ts` 解析 / 补默认 / 序列化，含 `theme = X` 覆盖与 cell-foreground 特殊值），浅色 / 深色各一个槽位（`pref.ts`，存的是颜色**副本**，启动不等目录），整套 shadcn 语义色、语法高亮色（shiki css-variables 主题）、终端 ITheme 都由 `derive.ts` 从一套主题推出来并写在 `<html>` 内联 style 上（`apply.ts`）。**`.dark` 按主题底色亮度切，不按明暗模式**。内置目录 = Falcon 两套 + Ghostty 全部 463 套（`assets/themes/ghostty-themes.ts`，`pnpm vendor-ghostty-themes` 从本机 Ghostty.app 或 GitHub 重新生成，懒加载）。界面色**只准用语义 token**，不许写死颜色；新增语义色去 `derive.ts` 加，不要回到 `styles.css` 写两套。
- 文件下载 / 上传（ADR 0008）：服务端 `transfer.ts` 走**流对流**——本地是 fs 流，远端是 `SshLink.execStream` 交出来的 ssh2 通道（POSIX `cat` / `cat >`，Windows 走每行独立可解的 base64 行），没有预览那个 16MB 上限；上传先写同目录临时文件、收满 `Content-Length` 才改名到位（中途断开不留截断文件），同名文件回 409 让前端问过再带 `overwrite=1` 重发。鉴权就是登录 cookie（下载是 `<a download>` 同源导航，上传是 XHR），别再发一种令牌。进度走 sonner toast，顺序确认用 `lib/confirmAsync.ts`。
- 文件面板（ADR 0009）：`FilesPanel.tsx` 是当前目录的平铺浏览器（路径栏 + 图标工具栏 + 多选表），不是树。mkdir / rename / remove 在 `files.ts`，命令构造是纯函数；删除只作用于 `resolveInside` 之后的工作目录内部，文件夹递归删、不能删工作目录本身。文件夹上传是 mkdir -p 再逐个走 0008 的 PUT。前端拼宿主机路径用 `lib/filePath.ts`。
- 飞书项目面板（`MeeglePanel.tsx`，ADR 0010）：不随焦点项目切换，三页（待办 / 空间 / 固定）加面板内下钻栈，粘贴飞书项目链接可直接打开视图 / 全景视图 / 工作项并固定；所有数据走 `api.meegle*`，前端不认识 CLI 原始字段；业务请求撞上 409 就重新拉 `/api/meegle/status` 让面板自己切到安装 / 登录提示。
- 文件查看（`FileView.tsx`）：图片与 HTML 预览的字节不经 JSON，走原始字节路由 `/api/projects/:id/raw/<token>/<path>`（ADR 0007）。HTML 在**没有 allow-same-origin** 的沙箱 iframe 里渲染，凭据是 URL 里只能读该项目文件的作用域令牌（`Auth.rawToken`），响应头带 CSP `sandbox` + nosniff；别为了"方便"给 iframe 加 allow-same-origin 或把登录 cookie 塞进 URL。前端拼地址一律用 `lib/rawUrl.ts`。
- 默认字体是内嵌的 Berkeley Mono TX-02（`lib/berkeley-mono.css`，family 名 `TX-02`）：界面 `--font-sans` / `--font-mono` 和终端默认正文都用它。商业字体，zip 不进仓库，woff2 由 `pnpm vendor-berkeley-mono` 从官方发行包抽出。缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
- 界面上**不允许硬编码中文**，一律走 `i18n.ts` 的 key（v1 只有中文资源）。
- 重组件（终端、命令面板、各种表单、设置）都在 `App.tsx` 里 `lazy()` 加载，新增浮层沿用这个做法。
- `vite.config.ts` 里的 `build.target: es2022` 和 `optimizeDeps.exclude: ["rioterm"]` 都是绕具体 bug 的，注释写了症状，别顺手删。

## 约定

- 相对 import 一律带 `.js` 后缀（server / shared 是 NodeNext 的硬要求，web 也保持同一风格；web 另有 `@/` 指向 `src/`）。
- 术语以 [CONTEXT.md](./CONTEXT.md) 为准，包括 _Avoid_ 列表——那里写的不只是命名偏好，Detach/Terminate、源项目/附属项目、宿主机/远端主机这些区分直接对应代码里的分支。
- 注释解释的是"为什么"和踩过的坑（多半是实测出来、文档里查不到的），密度偏高是刻意的；改动附近代码时保持同样的说明力度，注释与代码不符时先修注释。
- 架构决策写在 `docs/adr/`：0001 是持久会话为什么选 Zellij 及一长串实现要点，0002 是附属项目与删除护栏，0003 是多仓库项目与批量派生（含回滚与包围盒断言），0005 是 rio 引擎的自持装配层与 WebGPU 渲染器（含踩坑清单），0006 是主题系统（Ghostty 主题格式、双槽位、界面色派生规则、首帧策略），0007 是原始字节路由与 HTML / 图片预览沙箱（路径形状路由、作用域令牌、响应头护栏、图片缩放模型），0008 是文件下载 / 上传（流式 exec 通道、Windows 的 base64 行协议、临时文件 + 字节数核对的落盘规则），0009 是文件面板目录浏览器（平铺当前目录、mkdir / rename / remove、文件夹上传），0010 是飞书项目面板（宿主机上的 meegle CLI 当数据源、实测的 CLI 输出约定、device-code 登录）。做相关改动前先读对应 ADR。
