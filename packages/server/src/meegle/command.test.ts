import { strict as assert } from "node:assert";
import test from "node:test";
import {
  chunk,
  cliErrorText,
  commentArgs,
  groupForLookup,
  isValidHost,
  isValidKey,
  isValidUrl,
  loginArgs,
  mqlByIds,
  mqlLiteral,
  mqlRecent,
  mqlSearch,
  mqlValueText,
  normalizeAttribute,
  normalizeDetail,
  normalizeMqlRows,
  normalizeMultiViewItems,
  normalizeSpaces,
  normalizeTodo,
  normalizeTypes,
  normalizeUser,
  normalizeViewItems,
  normalizeViews,
  parseCliJson,
  parseLoginPrompt,
  parseStatus,
  parseUrlTarget,
  plainDescription,
  queryArgs,
  spacesArgs,
  viewSearchArgs,
  workItemUrl,
} from "./command.js";

test("comment list argv carries the project, item and page", () => {
  assert.deepEqual(commentArgs("p1", "123", 2), [
    "comment", "list", "--project-key=p1", "--work-item-id=123", "--page-num=2", "--format", "json",
  ]);
});

test("argv 用 --flag=value，用户输入以 - 开头也不会被当成 flag", () => {
  assert.deepEqual(viewSearchArgs("p1", "story", "-foo"), [
    "view",
    "search",
    "--project-key=p1",
    "--view-scope=story",
    "--key-word=-foo",
    "--format",
    "json",
  ]);
  assert.deepEqual(spacesArgs(), ["project", "search", "--page-num=1", "--format", "json"]);
  assert.deepEqual(spacesArgs("FX", 2), [
    "project",
    "search",
    "--project-key=FX",
    "--page-num=2",
    "--format",
    "json",
  ]);
  assert.deepEqual(loginArgs("meegle.com"), ["auth", "login", "--host=meegle.com", "--device-code"]);
  assert.equal(queryArgs("p", "SELECT 1")[3], "--mql=SELECT 1");
});

test("参数白名单", () => {
  assert.ok(isValidHost("project.feishu.cn"));
  assert.ok(isValidHost("my-tenant.example.com"));
  for (const bad of ["", "localhost", "a b.com", "-x.com", "x.com/", "http://x.com", 1]) {
    assert.equal(isValidHost(bad), false, String(bad));
  }
  assert.ok(isValidKey("67d7ba04296cba3d3ece0694"));
  assert.ok(isValidKey("26oZ-EaHR"));
  assert.ok(isValidKey("j__7WfhHRN"));
  for (const bad of ["", "a b", "--x", "x`y", "a".repeat(65), "x/y"]) {
    assert.equal(isValidKey(bad), false, bad);
  }
});

test("MQL 字面量：单引号双写、控制字符变空格", () => {
  assert.equal(mqlLiteral("it's"), "'it''s'");
  assert.equal(mqlLiteral("a\nb\u0000c"), "'a b c'");
  assert.equal(
    mqlSearch("p1", "issue", "登录'", 20),
    "SELECT `work_item_id`, `name`, `work_item_status`, `updated_at`, `business` FROM `p1`.`issue` " +
      "WHERE `name` LIKE '%登录''%' ORDER BY `work_item_id` DESC LIMIT 20"
  );
  assert.match(mqlRecent("p1", "story", 999), /LIMIT 100$/);
  // 非数字 id 直接丢，LIMIT 跟着有效数量走
  assert.equal(
    mqlByIds("p1", "issue", ["1", "x", "2"]),
    "SELECT `work_item_id`, `name`, `work_item_status`, `updated_at`, `business` FROM `p1`.`issue` " +
      "WHERE `work_item_id` IN (1, 2) LIMIT 2"
  );
});

test("parseCliJson：成功 / 信封错误 / unknown command / 纯文本", () => {
  assert.deepEqual(parseCliJson('{"a":1}', ""), { ok: true, data: { a: 1 } });
  assert.deepEqual(parseCliJson("null", ""), { ok: true, data: null });
  assert.deepEqual(parseCliJson("[]", ""), { ok: true, data: [] });
  // 错误信封在 stderr 上，stdout 是空的
  const err = parseCliJson(
    "",
    JSON.stringify({
      data: null,
      error: {
        code: "SERVER_CALL_FAILED",
        message:
          "error=ErrViewNotExist,message=view not exist,retriable=false\nlogid: 20260910110143F01F",
        retryable: true,
      },
      meta: {},
    })
  );
  assert.deepEqual(err, { ok: false, code: "SERVER_CALL_FAILED", message: "view not exist" });
  // stderr 上不是信封的 JSON 也不能当成功
  assert.deepEqual(parseCliJson("", '{"a":1}'), { ok: false, code: "BAD_OUTPUT", message: '{"a":1}' });
  assert.deepEqual(parseCliJson('unknown command "project" for "meegle"\n', ""), {
    ok: false,
    code: "UNKNOWN_COMMAND",
    message: 'unknown command "project" for "meegle"',
  });
  assert.deepEqual(parseCliJson("", "boom\nmore"), { ok: false, code: "BAD_OUTPUT", message: "boom" });
  const empty = parseCliJson("", "");
  assert.ok(!empty.ok && empty.message === "meegle 没有输出");
});

test("cliErrorText 取最里层的业务错误", () => {
  assert.equal(
    cliErrorText(
      "error=ErrServiceInternalError,message=Service Internal Error,biz error: project_key, view_scope and key_word are required,retriable=true\nlogid: x"
    ),
    "project_key, view_scope and key_word are required"
  );
  assert.equal(
    cliErrorText(
      "error=ErrMetadataError,message=metadata error,project access denied (Code: 3005) | Context: no permission,retriable=false"
    ),
    "metadata error,project access denied (Code: 3005) | Context: no permission"
  );
  assert.equal(cliErrorText("plain text\nsecond"), "plain text");
});

test("parseLoginPrompt 从 device-code 输出里抓链接与授权码", () => {
  const text =
    "\n  Please scan the QR code with your phone, or open the following URL in a browser:\n" +
    "  URL: https://project.feishu.cn/b/auth/mcp?channel=meegle-cli&mode=device&usercode=6YJBH-TLS5W\n" +
    "  Authorization code: 6YJBH-TLS5W\n\n████\n";
  assert.deepEqual(parseLoginPrompt(text), {
    url: "https://project.feishu.cn/b/auth/mcp?channel=meegle-cli&mode=device&usercode=6YJBH-TLS5W",
    code: "6YJBH-TLS5W",
  });
  assert.equal(parseLoginPrompt("  Please scan the QR code"), null);
});

test("parseStatus 三种形态", () => {
  assert.deepEqual(parseStatus({ authenticated: true, expires_in_minutes: 119, host: "project.feishu.cn" }), {
    authenticated: true,
    host: "project.feishu.cn",
    expiresInMinutes: 119,
  });
  assert.deepEqual(parseStatus({ authenticated: false, host: null, reason: "no local token" }), {
    authenticated: false,
    host: null,
    expiresInMinutes: undefined,
  });
  assert.deepEqual(parseStatus("garbage"), { authenticated: false, host: null });
});

test("normalizeUser / normalizeSpaces / normalizeTypes / normalizeViews", () => {
  assert.deepEqual(
    normalizeUser([
      {
        avatar_url: "https://x/a.png",
        email: "fay@example.com",
        name_cn: "fay-李丰豪",
        name_en: "fay",
        user_key: "7481570191529279507",
        username: "7481570191529279507",
      },
    ]),
    { key: "7481570191529279507", name: "fay-李丰豪", email: "fay@example.com", avatarUrl: "https://x/a.png" }
  );
  assert.equal(normalizeUser([]), null);

  assert.deepEqual(
    normalizeSpaces({
      pagination: { has_more: false, page_num: 1, page_size: 50, total: 1 },
      projects: [{ name: "FX", project_key: "67e9", simple_name: "fanruan-fx" }],
    }),
    { spaces: [{ key: "67e9", name: "FX", simpleName: "fanruan-fx" }], hasMore: false }
  );
  // 查不到空间时 CLI 直接回 null
  assert.deepEqual(normalizeSpaces(null), { spaces: [], hasMore: false });

  assert.deepEqual(
    normalizeTypes({
      list: [
        { api_name: "story", is_disable: 2, name: "需求", type_key: "story" },
        { api_name: "sprint", is_disable: 1, name: "迭代", type_key: "sprint" },
      ],
    }),
    [
      { key: "story", name: "需求", apiName: "story", disabled: false },
      { key: "sprint", name: "迭代", apiName: "sprint", disabled: true },
    ]
  );

  assert.deepEqual(normalizeViews([{ view_id: "26oZ-EaHR", view_name: "全部" }], { key: "story", name: "需求" }), [
    { id: "26oZ-EaHR", name: "全部", typeKey: "story", typeName: "需求" },
  ]);
  assert.deepEqual(normalizeViews({ data: null }, { key: "story", name: "需求" }), []);
});

test("MQL 信封值与行归一化", () => {
  assert.equal(mqlValueText({ value_type: "string_value", value: { string_value: "x" } }), "x");
  assert.equal(mqlValueText({ value_type: "long_value", value: { long_value: 7112390164 } }), "7112390164");
  assert.equal(
    mqlValueText({ value_type: "key_label_value", value: { key_label_value: { key: "issue", label: "缺陷" } } }),
    "缺陷"
  );
  assert.equal(
    mqlValueText({
      value_type: "key_label_value_list",
      value: { key_label_value_list: [{ key: "a", label: "组员开发" }, { key: "b", label: "验收" }] },
    }),
    "组员开发、验收"
  );
  assert.equal(
    mqlValueText({ value_type: "user_value", value: { user_value: { name_cn: "赵兴佩", name_en: "zxp" } } }),
    "赵兴佩"
  );
  assert.equal(mqlValueText({ value_type: "weird", value: { blob: {} } }), undefined);

  const rows = normalizeMqlRows({
    data: {
      "1": [
        {
          moql_field_list: [
            { key: "name", value: { string_value: "登录问题" }, value_type: "string_value" },
            {
              key: "work_item_status",
              value: { key_label_value_list: [{ key: "_v", label: "组员开发" }] },
              value_type: "key_label_value_list",
            },
            { key: "work_item_id", value: { long_value: 7112390164 }, value_type: "long_value" },
            { key: "updated_at", value: { string_value: "2026-09-10" }, value_type: "string_value" },
          ],
        },
        { moql_field_list: [{ key: "name", value: { string_value: "没 id 的行" }, value_type: "string_value" }] },
      ],
    },
    list: null,
  });
  assert.deepEqual(rows, [{ id: "7112390164", name: "登录问题", status: "组员开发", updatedAt: "2026-09-10" }]);
  // 空结果：data 是 {}
  assert.deepEqual(normalizeMqlRows({ data: {}, list: null }), []);
});

const ATTR = {
  create_by: { email: "roxy@example.com", key: "7496", name: "Roxy-杨子静" },
  create_time: "2025-08-08T15:36:08+08:00",
  owned_project: { key: "67d7", name: "一体化产研团队", simple_name: "b2rl2h" },
  role_members: [
    { key: "role_fe0eb9", name: "测试者" },
    { key: "role_a367e7", members: [{ email: "fay@example.com", key: "7481", name: "fay-李丰豪" }], name: "开发组长" },
  ],
  template: { id: 2771065, name: "改良" },
  update_time: "2026-09-10T07:49:09+08:00",
  updated_by: { email: "lipei@example.com", key: "7526", name: "Lipei-李培" },
  work_item_id: "6453102417",
  work_item_mod: "节点流",
  work_item_name: "图表模糊问题",
  work_item_status: { key: "0CRPpMACM", name: "开发组长" },
  work_item_type: { key: "67da6360e9d810fd8008b7a4", name: "产研任务" },
};

test("work_item_attribute → 列表行 / 详情", () => {
  assert.deepEqual(normalizeAttribute(ATTR, "project.feishu.cn"), {
    id: "6453102417",
    name: "图表模糊问题",
    spaceKey: "67d7",
    spaceName: "一体化产研团队",
    typeKey: "67da6360e9d810fd8008b7a4",
    typeName: "产研任务",
    status: "开发组长",
    url: "https://project.feishu.cn/b2rl2h/67da6360e9d810fd8008b7a4/detail/6453102417",
    updatedAt: "2026-09-10T07:49:09+08:00",
  });
  // 站点未知就没有链接，别拼出 https://null/…
  assert.equal(normalizeAttribute(ATTR, null)?.url, undefined);
  assert.equal(workItemUrl("h", "", "t", "1"), undefined);

  const detail = normalizeDetail(
    {
      work_item_attribute: ATTR,
      work_item_current_node: [
        { actual_begin_time: "2025-11-06T10:05:38+08:00", id: "state_0", name: "开发组长", owners: [{ key: "7481", name: "fay-李丰豪" }] },
      ],
      work_item_fields: [
        { key: "business", name: "业务线", value: "67ece7e8" },
        { key: "current_status_operator", name: "当前负责人", value: [{ key: "7481", name: "fay-李丰豪" }] },
        { key: "description", name: "描述", value: "运营反馈有图表模糊<!-- x -->" },
        { key: "priority", name: "优先级", value: { label: "一般紧急", value: "option_2" } },
      ],
    },
    "project.feishu.cn"
  );
  assert.ok(detail);
  assert.equal(detail.simpleName, "b2rl2h");
  assert.equal(detail.mode, "节点流");
  assert.equal(detail.template, "改良");
  assert.equal(detail.priority, "一般紧急");
  assert.equal(detail.description, "运营反馈有图表模糊");
  assert.equal(detail.createdBy, "Roxy-杨子静");
  assert.equal(detail.updatedBy, "Lipei-李培");
  assert.deepEqual(detail.currentNodes, [{ name: "开发组长", owners: ["fay-李丰豪"] }]);
  assert.deepEqual(detail.operators, ["fay-李丰豪"]);
  // 没人的角色不列
  assert.deepEqual(detail.roles, [{ name: "开发组长", members: ["fay-李丰豪"] }]);
  assert.equal(normalizeDetail({ work_item_attribute: null }, "h"), null);
});

test("plainDescription 把富文本 Markdown 里的图片与注释收掉", () => {
  const md =
    '![](https://project.feishu.cn/goapi/v1/tos/file/x.png?isSaas=1)<!--image:{"width":790,"uuid":"79BD"} -->\n\n\n\n这里不是删除字段，是将其移除字段栏  \n**加粗**保留';
  assert.equal(plainDescription(md), "[图片]\n\n这里不是删除字段，是将其移除字段栏\n**加粗**保留");
  assert.equal(plainDescription("  \n"), "");
});

test("view get 分页", () => {
  const page = normalizeViewItems(
    {
      pagination: { has_more: true, page_num: 1, page_size: 50, total: 3576 },
      work_item_list: [{ work_item_attribute: ATTR }, { work_item_attribute: null }],
    },
    "project.feishu.cn",
    1
  );
  assert.equal(page.items.length, 1);
  assert.equal(page.hasMore, true);
  assert.equal(page.total, 3576);
  assert.deepEqual(normalizeViewItems(null, "h", 3), { items: [], page: 3, hasMore: false });
});

test("mywork 行归一化与翻页判断", () => {
  const row = (id: number, extra: Record<string, unknown> = {}) => ({
    node_info: { node_name: "开发组长", node_state_key: "node_state_0" },
    project_key: "67d7",
    project_name: "一体化产研团队",
    schedule: { end_time: "", start_time: "" },
    state_info: { end_state_key_name: "", start_state_key_name: "开发评估" },
    work_item_info: { work_item_id: id, work_item_name: "", work_item_type_key: "issue" },
    ...extra,
  });
  const one = normalizeTodo({ list: [row(1, { schedule: null, finish_time: { finish_time: "2026-09-09 17:59" } })], total: 132 }, 1);
  assert.deepEqual(one.items, [
    {
      id: "1",
      name: "",
      spaceKey: "67d7",
      spaceName: "一体化产研团队",
      typeKey: "issue",
      nodeName: "开发组长",
      stateName: "开发评估",
      scheduleStart: undefined,
      scheduleEnd: undefined,
      finishedAt: "2026-09-09 17:59",
    },
  ]);
  assert.equal(one.hasMore, false);
  assert.equal(one.total, 132);

  const full = normalizeTodo({ list: Array.from({ length: 50 }, (_, i) => row(i + 1)), total: 132 }, 1);
  assert.equal(full.hasMore, true);
  assert.equal(normalizeTodo({ list: Array.from({ length: 50 }, (_, i) => row(i + 1)), total: 100 }, 2).hasMore, false);
  // this_week 没数据时 list 是 null
  assert.deepEqual(normalizeTodo({ list: null, total: 0 }, 1), { items: [], page: 1, hasMore: false, total: 0 });
});

test("groupForLookup 按 空间×类型 分组并去重 / chunk", () => {
  const groups = groupForLookup([
    { spaceKey: "p1", typeKey: "issue", id: "1" },
    { spaceKey: "p1", typeKey: "issue", id: "1" },
    { spaceKey: "p1", typeKey: "story", id: "2" },
    { spaceKey: "p2", typeKey: "issue", id: "3" },
    { spaceKey: "bad key", typeKey: "issue", id: "4" },
    { spaceKey: "p1", typeKey: "issue", id: "abc" },
  ]);
  assert.deepEqual(groups, [
    { spaceKey: "p1", typeKey: "issue", ids: ["1"] },
    { spaceKey: "p1", typeKey: "story", ids: ["2"] },
    { spaceKey: "p2", typeKey: "issue", ids: ["3"] },
  ]);
  assert.deepEqual(chunk([1, 2, 3, 4, 5], 2), [[1, 2], [3, 4], [5]]);
  assert.deepEqual(chunk([], 2), []);
});

test("bundledBinName 与 npm 包的 bin/ 命名一致", async () => {
  const { bundledBinName, resolveMeegleBin } = await import("./bin.js");
  assert.equal(bundledBinName("darwin", "arm64"), "meegle-darwin-arm64");
  assert.equal(bundledBinName("win32", "x64"), "meegle-win32-x64.exe");
  // 显式指定优先于一切
  assert.equal(resolveMeegleBin({ FALCON_MEEGLE_BIN: "/x/meegle" }), "/x/meegle");
  // 依赖装着：解析到包里本平台的二进制
  assert.match(resolveMeegleBin({}), /@lark-project\/meegle\/bin\/meegle-/);
});

test("isValidUrl 只收干净的 http(s) 链接", () => {
  assert.ok(isValidUrl("https://project.feishu.cn/b2rl2h/story/detail/1?node=2"));
  for (const bad of ["", "ftp://x", "https://a b", "https://x\ny", "http://" + "a".repeat(2100), 1]) {
    assert.equal(isValidUrl(bad), false, String(bad));
  }
});

test("parseUrlTarget 认三类页面，其余给原因", () => {
  const dec = (extra: Record<string, unknown>) => ({ host: "project.feishu.cn", simple_name: "b2rl2h", ...extra });
  assert.deepEqual(parseUrlTarget(dec({ url_kind: "workitem_detail", work_item_type: "story", work_item_id: "7072286406" })), {
    ok: true,
    target: { kind: "workitem", host: "project.feishu.cn", simpleName: "b2rl2h", typeKey: "story", id: "7072406".replace("406", "286406") },
  });
  assert.deepEqual(parseUrlTarget(dec({ url_kind: "view_multi_project", view_id: "3oX4fFoDg" })), {
    ok: true,
    target: { kind: "multiProjectView", host: "project.feishu.cn", simpleName: "b2rl2h", viewId: "3oX4fFoDg" },
  });
  // storyView / issueView / workObjectView 都是按类型的视图，带 work_item_type
  assert.deepEqual(parseUrlTarget(dec({ url_kind: "view_issue", view_id: "uFdSs-8DR", work_item_type: "issue" })), {
    ok: true,
    target: { kind: "view", host: "project.feishu.cn", simpleName: "b2rl2h", viewId: "uFdSs-8DR", typeKey: "issue" },
  });
  assert.equal(parseUrlTarget(dec({ url_kind: "view_workitem", view_id: "abc", work_item_type: "67da" })).ok, true);
  // 图表 / 甘特 / 总览带 view_id 但开不了
  for (const kind of ["view_chart", "view_user_gantt", "view_project_overview"]) {
    const r = parseUrlTarget(dec({ url_kind: kind, view_id: "x" }));
    assert.ok(!r.ok && r.message.includes(kind), kind);
  }
  const unknown = parseUrlTarget({ url_kind: "unknown", host: "project.feishu.cn" });
  assert.ok(!unknown.ok && unknown.message === "认不出这个链接指向什么");
  const home = parseUrlTarget(dec({ url_kind: "workitem_homepage", work_item_type: "story" }));
  assert.ok(!home.ok && home.message.includes("workitem_homepage"));
  assert.equal(parseUrlTarget("garbage").ok, false);
});

test("normalizeMultiViewItems：只有骨架，状态留给 enrich", () => {
  const page = normalizeMultiViewItems(
    {
      data: [
        { name: "图表增量更新", project_key: "67d7", work_item_id: 7043546793, work_item_type_key: "67da" },
        { name: "no id" },
      ],
      pagination: { has_more: false, page_num: 1, page_size: 50, total: 2 },
    },
    1
  );
  assert.deepEqual(page, {
    items: [{ id: "7043546793", name: "图表增量更新", spaceKey: "67d7", typeKey: "67da" }],
    page: 1,
    hasMore: false,
    total: 2,
  });
  assert.deepEqual(normalizeMultiViewItems(null, 2), { items: [], page: 2, hasMore: false });
});
