# 应用图标：内置几套可选、可上传自定义，选择按服务端存

原来的 logo 是一只金色折纸猎鹰，只有 `packages/web/public/favicon.svg` 一个源，PWA 图标和安装包的 AppIcon 都从它栅格化。现在换成「闪电鸟」系列，并且让用户在设置里挑，或者传一张自己的图。

## 决定一：选择是服务端级的，存在 settings 表

`settings.app_icon` 存选中的 id（内置 id 或 `custom`），`settings.app_icon_custom` 存自定义图的版本（内容 sha256 前 16 位），图本身在 `<dataDir>/app-icon/custom.png`。连这台服务端的所有浏览器与原生客户端看到同一个图标。

没做成每台设备各选各的（像主题那样存 localStorage）：图标的几个消费者里，PWA 清单与 iOS 主屏幕图标是浏览器**不带登录态**去服务端拉的，自定义图也得有地方放，本来就绕不开服务端。

存储值认不出（降级到没有这套图标的旧版本、手改过库）、或选了 `custom` 但图没了，一律回默认（`resolveAppIcon`）。图标永远得有一个能用的。

## 决定二：页面不等 JS 就是对的图标

- `index.html` 的 `<link rel="icon">` / `apple-touch-icon` 指向 `/api/app-icon/favicon` / `/api/app-icon/apple-touch-icon`，服务端按当前选择 302 到对应图片（跳转本身 `no-cache`）。登录页也是对的图标，没有「先闪一下默认的再换」。
- 设置里换了之后，`lib/appIcon.ts` 直接改 `<link>` 的 href，Chrome 当场换标签页图标。别的已打开的标签页、别的设备下次加载时换。
- PWA 清单（`/manifest.webmanifest`）从静态文件改成服务端现出（`webManifest()`，shared 纯函数），图标跟着选择走。路径没变，已安装的应用不用换清单地址；它们什么时候换图标取决于浏览器多久重新检查一次清单。开发时 vite 把这个路径代理到后端。

取图的几条路由（favicon / apple-touch 跳转、清单、`custom.png`）**不要登录**：鉴权钩子认路由上的 `config.publicAsset`。改选择、上传、删除照常要登录。自定义图因此对能访问到服务端的任何人可见——它只是个图标。

## 决定三：服务端不解码图片，自定义图由客户端规整好再上传

服务端没有图像库，也不想为此装一个（SEA 单文件发布，原生模块越少越好）。客户端负责把用户挑的图变成 **512×512 的 PNG**：等比缩放到长边贴边、居中、不裁切、空出的地方透明（`containRect`，web 与原生同一套取整，有对照用例）。

- web：`<img>` 解码 → canvas 画 → `toBlob("image/png")`。浏览器能解码的都行，含 SVG。
- 原生：GPUI 的解码器（各位图格式 + SVG 光栅化；SVG 按目标尺寸重画，免得 24×24 的 viewBox 放大发糊）→ `image` crate 缩放、编码。

服务端只看 PNG 签名与 IHDR：必须是正方形、边长 64–2048、不超过 2MB。上传即选中。地址带版本 `custom.png?v=<hash>`，对上了就 `immutable` 长缓存，对不上（旧页面拿着旧地址）给当前这张、不缓存。

清单里的自定义图只有一张 512 的 `purpose: any`：Chrome 的安装条件是「至少一张 ≥144px 的 any 图标」，够了；用户随手传的图不是照 maskable 安全区画的，不冒充 maskable。

## 决定四：内置图标由一份定义出全部形状，产物进仓库

`scripts/app-icons.mjs` 是全部内置图标的图形定义：底色 + 画在 1024 网格上的图形。`pnpm gen-icons` 出四种形状：

| 形状 | 文件 | 用途 |
|---|---|---|
| 圆角方块（圆角 229） | `public/icons/<id>/icon-{192,512}.png` | 标签页图标、设置里的预览、PWA any |
| maskable（图形缩到 0.84） | `public/icons/<id>/maskable-{192,512}.png` | PWA maskable，最紧的圆形遮罩（r = 40%）也切不到 |
| 满版方块 | `public/icons/<id>/apple-touch-icon.png` | iOS 自己裁超椭圆，给它圆角反而露出四个角的底色 |
| macOS 版式 | `native/.../assets/app-icons/<id>.png` | 原生 Dock（824/1024 的圆角块、四周留边、带投影） |

标签页图标用 192 的 PNG 而不是 SVG：默认图标（橙翼）是栅格插画，做成 SVG 就是一个 1MB 多的 favicon。安装包的 `AppIcon.icns` 由 `build-macos-pkg.mjs` 直接调 `app-icons.mjs` 按每档尺寸现画，不往仓库里放生成物。

id 列表在 shared 的 `APP_ICON_IDS`（顺序即设置里的顺序）、falcon-core 的 `APP_ICON_IDS`、`app-icons.mjs` 的 `ICONS` 三处。对账靠测试：web 查每个 id 在 `public/icons/<id>/` 下有全套文件、默认 id 与 `app-icons.mjs` 的 `DEFAULT_ID` 一致；原生查 falcon-core 的列表与嵌进二进制的一致、`assets/app-icons/` 下每张图都嵌进去了。

默认是**橙翼**（`emberwing`，设计稿「闪电鸟 | 品牌视觉完整设计稿」v02）。设计稿只给了 1254 见方的深色底栅格原图，原样存在 `scripts/app-icons/emberwing.png`，按稿里的规范放进深炭黑容器、原方图居中缩至 0.80。容器取 `#0F0F0F` 而不是稿里写的 `#101010`：原图外圈实测均值 rgb(15.5, 15.2, 14.9)，`#101010` 在把暗部拉亮后能看出一圈方块边。设计稿自己说明 32px 以下细节会明显损失、favicon 建议另做简化版，简化版没交付，16px 标签页图标里它就是一团橙色。有了矢量稿或简化版再换进 `app-icons.mjs`。

其余是可选项：电翼（与电光 / 深夜 / 羽白三个配色，设计稿「闪电鸟：电翼」）、鸟字、一闪（同一稿的方向 B / C）、雷鸟（参考闪电鸟画的正面展翅，512 网格放大一倍）。

## 决定五：原生客户端运行时换 Dock 图标

连上服务端（`load_all`）后取选择，`NSApplication.setApplicationIconImage` 换 Dock 图标；设置里改了当场换。内置图标嵌在二进制里（已经是 macOS 版式），自定义图从服务端取回后套版式：四角已经透明的（自带形状的 logo）原样用，满版方块缩进 824/1024 的圆角块、垫一层投影，与内置图标一致（`has_own_shape` / `mac_frame`）。

两个限制：

- **没启动时 Dock / 访达显示的是安装包里的 `AppIcon.icns`（默认图标）**。改 `.app` 自己的图标要往包里写 `Icon\r`，会弄坏签名。设置里有一行字说明这件事。
- Dock 图标整个 App 只有一个，几台服务端各开一个窗口、各选了不同图标时，最后取到 / 改过的那台说了算。

锁屏时截不到屏幕，自动化加了 `dock:<路径.tiff>` 步骤，从 AppKit 读回当前的 Dock 图标——验的是 AppKit 真收下了，不只是我们调了。
