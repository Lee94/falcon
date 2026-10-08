/**
 * cloudflared Quick Tunnel 的命令构造与输出解析。
 *
 * 纯函数，零 I/O。CLI 只在 falcon 后端本机 spawn（远端 HTTP 服务先经 SSH
 * 本地转发接到本机），直接 spawn 不经 shell，argv 数组没有转义问题。
 *
 * 脾气（2026.10.0 实测 / 2026.9.1 文档与源码对过）：
 * - Quick Tunnel 是 `cloudflared tunnel --url http://127.0.0.1:PORT`，不需要
 *   Cloudflare 账号。得到的是随机 `*.trycloudflare.com`，进程一退出 URL 作废。
 * - 公网 URL 打在 stderr 的 ASCII 框里；metrics 端口上还有 `/quicktunnel`
 *   `{"hostname":"…"}`（hostname 可能不带 https://）。两条路都认。
 * - `--no-autoupdate`：我们锁定版本，不要让它自己把 ~/.falcon/bin 里的二进制换掉。
 * - `--http-host-header`：vite / webpack 一类 dev server 默认只认 localhost，
 *   公网 Host 是 trycloudflare.com 时会 403。回环目标一律改写成 localhost。
 */

export const CLOUDFLARED_VERSION = "2026.10.0";

export const DEFAULT_BASE_URL = "https://github.com/cloudflare/cloudflared/releases/download";

/** 日志里那条公网地址。只认 trycloudflare.com，避免把别的 https 链接当隧道。 */
export const QUICK_TUNNEL_URL_RE = /https:\/\/[a-z0-9-]+\.trycloudflare\.com/i;

const METRICS_ADDR_RE = /Starting metrics server on (127\.0\.0\.1:\d+)/i;

export interface CloudflaredAsset {
  /** GitHub release 资产名 */
  name: string;
  /** darwin 是 tgz，linux / windows 是裸二进制 */
  kind: "binary" | "tgz";
}

export interface TunnelArgs {
  /** cloudflared 要打的本机 origin，已是 `http://host:port` */
  originUrl: string;
  /** 发给 origin 的 Host。回环时是 localhost:port */
  httpHostHeader: string;
  /** metrics 监听，形如 127.0.0.1:41234 */
  metrics: string;
}

/**
 * 本平台对应的官方资产。没有构建返回 null，调用方降级为「架构不支持」。
 * 命名跟 GitHub release 走：darwin/linux 的 x64 叫 amd64，不是 x64。
 */
export function cloudflaredAsset(
  platform: NodeJS.Platform = process.platform,
  arch: string = process.arch
): CloudflaredAsset | null {
  if (platform === "darwin") {
    if (arch === "arm64") return { name: "cloudflared-darwin-arm64.tgz", kind: "tgz" };
    if (arch === "x64") return { name: "cloudflared-darwin-amd64.tgz", kind: "tgz" };
    return null;
  }
  if (platform === "linux") {
    if (arch === "arm64") return { name: "cloudflared-linux-arm64", kind: "binary" };
    if (arch === "x64") return { name: "cloudflared-linux-amd64", kind: "binary" };
    return null;
  }
  if (platform === "win32") {
    if (arch === "x64") return { name: "cloudflared-windows-amd64.exe", kind: "binary" };
    return null;
  }
  return null;
}

export function downloadUrl(
  asset: CloudflaredAsset,
  version = CLOUDFLARED_VERSION,
  baseUrl = DEFAULT_BASE_URL
): string {
  return `${baseUrl.replace(/\/+$/, "")}/${version}/${asset.name}`;
}

/** `cloudflared --version` → 2026.10.0。对不上就当没装好，触发重下。 */
export function parseCloudflaredVersion(text: string): string | null {
  const m = text.match(/cloudflared version\s+(\d+\.\d+\.\d+)/i);
  return m?.[1] ?? null;
}

export function originUrl(host: string, port: number): string {
  const h = host.includes(":") ? `[${host}]` : host;
  return `http://${h}:${port}`;
}

/**
 * 回环写成 localhost：vite 的 allowedHosts 默认含 localhost、不含 127.0.0.1
 * 当 Host，更不含 trycloudflare.com。
 */
export function httpHostHeader(host: string, port: number): string {
  const loopback = host === "127.0.0.1" || host === "localhost" || host === "::1";
  const h = loopback ? "localhost" : host.includes(":") ? `[${host}]` : host;
  return `${h}:${port}`;
}

export function tunnelArgs(input: TunnelArgs): string[] {
  return [
    "tunnel",
    "--no-autoupdate",
    "--url",
    input.originUrl,
    "--http-host-header",
    input.httpHostHeader,
    "--metrics",
    input.metrics,
  ];
}

export function extractQuickTunnelUrl(text: string): string | null {
  const m = text.match(QUICK_TUNNEL_URL_RE);
  if (!m) return null;
  return m[0].replace(/\/+$/, "");
}

export function extractMetricsAddr(text: string): string | null {
  const m = text.match(METRICS_ADDR_RE);
  return m?.[1] ?? null;
}

/** `/quicktunnel` 的 JSON。hostname 有时是裸域名，有时已经带 https://。 */
export function parseQuickTunnelMetrics(body: string): string | null {
  try {
    const json = JSON.parse(body) as { hostname?: unknown };
    if (typeof json.hostname !== "string" || !json.hostname.trim()) return null;
    const raw = json.hostname.trim().replace(/^https:\/\//i, "").replace(/\/+$/, "");
    if (!/^[a-z0-9-]+\.trycloudflare\.com$/i.test(raw)) return null;
    return `https://${raw}`;
  } catch {
    return null;
  }
}

/** 进程退出时给用户看的最后几行，剥空行、截长度。 */
export function lastLogLines(text: string, n = 4, max = 280): string {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  const tail = lines.slice(-n).join(" · ");
  if (tail.length <= max) return tail;
  return tail.slice(tail.length - max);
}
