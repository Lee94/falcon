import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { isUsableTermSize } from "./termFit.js";

describe("isUsableTermSize", () => {
  const pane = { width: 960, height: 640 };

  it("rejects proposeDimensions() === undefined (renderer 还没量格子)", () => {
    assert.equal(isUsableTermSize(undefined, pane), false);
    assert.equal(isUsableTermSize(null, pane), false);
  });

  it("rejects 0-size container (flex 还没撑开)", () => {
    assert.equal(isUsableTermSize({ cols: 80, rows: 24 }, { width: 960, height: 0 }), false);
    assert.equal(isUsableTermSize({ cols: 80, rows: 24 }, { width: 0, height: 640 }), false);
  });

  it("rejects NaN / 下限残骸", () => {
    assert.equal(isUsableTermSize({ cols: Number.NaN, rows: 24 }, pane), false);
    assert.equal(isUsableTermSize({ cols: 150, rows: 1 }, pane), false);
    assert.equal(isUsableTermSize({ cols: 1, rows: 24 }, pane), false);
  });

  it("accepts a real fit", () => {
    assert.equal(isUsableTermSize({ cols: 128, rows: 40 }, pane), true);
    assert.equal(isUsableTermSize({ cols: 80, rows: 24 }, pane), true);
  });
});
