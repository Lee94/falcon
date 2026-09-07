import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { externalHref, resolveRel } from "./mdLink.js";

describe("externalHref", () => {
  it("only lets http(s) and mailto through", () => {
    assert.equal(externalHref("https://a.b/c"), "https://a.b/c");
    assert.equal(externalHref("mailto:x@y.z"), "mailto:x@y.z");
    assert.equal(externalHref("javascript:alert(1)"), null);
    assert.equal(externalHref("data:text/html,hi"), null);
    assert.equal(externalHref("docs/a.md"), null);
  });
});

describe("resolveRel", () => {
  it("resolves against the document directory", () => {
    assert.equal(resolveRel("docs", "./a.md"), "docs/a.md");
    assert.equal(resolveRel("docs/x", "../b.md"), "docs/b.md");
    assert.equal(resolveRel("", "README.md"), "README.md");
  });

  it("refuses to escape the workspace or use host-absolute paths", () => {
    assert.equal(resolveRel("docs", "../../etc/passwd"), null);
    assert.equal(resolveRel("docs", "/etc/passwd"), null);
  });

  it("drops anchors and queries", () => {
    assert.equal(resolveRel("docs", "a.md#top"), "docs/a.md");
    assert.equal(resolveRel("docs", "a.md?x=1"), "docs/a.md");
    assert.equal(resolveRel("docs", "#top"), null);
  });

  it("decodes percent-encoded segments so the path names the real file", () => {
    assert.equal(resolveRel("", "img%20dir/logo.png"), "img dir/logo.png");
    assert.equal(resolveRel("docs", "%E5%9B%BE/%E7%89%87.png"), "docs/图/片.png");
    // 解不动的按原文保留
    assert.equal(resolveRel("", "100%/a.png"), "100%/a.png");
    // 编码过的 `..` 同样是上跳
    assert.equal(resolveRel("docs", "%2E%2E/b.md"), "b.md");
  });
});
