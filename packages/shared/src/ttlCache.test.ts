import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { TtlCache } from "./ttlCache.js";

describe("TtlCache", () => {
  it("TTL 内命中，过期后 miss", async () => {
    const c = new TtlCache(1000);
    let n = 0;
    const load = async () => ++n;
    assert.equal(await c.getOrLoad("k", load, { now: 0 }), 1);
    assert.equal(await c.getOrLoad("k", load, { now: 999 }), 1);
    assert.equal(await c.getOrLoad("k", load, { now: 1000 }), 2);
    assert.equal(c.peek("k", 1000)?.value, 2);
    assert.equal(c.peek("k", 2000), undefined);
  });

  it("同 key 并发共用一个 in-flight", async () => {
    const c = new TtlCache(1000);
    let n = 0;
    let release!: (v: number) => void;
    const load = () =>
      new Promise<number>((resolve) => {
        n++;
        release = resolve;
      });
    const a = c.getOrLoad("k", load);
    const b = c.getOrLoad("k", load);
    assert.equal(n, 1);
    release(7);
    assert.deepEqual(await Promise.all([a, b]), [7, 7]);
    assert.equal(n, 1);
  });

  it("fresh 跳过已有条目，但仍并入正在飞的那一次", async () => {
    const c = new TtlCache(1000);
    let n = 0;
    assert.equal(await c.getOrLoad("k", async () => ++n, { now: 0 }), 1);
    assert.equal(await c.getOrLoad("k", async () => ++n, { fresh: true, now: 10 }), 2);

    let release!: (v: number) => void;
    const inflight = c.getOrLoad(
      "k",
      () =>
        new Promise<number>((resolve) => {
          n++;
          release = resolve;
        }),
      { fresh: true }
    );
    const joined = c.getOrLoad("k", async () => ++n, { fresh: true });
    release(9);
    assert.deepEqual(await Promise.all([inflight, joined]), [9, 9]);
    assert.equal(n, 3);
  });

  it("load 失败不入库，下次再试", async () => {
    const c = new TtlCache(1000);
    let n = 0;
    await assert.rejects(c.getOrLoad("k", async () => {
      n++;
      throw new Error("boom");
    }));
    assert.equal(c.peek("k"), undefined);
    assert.equal(await c.getOrLoad("k", async () => ++n), 2);
  });

  it("clear 之后已在飞的结果不许写回", async () => {
    const c = new TtlCache(1000);
    let release!: (v: number) => void;
    const pending = c.getOrLoad(
      "k",
      () => new Promise<number>((resolve) => {
        release = resolve;
      })
    );
    c.clear();
    release(1);
    assert.equal(await pending, 1);
    assert.equal(c.peek("k"), undefined);
    assert.equal(await c.getOrLoad("k", async () => 2), 2);
  });

  it("单条可以覆盖 TTL", async () => {
    const c = new TtlCache(1000);
    await c.getOrLoad("k", async () => "v", { ttlMs: 5000, now: 0 });
    assert.equal(c.peek("k", 4999)?.value, "v");
    assert.equal(c.peek("k", 5000), undefined);
  });
});
