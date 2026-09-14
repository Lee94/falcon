import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  applyDrop,
  CANVAS_GAP_PX,
  clampColumnWidth,
  columnWidth,
  layoutFrames,
  clampPaneHeight,
  clampSpot,
  column,
  COLUMN_MIN_PX,
  dropSpot,
  findPane,
  insertColumn,
  insertPane,
  isPinned,
  paneKeys,
  pinEdge,
  pinPane,
  paneMaxHeight,
  PANE_MIN_PX,
  removePane,
  replacePane,
  resolveSpot,
  setColumnBasis,
  setPaneBasis,
  syncColumns,
  unpinAll,
  visibleColumns,
  type ColumnLayout,
  type ColumnRect,
} from "./layout.js";

/** 列结构的简写：[["a","b"],["c"]] */
const cols = (...groups: string[][]): ColumnLayout[] => groups.map((g) => column(g));
const shape = (columns: ColumnLayout[]): string[][] => columns.map((c) => c.panes.map((p) => p.key));

describe("removePane", () => {
  it("摘掉窗口，列空了就不再占位", () => {
    assert.deepEqual(shape(removePane(cols(["a"], ["b", "c"]), "a")), [["b", "c"]]);
    assert.deepEqual(shape(removePane(cols(["a"], ["b", "c"]), "b")), [["a"], ["c"]]);
  });

  it("不存在的 key 什么也不改", () => {
    assert.deepEqual(shape(removePane(cols(["a"]), "zz")), [["a"]]);
  });
});

describe("insertPane / insertColumn", () => {
  it("插进指定列的指定格", () => {
    assert.deepEqual(shape(insertPane(cols(["a", "b"]), "x", { col: 0, index: 1 })), [
      ["a", "x", "b"],
    ]);
  });

  it("同一个 key 不会同时出现两次：已经在别处就是移动", () => {
    const next = insertPane(cols(["a", "b"], ["c"]), "c", { col: 0, index: 0 });
    assert.deepEqual(shape(next), [["c", "a", "b"]]);
    assert.equal(paneKeys(next).filter((k) => k === "c").length, 1);
  });

  it("独占一列插在第 at 列的位置", () => {
    assert.deepEqual(shape(insertColumn(cols(["a"], ["b"]), "x", 1)), [["a"], ["x"], ["b"]]);
    assert.deepEqual(shape(insertColumn(cols(["a"]), "x", 9)), [["a"], ["x"]]);
  });

  it("空排布里插一扇就是第一列", () => {
    assert.deepEqual(shape(insertPane([], "a", { col: 3, index: 3 })), [["a"]]);
  });
});

describe("replacePane", () => {
  it("就地换 key，位置与高度都留着", () => {
    let columns = cols(["a", "b"], ["c"]);
    columns = setPaneBasis(columns, "b", 200);
    const next = replacePane(columns, "b", "b2");
    assert.deepEqual(shape(next), [["a", "b2"], ["c"]]);
    assert.equal(next[0]!.panes[1]!.basis, 200);
  });

  it("换成一个已经在别处的 key 时不留副本", () => {
    const next = replacePane(cols(["a", "b"], ["c"]), "a", "c");
    assert.deepEqual(shape(next), [["c", "b"]]);
  });
});

describe("syncColumns", () => {
  it("live 之外的窗口摘掉，空列消失", () => {
    assert.deepEqual(shape(syncColumns(cols(["a"], ["b", "c"]), ["b"])), [["b"]]);
  });

  it("还没排布的按给定顺序各自成一列，接在最右", () => {
    assert.deepEqual(shape(syncColumns(cols(["a"]), ["a", "x", "y"])), [["a"], ["x"], ["y"]]);
  });

  it("已排布的保持原位，不因为 live 的顺序被重排", () => {
    assert.deepEqual(shape(syncColumns(cols(["b", "a"]), ["a", "b"])), [["b", "a"]]);
  });

  it("没有变化时结构稳定（列宽照旧）", () => {
    const before = setColumnBasis(cols(["a"], ["b"]), "", null);
    const after = syncColumns(before, ["a", "b"]);
    assert.deepEqual(shape(after), shape(before));
  });
});

describe("visibleColumns", () => {
  it("只画可见的窗口，空列不占位，隐藏的仍留在原排布里", () => {
    const columns = cols(["a", "x"], ["y"], ["b"]);
    const vis = visibleColumns(columns, (k) => k === "a" || k === "b");
    assert.deepEqual(shape(vis), [["a"], ["b"]]);
    assert.deepEqual(shape(columns), [["a", "x"], ["y"], ["b"]]);
  });
});

describe("dropSpot", () => {
  const rects: ColumnRect[] = [
    { left: 0, right: 400, panes: [{ top: 0, bottom: 300 }, { top: 300, bottom: 600 }] },
    { left: 408, right: 808, panes: [{ top: 0, bottom: 600 }] },
  ];

  it("列身上按窗口中线决定插在第几格", () => {
    assert.deepEqual(dropSpot({ x: 200, y: 100 }, rects), { kind: "into", col: 0, index: 0 });
    assert.deepEqual(dropSpot({ x: 200, y: 200 }, rects), { kind: "into", col: 0, index: 1 });
    assert.deepEqual(dropSpot({ x: 200, y: 500 }, rects), { kind: "into", col: 0, index: 2 });
  });

  it("列的左右边缘是「另起一列」", () => {
    assert.deepEqual(dropSpot({ x: 5, y: 300 }, rects), { kind: "column", at: 0 });
    assert.deepEqual(dropSpot({ x: 395, y: 300 }, rects), { kind: "column", at: 1 });
    assert.deepEqual(dropSpot({ x: 800, y: 300 }, rects), { kind: "column", at: 2 });
  });

  it("列之间的缝、最左最右之外也是另起一列", () => {
    assert.deepEqual(dropSpot({ x: 404, y: 10 }, rects), { kind: "column", at: 1 });
    assert.deepEqual(dropSpot({ x: -20, y: 10 }, rects), { kind: "column", at: 0 });
    assert.deepEqual(dropSpot({ x: 900, y: 10 }, rects), { kind: "column", at: 2 });
  });

  it("窄列上的边缘区按列宽收窄，列内始终落得进去", () => {
    const narrow: ColumnRect[] = [{ left: 0, right: 80, panes: [{ top: 0, bottom: 100 }] }];
    assert.deepEqual(dropSpot({ x: 40, y: 10 }, narrow), { kind: "into", col: 0, index: 0 });
  });

  it("空画布只能另起一列", () => {
    assert.deepEqual(dropSpot({ x: 10, y: 10 }, []), { kind: "column", at: 0 });
  });
});

describe("applyDrop", () => {
  it("落回原地返回原数组，调用方据此跳过一次 set", () => {
    const columns = cols(["a", "b"], ["c"]);
    assert.equal(applyDrop(columns, "a", { kind: "into", col: 0, index: 0 }), columns);
    assert.equal(applyDrop(columns, "a", { kind: "into", col: 0, index: 1 }), columns);
    // 独占一列的窗口拖到自己左右两条缝里也是没动
    assert.equal(applyDrop(columns, "c", { kind: "column", at: 1 }), columns);
    assert.equal(applyDrop(columns, "c", { kind: "column", at: 2 }), columns);
  });

  it("同列内往下挪：摘掉自己之后下标补偿一格", () => {
    assert.deepEqual(shape(applyDrop(cols(["a", "b", "c"]), "a", { kind: "into", col: 0, index: 3 })), [
      ["b", "c", "a"],
    ]);
  });

  it("独占列被摘掉后，它右边的列整体左移一格", () => {
    // a 独占第 0 列，拖进第 2 列（原下标）的顶上
    assert.deepEqual(
      shape(applyDrop(cols(["a"], ["b"], ["c", "d"]), "a", { kind: "into", col: 2, index: 0 })),
      [["b"], ["a", "c", "d"]]
    );
    // 拖成新列插在最右
    assert.deepEqual(shape(applyDrop(cols(["a"], ["b"], ["c"]), "a", { kind: "column", at: 3 })), [
      ["b"],
      ["c"],
      ["a"],
    ]);
  });

  it("排布里没有的 key 原样返回", () => {
    const columns = cols(["a"]);
    assert.equal(applyDrop(columns, "zz", { kind: "column", at: 0 }), columns);
  });
});

describe("resolveSpot", () => {
  it("可见下标翻成全量下标（别的项目的窗口不参与计数）", () => {
    const columns = cols(["x"], ["a", "y", "b"]);
    const vis = visibleColumns(columns, (k) => k === "a" || k === "b");
    // 可见看到的是一列 [a,b]；落在 b 之前 = 全量里 y 之后的那一格
    assert.deepEqual(resolveSpot(columns, vis, { kind: "into", col: 0, index: 1 }), {
      kind: "into",
      col: 1,
      index: 2,
    });
    // 越过最后一扇 = 列尾（全量列尾，不是可见列尾）
    assert.deepEqual(resolveSpot(columns, vis, { kind: "into", col: 0, index: 2 }), {
      kind: "into",
      col: 1,
      index: 3,
    });
  });

  it("新列落点按 id 找锚，找不到就接到最右", () => {
    const columns = cols(["x"], ["a"], ["b"]);
    const vis = visibleColumns(columns, (k) => k !== "x");
    assert.deepEqual(resolveSpot(columns, vis, { kind: "column", at: 1 }), { kind: "column", at: 2 });
    assert.deepEqual(resolveSpot(columns, vis, { kind: "column", at: 2 }), { kind: "column", at: 3 });
  });
});

describe("尺寸", () => {
  it("列宽有下限，上限由画布给", () => {
    assert.equal(clampColumnWidth(10), COLUMN_MIN_PX);
    assert.equal(clampColumnWidth(500, 400), 400);
    assert.equal(clampColumnWidth(Number.NaN), COLUMN_MIN_PX);
  });

  it("窗口高度的上限要给下面每扇留出最小高度", () => {
    assert.equal(paneMaxHeight({ columnHeight: 600, above: 0, below: 1 }), 600 - PANE_MIN_PX);
    assert.equal(paneMaxHeight({ columnHeight: 100, above: 0, below: 5 }), PANE_MIN_PX);
    assert.equal(clampPaneHeight(10, 500), PANE_MIN_PX);
    assert.equal(clampPaneHeight(900, 500), 500);
  });

  it("按 id / key 改尺寸，不按下标", () => {
    const columns = cols(["a"], ["b"]);
    const next = setColumnBasis(columns, columns[1]!.id, 320);
    assert.equal(next[0]!.basis, null);
    assert.equal(next[1]!.basis, 320);
    assert.equal(setPaneBasis(columns, "b", 120)[1]!.panes[0]!.basis, 120);
  });
});

describe("layoutFrames", () => {
  const viewport = { width: 1000, height: 600 };

  it("一列占满，多列按 max(半屏, min(全屏, 640))", () => {
    assert.equal(columnWidth(column([]), 1, 1000), 1000);
    // 半屏 496 < 640，取 640
    assert.equal(columnWidth(column([]), 2, 1000), 640);
    // 半屏 996 > 640，取半屏
    assert.equal(columnWidth(column([]), 3, 2000), (2000 - CANVAS_GAP_PX) / 2);
    // 钉死的宽度直接用
    assert.equal(columnWidth({ ...column([]), basis: 320 }, 3, 2000), 320);
  });

  it("列从左到右排，缝算进总宽", () => {
    const frames = layoutFrames(cols(["a"], ["b"]), viewport);
    assert.equal(frames.columns[0]!.x, 0);
    assert.equal(frames.columns[1]!.x, 640 + CANVAS_GAP_PX);
    assert.equal(frames.width, 640 * 2 + CANVAS_GAP_PX);
  });

  it("列内窗口平分列高，正好填满不留缝", () => {
    const frames = layoutFrames(cols(["a", "b", "c"]), viewport);
    const panes = frames.columns[0]!.panes;
    assert.deepEqual(panes.map((p) => p.y), [0, 200, 400]);
    assert.equal(panes[2]!.y + panes[2]!.height, 600);
  });

  it("钉死高度的窗口按钉的算，剩下的分给自适应的", () => {
    let columns = cols(["a", "b"]);
    columns = setPaneBasis(columns, "a", 150);
    const panes = layoutFrames(columns, viewport).columns[0]!.panes;
    assert.deepEqual(panes, [
      { key: "a", y: 0, height: 150 },
      { key: "b", y: 150, height: 450 },
    ]);
  });

  it("钉得太满时仍给每扇留出最小高度（宁可溢出也不给 0）", () => {
    let columns = cols(["a", "b"]);
    columns = setPaneBasis(columns, "a", 590);
    const panes = layoutFrames(columns, { width: 1000, height: 600 }).columns[0]!.panes;
    assert.equal(panes[1]!.height, PANE_MIN_PX);
  });
});

describe("固定列", () => {
  it("固定：摘出来独占一列钉在最右", () => {
    const next = pinPane(cols(["a", "b"], ["c"]), "a");
    assert.deepEqual(shape(next), [["b"], ["c"], ["a"]]);
    assert.equal(pinEdge(next), 2);
    assert.equal(isPinned(next, "a"), true);
    assert.equal(isPinned(next, "c"), false);
  });

  it("窗口高度跟着一起搬", () => {
    const next = pinPane(setPaneBasis(cols(["a", "b"]), "a", 200), "a");
    assert.equal(next.at(-1)!.panes[0]!.basis, 200);
  });

  it("至多固定一列：再固定一扇，旧的自动取消", () => {
    const first = pinPane(cols(["a"], ["b"], ["c"]), "a");
    const second = pinPane(first, "b");
    assert.deepEqual(shape(second), [["c"], ["a"], ["b"]]);
    assert.equal(second.filter((c) => c.pinned).length, 1);
    assert.equal(isPinned(second, "b"), true);
  });

  it("取消固定只清标记，列留在原地", () => {
    const next = unpinAll(pinPane(cols(["a"], ["b"]), "a"));
    assert.deepEqual(shape(next), [["b"], ["a"]]);
    assert.equal(pinEdge(next), 2);
    assert.equal(isPinned(next, "a"), false);
  });

  it("固定列的窗口关掉之后，空列连同固定一起消失", () => {
    const next = removePane(pinPane(cols(["a"], ["b"]), "a"), "a");
    assert.deepEqual(shape(next), [["b"]]);
    assert.equal(pinEdge(next), 1);
  });

  it("新列一律插在固定列左边", () => {
    const pinnedCols = pinPane(cols(["a"], ["b"]), "b");
    assert.deepEqual(shape(insertColumn(pinnedCols, "x", 9)), [["a"], ["x"], ["b"]]);
    assert.deepEqual(shape(syncColumns(pinnedCols, ["a", "b", "x"])), [["a"], ["x"], ["b"]]);
  });

  it("拖拽越不过固定列：落点夹到它左边", () => {
    const pinnedCols = pinPane(cols(["a"], ["b"], ["c"]), "c");
    assert.deepEqual(clampSpot(pinnedCols, { kind: "column", at: 3 }), { kind: "column", at: 2 });
    assert.deepEqual(clampSpot(pinnedCols, { kind: "column", at: 1 }), { kind: "column", at: 1 });
    // 列内插入不受影响：拖进固定列本身是允许的
    assert.deepEqual(clampSpot(pinnedCols, { kind: "into", col: 2, index: 1 }), {
      kind: "into",
      col: 2,
      index: 1,
    });
    assert.deepEqual(shape(applyDrop(pinnedCols, "a", { kind: "column", at: 3 })), [
      ["b"],
      ["a"],
      ["c"],
    ]);
  });

  it("固定列里唯一的窗口拖到最右 = 没动", () => {
    const pinnedCols = pinPane(cols(["a"], ["b"]), "b");
    assert.equal(applyDrop(pinnedCols, "b", { kind: "column", at: 2 }), pinnedCols);
  });
});

describe("findPane / paneKeys", () => {
  it("坐标与从左到右、列内从上到下的顺序", () => {
    const columns = cols(["a", "b"], ["c"]);
    assert.deepEqual(findPane(columns, "c"), { col: 1, index: 0 });
    assert.equal(findPane(columns, "zz"), null);
    assert.deepEqual(paneKeys(columns), ["a", "b", "c"]);
  });
});
