/**
 * 工作区排布：主区是从左到右的**列**，每列从上到下是若干**窗口**（pane）。
 *
 * 终端 / 文件 / 差异混排在同一套结构里，谁在哪一列哪一格，只由这里的 key 顺序决定
 * （key 约定见 lib/paneKey.ts）。纯函数、零 DOM：几何（列宽、窗口高、拖拽落点）
 * 也在这里算，组件只负责量出矩形再把结果贴回样式。
 *
 * 尺寸的两档：`basis === null` = 自适应（列按 columnWidth 的公式，窗口在列内平分剩下的
 * 高度），number = 用户拖过，按像素钉住。拖一次只钉一边（列钉左边那列、
 * 窗口钉上面那个），右边 / 下面的继续自适应——画布本来就是横向可滚的，不需要此消彼长。
 */

export interface PaneLayout {
  key: string;
  /** 窗口高度 px；null = 与同列其它自适应窗口平分 */
  basis: number | null;
}

export interface ColumnLayout {
  id: string;
  /** 列宽 px；null = 按列数自动分 */
  basis: number | null;
  panes: PaneLayout[];
  /**
   * 固定在最右：新开的窗口、对账补的新列都排在它左边，拖拽也越不过去，直到从菜单
   * 取消固定。至多一列，且**必须是数组里的最后一列**——这条不变式由下面每个会插列的
   * 函数一起维持（插列一律夹到 `pinEdge` 之前），别绕过它们直接 splice。
   */
  pinned?: boolean;
}

/** 窗口在排布里的坐标 */
export interface PaneAt {
  col: number;
  index: number;
}

/** 列宽下限：再窄终端就只剩三十来列，装不下一条命令 */
export const COLUMN_MIN_PX = 260;
/** 窗口高度下限：标题栏 + 两三行 */
export const PANE_MIN_PX = 96;

let seq = 0;

/** 列 id 只求唯一（React key 与拖拽标识），不承载任何语义 */
export function newColumnId(): string {
  seq += 1;
  return `c${Date.now().toString(36)}${seq.toString(36)}`;
}

export function column(keys: string[], basis: number | null = null): ColumnLayout {
  return { id: newColumnId(), basis, panes: keys.map((key) => ({ key, basis: null })) };
}

/** 从左到右、列内从上到下的全部 key —— 切换窗口的快捷键按这个顺序走 */
export function paneKeys(columns: ColumnLayout[]): string[] {
  return columns.flatMap((c) => c.panes.map((p) => p.key));
}

export function findPane(columns: ColumnLayout[], key: string): PaneAt | null {
  for (let col = 0; col < columns.length; col++) {
    const index = columns[col]!.panes.findIndex((p) => p.key === key);
    if (index >= 0) return { col, index };
  }
  return null;
}

/** 去掉空列。列是窗口的容器，空了就没有存在的理由（宽度也一并忘掉） */
function compact(columns: ColumnLayout[]): ColumnLayout[] {
  return columns.filter((c) => c.panes.length > 0);
}

export function removePane(columns: ColumnLayout[], key: string): ColumnLayout[] {
  return compact(
    columns.map((c) =>
      c.panes.some((p) => p.key === key)
        ? { ...c, panes: c.panes.filter((p) => p.key !== key) }
        : c
    )
  );
}

/**
 * 插进某列的某一格。key 已经在别处就是"移动"——先摘掉再插，
 * 同一个 key 绝不允许在排布里出现两次（两份 xterm 挂同一个会话会互相抢 WS）。
 */
export function insertPane(columns: ColumnLayout[], key: string, at: PaneAt): ColumnLayout[] {
  const basis = findPane(columns, key) ? paneBasis(columns, key) : null;
  const base = removePane(columns, key);
  const col = Math.min(Math.max(0, at.col), Math.max(0, base.length - 1));
  if (base.length === 0) return [column([key])];
  return base.map((c, i) => {
    if (i !== col) return c;
    const index = Math.min(Math.max(0, at.index), c.panes.length);
    const panes = c.panes.slice();
    panes.splice(index, 0, { key, basis });
    return { ...c, panes };
  });
}

/**
 * 独占一列插在第 at 列的位置（at = columns.length 就是接到最右）。
 * 有固定列时"最右"到它为止——真要钉到它右边只有 pinPane 一条路。
 */
export function insertColumn(columns: ColumnLayout[], key: string, at: number): ColumnLayout[] {
  const base = removePane(columns, key);
  const next = base.slice();
  next.splice(Math.min(Math.max(0, at), pinEdge(base)), 0, column([key]));
  return next;
}

/**
 * 新列能插到的最右位置：固定列的下标，没有固定列就是末尾。
 * 固定列永远是最后一列，所以这个下标也就是"固定区"的起点。
 */
export function pinEdge(columns: ColumnLayout[]): number {
  const i = columns.findIndex((c) => c.pinned);
  return i < 0 ? columns.length : i;
}

/** 这扇窗口是不是待在固定列里 */
export function isPinned(columns: ColumnLayout[], key: string): boolean {
  return columns.some((c) => c.pinned && c.panes.some((p) => p.key === key));
}

/**
 * 固定到最右：把这扇窗口摘出来独占一列钉在末尾。固定至多一列——两列都叫"最右"
 * 就没意义了，所以先把旧的那个标记清掉（它待的位置不动）。
 */
export function pinPane(columns: ColumnLayout[], key: string): ColumnLayout[] {
  const basis = paneBasis(columns, key);
  const base = unpinAll(removePane(columns, key));
  return [...base, { ...column([key]), panes: [{ key, basis }], pinned: true }];
}

/** 取消固定：只清标记，列不动——它本来就在最右，取消之后只是不再挡着别人 */
export function unpinAll(columns: ColumnLayout[]): ColumnLayout[] {
  return columns.some((c) => c.pinned)
    ? columns.map((c) => (c.pinned ? { ...c, pinned: false } : c))
    : columns;
}

function paneBasis(columns: ColumnLayout[], key: string): number | null {
  for (const c of columns) {
    const p = c.panes.find((x) => x.key === key);
    if (p) return p.basis;
  }
  return null;
}

/**
 * 就地换 key，位置与高度都留着。文件 / 差异的"预览"语义靠它：点另一个文件
 * 是同一扇窗口换了内容，不是新开一扇。
 */
export function replacePane(columns: ColumnLayout[], from: string, to: string): ColumnLayout[] {
  if (from === to) return columns;
  const base = to === from ? columns : removePane(columns, to);
  if (!findPane(base, from)) return base;
  return base.map((c) => ({
    ...c,
    panes: c.panes.map((p) => (p.key === from ? { ...p, key: to } : p)),
  }));
}

/**
 * 尺寸一律按 id / key 定位，不按下标：组件手上只有**可见**的那几列（别的项目的窗口
 * 被过滤掉了），下标与 store 里的全量排布对不上。
 */
export function setColumnBasis(
  columns: ColumnLayout[],
  id: string,
  basis: number | null
): ColumnLayout[] {
  return columns.map((c) => (c.id === id ? { ...c, basis } : c));
}

export function setPaneBasis(
  columns: ColumnLayout[],
  key: string,
  basis: number | null
): ColumnLayout[] {
  return columns.map((c) =>
    c.panes.some((p) => p.key === key)
      ? { ...c, panes: c.panes.map((p) => (p.key === key ? { ...p, basis } : p)) }
      : c
  );
}

/**
 * 与数据源对账：live 之外的 key 一律摘掉（会话结束、文件关了），live 里还没排布的
 * 按给定顺序各自追加成新列。
 *
 * 每次 set 之后都跑：漏掉一次，画布上就会有一扇连着死会话的窗口，或者开出来的
 * 会话找不到落脚的地方。
 */
export function syncColumns(columns: ColumnLayout[], live: string[]): ColumnLayout[] {
  const want = new Set(live);
  const kept = compact(
    columns.map((c) => ({ ...c, panes: c.panes.filter((p) => want.has(p.key)) }))
  );
  const have = new Set(paneKeys(kept));
  const missing = live.filter((k) => !have.has(k));
  if (!missing.length) return kept;
  // 补的新列也得让开固定列
  const at = pinEdge(kept);
  return [...kept.slice(0, at), ...missing.map((k) => column([k])), ...kept.slice(at)];
}

/**
 * 当前该画出来的列：只留 visible 的窗口，空列不占位。
 *
 * 不写回 store——切项目只是"看不见"，别的项目的排布（列宽、上下关系）要原样等在那，
 * 切回去还是老样子。
 */
export function visibleColumns(
  columns: ColumnLayout[],
  visible: (key: string) => boolean
): ColumnLayout[] {
  return compact(columns.map((c) => ({ ...c, panes: c.panes.filter((p) => visible(p.key)) })));
}

// ---------------- 几何 ----------------

/** 列间距，与画布上的 gap 同一个数 */
export const CANVAS_GAP_PX = 8;

/**
 * 自适应列的宽度：一列时占满；两列及以上每列 max(半屏, min(全屏, 640))——
 * 宽屏并排两列，再多的往右排，已在场的列不会因为新开第三列而被挤窄。
 * 640 = 40rem，与设计里其它"一栏内容"的上限同一个数。
 */
export function columnWidth(
  column: ColumnLayout,
  count: number,
  viewportWidth: number,
  gap = CANVAS_GAP_PX
): number {
  if (column.basis != null) return column.basis;
  if (count <= 1) return viewportWidth;
  return Math.max((viewportWidth - gap) / 2, Math.min(viewportWidth, 640));
}

export interface PaneFrame {
  key: string;
  y: number;
  height: number;
}

export interface ColumnFrame {
  id: string;
  x: number;
  width: number;
  panes: PaneFrame[];
}

export interface CanvasFrames {
  columns: ColumnFrame[];
  /** 内容总宽（含列间距），画布靠它撑出横向滚动区 */
  width: number;
}

/**
 * 排布 → 像素坐标。
 *
 * 画布不用嵌套的 flex 而是自己算坐标、把每扇窗口绝对定位上去：窗口的 DOM 必须**永远
 * 待在同一个父节点下**。xterm 的画布一旦换过父节点，渲染尺寸就错了——实测拖到另一列
 * 之后整屏空白，补一帧（fit + 重建字形）能把内容找回来，但字被拉成两倍大（画布位图与
 * CSS 尺寸的 dpr 比例对不上了）。改成只改 left/top/width/height 之后，拖窗口在 xterm
 * 眼里就只是一次 resize，而 resize 是它天天在处理的事。
 */
export function layoutFrames(
  columns: ColumnLayout[],
  viewport: { width: number; height: number },
  gap = CANVAS_GAP_PX
): CanvasFrames {
  let x = 0;
  const out: ColumnFrame[] = [];
  for (const column of columns) {
    const width = columnWidth(column, columns.length, viewport.width, gap);
    out.push({ id: column.id, x, width, panes: paneFrames(column.panes, viewport.height) });
    x += width + gap;
  }
  return { columns: out, width: Math.max(0, x - gap) };
}

/**
 * 列内各窗口的高度：钉死的按钉死的算，其余平分剩下的；除不尽的零头给最后一扇
 * 自适应窗口，免得列底留一条一像素的缝。
 */
function paneFrames(panes: PaneLayout[], height: number): PaneFrame[] {
  const fixed = panes.reduce((n, p) => n + (p.basis ?? 0), 0);
  const autos = panes.filter((p) => p.basis == null).length;
  const each = autos > 0 ? Math.max(PANE_MIN_PX, (height - fixed) / autos) : 0;
  let lastAuto = -1;
  panes.forEach((p, i) => {
    if (p.basis == null) lastAuto = i;
  });
  const out: PaneFrame[] = [];
  let y = 0;
  panes.forEach((p, i) => {
    const h =
      p.basis != null
        ? p.basis
        : i === lastAuto
          ? Math.max(PANE_MIN_PX, height - y)
          : each;
    out.push({ key: p.key, y: Math.round(y), height: Math.round(h) });
    y += h;
  });
  return out;
}

// ---------------- 拖拽落点 ----------------

/** 窗口拖到哪：落进某列的第 index 格，或在第 at 列的位置另起一列 */
export type DropSpot =
  | { kind: "into"; col: number; index: number }
  | { kind: "column"; at: number };

export interface PaneRect {
  top: number;
  bottom: number;
}

export interface ColumnRect {
  left: number;
  right: number;
  panes: PaneRect[];
}

/** 列两侧多宽算"要另起一列"。够窄才不会挡住列内插入，够宽才点得中 */
export const COLUMN_EDGE_PX = 28;

/**
 * 指针落点 → 落位。列之间的缝、最左 / 最右之外都算另起一列；落在列身上则按各窗口
 * 的中线决定插在第几格（与 Chrome 拖 tab 一样，越过邻居中点才换位）。
 */
export function dropSpot(
  point: { x: number; y: number },
  rects: ColumnRect[],
  edge = COLUMN_EDGE_PX
): DropSpot {
  if (rects.length === 0) return { kind: "column", at: 0 };
  if (point.x < rects[0]!.left) return { kind: "column", at: 0 };
  for (let i = 0; i < rects.length; i++) {
    const rect = rects[i]!;
    if (point.x > rect.right) {
      const next = rects[i + 1];
      // 落在两列之间的缝里：插一列进去
      if (!next) return { kind: "column", at: rects.length };
      if (point.x < next.left) return { kind: "column", at: i + 1 };
      continue;
    }
    const width = rect.right - rect.left;
    // 窄列上不留边缘区：整列都进不去反而更难受
    const band = Math.min(edge, width / 4);
    if (point.x - rect.left < band) return { kind: "column", at: i };
    if (rect.right - point.x < band) return { kind: "column", at: i + 1 };
    let index = 0;
    for (let j = 0; j < rect.panes.length; j++) {
      const pane = rect.panes[j]!;
      if (point.y > (pane.top + pane.bottom) / 2) index = j + 1;
    }
    return { kind: "into", col: i, index };
  }
  return { kind: "column", at: rects.length };
}

/**
 * 把落点夹到固定列左边。固定列右边不接新列，"固定在最右"才立得住；
 * 拖拽的指示线与真正落位共用它，免得松手之后窗口跳到别的地方去。
 */
export function clampSpot(columns: ColumnLayout[], spot: DropSpot): DropSpot {
  if (spot.kind !== "column") return spot;
  const edge = pinEdge(columns);
  return spot.at > edge ? { kind: "column", at: edge } : spot;
}

/**
 * 把拖拽落点应用到排布上。
 *
 * 落回原地（同列同格、或从独占列拖到自己那一列的缝里）返回原数组，调用方据此
 * 跳过一次 set——拖着没动也重排一遍会让终端白白量一次尺寸。
 */
export function applyDrop(
  columns: ColumnLayout[],
  key: string,
  raw: DropSpot
): ColumnLayout[] {
  const from = findPane(columns, key);
  if (!from) return columns;
  const spot = clampSpot(columns, raw);
  const alone = columns[from.col]!.panes.length === 1;
  if (spot.kind === "column") {
    // 本来就独占一列，拖到自己左右两条缝里等于没动
    if (alone && (spot.at === from.col || spot.at === from.col + 1)) return columns;
    const at = alone && spot.at > from.col ? spot.at - 1 : spot.at;
    return insertColumn(columns, key, at);
  }
  if (spot.col === from.col && (spot.index === from.index || spot.index === from.index + 1)) {
    return columns;
  }
  // 摘掉自己之后：同列里排在后面的格子上移一格；独占的那一列整个消失，
  // 它右边的列跟着左移一格
  const col = alone && spot.col > from.col ? spot.col - 1 : spot.col;
  const index =
    spot.col === from.col && spot.index > from.index ? spot.index - 1 : spot.index;
  return insertPane(columns, key, { col, index });
}

/**
 * 把可见列上量出来的落点翻成全量排布的坐标。
 *
 * 画布画的是 visibleColumns 的结果，而 store 里存着所有项目的列；两边的下标只在
 * "当前项目就是全部"时才碰巧一致。转换一律认 id 与 key，不认下标。
 */
export function resolveSpot(
  columns: ColumnLayout[],
  visible: ColumnLayout[],
  spot: DropSpot
): DropSpot {
  if (spot.kind === "column") {
    const anchor = visible[spot.at];
    if (!anchor) return { kind: "column", at: columns.length };
    const at = columns.findIndex((c) => c.id === anchor.id);
    return { kind: "column", at: at < 0 ? columns.length : at };
  }
  const vcol = visible[spot.col];
  const col = vcol ? columns.findIndex((c) => c.id === vcol.id) : -1;
  if (!vcol || col < 0) return { kind: "column", at: columns.length };
  const panes = columns[col]!.panes;
  // 落在可见的第 index 扇窗口**之前**；越过最后一扇就是列尾
  const anchorKey = vcol.panes[spot.index]?.key;
  const index = anchorKey ? panes.findIndex((p) => p.key === anchorKey) : -1;
  return { kind: "into", col, index: index < 0 ? panes.length : index };
}

// ---------------- 尺寸 ----------------

export function clampColumnWidth(px: number, max = Number.POSITIVE_INFINITY): number {
  if (!Number.isFinite(px)) return COLUMN_MIN_PX;
  return Math.round(Math.min(Math.max(px, COLUMN_MIN_PX), Math.max(COLUMN_MIN_PX, max)));
}

export function clampPaneHeight(px: number, max: number): number {
  if (!Number.isFinite(px)) return PANE_MIN_PX;
  return Math.round(Math.min(Math.max(px, PANE_MIN_PX), Math.max(PANE_MIN_PX, max)));
}

/**
 * 拖列内分隔线时，上面那个窗口最多能长到多高：列高扣掉它上面已钉死的、
 * 以及它下面每个窗口至少要留的那点高度。
 */
export function paneMaxHeight(opts: {
  columnHeight: number;
  above: number;
  below: number;
}): number {
  return Math.max(PANE_MIN_PX, opts.columnHeight - opts.above - opts.below * PANE_MIN_PX);
}
