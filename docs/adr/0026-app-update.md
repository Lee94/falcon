# 应用内更新：检查 GitHub Release，替换本机应用，装完后重新打开

发布已经挂在 GitHub Release 上（`v*` tag，`.github/workflows/package.yml` 把 macOS pkg 与 Windows 安装包传上去）。装好的应用要更新，之前只能再下一份安装包，或在开发机上按 `.agents/memory/native-client-update.md` 手工替换。设置里的「关于」只显示版本号。

## 决定一：对照 `Lee94/falcon` 的最新 Release，不自建更新服务器

`GET https://api.github.com/repos/Lee94/falcon/releases/latest`。tag 是 `v0.3.20` 这种三段版本号，比正在跑的 `CARGO_PKG_VERSION` 大才算有更新。同版本、更旧的、解析不了的（含预发布 `1.2.3-rc.1`）都不装，也不降级。工作区版本没加、只是本地多了提交时，这里看不出来——那不是一次发布。

安装包文件名与 `cargo xtask pkg` / `cargo xtask win` 一致：

- macOS：`Falcon-v<版本>-darwin-arm64.pkg` 或 `darwin-x64.pkg`
- Windows：`Falcon-v<版本>-win32-x64-setup.exe`

Linux 没有客户端安装包，检查直接说明没有这个平台的包。

下载地址必须是 `https://github.com/Lee94/falcon/releases/download/<tag>/<文件>`。查询串、`..`、别的主机一律拒绝。仓库对未登录请求返回 404（没有 Release，或仓库不可公开读）时，界面写「没有找到已发布的版本」。

## 决定二：macOS 改名换入 Falcon.app，再按原来的参数 `service install`

不跑 `installer`。那要管理员密码，postinstall 又以 root 身份转回当前用户。展开 pkg（`pkgutil --expand-full`）取出已经签过名的 `Falcon.app` 即可。

换入的目标：进程自己在某个 `Falcon.app` 里，就换这一份（用户可能把它移出「应用程序」）；否则换 `/Applications/Falcon.app`。旧应用改名为旁边的 `Falcon.backup-<时间>.app`，留下不删。新应用用改名换入，不覆盖正在映射的可执行文件，所以当前进程继续跑。`/tmp` 与目标不在同一卷时，先 `ditto` 到目标目录旁边再改名。

展开出来的包要同时满足：`Contents/MacOS/Falcon` 与 `Contents/Resources/falcon` 都在，`CFBundleShortVersionString` 与发布版本一致，`codesign --verify --deep --strict` 通过。对不上就按发布时的方式 ad-hoc 重签再验一次。下载带上的隔离属性（`com.apple.quarantine`）清掉，否则下次打开会被系统标成已损坏。已安装的 `~/.falcon/bin/falcon` 若带 `uchg`，安装前先 `chflags nouchg`。

然后用新应用里的 `Contents/Resources/falcon` 执行 `service install`，参数用 LaunchAgent 里原来的 `--host` / `--port` / `--data-dir`（一个都没写就仍走服务端默认值）。这一步会重启本机服务。持久会话在守护进程里，继续跑。`service install` 失败时，把旧应用改回原名，并用它里面的服务端再装一次。

正在运行的就是这份应用时，安装结束后走和「退出 Falcon」相同的确认：没有未保存的文件修改就退出，有的话先问一句，取消则留下当前进程。确认退出之后才派一个脱离本进程的帮手，等这个 pid 消失，再用 `open` 打开换入后的 `Falcon.app`。帮手在确认之前不派，避免用户取消后，下次普通退出仍被拉起第二份。开发构建的进程不在包里，只替换 `/Applications/Falcon.app`，不退出当前进程。

## 决定三：Windows 只下载安装包

Inno Setup 的脚本写了 `CloseApplications=yes`，运行它会关掉正在运行的 Falcon，绕过退出确认。更新只把 `setup.exe` 下到「下载」文件夹，并说明要等退出之后再运行。

## 决定四：纯逻辑在 falcon-core，安装在 falcon-desktop，界面只认 Platform

版本比较、选安装包、下载地址、`.app` 路径与改名换入在 `falcon-core` 的 `app_update.rs`，有单测。网络、`pkgutil`、签名、`service install`、退出后重新打开在 `falcon-desktop`。`Platform::app_updater` 与 `schedule_relaunch` 交给界面。设置「关于」与菜单栏「检查更新」共用一份状态；菜单栏只读忙闲，避免在 AppKit 回调里再进一次 GPUI 更新。安装前有确认框。

同一时间只跑一次检查或安装。
