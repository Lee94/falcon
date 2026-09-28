# 原生客户端：GPUI 薄客户端

`native/` 是 Rust + GPUI 写的桌面客户端，功能与 web 桌面端对齐，连本机与任意一台 falcon 服务端。它对 server 来说就是又一个 Viewer：同一套 REST、同一条 `/ws/sessions/:id`、同一个 `falcon_token` cookie。server 与 web 一行没改。

完整的取舍、指标与分期在 `docs/design/gpui-client.md`，这里只记已经定下来、改代码时必须守住的几条，以及实现过程中推翻了设计文档的地方。

## 决定一：只换前端，后端与协议不动

会话持久性的全部承诺（Zellij、SSH 退避重连、回放前缀、`OscColorGate`）都在后端，重写一遍只会重新踩坑。瓶颈也不在 Node：web 端的结构性开销（每扇终端一个 GL 上下文、不在场的窗口照跑 xterm、回放在主线程解析、字体异步到位后重建图集）全在浏览器这一侧。

`packages/shared` 仍是协议的唯一真相来源。`falcon-proto` 是手写的 serde 镜像，`tests/fixtures/` 由 `native/scripts/gen-fixtures.mjs` 起临时服务端落盘真实响应，server 改字段、重生 fixture，Rust 测试就红。

## 决定二：GPUI 只出现在 falcon-app

| crate | 装什么 | 对应 web |
|---|---|---|
| `falcon-proto` | 协议类型、WS 帧编解码 | `shared/src/index.ts` |
| `falcon-client` | REST / WS / 安装 WS / 流式传输 / 自动重登；内部自带 tokio 运行时，对外只给与执行器无关的 future | `api.ts`、`TerminalView.tsx` 的连接部分 |
| `falcon-term` | alacritty_terminal、快照、按键 / 鼠标 / 滚轮 / 粘贴编码、VT 模式跟踪 | `termAdapter.ts`、`lib/rio/*`、`termModes.ts` |
| `falcon-theme` | Ghostty 主题、语义色派生（与 `derive.ts` 逐字节一致） | `lib/theme/*` |
| `falcon-core` | 列排布、paneKey、项目树、会话标题、宿主机路径、文件搜索、提交图、工作区状态（store.ts 的纯逻辑部分） | `lib/*.ts`、`store.ts` |
| `falcon-app` | 视图、元素、动作、i18n、本地持久化、本机服务托管 | `components/*`、`useActions.ts` |

纯逻辑一律往下放：GPUI 是 pre-1.0 快照，每次破坏性升级只波及最上面一层；下面几层能脱离窗口单测，`falcon-core` / `falcon-theme` 还和 TS 读同一份测试向量（`tests/vectors/`），两边实现一分叉至少一边红——"四个地方必须叫同一个名字"这类规则现在跨了两个客户端，只有这样守得住。

## 决定三：GPUI 走 gpui-kit 锁定版本；Zed 的代码按文件抄

- 依赖 `gpui-kit` 总包，`=` 精确锁（它再精确锁 `gpui-pre` 快照、平台层与 gpui-component）；WebView 用 `gpui-wry` 同版本所锁的那个 wry。升级是一次有意的发版。
- Zed 的 `terminal` crate 牵着一串内部 crate，还会带进第二份 `gpui`（zed git 版），与 `gpui-pre` 类型不通，所以**按文件抄，不按 crate 依赖**：`falcon-term/src/keys.rs`、`falcon-app/src/terminal/element.rs` 出自 `gpui-pre` 对应的那个 zed commit（文件头写着），alacritty_terminal 用同一 commit 锁的 fork rev。升级 gpui-kit 时按新 commit diff 这几个路径。
- 因此 `native/` 整体是 GPL-3.0-or-later（个人使用）。server 与 web 走网络协议，是独立程序，不受影响。

## 决定四：VT 核心 alacritty_terminal，解析在网络线程

- 选它不是因为它最强，而是因为中间隔着 zellij：客户端只需忠实渲染 zellij 发出的序列，**字宽和 zellij 算得一样**（同为 `unicode-width 0.2`）比算得更正确重要。libghostty-vt（`!Send`、API 不稳、要 Zig）与 rio-vt（字宽自带表）隔在 `falcon-term` 接口后面观望。
- OUTPUT 帧在网络线程直接加锁 `advance`，脏位 0→1 才唤醒 UI，每帧至多一次 `notify`；UI 加锁拷一份可见区快照就放锁。
- REPLAY 帧（最大 4MB）离锁解析进一个新 Term，完成后整体换上；回放期间产生的应答（DA / DSR）一律丢弃，否则等于往 zellij 里敲一串垃圾。
- 不在场的窗口照常解析，但不在树里就不画。

## 决定五：连接时序照 web，但 appearance 必须先于 resize

原生客户端**不答颜色查询**（alacritty 的 `ColorRequest` 直接忽略），多个 Viewer 各答一次会把回答叠进 zellij 的输入里。代答交给 server 的 `OscColorGate`，它要先收到 `appearance` 才有颜色可答。

web 端是连上后先发 resize；原生必须**先发 appearance 再发 resize**：resize 是持久会话懒惰接回的信号，接回时 zellij 立刻发 OSC 11 查询，此时 gate 还不知道颜色就只能沉默，zellij 里的程序（Claude Code 的 auto 主题）就会猜错明暗。web 靠 xterm 自己答住了这个窗口，原生没有这层兜底。

其余照 web：每条新 socket 强制发一次 resize；格子量好之前不发；4401 不重连走重登；其他断开 1s×2ⁿ 退避封顶 15s；input 超 1MB 分片；30s 一次 WS ping（反代的 idle timeout）；睡眠唤醒后立即 `reconnect_now`（看墙钟跳变，拿不到系统通知）。

## 决定六：一台 falcon 服务端一个窗口；密码存钥匙串

- 项目 id、会话 id、窗口 key 只在一台服务端内唯一，一个窗口挂多台就得给它们全加前缀、排布格式也和 web 对不上。所以一台一窗，工作区按服务端配置分开存（`<数据目录>/servers/<id>/workspace.json`，JSON 形状与 web 的 `falcon.workspace` 一致）。
- server 的登录 token 只在内存里，重启就全失效。访问密码默认存钥匙串（`keyring`，service `com.falcon.app`），收到 401 / 4401 自动重登一次，失败才弹登录框。不为此改 server 加长效令牌。
- 非回环地址的 `http://` 要用户确认明文风险后才连；TLS 走 rustls + `rustls-platform-verifier`，认系统信任库，不做指纹固定。
- "本机"配置指向 launchd 托管的服务：App 包 Resources 里带 SEA 服务程序，启动时 `service install`（幂等兼升级）再等端口，逻辑出自原来的 `launcher.sh`。**install 要带上已安装服务原来的 `--host` / `--port` / `--data-dir`**（从 `~/Library/LaunchAgents/com.falcon.server.plist` 读，解析口径同 `config.ts`，在 `falcon-core/src/service_args.rs`）：不带参数会写默认配置，自定义过端口 / 数据目录的人一升级，服务就换了个家、原来的会话全看不见——pkg 的 postinstall 与 launcher.sh 也照同一口径带上（`scripts/macos-pkg/service-args.sh`）。"本机"配置连的端口也按它现算，不信存下来的。退出 App = 全部 Detach。只在本机配置上出现的动作（Finder 中显示、本机路径粘贴）按配置开关。

## 决定七：长尾功能全部原生，不留"在浏览器中打开"

文件面板 / 传输 / 查看（含 HTML 预览）、修改 / 差异 / 历史、转发 / 公网发布（ADR 0016 起在设置的「中转」页，不在右侧栏）、飞书项目、各种表单、设置、总览、Zellij 安装、askpass 都原生做。只有移动壳与 PWA 不进原生。

HTML 预览用 wry 的 WebView（WKWebView），守 ADR 0007 的边界：只加载带作用域令牌的 raw URL，不注入登录 cookie，非持久数据存储，导航限在同一 raw 前缀内。WebView 永远压在 GPUI 内容上面，所以浮层打开时先把它藏起来。

## 实现中推翻 / 补充的设计

- **没做 P0 的 web 基线与 go / no-go**：用户决定直接做完整实现。设计文档 §1.3 的指标仍是以后要对照的口径。
- **字体不另 vendor TTF**：`falcon-app/build.rs` 在构建时把 web 已有的 woff2 解成 TTF 嵌进二进制（`woff2-patched`）。GPUI 的 `add_fonts` 不认 WOFF2。
- **HTML 预览直接依赖 wry**（`lb-wry`，与 gpui-kit 同批发布的 `gpui-wry` 锁的是同一版本），不经 gpui-wry：它不重新导出 wry，而 `file_view/html.rs` 要直接在 `WebViewBuilder` 上挂导航 / 新窗口 / 下载的回调来守 ADR 0007 的边界。挂在默认开启的 `webview` feature 上。
- **外链图片**（飞书头像、Markdown 里的 https 图片）：给 GPUI 装一个 `HttpClient`（`falcon-app/src/http.rs`），转给 `falcon_client::fetch_external`——同一个网络运行时与 TLS，不带 falcon 的 cookie，只许 GET / HEAD，响应体 20MB 封顶。
- **图标字体走回退链而不是按码位指定**：GPUI 拒绝把不含 `m` 的字体当主字体（Symbols Nerd Font Mono 就是），所以它挂在回退链的第一位，排在 Ioskeley / Maple 前面——效果与 web 字体栈"图标字体排在主字体前"一致，Maple 的宽形 NF 图标不会被选中。
- **界面验证靠 Metal 回读**：`--features snapshot` 打开 `Window::render_to_image()`，配 `FALCON_AUTOMATE` 的脚本步骤（选项目、开终端、打字、点击、拖拽、滚轮、派发动作、截图、定速重画）驱动。锁屏 / 远程时 `screencapture` 是黑的，这条路不受影响；但锁屏时显示链路也不走，notify 过的内容不会自己画出来，`snap` 前要手动画两帧（第一帧里画布这类靠 prepaint 量尺寸的视图还是空的）。`snapshot` 带着 gpui-kit 的 test-support，压测用只含脚本驱动的 `automation` feature。
- **Windows 仍只守住不堵路**：平台相关的只有 `local_service.rs`（仅 macOS）、钥匙串与数据目录（跨平台 crate）、菜单栏；快捷键在非 mac 上是 ctrl-shift 系。

## 踩过的坑

- **量不出格子时不许 resize**：画布首帧还没拿到自己的 bounds，窗格按 0 排，终端被量成 2×3。zellij 在 alt screen 里没有回滚，缩到 2×3 再放大整屏内容就丢了；回放先于首次测量到达时服务端尺寸没变、不会重画，终端就一直是空的。终端元素量出的列 < 2 或行 < 1 时不报尺寸（xterm fit addon 同样保留旧值）。
- **VT 模式跟踪器的 OSC 扫描曾是 O(n²)**：BEL 与 ESC 分两趟各从当前位置扫到缓冲区末尾，zellij 的 OSC 8 超链接用 ST 收尾、整段没有 BEL，4MB 回放在 debug 构建里解析了 4 分钟（release 也要几秒），期间网络 worker 被占住、这扇终端一片空白。`falcon-term/src/modes.rs` 改成一趟找两者，有回归用例。**TS 版 `shared/src/termModes.ts` 是同样的写法**，服务端每段输出都跑它，尚未改。
- **对话框打开后要再聚焦输入框**：`window.open_dialog` 会把焦点收到对话框自己的容器上，先聚焦再打开等于白做——敲的字落回原先聚焦的终端（askpass 的密码会在 shell 里明文回显）。输入框里的 ↑↓ / 回车被 Input 的动作吃掉，列表导航要在捕获阶段截 `MoveUp` / `MoveDown` / `Enter` 动作，key_down 监听永远等不到。
- **通知层要推迟绘制**：按顺序排在对话框层后面仍会被设置对话框盖住，`deferred(...).with_priority(..)` 才稳。
- **侧栏、右侧面板必须是缓存视图**：GPUI 默认每帧重排整棵树，7 行的侧栏每帧就 3ms 多，历史面板再加近 3ms。包成 `Entity::cached` 后，终端刷屏时只有终端在重画。
- **rem 必须是 16px，界面缩放靠它**：gpui-component 的 Root 每帧拿 `theme.font_size` 调 `set_rem_size`。它一度被设成 13 想当"正文 13px"用，结果所有 rem 尺寸（`text_xs()`、`p_2()`、`h_8()`、组件库的按钮 / 输入框）一起缩成 web 的 81%——text-xs 只剩 9.75px，界面整体偏小。现在 `theme.font_size = 16 × 倍数`，正文 13px 显式挂在窗口根上；写死的像素一律写 `zoom::zpx(..)`，⌘+ / ⌘− / 设置里的「界面缩放」改的就是这个倍数（`zoom.rs` 顶部写了哪些不缩：画布几何、终端画面、窗口外框、圆角与缝）。存下来的侧栏 / 右面板宽度是缩放前的逻辑像素，拖宽时指针位移先除以倍数。
- **Root 把 Tab / Shift+Tab 绑成了焦点轮转**（context "Root"）：keymap 先于 `on_key_down` 匹配，终端里的 Tab 永远到不了 shell。在 "Terminal" context 上绑 `NoAction` 屏蔽掉。以后组件库再往 Root 上加裸键绑定，照此办理。
- **GPUI 的滚动监听从不 stop_propagation，只开一个轴的容器还会把另一轴的滚轮换算过来**：画布只开了横向，差异 / 文件 / 没有回滚的终端里纵向一滚，画布就跟着横漂。画布加 `restrict_scroll_to_axis()`（同 web 的 WheelAxisLock）；差异视图的 list 还能横滚时自己 stop_propagation（web 的 innerTakesWheel）。
- **窗口内容里的点击要在捕获阶段激活窗口**：zellij 常开鼠标上报，终端的 mouse_down 上报完就 stop_propagation，挂在窗口上的冒泡监听收不到——键盘焦点跟过去了，标题栏着色和侧栏选中却留在原处。
- **ContentMask 只有矩形**：窗口 `.rounded()` 裁不住子元素，终端 / 文件 / 差异的实心底和活动标题栏的 tint 会把圆角铺成直角。窗口最上层叠一圈窗口底色的粗描边盖住圆弧以外的角（`canvas.rs` 的 `corner_caps`），活动窗口的 `tint_strong` 描边也是叠上去的——给窗口加 border 会占布局，终端少一列、换活动窗口就触发 resize。
- **画布量出宽度之前不许滚到活动列**：开窗第一帧视口是 0，那时算的滚动量是错的、又记成"已滚过"，重启后活动窗口停在视口外。
- **滚动条要自己控制显隐**：GPUI 在 macOS（系统设成"自动"时）只在滚动时显示滚动条，它的 Hover 模式也只认指针落在滚动条那一窄条上。差异视图在指针进入时强制 `ScrollbarMode::Always`，对齐 web 的"悬停浮出"。

## 测量（2026-09-24，Apple Silicon，release，本机 falcon 服务端）

锁屏下用 `frames` 步骤以 60Hz 手动画（不含 GPU 提交），一列两扇终端、侧栏展开：

| 场景 | 帧耗时 p50 / p95 / 最慢 | 客户端 CPU | RSS |
|---|---|---|---|
| 空闲（每帧强制重画） | 1.99 / 2.56 / 2.84 ms | — | 127 MB |
| `yes … \| head -n 300000`（中英文 + 盒线） | 2.83 / 3.68 / 8.31 ms | 16–20% | 127 MB |
| 每帧无视缓存全量重排（悬停时的情形） | 4.83 / 5.87 / 7.62 ms | — | — |

真机上 GPUI 只在视图变脏时才画，空闲 CPU 接近 0。web 基线没有测（见上）。

## 没做 / 已知限制

- 全局事件推送（`/ws/events`）：会话 5s、askpass 1.5s、git 计数 8s 照 web 轮询。
- 无障碍：GPUI 的 AccessKit 支持刚合入上游，读屏基本不可用。
- 输入延迟（zed#26900）未实测；WebView、真实输入法、真实 SSH 远端与 Windows 都只在代码层面做了，没在真机上跑过。
- 侧栏每次全量重排约 3ms（7 行），开销分散在 GPUI 每个元素的排版与状态查找上，项目很多时悬停会变重。
