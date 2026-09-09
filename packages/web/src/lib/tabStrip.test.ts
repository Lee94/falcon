import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  DIFF_KEY,
  applyVisibleOrder,
  defaultStripKeys,
  dropIndex,
  fileKey,
  insertAfter,
  keysOther,
  keysToLeft,
  keysToRight,
  moveIndex,
  orderedStripKeys,
  parseStripKey,
  replaceKey,
  tabShift,
  termKey,
  weaveOrder,
} from "./tabStrip.js";

describe("strip keys", () => {
  it("round-trips terminal / file / diff", () => {
    assert.deepEqual(parseStripKey(termKey("abc")), {
      kind: "terminal",
      key: "t:abc",
      id: "abc",
    });
    assert.deepEqual(parseStripKey(fileKey({ projectId: "p", path: "src/a.ts" })), {
      kind: "file",
      key: "f:p:src/a.ts",
      projectId: "p",
      path: "src/a.ts",
    });
    assert.deepEqual(parseStripKey(DIFF_KEY), { kind: "diff", key: "d" });
  });

  it("file path 里的冒号算进 path，不切断 projectId", () => {
    const key = fileKey({ projectId: "proj", path: "weird:name.txt" });
    assert.deepEqual(parseStripKey(key), {
      kind: "file",
      key,
      projectId: "proj",
      path: "weird:name.txt",
    });
  });

  it("defaultStripKeys 是会话 → 文件 → 差异", () => {
    assert.deepEqual(
      defaultStripKeys({
        terminalIds: ["a", "b"],
        files: [{ projectId: "p", path: "x" }],
        hasDiff: true,
      }),
      ["t:a", "t:b", "f:p:x", "d"]
    );
  });
});

describe("orderedStripKeys", () => {
  it("记住的顺序优先，新开的接到末尾，关掉的消失", () => {
    assert.deepEqual(orderedStripKeys(["t:b", "t:a", "d"], ["t:a", "t:b", "t:c"]), [
      "t:b",
      "t:a",
      "t:c",
    ]);
  });

  it("空 tabOrder 就是默认顺序", () => {
    assert.deepEqual(orderedStripKeys([], ["t:a", "f:p:x"]), ["t:a", "f:p:x"]);
  });
});

describe("moveIndex / weaveOrder", () => {
  it("moveIndex 把一项挪到目标下标", () => {
    assert.deepEqual(moveIndex(["a", "b", "c"], 0, 2), ["b", "c", "a"]);
    assert.deepEqual(moveIndex(["a", "b", "c"], 2, 0), ["c", "a", "b"]);
    assert.deepEqual(moveIndex(["a", "b", "c"], 1, 1), ["a", "b", "c"]);
  });

  it("weaveOrder 只重排可见项，隐藏项原地不动", () => {
    // full 里 t:hidden 属于别的项目，拖可见的 a/b 时它不该跟着跳
    const full = ["t:a", "t:hidden", "t:b"];
    const visible = ["t:a", "t:b"];
    const after = ["t:b", "t:a"];
    assert.deepEqual(weaveOrder(full, visible, after), ["t:b", "t:hidden", "t:a"]);
  });

  it("applyVisibleOrder 对终端 id 数组同样织回去", () => {
    const tabs = ["a", "hidden", "b"];
    const vis = new Set(["a", "b"]);
    assert.deepEqual(
      applyVisibleOrder(tabs, ["b", "a"], (id) => vis.has(id)),
      ["b", "hidden", "a"]
    );
  });
});

describe("insertAfter / replaceKey / close ranges", () => {
  it("insertAfter 插到指定 key 右侧，找不到就追加", () => {
    assert.deepEqual(insertAfter(["a", "b"], "a", "x"), ["a", "x", "b"]);
    assert.deepEqual(insertAfter(["a", "b"], "nope", "x"), ["a", "b", "x"]);
    assert.deepEqual(insertAfter(["a", "b"], undefined, "x"), ["a", "b", "x"]);
    assert.deepEqual(insertAfter(["a", "x"], "a", "x"), ["a", "x"]);
  });

  it("replaceKey 把 pending id 换成真会话 id", () => {
    assert.deepEqual(replaceKey(["t:p1", "d"], "t:p1", "t:real"), ["t:real", "d"]);
  });

  it("close left / right / other 按当前下标切", () => {
    const keys = ["a", "b", "c", "d"];
    assert.deepEqual(keysToLeft(keys, 2), ["a", "b"]);
    assert.deepEqual(keysToRight(keys, 2), ["d"]);
    assert.deepEqual(keysOther(keys, 2), ["a", "b", "d"]);
    assert.deepEqual(keysToLeft(keys, 0), []);
    assert.deepEqual(keysToRight(keys, 3), []);
  });
});

describe("drag geometry", () => {
  const lefts = [0, 100, 220];
  const widths = [100, 120, 80];

  it("dropIndex 越过邻居中点才换位", () => {
    // 中点：50 / 160 / 260
    assert.equal(dropIndex(40, lefts, widths), 0);
    assert.equal(dropIndex(50, lefts, widths), 0);
    assert.equal(dropIndex(159, lefts, widths), 0);
    assert.equal(dropIndex(160, lefts, widths), 1);
    assert.equal(dropIndex(259, lefts, widths), 1);
    assert.equal(dropIndex(260, lefts, widths), 2);
  });

  it("tabShift：向右拖时中间项左让，向左拖时中间项右让", () => {
    const w = 100;
    // from 0 → to 2：index 1 和 2 左移
    assert.equal(tabShift(0, 2, 0, w), 0);
    assert.equal(tabShift(0, 2, 1, w), -w);
    assert.equal(tabShift(0, 2, 2, w), -w);
    // from 2 → to 0：index 0 和 1 右移
    assert.equal(tabShift(2, 0, 0, w), w);
    assert.equal(tabShift(2, 0, 1, w), w);
    assert.equal(tabShift(2, 0, 2, w), 0);
    // 没换位
    assert.equal(tabShift(1, 1, 0, w), 0);
    assert.equal(tabShift(1, 1, 2, w), 0);
  });
});
