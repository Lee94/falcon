import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { brailleDots, isSprite, junctionStop, spriteOps, type SpriteMetrics, type SpriteOp } from "./sprites.js";

const M: SpriteMetrics = { cellW: 12, cellH: 12, thick: 2 };
const ops = (cp: number, m: SpriteMetrics = M) => {
  const r = spriteOps(cp, m);
  assert.ok(r, `no ops for ${cp.toString(16)}`);
  return r!;
};

/** 只按矩形 op 填一张位图（盒线 / 块元素 / braille 只用矩形） */
function fill(list: SpriteOp[], m: SpriteMetrics): (x: number, y: number) => boolean {
  const bits = new Uint8Array(m.cellW * m.cellH);
  for (const op of list) {
    assert.equal(op.op, "rect");
    if (op.op !== "rect") continue;
    for (let y = op.y; y < op.y + op.h; y++) for (let x = op.x; x < op.x + op.w; x++) bits[y * m.cellW + x] = 1;
  }
  return (x, y) => x >= 0 && y >= 0 && x < m.cellW && y < m.cellH && bits[y * m.cellW + x] === 1;
}

/** 3×3 junction 网格里各块左上角像素的填充情况 */
function junction(cp: number): number[][] {
  const at = fill(ops(cp), M);
  const xg = Math.floor((M.cellW - 3 * M.thick) / 2);
  const yg = Math.floor((M.cellH - 3 * M.thick) / 2);
  return [0, 1, 2].map((r) => [0, 1, 2].map((c) => (at(xg + c * M.thick, yg + r * M.thick) ? 1 : 0)));
}

describe("isSprite", () => {
  it("ASCII 与普通文字一次比较即出，三段范围命中", () => {
    assert.equal(isSprite(0x41), false);
    assert.equal(isSprite(0x4e2d), false);
    assert.equal(isSprite(0x2500), true);
    assert.equal(isSprite(0x259f), true);
    assert.equal(isSprite(0x25a0), false);
    assert.equal(isSprite(0x2800), true);
    assert.equal(isSprite(0xe0b0), true);
    assert.equal(isSprite(0xe0c0), false);
  });
});

describe("盒线：细 / 粗", () => {
  it("─ 贯穿整格、居中、thick 高；━ 更粗；│ 贯穿整高", () => {
    const h = fill(ops(0x2500), M);
    for (let x = 0; x < 12; x++) {
      assert.equal(h(x, 5) && h(x, 6), true, `x=${x}`);
      assert.equal(h(x, 4) || h(x, 7), false, `x=${x} 出界`);
    }
    const heavy = fill(ops(0x2501), M);
    assert.equal(heavy(0, 4) && heavy(0, 7), true);
    const v = fill(ops(0x2502), M);
    for (let y = 0; y < 12; y++) assert.equal(v(5, y) && v(6, y) && !v(4, y) && !v(7, y), true, `y=${y}`);
  });

  it("┌ 两条臂在中心相接；┼ 横竖都贯穿；╵ 只到中心", () => {
    const corner = fill(ops(0x250c), M);
    assert.equal(corner(6, 11), true);
    assert.equal(corner(11, 6), true);
    assert.equal(corner(6, 6), true);
    assert.equal(corner(0, 6), false);
    assert.equal(corner(6, 0), false);
    const cross = fill(ops(0x253c), M);
    for (let i = 0; i < 12; i++) {
      assert.equal(cross(i, 6), true, `row ${i}`);
      assert.equal(cross(6, i), true, `col ${i}`);
    }
    const stub = fill(ops(0x2575), M);
    assert.equal(stub(5, 0), true);
    assert.equal(stub(5, 6), true);
    assert.equal(stub(5, 8), false);
  });

  it("混合粗细：┝ 竖细横粗，臂能接上", () => {
    const at = fill(ops(0x251d), M);
    assert.equal(at(5, 0), true);
    assert.equal(at(11, 6), true);
    assert.equal(at(11, 4), true); // 粗臂比细臂高
  });

  it("虚线：三段 / 四段 / 两段，从亮开始且含暗段", () => {
    for (const [cp, n] of [[0x2504, 3], [0x2508, 4], [0x254c, 2]] as const) {
      const list = ops(cp);
      assert.equal(list.length, n);
      const at = fill(list, M);
      assert.equal(at(0, 6), true);
      assert.ok([...Array(12).keys()].some((x) => !at(x, 6)), "有暗段");
    }
    assert.equal(ops(0x2506).length, 3);
  });

  it("圆角与对角线是描边", () => {
    const arc = ops(0x256d)[0]!;
    assert.equal(arc.op, "stroke");
    if (arc.op === "stroke") {
      assert.deepEqual(arc.points[0], [6, 12]);
      assert.deepEqual(arc.points[arc.points.length - 1], [12, 6]);
    }
    const diag = ops(0x2571)[0]!;
    assert.equal(diag.op, "stroke");
    if (diag.op === "stroke") assert.deepEqual(diag.points, [[12, 0], [0, 12]]);
    assert.equal(ops(0x2573).length, 2);
  });
});

describe("盒线：双线 junction", () => {
  it("junctionStop 规则", () => {
    assert.equal(junctionStop(0, true, [], []), 2);
    assert.equal(junctionStop(0, false, [], []), 2);
    assert.equal(junctionStop(1, false, [], []), 1);
    assert.equal(junctionStop(0, false, [0, 2], []), 0);
    assert.equal(junctionStop(0, false, [], [0, 2]), 2);
    assert.equal(junctionStop(1, false, [0, 2], [0, 2]), 0);
    assert.equal(junctionStop(1, false, [], [0, 2]), 2);
  });

  it("四个角与 T 形", () => {
    assert.deepEqual(junction(0x2554), [[1, 1, 1], [1, 0, 0], [1, 0, 1]]); // ╔
    assert.deepEqual(junction(0x2557), [[1, 1, 1], [0, 0, 1], [1, 0, 1]]); // ╗
    assert.deepEqual(junction(0x255a), [[1, 0, 1], [1, 0, 0], [1, 1, 1]]); // ╚
    assert.deepEqual(junction(0x255d), [[1, 0, 1], [0, 0, 1], [1, 1, 1]]); // ╝
    assert.deepEqual(junction(0x2563), [[1, 0, 1], [1, 0, 1], [1, 0, 1]]); // ╣
    assert.deepEqual(junction(0x2560), [[1, 0, 1], [1, 0, 1], [1, 0, 1]]); // ╠
    assert.deepEqual(junction(0x2566), [[1, 1, 1], [0, 0, 0], [1, 1, 1]]); // ╦
    assert.deepEqual(junction(0x2569), [[1, 1, 1], [0, 0, 0], [1, 1, 1]]); // ╩
    assert.deepEqual(junction(0x256c), [[1, 1, 1], [1, 0, 1], [1, 1, 1]]); // ╬
  });

  it("╦ 的两条竖线从下边线往下、╩ 从上边线往上；═ ║ 贯穿", () => {
    const t = fill(ops(0x2566), M);
    assert.equal(t(3, 11), true);
    assert.equal(t(7, 11), true);
    assert.equal(t(3, 0), false);
    const b = fill(ops(0x2569), M);
    assert.equal(b(3, 0), true);
    assert.equal(b(3, 11), false);
    const h = fill(ops(0x2550), M);
    assert.equal(h(0, 3) && h(11, 3) && h(0, 7) && h(11, 7), true);
    assert.equal(h(0, 5), false);
  });

  it("单双混合：╒ ╘ ╤ ╧ ╪ ╫ ╨", () => {
    assert.deepEqual(junction(0x2552), [[0, 1, 1], [0, 1, 0], [0, 1, 1]]); // ╒
    assert.equal(fill(ops(0x2552), M)(5, 11), true);
    assert.equal(fill(ops(0x2552), M)(5, 0), false);
    assert.deepEqual(junction(0x2558), [[0, 1, 1], [0, 1, 0], [0, 1, 1]]); // ╘
    assert.equal(fill(ops(0x2558), M)(5, 0), true);
    assert.deepEqual(junction(0x2564), [[1, 1, 1], [0, 0, 0], [1, 1, 1]]); // ╤
    assert.equal(fill(ops(0x2564), M)(5, 11), true);
    assert.deepEqual(junction(0x2567), [[1, 1, 1], [0, 0, 0], [1, 1, 1]]); // ╧
    assert.equal(fill(ops(0x2567), M)(5, 0), true);
    assert.deepEqual(junction(0x256a), [[1, 1, 1], [0, 1, 0], [1, 1, 1]]); // ╪
    assert.deepEqual(junction(0x256b), [[1, 0, 1], [1, 1, 1], [1, 0, 1]]); // ╫
    assert.deepEqual(junction(0x2568), [[1, 0, 1], [1, 1, 1], [0, 0, 0]]); // ╨
  });
});

describe("块元素", () => {
  it("上半 / 全块 / 左半 / 阴影 / 象限", () => {
    assert.deepEqual(ops(0x2580), [{ op: "rect", x: 0, y: 0, w: 12, h: 6 }]);
    assert.deepEqual(ops(0x2588), [{ op: "rect", x: 0, y: 0, w: 12, h: 12 }]);
    assert.deepEqual(ops(0x258c), [{ op: "rect", x: 0, y: 0, w: 6, h: 12 }]);
    assert.deepEqual(ops(0x2591), [{ op: "rect", x: 0, y: 0, w: 12, h: 12, alpha: 0.25 }]);
    const lower1 = ops(0x2581)[0] as { y: number; h: number };
    assert.equal(lower1.y + lower1.h, 12);
    assert.ok(lower1.h >= 1);
    const q = fill(ops(0x259a), M); // ▚ 左上 + 右下
    assert.equal(q(0, 0) && q(11, 11), true);
    assert.equal(q(11, 0) || q(0, 11), false);
  });
});

describe("braille", () => {
  it("点位掩码", () => {
    assert.deepEqual(brailleDots(0x2801), [[0, 0]]);
    assert.deepEqual(brailleDots(0x2880), [[1, 3]]);
    assert.equal(brailleDots(0x28ff).length, 8);
    assert.equal(ops(0x2801).length, 1);
    const at = fill(ops(0x2880), M);
    assert.equal(at(8, 10) && at(9, 11), true);
    assert.equal(at(1, 1) || at(8, 1), false);
  });
});

describe("powerline", () => {
  it("三角是三点多边形，半圆首尾贴边，chevron 是描边", () => {
    const tri = ops(0xe0b0)[0]!;
    assert.equal(tri.op, "poly");
    if (tri.op === "poly") assert.deepEqual(tri.points, [[0, 0], [12, 6], [0, 12]]);
    const disc = ops(0xe0b4)[0]!;
    assert.equal(disc.op, "poly");
    if (disc.op === "poly") {
      assert.deepEqual(disc.points[0], [0, 0]);
      assert.deepEqual(disc.points[disc.points.length - 1], [0, 12]);
      const maxX = Math.max(...disc.points.map((p) => p[0]));
      assert.ok(maxX > 11.9 && maxX <= 12.0001);
    }
    assert.equal(ops(0xe0b1)[0]!.op, "stroke");
    assert.equal(spriteOps(0xe0c0, M), null);
  });
});
