import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { dprQuery, watchDpr, type DprEnv, type MediaQueryLike } from "./dpr.js";

class FakeMql implements MediaQueryLike {
  listeners = new Set<() => void>();
  constructor(readonly query: string) {}
  addEventListener(_type: "change", fn: () => void): void {
    this.listeners.add(fn);
  }
  removeEventListener(_type: "change", fn: () => void): void {
    this.listeners.delete(fn);
  }
  fire(): void {
    for (const fn of [...this.listeners]) fn();
  }
}

function fakeEnv(initialDpr: number): DprEnv & { dpr: number; created: FakeMql[] } {
  const env = {
    dpr: initialDpr,
    created: [] as FakeMql[],
    matchMedia(query: string) {
      const m = new FakeMql(query);
      env.created.push(m);
      return m;
    },
    devicePixelRatio: () => env.dpr,
  };
  return env;
}

describe("watchDpr", () => {
  it("按当前 dpr 建查询，变化后回调新值并按新 dpr 重新武装", () => {
    const env = fakeEnv(2);
    const seen: number[] = [];
    watchDpr((d) => seen.push(d), env);
    assert.equal(env.created.length, 1);
    assert.equal(env.created[0]!.query, dprQuery(2));

    env.dpr = 1;
    env.created[0]!.fire();
    assert.deepEqual(seen, [1]);
    // 旧查询已解绑，新查询按 1dppx 建
    assert.equal(env.created[0]!.listeners.size, 0);
    assert.equal(env.created.length, 2);
    assert.equal(env.created[1]!.query, dprQuery(1));

    env.dpr = 1.5;
    env.created[1]!.fire();
    assert.deepEqual(seen, [1, 1.5]);
  });

  it("stop 之后不再回调，也不再建新查询", () => {
    const env = fakeEnv(2);
    const seen: number[] = [];
    const stop = watchDpr((d) => seen.push(d), env);
    stop();
    assert.equal(env.created[0]!.listeners.size, 0);
    env.dpr = 3;
    env.created[0]!.fire();
    assert.deepEqual(seen, []);
    assert.equal(env.created.length, 1);
  });
});
