/**
 * OSC 52：应用把剪贴板内容塞进 escape sequence，终端负责写到系统剪贴板。
 *
 * Claude Code / vim / tmux 的「选中即复制」都走这条。xterm.js 默认不处理，
 * 而且官方 addon 曾用裸 atob() 当 Latin-1 写剪贴板，CJK 会烂掉——这边自己解。
 *
 * 只写不读：`?` 查询会把本机剪贴板漏给 PTY，不答。
 */

/** 从 OSC 52 的 data 字段取出要写入的文本。查询 / 非法载荷返回 undefined。 */
export function osc52ClipboardText(data: string): string | undefined {
  const sep = data.indexOf(";");
  if (sep === -1) return undefined;
  const payload = data.slice(sep + 1);
  if (payload === "?") return undefined;
  return decodeOsc52Base64(payload);
}

/** base64 → UTF-8。空串是合法的「清空剪贴板」；解不开返回 undefined。 */
export function decodeOsc52Base64(b64: string): string | undefined {
  const compact = b64.replace(/\s+/g, "");
  if (!compact) return "";
  try {
    const binary = atob(compact);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i) & 0xff;
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    return undefined;
  }
}

export function writeBrowserClipboard(text: string): Promise<void> {
  if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
    return Promise.reject(new Error("clipboard unavailable"));
  }
  return navigator.clipboard.writeText(text);
}
