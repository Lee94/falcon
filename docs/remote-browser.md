# 远端浏览器（Neko / WebRTC）

侧边栏的地球按钮会打开一个独立窗口。Falcon 只反代 Neko 的页面、登录 cookie 与 WebRTC 信令；网页画面、音频与控制数据走 WebRTC，浏览器进程和网站请求都发生在目标主机上。

当前第一版面向 Linux Server。macOS / Windows 目标机保留同一入口，但需要另行部署等价的服务端；不要把它们当成已支持的目标。

## 1. 在目标 Linux 主机部署 Neko

下面示例用 Firefox，保存浏览器登录资料，并让 Falcon 管理访问。把 `dev-linux` 换成 Falcon 配置里的 target `id`，二者必须完全一致。

```yaml title="docker-compose.yaml"
services:
  neko:
    image: "ghcr.io/m1k1o/neko/firefox:latest"
    restart: "unless-stopped"
    shm_size: "2gb"
    ports:
      # Falcon 只通过 SSH 连这个端口；不要绑定 0.0.0.0
      - "127.0.0.1:18080:8080"
      # WebRTC 媒体端口必须能被 Falcon 客户端直接访问，或改用 TURN
      - "59000:59000/udp"
      - "59000:59000/tcp"
    volumes:
      # 网站 Cookie / 登录态所在目录；Firefox 示例，换成其它浏览器时路径不同
      - "./firefox-profile:/home/neko/.mozilla/firefox/profile.default"
      - "./neko-data:/opt/neko"
    environment:
      NEKO_SERVER_PATH_PREFIX: "/remote-browser/dev-linux"
      NEKO_DESKTOP_SCREEN: "1920x1080@30"

      # Falcon 已经做登录；Neko 不再加第二层密码
      NEKO_MEMBER_PROVIDER: "noauth"
      NEKO_SESSION_COOKIE_ENABLED: "true"
      NEKO_SESSION_COOKIE_NAME: "NEKO_SESSION"
      NEKO_SESSION_COOKIE_SECURE: "false"
      NEKO_SESSION_COOKIE_HTTP_ONLY: "true"
      NEKO_SESSION_COOKIE_PATH: "/remote-browser/dev-linux/"
      NEKO_SESSION_FILE: "/opt/neko/sessions.json"

      # 客户端能直接连到目标主机时，用一个 TCP + UDP 复用端口
      NEKO_WEBRTC_TCPMUX: "59000"
      NEKO_WEBRTC_UDPMUX: "59000"
      # 填目标主机对客户端可见的 IP / DNS 名（局域网、VPN 或公网地址）
      NEKO_WEBRTC_NAT1TO1: "<客户端可访问的目标主机地址>"
```

```bash
docker compose up -d
```

`noauth` 不是把实例公开：HTTP 只绑回环地址，只有经 Falcon 鉴权后的请求会到达它；`59000` 是 WebRTC 媒体口，不提供管理 API。

如果网站登录态要跨容器重启保留，浏览器 profile volume 必须配置正确：Firefox 是上面的路径；Chromium / Chrome 通常是 `/home/neko/.config/<浏览器目录>`，以对应镜像文档为准。

## 2. Falcon 目标

不配置也能用默认值：只要 `remote-browser.json` 不存在，Falcon 会临时生成一个本机目标 `local`，以及每台绑定了项目的 SSH Host 各一个目标，端口都按 `18080`。SSH 目标 id 由主机名转成 URL 安全的小写形式（例如 `Dev Linux` → `dev-linux`；重名时会追加稳定短哈希）。**这只是省掉映射文件，不会替你安装或启动 Neko**；目标主机仍要已有 Neko 监听自己的 `127.0.0.1:18080`，并且 `server.path_prefix` 最后一段要与这个目标 id 一致。

需要改端口、固定 id，或只允许部分主机时，在 Falcon 服务端数据目录创建 `remote-browser.json`：

```json title="<Falcon 数据目录>/remote-browser.json"
{
  "targets": [
    {
      "id": "dev-linux",
      "hostId": "<Falcon 中已保存 SSH Host 的 id>",
      "port": 18080
    },
    {
      "id": "local",
      "port": 18080
    }
  ]
}
```

规则：

- `id`：小写字母或数字开头，可含 `-` / `_`，最长 48；它同时是 URL 与 Neko `server.path_prefix` 的最后一段。
- `hostId`：已保存 SSH 主机的 id。SSH 项目必须绑定该主机；没绑定主机的旧项目不会静默落到本机。
- 不带 `hostId` 的 target 表示 Falcon 服务端本机。
- `port`：目标主机 `127.0.0.1` 上 Neko 的端口，不是公网地址。配置文件只允许选主机与端口，不能写任意 URL。
- 每台主机只能有一个 target；SSH 项目找不到对应 target 会显示配置说明，不回退到本机。
- 文件每次打开时读取，改完不用重启 Falcon。**一旦这个文件存在，就完全以它为准，不再补默认目标**。

## 3. WebRTC 网络要求

HTTP 反代只负责页面与 `/api/ws` 信令；**WebRTC 媒体不经 SSH 转发**。所以客户端必须能访问 `NEKO_WEBRTC_NAT1TO1` 所指地址上的 TCPMUX / UDPmux 端口（上例都是 `59000`），或者用 TURN。

- 同一局域网 / VPN：把 `NAT1TO1` 设成目标主机的 LAN/VPN IP，并放行 `59000/tcp` 与 `59000/udp`。
- 公网 NAT：`NAT1TO1` 填公网地址，并在路由器 / 防火墙上按原端口映射 TCP 与 UDP。
- 只有 SSH 可达、没有直连端口：需要一台双方都能访问的 TURN 服务，并配置 `NEKO_WEBRTC_ICESERVERS_BACKEND` 与 `NEKO_WEBRTC_ICESERVERS_FRONTEND`；Falcon 不会替你临时打开防火墙或做 UDP 转发。

## 4. 安全边界

- `/remote-browser/*` 使用 Falcon 登录 cookie；未登录的独立窗口先显示 Falcon 自带登录页。
- Falcon 只把 `NEKO_SESSION` cookie 转给 Neko，并把返回的 cookie 固定在 `/remote-browser/<target-id>/`；`falcon_token`、`Authorization`、`Forwarded` 与 `X-Forwarded-*` 都不会发给上游。
- WebSocket 与修改型请求会检查 `Origin` 与 `Host` 一致，阻止从别的站点借登录态打进来。
- Neko 的 HTTP 端口必须只监听 `127.0.0.1`。Falcon 不信任客户端传入的端口或地址。
- 远端浏览器窗口导航只放行 `/remote-browser/` 前缀，外部链接交给系统浏览器；项目原始 HTML 预览仍保持原来的无痕、只读边界。

## 5. 排错

- 打开后显示配置说明：检查 `remote-browser.json` 的 `id`、`hostId`、`port`，以及 SSH 项目是否绑定主机。
- 页面能加载但画面一直连接中：多半是 WebRTC 媒体不可达；检查 `NAT1TO1`、`59000` 的 TCP/UDP 放行，或配置 TURN。
- Neko 页面出现登录表单：确认 `NEKO_MEMBER_PROVIDER=noauth`、cookie 配置与路径前缀一致。
- 网站登录重启后丢失：检查浏览器 profile volume 路径，而不是 Falcon 的 cookie。
