/**
 * Markdown 文档里链接目标的解析。纯函数，供 Markdown 预览用。
 */

/**
 * 能直接交给浏览器的地址。
 *
 * 白名单而不是黑名单：`javascript:` 只是最有名的那个，`data:text/html` 同样能
 * 执行脚本。认不出的一律降级成纯文本，代价只是少一个可点的链接。
 */
export function externalHref(href: string): string | null {
  const s = href.trim();
  return /^(https?:\/\/|mailto:)/i.test(s) ? s : null;
}

/**
 * 文档里的相对链接 → 工作目录相对路径，可以直接喂给查看 tab。
 *
 * 跑出工作目录（`../` 太多）返回 null：后端那道护栏会拒绝，与其让用户点出一个
 * 报错，不如在这里就不给点。锚点与 query 一并丢掉——查看 tab 里没有它们的语义。
 *
 * href 是 URL 而不是路径：Markdown 里带空格的文件名只能写成 `img%20dir/a.png`
 * （或用 <> 包起来），marked 原样给出。每段 decodeURIComponent 还原成真实文件名，
 * 之后再交给 rawUrl 编码或喂给查看 tab 才不会二次编码成 `%2520`。解不动的
 * （孤零零的 `%`）按原文保留——那多半本来就是文件名的一部分。
 */
export function resolveRel(dir: string, href: string): string | null {
  const clean = href.split("#")[0]!.split("?")[0]!.trim();
  if (!clean) return null;
  // 绝对路径指的是宿主机的根，不在工作目录里，给不出来
  if (clean.startsWith("/")) return null;
  const segs = dir ? dir.split("/").filter(Boolean) : [];
  for (const raw of clean.split("/")) {
    const part = decodeSegment(raw);
    if (!part || part === ".") continue;
    if (part === "..") {
      if (segs.length === 0) return null;
      segs.pop();
      continue;
    }
    segs.push(part);
  }
  return segs.length ? segs.join("/") : null;
}

function decodeSegment(seg: string): string {
  try {
    return decodeURIComponent(seg);
  } catch {
    return seg;
  }
}
