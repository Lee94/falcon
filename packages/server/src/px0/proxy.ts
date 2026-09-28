/**
 * px0 反代的纯函数：请求 / 响应头过滤，以及「正在启动」页（ADR 0017 决定六、七）。
 *
 * 反代只改下面几处，其余原样透传——px0 本来就认 `/px0/<id>/` 这个前缀，路径不用改写：
 * - 请求去掉 cookie / authorization：falcon 的登录令牌不该送进宿主机上的第三方进程
 *   （px0 开 -verbose 会把请求打到终端上）。
 * - 请求的 Host / Origin 换成 px0 认的样子。px0 会改东西的 POST（派 agent 编辑、
 *   commit / push、写会话、PR 评论、装语言服务器）都过它的 localPost：Host 必须是
 *   IP 或 localhost，且 Origin 的 host 必须等于 Host。经反代后 Host 是 127.0.0.1，
 *   浏览器的 Origin 却是 falcon 的地址（内网里可能是主机名），原样转发一律 403。
 *   所以**只有同源请求**（Origin 的 host 等于浏览器发来的 Host）才把 Origin 改成
 *   `http://127.0.0.1`；跨源的 Origin 原样转过去，让 px0 照旧拒掉——falcon 的 cookie
 *   是 SameSite=Lax，同站不同端口的页面仍能带着它发 POST，这道检查还有用。
 * - 响应里 CSP 的 `frame-ancestors 'none'` 改成 `'self'`（main 分支已带上，下个版本起
 *   生效），留出以后嵌进工作区窗口的余地；去掉 set-cookie——px0 不用 cookie，
 *   而它和 falcon 同源，放行等于让它能改写 falcon 的登录 cookie。
 */

import type { IncomingHttpHeaders, OutgoingHttpHeaders } from "node:http";

/** 逐跳头：只对一跳有意义，反代两侧各自管理（RFC 9110 §7.6.1） */
const HOP_BY_HOP = new Set([
  "connection",
  "keep-alive",
  "proxy-connection",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

/** `Connection: foo, bar` 里点名的头也是逐跳的 */
function connectionTokens(headers: IncomingHttpHeaders): Set<string> {
  const raw = headers.connection;
  const text = Array.isArray(raw) ? raw.join(",") : (raw ?? "");
  return new Set(
    text
      .split(",")
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean)
  );
}

/** Origin 的 host[:port]；不是合法 URL（含字面量 "null"）返回 null */
function originHost(origin: string): string | null {
  try {
    return new URL(origin).host || null;
  } catch {
    return null;
  }
}

/**
 * upstreamHost 必须是 IP 或 localhost（px0 的 localPost 只认这两种 Host），
 * 这里固定传 127.0.0.1。
 */
export function upstreamRequestHeaders(
  headers: IncomingHttpHeaders,
  upstreamHost: string
): OutgoingHttpHeaders {
  const named = connectionTokens(headers);
  const out: OutgoingHttpHeaders = {};
  for (const [key, value] of Object.entries(headers)) {
    const k = key.toLowerCase();
    if (value === undefined) continue;
    if (HOP_BY_HOP.has(k) || named.has(k)) continue;
    if (k === "host" || k === "cookie" || k === "authorization") continue;
    out[k] = value;
  }
  out.host = upstreamHost;
  const origin = typeof headers.origin === "string" ? headers.origin : undefined;
  if (origin !== undefined && headers.host && originHost(origin) === headers.host.toLowerCase()) {
    out.origin = `http://${upstreamHost}`;
  }
  return out;
}

export function rewriteCsp(value: string): string {
  return value.replace(/frame-ancestors\s+'none'/gi, "frame-ancestors 'self'");
}

export function downstreamResponseHeaders(headers: IncomingHttpHeaders): OutgoingHttpHeaders {
  const named = connectionTokens(headers);
  const out: OutgoingHttpHeaders = {};
  for (const [key, value] of Object.entries(headers)) {
    const k = key.toLowerCase();
    if (value === undefined) continue;
    if (HOP_BY_HOP.has(k) || named.has(k)) continue;
    if (k === "set-cookie") continue;
    if (k === "content-security-policy") {
      // 多条 CSP 头与逗号拼成一条等价（各条策略取交集）
      const csp: string | string[] = value;
      out[k] = Array.isArray(csp) ? csp.map(rewriteCsp).join(", ") : rewriteCsp(csp);
      continue;
    }
    out[k] = value;
  }
  return out;
}

/** 入口页的样子：正在起（带阶段），或起不来（带原因） */
export type Px0PageState =
  | { kind: "starting"; stage: Px0Stage }
  | { kind: "error"; message: string };

export type Px0Stage = "preparing" | "downloading" | "installing" | "launching";

const STAGE_TEXT: Record<Px0Stage, string> = {
  preparing: "正在连接宿主机…",
  downloading: "正在下载 px0…",
  installing: "正在把 px0 装到宿主机…",
  launching: "正在启动 px0…",
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * px0 还没起来时入口页回的 HTML。没有脚本：起着就 meta refresh 每秒刷一次，
 * 起好了刷新落到 px0 本体；起不来就停在这里显示原因，重试链接带 `?retry=1`。
 *
 * 文案写死中文：这一页由服务端直接吐出、不经 web 的 i18n，与服务端的错误文案同一口径。
 * 颜色只用系统色（Canvas / CanvasText），跟着系统明暗走——这里拿不到 falcon 的主题。
 */
export function px0StatusPage(state: Px0PageState, basePath: string, projectName: string): string {
  const title = escapeHtml(`px0 · ${projectName}`);
  const home = escapeHtml(basePath);
  const refresh =
    state.kind === "starting" ? `<meta http-equiv="refresh" content="1;url=${home}">` : "";
  const body =
    state.kind === "starting"
      ? `<p class="lead">${escapeHtml(STAGE_TEXT[state.stage])}</p>` +
        `<p class="hint">首次打开要下载并安装 px0，可能要半分钟。</p>`
      : `<p class="lead">px0 没能启动</p>` +
        `<pre>${escapeHtml(state.message)}</pre>` +
        `<p><a href="${home}?retry=1">重试</a></p>`;
  return [
    "<!doctype html>",
    '<html lang="zh-CN">',
    "<head>",
    '<meta charset="utf-8">',
    '<meta name="viewport" content="width=device-width,initial-scale=1">',
    refresh,
    `<title>${title}</title>`,
    "<style>",
    ":root{color-scheme:light dark}",
    "body{margin:0;min-height:100vh;display:grid;place-items:center;background:Canvas;color:CanvasText;",
    "font:14px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace}",
    "main{max-width:640px;padding:24px}",
    ".lead{font-size:16px;margin:0 0 8px}",
    ".hint{opacity:.6;margin:0}",
    "pre{white-space:pre-wrap;word-break:break-all;opacity:.8}",
    "a{color:LinkText}",
    "</style>",
    "</head>",
    `<body><main>${body}</main></body>`,
    "</html>",
  ]
    .filter(Boolean)
    .join("\n");
}
