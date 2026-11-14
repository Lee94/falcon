# 本地原生客户端更新

## 应用内更新

设置「关于」和菜单栏「检查更新」对照 `Lee94/falcon` 的 GitHub Release（ADR 0026）。macOS 下载 pkg、展开 `Falcon.app`、改名换入后，按 LaunchAgent 原来的参数执行 `service install`，再经正常退出重新打开新应用。没保存的文件修改会先问一句，取消则不退出。开发构建不在 `.app` 里，不退出当前进程。同版本号的本地提交不会被当成新版本。下面这一节仍是开发机上从源码替换已安装应用的步骤。

## 只更新客户端

- 正式构建：在 `native/` 执行 `cargo build --release -p falcon-app`（产物 `native/target/release/falcon-app`）。
- 已安装应用位于 `/Applications/Falcon.app`。先用 `ditto` 复制为同一目录下的临时应用，再以 `falcon-app` 替换 `Contents/MacOS/Falcon`，按工作区版本更新 `Info.plist`，执行 `codesign --force --deep --sign - <临时应用>`，并用 `codesign --verify --deep --strict <临时应用>` 验证。
- 应用启动可能用 `Contents/Resources/falcon` 重新安装本机服务。先读取 `~/Library/LaunchAgents/com.falcon.server.plist` 的 `ProgramArguments`，把当前安装的服务端程序复制进临时应用，避免旧应用捆绑的服务端覆盖当前服务端。核对两份服务端程序的 SHA-256，保留原 `--host`、`--port` 和 `--data-dir`。
- `/Applications` 对当前管理员用户可写时，可以先把旧应用改名备份，再把临时应用改名为 `Falcon.app`，无需修改原应用内 root 所有的文件。替换失败时把备份改回原名。不要强制终止用户客户端，以免丢失文件编辑内容；退出须经过正常退出流程。
- 启动后核对应用版本、签名、进程路径、客户端到原端口的连接；`dwarfdump --uuid` 可核对安装的客户端是否来自本次构建，重新签名不改变该 UUID。

## 服务端命令注意事项

服务端当前不支持 `--version`。`main.rs` 仅对 `service` 与 `respawn` 做子命令分流，其余参数交给配置解析；未知参数可能被忽略，直接运行 `falcon --version` 会按默认配置启动服务。不要用这个命令探测版本，尤其不能对真实数据目录启动额外实例。

版本读取工作区 `native/Cargo.toml`、应用 `Info.plist`，或已运行服务端的 `/api/system`（按接口要求鉴权）。服务控制使用源码已确认的 `falcon service status` / `falcon service restart`；重启使用已有 LaunchAgent 配置。开发验证使用独立端口和短临时数据目录。

## 客户端与服务端同时更新

- 运行 `cargo build --release -p falcon-app` 与 `cargo xtask server`；服务端发布产物包含全部目标的 sessiond。临时应用的 `Contents/Resources/falcon` 必须替换为本次服务端产物，再签名、校验并切换应用。
- 2026-11-08 已清掉旧数据并重装。当前 LaunchAgent 只写了程序路径 `/Users/fay/.falcon/bin/falcon`，没有额外的 `--host` / `--port` / `--data-dir`，因此是 `127.0.0.1:4923` 与 `~/.falcon`。数据库是新建的 `falcon.db`，访问密码未设置。`~/.mojito` 已删除。每次更新仍应读取 LaunchAgent 的实际配置，不固定使用这些参数。
- 更早的安装用过 `mojito.db`。备份数据库前按 `db::db_file` 和实际文件选择路径，使用 SQLite backup API，不能假定总是 `falcon.db`。
- 旧的 `/Users/fay/.mojito/bin/falcon` 曾带 `uchg`（不可修改）标记，会使应用启动时的 `service install` 失败且继续连接旧服务。当前 `~/.falcon/bin/falcon` 没有该标记。若标记再次出现，更新前先 `chflags nouchg`，用新服务端执行带原参数的 `service install`。
- 更新完成必须核对应用捆绑的服务端与实际运行路径的 SHA-256，并鉴权调用 `/api/agents` 验证新接口；同版本号或客户端已启动不代表服务端已经更新。核对更新前后会话 id 与活跃状态，保留应用、服务端二进制、数据库和 LaunchAgent 的备份。

## 菜单栏上的两份应用

旧进程不退出，新进程再挂一个状态项。这一版起，同时有多份时，状态项图标旁边和左边的应用菜单会写出版本号。版本号相同（本地更新经常不改工作区版本）再写可执行文件的修改时间，同一分钟再写秒，仍相同则写 pid。只剩一份时这些字会去掉。已经启动的旧进程没有这段逻辑，图标仍是一只鸟；新进程会带上版本，因此能够区分。两边都是这一版之后，各自显示自己的标记。

## 2026-11-09 上午这次安装

- 客户端与服务端一起更换，版本号仍是 0.3.19。LaunchAgent 仍只有程序路径，监听 `127.0.0.1:4923`，数据目录 `~/.falcon`。`~/.falcon/bin/falcon` 没有 `uchg`。
- 应用内 `Contents/Resources/falcon` 与 `~/.falcon/bin/falcon` 的 SHA-256 都是 `fe0f919a8a18514bcf43e3d53b174fa95c2d94fd38edc7ff6cee085d400fe24c`。客户端 UUID 是 `04DE599E-84C6-3469-8572-F39C9912E029`。
- 本次备份在 `~/.falcon-backups/20261109-095231`（服务端程序、数据库、LaunchAgent）和 `/Applications/Falcon.backup-20261109-095231.app`。持久会话 `627e8d29-24f5-4b89-9615-56155f8500aa`（九数云）更新后仍是 `active`。
- 上一份同时安装的服务端 SHA-256 是 `a4a0a2ccc076724b30ee922b15250e04094d6cac2c68f897aaff4989b4dbe37b`，客户端 UUID 是 `D60538B7-7E5F-3A3B-B0A0-4864725C85D8`，备份在 `~/.falcon-backups/20261109-004333` 与 `/Applications/Falcon.backup-20261109-004333.app`。更早的 `~/.falcon-backups/20261109-002844` 备份时 `ssh_hosts` 是空的；那之后写入的主机在 004333 与本次备份里都有。

## 2026-11-09 中午只更新服务端

- 只跑了 `cargo xtask server`，没有重编客户端。五个目标的 sessiond 源码不新于今早的产物，这次嵌入的仍是那一份。版本号仍是 0.3.19。LaunchAgent 仍只有程序路径，监听 `127.0.0.1:4923`，数据目录 `~/.falcon`。`~/.falcon/bin/falcon` 没有 `uchg`。
- 应用内 `Contents/Resources/falcon`、`~/.falcon/bin/falcon` 与 `release/falcon-v0.3.19-darwin-arm64` 的 SHA-256 都是 `14d31caca61a74247f8de83d3db2c9437a4b792c42e6672a5a1eff2e2eb964f8`。客户端 UUID 仍是 `04DE599E-84C6-3469-8572-F39C9912E029`。当时正在运行的客户端（pid 30684）没有退出，可执行文件仍映射在备份应用里。
- 备份在 `~/.falcon-backups/20261109-125956`（服务端程序、`sqlite3 .backup` 的数据库、LaunchAgent、更新前的会话列表）和 `/Applications/Falcon.backup-20261109-125956.app`。更新前在跑的 5 个持久会话更新后仍是 `active`：`e18aed9d-b0c1-405d-a147-a9f6a0337e2f`、`07ca8ffc-c880-4c1b-9c62-e9b56e839a82`、`b3a7ba9f-5b0c-4de3-a036-aeb451039b52`、`4da03279-92b6-45c3-aaa6-7526de728dff`、`042fa135-bdba-4f36-a228-34a5fdc0f82d`。上午那条 `627e8d29-24f5-4b89-9615-56155f8500aa` 这次更新前已经不在会话列表里。
- 2026-11-09 13:10 用户要求清掉全部旧版本。`/Applications` 里只剩当前的 `Falcon.app`。`~/.falcon-backups` 已整份删除（含上面列出的各次备份）。`~/.falcon/bin` 里旧的 `meegle-1d797e77c6f1` 已删除。`release/` 只留当前的 `falcon-v0.3.19-darwin-arm64`（SHA-256 `14d31caca61a74247f8de83d3db2c9437a4b792c42e6672a5a1eff2e2eb964f8`），0.3.0 / 0.3.15 的二进制和 pkg、11 月 8 日的 `Falcon-v0.3.19-darwin-arm64.pkg`、Tern 插件包与 `falcon-tern` 二进制都已删除。上面各节里的备份路径已经不存在。

## 2026-11-09 13:28 只更新客户端

- `cargo build --release -p falcon-desktop`，没有 `snapshot` / `automation`。版本号仍是 0.3.19。换上前客户端已经退出。旧应用改名为 `/Applications/Falcon.backup-20261109-132833.app`，再把签过名的临时应用改成 `Falcon.app`。
- 客户端 UUID 是 `1FDBFFC2-881B-38B7-A787-6C843E3C2894`，与 `native/target/release/falcon-desktop` 一致。签名 `codesign --verify --deep --strict` 通过。进程连着 `127.0.0.1:4923`。
- 应用内 `Contents/Resources/falcon` 在签名前换成了当时正在跑的服务端，SHA-256 仍是 `14d31caca61a74247f8de83d3db2c9437a4b792c42e6672a5a1eff2e2eb964f8`，与 `~/.falcon/bin/falcon` 相同，没有 `uchg`。启动时服务被重新拉起（新 pid），二进制没变。5 个持久会话仍是 `active`：`e18aed9d-b0c1-405d-a147-a9f6a0337e2f`、`07ca8ffc-c880-4c1b-9c62-e9b56e839a82`、`b3a7ba9f-5b0c-4de3-a036-aeb451039b52`、`4da03279-92b6-45c3-aaa6-7526de728dff`、`042fa135-bdba-4f36-a228-34a5fdc0f82d`。
