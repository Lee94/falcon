import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { rawUrl } from "./rawUrl.js";

describe("rawUrl", () => {
  const base = "/api/projects/p1/raw/tok/";

  it("keeps slashes and encodes each segment", () => {
    assert.equal(rawUrl(base, "docs/index.html"), `${base}docs/index.html`);
    assert.equal(rawUrl(base, "a b/c#1?.png"), `${base}a%20b/c%231%3F.png`);
    assert.equal(rawUrl(base, "图/片.png"), `${base}%E5%9B%BE/%E7%89%87.png`);
  });

  it("drops empty segments", () => {
    assert.equal(rawUrl(base, "/a//b"), `${base}a/b`);
  });
});
