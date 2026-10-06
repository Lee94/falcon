/**
 * 工作区排布：主区是从左到右的**列**，每列从上到下是若干**窗口**（pane）；列再分到
 * 一块块**画布**上（`ColumnLayout.canvas`），一次只显示一块，一块就是一屏。
 *
 * 终端 / 文件 / 差异混排在同一套结构里，谁在哪一列哪一格，只由这里的 key 顺序决定
 * （key 约定见 lib/paneKey.ts）。纯函数、零 DOM：几何（列宽、窗口高、拖拽落点）
 * 也在这里算，组件只负责量出矩形再把结果贴回样式。
 *
 * 画布不横向滚动：一块画布上的列正好铺满视口宽，排不下（canvasFits）的新列另起一块
 * 画布（assignCanvases）。见 docs/adr/0012 的「多画布」一节。
 *
 * 尺寸的两档：`basis === null` = 自适应（列平分画布上剩下的宽，窗口在列内平分剩下的
 * 高度），number = 用户拖过，按像素钉住。拖一次只钉一边（列钉左边那列、
 * 窗口钉上面那个），右边 / 下面的跟着让位。
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
   *
   * 有多块画布时固定列**每块都有**：它接在每块画布的最右（canvasGroups），所以它的
   * `canvas` 不看。
   */
  pinned?: boolean;
  /**
   * 所在画布的 id。同一块画布的列在数组里保持连续（插列的函数一起维持，与 pinned 同理）。
   * undefined = 还没分：对账补进来的、取消固定的、旧版本落盘的——由 assignCanvases 按
   * "放得下就跟左边那一列同一块，放不下另起一块"补上。
   */
  canvas?: string;
}

/** 窗口在排布里的坐标 */
export interface PaneAt {
  col: number;
  index: number;
}

/** 列宽下限：再窄终端就只剩三十来列，装不下一条命令 */
export const COLUMN_MIN_PX = 260;
/**
 * 一列"排得下"的宽：自适应的列平分下来不到这么宽，新开的列就另起一块画布，而不是
 * 把已在场的挤窄。560 ≈ 默认字号下 70 列终端——13 寸笔记本去掉侧栏还能并排两列，
 * 宽屏并排两到三列。
 */
export const COLUMN_FIT_PX = 560;
/** 窗口高度下限：标题栏 + 两三行 */
export const PANE_MIN_PX = 96;

let seq = 0;

/** 列 id 只求唯一（React key 与拖拽标识），不承载任何语义 */
export function newColumnId(): string {
  seq += 1;
  return `c${Date.now().toString(36)}${seq.toString(36)}`;
}

/** 画布 id 同理，只求唯一；它要落盘（同一块画布上有哪些列得活过刷新） */
export function newCanvasId(): string {
  seq += 1;
  return `v${Date.now().toString(36)}${seq.toString(36)}`;
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
 *
 * canvas 给了就落在那块画布上（拖拽落点、挪到某块画布：用户指了地方，挤也认）；
 * 不给就是还没分，等 assignCanvases 按排不排得下来定。
 */
export function insertColumn(
  columns: ColumnLayout[],
  key: string,
  at: number,
  canvas?: string
): ColumnLayout[] {
  const base = removePane(columns, key);
  const next = base.slice();
  const col = column([key]);
  next.splice(Math.min(Math.max(0, at), pinEdge(base)), 0, canvas ? { ...col, canvas } : col);
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

/**
 * 取消固定：只清标记，列不动——它本来就在最右，取消之后只是不再挡着别人。
 * 它原先在每块画布上都有，取消之后归哪块重新分（assignCanvases：最后一块放得下就进去，
 * 放不下另起一块）。
 */
export function unpinAll(columns: ColumnLayout[]): ColumnLayout[] {
  return columns.some((c) => c.pinned)
    ? columns.map((c) => (c.pinned ? { ...withoutCanvas(c), pinned: false } : c))
    : columns;
}

function withoutCanvas(c: ColumnLayout): ColumnLayout {
  const { canvas: _, ...rest } = c;
  return rest;
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
 * 按给定顺序各自追加成新列。新列还没分画布，由 assignCanvases 按排不排得下去分。
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

// ---------------- 画布 ----------------

/** 一块画布：它的列按排布顺序，固定列接在最右 */
export interface CanvasGroup {
  id: string;
  columns: ColumnLayout[];
}

/**
 * 把（已按项目过滤过的）可见列分成一块块画布。画布按它第一列在排布里的先后排，块内
 * 按排布顺序；固定列接在**每一块**的最右——它就是为了"切到哪块都在"才固定的。只有
 * 固定列时它自己算一块。
 *
 * 认 id 分组而不是认连续段：插列的函数都维持着"同一块的列连续"，但这里不靠它，
 * 万一断开了也只是块内顺序有点怪，不会冒出两块同 id 的画布。还没分画布的列跟着左边
 * 那一列走（store 量到画布宽之后会用 assignCanvases 正式分好，这里只是不让它们无处可去）。
 */
export function canvasGroups(columns: ColumnLayout[]): CanvasGroup[] {
  const groups: CanvasGroup[] = [];
  const byId = new Map<string, CanvasGroup>();
  let prev: CanvasGroup | null = null;
  let pinned: ColumnLayout | null = null;
  for (const c of columns) {
    if (c.pinned) {
      pinned = c;
      continue;
    }
    const id: string = c.canvas ?? prev?.id ?? c.id;
    let group = byId.get(id);
    if (!group) {
      group = { id, columns: [] };
      byId.set(id, group);
      groups.push(group);
    }
    group.columns.push(c);
    prev = group;
  }
  if (!pinned) return groups;
  if (groups.length === 0) return [{ id: pinned.canvas ?? pinned.id, columns: [pinned] }];
  return groups.map((g) => ({ ...g, columns: [...g.columns, pinned!] }));
}

/** 按画布排好的可见列（画布一块接一块，固定列在最后）：切窗口的快捷键按它走 */
export function orderByCanvas(columns: ColumnLayout[]): ColumnLayout[] {
  const groups = canvasGroups(columns);
  const pinned = columns.find((c) => c.pinned);
  const ordered = groups.flatMap((g) => g.columns.filter((c) => !c.pinned));
  return pinned ? [...ordered, pinned] : ordered;
}

/**
 * 正在显示第几块：活动窗口所在的那块；活动窗口在固定列里（每块都有它）或者不在场，
 * 就沿用 remembered（上一次显示的那块），它也不在了就第一块。
 */
export function canvasIndex(
  groups: CanvasGroup[],
  activeKey: string | null,
  remembered: string | null
): number {
  if (activeKey) {
    const i = groups.findIndex((g) =>
      g.columns.some((c) => !c.pinned && c.panes.some((p) => p.key === activeKey))
    );
    if (i >= 0) return i;
  }
  const j = remembered ? groups.findIndex((g) => g.id === remembered) : -1;
  return j >= 0 ? j : 0;
}

/**
 * 这几列（含固定列）摆在 width 宽的画布上排不排得下：钉了宽的按钉的算，自适应的
 * 按 COLUMN_FIT_PX 算。一列永远排得下。
 */
export function canvasFits(columns: ColumnLayout[], width: number, gap = CANVAS_GAP_PX): boolean {
  if (columns.length <= 1) return true;
  const need = columns.reduce((n, c) => n + (c.basis ?? COLUMN_FIT_PX), 0);
  return need + gap * (columns.length - 1) <= width;
}

/**
 * 给还没分画布的可见列分画布——"排不下就自动新开一块画布"就在这里。
 *
 * 按排布顺序逐列：跟它左边最近的那一列（看得见、已分好）同一块，前提是加上它之后那块
 * 还排得下（canvasFits，含固定列）；排不下就另起一块新画布，并挪到左边那块的最后一列
 * 后面，保持同一块的列连续——这样新画布就紧跟在原来那块后面。左边没有列的另起一块。
 *
 * 只分 visible 的：别的项目补进来的列，等切过去看得见了再按那时的画布分。画布宽还没
 * 量出来（width <= 0）什么也不做。没东西可分时返回原数组。
 *
 * 分过的不再动：窗口缩窄了、邻居关掉了，已在场的列都待在原来那块（窄了就挤一挤，
 * 见 columnWidths），不会在画布之间跳来跳去。
 */
export function assignCanvases(
  columns: ColumnLayout[],
  visible: (key: string) => boolean,
  width: number,
  gap = CANVAS_GAP_PX
): ColumnLayout[] {
  if (!(width > 0)) return columns;
  const shown = (c: ColumnLayout) => c.panes.some((p) => visible(p.key));
  if (!columns.some((c) => !c.pinned && c.canvas === undefined && shown(c))) return columns;
  const view = (c: ColumnLayout): ColumnLayout => ({
    ...c,
    panes: c.panes.filter((p) => visible(p.key)),
  });
  const pinned = columns.filter((c) => c.pinned && shown(c)).map(view);
  const out = columns.slice();
  let i = 0;
  while (i < out.length) {
    const c = out[i]!;
    if (c.pinned || c.canvas !== undefined || !shown(c)) {
      i++;
      continue;
    }
    let prev: string | undefined;
    for (let j = i - 1; j >= 0; j--) {
      const p = out[j]!;
      if (!p.pinned && p.canvas !== undefined && shown(p)) {
        prev = p.canvas;
        break;
      }
    }
    if (prev !== undefined) {
      const mates = out.filter((x) => !x.pinned && x.canvas === prev && shown(x)).map(view);
      if (canvasFits([...mates, view(c), ...pinned], width, gap)) {
        out[i] = { ...c, canvas: prev };
        i++;
        continue;
      }
    }
    const fresh: ColumnLayout = { ...c, canvas: newCanvasId() };
    const end = prev === undefined ? -1 : lastIndex(out, (x) => !x.pinned && x.canvas === prev);
    if (end > i) {
      // 摘掉自己之后 end 前移了一格，插在 end 上正好是原来那一列的后面；
      // 下标 i 上现在是下一列，不前进
      out.splice(i, 1);
      out.splice(end, 0, fresh);
    } else {
      out[i] = fresh;
      i++;
    }
  }
  return out;
}

function lastIndex<T>(list: T[], pred: (x: T) => boolean): number {
  for (let i = list.length - 1; i >= 0; i--) if (pred(list[i]!)) return i;
  return -1;
}

/** 某块画布最后一列之后的下标（新列插在这里就是接在这块画布的最右）；没有这块返回 null */
export function canvasEnd(columns: ColumnLayout[], canvas: string): number | null {
  const end = lastIndex(columns, (c) => !c.pinned && c.canvas === canvas);
  return end < 0 ? null : end + 1;
}

/**
 * 把一扇窗口挪到另一块画布：独占一列，接在那块画布的最右（挤也认，用户指了地方）。
 * target 为 null = 新开一块，紧跟在 after 那块后面（after 找不到就放到最后）。
 * 本来就独占一列待在 target 上的，原样返回。
 */
export function moveToCanvas(
  columns: ColumnLayout[],
  key: string,
  target: string | null,
  after: string | null
): ColumnLayout[] {
  const from = findPane(columns, key);
  if (!from) return columns;
  const home = columns[from.col]!;
  if (target !== null && !home.pinned && home.canvas === target && home.panes.length === 1) {
    return columns;
  }
  const base = removePane(columns, key);
  if (target !== null) {
    return insertColumn(base, key, canvasEnd(base, target) ?? pinEdge(base), target);
  }
  const at = (after !== null ? canvasEnd(base, after) : null) ?? pinEdge(base);
  return insertColumn(base, key, at, newCanvasId());
}

// ---------------- 几何 ----------------

/** 列间距，与画布上的 gap 同一个数 */
export const CANVAS_GAP_PX = 8;

/**
 * 一块画布上各列的宽：正好铺满 width，不溢出——主区不再横向滚动，放不下的列在别的画布上。
 *
 * - 只剩一列时占满，连钉死的宽度也不认：把手只长在列与列之间，最后一列没有；而只有
 *   一扇窗口时最大化按钮也藏了。并排时拖窄的列在邻居关掉 / 切到只有它的项目之后，就会
 *   窄窄地停在左边、右边空一大片，没有任何办法拉回来。basis 不清掉，再有列进来时照旧按它排。
 * - 钉了宽的按钉的给，自适应的平分剩下的；一列自适应的都没有时最后一列收尾（与窗口高度
 *   同一个道理：最后一列的右边没有把手）。
 * - 剩下的不够每列自适应的分到 COLUMN_MIN_PX（窗口缩窄了、或者硬拖进来的）：钉了宽的按
 *   各自超出下限的部分等比让出来；让到下限还不够，就全部平分——宁可比下限窄也不溢出画布。
 */
export function columnWidths(
  columns: ColumnLayout[],
  width: number,
  gap = CANVAS_GAP_PX
): number[] {
  const n = columns.length;
  if (n === 0) return [];
  if (n === 1) return [Math.max(0, width)];
  const avail = Math.max(0, width - gap * (n - 1));
  const lastAuto = columns.map((c) => c.basis == null).lastIndexOf(true);
  const isAuto = columns.map((c, i) => c.basis == null || (lastAuto < 0 && i === n - 1));
  const autos = isAuto.filter(Boolean).length;
  const fixed = columns.map((c, i) => (isAuto[i] ? 0 : c.basis!));
  const fixedSum = fixed.reduce((a, b) => a + b, 0);
  const room = avail - fixedSum;
  if (room >= autos * COLUMN_MIN_PX) {
    return columns.map((_, i) => (isAuto[i] ? room / autos : fixed[i]!));
  }
  // 钉了宽的让位：各自超出下限的部分按比例让
  const need = autos * COLUMN_MIN_PX - room;
  const slack = fixed.reduce((n, w, i) => n + (isAuto[i] ? 0 : Math.max(0, w - COLUMN_MIN_PX)), 0);
  if (slack >= need && slack > 0) {
    const k = need / slack;
    return columns.map((_, i) =>
      isAuto[i] ? COLUMN_MIN_PX : fixed[i]! - Math.max(0, fixed[i]! - COLUMN_MIN_PX) * k
    );
  }
  return columns.map(() => avail / n);
}

/**
 * 拖列缝时，左边那列最多能拖到多宽：右边每列至少留下限宽。
 * left = 这一列的左边，right = 它右边还有几列。
 */
export function columnMaxWidth(opts: { width: number; left: number; right: number }): number {
  return Math.max(
    COLUMN_MIN_PX,
    opts.width - opts.left - opts.right * (COLUMN_MIN_PX + CANVAS_GAP_PX)
  );
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
}

/**
 * 一块画布的排布 → 像素坐标（columns 就是这一块上的列，含接在最右的固定列）。
 * 列的左右边取整，最后一列的右边正好落在 viewport.width 上。
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
  const widths = columnWidths(columns, viewport.width, gap);
  let x = 0;
  const out: ColumnFrame[] = [];
  columns.forEach((column, i) => {
    const left = Math.round(x);
    const right = i === columns.length - 1 ? Math.max(left, viewport.width) : Math.round(x + widths[i]!);
    out.push({
      id: column.id,
      x: left,
      width: right - left,
      panes: paneFrames(column.panes, viewport.height),
    });
    x += widths[i]! + gap;
  });
  return { columns: out };
}

// ---------------- 画布缩略图 ----------------

/** 画布条上缩略图的高（px）；宽按画布的宽高比算，见 canvasThumbSize */
export const CANVAS_THUMB_H = 16;
/** 缩略图宽的上下限：竖屏 / 超宽屏也不至于缩成一条线或撑满画布条 */
export const CANVAS_THUMB_MIN_W = 20;
export const CANVAS_THUMB_MAX_W = 40;

/** 缩略图的尺寸：高固定，宽按画布宽高比；画布还没量出来时给个常见比例 */
export function canvasThumbSize(viewport: { width: number; height: number }): {
  width: number;
  height: number;
} {
  const ratio = viewport.width > 0 && viewport.height > 0 ? viewport.width / viewport.height : 1.5;
  const width = Math.round(CANVAS_THUMB_H * ratio);
  return {
    width: Math.min(CANVAS_THUMB_MAX_W, Math.max(CANVAS_THUMB_MIN_W, width)),
    height: CANVAS_THUMB_H,
  };
}

export interface ThumbRect {
  key: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * 画布缩略图里每扇窗口的矩形：按这块画布真实的几何（layoutFrames）等比缩进 size，
 * 窗口之间留 gap 宽的缝。不能直接缩真实坐标：8px 的列缝、窗口之间没有缝，缩到十几像素
 * 高全糊成一块，认不出几列几扇。所以按"格子"缩——列占到下一列的左边为止、窗口占到
 * 下一扇的顶为止，取整之后各自让出 gap；整张图正好铺满 size，四周不留边（边距归外框）。
 */
export function canvasThumb(
  columns: ColumnLayout[],
  viewport: { width: number; height: number },
  size: { width: number; height: number },
  gap = 1
): ThumbRect[] {
  if (!(viewport.width > 0 && viewport.height > 0)) return [];
  const frames = layoutFrames(columns, viewport).columns;
  const sx = (size.width + gap) / viewport.width;
  const sy = (size.height + gap) / viewport.height;
  const out: ThumbRect[] = [];
  frames.forEach((col, ci) => {
    const left = Math.round(col.x * sx);
    const right = Math.round((frames[ci + 1]?.x ?? viewport.width) * sx);
    col.panes.forEach((pane, pi) => {
      const top = Math.round(pane.y * sy);
      const bottom = Math.round((col.panes[pi + 1]?.y ?? viewport.height) * sy);
      out.push({
        key: pane.key,
        x: left,
        y: top,
        width: Math.max(1, right - left - gap),
        height: Math.max(1, bottom - top - gap),
      });
    });
  });
  return out;
}

/**
 * 列内各窗口的高度：钉死的按钉死的算，其余平分剩下的；除不尽的零头给最后一扇
 * 自适应窗口，免得列底留一条一像素的缝。
 *
 * 一扇自适应的都没有（钉了上面那扇之后把下面的关掉 / 拖走，只剩一扇时最常见）就让最后
 * 一扇收尾：把手只长在两扇之间，最后一扇的底边拖不动，按钉的高度排就会在列底留一截
 * 永远填不上的空。
 */
function paneFrames(panes: PaneLayout[], height: number): PaneFrame[] {
  const fixed = panes.reduce((n, p) => n + (p.basis ?? 0), 0);
  const autos = panes.filter((p) => p.basis == null).length;
  const each = autos > 0 ? Math.max(PANE_MIN_PX, (height - fixed) / autos) : 0;
  const lastAuto = panes.map((p) => p.basis == null).lastIndexOf(true);
  const filler = lastAuto >= 0 ? lastAuto : panes.length - 1;
  const out: PaneFrame[] = [];
  let y = 0;
  panes.forEach((p, i) => {
    const h =
      i === filler ? Math.max(PANE_MIN_PX, height - y) : p.basis != null ? p.basis : each;
    out.push({ key: p.key, y: Math.round(y), height: Math.round(h) });
    y += h;
  });
  return out;
}

// ---------------- 拖拽落点 ----------------

/**
 * 窗口拖到哪：落进某列的第 index 格，或在第 at 列的位置另起一列。
 * canvas 只在翻成全量坐标之后才有（resolveSpot）：另起的那一列落在哪块画布上。
 */
export type DropSpot =
  | { kind: "into"; col: number; index: number }
  | { kind: "column"; at: number; canvas?: string };

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
  return spot.at > edge ? { ...spot, at: edge } : spot;
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
  const home = columns[from.col]!;
  const alone = home.panes.length === 1;
  if (spot.kind === "column") {
    // 本来就独占一列，拖到自己左右两条缝里等于没动
    const sameCanvas = spot.canvas === undefined || spot.canvas === home.canvas;
    if (alone && sameCanvas && (spot.at === from.col || spot.at === from.col + 1)) return columns;
    const at = alone && spot.at > from.col ? spot.at - 1 : spot.at;
    return insertColumn(columns, key, at, spot.canvas);
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
 * 画布画的是当前那块画布上的可见列（含接在最右的固定列），而 store 里存着所有项目、
 * 所有画布的列；两边的下标只在"只有一块画布、当前项目就是全部"时才碰巧一致。
 * 转换一律认 id 与 key，不认下标。
 *
 * 另起一列时认它左边那一列：插在它后面、落在它那块画布上；左边没有（落在最左）才认
 * 右边那一列、插在它前面。固定列不当锚——它在每块画布上都有，在排布里却永远是最后一列，
 * 认它就会插到最后一块画布的末尾去。两边都没有（画布上只有固定列）就接到固定列前面，
 * 不指定画布，交给 assignCanvases 去分。
 */
export function resolveSpot(
  columns: ColumnLayout[],
  visible: ColumnLayout[],
  spot: DropSpot
): DropSpot {
  if (spot.kind === "column") {
    const indexOf = (c: ColumnLayout | undefined) =>
      c && !c.pinned ? columns.findIndex((x) => x.id === c.id) : -1;
    const left = indexOf(visible[spot.at - 1]);
    if (left >= 0) return withCanvas(left + 1, columns[left]!.canvas);
    const right = indexOf(visible[spot.at]);
    if (right >= 0) return withCanvas(right, columns[right]!.canvas);
    return { kind: "column", at: pinEdge(columns) };
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

function withCanvas(at: number, canvas: string | undefined): DropSpot {
  return canvas === undefined ? { kind: "column", at } : { kind: "column", at, canvas };
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
