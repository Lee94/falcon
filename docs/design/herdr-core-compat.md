# 兼容 herdr-core 与「无项目」分组：详细设计

> 状态：**实施中**（2026-11-01）· 决定见 [ADR 0026](../adr/0026-herdr-core-compat.md)，本文给做法 · 守护进程底座见 [ADR 0020](../adr/0020-session-daemon-libghostty.md) 与 [`session-daemon.md`](./session-daemon.md)

---

## 0. 一句话

**在 `falcon-sessiond` 里内置一个每宿主机一份的 `hc` 协议网关（`falcon-sessiond hc serve` / `hc connect`），把 `<root>/sessiond/sessions/` 下的全部会话按 `herdr-core` 的 JSON-RPC 与 Framed 协议暴露给 `hc` CLI 和 `herdr-app`；同时在 falcon 侧为每台机器增加内置的「无项目」分组（`standalone:local` / `standalone:host:<id>`），既支持直接在机器上开独立终端，也自动收养从 `hc` / `herdr-app` 新建的会话。**

---

## 1. 整体架构与三条接入路径

```
┌────────────────────────────────────────────────────────────────────────────┐
│ 宿主机（本机 ~/.falcon 或 SSH 远端 ~/.falcon）                               │
│                                                                            │
│  holder (sid 1) ── <run>/0123...sock ◀──┬── falcon-server（本机或经 bridge） │
│  holder (sid 2) ── <run>/4567...sock ◀──┤                                  │
│  <sessions>/<sid>.{json,exit,labels.json,screen.txt}                       │
│                                         │                                  │
│  falcon-sessiond hc serve ──────────────┘                                  │
│    ├─ 监听 $XDG_STATE_HOME/herdr-core/falcon/falcon.sock                   │
│    ├─ 持有 $XDG_STATE_HOME/herdr-core/falcon/core.lock (flock LOCK_EX)     │
│    └─ 状态 <root>/sessiond/hc/{core.json, terminals.json, docs.json}       │
└───────────────────────────────▲────────────────────────────────────────────┘
                                │
        ┌───────────────────────┼───────────────────────┐
        │ 路径 A                │ 路径 B                │ 路径 C
        │ HC_SESSION=falcon     │ hc machine add falcon │ hc machine add box
        │ hc ls / attach / send │ --via 'falcon-sessiond│ --host user@box
        │                       │   hc connect --'      │ --via '.falcon/bin/
        │                       │                       │   falcon-sessiond hc connect --'
        `hc` CLI                本机 default hc          本机 default hc / herdr-app
```

- **路径 A（同机直连）**：`HC_SESSION=falcon hc ls`、`hc read t1`、`hc send t1 "…"`。
- **路径 B（挂入本机 `default` core / `herdr-app`）**：`hc machine add falcon --via "$HOME/.falcon/bin/falcon-sessiond hc connect --"`。本机 `herdr-app` 自动看到 `falcon/t1` 等会话及对应的 Space / Tab。
- **路径 C（SSH 远端直连）**：`hc machine add mybox --host user@mybox --via '.falcon/bin/falcon-sessiond hc connect --'`。

---

## 2. 宿主机文件与协议扩展（`falcon-proto::sessiond` & holder）

所有改动保持**只增不改**，新版网关与服务端都能兼容正在运行的旧 holder。

### 2.1 新增宿主机文件

```
<root>/bin/falcon-sessiond                  指向当前 falcon-sessiond-<版本>-<hash8> 的符号链接
<root>/sessiond/sessions/<sid>.labels.json  会话标签（BTreeMap<String, String>，原子覆盖写）
<root>/sessiond/sessions/<sid>.screen.txt   shell 退出前导出的最后 1000 行纯文本（供退出后 terminal.read）
<root>/sessiond/hc/core.json                网关的 core_id（UUID）与创建时间
<root>/sessiond/hc/terminals.json           sid ⇄ 短地址序号（u32）、自定义 name、keep、历史标签与元数据
<root>/sessiond/hc/docs.json                herdr-app 的文档存储（Map<name, {data, version, updated_at}>）
```

`.labels.json` 与 `.screen.txt` 随 `.exit` 一起在 `layout::entries` 里按 7 天过期清理。

### 2.2 holder 帧协议增量（`falcon-proto/src/sessiond.rs`）

1. **`Role::Hc => "hc"`** 与 **`Hello.output: Option<bool>`**（缺省 `None` 视为 `true`）：
   - 网关的后台监控连接以 `role: Role::Hc, output: Some(false)` 握手，只收 `Welcome`、`Status`、`Event`、`Exit`，不收 `Output` 帧，零带宽开销；
   - `terminal.attach` 接上来的 viewer 连接以 `output: Some(true)` 握手，独立走 `Sync` 与实时输出。
2. **`Sync.history: Option<u32>`**：
   - `None`：导出完整主屏历史（现有行为）；
   - `Some(0)`：只导当前屏幕，不含主屏历史回滚行；
   - `Some(n)`：最多保留最近 `n` 行历史。
3. **`ForegroundIs` 扩展字段**：
   - 增加 `#[serde(default)] pub pid: Option<u32>`、`pub argv: Option<Vec<String>>`、`pub cwd: Option<String>`；
   - macOS 用 `proc_pidinfo(PROC_PIDVNODEPATHINFO)` 取 cwd、`sysctl(KERN_PROCARGS2)` 取 argv；Linux 读 `/proc/<pgid>/cwd` 与 `/proc/<pgid>/cmdline`。
4. **`Event::Size { cols: u16, rows: u16 }`**：
   - holder 在 `apply_effective_size` 实际改变 PTY 尺寸时向所有已握手连接广播，供 `hc` 网关发出 `terminal.update`。
5. **`ClientMsg::Modes { req: u32 }`（`0x0c`）与 `HolderMsg::ModesAre`（`0x4c`）**：
   - holder 直接从自身的 `TermModeTracker` 返回 `{ req, bracketed_paste, app_cursor, alt_screen, mouse_tracking }`，供 `terminal.send`（判 `?2004h`）、`terminal.keys`（判 `?1h` DECCKM）与 `terminal.read`（判备用屏）即时查询。
6. **`SpawnSpec`、`SessionMeta`、`ListEntry` 增加 `labels`（及 `ListEntry` 增加 `cwd`、`argv`）**：
   - `spawn` 启动时若 `spec.labels` 非空，先写出 `<sid>.labels.json`；
   - `falcon-sessiond list --json` 返回每条会话的 `cwd`、`argv`、`labels`，供 `falcon-server` 自动发现并收养 `hc` 侧新建的会话。
7. **`falcon-sessiond label <SID> [k=v | k- ...]` 子命令**：
   - 原子读改写 `<sid>.labels.json`，供 `falcon-server` 在项目或会话改名时同步更新标签。

---

## 3. `falcon-sessiond hc` 网关设计

### 3.1 子命令与锁

- `falcon-sessiond hc serve [--session <name>]`：
  - 默认会话名：`root` 为 `~/.falcon` 时取 `falcon`（环境变量 `HC_SESSION` 非空且非 `default` 时优先用 `HC_SESSION`），否则取 `falcon-<fnv8(root)>`。
  - 状态目录：若设了 `XDG_RUNTIME_DIR`，锁在 `$XDG_STATE_HOME/herdr-core/<session>/core.lock`、socket 在 `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`；否则都在 `$XDG_STATE_HOME/herdr-core/<session>/`（缺省 `~/.local/state/herdr-core/<session>/`）。
  - 启动时打开 `core.lock` 并 `flock(LOCK_EX | LOCK_NB)`：
    - 若已被锁住：尝试连 socket 发 `server.status`，若是自己的 `falcon-sessiond` 网关则幂等退出 0；若是真 `herdr-core` 则报错退出 1 并提示解决命令。
    - 拿到锁后把 socket 路径写进 `core.lock`，删掉残留的旧 socket 文件并 `bind`。
- `falcon-sessiond hc connect [--session <name>] [--] [ignored...]`：
  - 吸收 `hc machine` 传过来的 `--` 及尾部参数；
  - 连不上 socket 时自动在后台拉起 `falcon-sessiond hc serve`（脱离控制终端），轮询等就绪（最多 5 秒），随后双向转发 `stdin/stdout ⇄ UnixStream`。
- `falcon-sessiond hc status [--json]`：
  - 探测网关是否在跑、socket 路径、会话数与 `hc` 二进制位置，供 `falcon-server` 状态接口调用。

### 3.2 双模式线协议（`hc/wire.rs`）

1. **Plain 模式（默认）**：按 `\n` 分割的 JSON-RPC 2.0，单行上限 8 MiB。
2. **Framed 模式**：客户端在 `hello` 带 `"framed": true`；网关先按 Plain 模式发回 `hello` 响应行并 flush，随后双向切入二进制帧：
   - 帧头 9 字节：`[channel: u32 LE][kind: u8][len: u32 LE][payload]`，单帧上限 16 MiB，数据分片 16 KiB。
   - Channel `0`：控制通道（`kind = 0` DATA，载荷是一条 JSON-RPC 消息，不含换行），优先级高于数据通道。
   - Channel `≥ 1`：终端 attach 通道：
     - `DATA (0)`：网关→客户端为 PTY 输出，客户端→网关为键盘输入；
     - `CREDIT (1)`：客户端→网关，`u32 LE` 增量窗口；初始窗口 256 KiB，落后超过 4 MiB 时丢弃旧数据、待信用恢复后重发一次首帧；
     - `CLOSE (2)`：网关→客户端载荷为 `{"reason":"exited"|"killed"|"holder_lost"|"detached","exit_code":...}`；客户端→网关为空载荷（detach）；
     - `RESIZE (3)`：客户端→网关，`[cols: u16 LE][rows: u16 LE]`；
     - `FOCUS (4)` / `FACTS (5)`：接收并忽略 `FACTS` 的调色板覆盖（保持 falcon 会话自身的主题色）。

### 3.3 终端地址与注册表（`hc/registry.rs`）

- 序号 `1, 2, 3...` 单调递增，编码为 `t` + 30 进制双射（字符集 `0123456789bcdfghjkmnpqrstvwxyz`，无元音与 `l`：`1 -> t1`，`9 -> t9`，`10 -> t10`，`20 -> t1b`，`280 -> t100`），持久化在 `<root>/sessiond/hc/terminals.json`。
- 支持按 `t<addr>`、完整 UUID、16 位 `<sid>` 或自定义 `name`（`[a-z0-9-_.]{1,64}`，且不与 `t<digits>` 冲突）解析目标终端。

### 3.4 状态映射、标签合成与事件流（`hc/mirror.rs`）

- **状态与 `turn`**：
  - 将 holder 的 `ProgramStatusRecord` 映射为 `hc` 的 `TerminalStatus`（`state`、`kind`、`progress`、`id`、`app`、`title`、`message`、`updated_at`）；
  - 当会话的活跃状态从 `working` / `blocked` 变为非活跃（`idle` / `done` / `error` / 清空 / 进程退出）时，`turn` 递增 1。
- **标签合成**：
  - 读取 `<sid>.labels.json`；
  - 若开启了 `herdr` 标签合成（缺省开启）且用户未手动覆盖对应键，根据 `falcon.project` / `falcon.project_name` / `falcon.session_title` 自动补齐：
    - `herdr.space = falcon.<project_id>`（无项目分组为 `falcon.standalone`）
    - `herdr.tab = falcon.<sid>`
    - `where = <project_name>`
- **`events.subscribe`**：
  - 维护单调递增 `seq: u64` 与容量 1024 的环形缓冲区；
  - 发出 `terminal.birth`、`terminal.update`、`terminal.exit`、`terminal.remove`、`doc.change`；
  - 每 15 秒无事件时发送 `{"event":"heartbeat","seq":...}`；
  - 有订阅者时每 1 秒对活跃 holder 发一次 `Foreground` 探测，驱动 `command` / `fg_pid` / `fg_argv` / `fg_cwd` 的 `terminal.update`。

### 3.5 首帧拼装与查询过滤（`hc/paint.rs` & `hc/queryfilter.rs`）

- **首帧（First Paint）**：
  1. `0x18`（CAN）取消半截序列 + 复位前导 `\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[>4;0m\x1b[<u\x1b[r\x1b[?1049l\x1b[2J\x1b[3J\x1b[H`；
  2. Holder `Sync { history }` 返回的 `snapshot`；
  3. 当前标题 `\x1b]2;<title>\x07` + `\x1b[?9001l`；
  4. 当前 `ProgramStatusRecord` 重编码回 `\x1b]7501;...\x07`；
  5. `continuation` 字节。
- **实时流查询过滤**：
  - 剥除已被 VT 进程回答过的终端查询（`CSI c` / `CSI > c` / `CSI = c`、`CSI 5n` / `CSI 6n` / `CSI ? 6n`、`CSI > q`、`CSI ? u`、`CSI ? $ p`、`CSI 14/16/18 t`、`OSC 4/10/11/12/52 ; ?`、`DCS $ q`、`DCS + q`、`ESC Z`），保留一切绘制序列与状态上报。

### 3.6 RPC 方法全集（一、二、三期）

- **一期（连接 / 查看 / 附着 / 文档）**：`hello`、`server.status`、`rpc.discover`、`terminal.list`、`terminal.get`、`terminal.read`、`terminal.attach`、`terminal.kill`、`terminal.rm`、`events.subscribe`、`doc.get`、`doc.set`。
- **二期（Agent 操控）**：
  - `terminal.send`：检查进程存活；非 `force` 时若处于 `blocked` 则拒绝（`-32009 blocked`），并等待输出静默满 3 秒（超时报 `-32008 timeout`）；查询 holder 的 `ModesAre.bracketed_paste`，开启时用 `\x1b[200~...\x1b[201~` 包裹文本（剥掉内部原有的粘贴起止标记）；`enter` 缺省为 `true`，粘贴后睡 200 ms 再发 `\r`。
  - `terminal.keys`：解析按键序列（`Enter`、`Tab`、`BTab`、`Escape`、`BSpace`、`Space`、`Up/Down/Right/Left` 按 `ModesAre.app_cursor` 切换 `CSI A` 与 `SS3 A`、`Home/End/PgUp/PgDn/IC/DC`、`F1–F12`、`C-x`、`M-x`、`^X`）。
  - `terminal.interrupt`：发送 `\x03`（SIGINT）。
  - `terminal.wait`：支持等待任意状态变化、等待指定 `status`、等待 `turn` 推进、等待 `exit`，带 `timeout`（毫秒，缺省 300_000 ms）。
  - `terminal.label`：支持数组 `["k=v", "k-"]` 或对象 `{"set":{...},"remove":[...]}`，更新 `<sid>.labels.json` 并广播 `terminal.update`。
  - `terminal.keep`：设置 `keep: bool`，`keep=true` 的会话在退出后保留在 `terminal.list` 直至 `terminal.rm`。
- **三期（新建会话）**：
  - `terminal.run` / `terminal.put`：调用 `spawn::spawn` 拉起新 holder，`owner` 设为 `"hc:<core_id>"`，`command` 缺省用宿主机的默认登录 shell（`hello.features` 含 `"shell"`），写入初始标签并返回 `TerminalInfo`。
  - `templates.list`：返回 `{"templates": []}`。

---

## 4. falcon 侧「无项目」分组与外部会话收养

### 4.1 合成项目 ID：`standalone:local` 与 `standalone:host:<hostId>`

在 [`sessions/machine.rs`](../../native/crates/falcon-server/src/sessions/machine.rs) 与 [`falcon-core/src/project_tree.rs`](../../native/crates/falcon-core/src/project_tree.rs) 中定义「无项目」分组的合成 ID：

- 本机：`standalone:local`
- 已保存的 SSH 主机：`standalone:host:<hostId>`

与 AI 助手专用的 `machine:local` / `machine:host:<hostId>` 的区别：

| 特性 | `machine:*`（AI 助手线程） | `standalone:*`（无项目分组） |
|---|---|---|
| 在 `GET /api/sessions` 中返回 | 否（被过滤） | **是**（出现在侧栏、总览、画布中） |
| `owner_row(db, id)` 解析 | 是（家目录） | **是**（家目录） |
| `POST /api/projects/{id}/sessions` | 仅内部调用 | **支持**（前端直接调同一接口新建会话） |
| `/api/projects/{id}/files/*` | 支持 | **支持**（文件面板浏览机器家目录 `~`） |
| `/api/projects/{id}/git/*` | — | 直接返回 `NoWorkingDir`（不扫家目录 Git） |

### 4.2 客户端侧栏与工作区表现

1. **`falcon-core::project_tree`**：
   - 每个 `ServerGroup`（`ServerKind::Local` 与 `ServerKind::Host(host)`）带一个 `standalone_id: Option<String>`（`standalone:local` 或 `standalone:host:<host.id>`）。
   - `standalone` 会话不进入 `folders`，而是挂在所属 `ServerGroup.standalone_sessions` 下。
2. **`falcon-core::workspace`**：
   - `select_project` 与 `apply_projects` 允许选中 `is_standalone_project_id(id)`，即便它不在 `projects` 表中。
3. **`falcon-ui::sidebar`**：
   - 在每台机器（`server_node`）展开时，渲染一行「无项目」分组节点（`standalone_node`，深度 1，图标 `SquareTerminal`，文案 `t!("sidebar.standalone")`）：
     - 行尾带 `+` 按钮（左键新建终端，右键弹出「新建终端 / Claude Code / Codex / …」菜单）与会话计数徽章；
     - 展开后直接渲染深度 2 的 `session_row` 列表，无需多余的 Git 分支层；
     - 机器行本身的右键菜单也提供「新建无项目终端」。
4. **画布、总览与命令面板**：
   - 画布窗格头对 `standalone:host:<id>` 显示该 SSH 主机的色彩条（`host_bar_from_ssh`）与 `user@host:port` 位置标签；对 `standalone:local` 显示 `本机 · <platform>`。

### 4.3 自动收养 `hc` / `herdr-app` 新建的会话

当本机或 SSH 宿主机上列出会话（`local_list_sessions` / `SshLink::list_sessions`）时：

1. 遍历 `alive == true`（或刚退出）且 `owner.starts_with("hc:")` 的条目；
2. 若 `db.get_session(&entry.session_id)` 不存在（且 `entry.session_id` 是合法 UUID，或由 `sid` 派生确定性 UUID）：
   - 检查 `entry.labels.get("falcon.project")` 是否指向该机器上已存在的项目；
   - 若是则归入该项目，否则归入该机器的 `standalone:local` 或 `standalone:host:<hostId>`；
   - 调用 `db.create_session` 入库并加入 `SessionManager` 内存表，前端通过 WebSocket 实时看到新会话。
3. 孤儿清理（`manager.rs::orphans`）仍只清理 `entry.owner == instance_id` 的会话，绝不误杀 `hc:*` 会话。
