import { strict as assert } from "node:assert";
import test from "node:test";
import { externalHref, resolveRel } from "./mdLink.js";

test("externalHref 只放行 http(s) 与 mailto", () => {
  assert.equal(externalHref("https://example.com/a"), "https://example.com/a");
  assert.equal(externalHref("  http://x.dev "), "http://x.dev");
  assert.equal(externalHref("mailto:a@b.c"), "mailto:a@b.c");
  for (const bad of [
    "javascript:alert(1)",
    "JavaScript:alert(1)",
    "data:text/html;base64,PHNjcmlwdD4=",
    "./README.md",
    "#anchor",
  ]) {
    assert.equal(externalHref(bad), null, bad);
  }
});

test("resolveRel 按文档所在目录解析", () => {
  assert.equal(resolveRel("docs/adr", "0001-x.md"), "docs/adr/0001-x.md");
  assert.equal(resolveRel("docs/adr", "./0001-x.md"), "docs/adr/0001-x.md");
  assert.equal(resolveRel("docs/adr", "../design/y.md"), "docs/design/y.md");
  assert.equal(resolveRel("", "README.md"), "README.md");
  // 锚点与 query 在查看 tab 里没有意义，切掉
  assert.equal(resolveRel("docs", "a.md#section"), "docs/a.md");
  assert.equal(resolveRel("docs", "a.md?v=1"), "docs/a.md");
});

test("resolveRel 拒绝跑出工作目录的链接", () => {
  assert.equal(resolveRel("docs", "../../etc/passwd"), null);
  assert.equal(resolveRel("", "../x"), null);
  assert.equal(resolveRel("docs", "/etc/passwd"), null);
  assert.equal(resolveRel("docs", "#only-anchor"), null);
  assert.equal(resolveRel("docs", ""), null);
});
