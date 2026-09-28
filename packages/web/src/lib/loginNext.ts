/**
 * 登录后回跳的地址。没登录的浏览器直接打开 px0 入口（`/px0/<项目 id>/`，原生客户端
 * 就是这么交给系统浏览器的）时，服务端把它送到 `/?next=<原地址>`，登录成功后回去。
 *
 * 只认站内的 px0 入口：`/px0/` 开头保证是同源路径，挡掉 `//evil.example`、
 * `/\evil.example`、`javascript:` 这类开放跳转。别的地址一律当没有，照常进首页。
 */
export function loginNext(search: string): string | null {
  const next = new URLSearchParams(search).get("next");
  if (!next || !next.startsWith("/px0/") || next.includes("\\")) return null;
  return next;
}
