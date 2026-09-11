import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  clearMeegleCache,
  loadMeegleCache,
  meegleCacheStale,
  peekMeegleCache,
  writeMeegleCache,
} from "./meegleCache.js";

describe("meegleCache", () => {
  it("写入后能 peek，clear 后没了", () => {
    clearMeegleCache();
    writeMeegleCache("todo:todo", { items: [1] });
    assert.deepEqual(peekMeegleCache("todo:todo"), { items: [1] });
    clearMeegleCache();
    assert.equal(peekMeegleCache("todo:todo"), undefined);
  });

  it("刚写入的不算 stale，getOrLoad 命中不再跑 load", async () => {
    clearMeegleCache();
    writeMeegleCache("k", 1);
    assert.equal(meegleCacheStale("k"), false);
    let n = 0;
    assert.equal(await loadMeegleCache("k", async () => ++n), 1);
    assert.equal(n, 0);
    assert.equal(await loadMeegleCache("k", async () => ++n, true), 1);
    assert.equal(n, 1);
  });
});
