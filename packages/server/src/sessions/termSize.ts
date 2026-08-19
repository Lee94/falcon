/**
 * 接回时的终端尺寸。
 *
 * 客户端已经拒绝把未量好的 80×24 发给 PTY（见 web termFit），但服务端
 * liveEntry 以前硬编码 80×24，ensureAttached 会立刻按这个尺寸 attach。
 * Zellij 跟着 SIGWINCH，grok / vim / htop 被挤成 24 行，状态就没了。
 */

export interface TermSize {
  cols: number;
  rows: number;
}

export function parseStoredTermSize(cols: unknown, rows: unknown): TermSize | null {
  if (!Number.isInteger(cols) || !Number.isInteger(rows)) return null;
  const c = cols as number;
  const r = rows as number;
  if (c < 2 || r < 1) return null;
  return { cols: c, rows: r };
}

export function fallbackTermSize(size: TermSize | null): TermSize {
  return size ?? { cols: 80, rows: 24 };
}

/**
 * 一个 Viewer 连上来时要不要立刻 attach。
 *
 * 即使库里存了上次尺寸，也等这条连接自己报格子：xterm 在 fit 之前是 80×24，
 * 先 attach 再 replay，205 列的 dump-screen 会在窄屏上折行，TUI 看起来像没恢复。
 * 库里的尺寸只给「概览里点接回 / SSH 自动重连」这种当时没有新 Viewer 在量格子的路径。
 */
export function decideViewerAttach(opts: {
  durable: boolean;
  hasBackend: boolean;
}): "hello" | "wait-size" | "dead" {
  if (opts.hasBackend) return "hello";
  if (!opts.durable) return "dead";
  return "wait-size";
}
