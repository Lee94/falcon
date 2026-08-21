import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { layoutCommitGraph, type GraphInput } from "./gitGraph.js";

/** 用单字母当 sha，可读性比 40 位十六进制强得多 */
const chain = (spec: [string, ...string[]][]): GraphInput[] =>
  spec.map(([sha, ...parents]) => ({ sha, parents }));

describe("layoutCommitGraph", () => {
  it("keeps a linear history in one lane", () => {
    const rows = layoutCommitGraph(chain([["c", "b"], ["b", "a"], ["a"]]));
    assert.deepEqual(
      rows.map((r) => r.lane),
      [0, 0, 0]
    );
    assert.deepEqual(
      rows.map((r) => r.width),
      [1, 1, 1]
    );
    assert.ok(rows.every((r) => !r.merge));
  });

  it("gives the second parent of a merge its own lane, then folds it back in", () => {
    //   m        lane 0（合并）
    //   |\
    //   a b      a 在 0，b 在 1
    //   |/
    //   r        两条泳道汇回 0
    const rows = layoutCommitGraph(
      chain([["m", "a", "b"], ["a", "r"], ["b", "r"], ["r"]])
    );
    assert.deepEqual(
      rows.map((r) => r.lane),
      [0, 0, 1, 0]
    );
    assert.equal(rows[0]!.merge, true);
    // 合并行有两条从圆点出发的线：继承的 0 与新开的 1
    assert.deepEqual(
      rows[0]!.segments.filter((s) => s.from === null).map((s) => s.to),
      [0, 1]
    );
    // b 那行是泳道 1 的终点，r 那行把两条线都收进圆点
    assert.deepEqual(
      rows[3]!.segments.filter((s) => s.to === null).map((s) => s.from),
      [0, 1]
    );
    assert.equal(rows[3]!.width, 2);
  });

  it("draws a pass-through for a lane that has nothing to do with this row", () => {
    // b 那行时，泳道 1 正等着 side，跟本行无关，要直着穿过去
    const rows = layoutCommitGraph(
      chain([["m", "b", "side"], ["b", "r"], ["side", "r"], ["r"]])
    );
    const through = rows[1]!.segments.filter((s) => s.from !== null && s.from === s.to);
    assert.deepEqual(through, [{ from: 1, to: 1, lane: 1 }]);
  });

  it("reuses the lane already waiting for a second parent instead of doubling it", () => {
    // x 与 m 都以 s 为第二个父：m 处理完泳道 1 已在等 s，x 不该再开一条
    const rows = layoutCommitGraph(
      chain([["x", "m", "s"], ["m", "b", "s"], ["b", "s"], ["s"]])
    );
    assert.ok(rows.every((r) => r.width <= 2));
  });

  it("stops at the page boundary rather than trailing a line into nothing", () => {
    // a 的父提交在下一页：这一行不该再往下画
    const rows = layoutCommitGraph(chain([["b", "a"], ["a", "older"]]));
    assert.deepEqual(rows[1]!.segments.filter((s) => s.to !== null), []);
    assert.equal(rows[1]!.width, 1);
  });

  it("does not build a staircase when the results are unrelated (search / author filter)", () => {
    // 筛选后的结果之间没有父子关系，全部落在同一条泳道上
    const rows = layoutCommitGraph(
      chain([["q", "q1"], ["w", "w1"], ["e", "e1"], ["r", "r1"]])
    );
    assert.deepEqual(
      rows.map((r) => r.lane),
      [0, 0, 0, 0]
    );
    assert.ok(rows.every((r) => r.width === 1));
  });

  it("handles an empty page", () => {
    assert.deepEqual(layoutCommitGraph([]), []);
  });
});
