# 文件下载与上传：流式 exec 通道

文件面板原本只能"看"：列目录、读文本、预览图片和 HTML。浏览器与宿主机之间搬一个文件——把远端跑出来的构建产物拿到本机、把本机的一份数据塞进远端工作目录——终端里做不到（终端只是字节流），只能另起 scp。现在文件面板上文件行右键有「下载」、目录行右键有「上传到此文件夹…」（面板头部另有上传到根目录的按钮），查看 tab 的工具栏也有下载按钮。实现在 `transfer.ts`（命令构造、base64 行编解码、落盘规则）、`routes.ts`（两条路由）、`sessions/ssh.ts`（`execStream`）、`FilesPanel.tsx` / `api.ts`（右键菜单、XHR 上传、进度 toast）。

## 决定一：不复用原始字节路由，另起两条按流走的路由

原始字节路由（ADR 0007）把整个文件攒成 Buffer 再回，上限 16MB——预览要的就是这样（截断的图片没有意义，索性拒绝）。下载没有理由受这个限制，上传更不可能先攒进内存。所以 `GET /api/projects/:id/download?path=` 与 `PUT /api/projects/:id/upload?path=&name=` 都是**流对流**：本地是 `fs.createReadStream` / `createWriteStream`，远端是 SSH exec 通道的 stdout / stdin，中途不落 Buffer。

鉴权用普通登录 cookie。下载由主页面的 `<a download>` 点击触发，是同源导航，浏览器带 cookie；上传是 XHR，同样带。原始字节路由之所以要把令牌放进 URL，是因为它的请求来自 opaque origin 的沙箱 iframe，这里没有那个问题，也就不该再多一种凭据。

下载响应一律 `application/octet-stream` + `Content-Disposition: attachment`：这是"存到本地"，不是"在浏览器里看"，文件类型让浏览器按后缀自己认。`filename*=UTF-8''…` 给中文文件名，`filename="…"` 是 ASCII 兜底（非 ASCII 换成下划线，引号与反斜杠会破坏 quoted-string）。`Content-Length` 从一次 cap 0 的读取拿到（`readWorkspaceBytes(…, 0)`，与查看 tab 判"存在 / 是目录 / 没权限"共用同一段代码），浏览器的下载进度条靠它。远端多一次往返，对一次下载不算什么；换来的是所有错误都能在响应头发出之前变成 4xx。

上传的 `Content-Length` 是必需的（浏览器给 File 请求体一定带），服务端拿它做落盘核对（见决定三）。

## 决定二：远端仍不走 SFTP；Windows 走"每行独立可解"的 base64

不走 SFTP 的理由同 fs.ts（sftp-server 被禁的机器并不少见）。POSIX 的 exec 通道对原始字节是可靠的（paste.ts 早就靠 `cat > file` 收 stdin 原始字节），所以下载就是 `cat 'file'`、上传就是 `cat > 'tmp'`，零开销。

Windows 的 OpenSSH 会按代码页改写通道上的字节（paste.ts 里踩过），两个方向都只能走 base64。但不能像 readCommand 那样整文件 `[Convert]::ToBase64String` 一坨——那要把整个文件读进内存，两端都是。这里用的是 **base64 行**：编码端每 48KB（3 的倍数）输出一行，每一行都是一段完整的 base64，解码端逐行解即可（`Base64LineEncoder` / `Base64LineDecoder`，Node 侧；PowerShell 侧下载是分块 `Read` + `WriteLine`，上传是 `ReadLine` + `FromBase64String` 逐行写 FileStream）。行长不必固定：PowerShell 的 `Read` 一次没给满也没关系，解码端不关心行长，只要每行自成一段。`[Console]::Out.WriteLine` 而不是 `Write-Output`：后者一行行过 PowerShell 的格式化管道，几万行下来慢得多。

`SshLink.execStream` 把 ssh2 的通道原样交出来：写入端是 stdin、读取端是 stdout、`close` 带退出码。**调用方必须把读端读起来**——ssh2 要等读端 `end` 之后才发 `close`，只等 close 不读 stdout 会永远等下去。

## 决定三：上传先写临时文件，收满声明的字节数才改名到位

浏览器中途关掉标签页时，宿主机那头收到的只是 stdin 的 EOF，`cat` 照样退 0。直接 `cat > 目标` 会留下一个悄悄截断的文件，比"上传失败"糟得多——用户看不出来。所以：写同目录下的 `.<名字>.<随机>.falcon-upload`，写完用 `wc -c` / `FileStream.Length` 比对请求声明的 `Content-Length`，一致才 `mv -f` / `Move-Item -Force` 到位，不一致就删掉临时文件并报 `ESHORT`。本地实现同一套规则（`ws.bytesWritten` 比 `Content-Length`）。

检查顺序：目标是文件夹（EISDIR）、上级目录不存在（ENOENT）、没有写权限（EACCES）先报，同名文件存在（EEXIST，没带 `overwrite=1` 时）最后报——前端问过用户"要覆盖吗"之后带 `overwrite=1` 重发，不该再撞上一个"覆盖也救不了"的错。前端先看目录缓存里有没有同名条目提前问（缓存可能过期），服务端的 409 是第二道；两道都问的是同一个确认框。

失败分支里 `rm` 排在 `printf` 前面：客户端已断开时 stdout 可能是关着的，`printf` 会吃 SIGPIPE 把 shell 带走，清理必须先做。即便如此，Windows 的 sshd 关通道时会把进程树整个杀掉（见 zellij 持久化那段历史），脚本自己的清理跑不到，临时文件会留下——所以每次上传前顺手把同目录里超过 6 小时的 `*.falcon-upload` 扫掉（本地与两种远端同一规则）。6 小时是"正在传的绝不会被误删"与"垃圾不会留太久"之间的折中。

`receiveRemote` 里 `pipeline(body, channel)` 失败不等于上传失败：远端脚本在前置检查失败时会立刻退出（EEXIST 等），这时 pipeline 以"写入已关闭的流"报错，真相在退出码和 stdout 的错误码里。所以先记下 pipe 错误，等 `close` 拿到退出码再下结论；请求体那头中断时 pipeline 已把通道 destroy，`close` 通常还会到，但 SSH 连接本身死了就等不到——兜一个 10 秒超时。

## 决定四：前端用 XHR 上传，进度走 toast，确认框包成 promise

`fetch` 不给上传进度，XHR 给（`xhr.upload.onprogress`），就为这一件事用 XHR。请求体就是 `File` 本身，浏览器按流发、自动带 `Content-Length`。

多个文件顺序上传而不是并发：每个文件都可能要弹一次"覆盖吗"，并发的确认框会互相盖住；SSH 链路上并发也不会更快。store 的 `askConfirm` 是回调式的、取消没有回调，顺序流程里没法"等用户答完再继续"，于是有了 `lib/confirmAsync.ts`：订阅 store，观察 `confirm` 从我们这份 spec 变成别的即视为关闭。进度用 sonner 的 `toast.loading` 就地更新成 success / error——面板本身不摆进度条，上传是偶发动作，常驻的进度区大多数时候是空的。

上传完成只重拉目标那一层（`reloadDir`）并确保它展开，不整树刷新。

## 已知取舍

- 下载没有超时：几百 MB 的文件在慢链路上要传很久，任何固定超时都会误伤；客户端断开时 fastify 会 destroy 源流，通道随之关闭，远端 `cat` 收到 SIGPIPE 退出。
- 远端文件在下载过程中被改写，`Content-Length` 与实际字节数对不上，浏览器会报下载失败。这与任何静态文件服务器一样，接受。
- 前端上传的 409 重试会把整个文件再发一遍（第一次的请求体已被服务端读掉丢弃）。目录缓存命中时提前问过，走到这条路的只有缓存过期的情况。
- Windows 远端与真实 SSH 主机上的路径实现后未在真机上验过（本地项目在 Chrome 里验过，POSIX 远端命令串用本机 `sh -c` 模拟通道验过），改动那边先在 172.16.25.134 那台机器上跑一遍。
- 不做拖拽上传。文件夹上传、删除、重命名后来补在 ADR 0009——面板从"看 + 搬运"扩成目录浏览器之后，这几件在终端里做太绕。
