# Falcon

Web 端持久化终端工作台：以项目为单位管理终端会话，**会话不因网页关闭而终止**；支持本地文件夹项目与远程 SSH 项目。

术语与领域模型见 [CONTEXT.md](./CONTEXT.md)，核心架构决策见 [docs/adr/](./docs/adr/)。

## 能力承诺（分层）

| 场景 | 保证 |
| --- | --- |
| 关闭 / 刷新浏览器页面 | 所有会话继续运行（硬保证），重开时回放历史输出 |
| SSH 链路断开 | 持久会话中的命令继续运行，自动指数退避重连并接回 |
| 后端进程重启 | 持久会话自动接回，历史从宿主机上的终端状态完整重建；非持久会话标记为已丢失 |

**持久会话 = falcon 自己的会话守护进程**（falcon-sessiond）。宿主机（本地 = 后端所在机器，SSH = 远端机器）上的每个会话由一对进程持有：holder 持 PTY、VT 进程（libghostty-vt）维护终端状态、回答终端查询、导出接回用的快照。守护进程由**后端推到宿主机**的 `~/.falcon/bin/`（文件名带内容哈希，推完握手校验），宿主机不需要能出网、不需要 curl / tar。推不上或跑不起来则降级为普通会话，并在 UI 上标注具体原因（目录不可写、`noexec` 挂载、架构不支持等），而不是笼统的"非持久"。

程序可以用 OSC 7501（Program Status Protocol）报告自己在工作、等授权、完成还是出错——Claude Code 就会这么做。falcon 在侧栏和窗口标题栏上显示状态徽标，会话卡在等授权 / 等回答时弹一次提醒，没人在看的会话也一样。

ssh 到宿主机上也能直接接回自己的会话：`~/.falcon/bin/falcon-sessiond-<版本>-<哈希> list` 看有哪些，`… attach <SID>` 接上（Ctrl+\ 断开，会话照常运行）。

SSH 连接可以在设置里预先保存成远端主机，新建 SSH 项目时直接选择，不用每次重填主机、端口和凭据。

SSH 项目的右侧面板可以加端口转发：本地转发把远端服务映射到本机端口，远端转发把本机能到达的地址暴露给远端。规则跟着项目走，隧道走同一条 SSH 链路；没有打开的终端也可以保持转发。启用中的转发在 SSH 断线后会跟着自动重连。

同一块面板还能把 HTTP 服务发布到公网：走 Cloudflare Quick Tunnel，无需 Cloudflare 账号，得到一条临时的 `*.trycloudflare.com` 地址。本地项目直接打本机端口；SSH 项目可选打本机或远端（远端先经 SSH 接到后端）。`cloudflared` 只在 falcon 后端本机跑，第一次发布时按锁定版本下载到 `~/.falcon/bin/`。地址随进程重启而变；任何拿到链接的人都能访问。

右侧栏另有「飞书项目」面板：看当前用户在飞书项目（Meegle）里的待办 / 本周 / 逾期 / 已办，选空间按关键字搜视图与工作项，粘贴飞书项目链接直接打开视图、全景视图或工作项，常用的可以固定在面板里（存服务端，换浏览器还在），点开看详情、一键跳去飞书。数据来自 [meegle CLI](https://github.com/larksuite/meegle-cli)，它随服务内置（`@lark-project/meegle` 是锁定版本依赖，单文件发布时本平台的二进制编进服务端），不用另外安装；没登录时面板给出 device-code 登录链接，在浏览器里授权即可。

终端里可以直接粘贴截图或拖入图片文件——包括 SSH 远端的会话。图片会写到会话宿主机的 `<falcon 根>/paste/`（远端为 `~/.falcon/paste/`），终端输入框里出现的是它的落盘路径；Claude Code 等 TUI 认输入框里的图片路径，效果等同把文件拖进原生终端。剪贴板同时有文本和位图时贴文本（Excel / 网页复制的常态），旧图 24 小时后自动清理。

首次在某台远端主机上创建会话时会询问一次授权——这会往你的服务器写入可执行文件。授权按主机记（host + port + username），同一台机器上的后续项目不再询问。

推送会失败——SSH 通道半路断开很常见。因此：探测 / 上传这类瞬时故障会**自动重试 3 次**（退避 1.5s、4s，进度条上会说明"正在第几次重试"）；三次都失败则给出远端的原始报错和一个**重试**按钮。跑不起来（`noexec`、架构不符）、目录不可写这类稳定的环境事实不做自动重试——先去改环境，改完再点重试。

**升级 falcon 时**：新版本推上去的守护进程按内容另起文件名，旧的不覆盖，已有会话照常运行、照常接回；只有 VT 进程会被热换成新版本（libghostty-vt 的二进制快照交接，换不了就用原始输出重建）。

### 已知限制

- **Windows 宿主**：守护进程先用 `CREATE_BREAKAWAY_FROM_JOB` 脱离调用方所在的 Job Object（sshd 的会话、被进程管理器拉起的后端都在 Job 里，Job 关闭时整棵进程树陪葬）；Job 不允许脱离时改经 WMI（`Win32_Process.Create`）拉起，依赖宿主机允许本用户做 WMI 进程创建（默认允许）。远端首次使用时会做一次真实断线验证，验证不通过则标注为非持久。**这条路径还没在 Windows 真机上验证过**。
- **Windows 上没有程序状态**：ConPTY 会吞掉它不认识的 OSC，OSC 7501 到不了守护进程。
- Windows ARM64 跑 x64 的构建，靠系统的 x64 模拟，能否跑起来由部署时的 `version` 握手裁决。
- **派生超大仓库**：`git worktree add` 要物化整个工作区，几 GB 的仓库可能要几十秒。派生走的是普通 REST 请求（没有分阶段进度可报，加一套 WebSocket 只会多一层协议），放在反向代理后面时请确认 idle timeout 够长。
- **Windows 上的派生路径长度**：目标路径超过 200 字符会被拒绝创建。建得出来却因为 `MAX_PATH` 删不掉，比一开始就说不行更糟。

## 附属项目（git worktree）

项目工作目录在一个 git 仓库里时，项目菜单会多出「派生附属项目…」：选一条分支（新建或检出已有），falcon 跑一次 `git worktree add`，在**仓库根的同级**建出目录并落一条新项目。

```
D:\code\
├── falcon\            ← 源项目
├── falcon-feat-x\     ← 附属项目（分支 feat/x）
└── falcon-hotfix\     ← 附属项目（分支 hotfix）
```

附属项目在侧栏缩进显示在源项目下面，有自己的名字、自己的会话、自己的生命周期，与源项目共用同一台宿主机和同一份连接配置（SSH 项目也能派生，命令在远端跑）。术语定义见 [CONTEXT.md](./CONTEXT.md)。

**删除附属项目会连目录一起删**——这是 falcon 唯一会删除用户目录的地方，所以：

- 删除前会读一次工作区状态，把未提交的改动、**以及被 `.gitignore` 忽略的文件**（`.env`、本地数据库这些通常是全世界唯一一份）摆在确认框里；读不到状态就明说"无法确认"，绝不当成"是干净的"。
- 动手前要同时满足一串断言：路径是 falcon 自己记下的、目录确实是 falcon 建的、不是仓库根 / 家目录 / 盘符根、深度足够、没踩到别的项目的工作目录，且它此刻**仍然是那个仓库的 worktree**（主仓库丢失时退而检查 worktree 独有的 `.git` 文件）。任何一条不成立就不删，把路径告诉你。
- 被 `git worktree lock` 锁住的 worktree 一律不碰，也绝不自动解锁。
- 分支永远保留，`git worktree prune` 只在走了兜底删除时才跑。
- 删源项目会级联删掉它的全部附属项目（确认框逐条列出路径），但**源项目自己的目录不动**——那不是 falcon 建的。

## 快速开始

服务端与客户端都是 Rust（`native/`，一个 Cargo workspace）：服务端 `falcon-server`，客户端一套界面（`falcon-ui`）出原生桌面（`falcon-desktop`）与浏览器（wasm，`falcon-web`）两种产物。从源码跑只需要 Rust 工具链（浏览器版另要 `rustup target add wasm32-unknown-unknown` 与同版本的 `wasm-bindgen-cli`，见 `native/scripts/build-web.sh`）。构建 / 打包任务都是 `cargo xtask <命令>`（`native/xtask`，`cargo xtask --help` 列全部）：

```bash
cd native
cargo xtask web                       # 浏览器版客户端，产物 native/target-wasm/dist
cargo run --release -p falcon-server  # 服务端，托管上面那份产物
cargo run -p falcon-desktop           # 或者直接开原生客户端
```

打开 http://localhost:4923 。注意不带参数时用的是默认数据目录 `~/.falcon`——和装好的服务同一个，试跑另起一个时带上 `--port` / `--data-dir`（`cd native && cargo run -p falcon-server -- --port 4950 --data-dir /tmp/fal`）。

### 启动参数

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `--port` | `4923` | 监听端口 |
| `--host` | `127.0.0.1` | 绑定地址；绑非 localhost 前必须先设置访问密码 |
| `--data-dir` | `~/.falcon` | 数据目录（SQLite、加密密钥、本机的会话守护进程与它的会话） |

对应环境变量：`FALCON_PORT` / `FALCON_HOST` / `FALCON_DATA_DIR`。旧名 `MOJITO_*` 仍可作回退。

本机若已有 `~/.mojito` 且还没有 `~/.falcon`，会继续用旧目录（库文件同理：只有 `mojito.db` 时不另开空库）——后端自己的 SQLite 开在里面，不能边跑边改名。远端在探测到宿主机后会尝试把 `~/.mojito` 改名为 `~/.falcon`；改不了（被占用、没权限）则沿用旧路径。

**从 Zellij 版本升级**：Zellij 已整体换成 falcon 自己的会话守护进程，旧会话不兼容——升级后旧的持久会话在列表里显示为已丢失，由你手动清除。会话里的程序其实还活在宿主机的 Zellij 里，falcon 不再管它们：想救可以 ssh 上去 `zellij attach mj-<…>`，用完自己结束（`zellij kill-all-sessions`）。

### 开发模式

```bash
cd native
cargo run -p falcon-server     # 开发服务端
cargo xtask web                # 改了客户端后重编浏览器版，刷新页面即可
cargo run -p falcon-desktop    # 原生客户端连本机服务
cargo xtask meegle             # 开发构建要用「飞书项目」面板时，先取一次锁定版本的 meegle CLI
```

## 单文件发布

```bash
cd native
cargo xtask server                        # 打当前平台
cargo xtask server --target linux-x64     # 交叉编译：先 rustup target add 对应三元组并配好链接器
cargo xtask server --skip-web             # web 产物已是最新时跳过 web 构建
```

产物在 `release/falcon-v<版本>-<平台>`，单个可执行文件（浏览器版客户端与锁定版本的 meegle CLI 都编在里面）：

```bash
./falcon-v0.1.0-linux-x64 --port 8080
```

macOS 还可以打一份安装包（把 `Falcon.app` 装进 `/Applications`，并注册用户级 launchd 服务，默认 http://127.0.0.1:4923）：

```bash
cargo xtask pkg                # 没有当前平台的服务端产物就先 cargo xtask server
cargo xtask pkg --skip-bin     # 只用已有的 release/falcon-v*-darwin-*
```

要 macOS 自带的 pkgbuild / iconutil / codesign，外加 `brew install librsvg`（rsvg-convert 现画 AppIcon）。

产物 `release/Falcon-v<版本>-darwin-<arch>.pkg`。没有开发者签名，别人机器上 Gatekeeper 会拦，系统设置里「仍要打开」即可。已经装过服务的机器可以直接拿它升级：postinstall 与 App 启动时都会先从 LaunchAgent 读出原来的 `--host` / `--port` / `--data-dir` 再 `service install`（`service-args.sh`），自定义端口与数据目录照旧。注意装着的 Falcon.app 每次启动都会用它自带的服务端重装服务，只换服务端二进制、不换 App 的话，下次打开旧 App 服务就被换回旧版。

Windows 只能打原生客户端的安装包（服务端不支持 Windows，装好后在客户端里连别处的 falcon 服务）：

```bash
cargo xtask win                # cargo build --release + Inno Setup
cargo xtask win --skip-cargo   # 只用已有的 native/target/release/falcon-desktop.exe
```

产物 `release/Falcon-v<版本>-win32-x64-setup.exe`，默认按当前用户装、不要管理员。必须在 Windows 上打：
需要 VS 生成工具（rc.exe 给 exe 编图标与版本信息）和 Inno Setup 6（`winget install JRSoftware.InnoSetup --scope user`，
或用 `FALCON_ISCC` 指到 ISCC.exe）。没有代码签名，别人机器上 SmartScreen 会拦，「更多信息 → 仍要运行」即可。

GitHub Actions（`.github/workflows/package.yml`）跑的就是这两条命令（经 `cargo run --manifest-path native/Cargo.toml -p xtask -- …`，CI 上删了 `native/.cargo/config.toml`，别名跟着没了）：推 `v*` tag 时 Windows 安装包与
macOS（Apple Silicon）pkg 一起打，挂到对应的 GitHub Release 上；Actions 页手动触发只出构建产物、不发版。
包里的版本号都取 `native/Cargo.toml` 的工作区版本（`[workspace.package] version`），打 tag 前先改好。

原理：`cargo build --release -p falcon-server --features embed-web,embed-meegle,embed-sessiond`——浏览器版客户端
（rust-embed）、本平台的 meegle CLI、5 个目标的会话守护进程（gzip）都编进二进制，没有首次运行的解压步骤；
meegle 在第一次用到时释放到 `<dataDir>/bin/meegle-<内容哈希>`，守护进程在第一次用到时释放 / 推送。
守护进程要 Zig（xtask 自己按钉死的版本下载）、rustup 的 5 个目标与 `cargo install cargo-zigbuild`。

注意事项：

- 体积约 40 MB，其中 meegle CLI 约 11 MB。
- 不支持 Windows 目标：服务的打包与托管只做了 macOS / Linux（Windows 宿主作为 SSH 远端是支持的）。

### 常驻运行（守护）

```bash
./falcon-v0.1.0-linux-x64 service install --host 0.0.0.0 --port 4923
./falcon-v0.1.0-linux-x64 service status     # 也有 start / stop / uninstall
```

`service install` 把 falcon 注册成系统服务：macOS 用 launchd（用户级 LaunchAgent），
Linux 用 systemd（root 装 system 级，普通用户装 user 级）。崩溃自动拉起、开机自启，
启动参数在 install 时指定并原样写进服务配置。单文件发布时会先把当前二进制拷到
`<dataDir>/bin/falcon`，服务永远跑这个固定路径——活动监视器 / `ps` 里进程名是
`falcon`，不是带版本号的下载文件名。

- 日志：Linux 看 `journalctl -u falcon -f`（user 级加 `--user`）；macOS 在
  `<dataDir>/logs/falcon.log`。
- Linux 普通用户安装后需执行一次 `sudo loginctl enable-linger <用户名>`，
  否则登出即停、开机不起。
- 绑非 localhost 前先在 localhost 启动并设好访问密码（数据目录一致），
  否则服务会反复启动失败。
- 没有 systemd 的环境（容器等）：`service install` 会打印 unit 内容，
  自行接到所用的 init；容器里通常直接前台跑，交给容器编排重启即可。

### 更新

两种方式任选：

```bash
# 方式一：用新二进制重跑 install，覆盖 <dataDir>/bin/falcon 并重启
./falcon-v0.2.0-linux-x64 service install --host 0.0.0.0 --port 4923

# 方式二：手动替换后再 restart（必须 mv，不要 cp）
mv falcon.new ~/.falcon/bin/falcon && ~/.falcon/bin/falcon service restart
```

替换文件必须用 `mv`（先传到同盘临时名再改名），**不要 `cp` 原地覆盖**：
macOS 上覆盖正在运行的二进制会令代码签名失效、相关进程被内核直接 SIGKILL，
Linux 上则会报 ETXTBSY。`mv` 换的是目录项与 inode，两个平台都安全。

数据无需干预：DB 迁移在启动时自动执行；`runtime/<哈希>` 目录按新版本重新解压，
旧目录自动清理。回滚就是用旧二进制重跑 `service install`——但若新版本做过
DB 迁移，旧代码不一定认识新库，降级前先备份数据目录里的 `falcon.db`。

## 安全说明

- 访问密码：单一密码门禁（scrypt 哈希）。绑定 localhost 可免密；绑定 `0.0.0.0` 强制要求已设密码，否则拒绝启动。
- SSH 凭据：密码 / passphrase 以 AES-256-GCM 加密落盘，密钥在数据目录 `secret.key` —— 防数据库文件单独泄露，不防整机被攻破。
- Host key：TOFU（首次连接记录指纹，之后指纹变化告警拒连）。
- TLS 不内置，对外部署请置于反向代理之后。
- 会话守护进程由后端推送，不从网络下载；它的 socket 只给当前用户（目录 0700、socket 0600、accept 时核对对端 uid），Windows 上的命名管道 DACL 只放行当前用户并拒绝远程客户端。

## 从远端卸载

falcon 在远端只写 `~/.falcon/`（二进制、守护进程的会话记录 / socket / 日志），删掉即可完全卸载（先 `falcon-sessiond kill` 掉还在跑的会话）。会话路径太长时 socket 会落到 `/tmp/falcon-sd-<uid>-<哈希>/`。删除项目不会自动清理远端——那台机器上可能还有你想留着的会话。

**唯一的例外是附属项目**：它的工作目录是 falcon 自己 `git worktree add` 出来的，删除时会跑 `git worktree remove --force` 并清理该目录。清理失败（被 lock、被占用、权限不足）时会把残留路径原样告诉你，项目记录照删——留一条删不掉的项目行，你唯一的出路是去改 SQLite；残留目录你自己删得掉。源项目自己的目录、`~/.falcon/` 一概不动，**分支也永远不删**（未推送的提交只有那一份）。

## 升级 ghostty（会话守护进程里的 libghostty-vt）

改 `native/sessiond/ghostty.pin` 的 commit 与源码包 sha256，然后：

```bash
cd native
cargo xtask ghostty --targets all     # 按新 pin 编静态库
cargo xtask ghostty-bindgen           # 重新生成绑定，diff 就是这次的 ABI 变化
cargo xtask sessiond --check --targets all && cargo xtask sessiond --test   # 零警告 + 真 PTY 用例 + 接回比对语料
```

宿主机上已有的会话不受影响：新版 falcon 接回时把它们的 VT 进程热换成新版本。

## 工程结构

```
native/
├── crates/
│   ├── falcon-proto/    线上协议类型（服务端与客户端共用）
│   ├── falcon-server/   服务端：axum + portable-pty + russh + rusqlite，会话核心跑在单线程 LocalSet 上
│   ├── falcon-client/   客户端的 REST / WS（原生 tokio、浏览器 fetch / WebSocket）
│   ├── falcon-term/     终端：alacritty_terminal + 按键 / 鼠标 / 滚轮编码
│   ├── falcon-theme/    主题系统（Ghostty 主题、界面色派生）
│   ├── falcon-core/     客户端纯逻辑（列式工作区排布、会话标题、快捷键……）
│   ├── falcon-platform/ 平台能力接口（偏好、服务端来源、访问密码、下载、HTML 预览……）
│   ├── falcon-ui/       GPUI 界面（不写平台分支，只认 falcon-platform）
│   ├── falcon-desktop/  原生客户端：入口 + 平台能力的原生实现
│   └── falcon-web/      浏览器版：wasm 入口 + 平台能力的浏览器实现
├── web/                 浏览器版的宿主页与内置图标
├── sessiond/            会话守护进程 falcon-sessiond（独立工作区：libghostty-vt 绑定、holder / VT 进程）
├── xtask/               构建 / 打包 / 资源生成任务（cargo xtask）
└── assets/fonts/        内嵌字体（woff2）
```
