/**
 * 窗口（pane）的 key 约定：`t:<sessionId>` / `f:<projectId>:<path>` / `d`。
 *
 * 工作区的排布（lib/layout.ts）只搬运这一个字符串，具体是终端、文件还是差异，
 * 靠解析 key 得到——列里混排三种视图，再给每种各存一份顺序必然漂移。
 *
 * path 里可以有冒号（相对路径里极少见但合法），所以 file key 只在 projectId 后切一次。
 */

export type PaneFile = { projectId: string; path: string };

export type PaneItem =
  | { kind: "terminal"; key: string; id: string }
  | { kind: "file"; key: string; projectId: string; path: string }
  | { kind: "diff"; key: string };

/** 差异只有一个（就地替换的预览语义），不带参数 */
export const DIFF_KEY = "d";

export function termKey(id: string): string {
  return `t:${id}`;
}

export function fileKey(file: PaneFile): string {
  return `f:${file.projectId}:${file.path}`;
}

export function parsePaneKey(key: string): PaneItem | null {
  if (key === DIFF_KEY) return { kind: "diff", key };
  if (key.startsWith("t:")) {
    const id = key.slice(2);
    return id ? { kind: "terminal", key, id } : null;
  }
  if (key.startsWith("f:")) {
    const rest = key.slice(2);
    const cut = rest.indexOf(":");
    if (cut <= 0) return null;
    const projectId = rest.slice(0, cut);
    const path = rest.slice(cut + 1);
    if (!path) return null;
    return { kind: "file", key, projectId, path };
  }
  return null;
}

/** 终端 key 里的会话 id；不是终端就给 null */
export function paneSessionId(key: string): string | null {
  const item = parsePaneKey(key);
  return item?.kind === "terminal" ? item.id : null;
}
