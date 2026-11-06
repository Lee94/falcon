# 本地原生客户端更新

## 只更新客户端

- 正式构建：在 `native/` 执行 `cargo build --release -p falcon-desktop`，不启用 `snapshot` 或 `automation`。
- 已安装应用位于 `/Applications/Falcon.app`。先用 `ditto` 复制为同一目录下的临时应用，再替换 `Contents/MacOS/Falcon`，按工作区版本更新 `Info.plist`，执行 `codesign --force --deep --sign - <临时应用>`，并用 `codesign --verify --deep --strict <临时应用>` 验证。
- 应用启动可能用 `Contents/Resources/falcon` 重新安装本机服务。先读取 `~/Library/LaunchAgents/com.falcon.server.plist` 的 `ProgramArguments`，把当前安装的服务端程序复制进临时应用，避免旧应用捆绑的服务端覆盖当前服务端。核对两份服务端程序的 SHA-256，保留原 `--host`、`--port` 和 `--data-dir`。
- `/Applications` 对当前管理员用户可写时，可以先把旧应用改名备份，再把临时应用改名为 `Falcon.app`，无需修改原应用内 root 所有的文件。替换失败时把备份改回原名。不要强制终止用户客户端，以免丢失文件编辑内容；退出须经过正常退出流程。
- 启动后核对应用版本、签名、进程路径、客户端到原端口的连接；`dwarfdump --uuid` 可核对安装的客户端是否来自本次构建，重新签名不改变该 UUID。

## 服务端命令注意事项

服务端当前不支持 `--version`。`main.rs` 仅对 `service` 与 `respawn` 做子命令分流，其余参数交给配置解析；未知参数可能被忽略，直接运行 `falcon --version` 会按默认配置启动服务。不要用这个命令探测版本，尤其不能对真实数据目录启动额外实例。

版本读取工作区 `native/Cargo.toml`、应用 `Info.plist`，或已运行服务端的 `/api/system`（按接口要求鉴权）。服务控制使用源码已确认的 `falcon service status` / `falcon service restart`；重启使用已有 LaunchAgent 配置。开发验证使用独立端口和短临时数据目录。

## 客户端与服务端同时更新

- 运行 `cargo build --release -p falcon-desktop` 与 `cargo xtask server`；服务端发布产物包含 meegle、全部目标的 sessiond 和本平台 mesh sidecar。临时应用的 `Contents/Resources/falcon` 必须替换为本次服务端产物，再签名、校验并切换应用。
- 当前服务配置为 `--host 0.0.0.0 --port 6789 --data-dir /Users/fay/.mojito`；每次更新仍应读取 LaunchAgent 的实际配置，不固定使用这些参数。
- 旧安装实际使用 `mojito.db`；备份数据库前按 `db::db_file` 和实际文件选择路径，使用 SQLite backup API，不能假定总是 `falcon.db`。
- `/Users/fay/.mojito/bin/falcon` 带 `uchg`（不可修改）标记，会使应用启动时的 `service install` 失败且继续连接旧服务。用户已授权更新时，先 `chflags nouchg`，用新服务端执行带原参数的 `service install`，最后恢复 `chflags uchg`。
- 更新完成必须核对应用捆绑的服务端与实际运行路径的 SHA-256，并鉴权调用 `/api/agents` 验证新接口；同版本号或客户端已启动不代表服务端已经更新。核对更新前后会话 id 与活跃状态，保留应用、服务端二进制、数据库和 LaunchAgent 的备份。

## Tern 原生 OMP 接入

- 本机 Tern 连接程序为 `~/.local/bin/falcon-tern`，连接配置为 `~/Library/Application Support/Tern/plugin-data/falcon/bridge.json`。本机服务仍使用实际 LaunchAgent 的端口和数据目录，`sessiondRoot` 必须一致。
- 连接程序先请求服务准备持久会话，再读取 `<data-dir>/sessiond/binary.json` 获取经握手验证的带哈希守护进程路径，不依赖手动创建的 `bin/falcon-sessiond`。
- OMP 显示扩展位于 `~/.omp/agent/extensions/falcon-tern.ts`，通过私有 Unix 套接字协作切换。首次安装后旧进程必须重新启动并恢复会话；`/reload` 不加载新扩展。已加载扩展后的界面切换保留同一进程和未提交草稿，不注入按键或信号。
- 守护进程的 TSP hello 门控须保持后续所有 DA1 回包的先进先出顺序；只延迟探测本身的回包会让后续能力查询提前结束原生协商。普通光标位置回包仍立即发送。
- Tern 0.9.12 的 DA1 为 `CSI ?62;52;c`，末尾含省略参数。`terminal_replies::ReplyFilter` 必须按 CSI 语法接受空参数，否则外部回包会与 VT 的回包重复，WebSocket 合并输出时会抢先结束 OMP 的 TSP 检测。原生 hello 回包及括号粘贴中的原文仍须保留。
- WebSocket 连接程序在原生 Tern 模式下不得预先切到备用屏：即使 OMP 的原生协商已成功，inline 图形界面在备用屏中仍不可见。保留主屏，由来宾自身的 VT 字节流控制全屏程序；非原生模式沿用原备用屏处理。
- macOS 真机已验证：OMP 18.16.0 / Tern 0.9.12 原生编辑器、中文与表情草稿、断开恢复字符界面、服务重启接回、原生界面运行期间 VT 热升级及升级后继续编辑；同一进程 PID 保持不变。
