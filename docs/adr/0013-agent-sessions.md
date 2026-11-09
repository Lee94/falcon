# Agent 会话：开场直接跑 claude / codex / grok

会话一直只有一种开法：起一个 shell，用户自己敲 `claude`。但这个产品的日常就是"在某个项目里开一个 Claude Code / Codex / Grok"，每次都要先等 shell 起来、再敲一次命令、还得记得当前目录对不对。

现在会话多一个可选的**开场 CLI**：新建时选「Claude Code / Codex / Grok」，开出来就是那个 CLI；选「普通终端」还是原来的 shell。CLI 退出后落回登录 shell，**会话不跟着结束**。

决定：

1. **CLI 不是"另一种会话"，只是换了个开场命令**。持久性、接回、Detach / Terminate、尺寸同步全部照旧，`sessions` 表只多一列 `agent`（null = 普通 shell），API 只多一个 `agent` 字段。
2. **靠一段写在宿主机上的启动脚本**（`<falcon 根>/agents/falcon-<agent>.sh|.cmd`），Zellij 的 `--default-shell` 指向它。脚本先跑 CLI，CLI 退出后 `exec` 真实登录 shell。
3. **CLI 不在 PATH 上不算错误**：提示一句然后照常落回 shell。把会话开起来比报错有用——用户可以就地 `npm i -g` 装完再开一个。
4. **每次附着都重写一遍脚本**（本地一次 fs 写、远端一次往返）：用户删了它、换了登录 shell、falcon 升级换了脚本内容，都能自愈。写不成就退回普通 shell——把一个不存在的路径当 shell 交给 Zellij，pane 根本起不来，用户只会看到一片空白。

## Considered Options

- **把 CLI 直接当 `--default-shell` 传下去**：`--default-shell` 是 PathBuf，不接受参数，而且 CLI 一退出就没人占着 pane 了，Zellij 会把 pane 连同会话一起收掉——用户按一次 Ctrl-D 整个会话就没了。
- **先起普通 shell，再往 PTY 里写 `claude\n`**：不用碰后端，但要猜 shell 什么时候就绪。慢机器、SSH 远端、装了慢 prompt 的 zsh 上，字符会打丢或打进上一条提示符。
- **脚本里读 `$SHELL` 当回落 shell**：Windows 远端那条路径上我们自己把 `SHELL` 设成了传给 Zellij 的 shell（ssh.ts 的 WMI 兜底），脚本再去读 `$SHELL` 就是递归调用自己。改成**生成脚本时把登录 shell 的绝对路径写死进去**。
- **把脚本塞进 Zellij 的安装流程（`ensureDirs`）**：那条路径只在 Zellij 就绪时才跑，非持久会话（Zellij 装不上、Windows Job Object）就没有脚本可用。改成独立一步，与 askpass 包装同一个模式。

## 实现要点

- **纯函数在 `sessions/agent.ts`**：可执行名、脚本文件名、脚本内容、远端写入命令，全部零 I/O 且有单测；落盘分两条（本地 `node:fs`，远端一条 `mkdir && printf && chmod` / PowerShell `Set-Content`），照 `askpass/install.ts` 的样子写。
- **POSIX 脚本走 `<shell> -i -l -c`**（三个 flag 分开写）：`-l` 读登录文件，`-i` 才读 `~/.zshrc`。纯 `zsh -l -c` 是 login 非交互，不读 `.zshrc`——PATH / nvm / `~/.local/bin` 写在那里的远端 Linux 上，表现为新建 Claude 会话变成「找不到 CLI、落回普通 shell」。macOS 本地往往靠 `/etc/zprofile` 的 path_helper 蒙对，所以本机验过不等于远端没问题。
- **Windows 脚本（.cmd）的提示语只用 ASCII**：cmd 按 OEM 代码页读脚本文件，中文在默认 936 / 437 下必乱码。
- **会话默认名跟着 agent 走**（`Claude 2` / `Grok 3` / `Terminal 1`），前端仍可传 `name` 覆盖。
- **认不出的 `agent` 一律当普通终端**：宁可开出一个 shell，也不要 400 掉一个新会话。
- **前端三处共用一份菜单**（`useActions.newSessionItems`）：侧栏 checkout 行的 ＋、命令面板、窗口标题栏右键的「在右侧新建终端」。侧栏的会话行按 `session.agent` 换图标（Bot / SquareTerminal）。
- **未验证**：Windows 远端的脚本写入只有单测，没有真机跑过。

## 后记（2026-10-21）

能续接的 CLI（claude / grok）开场时由 falcon 钉对话 id（就是会话 id），宿主机上的会话没了、原地重开时脚本用它 `--resume`。id 与模式经环境变量交给脚本，脚本仍是每个 agent 一份。见 ADR 0024。POSIX SSH 远端验过：`zsh -l -c` 找不到 `~/.local/bin/claude`（PATH 在 `.zshrc`），改 `-i -l -c` 后 `command -v claude` 与 `claude --version` 都成功。

## 后记（2026-11-04）：本地 CLI 自动检测

本地新建会话菜单只列出后端本机已安装的 CLI，侧栏、画布与命令面板共用检测结果。`GET /api/agents` 返回 `Vec<SessionAgent>`；首次结果返回前只显示普通终端。检测支持 Claude Code、Codex、Grok、Devin、Gemini CLI、OpenCode、Cursor Agent（`cursor-agent`）、GitHub Copilot（`copilot`）、Aider、Amp、Qwen Code（`qwen`）、Factory Droid（`droid`）、Antigravity CLI（`agy`）、Oh My Pi（`omp`）。新增工具沿用已有启动脚本，只有 claude / grok 具有按 Falcon 会话 id 续接的能力。

`sessions/agent_detect.rs` 重新解析交互登录 shell 的环境，在 PATH 中检查固定清单里的可执行文件；Unix 检查执行权限，Windows 按 PATHEXT 检查 `.exe` / `.cmd` 等文件。检测不运行 CLI，也不安装软件；符号链接跟随到目标，同名目录、损坏链接和不可执行文件不显示。服务端缓存 30 秒并合并并发探测，客户端登录后立即请求、之后每 30 秒刷新，安装器修改 shell 配置后也能重新发现。

本地与 SSH 项目均只显示目标机器实际检测到的 CLI。`GET /api/agents` 不带参数时检测后端本机，带 `hostId` 或 `projectId` 时经 SSH 在远端的交互登录 shell 中执行 `command -v`（Windows 使用 `Get-Command`），不启动 agent。同一主机的项目共用客户端清单并每 30 秒刷新；检测失败或尚未完成时不显示候选项，不返回完整清单。
