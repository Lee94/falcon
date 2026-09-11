import assert from "node:assert/strict";
import { test } from "node:test";
import { meegleDisplayKey } from "./meegleKey.js";

test("display key uses the configured template prefix", () => {
  assert.equal(meegleDisplayKey({ id: "7105690993", template: "一般 BUG" }), "g-7105690993");
  assert.equal(meegleDisplayKey({ id: "7112501423", template: "迭代BUG" }), "f-7112501423");
  assert.equal(meegleDisplayKey({ id: "7112723046", template: "设计确认" }), "f-7112723046");
  assert.equal(meegleDisplayKey({ id: "7113000000", template: "需求任务" }), "m-7113000000");
});

test("unknown templates safely retain the raw work item id", () => {
  assert.equal(meegleDisplayKey({ id: "123", template: "新模板" }), "123");
  assert.equal(meegleDisplayKey({ id: "456" }), "456");
});
