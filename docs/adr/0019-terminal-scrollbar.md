# 终端滚动条：位置从 zellij 里的插件问出来

终端的滚动一直发生在宿主机的 zellij 里：zellij attach 后客户端常驻备用屏、开着鼠标上报，滚轮变成 SGR 报文发给 zellij，由它滚自己的 scroll buffer 再整屏重画。前端（xterm / rio / 原生的 alacritty）本地没有 scrollback，自带的滚动条永远不出现。要画滚动条，位置只能问 zellij。

## 背景：CLI 问不到「视口下方还有多少」

逐条实测并读了 0.44.3 / 0.45.1 源码：

- `list-panes --json` 没有滚动字段，`cursor_coordinates_in_pane` 不随滚动变。
- `dump-screen` 不带 `--full` 给的是**当前滚到的那一屏**；带 `--full` 给的是视口**及以上**（`Grid::dump_screen` 的注释原话），视口下方的行不在里面。所以「顶上还有多少」拿得到，「总共多少」拿不到。行是逻辑行，折行的长行算一行。会话序列化同样不含视口下方。
- 直接跑 zellij 时看到的 `SCROLL: 0/35` 是面板边框标题画的 `Grid::scrollback_position_and_length()`——(视口下方的行数, 视口上方的显示行数 + 视口下方)。falcon 关了边框，这两个数就没有出口了。
- **0.45 新增插件事件 `ActivePaneScroll(Option<(position, length)>)`**，就是这两个数、变化时推送；插件 API 还能按 pane id 滚动。没有 CLI 口子，所以要写插件。

否掉的路：数滚轮估算（滚上去以后来了新输出就对不上，之前就否过）；`dump-screen` 比对（总长拿不到，而且每次要搬上万行）；前端拉一份历史在本地翻（交互变了：翻历史时看的是快照）。

## 决定一：一个 zellij 插件，按需一问一答

插件源码在 `packages/server/zellij-plugin`（Rust，`zellij-tile` 精确锁到与 zellij 相同的版本），产物 `packages/server/assets/falcon-scroll.wasm` **提交进仓库**——平时的构建、`build:bin`、CI 都直接用它，只有改插件或升 zellij 才要 `pnpm build:zellij-plugin`（要 rustup 管的工具链带 wasm32-wasip1，脚本头上写了装法与两个坑）。

插件订阅 `ActivePaneScroll`、手上永远是最新值；后端按需 `zellij pipe --name falcon-scroll -- "get <pane>"` / `"seek <pane> <position>"`，回一行 `<position> <length>`。seek 先按整页（pane 行数减一，同 zellij 的 page scroll）再逐行滚，等滚完后那次事件到了再回话，滚不动时由 150ms 定时器兜底。

- **随会话以后台插件加载**（会话配置里的 `load_plugins`），不靠 `zellij pipe --plugin` 现拉：现拉的是一个浮动 pane（只是浮动层恰好隐藏），实测那条管道还会挂住不退出，拉起它的第一条消息也会因为早于授权结果而落空。
- **查询按名字广播、不带 `--plugin`**：会话里没有这个插件时（老会话、插件没部署上）带 `--plugin` 会现场拉起一个浮动实例；广播时没人认领就直接放行，回话为空。
- **一问一答，不常驻**：每个会话的 PTY 已经占一条 SSH channel，sshd 默认 `MaxSessions 10`，再给每个会话挂一条常驻管道，能开的会话数就减半。查询本机 10–40ms、局域网 SSH 35–45ms，拖动时跟手。
- **stdin 必须给 EOF**：stdin 不是终端时，`zellij pipe` 收到放行后还要把 stdin 读到头才退出。SSH 走 `execWithInput(cmd, "")`，本机的 `localExec` 留着 stdin 管道不关，所以单独 spawn（`stdio: ignore`）。
- **后台插件的管道在 `pipe()` 返回后自动放行**，seek 要等滚完再回话就得显式 `block_cli_pipe_input`。

## 决定二：插件由后端推到宿主机，并预写授权

与 px0 同理不让宿主机自己下载。宿主机上固定路径 `<falcon 根>/zellij/plugins/falcon-scroll.wasm`，旁边 `.sha256` 对得上就不再推（`sessions/scrollPlugin.ts`）。升级时原子替换：zellij 的插件缓存只在会话进程内存里，新会话读到新文件，老会话继续用内存里的旧实例——所以插件协议只能向后兼容地改。

后台插件没有 pane，弹授权界面也没人能点，所以后端往 zellij 的 `permissions.kdl` 追加预授权条目。**键是插件路径本身，不带 `file:`**（`RunPluginLocation::File` 的 Display 就是路径，带前缀匹配不上）。文件位置跟 directories crate 走：Linux 在 `XDG_CACHE_HOME`（zellijEnv 已指进 falcon 自己的 cache 目录），macOS 写死在 `~/Library/Caches/org.Zellij-Contributors.Zellij/`、与用户自己的 zellij 共用，所以只追加不覆盖。

只做 POSIX 宿主机。Windows 远端没有测试机、推二进制要另走 base64 行协议，先不支持，那边的会话照旧没有滚动条。部署失败不挡开会话，新会话退回老配置而已。

## 决定三：新旧会话两套配置，建会话时定终身

`ActivePaneScroll` 只在边框样式为 titles（0.45 新增的默认值）且 tab 里只有一个 pane 时才发；`pane_frames false` 会把样式压成 None。单 pane 时 titles 样式不画标题行、滚动时也不画指示（实测），外观与关边框一致。

但**不能直接改 config.kdl**：升级前建的会话跑在 0.44.3 的 server 上，接回时客户端把配置带过去，0.44.3 不认识 titles、又没了 `pane_frames false`，实测给老会话画上整圈边框。所以：

- 新配置是单独的 `config/scroll.kdl`（`scrollConfigBody`），attach 时 `--config` 指过去，CLI 选项传 `--pane-frame-style titles`、**不传** `--pane-frames false`；
- 建会话时插件已就位才用新配置，记在 `sessions.scroll_plugin`，接回时照这个来；老会话照旧读 config.kdl；
- zellij 的配置热重载各管各的文件，falcon 每次 ensureDirs 重写两份文件也不会串（实测）。

## 决定四：滚动条是问了才知道的浮层

协议：客户端发 `{type:"scroll"}`（问）或 `{type:"scroll", seek}`（滚到视口下方还剩 seek 行处），服务端广播 `{type:"scroll", position, length, rows}`——大家看的是同一个 zellij 视口。会话不支持时没有回话，前端就不画。服务端单飞：一条在飞时后来的请求只留最后一个，seek 压过只问位置。

两个客户端（web `TerminalScrollbar.tsx`、原生 `terminal/view.rs`）同一套行为，几何纯函数在 web `lib/termScroll.ts` 与 `falcon-core/src/term_scroll.rs`：

- 平时不画；滚轮之后亮 1.2s，悬停、拖动时常亮；亮着期间每秒再问一次（输出还在涨）。
- 什么时候问：会话变成 active 时问一次（顺带知道支不支持）；看到自己发出去的滚轮报文后稍等 60ms 问（web 在 onData 里认 SGR 按键 64–95，xterm / rio / 触摸都走这里；原生在发滚轮报文处）；悬停时问。
- 拖滑块时滑块先跟手，seek 16ms 节流；松手补发落点，到下一次回话之前停在松手处。点轨道 = 滑块中心跳过去并可接着拖。
- 没有可滚的历史（含 vim 这类备用屏程序，插件回 `0 0`）时整条不画、不挡最右一列的点击。
- 单位是 zellij 的显示行，滑块总长 `length + rows`、上方 `length - position`。

## 顺带：升到 zellij 0.45.1

插件事件是 0.45 才有的。升级本身踩到一个必须处理的坑：0.45 默认 `scroll_mode_sync true`，滚轮会把会话隐式切进 Scroll 模式，而我们清空了全部键位——滚一下之后敲的字全被吞、视口不回底，像卡死。config.kdl 与 CLI 都加了 `scroll_mode_sync false`（0.44.3 读到这一行不报错）。契约版本仍是 1，0.45.1 客户端接回 0.44.3 的会话、对它跑 list-panes / list-clients / dump-screen 都实测正常。未验：Windows 远端。

## 已知限制

- 老会话（升级前建的、Windows 远端的）没有滚动条，新建会话才有。
- 位置靠问，空闲时滑块不会自己跟着输出长；只在亮着的那一会儿每秒刷新。
- 「视口下方」向下翻页偶有一行的出入（seek 结果以回话为准，不影响显示）。
