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
- **POSIX 脚本走 `<shell> -l -c`**：登录 shell 才有用户 `~/.profile` / `~/.zshrc` 里的 PATH——`claude` 装在 `~/.local/bin`、`grok` 装在 `~/.grok/bin`、npm 全局装在 nvm 的 shim 目录，全靠它。
- **Windows 脚本（.cmd）的提示语只用 ASCII**：cmd 按 OEM 代码页读脚本文件，中文在默认 936 / 437 下必乱码。
- **会话默认名跟着 agent 走**（`Claude 2` / `Grok 3` / `Terminal 1`），前端仍可传 `name` 覆盖。
- **认不出的 `agent` 一律当普通终端**：宁可开出一个 shell，也不要 400 掉一个新会话。
- **前端三处共用一份菜单**（`useActions.newSessionItems`）：侧栏 checkout 行的 ＋、命令面板、窗口标题栏右键的「在右侧新建终端」。侧栏的会话行按 `session.agent` 换图标（Bot / SquareTerminal）。
- **未验证**：只在 macOS 本地宿主上真机验过（claude 2.1.270、grok 1.0.30 都能正常开出 TUI，codex 本机没装、走的是"找不到就提示并落回 shell"那条）。SSH 远端与 Windows 远端的脚本写入只有单测，没有真机跑过。
