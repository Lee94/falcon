# Mojito

Web 端持久化终端工作台：以项目为单位管理终端会话，**会话不因网页关闭而终止**；支持本地文件夹项目与远程 SSH 项目。

术语与领域模型见 [CONTEXT.md](./CONTEXT.md)，核心架构决策见 [docs/adr/](./docs/adr/)。

## 能力承诺（分层）

| 场景 | 保证 |
| --- | --- |
| 关闭 / 刷新浏览器页面 | 所有会话继续运行（硬保证），重开时回放历史输出 |
| SSH 链路断开 | 持久会话中的命令继续运行，自动指数退避重连并接回 |
| 后端进程重启 | 持久会话可懒惰接回（`dump-screen` 重建历史）；非持久会话标记为已丢失 |

**持久会话 = Zellij 包装**。宿主机（本地 = 后端所在机器，SSH = 远端机器）上没有 Zellij 时，由**宿主机自行下载**锁定版本的二进制到 `~/.mojito/bin/`——不经后端中转，不走 SFTP。装不上则降级为普通会话，并在 UI 上标注具体原因（缺 curl、`noexec` 挂载、架构不支持等），而不是笼统的"非持久"。

SSH 连接可以在设置里预先保存成远端主机，新建 SSH 项目时直接选择，不用每次重填主机、端口和凭据。

SSH 项目的右侧面板可以加端口转发：本地转发把远端服务映射到本机端口，远端转发把本机能到达的地址暴露给远端。规则跟着项目走，隧道走同一条 SSH 链路；没有打开的终端也可以保持转发。启用中的转发在 SSH 断线后会跟着自动重连。

终端里可以直接粘贴截图或拖入图片文件——包括 SSH 远端的会话。图片会写到会话宿主机的 `<mojito 根>/paste/`（远端为 `~/.mojito/paste/`），终端输入框里出现的是它的落盘路径；Claude Code 等 TUI 认输入框里的图片路径，效果等同把文件拖进原生终端。剪贴板同时有文本和位图时贴文本（Excel / 网页复制的常态），旧图 24 小时后自动清理。

首次在某台远端主机上创建会话时会询问一次授权——这会往你的服务器写入可执行文件。授权按主机记（host + port + username），同一台机器上的后续项目不再询问。

装的过程会失败——远端网络抖动、镜像抽风、SSH 通道半路断开都很常见。因此：探测 / 下载 / 解压这类瞬时故障会**自动重试 3 次**（退避 1.5s、4s，进度条上会说明"正在第几次重试"）；下载带连接与停滞超时，连上却一个字节不来的黑洞路由不会让进度条转到天荒地老；三次都失败则给出远端的原始报错（如 `curl: (28) ...`）和一个**重试**按钮。跑不起来（`noexec`、架构不符）、缺 curl、缺 tar 这类稳定的环境事实不做自动重试——先去改环境，改完再点重试。

### 已知限制

- **内网 / 无出网的远端**：宿主机下不到二进制时，手动把 [Zellij](https://github.com/zellij-org/zellij/releases) 的 `no-web` 二进制放到远端 `~/.mojito/bin/zellij-<版本>` 即可（版本号见安装失败提示）。
- **Windows 本地宿主**：若 mojito 后端进程处于 Job Object 中（被某些进程管理器或 IDE 拉起时常见），Zellij server 会随后端退出被杀。mojito 启动时会实测并诚实降级为非持久，而不是让你以为会话持久。
- **Windows 远端**：Zellij 的 server 进程无法脱离 Win32-OpenSSH 的 Job Object（上游未修复），普通方式拉起的会话活不过 SSH 通道关闭。mojito 改经 WMI（`Win32_Process.Create`）把 server 生到 sshd 进程树之外，因此依赖宿主机允许本用户做 WMI 进程创建（默认允许）。首次使用时仍会做一次真实断线验证，验证不通过则标注为非持久。
- Windows 无官方 ARM64 构建，靠 x64 模拟运行，能否跑起来由安装时的 `--version` 握手裁决。
- **派生超大仓库**：`git worktree add` 要物化整个工作区，几 GB 的仓库可能要几十秒。派生走的是普通 REST 请求（没有分阶段进度可报，加一套 WebSocket 只会多一层协议），放在反向代理后面时请确认 idle timeout 够长。
- **Windows 上的派生路径长度**：目标路径超过 200 字符会被拒绝创建。建得出来却因为 `MAX_PATH` 删不掉，比一开始就说不行更糟。

## 附属项目（git worktree）

项目工作目录在一个 git 仓库里时，项目菜单会多出「派生附属项目…」：选一条分支（新建或检出已有），mojito 跑一次 `git worktree add`，在**仓库根的同级**建出目录并落一条新项目。

```
D:\code\
├── mojito\            ← 源项目
├── mojito-feat-x\     ← 附属项目（分支 feat/x）
└── mojito-hotfix\     ← 附属项目（分支 hotfix）
```

附属项目在侧栏缩进显示在源项目下面，有自己的名字、自己的会话、自己的生命周期，与源项目共用同一台宿主机和同一份连接配置（SSH 项目也能派生，命令在远端跑）。术语定义见 [CONTEXT.md](./CONTEXT.md)。

**删除附属项目会连目录一起删**——这是 mojito 唯一会删除用户目录的地方，所以：

- 删除前会读一次工作区状态，把未提交的改动、**以及被 `.gitignore` 忽略的文件**（`.env`、本地数据库这些通常是全世界唯一一份）摆在确认框里；读不到状态就明说"无法确认"，绝不当成"是干净的"。
- 动手前要同时满足一串断言：路径是 mojito 自己记下的、目录确实是 mojito 建的、不是仓库根 / 家目录 / 盘符根、深度足够、没踩到别的项目的工作目录，且它此刻**仍然是那个仓库的 worktree**（主仓库丢失时退而检查 worktree 独有的 `.git` 文件）。任何一条不成立就不删，把路径告诉你。
- 被 `git worktree lock` 锁住的 worktree 一律不碰，也绝不自动解锁。
- 分支永远保留，`git worktree prune` 只在走了兜底删除时才跑。
- 删源项目会级联删掉它的全部附属项目（确认框逐条列出路径），但**源项目自己的目录不动**——那不是 mojito 建的。

## 快速开始

```bash
pnpm install
pnpm build
node packages/server/dist/index.js
```

打开 http://localhost:4923 。

### 启动参数

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `--port` | `4923` | 监听端口 |
| `--host` | `127.0.0.1` | 绑定地址；绑非 localhost 前必须先设置访问密码 |
| `--data-dir` | `~/.mojito` | 数据目录（SQLite、加密密钥、本地 Zellij 二进制） |

对应环境变量：`MOJITO_PORT` / `MOJITO_HOST` / `MOJITO_DATA_DIR`。

### 开发模式

```bash
pnpm dev:server
```

```bash
pnpm dev:web
```

前端开发服务器在 5173 端口，`/api` 与 `/ws` 代理到 4923。

## 单文件发布

```bash
pnpm build:bin                              # 打当前平台
pnpm build:bin --target linux-x64,linux-arm64   # 交叉打包（或 --target all）
```

产物在 `release/mojito-v<版本>-<平台>`，单个可执行文件，不依赖已安装的 Node：

```bash
./mojito-v0.1.0-linux-x64 --port 8080
```

原理：esbuild 把 server 打成单个 bundle，与 node-pty 原生扩展、web 静态资源一起
封进 Node SEA（Single Executable Application）blob，注入 nodejs.org 官方 Node
二进制。首次运行把原生扩展与静态资源解压到 `<dataDir>/runtime/<内容哈希>/`，
版本升级后旧目录自动清理。

注意事项：

- 体积约 150–170 MB，Node 运行时占大头，是所有 Node 单文件方案的固有成本。
- 构建机需能访问 nodejs.org 与 registry.npmjs.org（下载缓存在 `build/cache/`）。
- macOS 产物必须在 macOS 上构建（注入后要重新 ad-hoc 签名，`codesign` 只有 macOS 有）；
  Linux 产物在任意平台都能构建。
- 不支持 Windows 目标：服务本身依赖 Zellij 与 POSIX shell。
- Homebrew 等发行版的 Node 编译时可能禁用了 SEA，所以生成 blob 也用下载的官方 Node，
  本机 Node 版本不影响产物。

### 常驻运行（守护）

```bash
./mojito-v0.1.0-linux-x64 service install --host 0.0.0.0 --port 4923
./mojito-v0.1.0-linux-x64 service status     # 也有 start / stop / uninstall
```

`service install` 把 mojito 注册成系统服务：macOS 用 launchd（用户级 LaunchAgent），
Linux 用 systemd（root 装 system 级，普通用户装 user 级）。崩溃自动拉起、开机自启，
启动参数在 install 时指定并原样写进服务配置。单文件发布时会先把当前二进制拷到
`<dataDir>/bin/mojito`，服务永远跑这个固定路径——活动监视器 / `ps` 里进程名是
`mojito`，不是带版本号的下载文件名。

- 日志：Linux 看 `journalctl -u mojito -f`（user 级加 `--user`）；macOS 在
  `<dataDir>/logs/mojito.log`。
- Linux 普通用户安装后需执行一次 `sudo loginctl enable-linger <用户名>`，
  否则登出即停、开机不起。
- 绑非 localhost 前先在 localhost 启动并设好访问密码（数据目录一致），
  否则服务会反复启动失败。
- 没有 systemd 的环境（容器等）：`service install` 会打印 unit 内容，
  自行接到所用的 init；容器里通常直接前台跑，交给容器编排重启即可。

### 更新

两种方式任选：

```bash
# 方式一：用新二进制重跑 install，覆盖 <dataDir>/bin/mojito 并重启
./mojito-v0.2.0-linux-x64 service install --host 0.0.0.0 --port 4923

# 方式二：手动替换后再 restart（必须 mv，不要 cp）
mv mojito.new ~/.mojito/bin/mojito && ~/.mojito/bin/mojito service restart
```

替换文件必须用 `mv`（先传到同盘临时名再改名），**不要 `cp` 原地覆盖**：
macOS 上覆盖正在运行的二进制会令代码签名失效、相关进程被内核直接 SIGKILL，
Linux 上则会报 ETXTBSY。`mv` 换的是目录项与 inode，两个平台都安全。

数据无需干预：DB 迁移在启动时自动执行；`runtime/<哈希>` 目录按新版本重新解压，
旧目录自动清理。回滚就是用旧二进制重跑 `service install`——但若新版本做过
DB 迁移，旧代码不一定认识新库，降级前先备份数据目录里的 `mojito.db`。

## 安全说明

- 访问密码：单一密码门禁（scrypt 哈希）。绑定 localhost 可免密；绑定 `0.0.0.0` 强制要求已设密码，否则拒绝启动。
- SSH 凭据：密码 / passphrase 以 AES-256-GCM 加密落盘，密钥在数据目录 `secret.key` —— 防数据库文件单独泄露，不防整机被攻破。
- Host key：TOFU（首次连接记录指纹，之后指纹变化告警拒连）。
- TLS 不内置，对外部署请置于反向代理之后。
- **Zellij 二进制不做完整性校验**：走默认 GitHub 源时依赖 HTTPS 保证来源。若你为某台主机配置了自定义下载地址，控制该地址者即可在该主机上执行任意代码——请只填可信来源。

## 从远端卸载

mojito 在远端只写 `~/.mojito/`（二进制、Zellij 的 socket / config / data / cache），删掉即可完全卸载。删除项目不会自动清理远端——那台机器上可能还有你想留着的会话。

**唯一的例外是附属项目**：它的工作目录是 mojito 自己 `git worktree add` 出来的，删除时会跑 `git worktree remove --force` 并清理该目录。清理失败（被 lock、被占用、权限不足）时会把残留路径原样告诉你，项目记录照删——留一条删不掉的项目行，你唯一的出路是去改 SQLite；残留目录你自己删得掉。源项目自己的目录、`~/.mojito/` 一概不动，**分支也永远不删**（未推送的提交只有那一份）。

例外：macOS 远端上 Zellij 的 cache 路径由其依赖库硬编码，无法重定向，会残留 `~/Library/Caches/org.Zellij-Contributors.Zellij`。Linux 远端无此问题。

## 升级 Zellij 版本

```bash
pnpm update-zellij 0.44.4
```

脚本会校验该 tag 下五个 target 的 `no-web` 产物齐全，然后更新锁定版本。升级前请确认 Zellij 的 `CLIENT_SERVER_CONTRACT_VERSION` 未变——它一变，宿主机上已有的会话就接不回来了。

## 工程结构

```
packages/
├── shared/   共享类型（Project、Session、WS 协议）
├── server/   Fastify + @lydell/node-pty + ssh2 + better-sqlite3
└── web/      Vite + React 19 + Tailwind v4 + shadcn/ui + xterm.js + i18next（中文）
```
