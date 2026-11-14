# 部署

开发时直接跑服务端与客户端的步骤见 [README](../README.md)。本文是启动参数、安装包、系统服务与卸载。

## 启动参数

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `--port` | `4923` | 监听端口 |
| `--host` | `127.0.0.1` | 绑定地址。能连上该地址就允许使用，没有访问密码 |
| `--data-dir` | `~/.falcon` | 数据目录（SQLite、加密密钥、本机的会话守护进程与它的会话） |

对应环境变量：`FALCON_PORT`、`FALCON_HOST`、`FALCON_DATA_DIR`。旧名 `MOJITO_*` 仍可作回退。

本机若已有 `~/.mojito` 且还没有 `~/.falcon`，会继续用旧目录。库文件同理：只有 `mojito.db` 时不另开空库。后端自己的 SQLite 开在这个目录里，不能边跑边改名。远端在探测到宿主机后会尝试把 `~/.mojito` 改名为 `~/.falcon`；改不了（被占用、没权限）则沿用旧路径。

## 单文件服务端

```bash
cd native
cargo xtask server                        # 当前平台
cargo xtask server --target linux-x64     # 交叉编译：先 rustup target add 对应三元组并配好链接器
```

平台名：`darwin-arm64`、`darwin-x64`、`linux-x64`、`linux-arm64`。不支持把服务端编成 Windows 目标。Windows 作为 SSH 远端是支持的。

产物在 `release/falcon-v<版本>-<平台>`，一个可执行文件。五个目标的会话守护进程（gzip）编在里面，没有首次运行时解压整包的步骤；守护进程在第一次用到时释放或推送。构建要 Zig（xtask 按锁定版本自己下载）、rustup 的五个目标，以及 `cargo install cargo-zigbuild`。

```bash
./falcon-v<版本>-linux-x64 --port 8080
```

版本号取 `native/Cargo.toml` 的 `[workspace.package] version`。

## macOS 安装包

安装包把 `Falcon.app` 装进 `/Applications`，并注册用户级 launchd 服务，默认 http://127.0.0.1:4923。App 里是原生客户端，Resources 里带着服务端。

```bash
cd native
cargo xtask pkg                # 没有当前平台的服务端产物就先 cargo xtask server
cargo xtask pkg --skip-bin     # 只用已有的 release/falcon-v*-darwin-*
```

要在 macOS 上打，用系统自带的 pkgbuild、iconutil、codesign，外加 `brew install librsvg`（rsvg-convert 画 AppIcon）。

产物 `release/Falcon-v<版本>-darwin-<arch>.pkg`。没有开发者签名，别人机器上 Gatekeeper 会拦，在系统设置里选择仍要打开即可。已经装过服务的机器可以直接拿它升级：postinstall 与 App 启动时都会先从 LaunchAgent 读出原来的 `--host`、`--port`、`--data-dir`，再执行 `service install`。自定义端口与数据目录照旧。装着的 Falcon.app 每次启动都会用它自带的服务端重装服务。只换服务端二进制、不换 App 的话，下次打开旧 App，服务会被换回旧版。

## Windows 安装包

安装包里是原生客户端 + 同目录的服务端 `falcon-server.exe`。装完安装包会用它跑
`falcon service install`，注册当前用户登录自启的计划任务（schtasks），默认
http://127.0.0.1:4923；卸载时对应 `service uninstall`。客户端启动时也会用捆绑的
服务端补装 / 升级服务——升级不带参数，沿用已装任务原来的 `--host` / `--port` /
`--data-dir`（从任务 XML 的 Exec 段读回）。

```bash
cd native
cargo xtask win                # cargo build --release；没有 release/falcon-v*-win32-x64.exe 就先 cargo xtask server --skip-sessiond（守护进程不在 Windows 构建，embed 取 CI 的 falcon-sessiond-embed）
cargo xtask win --skip-cargo   # 只用已有的 falcon-desktop.exe + 服务端发布产物
```

产物 `release/Falcon-v<版本>-win32-x64-setup.exe`，默认按当前用户安装，不需要管理员。必须在 Windows 上打：需要 VS 生成工具（rc.exe 给 exe 编图标与版本信息）和 Inno Setup 6（`winget install JRSoftware.InnoSetup --scope user`，或用 `FALCON_ISCC` 指到 ISCC.exe）。没有代码签名，别人机器上 SmartScreen 会拦，选择「更多信息 → 仍要运行」即可。

## Linux 服务端安装文件

只含服务端，装好即注册 system 级 systemd 服务（数据目录 `/var/lib/falcon`）：

```bash
cd native
cargo xtask linux --target linux-x64      # 要 release/falcon-v*-linux-x64，没有先 cargo xtask server
```

产物三个：`falcon-v<版本>-linux-<arch>.deb`、`falcon-v<版本>-linux-<arch>.rpm` 与
`falcon-v<版本>-linux-<arch>-install.sh`（自解包脚本，`sudo ./…-install.sh` 装到
/usr/local/bin 并注册服务）。deb / rpm 只放 /usr/bin/falcon，脚本直接调
`service install` / `uninstall`；升级时 package 管理器走到同一对脚本，已有服务
保留原启动参数。install.sh 在无 root 时装 `~/.local/bin`、注册 user 级服务。

## GitHub Actions

[`.github/workflows/package.yml`](../.github/workflows/package.yml) 跑的就是 `cargo xtask win` 与 `cargo xtask pkg`。CI 上会删掉 `native/.cargo/config.toml`，xtask 别名跟着没了，所以写成 `cargo run --manifest-path native/Cargo.toml -p xtask -- …`。推 `v*` tag 时，Windows 安装包与 macOS（Apple Silicon）pkg 一起打，挂到对应的 GitHub Release 上。在 Actions 页手动触发只出构建产物，不发版。打 tag 前先改好 `native/Cargo.toml` 里的版本号。

## 常驻运行

```bash
./falcon-v<版本>-linux-x64 service install --host 0.0.0.0 --port 4923
./falcon-v<版本>-linux-x64 service status
```

`service` 的子命令还有 `start`、`stop`、`restart`、`uninstall`。启动参数只在 `install` 时生效，并原样写进服务配置。

`service install` 把 falcon 注册成系统服务：macOS 用 launchd（用户级 LaunchAgent），Linux 用 systemd（root 装 system 级，普通用户装 user 级），Windows 用计划任务（schtasks，当前用户登录自启，免管理员）。崩溃自动拉起，开机自启。单文件发布时会先把当前二进制拷到 `<dataDir>/bin/falcon`，服务永远跑这个固定路径。活动监视器与 `ps` 里的进程名是 `falcon`。

- 日志：Linux 看 `journalctl -u falcon -f`（user 级加 `--user`）。macOS 在 `<dataDir>/logs/falcon.log`。
- Linux 普通用户安装后需执行一次 `sudo loginctl enable-linger <用户名>`，否则登出即停、开机不起。
- 没有 systemd 的环境（容器等）：`service install` 会打印 unit 内容，自行接到所用的 init。容器里通常直接前台跑，交给容器编排重启。

## 更新

装好的 macOS 应用可以在设置「关于」或菜单栏里「检查更新」（ADR 0026）。它对照 GitHub 上 `Lee94/falcon` 的最新 Release，下载对应平台的安装包。macOS 上替换本机的 `Falcon.app`，再按 LaunchAgent 里原来的 `--host` / `--port` / `--data-dir` 执行 `service install`，然后关闭并重新打开新版本。文件窗口里没保存的修改会先问一句，取消则不关闭。Windows 安装包会关掉正在运行的程序，所以只下载、不启动。

服务端单独换二进制时，两种方式任选：

```bash
# 用新二进制重跑 install，覆盖 <dataDir>/bin/falcon 并重启
./falcon-v<版本>-linux-x64 service install --host 0.0.0.0 --port 4923

# 手动替换后再 restart。必须改名换入，不要原地覆盖
mv falcon.new ~/.falcon/bin/falcon && ~/.falcon/bin/falcon service restart
```

替换文件必须用 `mv`（先传到同盘临时名再改名），不要用 `cp` 原地覆盖。macOS 上覆盖正在运行的二进制会令代码签名失效，相关进程被内核直接 SIGKILL。Linux 上则会报 ETXTBSY。`mv` 换的是目录项，两个平台都安全。`service install` 自己拷贝时也是先写入临时文件再改名。

数据无需干预：库结构在启动时自动补齐。回滚就是用旧二进制重跑 `service install`。新版本若改过库结构，旧代码不一定认识新库，降级前先备份数据目录里的 `falcon.db`。

## 安全

- 没有账户或访问密码。能连上服务就允许使用。对外绑定时请自行限制谁能访问这个地址。
- SSH 凭据：密码与 passphrase 以 AES-256-GCM 加密落盘，密钥在数据目录的 `secret.key`。这防的是数据库文件单独泄露，不防整机被攻破。
- Host key：首次连接记录指纹（TOFU），之后指纹变化则告警并拒绝连接。
- TLS 不内置。对外部署请置于反向代理之后。
- 会话守护进程由后端推送，不从网络下载。它的 socket 只给当前用户（目录 0700、socket 0600，accept 时核对对端 uid）。Windows 上的命名管道 DACL 只放行当前用户，并拒绝远程客户端。

## 从远端卸载

falcon 在远端只写 `~/.falcon/`（二进制、守护进程的会话记录、socket、日志），删掉即可完全卸载。先用 `falcon-sessiond kill <SID>` 结束还在跑的会话。会话路径太长时 socket 会落到 `/tmp/falcon-sd-<uid>-<哈希>/`。删除项目不会自动清理远端：那台机器上可能还有你想留着的会话。

附属项目是例外。它的工作目录是 falcon 自己用 `git worktree add` 建的，删除时会清理该目录。清理失败（被 lock、被占用、权限不足）时会把残留路径原样告诉你，项目记录照删。源项目自己的目录和 `~/.falcon/` 不动，分支也不删。规则见 [using.md](./using.md) 的附属项目一节。
