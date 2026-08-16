# 持久会话由 Zellij 包装实现

会话需要在"后端与远端断连"和"后端进程重启"后存活。我们决定：所有宿主机（本地 = 后端所在机器，SSH = 远端机器）统一用 Zellij 包装 shell，**由宿主机自行下载**锁定版本的二进制到 `~/.mojito/bin/`，装不上的宿主降级为普通会话并在 UI 上标注**具体原因**。tmux 全面移除。

选 Zellij 的决定性理由是它在 0.44.0（2026-03-23）加入了原生 Windows 支持——tmux 和 zmx 都只有 Unix，而 mojito 需要覆盖本地 Windows 宿主。一个工具覆盖三种宿主，持久会话逻辑从"每平台一套外部依赖"收敛成一条代码路径。

## Considered Options

- **tmux**（本 ADR 的前一版决定）：成熟、宿主常已预装，但没有 Windows，本地 Windows 会话永远无法持久。在需要为 Windows 另找方案的前提下，"少传一个二进制"的收益不足以支撑长期维护两套 capture 语义、两套配置和两套探测逻辑。
- **zmx**：单二进制仅 2.9 MB（Zellij 是 13.9 MB），终端仿真保真度最好（libghostty-vt）。但同样没有 Windows，且 v0.7 明确警告"升级 IPC 会杀掉所有会话"——对一个承诺"断线命令不死"的产品是直接冲突。
- **自研 session host**（前一版已否决，这次重新审视后仍否决）：Windows 上让一个独立进程始终持有 ConPTY、后端只当 named pipe 客户端，技术上成立且能绕开 ConPTY 无法重新接管的限制。但有三项不随工作量消失的成本：① 用户失去"mojito 不可信时用 `zellij attach` 自救"的逃生通道，我们的 bug 会直接杀死用户的长跑任务且没有第二入口；② 要正确重建终端状态就得在服务端跑 VT 状态机（`@xterm/headless`），这正是"重写 tmux 核心"那部分；③ 平台边角（ConPTY resize、`CREATE_BREAKAWAY_FROM_JOB`、socket 权限）全部转移到我们头上。
- **mosh**：只解决网络漫游，不解决后端重启，被否。

## 实现要点

以下大半是在 Docker sshd 上实测踩出来的，文档里查不到。踩中任意一条，症状都是"会话建起来了但终端是废的"。

- **必须写一份 `config.kdl` 到我们独占的配置目录**。原本想让配置目录保持为空以实现"零配置文件"，但 `options --default-mode locked` 这类 CLI 参数**只在新建会话时生效**：attach 到已存在的会话时会被会话自身的模式状态覆盖，而最后一个客户端断开后模式退回 normal。症状是断线重连后终端像卡死一样不响应键盘（按键全被当快捷键吃掉），而新建会话完全正常——极难排查。配置文件里的 `default_mode` 每次客户端连接都会读。同时用 `keybinds clear-defaults=true` 做纵深防御。
- **layout 必须写成文件并用绝对路径引用**。`options --default-layout` 走 PathBuf 解析，只在 layout_dir 里找文件，够不到编译进二进制的内置 assets（引用 `no-plugins` 报 `IoError: The layout was not found`），而 `--layout-dir` 又不是顶层 flag。
- **必须探测远端登录 shell 并显式传 `--default-shell`**。SSH exec 是非登录非交互的，环境里没有 `$SHELL`，而实测 Zellij 在 `$SHELL` 为空时 pane 根本起不来（不是文档说的退到 `/bin/sh`），表现为会话建成了但屏幕全空。
- **整条启动命令要包进登录 shell（`sh -l -c '...'`）**。同样因为 SSH exec 非登录，`/etc/profile` 与 `~/.profile`、`~/.bash_profile` 一概不读——而 nvm、pyenv、cargo、Homebrew、Go、`~/.local/bin` 的 PATH 恰恰大多写在那里。不套这一层，用户会发现终端里"很多装好的命令找不到"，但自己 `ssh` 进去却好好的（`~/.bashrc` 里的那部分因为 shell 是交互式而侥幸生效，更显得没头绪）。套在最外层而非只包最终 shell，是因为 Zellij server 也由这条命令拉起，server 的环境会被它 spawn 的每个 shell 继承，一次修到底。**注意 server 是持久的**：改动只对新建的 Zellij server 生效，已存在的会话要终止后重建才能拿到新环境。
- **`--show-startup-tips false`**（连同 `--show-release-notes false`）。默认会先显示一屏 "Zellij Tip #N" 挡在 shell 前面等用户按键。
- **`dump-screen` 必须显式带 `--pane-id`**。不给的话抓的是"当前聚焦 pane"，而在没有客户端连接时——后端重启后接回，正是最需要它的时刻——焦点落在后台 plugin pane 上（`zellij:link` 等即使 layout 里没写也会加载），dump 出来只有一个 `\r\n`。所以 capture 要先 `list-panes` 找到 terminal pane 再抓。
- `--session-serialization false`：关掉复活序列化，否则被杀的会话会以 `(EXITED - attach to resurrect)` 留在 `zellij ls` 里污染存活判断。
- 历史重建用 `dump-screen --ansi --full`。它没有 `-S -2000` 那样的行数参数，靠 `--scroll-buffer-size 2000` 把 buffer 本身设小来限行。
- 会话名 `mj-<uuid 前 16 hex>`。截断是为了绕开 socket 路径长度上限（macOS 仅 104 字符）。
- Windows 远端所有命令走 `powershell -EncodedCommand`（UTF-16LE + base64）。OpenSSH for Windows 的默认 shell 由注册表 `DefaultShell` 决定，可能是 cmd / PowerShell 5.1 / 7，三者引号规则各异；base64 串里没有任何一层 shell 会改写的字符。脚本统一前置 `$ProgressPreference = 'SilentlyContinue'`，否则 `Add-Type` 之类会把进度流序列化成 CLIXML 写进 stderr，污染报给用户的错误详情。

## 供应链与授权

二进制**不做完整性校验**。走默认 GitHub 源时 HTTPS 已保证来源与完整性（同源的 `.sha256sum` 只防传输损坏，能换二进制的攻击者也能换它）。代价是：每台主机可配置的下载地址一旦指向不可信来源，控制该地址者即可在宿主机上执行任意代码——这一点在配置界面上有明确警告，是有意接受的权衡。

安装授权按主机记（host + port + username），不按项目：授权的实质是"允许 mojito 在这台机器上装东西"，与工作目录无关，同一台机器上的第二个项目不该再问一遍。表与 `known_hosts` 分开——后者表达"这台机器的身份可信"（键是 host+port，与登录用户无关），两件事的键不同。

## 验证状态

Linux 远端（Docker sshd，debian:12）上端到端验证通过：探测 → 远端 curl 下载 → tar 解压 → `--version` 握手 → 原子 rename → 建会话 → 命令执行 → `dump-screen` 历史重建（带 ANSI）→ 断链接回（历史与键盘均恢复）→ 终止清理。三条降级分支也各自验证给出正确原因：缺 curl/wget → `no-downloader`、家目录只读 → `dir-not-writable`、`noexec` 挂载 → `verify-failed`（下载解压均成功，卡在握手，正是该设计的目的）。

**未验证**：macOS 宿主、Windows 本地宿主的完整会话流程、Windows 远端全部路径（含断线验证）、架构不支持分支。

## Consequences

- ADR 的前一版（tmux）已被本文完整替换。会话恢复协议、能力探测、命名约定全部改写。
- Windows 本地宿主启用前会用 `IsProcessInJob` 实测：后端若处于 Job Object 中，Zellij server 会被连坐杀掉（PR #5195 未合并），此时诚实降级为非持久，而不是让用户以为会话持久。
- Windows 远端首次使用时做真实断线验证（建后台会话 → 断 SSH → 重连 → `zellij ls`），结果持久化到 `zellij_hosts`。
- 锁定 v0.44.3（发布于 2026-05-13）。此后合并的 Windows 修复只在 main、官方无发布时间表，这些问题我们暂时承担。
- 已知会带着走的缺陷：#5311（alternate-screen 程序 resize 后 `dump-screen` 带陈旧行，历史重建有视觉瑕疵）、#5052（pwsh 下 cwd 失效）、Windows 无 ARM64 产物（靠 x64 模拟，`--version` 握手裁决）。
- Zellij 未来若把 `CLIENT_SERVER_CONTRACT_VERSION` 提到 2，跨版本会话断裂会重演；二进制按版本命名、旧版不自动删，正是为此留的余地。
- mojito 只管理自己创建的会话（`mj-` 前缀），绝不接管用户自有的 Zellij 会话。
