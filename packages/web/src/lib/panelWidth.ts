/**
 * 左右两栏的宽度规格。设计写死了 260 / 220–420（docs/design/ui-redesign.md §4），
 * 右侧四个面板共用同一个槽位，所以用同一套数字——切面板不能改宽度，
 * 否则终端跟着 reflow。
 */
export const PANEL_WIDTH_DEFAULT = 260;
export const PANEL_WIDTH_MIN = 220;
export const PANEL_WIDTH_MAX = 420;

export type ResizeEdge = "left" | "right";

export function clampPanelWidth(
  n: number,
  min = PANEL_WIDTH_MIN,
  max = PANEL_WIDTH_MAX
): number {
  if (!Number.isFinite(n)) return PANEL_WIDTH_DEFAULT;
  return Math.min(max, Math.max(min, Math.round(n)));
}

/** localStorage 里读出来的值：缺字段 / 类型不对都回落到默认 */
export function parsePanelWidth(n: unknown): number {
  return typeof n === "number" ? clampPanelWidth(n) : PANEL_WIDTH_DEFAULT;
}

/**
 * 把手在面板哪一侧，决定拖的方向：右侧把手往右拉变宽，左侧把手往左拉变宽。
 */
export function resizePanelWidth(opts: {
  startWidth: number;
  startX: number;
  clientX: number;
  edge: ResizeEdge;
  min?: number;
  max?: number;
}): number {
  const delta = opts.clientX - opts.startX;
  const next = opts.edge === "right" ? opts.startWidth + delta : opts.startWidth - delta;
  return clampPanelWidth(next, opts.min, opts.max);
}
