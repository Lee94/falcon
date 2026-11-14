# 客户端渲染统一至 Native Shell（NSH）

在 [ADR 0015](./0015-gpui-native-client.md) 落地后，仓库中形成了两套割裂的客户端实现：桌面端（macOS / Windows）使用 Rust + `gpui-kit`（`falcon-ui` / `falcon-desktop` / `falcon-platform`），移动端使用 Kotlin + Jetpack Compose + 手写 JNI（`android/` + `falcon-android`）；Linux 仅有服务端安装包而无桌面客户端，浏览器静态版与 TUI 客户端缺失，右侧栏插件受限于固定词汇的声明式 `PluginView`（[ADR 0025](./0025-right-panel-plugins.md)）。

本 ADR 决定将客户端界面与渲染层统一迁移至 **Native Shell（NSH，`longbridge/nsh`）**，以一套纯 Rust 组件与响应式状态图驱动 **macOS、Windows、Linux、Android、Web（`wasm32`）与 TUI** 六端渲染后端。

## 决定一：服务端、会话守护进程与线协议不动，下层五个 crate 原样复用

持久会话（`falcon-sessiond`，[ADR 0020](./0020-session-daemon-libghostty.md)）、宿主机节点网关（[ADR 0027](./0027-host-owned-nodes.md)）、SSH 退避重连、16ms 输出合并窗口、多观看端尺寸仲裁（[ADR 0022](./0022-terminal-size-latest-visible.md)）与 `/ws/sessions/:id` 混合帧协议全部保留在服务端，不做破坏性修改。

客户端下层已与 GPUI 解耦的五个纯逻辑 crate 继续作为唯一共享基座（均保持 `wasm32-unknown-unknown` 兼容）：

| crate | 职责 | 迁移期变化 |
|---|---|---|
| `falcon-proto` | 协议类型、WS 帧编解码、VT 模式跟踪、`PluginView` | 仅为插件清单增量补充可选 `.nshpkg` 字段（`#[serde(default)]`） |
| `falcon-client` | REST / WS / 流式传输；原生自带 `falcon-net` tokio 运行时，`wasm32` 走浏览器 WebSocket | 原样复用 |
| `falcon-term` | 裁去 `tty` 的 `alacritty_terminal`、可见区快照、键鼠滚轮与粘贴编码、路径链接识别 | 原样复用 |
| `falcon-theme` | 465 套内置 Ghostty 主题、双槽位与 OKLab 语义色派生（金标准 `derive_golden` 冻结） | 原样复用 |
| `falcon-core` | 列式工作区排布、`pane_key`、自动标题、宿主机路径、文件编辑判定、多客户端画布缩放等 | 收拢 `AgentModeTransition` 状态机与工作区轮询协调器 |

`falcon-platform` 移除所有 `gpui_kit` 类型（`App`、`Window`、`Bounds`、`Pixels`、`Global`），改为纯 Rust 的宿主能力接口（本地键值存储、工作区与服务端列表持久化、密码存储、本机服务 `LocalService`、应用图标、提示音、应用更新器 `AppUpdater`）。HTML 预览改用 NSH 标准元素 `web_view`，不再经 `Platform` 暴露 `lb-wry` 句柄。

## 决定二：主应用走纯 Rust NSH 原生后端，零 QuickJS 开销

NSH 同时支持 TSX（QuickJS）与纯 Rust（`#[component]` + `nsh-core` + `nsh-base`）两种编写方式。Falcon 客户端采用**纯 Rust 主应用 + 各平台原生渲染后端**，主界面路径不初始化 QuickJS 虚拟机：

- **macOS**：`nsh-platform-macos`（AppKit + CoreText）
- **Windows**：`nsh-platform-windows`（Win32 + Direct2D + DirectWrite）
- **Linux**：`nsh-platform-linux`（GTK 4 + Pango，独立于无 GUI 依赖的服务端包打包）
- **Android**：`nsh-platform-android`（Android Views + StaticLayout，`xtask android` 直出 APK，退役 Gradle / Kotlin / 手写 JNI）
- **Web**：`nsh-platform-web`（HTML/CSS + Wasm，默认可由 `falcon-server` 同源托管静态产物，同时保留独立静态目录输出）
- **TUI**：`nsh-platform-tui`（`ratatui` 字符网格，支持 `falcon tui` 子命令或独立入口在纯终端环境打开工作台）

内置重型视图优先复用 `nsh-base` 原生组件：结构化 Agent GUI 时间线用 `text_view`，文件查看与编辑用 `editor`（外包 `falcon_core::file_text` 守住合法未截断 UTF-8、换行还原、哈希判冲突与未保存拦截，[ADR 0023](./0023-file-view-editing.md)），差异对比用 `diff_view`，HTML 原始字节预览用 `web_view(raw_url).incognito(true)`（严守 [ADR 0007](./0007-raw-file-preview.md) 作用域令牌与导航前缀拦截）。

## 决定三：Ghostty 主题动态覆盖 NSH Tokens，深浅只看主题底色亮度

NSH 默认按操作系统明暗切换静态 `theme.json` 的 `light` / `dark` 分支；而 Falcon 的契约（[ADR 0006](./0006-ghostty-themes.md)、[ADR 0011](./0011-floating-island-shell.md)）规定浅色槽位与深色槽位可放任意 Ghostty 主题，且**界面深浅（`is_dark`）只由当前生效主题底色亮度（`falcon_theme::appearance_from_rgb`）决定**。

因此客户端不依赖静态双分支色表，而是在每次主题或明暗模式变化时：
1. 由 `falcon_theme::resolve_theme_mode` 选出槽位并经 `derive_theme` 算出 `ResolvedTheme`；
2. 将派生出的标准 token 与 Falcon 扩展 token（`app`、`app-border`、`tint`、`tint-strong`、`selection`、`syntax-*`、`graph-0..5`）同时写入 NSH `Tokens` 的 `light` 与 `dark` 分支并通过 `set_tokens` 注入；
3. 调用 `set_color_scheme` 将宿主窗口外观强制对齐 `resolved.is_dark`，保证原生滚动条、输入法候选窗与状态栏不反色。

界面缩放沿用 `zpx` 语义：⌘+ / ⌘− 调整根字号缩放倍数（`16pt × scale`），画布几何缝隙、圆角与终端画面字号保持独立。文案继续存放在单一 `locales/zh-CN.json` 并经 `t!("key")` 读取，禁止在界面代码中硬编码中文或颜色值。

## 决定四：基于 NSH 原语自建 `FalconTerminal`，预留 `NativeComponent` 替换口

`nsh_base::Terminal` 采用 `virtual_list` 与固定 16 色表，不支持 Ghostty 256 色与 OSC 10–12 覆盖、终端鼠标模式上报、[ADR 0022](./0022-terminal-size-latest-visible.md) 非尺寸持有窗口的缩字平移（`MIN_FOLLOW_SCALE = 0.5`）及文件路径链接点击。

`falcon-nsh` 直接基于 `falcon-term::TermCore` 构建跨端组件 `FalconTerminal`：
1. **默认渲染面**：使用 `nsh_core::prelude::canvas(Painter)` 一次录制背景合并矩形、选区、等宽文本段、盒线/块元素几何、光标与悬停路径下划线；非尺寸持有窗口直接在 `Painter` 顶层施加 `translate` + `scale`；光标格上方挂载透明 `1pt` 宽隐藏 `Input` 承接各端原生输入法候选窗定位、预编辑组合串与软键盘唤起。
2. **TUI 形态特化**：在 `Idiom::Terminal` 下自动切为字符行渲染，并采用显式的**工作区导航态 / 终端穿透态**双模焦点（在终端窗口按 `Enter` 捕获全部按键发往远端 PTY，按逃生前缀 `Ctrl+\` 返回外层工作台导航）。
3. **连接与查询纪律**：严格保持 `appearance` 先于首次 `resize` 发送；测得列 `< 2` 或行 `< 1` 时绝不发 `resize`；`durable` 持久会话不在客户端回写终端查询应答。
4. **`NativeComponent` 预留口**：将绘制面隔离在 `TerminalSurface` 抽象后；若实测某平台极端刷屏帧耗时 p95 未达标，可在 `falcon-app` 按平台热替换为原生绘制面（CoreText / Direct2D / Android Canvas），上层会话与输入逻辑零改动。

## 决定五：右侧栏插件采用 `PluginView` + `.nshpkg` 自托管签名沙箱双轨制

1. **存量 `PluginView` 轨道完整保留**：内置插件（系统监控、Docker）与命令插件（`<数据目录>/plugins/<id>/plugin.json`）协议与安全边界不变，由 `falcon-nsh` 用语义色原生组件渲染。
2. **新增 `.nshpkg` 签名沙箱轨道**：当插件清单声明已签名的 `.nshpkg` 包时，客户端仅在该插件面板打开期间挂载隔离的子 `AppView`（限制 QuickJS 堆上限与调用预算，默认禁止任意网络与本地文件访问），注入当前 Ghostty 派生 `Tokens`，并通过受控宿主通道与服务端白名单动作交互。
3. **自托管信任模型**：为避免中心化私钥阻碍自托管用户编写本地插件，`falcon-server` 在 `<数据目录>/plugins/.keys/`（`0600`）按需生成本实例的 Ed25519 插件根密钥与签发证书（或对放入插件目录的包执行 `nsh_package::resign`），经 `GET /api/plugins` 下发本实例根公钥，客户端使用 `nsh_package::TrustStore` 强制验签与防降级校验后再加载。

## 迁移策略与后记

采用特性分支分阶段推进，在完成桌面宿主集成（macOS 菜单栏状态项与多实例标记、关最后一扇窗口保活、LaunchAgent 原参数 `service install`、应用更新未保存拦截与 `.app` 备份回滚、Windows 计划任务与凭据库、Android 旧版 `falcon_android_prefs.xml` 首启自动迁移）、真服务端 REST + `/ws/sessions/:id` 实时接线与 `xtask` 六端打包（`pkg` / `win` / `linux-desktop` / `android` / `web` / `tui`）后，已完整退役 `falcon-ui`、`falcon-desktop`、`falcon-android`、`android/` 与 `gpui-kit` / `lb-wry` 依赖。
