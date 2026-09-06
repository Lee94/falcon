# rio 引擎自持装配层并自研 WebGPU 渲染器

rio 引擎（rioterm，Rio 的 Rust VT 核心编译成 WASM）解析吞吐是 xterm.js 的 3.8×，但它自带的渲染器是纯 JS 的 canvas 2D：每帧全量重画所有行、逐格 `fillText`、每格把颜色 resolve 成 CSS 字符串、完全没用 WASM 给出的脏行标记；低端机上 TUI 高频重绘掉帧一半。我们决定：**复刻 rioterm 的 `open()` 装配层放进本仓库（`packages/web/src/lib/rio/`），Terminal / VT 核心仍用 rioterm，画面由自研的 WebGPU 渲染器画，WebGPU 不可用或设备丢失时回落 rioterm 自带的 canvas 渲染器。** 引擎选项不加新值：设置里仍是 `rio`，内部自动优先 WebGPU。

为什么必须复刻 `open()`：rioterm 的 `open()` 把 `CanvasRenderer | DOMRenderer` 写死，没有渲染器注入口；而且渲染器构造时量死 cell 尺寸，改字体只能 serialize → dispose → 重开整个 WASM 实例 → 手工猜 VT 模式。自己持有这一层之后，换字体 / 字号 / 主题 / DPR 只换渲染器，Terminal 不动——`Terminal.setCellSize` 保 cols/rows，内容、alt screen、鼠标模式、bracketed paste 全都天然保住；以前 RioAdapter 里 IME 与全局快捷键的 capture 拦截 + 克隆重派发补丁也随之删除。

## Considered Options

- **继续用 rioterm 自带 canvas 渲染器**：短板不只是慢——每帧不清屏导致半透明选区跨帧累积、DPR 只在构造时读一次、5 种下划线都画成 1px 单线、空格上的下划线直接丢、改外观全实例重建。逐条修等于重写。
- **给上游提 PR 加渲染器注入口**：周期不可控，而且 `open()` 里的键盘 / 剪贴板 / 滚轮策略本来就要按我们的宿主改（IME 放行、全局键冒泡、滚轮攒余量、不双写剪贴板）。
- **WebGL2 而非 WebGPU**：覆盖面更广（Firefox Linux / Android、老 iOS），但 iOS 26 / Safari 26 已有 WebGPU，xterm.js 的 webgl addon 已经占了那条路；两者渲染器结构一样，只是后端不同，后续需要时可加。
- **把 Rio 桌面版的 sugarloaf（wgpu）编成 wasm**：才是"真正的 Rio WebGPU"，但 riotermjs 有意只编 librio，sugarloaf 上 web 要自带字体、拉进 wgpu 与 swash，wasm 从 1.1MB 涨到数 MB，还得自维护 Rust 构建。
- **选中：复刻 `open()` + 自研 WebGPU 渲染器**，设计蓝本是 sugarloaf `grid/`（Ghostty 渲染器的移植）。

## 实现要点

以下多半是这次落地时实测出来的，文档里查不到。

- **渲染器契约与 construct-then-swap**（`renderer.ts` / `open.ts`）：渲染器构造同步、自己 `terminal.setCellSize` + 订阅 `onUpdate`；换渲染器先构造新的，成功了才 dispose 旧的，新的构造抛异常则旧的原样保留。两种渲染器的 cell 量法不要求一致，换完宿主总会再 fit 一次。
- **adopt 时必须先 fit 再回放输出队列**。重载页面时 WS 的整份 replay 比 wasm + GPU 设备先就绪，会被写进默认的 80×24 网格；随后 resize 把 alt screen 的内容截掉，而服务端按尺寸去重不会再让 zellij 重绘——屏幕就一直空着，直到用户敲一个键。canvas 路径以前没暴露，是因为没有 GPU 设备获取那段延迟。
- **bg 层用 cols×rows 的 `rgba8unorm` 纹理 + 一个全屏三角形**，片元按 `floor(pixel / cell)` 查格，不用 storage buffer：compat 模式（Android 的 GLES 后端）`maxStorageBuffersInFragmentStage` 可能为 0。脏行用 `writeTexture` 单行上传，它没有 256 字节对齐要求（那是 `copyBufferToTexture` 的）。
- **fg 层每行固定容量槽位（cols × 4 实例）**，脏行只 `writeBuffer` 自己的槽，逐行 draw；50 个 draw 在一个 pass 里可忽略，省掉 sugarloaf 那种每帧 concat。实例 24 字节，布局由 `frame.test.ts` 回读逐字段钉死。
- **脏行自己维护 `pending`**：`snapshot()` 内部会 `reset_dirty()`，一帧中途失败（图集满重建）脏信息就丢了；WASM 的标记先并进 pending，上传成功才清。`displayOffset ≠ 0`（看历史）时每帧全量：WASM 滚动时是否全置脏没有文档保证。
- **block 光标与失焦空心框走 uniform 在 shader 里画**，bar / underline 光标是独立的 1 实例第三次 draw：光标移动 / 闪烁不脏任何行，blink-only 帧只写 uniform。
- **颜色全在 CPU 折算成 premultiplied RGBA8**（调色板、inverse、dim、选区 over 合成），shader 无状态位；u32 字节序 r 在最低位，写进 `Uint32Array` 后小端字节就是 `[r,g,b,a]`，纹理与 `unorm8x4` 顶点属性都不用 swizzle。选区背景保留 alpha 在 CPU 合成（canvas 回退才需要 `opaqueOver` 预混），选中文字保留原色。
- **cell 尺寸以物理像素整数为唯一真相**，CSS 值是物理值 / dpr、允许分数，只给 fit / cellAt 用。行高按字体行盒（`fontBoundingBoxAscent + Descent`）× lineHeight 取整，与 xterm 同口径：两引擎在同一个 pane 算出相同行数，切引擎不会挤一下 zellij；rioterm 用 `fontSize × lineHeight`，lineHeight 1 时 g/y 的尾巴直接被裁。
- **字形位图只开 ink 盒那么大**（`measureText` 的 actualBoundingBox 四边各加 1px），保留溢出（斜体尾巴、Nerd 图标、CJK 回退字体）。emoji 不查 Unicode 表：彩色字体会无视 fillStyle，像素颜色偏离画笔色就进 RGBA8 图集。
- **字形按前景色的颜色桶栅格，不能永远画白字再着色**（2026-09 实测）：Chrome（Skia）按画笔亮度查 gamma 预混表修正灰度遮罩，白字最重、黑字最轻，同一字形覆盖量差 12%（F 竖笔 2.88 vs 2.65px）；rio 原来永远画白字取 alpha，浅色主题（#171717 字 / 白底）下端到端墨量比 xterm 多 28% 到 33%，用户看着就是字重更大。这张表只看画笔每通道的高 2 位（灰阶扫描跳变点正好在 64 / 128 / 192，150 个随机色与其 2 bit 量化色遮罩逐像素全等，按亮度换成灰再画只有 111/150 全等），所以 `glyphKey` 多 6 bit 颜色桶、`buildRow` 把折算完的 fg 传给 lookup、按桶中点色画字，彩色字形跨桶共享。修完 rio 与浏览器原生画该颜色逐像素一致；xterm 自己还比原生再细 10% 到 14%（它的图集用 `gl.LINEAR` 采样、抠底阈值 |Δ|/12），没有继续追。Ghostty 的对照：CoreText 栅格默认关 font smoothing 得到颜色中性遮罩，`font-thicken-strength` 靠往 alpha-only 上下文里画不同亮度的灰控制加粗量，`alpha-blending = linear-corrected` 在片元里按 fg/bg 亮度修正 alpha；浏览器里拿不到颜色中性遮罩，所以走按桶栅格。
- **盒线 / 块元素 / braille / powerline 程序化绘制**（`sprites.ts`）：字体里的盒线字形按字体自己的行盒设计，Maple Mono 的 ┼ 比 1.0 倍行高的格子高 50%，上下溢进邻行，回退字体又常与格宽对不齐。双线 junction 用 3×3 网格 + 停线规则复原 Unicode 图表里所有 ╒ ╤ ╔ ╬ 组合，见 `sprites.test.ts`。
- **键盘分流**（`keyRoute.ts`）：IME 组合（`isComposing` / keyCode 229）与命中全局快捷键的按键既不 preventDefault 也不 stopPropagation，前者让组合文本落进 textarea 走 compositionend，后者让事件冒泡到 App 的 window 监听。⌘C / Ctrl+Shift+C 只在有选区时复制，与 rioterm 一致。
- **IME 候选框锚点与预编辑层**（`ime.ts` / `open.ts`）：浏览器按接收 composition 的 textarea 定位系统候选框，不认 canvas/WebGPU 光标。隐藏 textarea 因此保留一个真实 cell 的尺寸，并按 `cursorPosition × cell metrics` 移到终端光标；当前 tab 的 `onUpdate` 按帧合并同步，换渲染器后立即按新 cell 重算，查看回滚区时沿用最后一个有效位置。组合中的拼音落在透明 textarea 里看不见，渲染器也画不了（组合串没进 PTY，WASM 里没有格子），所以照 xterm 的 composition-view 加一个 DOM 覆盖层：按终端字体与主题字/底色把 `compositionupdate.data` 画在光标格上，最多延伸到网格右边缘（rtl 容器 + LRM，超长时看到末尾），组合期间 textarea 与覆盖层同宽，候选框才锚在组合串末尾。
- **滚轮分两种口径**（`wheel.ts`）：程序接管滚轮（`terminal.modes()` 的 mouseTracking 或 altScreen，zellij 里永远是）时一个 DOM 事件最多折成一次点击。rioterm 的 `scroll_wheel(n)` 会把 n 行翻成 n 条 SGR 上报 / n 个方向键（Node 里驱动 wasm 实测），zellij 收到每条上报自己再滚 3 行，macOS 鼠标一格 deltaY 上百像素折五六行就冲出十几行，用户看着是"一滚就过了"。xterm.js（`MouseService._consumeWheelEvent`）行数只当门槛、每事件最多一条、多出的整行丢掉、|deltaY| < 50 当触控板乘 0.3，这里照抄，两引擎手感一致。本地 scrollback 才按行推并攒余量：rioterm 用 `Math.trunc(lines)`，触控板慢滚在 scrollback 里根本不动。
- **鼠标按键上报自己合成**（`mouse.ts` + shared 的 `TermModeTracker`）：rioterm 0.1.8（也是 npm 上的最新版）的 WASM 只导出 `scroll_wheel`，按下 / 松开 / 拖动没有 API，`modes()` 也只给一个 mouseTracking 位；上游 dom.ts 只做本地选区，zellij 切 tab、vim 点光标在 rio 下全是哑的。做法：输出一律经 `handle.write()` 喂给服务端回放前缀用的同一个 `TermModeTracker`（回放前缀本身也在流里，重连后照样对上），拿到精确的协议（?9 / ?1000 / ?1002 / ?1003）与编码（?1006 / ?1016），按 xterm.js `MouseStateService` 的筛选 / 编码 / 移动去重合成报文，`terminal.input()` 经 send_text 原样到 onData。按住 Shift 绕过上报做本地选区（zellij 文档、Ghostty / Alacritty / Kitty 都是 Shift；xterm.js 在 macOS 上要开 `macOptionClickForcesSelection` 才有绕过键且是 Option，我们没开，所以 xterm 引擎在 macOS 上没有绕过键，这是两引擎目前唯一的手感差异）。默认单字节编码超过 ASCII 的报文丢掉：输入通道是 JSON 文本帧，0x80 以上的字节会被 UTF-8 成两字节；xterm 引擎那边这种报文走 onBinary 而 adapter 只接了 onData，本来就发不出去。
- **`navigator.gpu` 只在 secure context 暴露**：这个项目常用 `http://<局域网 IP>:4923` 访问，那时它是 undefined，先查 `isSecureContext` 而不是等 `requestAdapter` 返回 null。`isFallbackAdapter` 新规范挪进了 `adapter.info`，两处都看。
- **一个页面一个 GPUDevice**（`gpu.ts` 单例），pipeline 按 canvas format 挂在设备上共享；`getPreferredCanvasFormat()` 在 Safari 是 `bgra8unorm`，绝不硬编码。device lost 先重新 `requestAdapter` 原地重建，拿不到再退 canvas；`"destroyed"` 是我们主动 destroy 才有的原因，出现即忽略。
- **WGSL 整数 varying 必须 `@interpolate(flat)`**，否则编译失败（Safari 最严）；WGSL 写成字符串常量，tsconfig 没有 vite/client 类型，`?raw` 会报错。
- **TS 类型**：`@webgpu/types` 走 `src/lib/rio/webgpu-types.d.ts` 的三斜线引用，不动 tsconfig 的 `types`（写了就关掉 `@types/*` 自动包含，测试里的 `node:test` 立刻失去类型）。TS 5.7+ 的 `Uint8Array<ArrayBufferLike>` 与 `BufferSource` 对不上，`writeTexture` 处要 cast。
- **验证技巧**：WebGPU 画布 present 之后 `toDataURL` / `drawImage` 回读是空的，看像素只能用页面截图再裁剪；缩放截图会把 1px 线吞掉，"有缝"要用像素级测量确认。rio-vt 不把 SGR 21 当双下划线，测试要用 `4:2`。
- **调试逃生口**：`localStorage["falcon.rio.renderer"] = "canvas" | "webgpu"` 覆盖自动选择；DEV 下 `globalThis.__rioHandles`（活着的 handle：`rendererKind` / `fallbackReason` / `renderer.stats`）与 `globalThis.__rioGpu`。

## 供应链与授权

- `rioterm` 0.1.8（MIT，raphamorim/riotermjs）：**锁定精确版本**。我们依赖它的 `handleKeyboardEvent` 行为、格子打包格式（`frame.ts` 复制了一份常量而不 import，那个模块顶层就引 wasm 胶水）、`CanvasRenderer` 的公共契约与 `open()` 的接线细节；升级前重新核对 src/dom.ts、canvas.ts、keys.ts、core.ts。`open.ts` 文件头标注了移植来源。
- 设计参考 sugarloaf `grid/`（MIT）与 Ghostty；盒线表照 Unicode 图表手写，没有拷贝代码。
- `@webgpu/types`（BSD-3）仅类型；不新增运行时依赖。

## 验证状态

已在 Chrome 桌面（macOS，dpr 2，localhost）验证：文字 / 颜色 / 粗斜体 / dim / inverse、5 种下划线与删除线、CJK 宽字符、彩色 emoji、盒线（细 / 粗 / 双 / 圆角 / 对角线 / 虚线）与块元素 / 阴影 / braille、块光标闪烁与失焦空心、⌘K 等全局快捷键穿透、键盘输入与回显；换字号 / 主题 / 光标样式时同一个 Terminal 实例、不新建 WebSocket、VT 模式保留；`navigator.gpu` 缺失时回落 canvas、每页面一次 toast、设置页提示；盒线相邻格像素级相接。单测 79 个覆盖纯函数层（key 分流、滚轮、DPR、GPU 获取、度量、颜色、图集分配、装饰、行构建与脏行、sprite）。鼠标上报（2026-09-05）在无头 Chrome 里用 CDP 真实鼠标输入对着直接装配的 `openRio` 验过：?1002/?1006 的点击、拖动（含 window 级松开）、右键、Shift 绕过做本地选区、?1003 无按键移动、默认单字节编码、RIS 与 ?1002l 之后回到本地选区，报文字节与 xterm.js 逐条一致；没有在真实 zellij 会话里点过，那条链路（WS → server → PTY）与 xterm 引擎共用。

微基准（2026-09-02，Mac mini Apple Silicon，Chrome，dpr 2，220×50，系统等宽 13px，3 轮中位；脚本 `scripts/bench-term-engines.js`）：

| | xterm-webgl | rio-webgpu | rio-canvas |
|---|---|---|---|
| 10.4MB / 10 万行 ASCII 灌屏，1× | 392 ms | 50 ms | 50 ms |
| 5.9MB / 5 万行中文灌屏，1× | 367 ms | 150 ms | 150 ms |
| 同上两项，4× CPU 节流 | 1517 / 917 ms | 149 / 550 ms | 149 / 550 ms |
| TUI 每 tick 5 帧 × 60 tick，4×（满帧 ≈ 1000） | 1051 ms | 1003 ms | 2382 ms |
| TUI 每 tick 20 帧（27MB/s），4×：tick 墙钟 / 画完积压 | 1001 / 1317 ms | 1016 / 1051 ms | 2967 / 3001 ms |
| TUI 每 tick 40 帧（55MB/s），4× | 1010 / 2199 ms | 1701 / 1716 ms | 3700 / 3736 ms |

读法：灌屏是解析为主，rio 的 WASM 解析比 xterm 快 8×（中文 2.4×），两种 rio 渲染器只画最后一帧所以相同。TUI 重绘才看渲染器：4× 节流下 rio-canvas 掉到 25fps，rio-webgpu 与 xterm-webgl 都保住 60fps；再加压时 xterm 的 write 是异步的，靠推迟解析保住 tick 但画面积压（40 帧 / tick 时落后 1.2s），rio 的 write 同步，画面永远最新、压力直接体现在 tick 上（40 帧 / tick 降到 35fps）。全速 1× 下三者在 55MB/s 的重压下都满帧。

未验证：Safari 26 / iOS、Android Chrome、Firefox；DPR 变化（跨屏拖窗）；真实 device lost；真实输入法；触屏；多 tab 泄漏。

## Consequences

- `open.ts` 与上游 rioterm 的 `open()` 会漂移，由我们维护；上游修的 bug 不会自动带过来。
- 引擎仍是两值（xterm / rio），WebGPU 与 canvas 的切换对用户不可见，只靠 toast 与设置页提示；花屏但不抛异常时靠 localStorage 逃生口。
- 明确不做（可后补）：触屏滚动、软键盘退格连删（rio 的 textarea 现在归我们，xterm 那套哨兵可以搬）、跨终端共享图集、DOMRenderer。
