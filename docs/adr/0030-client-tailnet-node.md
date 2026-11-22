# 客户端内置 Tailscale 节点：桌面与安卓各自作为一个端点接入 tailnet

状态：提议（2026-11-21），未实施。

## 背景

客户端现在连服务端完全靠系统网络。服务端在 tailnet 里（例如家里的机器只开了 Tailscale），客户端这台设备就必须先装好、登录官方 Tailscale。安卓上的 /24 扫描能找到 Tailscale 节点，也是因为官方 App 起了 tun 网卡。

这在几种场景下行不通：

- **公司电脑没有管理员权限**，装不了 tailscaled，也起不了 tun。
- **安卓同时只能开一个 VPN**。公司 VPN 或代理已经占着 VpnService，Tailscale App 就开不了。
- **不想让整台设备入网**，只想让 Falcon 自己能连到 tailnet 里的那几台服务端。

另外两件已有的东西和这次不是一回事：

- 服务端的 `GET /api/tailscale/devices`（`falcon-server/src/tailscale/`）看的是**后端那台机器**所在的 tailnet，用来添加 SSH 主机。本 ADR 管的是**客户端这台设备**自己入网。两边互不依赖。
- c1c21dd 曾在服务端用 sidecar（`native/mesh/tsnet`）嵌过 tsnet，5f6abc4 去掉了。那是服务端入网，场景和进程模型都不同，这里只沿用它钉的 tsnet 版本和「密钥不进 argv」等做法。

本 ADR 定的是：**Falcon 客户端（桌面与安卓）进程内跑一个 userspace 的 Tailscale 节点。它以一台独立设备的身份出现在 tailnet 里，客户端连服务端的 HTTP / WS 可以经它出去。它不要 root，不要 tun，也不占安卓的 VPN 名额。**

## 用户看到的

1. **设置 → Tailscale**（这是客户端本机的设置，不存服务端）：一个开关「让 Falcon 加入 Tailscale 网络」，下面是状态。
   - 第一次打开：显示「登录」按钮。桌面点了打开系统浏览器；安卓打开 Custom Tab。登录完成后自动变成「已连接」。
   - 已连接：显示本机在 tailnet 里的名字（默认 `falcon-<主机名 / 机型>`，可改）、Tailscale IP、tailnet 名，以及「断开」和「退出登录」。
   - 高级（默认折叠）：控制服务器地址（填 Headscale 时用，默认官方）；auth key（只写不读，填了就不用浏览器登录）。
2. **添加服务端 → 从 Tailscale 添加**：列出内置节点看到的设备，复用现有 Tailscale 设备行（名称、系统、在线）。正在跑 Falcon 的设备标上「Falcon」，点一下就预填地址 `http://<MagicDNS 名>:4923`。安卓上这项取代「Tailscale 节点不在同一 /24 段就得手填」那条提示。
3. **服务端条目**：走内置节点的连接在条目上带一个小标记「经 Tailscale」。这种明文 `http://` **不再弹**「非回环明文」警告，因为 WireGuard 已经加密了这一段。
4. 服务端条目多一个「网络」选项：`自动`（默认）/ `直连` / `经内置 Tailscale`。绝大多数用户不用改它。

| 节点状态 | 设置页 | 指向 tailnet 的服务端条目 |
|---|---|---|
| 关闭 | 开关关 | 按直连走（系统装了 Tailscale 就照常能连） |
| 需要登录 | 「登录」按钮 / 链接 | 连不上时提示「内置 Tailscale 尚未登录」 |
| 连接中 | 转圈 | 等节点就绪后自动重连（WS 现有的重连逻辑） |
| 已连接 | 名字 · IP · tailnet | 「经 Tailscale」标记 |
| 出错 | 错误原文 + 重试 | 同「需要登录」，文案换成错误 |

## 决定一：嵌 tsnet，编成 c-shared 动态库，用到时才 dlopen

| 方案 | 结论 |
|---|---|
| **tsnet（Go）编 c-shared，客户端 dlopen** | **采用** |
| tailscale-rs（官方 Rust 实现，2026-04 预览） | 不采用。README 明说「unstable and insecure」，密码学没审计，0.4 起还要设 `TS_RS_EXPERIMENT` 才能用。它进入稳定版后可以换成它，见下文「Rust 包装层」 |
| 上游 libtailscale（C 封装） | 不直接用。它的 `tailscale_dial` 返回一个 socketpair 的 fd，Windows 上不成立，Kotlin 也接不住。我们只需要它的一小部分，自己写一层几十行的 Go 薄壳，比跟着它的 ABI 走省事 |
| sidecar 进程（老 falcon-tsnet 的做法） | 不采用。安卓 12 起有 phantom process killer，会杀 App 起的子进程；从数据目录 exec 又受 W^X 限制。桌面上 sidecar 本可以用，但两端用同一种形态更省事 |
| 安卓 VpnService | 不采用。它占唯一的 VPN 名额、要用户授权、整机路由，正是我们想避开的 |
| 只靠官方 Tailscale 客户端 | 现状，继续支持（`直连`）。本 ADR 补的是装不了、开不了它的情况 |

为什么用动态库加 dlopen，而不是静态链接：

- **开发构建不需要 Go。** `cargo build -p falcon-desktop` / `cargo xtask android` 照旧能跑。找不到库时设置页显示「此构建未包含 Tailscale」，其余功能不受影响。
- **Go 运行时只在打开开关时才进进程。** 它有几十 MB 内存和一批线程，不用的人不该付这个代价。
- Windows 客户端是 MSVC 构建。Go 的 c-archive 只能用 mingw 链，混进 MSVC 很脆弱；c-shared 出来的 DLL 只暴露 C ABI，MSVC 进程加载没有问题。

代价与限制：

- Go 共享库**卸载不了**（dlclose 不安全）。断开只调 tsnet 的 `Close`，库留在进程里，再打开时复用。一个进程最多只有一个节点。这正好符合「一台客户端一个节点」（决定四）。
- 体积：tsnet 编出来约 25–35 MB，压缩后每个 ABI 约 10 MB。安卓只打 arm64-v8a 时 APK 会大约 10 MB，见「待定」。

## 决定二：数据面只走 tsnet 自带的回环 SOCKS5，要口令

tsnet 的 `Server.Loopback()` 会在 `127.0.0.1` 的随机端口起一个 SOCKS5 代理（以及 LocalAPI），每次启动给一份随机口令。客户端所有经 tailnet 的连接都通过这个代理出去。

**不用「每个服务端在本机回环开一个免认证的转发口」**，虽然那样客户端的网络栈一行都不用改。原因是：falcon 服务端已经不验证请求了（`auth.rs`：「能连上就允许使用」）。一个免认证的回环口就等于把服务端交给本机任意进程。安卓上别的 App 也能连 `127.0.0.1`，那就是任意命令执行。SOCKS5 的口令只放在内存里，每次启动换新，其他进程拿不到。

各端怎么接：

- **桌面 REST（reqwest）**：路由到内置节点的服务端，`reqwest::Client` 改用显式代理 `socks5h://<user>:<pass>@127.0.0.1:<port>`（reqwest 的 `socks` feature）。`h` 表示域名交给代理去解析，MagicDNS 名由 tsnet 解析。URL、Host 头、TLS 的 SNI 都不变，所以 `https://*.ts.net` 的证书照样能校验。现有的 `.no_proxy()` 只是不读环境变量，跟显式代理不冲突。
- **桌面 WS（`ws/native.rs`）**：它本来就是自己 `TcpStream::connect` 的。路由到内置节点时，改为先连代理口、做一次 SOCKS5 握手（RFC 1928 加 RFC 1929 用户名口令，CONNECT 发域名），之后 TLS 和 WS 握手不变。握手大约 60 行，自己写，或者用 reqwest 那边已经拉进来的 `tokio-socks`。
- **安卓（OkHttp，Kotlin）**：不用 `Proxy(SOCKS)`。Java 的 SOCKS5 认证要靠全局的 `Authenticator`，Android 各版本的实现也不统一。改成给这台服务端单独 `httpClient.newBuilder()`，加上两样东西：
  - 自定义 `Dns`：不真正解析，返回 `InetAddress.getByAddress(host, 占位地址)`，把原始主机名带下去。
  - 自定义 `SocketFactory`：造一个 `Socket` 子类，在 `connect(endpoint, timeout)` 里实际连 `127.0.0.1:<代理口>`，用 `endpoint.hostString` 取回原主机名，做 SOCKS5 握手，然后把这条 socket 交给 OkHttp。

  HTTP、TLS、WS 都由 OkHttp 照常叠在上面，`TerminalScreen` 和 `FalconApiClient` 的调用方不用改。

- 不用 libtailscale 式的「dial 返回 fd」：Windows 没有 socketpair，Kotlin 也接不住 fd。

## 决定三：控制面是一组收发 JSON 的 C 函数，客户端轮询

Go 薄壳（`native/tailnet/`）导出的函数尽量少，参数和返回值都是 UTF-8 JSON 字符串，ABI 以后不用再动：

| 函数 | 作用 |
|---|---|
| `ftn_start(cfg_json) -> err_json\|null` | 起节点。cfg 包含 `stateDir`、`hostname`、`controlUrl?`、`authKey?`。同一进程重复调用就是重启 |
| `ftn_status() -> json` | 返回 `state`（`stopped` / `starting` / `needs_login` / `running` / `error`）、`loginUrl?`、`self`（名字、IP、MagicDNS 后缀、tailnet 名）、`peers`（用 `falcon_proto::TailscaleDevice` 的形状）、`error?` |
| `ftn_proxy() -> json` | 返回 `{addr, user, pass}`，也就是 `Loopback()` 的结果。只在 `running` 时有值 |
| `ftn_logout()` / `ftn_stop()` | 退出登录（清掉节点身份）/ 停止 |
| `ftn_set_interfaces(json)` | 仅安卓使用，见决定六 |
| `ftn_free(ptr)` | 释放 Go 侧返回的字符串 |

- 登录 URL 不从日志里捞（老 sidecar 是这么干的）。改为用 `LocalClient().WatchIPNBus` 订阅 `BrowseToURL`，存进状态，由 `ftn_status` 返回。
- **不做 Go 调 C 的回调。** Go 回调跨 cgo、再跨 JNI 进 Kotlin，线程模型很麻烦。Rust 侧每 1 秒拉一次状态（`starting` / `needs_login` 时）或每 5 秒拉一次（`running` 时），变化了才通知界面，代价可以忽略。
- 返回的 peer 用现有的 `TailscaleDevice`（`falcon-proto/src/tailscale.rs`），这样设备行组件可以直接复用。

**Rust 包装层**是新 crate `native/crates/falcon-tailnet`：用 `libloading` 找库并加载，封装出 `Tailnet::start / status / proxy / stop / logout` 和一个状态 watch。桌面的 `falcon-client` 和安卓的 `falcon-android` 都依赖它。以后换 tailscale-rs，只换这一层。

库的查找顺序：环境变量 `FALCON_TAILNET_LIB` → 可执行文件旁边（Windows）/ `../Frameworks`（macOS 的 .app）→ 开发产物 `native/target-tailnet/<三元组>/` → 安卓直接按 soname `libfalcon_tailnet.so` 加载（在 jniLibs 里）。

## 决定四：一台客户端一个节点，按目标是否在它的 peer 里决定走不走它

- **一个进程一个节点**。桌面是一个进程多个窗口（一台服务端一个窗口，见 ADR 0015），所有窗口共用这个节点。安卓同理。
- 状态目录：桌面放在客户端数据目录下的 `tailnet/`，安卓放在 `filesDir/tailnet/`，权限 0700，里面存节点密钥。节点**不是 ephemeral**，这样重启后 IP 和名字不变，也不用重新登录。
- 配置（开关、设备名、控制服务器）存客户端 prefs（`Platform::store_pref`）。auth key 存平台的密码库（`Platform::store_password`，用一个保留 id），只写不读。安卓也一样。
- 服务端条目加一个字段 `route: auto | direct | tailnet`（桌面 `ServerProfile`、安卓 `ServerProfile` 都加，`serde(default)` 默认 `auto`，旧数据不用迁移）。

  `auto` 的判定规则是：**内置节点处于 `running`，并且 URL 的主机名能在它的 peer 里找到**（等于某个 peer 的 MagicDNS 名或短名，或者等于某个 peer 的 Tailscale IP），这时走内置节点，否则直连。只看地址段（100.64/10）不够：系统 Tailscale 也可能在线，而且连的是另一个 tailnet。

- 路由在**每次建连接时**决定（REST 的 `Client` 按路由缓存，WS 每次重连时重新判定），所以节点在中途上线或下线，下一次重连就会切过去，不用重开窗口。

## 决定五：从内置节点发现 Falcon 服务端

「从 Tailscale 添加」列出的是内置节点的 peer。对其中在线的设备，经 SOCKS 并发探测 `GET http://<名字>:4923/api/system`（与安卓局域网扫描认 Falcon 用的是同一个接口；每个超时 1.5 秒，最多同时 8 个），探到的标上「Falcon」并排在前面。默认端口 4923，也可以手填别的端口。

这只是添加时的一次性探测，不做后台轮询。

## 决定六：安卓的特殊处理

- **网卡与 DNS**：targetSdk ≥ 30 时，App 用 netlink 枚举网卡会被拒绝，Go 的 `net.Interfaces()` 会报错，tsnet 的 netmon 就判断不了网络变化。官方安卓 App 的做法是从 Java 侧把网卡列表交给 `netmon.RegisterInterfaceGetter`。我们也这么做：Kotlin 用 `ConnectivityManager` 监听默认网络，把 `LinkProperties` 里的网卡、地址和 DNS 服务器序列化后经 JNI 调 `ftn_set_interfaces`。网络切换（Wi-Fi ↔ 蜂窝）时再推一次。具体的符号名以钉住版本的源码为准。
- **生命周期**：节点跟着 App 进程走。App 进后台被冻结，节点也停；回到前台后 tsnet 自己重连 DERP 和控制面，终端 WS 走现有的重连。首版不做前台服务，不在后台保活。
- **没有 VPN 授权弹窗**，也不占 VpnService。用户已经开着别的 VPN 时，内置节点仍然能用（它的流量走那个 VPN 出去）。
- **登录**：`loginUrl` 用 Custom Tab 打开。登录完成后由轮询发现状态变成 `running`，不需要回调 deep link。

## 决定七：构建与打包

- Go 模块放在 `native/tailnet/`（`go.mod` 只依赖 `tailscale.com`，钉老 sidecar 用过的 v1.104.2 或更新的稳定版），入口 `main.go` 加 `//export`。
- `cargo xtask tailnet [--targets …] [--check|--tidy]`：Go 工具链（钉 1.27.4）跟 Zig 一样由 xtask 钉哈希下载到 `native/.cache/go/`，模块与编译缓存也在那里，不要求系统装 Go；`--check` 跑 `go vet`，`--tidy` 只在升 tailscale.com 时重写 go.sum。c-shared 要 cgo，各端的 C 工具链如下：
  - macOS：系统 clang，只编本机架构（`cargo xtask pkg` 也只打本机架构，不做 `lipo`）。Go 1.27 的目标文件按 macOS 13 编，库的部署目标定 13.0；客户端本身仍是 11.0，更老的系统上 dlopen 失败，按「库加载失败」处理。
  - Windows：`zig cc -target x86_64-windows-gnu`（Zig 已由 xtask 管理），也可以在 Windows runner 上用 mingw。
  - Android：xtask 已经找好的 NDK clang（`android.rs` 的 `find_ndk_clang_bin`），默认 arm64-v8a，跟随 `--abis`。
  - Linux 桌面：系统 cc。目前 Linux 只发服务端，Linux 客户端只用于开发。
  - 产物放到 `native/target-tailnet/<三元组>/`，用 `-trimpath -ldflags="-s -w"` 压体积。
- 打包时把库放进各端约定的位置：`cargo xtask pkg` → `Falcon.app/Contents/Frameworks/libfalcon_tailnet.dylib`，随 app 一起签名；`cargo xtask win` → 和 `falcon-desktop.exe` 同目录，Inno Setup 一并安装；`cargo xtask android` → `jniLibs/<abi>/libfalcon_tailnet.so`。
- 服务端发布物（`cargo xtask server` / `linux`）**不包含**这个库。
- `.github/workflows/package.yml` 加上 `cargo xtask tailnet` 的构建和 Go 的 `go vet`。包装 crate 的单测用一个假库（导出同名符号的小 C 文件）覆盖加载与解析逻辑，不在 CI 里真的连控制面。

## 安全

- 内置节点在 tailnet 里是**一台独立设备**，受 tailnet ACL 约束。建议在文档里给出 ACL 示例：给它打 tag（如 `tag:falcon-client`），只允许它访问服务端的 4923 端口。服务端本身没有门禁（ADR 0026 之后），所以 tailnet 里能连到 4923 端口的设备都能用它。这一点已经是事实，并非本 ADR 新引入，但客户端入网会让更多设备进入 tailnet，必须写清楚。
- 节点**不监听任何 tailnet 端口**（不调 `Listen`），只出不进。不开 `serve` / `funnel` / Taildrop，不当 exit node。
- SOCKS 口令每次启动随机生成，只存在内存里，不写日志、不落盘。auth key 只写不读，并在调完 `ftn_start` 后由 Go 侧清掉。日志里的登录 URL 截掉 query 部分。
- 状态目录的权限是 0700。退出登录时清空状态目录。

## 不做（首版）

- **服务端也用这份库入网**（服务端机器不装 tailscaled，用 tsnet `Listen` 暴露 4923）。可以复用同一个 Go 薄壳，但要另写 ADR：要考虑服务端没有门禁时的暴露面，以及 sessiond 与远端节点的路径。
- **子网路由、exit node**：tsnet 默认不接受子网路由（`RouteAll` 关）。首版只访问 tailnet 里的设备本身，以后可以在设置的「高级」里加一个开关。
- **客户端直接 SSH 到 tailnet 主机**：客户端从来不发 SSH，SSH 由服务端负责（ADR 0016 / 0027）。
- iOS：Falcon 没有 iOS 客户端。

## 待定

1. **安卓包体**：库默认打进 APK（多约 10 MB），还是分成带 / 不带 Tailscale 两个 flavor？倾向默认打进去，因为安卓正是最需要这个功能的一端。
2. **系统 Tailscale 和内置节点在同一个 tailnet 里同时在线**时，`auto` 应该偏向哪个？内核 tun 比 userspace netstack 快。倾向偏向内置节点，因为行为可预期；用户想走系统那条就把条目改成 `直连`。
3. **节点身份**：每台设备一个持久节点（本 ADR 的选择），还是做成 ephemeral（关掉开关就从 tailnet 消失）？后者 IP 每次会变，MagicDNS 名会冲突后缀。

## 实施顺序

1. `native/tailnet` 的 Go 薄壳、`cargo xtask tailnet`、`falcon-tailnet` 包装 crate（含假库单测）。
2. 桌面：`falcon-client` 加路由（REST 走显式 socks5h，WS 在 native.rs 做 SOCKS 握手）、`ServerProfile.route`、设置页 Tailscale、「从 Tailscale 添加」的探测。
3. 安卓：`falcon-android` 加 JNI（start / status / proxy / set_interfaces），Kotlin 加 `TailnetSocketFactory` + `Dns`，在 `FalconApiClient` 里按服务端选择 client，加上设置项与网卡推送。
4. 打包：pkg / win / android 带上库，CI 构建与检查。
5. 回到 `CLAUDE.md` 的 ADR 列表，把状态改成已实施。
