import assert from "node:assert/strict";
import { test } from "node:test";
import type { MeegleWorkItemDetail } from "@falcon/shared";
import { formatMeegleContext } from "./meegleContext.js";

const detail: MeegleWorkItemDetail = {
  id: "7105690993",
  name: "Broken chart",
  spaceKey: "space",
  typeKey: "issue",
  template: "一般BUG",
  currentNodes: [{ name: "Fixing", owners: ["Private owner"] }],
  operators: ["Private operator"],
  roles: [{ name: "QA", members: ["Private tester"] }],
};
const t = (key: string) => key;

test("AI context leads with the prefixed work item key and drops routing metadata", () => {
  const output = formatMeegleContext({ ...detail, description: "Steps" }, "2026-01-02T03:04:05Z", t);
  assert.match(output, /^# g-7105690993 Broken chart\n/);
  assert.match(output, /meegle\.contextDescription/);
  assert.doesNotMatch(output, /space|issue|2026-01-02|Fixing/);
  assert.doesNotMatch(output, /Private/);
});

test("sections with nothing in them are omitted instead of spending tokens on placeholders", () => {
  assert.equal(formatMeegleContext(detail, "now", t), "# g-7105690993 Broken chart\n");
  const output = formatMeegleContext({ ...detail, description: "Original text" }, "now", t);
  assert.ok(output.includes("Original text"));
  assert.doesNotMatch(output, /contextAttachments|contextComments/);
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

test("comments carrying neither text nor files do not open a section of their own", () => {
  const output = formatMeegleContext({
    ...detail,
    comments: [{ content: "  ", createdAt: "2026-01-03 10:00:00" }],
  }, "now", t);
  assert.equal(output, "# g-7105690993 Broken chart\n");
});

test("incomplete reads are disclosed even when the section came back empty", () => {
  const output = formatMeegleContext({
    ...detail,
    description: "Original text",
    commentsUnavailable: true,
  }, "now", t);
  assert.ok(output.includes("Original text"));
  assert.match(output, /meegle\.contextComments/);
  assert.match(output, /contextCommentsIncomplete/);
});

test("unknown templates keep the raw id in the heading rather than inventing a prefix", () => {
  const output = formatMeegleContext({ ...detail, template: undefined }, "now", t);
  assert.match(output, /^# 7105690993 Broken chart\n/);
});
