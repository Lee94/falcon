/**
 * History 左侧那条提交图的泳道布局。纯函数，不碰 DOM。
 *
 * 输入是服务端按 --date-order 给的一页提交（新 → 旧），输出每行画什么：
 * 圆点落在第几条泳道、以及经过这一行的线段怎么走。渲染层只管把这些序号
 * 翻译成 SVG 坐标，一行一个独立的小 SVG——subject 可能换行导致行高不等，
 * 一整张贯穿的大图会跟行对不齐。
 *
 * 算法是 gitk 那一套的简化版：维护一个"泳道 → 我在等哪个 sha"的数组，
 * 每处理一条提交就把它占的泳道换成它的第一个父提交，其余父提交各开一条。
 */

/** 一行里的一段线。序号是泳道号，渲染层再乘泳道宽度 */
export interface GraphSegment {
  /** 从行顶部进来的泳道；从圆点出发的线为 null */
  from: number | null;
  /** 往行底部去的泳道；汇入圆点的线为 null */
  to: number | null;
  /** 上色用的泳道号：斜线跟着它连去的那条泳道走 */
  lane: number;
}

export interface GraphRow {
  /** 圆点所在泳道 */
  lane: number;
  /** 这一行占了多少条泳道（含圆点），渲染层据此算图形区宽度 */
  width: number;
  segments: GraphSegment[];
  /** 合并提交（多于一个父）。图上画成空心点，与 git 图形客户端的惯例一致 */
  merge: boolean;
}

export interface GraphInput {
  sha: string;
  parents: string[];
}

/**
 * 每行的泳道与线段。
 *
 * **只连本页里真实存在的父提交**。两个理由，缺一个都会画出错的图：
 * - 翻页边界上，最后几条的父提交在下一页。照连的话页尾会挂着几根通向
 *   空白的竖线，看起来像历史断在这里。
 * - 搜索 / 按作者筛选时，结果之间根本没有父子关系。照连的话每条提交都要
 *   开一条新泳道，一页下来就是一道六十级的阶梯，还全是断线。
 */
export function layoutCommitGraph(commits: GraphInput[]): GraphRow[] {
  const present = new Set(commits.map((c) => c.sha));
  // lanes[i] = 第 i 条泳道正在等的 sha；null = 这条泳道空着
  const lanes: (string | null)[] = [];
  const rows: GraphRow[] = [];

  const firstFree = (): number => {
    const i = lanes.indexOf(null);
    if (i >= 0) return i;
    lanes.push(null);
    return lanes.length - 1;
  };

  for (const commit of commits) {
    // 本行开始前的样子：进来的线段都取自它
    const before = [...lanes];

    // 谁在等这条提交？第一条泳道归它，其余的在本行汇入后关闭
    const waiting = before.reduce<number[]>((acc, sha, i) => {
      if (sha === commit.sha) acc.push(i);
      return acc;
    }, []);
    const lane = waiting[0] ?? firstFree();
    for (const i of waiting.slice(1)) lanes[i] = null;

    // 从圆点往下走的泳道。第一个父继承本泳道，其余的（合并进来的那些分支）各开一条
    const parents = commit.parents.filter((p) => present.has(p));
    const outgoing = new Set<number>();
    lanes[lane] = parents[0] ?? null;
    if (parents[0]) outgoing.add(lane);
    for (const parent of parents.slice(1)) {
      // 已经有泳道在等这个父提交就复用，否则同一条历史会被画成两根平行线
      const existing = lanes.indexOf(parent);
      const target = existing >= 0 ? existing : firstFree();
      lanes[target] = parent;
      outgoing.add(target);
    }

    // 尾部的空泳道收掉，免得 width 被历史最大值一直撑着
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) lanes.pop();

    const segments: GraphSegment[] = [];
    before.forEach((sha, i) => {
      if (sha === null) return;
      // 等的就是本行这条提交：线走到圆点为止
      if (sha === commit.sha) segments.push({ from: i, to: null, lane });
      // 与本行无关：直着穿过去。泳道号中途不会变（只有 firstFree 分配时才动，
      // 而那只发生在空位上），所以穿过去的线永远是竖直的
      else segments.push({ from: i, to: i, lane: i });
    });
    for (const i of outgoing) segments.push({ from: null, to: i, lane: i });

    rows.push({
      lane,
      width: Math.max(before.length, lanes.length, lane + 1),
      segments,
      merge: commit.parents.length > 1,
    });
  }

  return rows;
}

/**
 * 泳道配色。
 *
 * 按泳道序号取，不按分支——一条提交上可能没有任何 ref，"它属于哪个分支"
 * 在 log 输出里根本不成立，而泳道是画面上唯一稳定的东西。第 0 条泳道永远
 * 同一个颜色，于是主线在整页上颜色一致，这正是扫一眼时要的信息。
 */
export const GRAPH_COLOR_COUNT = 6;

export function laneColor(lane: number): string {
  return `var(--graph-${(lane % GRAPH_COLOR_COUNT) + 1})`;
}
