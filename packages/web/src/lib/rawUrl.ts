/**
 * 原始字节地址。`rawBase` 来自 `GET /api/projects/:id/file` 的响应
 * （`/api/projects/<id>/raw/<token>/`），后面接工作目录相对路径。
 *
 * 逐段 encodeURIComponent 而不是整串：`/` 是路径分隔符得留着，而 `#` / `?` / `%`
 * 与空格出现在文件名里时必须编码，否则服务端拿到的是被截断或解错的路径。
 */
export function rawUrl(rawBase: string, path: string): string {
  const encoded = path
    .split("/")
    .filter((seg) => seg.length > 0)
    .map(encodeURIComponent)
    .join("/");
  return `${rawBase}${encoded}`;
}
