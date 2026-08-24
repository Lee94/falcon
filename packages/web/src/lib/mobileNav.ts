/**
 * 终端区横滑切换会话的纯计算。手势判定与相邻会话查找都在这里，
 * 好让阈值和边界行为有测试钉住——手势 bug 在真机上极难复现。
 */

export interface SwipePoint {
  x: number;
  y: number;
  /** 毫秒时间戳，同一事件源内可比即可（touch 事件用 e.timeStamp） */
  t: number;
}

/**
 * 判定一次触摸是否算横滑。返回 1 = 左滑（看下一个）、-1 = 右滑（看上一个）、
 * 0 = 不是横滑。
 *
 * 阈值取向保守：终端区里长按拖动是 xterm 选区、竖向是滚动，误触发切换
 * 比漏判恶劣得多（画面整个换掉）。
 */
export function swipeDir(start: SwipePoint, end: SwipePoint): -1 | 0 | 1 {
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  if (end.t - start.t > 500) return 0; // 慢速拖动多半是选区
  if (Math.abs(dx) < 64) return 0; // 位移不够
  if (Math.abs(dy) > Math.abs(dx) * 0.6) return 0; // 斜向按滚动算
  return dx < 0 ? 1 : -1;
}

/**
 * 同项目内的相邻会话。顺序取列表序（与切换面板一致），到头返回 null
 * 不回绕——回绕会让"再滑一下"跳回第一个，方向感直接失效。
 */
export function siblingSession<T extends { id: string; projectId: string }>(
  sessions: readonly T[],
  currentId: string,
  dir: -1 | 1
): T | null {
  const current = sessions.find((s) => s.id === currentId);
  if (!current) return null;
  const mine = sessions.filter((s) => s.projectId === current.projectId);
  const i = mine.findIndex((s) => s.id === currentId);
  return mine[i + dir] ?? null;
}
