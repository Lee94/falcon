# 公网发布：Cloudflare Quick Tunnel

> 挂载点已由 ADR 0016 改掉：规则不再挂项目，而是挂本机或一台已保存的 SSH Host（`origin` 字段随之去掉），远端桥走主机链路；界面从右侧栏搬进设置的「中转」页。下文决定二里的「该项目的 SshLink」、决定五整节按 0016 读。

把项目里跑着的 HTTP 服务（vite / storybook / webhook 调试）发到公网，给同事或外部回调一个 HTTPS 地址。选 Cloudflare Quick Tunnel：一条命令、不用账号、不用开入站端口。

## 决定一：只做 Quick Tunnel，不做 Named Tunnel

Quick Tunnel（`cloudflared tunnel --url http://127.0.0.1:PORT`）随机分配 `*.trycloudflare.com`，进程退出地址作废。Named Tunnel 要 Cloudflare 账号、证书、DNS，适合生产；falcon 是开发工作台，预览和 webhook 才是这条路径的用户。账号绑定、自定义域名留给以后。

URL **不入库**。存下来的一定是过期的，界面上会骗人。规则本身（项目、目标、启用）持久化，后端重启后重新要一条新地址。

## 决定二：cloudflared 只在 falcon 后端本机跑

跟 meegle CLI 同一条边界（ADR 0010）：二进制、出网、进程都在跑 falcon 的那台机器上。远端宿主机上既没有 cloudflared，也不需要出网到 Cloudflare。

远端 HTTP 服务的走法：本机 `listen(0)` 接到一个临时端口，经该项目的 `SshLink.forwardOut` 打到远端 `destHost:destPort`，再把 Quick Tunnel 指到这个临时端口。SSH 断了就拆桥、标「SSH 链路断开」，链路回来再拉——和端口转发同一套 `onLinkDown` / `onLinkUp`。

本机目标不经过 SSH。本地项目只能发本机端口。

## 决定三：二进制按需下载锁定版本，不封进 SEA

cloudflared 每个平台约 20 MB，封进单文件发布会明显涨体积，而公网发布不是打开就能用的核心路径。第一次启用时从 GitHub release 拉锁定版本（`cloudflared/command.ts` 里的 `CLOUDFLARED_VERSION`）到 `<dataDir>/bin/cloudflared`：

- `FALCON_CLOUDFLARED_BIN` 显式指定优先，不校验版本；
- 已装且 `--version` 对得上就直接用；
- 下载失败才退到 PATH 上的 `cloudflared`。

darwin 资产是 `.tgz`，linux / windows 是裸二进制。升级是一次有意的发版：改常量、把 `command.ts` 顶部那几条脾气重新跑一遍。`--no-autoupdate` 关掉它自己换二进制。

不做完整性校验，理由与 Zellij 相同（ADR 0001）。

## 决定四：只代理 HTTP，Host 改写成 origin 自己的

Quick Tunnel 是 HTTP(S) 反代，TCP（postgres 等）要 Named Tunnel，v1 不做。

`--http-host-header` 按真正的 origin 写，回环一律 `localhost:port`：vite / webpack 的 `allowedHosts` 默认认 localhost，不认 `127.0.0.1`，更不认 `*.trycloudflare.com`。远端目标时 Host 仍是远端的 `localhost:destPort`，不是本机桥的临时端口——dev server 认的是它自己听的那个。

公网 URL 从两条路取，谁先到用谁：stderr 里的 `https://*.trycloudflare.com`，以及 metrics 端口上的 `GET /quicktunnel` `{"hostname":"…"}`。有的版本框打得晚、metrics 先好。

## 决定五：挂在现有转发面板，不新开一栏

右侧栏已经有文件 / 修改 / 历史 / 转发 / 飞书五格。公网发布和端口转发都是「把一个端口接到别处」，同一块面板两节：SSH 项目上面是端口转发、下面是公网发布；本地项目只有公网发布。规则都按焦点项目走。

术语见 CONTEXT.md 的 **Public Share（公网发布）**。界面文案要写明「任何拿到链接的人都能访问」——Quick Tunnel 没有访问控制。

## 验证状态

纯函数层（资产名、argv、URL / metrics JSON 解析、输入校验、删项目级联）有单测。进程拉起、Quick Tunnel 拿地址、远端桥、SSH 断线拆桥要在真机验。
