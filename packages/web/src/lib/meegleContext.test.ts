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

test("AI context omits work item metadata and keeps only the useful content sections", () => {
  const output = formatMeegleContext(detail, "2026-01-02T03:04:05Z", t);
  assert.match(output, /^# Broken chart/);
  assert.match(output, /meegle\.contextDescription/);
  assert.match(output, /meegle\.contextAttachments/);
  assert.match(output, /meegle\.contextComments/);
  assert.doesNotMatch(output, /123|space|issue|2026-01-02|Fixing/);
  assert.doesNotMatch(output, /Private/);
});

test("AI context preserves description Markdown, attachments and comments", () => {
  const markdown = "Steps\n\n```js\nthrow new Error('oops');\n```\n\n![screen](https://example.com/a.png)";
  const output = formatMeegleContext({
    ...detail,
    name: "Title\n## Not a section",
    business: "Charts",
    description: "Steps [图片]",
    descriptionMarkdown: markdown,
    contextFields: [{ name: "Environment", value: "Browser v1\nOS v2" }],
    attachments: [{ name: "trace.txt", url: "https://example.com/trace.txt" }],
    comments: [{
      content: "Please check this case",
      createdAt: "2026-01-03 10:00:00",
      attachments: ["https://example.com/comment.png"],
    }],
  }, "now", t);
  assert.ok(output.includes(markdown));
  assert.ok(output.includes("[trace.txt](https://example.com/trace.txt)"));
  assert.ok(output.includes("Please check this case"));
  assert.ok(output.includes("https://example.com/comment.png"));
  assert.doesNotMatch(output, /Environment|Browser v1|Charts/);
  assert.doesNotMatch(output, /\n## Not a section/);
  assert.doesNotMatch(output, /\[图片\]/);
});

test("older details fall back to their available description without inventing data", () => {
  const output = formatMeegleContext({ ...detail, description: "Original text" }, "now", t);
  assert.ok(output.includes("Original text"));
  assert.match(output, /contextUnavailable/);
});

test("incomplete comment reads are disclosed without discarding title and description", () => {
  const output = formatMeegleContext({
    ...detail,
    description: "Original text",
    commentsUnavailable: true,
  }, "now", t);
  assert.ok(output.includes("Original text"));
  assert.match(output, /contextCommentsIncomplete/);
});
