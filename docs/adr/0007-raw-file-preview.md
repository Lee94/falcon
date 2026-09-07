# 原始字节路由与 HTML / 图片预览沙箱

查看 tab 原先只有两种"看"法：文本走源码视图（Markdown 另有渲染），图片由后端读回 base64 塞进 JSON 拼 data URL。HTML 文件只能看源码；图片受 2MB 文本上限限制，也没有缩放。现在后端多了一条**原始字节路由** `GET /api/projects/:id/raw/<token>/<path>`，把工作目录里的文件按它本来的 Content-Type 原样吐给浏览器；HTML 在沙箱 iframe 里直接渲染，图片用原生 `<img src>` 加载并配了缩放，Markdown 里的相对图片也顺手内联了。实现在 `files.ts`（MIME 表、`readWorkspaceBytes`）、`routes.ts`（路由与响应头）、`auth.ts`（作用域令牌）、`FileView.tsx`（`HtmlPane` / `ImagePane`）。

## 决定一：路由是路径形状，不是 `?path=`

HTML 页面里 `./assets/style.css`、`../img/logo.png` 这类相对引用由浏览器按 URL 规则相对**当前文档地址**解析。文档地址若是 `/api/projects/x/file?path=docs/index.html`，`./style.css` 会解析到 `/api/projects/x/style.css`——全错。只有把工作目录相对路径原样铺在 URL 路径里（`.../raw/<token>/docs/index.html`），相对引用才落在同一前缀下（`.../raw/<token>/docs/assets/style.css`）。这也是为什么 `<iframe>` 用 `src` 而不是 `srcdoc` + `<base>`：`src` 下文档的 `location` / `import.meta.url` / 相对导航全都是对的，`srcdoc` 只能凑合前两项。

前端拼地址时按段 `encodeURIComponent`（`lib/rawUrl.ts`）：`/` 是分隔符要留着，`#` / `?` / `%` / 空格出现在文件名里必须编码。服务端拿到的 `*` 参数由 find-my-way 解码，再走 `relSegments` 的 `..` 护栏——与 `?path=` 那条路是同一段代码，不存在第二套解析。

## 决定二：HTML 跑在 opaque origin 的沙箱里，凭据放进 URL

仓库里的 HTML 不可信：一个 clone 下来的项目、一份 agent 生成的报告，都可能带着脚本。它若与本站同源运行，就能拿着用户的登录 cookie 调 `/api/*`——开会话、跑命令、删项目，全是用户身份。所以 `<iframe sandbox>` **不给 `allow-same-origin`**：文档 origin 是 opaque 的，脚本碰不到本站的 cookie / storage，fetch `/api` 会被当跨源请求（实测 Chrome：沙箱内读 `document.cookie` 与 `localStorage` 直接抛 SecurityError，`fetch("/api/projects")` 整个失败——TypeError: Failed to fetch，连 401 都拿不到；`location.origin` 仍显示页面 URL 的 origin，别拿它判断沙箱，看 `self.origin` 才是 "null"）。`allow-scripts` 必须给，否则大半页面白屏；`allow-forms` / `allow-popups` / `allow-modals` 是页面正常交互所需，弹出的窗口仍在沙箱内（没有 `allow-popups-to-escape-sandbox`）。

代价是**浏览器不给 opaque origin 的子资源请求带 SameSite=Lax 的登录 cookie**（iframe 自身的导航请求由父页面发起，cookie 是带的；页面里的 `<link>` / `<img>` / `<script>` 不带）。开了密码的部署下这些请求全会 401。凭据只能放在 URL 里，于是有了 `Auth.rawToken(projectId)`：

- 一枚随机令牌，**只对"读这个项目的文件"有效**（`rawTokenValid` 同时核对项目 id），拿着它过不了 `isAuthenticated`——它与登录 token 是两张表；
- 会被页面里的脚本从 `location.href` 读到，但脚本本来就在读这个项目的文件，多拿到的只是"读同项目其它文件"这件它已经能做的事；
- 6 小时寿命、按项目复用（剩余寿命不足一半才换新）、`logout` 整批作废。每次 `GET /file` 都把当前令牌随 `rawBase` 带回，前端不缓存、不处理过期——刷新即换新。

`onRequest` 的 cookie 钩子对这条路由放行（路由 `config.rawToken`），路由自己验：登录 cookie 若在（用户直接在新标签页打开原始地址）也认。不需要鉴权的部署（loopback 且没设密码）下令牌形同虚设，路由行为与其它 `/api` 一致。

## 决定三：响应头是另一半护栏

- `Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-modals`——"在新标签页打开"会把这份 HTML 当**顶层页面**打开，那时没有 `<iframe sandbox>` 罩着。CSP 的 `sandbox` 指令让它在顶层照样跑在 opaque origin 里，效果与 iframe 属性一致。子资源响应上这个头无意义但无害，统一加。
- `X-Content-Type-Options: nosniff`——表里认不出的扩展名回 `application/octet-stream`，nosniff 让浏览器不把它猜成脚本或文档。
- `Cache-Control: no-store`——用户改完文件按刷新就想看到新的，这里不发 ETag。前端刷新时给 `<img>` / `<iframe>` 换 key 重挂，同一 `src` 不重挂不会重新请求。
- 截断的字节对浏览器没有意义（半张图、半个脚本），超过 `WORKSPACE_RAW_CAP`（16MB）回 413 而不是截断的 200。

MIME 表是手写的几十个扩展名（`files.ts`），不引 mime-db：web 预览会碰到的类型就那些，整张表大而无当。

## 决定四：图片只判类型不读字节，缩放在前端做

`GET /file` 对图片按 cap 0 读（POSIX `head -c 0`、PowerShell 循环 0 字节，都是合法空读），只报 mime 与大小；字节由浏览器经原始字节路由自己取——JSON 里不再有 base64，2MB 的文本上限对图片不再适用，改按 16MB 的原始上限判 too-large。

`ImagePane` 的缩放模型：`fit`（贴合窗口，但不放大小图——16px 的图标撑满屏幕只是一团马赛克）或一个具体倍率。⌘/Ctrl + 滚轮与触控板捏合缩放（Chrome 里捏合也是带 ctrlKey 的 wheel，delta 很小；按 delta 指数缩放两种设备手感都自然，且必须以 `passive: false` 自己挂监听，React 的 onWheel 拦不住浏览器页面缩放）、双击在 fit 与 1:1 之间切换、放大后拖拽平移。缩放以指针位置为锚点：先记下光标在图片上的相对位置，等新尺寸排完版（`useLayoutEffect`）再补滚动量，光标底下那个像素就不动。放大到 2 倍以上切 `image-rendering: pixelated`——那时是在看像素，插值糊成一片反而看不清。

只有 viewBox 没有固有尺寸的 SVG，各浏览器给的 `naturalWidth` 不一样：Chrome 实测按默认 300×150 套 viewBox 比例给出 150×150，缩放照常可用（矢量放大不糊）；给 0 的浏览器（Firefox）走兜底——按浏览器默认画、不摆缩放条。

## 已知取舍

- iframe 底色写死白色（`HtmlPane`），是 CLAUDE.md"界面色只准用语义 token"的例外：那是浏览器的画布默认色而不是界面色，没设背景的 HTML 在浏览器里就是白底黑字，跟着本站深色主题走会变成黑底黑字。
- 远端项目的每个子资源都是一次 SSH exec + base64 回传，几十个资源的页面首屏会慢。真到那一步再考虑在 `readWorkspaceBytes` 上加一层短暂缓存，眼下没有证据需要。
- 令牌在 URL 里，会进浏览器历史与服务端访问日志。它能做的只有读这个项目的文件，且 6 小时后作废；接受。
