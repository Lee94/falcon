# 文件面板：目录浏览器、改目录、文件夹上传

文件面板原先是一棵懒加载的树：点文件开查看 tab，右键能下载 / 往目录里上传（ADR 0008）。改工作目录——新建文件夹、重命名、删除、上传一整棵目录——明确不做，理由是"改文件是终端里的事"。用下来发现这几件在终端里做太绕：得先 `cd` 到对的地方、文件名要自己敲、删错了的确认在 TUI 里，而浏览器这边已经握着工作目录的列表。

面板改成目录浏览器：顶上路径栏、图标工具栏、当前目录平铺成表，支持多选。mkdir / rename / remove 三条变异路由进 `files.ts`，文件夹上传是 mkdir -p 再逐个走 0008 的上传。

## 决定一：平铺当前目录，不要树

树适合"同时看见好几层"，但路径栏、多选、按修改时间排序、工具栏上的"返回上层 / 新建 / 删除"都是文件管理器的交互，硬塞进树会变成两套模型。列目录的 API 本来就是一层一层拉的（`GET /files?path=`），前端只是不再把每一层嵌进上一层下面。

路径栏显示的是**宿主机绝对路径**（最初实现为 `project.workingDir` + 相对路径）。用户可以改完回车跳转；最初只允许工作目录以内，后来开放登录态 API 的宿主机绝对路径，见后记。API 请求的原始路径段仍拒绝 `..`（UI 路径栏可以先词法折叠），raw 预览仍有独立的目录范围，不能据此放宽为整机读取。

最初没有 `workingDir` 的项目只显示工作目录相对路径。绝对导航上线后，无 cwd 的面板先通过 `/api/fs/list` 取得真实 home，并用它作为初始绝对目录；客户端不把 `/` 当项目根的别名。

## 决定二：列目录带 size / mtime，隐藏文件默认不显示

表头有名称、大小、修改时间，目录的大小画成 —。远端 POSIX 在现有 `ls -1A` 循环里加一次 `stat`（先 `stat -c`，没有再 `stat -f`）；Windows 走 `Get-ChildItem` 的 `Length` / `LastWriteTimeUtc`。协议变成 `d|f <size> <mtime> <名字>`，名字是第三个空格之后的全部。

点开头的文件默认藏起来（眼珠按钮打开）。0008 时期的树把 `.github` 跟普通目录排在一起，是因为没有开关；有了开关之后默认跟文件管理器一致，要看点一下。

右侧栏默认 260px，大小和修改时间用容器查询在 ≥300 / ≥360 时才露出来，名称永远在。

## 决定三：mkdir / rename / remove 走 files.ts，不碰 git/remove.ts

`git/remove.ts` 删的是附属项目的 worktree 目录，护栏是"别把仓库根 / 家目录 rm -rf 掉"。文件面板删的是工作目录里用户刚点的那几项，护栏是 `resolveInside`。合成一条路径会把两种否决搅在一起。

三条命令都是纯函数，本地走 `node:fs`（`mkdir` / `rename` / `rm({ recursive: true })`），远端 POSIX 是 `mkdir` / `mv --` / `rm -rf --`，Windows 是 `New-Item` / `Move-Item -LiteralPath` / `[IO.Directory]::Delete`（不用 `Remove-Item`，理由同 ADR 0002：通配展开和 junction）。符号链接作为参数时只删链接本身。

删除是批量的：一次请求多条路径，祖先收下子孙，每条单独试，部分失败仍 200，`removed` / `errors` 分开。不能删工作目录本身。前端先弹确认框，列要删的名字。

重命名只改最后一段，不移动到别的目录；目标已存在回 409。

新建文件夹非递归（同名即 409）。文件夹上传走递归 mkdir：中间层已存在当成功，已存在一个同名**文件**仍是 409。

## 决定四：文件夹上传 = mkdir -p + 0008 的逐文件 PUT

浏览器 `webkitdirectory` 给出的每个 `File` 带 `webkitRelativePath`（含被选中的顶层文件夹名）。先对最深的那批中间层 `POST /mkdir recursive=1`，再按相对路径逐个 `PUT /upload`。覆盖确认、进度 toast、顺序上传全部复用 0008。空文件夹浏览器给不出来，接受。

不做拖拽上传、不做打包下载。多选下载只处理文件，文件夹跳过并 toast；浏览器对连续的 `<a download>` 会拦，两次之间隔 250ms。

## 已知取舍

- 递归删除大目录（远端 `node_modules`）超时 120 秒，再长的只能去终端。
- Windows 远端与真实 SSH 主机上的 mkdir / rename / remove 未在真机上验过（本地项目在浏览器里验），改动那边先在 172.16.25.134 那台机器上跑一遍。
- 多选下载不是 zip：每个文件一次浏览器下载，文件夹请去终端打包。

## 后记：登录态 API 接受宿主机绝对路径

右侧文件面板不再被工作目录圈住：list / read / save / download / upload / mkdir / rename / remove 的 `path` 可以是宿主机绝对路径，原项目相对路径继续兼容。POSIX 是 `/foo`；Windows 是 `C:/foo`、`C:\foo` 或 UNC `\\server\share\foo`，对外响应统一 `/`。路径按实际宿主机探测到的 HostKind 解释，不按后端运行平台猜测；POSIX 文件名里的反斜杠不会被改成分隔符。

API 层把绝对请求映射为文件系统根 + 相对 segments，再复用 `files.rs` 与 `transfer.rs`。底层 `resolve_inside` 不变，也不改 git/worktree 的删除入口。`.` / `..`、Windows 盘符相对路径 / 设备命名空间 / 尾点空格别名拒绝。绝对请求的 listing.path / entry.path、save / upload / mkdir / rename / remove 返回值保持绝对身份；相对请求仍回相对路径。Quick Open `/files/index` 仍只索引项目工作目录，不能经 path 参数扩大范围。

multi 的初始位置 / 快捷切换由客户端直接传成员 `dir`，不用 repo 参数、不改数据库。绝对路径操作不依赖虚拟容器目录存在；相对请求与 Quick Open 仍沿用原工作目录选择规则。

重定位操作根不能丢掉原护栏：rename / remove 同时禁止项目 working_dir、multi 成员根、POSIX `/`、盘符根、UNC share 根；目标是项目 / 成员根的祖先也拒绝，否则删除父目录就能绕过根保护。rename 的目标也检查同一护栏，不能覆盖保护根。普通的项目外目录仍可正常改动；mkdir / 上传不受这项删除护栏限制。批量删除仍部分成功、每条返回 errors，空相对路径仍忽略。

绝对路径预览的 raw token 另外绑定不可变目录范围，不能借登录态的绝对路径支持扩大原 project token；具体见 ADR 0007 后记。变异回归测试只在临时数据目录上运行，Windows / UNC 的路径与保护根用纯函数测试；实际 Windows / SSH 传输仍需隔离环境真机验证。
