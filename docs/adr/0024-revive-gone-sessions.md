# 宿主机上的会话没了：原地重开，agent 接着原来的对话

持久会话的 shell 挂在宿主机的 holder 下（ADR 0020），falcon 服务端重启、SSH 断线都伤不到它。holder 本身没了就救不回来：被人 `kill` 掉、OOM、宿主机重启。以前这种会话记 `dead(session-gone)`，留在侧栏等用户手动清，窗口里只剩一句"已丢失"。日常里这多半发生在一台机器上开着好几个 Claude Code 的时候：重启一次 Mac，就得挨个清掉死会话、重新开、再在每个里面 `claude --resume` 找回刚才那段对话。

现在：接回时判出 Gone，就用**同一个会话 id 原地重开**。claude / grok 开场的会话接着原来那段对话；其余会话（普通 shell、codex）开一个新的登录 shell。重开不成才跟以前一样 `dead(session-gone)`。

## 决定一：原地重开，会话 id 不变

不是"清掉旧记录、另建一个"：窗口（列式工作区按会话 id 记排布，ADR 0012）、对话终端（`assistant_threads.session_id`，ADR 0021）、客户端缓存的列表都指着这个 id，换 id 就得一处处去改指向，换不全就是一扇空窗口。原地重开之后它们什么都不用知道，客户端看到的只是 `unverified → active` 加一次回放，已有的处理就接得住。

守护进程那一侧本来就支持同名重开：spawn 先删掉上一个同名会话的退出记录，监听前删掉残留的 socket（`falcon-sessiond` 的 spawn.rs / ipc.rs）。

没回来的是屏幕与回滚：它们在 holder 的 VT 进程里，随 holder 一起没了。agent 的对话在 CLI 自己的存档里，所以能续上；普通 shell 里跑着的东西续不上。

## 决定二：只认 Gone

- `exited`（shell 自己退了：`exit`、Ctrl-D）是用户的意思，照旧 dead。
- 非持久会话本来就没有 holder，服务端一重启就没了（`backend-restart` / `link-lost`），不在这次范围里。
- 已经记成 `dead(session-gone)` 的旧行不动。

Gone 的证据照 ADR 0020 后记的口径：只认宿主机给的（本机没有退出记录、meta 里的 pid 不在了；SSH 上 bridge 的退出码），准备 / 部署失败一律留在 unverified。判错了也不会多出一个会话：spawn 先看同名 holder 活着没有，活着就直接接上它。

服务端开着的时候 holder 被杀，`handle_backend_exit` 不再当场记 dead，而是转 unverified、交给自动接回：接回那条路再判一次 Gone，然后在 `ensure_attached` 的单飞里重开。后端重启后的接回也走这条路，所以只有一份重开逻辑。

## 决定三：对话 id 就是会话 id

续接得知道 CLI 自己的对话 id。程序状态（OSC 7501）里没有这个字段，从 CLI 的存档目录里反查又要依赖它们的内部布局。claude 与 grok 都能在开场时**由我们指定** id（`--session-id <uuid>`），事后 `--resume <uuid>`。falcon 的会话 id 本来就是 UUID v4，一个会话对应一段对话，直接拿它当对话 id，不用另外存一列。

- 启动脚本还是每个 agent 一份（ADR 0013），id 与是否续接经环境变量 `FALCON_AGENT_SESSION` / `FALCON_AGENT_RESUME` 交给它（`agent::launch_env`），落回登录 shell 之前 unset。
- 重开时先 `--resume`，失败再按同一个 id 新开。两个 CLI 找不到对话时都以退出码 1 结束（2026-10 实测 claude 2.1.306、grok 1.0.62）。会失败的情形：开了没说话（对话还没落盘），或者是这个功能上线之前开的会话（当时没钉 id）。
- codex 开场没法指定 id（只有事后的 `codex resume <id>`），算不能续接。重开时退成普通 shell，`sessions.agent` 置空，侧栏图标跟着变。重开一个全新的 codex 不算"恢复"，不如给个 shell 让人自己决定。

## 已知的不足

- 在会话里 `/clear`、`/resume` 换到别的对话，或者退出 CLI 后手敲 `claude` 开了新对话：重开时续接的仍是开场那一段。要跟上得让 CLI 报出当前对话 id（hooks 或 OSC），这次没做。
- CLI 退出、落回 shell 之后 holder 才没的，重开时还是会把 CLI 拉起来、接着那段对话。
- 宿主机重启后服务端会一口气把所有持久会话都重开。原来开着几个 claude，就会同时起几个 `claude --resume`。
- 终端里没有"刚才重开过"的提示：普通 shell 是一个新的提示符，agent 是它自己续接后的界面。服务端日志有一行。
- Windows 宿主机上的续接分支只有单测（同 ADR 0013 的 .cmd 脚本），没在真机上跑过。
