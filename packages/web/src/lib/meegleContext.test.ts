import assert from "node:assert/strict";
import { test } from "node:test";
import type { MeegleWorkItemDetail } from "@falcon/shared";
import { formatMeegleContext } from "./meegleContext.js";

const detail: MeegleWorkItemDetail = {
  id: "123",
  name: "Broken chart",
  spaceKey: "space",
  typeKey: "issue",
  currentNodes: [{ name: "Fixing", owners: ["Private owner"] }],
  operators: ["Private operator"],
  roles: [{ name: "QA", members: ["Private tester"] }],
};
const t = (key: string) => key;

test("AI context keeps source identity and explicitly identifies missing information", () => {
  const output = formatMeegleContext(detail, "2026-01-02T03:04:05Z", t);
  assert.match(output, /copyKeyLabel: 123/);
  assert.match(output, /contextSpaceKey: space/);
  assert.match(output, /contextTypeKey: issue/);
  assert.match(output, /d_business: meegle.contextUnavailable/);
  assert.match(output, /contextFetchedAt: 2026-01-02T03:04:05Z/);
  assert.match(output, /d_currentNode: Fixing/);
  assert.doesNotMatch(output, /Private/);
});

test("AI context preserves Markdown images, code and custom fields instead of preview text", () => {
  const markdown = "Steps\n\n```js\nthrow new Error('oops');\n```\n\n![screen](https://example.com/a.png)";
  const output = formatMeegleContext({
    ...detail,
    name: "Title\n## Not a section",
    business: "Charts",
    description: "Steps [图片]",
    descriptionMarkdown: markdown,
    contextFields: [{ name: "Environment", value: "Browser v1\nOS v2" }],
  }, "now", t);
  assert.ok(output.includes(markdown));
  assert.ok(output.includes("### Environment\n\nBrowser v1\nOS v2"));
  assert.match(output, /d_business: Charts/);
  assert.doesNotMatch(output, /\n## Not a section/);
  assert.doesNotMatch(output, /\[图片\]/);
});

test("older details fall back to their available description without inventing data", () => {
  const output = formatMeegleContext({ ...detail, description: "Original text" }, "now", t);
  assert.ok(output.includes("Original text"));
  assert.match(output, /contextGapsNote/);
  assert.match(output, /contextAttachmentsNote/);
});

test("failed supplementary reads must not become a seemingly complete clipboard context", () => {
  assert.throws(() => formatMeegleContext({
    ...detail,
    contextFieldsUnavailable: true,
  }, "now", t), /copyContextFailed/);
});
