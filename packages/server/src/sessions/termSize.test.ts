import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  decideViewerAttach,
  fallbackTermSize,
  parseStoredTermSize,
} from "./termSize.js";

describe("parseStoredTermSize", () => {
  it("accepts a previously measured size", () => {
    assert.deepEqual(parseStoredTermSize(205, 57), { cols: 205, rows: 57 });
  });

  it("rejects missing or junk so we do not treat 80×24 fallback as measured", () => {
    assert.equal(parseStoredTermSize(null, null), null);
    assert.equal(parseStoredTermSize(undefined, 24), null);
    assert.equal(parseStoredTermSize(80, 0), null);
    assert.equal(parseStoredTermSize(1.5, 24), null);
  });
});

describe("fallbackTermSize", () => {
  it("only used when nothing was ever measured (new session)", () => {
    assert.deepEqual(fallbackTermSize(null), { cols: 80, rows: 24 });
    assert.deepEqual(fallbackTermSize({ cols: 205, rows: 57 }), { cols: 205, rows: 57 });
  });
});

describe("decideViewerAttach", () => {
  it("waits for this viewer's resize before reattach, even if a size is stored", () => {
    assert.equal(
      decideViewerAttach({ durable: true, hasBackend: false }),
      "wait-size"
    );
  });

  it("replays to a new viewer when the backend is already live", () => {
    assert.equal(decideViewerAttach({ durable: true, hasBackend: true }), "hello");
  });

  it("marks a non-durable session dead once its backend is gone", () => {
    assert.equal(decideViewerAttach({ durable: false, hasBackend: false }), "dead");
  });
});
