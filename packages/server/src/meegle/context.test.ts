import { strict as assert } from "node:assert";
import test from "node:test";
import {
  businessText, detailContext, fieldsArgs, isContextField, mqlNextArgs,
  mqlRecent, mqlSearch, normalizeDetail, normalizeFields, normalizeMqlRows, workItemArgs,
} from "./command.js";

const metadata = normalizeFields({
  list: [
    { field_key: "business", field_name: "业务线", field_type: "_business", option: [
      { option_id: "parent", option_name: "数据分析", children: [
        { option_id: "leaf", option_name: "AI" },
        { option_id: "old", option_name: "历史业务", disabled: true },
      ] },
    ] },
    { field_key: "steps", field_name: "复现步骤", field_type: "multi-text" },
    { field_key: "expected", field_name: "预期结果", field_type: "text" },
    { field_key: "actual", field_name: "实际结果", field_type: "text" },
    { field_key: "env", field_name: "客户环境", field_type: "select",
      option: [{ option_id: "cloud", option_name: "公有云" }] },
    { field_key: "version", field_name: "影响版本", field_type: "workitem_related_multi_select" },
    { field_key: "logs", field_name: "日志", field_type: "multi-text" },
    { field_key: "links", field_name: "相关链接", field_type: "link" },
    { field_key: "owner", field_name: "环境负责人", field_type: "user" },
    { field_key: "email", field_name: "邮箱", field_type: "text" },
    { field_key: "users", field_name: "复现用户", field_type: "multi-user" },
    { field_key: "template_version", field_name: "流程版本", field_type: "number" },
  ],
  pagination: { has_more: true },
}, 1);

test("business resolves only authoritative option IDs / MQL cascade labels, never space or guessed names", () => {
  const options = metadata.items[0].options;
  assert.equal(metadata.hasMore, true);
  assert.equal(businessText("leaf", options), "数据分析 / AI");
  assert.equal(businessText("old", options), "数据分析 / 历史业务");
  assert.equal(businessText("unknown", options), undefined);
  assert.equal(businessText("业务名称也不能凭字符串猜"), undefined);
  assert.equal(businessText({ value: { cascade_key_label_value: {
    key: "parent", label: "数据分析", children: [{ key: "leaf", label: "AI", children: null }],
  } } }), "数据分析 / AI");
  assert.equal(businessText({ name: "所属空间" }), undefined);
  assert.equal(businessText(["leaf", "leaf"], options), "数据分析 / AI");
});

test("detail context retains diagnosis text, code and image links, excludes personnel and unsupported objects", () => {
  const markdown = "步骤\n![screen](https://example.com/s.png)<!-- image:private -->\n```js\nfail()\n```";
  const result = detailContext({ work_item_fields: [
    { key: "business", value: "leaf" },
    { key: "steps", value: markdown },
    { key: "expected", value: "成功" },
    { key: "actual", value: "失败" },
    { key: "env", value: { label: "公有云", value: "cloud" } },
    { key: "version", value: [{ id: 123, name: "v1.2.3" }] },
    { key: "logs", value: "```text\nError: boom\n```" },
    { key: "links", value: { name: "trace", url: "https://example.com/trace" } },
    { key: "owner", value: { name: "某人", email: "private@example.com" } },
    { key: "email", value: "private@example.com" },
    { key: "users", value: [{ name: "某人", email: "private@example.com" }] },
    { key: "template_version", value: 4 },
    { key: "unknown", value: { email: "private@example.com" } },
  ] }, metadata.items);
  assert.equal(result.business, "数据分析 / AI");
  assert.equal(result.contextFields.length, 7);
  assert.equal(result.contextFields[0].value, markdown.replace(/<!--[\s\S]*?-->/g, ""));
  assert.deepEqual(result.contextFields.find((f) => f.name === "影响版本"), { name: "影响版本", value: "v1.2.3" });
  assert.ok(!JSON.stringify(result).includes("private@example.com"));
  assert.ok(metadata.items.filter(isContextField).every((f) => !["owner", "email", "users", "template_version"].includes(f.key)));
  assert.deepEqual(detailContext({ work_item_fields: [{ key: "logs", value: { email: "x" } }] }, metadata.items).contextFields, []);
});

test("descriptionMarkdown strips only HTML comments, legacy description stays compact", () => {
  const md = "\n![screen](https://example.com/s.png)<!-- image:data -->\n\n\n```ts\nx();  \n```\n";
  const detail = normalizeDetail({
    work_item_attribute: { work_item_id: "1", owned_project: { name: "空间" } },
    work_item_fields: [{ key: "description", value: md }],
  }, null)!;
  assert.equal(detail.descriptionMarkdown, md.replace("<!-- image:data -->", ""));
  assert.ok(detail.description?.includes("[图片]"));
  assert.equal(detail.business, undefined);
});

test("new argv remain literal argv, MQL second page uses opaque session and fixed group", () => {
  assert.ok(workItemArgs("p", "1", ["business", "steps"]).includes('--fields=["business","steps"]'));
  assert.ok(fieldsArgs("p", "issue", 2).includes("--page-num=2"));
  assert.match(mqlRecent("p", "issue"), /LIMIT 100$/);
  assert.match(mqlSearch("p", "issue", "bug"), /LIMIT 100$/);
  assert.deepEqual(mqlNextArgs("p", { session_id: "-session" }), [
    "workitem", "query", "--project-key=p", "--session-id=-session",
    '--group-pagination-list=[{"group_id":"1","page_num":2}]', "--format", "json",
  ]);
  assert.equal(mqlNextArgs("p", {}), undefined);
});

test("MQL list business uses labeled hierarchy or defers IDs to batched metadata resolution", () => {
  const data = (value: unknown) => ({ data: { "1": [{ moql_field_list: [
    { key: "work_item_id", value: { long_value: 1 } },
    { key: "business", value },
  ] }] } });
  assert.equal(normalizeMqlRows(data({ key_label_value: { key: "leaf", label: "AI" } }))[0].business, "AI");
  const unresolved = normalizeMqlRows(data({ string_value: "leaf" }))[0];
  assert.equal(unresolved.business, undefined);
  assert.equal(businessText(unresolved.businessValue, metadata.items[0].options), "数据分析 / AI");
});
