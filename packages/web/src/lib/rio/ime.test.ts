import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { imeCursorRect } from "./ime.js";

describe("imeCursorRect", () => {
  const grid = {
    cols: 80,
    rows: 24,
    cellWidth: 9.5,
    cellHeight: 18.25,
    displayOffset: 0,
  };

  it("positions the IME textarea on the terminal cursor cell", () => {
    assert.deepEqual(imeCursorRect({ line: 3, col: 5 }, grid), {
      left: 47.5,
      top: 54.75,
      width: 9.5,
      height: 18.25,
    });
  });

  it("keeps a cursor at the right margin inside the grid", () => {
    assert.deepEqual(imeCursorRect({ line: 0, col: 80 }, grid), {
      left: 750.5,
      top: 0,
      width: 9.5,
      height: 18.25,
    });
  });

  it("does not move the live cursor anchor into a scrollback viewport", () => {
    assert.equal(imeCursorRect({ line: 3, col: 5 }, { ...grid, displayOffset: 2 }), null);
  });

  it("rejects a cursor or cell metrics outside the visible grid", () => {
    assert.equal(imeCursorRect({ line: 24, col: 5 }, grid), null);
    assert.equal(imeCursorRect({ line: 3, col: 5 }, { ...grid, cellWidth: 0 }), null);
    assert.equal(imeCursorRect({ line: Number.NaN, col: 5 }, grid), null);
  });
});
