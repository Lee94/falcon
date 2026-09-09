/**
 * Tab 栏的纯顺序运算。Chrome 把整条 strip 当成一个可排序列表：
 * 终端 / 文件 / 差异混排，拖拽改的是这一条，不是各类型自己的数组。
 *
 * key 约定：`t:<sessionId>` / `f:<projectId>:<path>` / `d`。
 * path 里可以有冒号（相对路径极少见），所以 file key 只在 projectId 后切一次。
 */

export type StripFile = { projectId: string; path: string };

export type StripItem =
  | { kind: "terminal"; key: string; id: string }
  | { kind: "file"; key: string; projectId: string; path: string }
  | { kind: "diff"; key: string };

export const DIFF_KEY = "d";

export function termKey(id: string): string {
  return `t:${id}`;
}

export function fileKey(file: StripFile): string {
  return `f:${file.projectId}:${file.path}`;
}

export function parseStripKey(key: string): StripItem | null {
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

export function defaultStripKeys(input: {
  terminalIds: string[];
  files: StripFile[];
  hasDiff: boolean;
}): string[] {
  const keys = [
    ...input.terminalIds.map(termKey),
    ...input.files.map(fileKey),
  ];
  if (input.hasDiff) keys.push(DIFF_KEY);
  return keys;
}

/**
 * 用记住的顺序排当前可见 key。tabOrder 里没有的（刚开的）接到末尾，
 * 已经关掉的从结果里消失。
 */
export function orderedStripKeys(tabOrder: string[], visible: string[]): string[] {
  const want = new Set(visible);
  const out: string[] = [];
  for (const key of tabOrder) {
    if (!want.has(key)) continue;
    out.push(key);
    want.delete(key);
  }
  for (const key of visible) {
    if (want.has(key)) out.push(key);
  }
  return out;
}

export function moveIndex<T>(arr: T[], from: number, to: number): T[] {
  if (
    from === to ||
    from < 0 ||
    to < 0 ||
    from >= arr.length ||
    to >= arr.length
  ) {
    return arr.slice();
  }
  const next = arr.slice();
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item!);
  return next;
}

/**
 * 把可见子集的新顺序织回完整顺序：full 里属于可见集合的坑按新顺序填，
 * 其它项目的隐藏 tab 原地不动。
 */
export function weaveOrder(
  full: string[],
  visibleBefore: string[],
  visibleAfter: string[]
): string[] {
  const vis = new Set(visibleBefore);
  let i = 0;
  const woven = full.map((key) => (vis.has(key) ? visibleAfter[i++]! : key));
  if (i < visibleAfter.length) woven.push(...visibleAfter.slice(i));
  return woven;
}

/** 可见项（终端 id / 文件）按新顺序填回完整数组，隐藏项位置不变。 */
export function applyVisibleOrder<T>(
  full: T[],
  visibleAfter: T[],
  isVisible: (item: T) => boolean
): T[] {
  let i = 0;
  const next = full.map((item) => (isVisible(item) ? visibleAfter[i++]! : item));
  if (i < visibleAfter.length) next.push(...visibleAfter.slice(i));
  return next;
}

export function insertAfter(keys: string[], afterKey: string | undefined, newKey: string): string[] {
  if (keys.includes(newKey)) return keys;
  if (!afterKey) return [...keys, newKey];
  const at = keys.indexOf(afterKey);
  if (at < 0) return [...keys, newKey];
  const next = keys.slice();
  next.splice(at + 1, 0, newKey);
  return next;
}

export function replaceKey(keys: string[], from: string, to: string): string[] {
  return keys.map((key) => (key === from ? to : key));
}

export function keysToLeft(keys: string[], index: number): string[] {
  return keys.slice(0, Math.max(0, index));
}

export function keysToRight(keys: string[], index: number): string[] {
  return keys.slice(index + 1);
}

export function keysOther(keys: string[], index: number): string[] {
  return keys.filter((_, i) => i !== index);
}

/**
 * 拖着 from 的 tab 时，其它 tab 要让出的位移。
 * 被拖的那一项自己跟指针走，不算在这里。
 */
export function tabShift(from: number, to: number, index: number, draggedWidth: number): number {
  if (index === from) return 0;
  if (from < to && index > from && index <= to) return -draggedWidth;
  if (from > to && index >= to && index < from) return draggedWidth;
  return 0;
}

/**
 * 被拖 tab 的中心落到哪一格。格子边界是各 tab 原位置的中点——
 * 越过邻居中点才换位，跟 Chrome 一样，不会刚过缝就跳。
 */
export function dropIndex(centerX: number, lefts: number[], widths: number[]): number {
  if (lefts.length === 0) return 0;
  let best = 0;
  for (let i = 0; i < lefts.length; i++) {
    const mid = lefts[i]! + widths[i]! / 2;
    if (centerX >= mid) best = i;
  }
  return best;
}
