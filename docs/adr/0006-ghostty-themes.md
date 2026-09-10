# 主题系统：Ghostty 主题格式、双槽位、整套界面色由主题派生

以前的"主题"是两件事拼起来的：界面只有 shadcn neutral 的浅 / 深两套写死在 `styles.css`，终端另有一个 17 项的固定配色下拉（`lib/term.ts` 里手抄的 xterm ITheme 表），"跟随界面"时才与界面同色。用户装的是 Ghostty，自己维护着主题文件，却没法把同一套配色带进 Falcon；界面与终端两套颜色各说各话。

现在决定：**主题的数据模型就是 Ghostty 主题文件**（`background` / `foreground` / `cursor-color` / `cursor-text` / `selection-background` / `selection-foreground` / `palette = N=#rrggbb`），**浅色 / 深色各一个槽位**（对应 Ghostty 的 `theme = light:X,dark:Y`），**整个应用的颜色——shadcn 语义 token、语法高亮、终端画面——全部从当前槽位那一套主题派生**。内置目录是 Falcon 两套默认加 Ghostty 1.3.1 内置的全部 463 套，名字与 `ghostty +list-themes` 一字不差；也可以贴一段 Ghostty 主题文本当自定义主题。实现在 `packages/web/src/lib/theme/`，全部纯函数层有单测（67 个）。

## Considered Options

- **保留界面两套 + 终端独立配色**：最省事，但界面与终端永远两种调性，"主题"这个词在产品里就是分裂的。
- **自己定义一个主题 JSON 格式，再写 Ghostty 导入器**：多一层格式转换，而且导入永远是有损的——用户改了 Ghostty 文件还要重新导入。直接把 Ghostty 格式当数据模型，解析 → 序列化是恒等的（测试钉住），"复制为 Ghostty 主题"就是把颜色原样写回去。
- **界面色在 CSS 里用 `color-mix()` 从几个基色变量推**：CSS 就是设计文档，改比例不用动 JS；但语义色要按对比度在普通色 / 亮色之间挑、不够时再往字色掺，这种判断 CSS 做不了，而且没法单测。选中：**全部在 JS 里推（`derive.ts`），把结果写到 `<html>` 的内联 style 上**，CSS 只留兜底与 `color-scheme`。
- **只允许一套主题，明暗时自动反色**：反色出来的主题没人认得；Ghostty 自己也是 light/dark 两套。
- **主题只存名字，启动时查目录**：目录 77KB，首帧要等它；改成槽位里存颜色**副本**，目录只在打开选择器时懒加载，目录升级也不会在用户没动过设置时悄悄换脸。

## 实现要点

- **`ghostty.ts` 的解析宽容、缺省严格照 Ghostty**：认 `#rrggbb` / `rrggbb` / X11 颜色名（与 CSS 名字有四处不同：green / gray / maroon / purple 取 X11 值，gray0–100 按公式生成）；`cell-foreground` / `cell-background` 特殊值；`palette` 0–255（16 以上进 `extended`，xterm 那边展开成整份 240 色）；`key =` 空值是恢复默认；无关键（`font-size`、`keybind`……）静默跳过，所以整份 `~/.config/ghostty/config` 贴进来也能用。缺省推导用**最终**的前后景：只写了 background / foreground 的主题，光标与选区跟新的字底走。`selection-*` 缺省是窗口前后景互换（Ghostty 文档原话），`selection-foreground = cell-foreground` 在 xterm 里就是不给 `selectionForeground`，ThemeColors 用 `null` 表示。
- **`theme = X` 以内置主题为底再覆盖**：与 Ghostty 读配置的顺序一致；`light:X,dark:Y` 取当前槽位那一半。自定义编辑器里默认填的就是 `theme = <当前主题名>`，用户只需在下面写要改的键。
- **界面色派生规则反推自 shadcn neutral**（`derive.ts`）：底色往字色掺 t（OKLab 里掺，`color.ts` 的转换按 Björn Ottosson 原文——**逆矩阵 g 行的第三个系数是 −0.3413193965**，抄错一位灰色会泛绿）。浅色 muted 0.035、深色 secondary 0.15、accent 0.27、card 0.07、popover 0.15……都是从 oklch(0.97) / (0.269) / (0.371) 这些值倒算出来的，Falcon 默认主题跑出来与旧 CSS 相差 < 0.012 L（测试钉住）。**深浅两套比例不对称是刻意的**：深底上 muted-foreground 离字色更近（0.33 vs 0.44），同样的 WCAG 对比度在黑底上看着更暗，shadcn 自己就是这样。浅色 primary 就是字色本身。
- **语义色（destructive / success / warning）与高亮色取自 ANSI**：先在普通色与亮色里挑对比度 ≥ 4.5 的（普通色优先），都不够就取更高的那个再往字色掺到 3:1；muted-foreground 保底 3.5:1（3024 Day 这类灰字主题按比例掺出来的 muted 淡到读不出）。主题自己的字色都不到 3:1（C64）时不强求——那是用户选的复古主题。`destructive-foreground` 在底 / 字里挑对比更高的，对照的是按钮实际填色：浅色是实心 `destructive`，深色是 `bg-destructive/60` 叠在底上（button / badge 的 `text-white` 因此改成 `text-destructive-foreground`）。只对实心粉红挑会在深色主题把深字铺到半透明暗红上。
- **`.dark` 按主题底色亮度切，不按明暗模式**：给浅色槽位选一套深底主题，界面就按深色算（color-scheme、Tailwind `dark:`、sonner、给 PTY 的 COLORFGBG 全跟着）。深浅判定与 `@falcon/shared` 的 `appearanceFromHex` 同一公式，index.html 内联脚本里又抄了一份——三处必须一致。
- **首帧**：index.html 的内联脚本读 `falcon.themes`（旧 key `falcon.theme` 只剩明暗模式，作迁移兜底），只写 `--background` / `--foreground` 与 `.dark`——那时 body 还是空的，别的看不见；整套 71 个 token 由 store 模块初始化时 `applyThemeToDom` 落，早于 React 首次渲染。
- **语法高亮改用 shiki 的 css-variables 主题**：token 的 style 是 `color: var(--shiki-token-keyword)`，值由 derive.ts 从 ANSI 派生（keyword ← 紫，string ← 绿，constant ← 黄，function ← 蓝，comment ← 亮黑掺到 3:1）。以前"明暗各装一套 github 主题、每个 token 带两色、CSS 按 `.dark` 取"的机制整个删掉；换主题不重新 tokenize 这一点保持。**单主题模式下 shiki 不给 `htmlStyle`**（那是多主题 + `defaultColor:false` 才有的），只给 `color` / `fontStyle` 位，`tokenStyle()` 自己拼成 React style——第一版漏了这一步，文件全是白字。
- **xterm 6 的 `.xterm-viewport` 底色**：主题底色只写在 `.xterm` 与 scrollable-element 的内联 style 上，viewport 仍是 xterm.css 的 `#000`，行数凑不满宿主高度时最后一行下面露一条黑边——深底主题从来看不出来，换成浅底主题第一眼就是它。`styles.css` 里让它跟 `--background`。
- **验证时的工具坑**：Chrome DevTools MCP 的 `fill` 直接写 textarea 的 value，会把 React 的 value tracker 一起改掉，之后再派发 input 事件 React 认为"没变"不触发 onChange；看起来像编辑器不响应粘贴，实际真人键入 / 粘贴都正常。要模拟输入用键盘 `type_text`。
- **终端配色对象引用稳定**：store 按槽位对象 `WeakMap` 缓存派生结果，`activeTheme.xterm` 同一套主题永远是同一个对象——rio 适配器按引用比对决定要不要重建渲染器。
- **选择器的实时预览走 store 的 `previewTheme`**（不是只改 DOM）：终端也要跟着换，否则预览是半截的。键盘连按时 120ms 节流——每次换主题 rio 都会重建渲染器、TerminalView 会给服务端发 appearance（服务端在深浅翻转时往 zellij 注 `997` 通知）。弹层关掉复原到真正的槽位主题。
- **选择器只列与槽位同明暗的主题**（按 catalog 的 appearance，即底色亮度）：`.dark` 按底色亮度切，深色槽位选进一套浅色主题整站就翻成浅色界面，"深色主题"这个设置名不副实。自定义编辑器不设限——贴进来的是什么就是什么，那是用户明确要的。
- **选择器的 Popover 要 `modal`**：它开在设置对话框里，Radix Dialog 的 react-remove-scroll 把 portal 到 body 的弹层当成"对话框外"，滚轮一律 preventDefault，列表滚不动（直接赋 scrollTop 能动，是事件被拦不是 CSS）。modal 让 Popover 自己再压一层滚动锁到栈顶，锁的范围就是弹层本身；点外关闭、点选、进编辑器都不受影响。
- **旧偏好迁移**：`falcon.theme`（明暗模式字符串）直接带过来；`falcon.term.themeId` 能对上 Ghostty 名字的（14 个）在首次启动异步拉目录后补进对应槽位，Campbell / Light+ 没有对应，落回 Falcon 默认。`sanitizeTermPref` 现在直接丢掉 themeId。
- **内置目录的产物格式**：每行 `名字 \t 22 个不带 # 的 rrggbb`，77KB / gz 31KB，独立 chunk 只在打开选择器时拉；`ghostty-themes.meta.ts` 单独给设置页显示条数与来源。`pnpm vendor-ghostty-themes` 优先读本机 `Ghostty.app/Contents/Resources/ghostty/themes`（就是用户那个 Ghostty 认的主题），没装才去 GitHub 稀疏克隆。

## 供应链与授权

- 内置主题数据来自 mbadolato/iTerm2-Color-Schemes 的 `ghostty/` 目录（MIT，`assets/themes/LICENSE.txt`），经 Ghostty 1.3.1 打包分发；只是颜色数据，不含代码。
- shiki 的 `createCssVariablesTheme` 是 `@shikijs/core` 自带的公开 API，不新增依赖。
- 没有引入颜色库：sRGB ↔ OKLab 转换 30 行自己写（`color.ts`），往返恒等有测试。

## 验证状态

已在 Chrome 桌面（macOS，vite dev）验证：默认主题下界面与改动前同款（Falcon Light / Dark 复现 shadcn neutral）；设置页两个槽位的选择器只列与槽位同明暗的主题（浅色 78 / 深色 387，合计 465 = Falcon 2 + Ghostty 463），弹层内滚轮可滚、搜索过滤、键盘高亮即整站预览（含设置对话框自身与侧栏）、回车选定后落盘（`falcon.themes` 深色槽位 = Catppuccin Mocha，含颜色副本）；自定义编辑器贴入用户的 noctis-lux（Ghostty 主题文件原文）实时预览、应用后进浅色槽位（kind = custom）；切明暗模式后 `.dark` / color-scheme / meta theme-color / 71 个 token 全部跟着换；终端（xterm）底 / 字 / ANSI 与文件查看的语法高亮都跟主题走；刷新页面后 index.html 内联脚本单独跑出的首帧底字与 React 落的一致。单测 522 个全绿（主题层 67 个），`tsc` 两个包干净，`vite build` 通过。

未验证：Safari / Firefox / 移动端；rio 引擎下高频预览的渲染器重建开销；真实 Ghostty 用户主题文件之外的奇怪写法（多行值、`config-file` 引用）。

## Consequences

- `styles.css` 里不再有两套颜色；新增语义色去 `derive.ts` 加一条派生规则并补测试，别回到 CSS 写死。
- 主题的深浅与明暗模式是两个概念（CONTEXT.md 已收录），代码里 `themeMode` 是槽位、`activeTheme.appearance` 是深浅，别混用。
- Ghostty 升级新增主题时跑一次 `pnpm vendor-ghostty-themes` 再提交产物；目录条数变了测试会提醒。
- 明确不做（可后补）：主题随会话 / 项目区分；自定义主题多套并存（现在一个槽位只存一套自定义）；从 Ghostty 配置文件路径直接读取（浏览器拿不到本机文件，只能贴）。
