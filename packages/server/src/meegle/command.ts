/**
 * meegle CLI（@lark-project/meegle）的命令构造与输出解析。
 *
 * 与 git/command.ts 同一分层：**纯函数，零 I/O**。不同的是这里只产出 argv 数组、
 * 从不拼命令行字符串——CLI 只在 falcon 后端所在的机器上跑（登录态存在那台机器的
 * 钥匙串里，远端主机上没有），直接 spawn 不经 shell，没有转义问题。
 *
 * 实测过的几条 CLI 脾气（1.0.9 与内置的 1.0.23 行为一致），代码里的判断都来自这些：
 * - 成功的 JSON 走 stdout；业务失败是 `{data:null, error:{code,message}}` 的 JSON，
 *   **打到 stderr**、退出码 1（`auth status` 未登录时同样退出码 1 但 JSON 在 stdout）。
 *   所以两条流都要试着当 JSON 解析，退出码只是旁证。
 * - 命令清单是登录后从服务端拉的（缓存在 ~/.meegle/cache）。**未登录时任何业务
 *   命令都报 `unknown command`**，与真正打错命令无法区分，得再问一次 auth status。
 * - `view search` 三个参数都必填，关键字是子串匹配，没有"列出全部视图"的路子。
 * - `mywork todo` 返回的 work_item_name 是空串，名字要靠 MQL 按 ID 补。
 * - MQL 的 FROM 可以直接用 project_key 与 type_key，不必换成空间名 / 类型名；
 *   单引号按 SQL 规矩双写即可。
 * - 用户输入一律走 `--flag=value` 形式：值以 `-` 开头时（关键字"-foo"）
 *   `--flag value` 会被 pflag 当成下一个 flag。
 */

import type {
  MeegleLogin,
  MeeglePage,
  MeegleSpace,
  MeegleTodoAction,
  MeegleTodoItem,
  MeegleUser,
  MeegleView,
  MeegleWorkItem,
  MeegleWorkItemDetail,
  MeegleWorkItemType,
} from "@falcon/shared";

/** 交互式启动时的更新提示会往 stdout 里塞非 JSON，明确关掉 */
export const CLI_ENV: Record<string, string> = { MEEGLE_NO_UPDATE_CHECK: "1" };

/** CLI 的固定页长（project search / view get / mywork todo / MQL 都是 50） */
export const CLI_PAGE_SIZE = 50;

export const TODO_ACTIONS: readonly MeegleTodoAction[] = ["todo", "this_week", "overdue", "done"];

export function isTodoAction(v: unknown): v is MeegleTodoAction {
  return typeof v === "string" && (TODO_ACTIONS as readonly string[]).includes(v);
}

/** 站点域名：只放行主机名字符，`meegle auth login --host` 后面不该出现别的 */
export function isValidHost(v: unknown): v is string {
  return (
    typeof v === "string" &&
    /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$/i.test(v)
  );
}

/** 空间 key / 类型 key / 视图 id 都是 URL 安全的短串；放行别的就等于把任意 argv 交给 CLI */
export function isValidKey(v: unknown): v is string {
  return typeof v === "string" && /^[A-Za-z0-9_][A-Za-z0-9_-]{0,63}$/.test(v);
}

export function isValidId(v: unknown): v is string {
  return typeof v === "string" && /^\d{1,20}$/.test(v);
}

/** 粘贴进来的飞书项目链接：只收 http(s)，不含空白 / 控制字符，长度封顶 */
export function isValidUrl(v: unknown): v is string {
  return (
    typeof v === "string" &&
    v.length <= 2048 &&
    // eslint-disable-next-line no-control-regex
    /^https?:\/\/[^\s\u0000-\u001f]+$/.test(v)
  );
}

// ---- argv ----

export function statusArgs(): string[] {
  return ["auth", "status", "--format", "json"];
}

export function versionArgs(): string[] {
  return ["version"];
}

/** device-code 模式：不开浏览器，把授权链接打到 stdout，后端抓出来交给前端 */
export function loginArgs(host: string): string[] {
  return ["auth", "login", `--host=${host}`, "--device-code"];
}

export function meArgs(): string[] {
  return ["user", "search", "--user-keys=current_login_user()", "--format", "json"];
}

/** 不带关键字 = 当前用户最近访问过的空间（按访问时间由近及远） */
export function spacesArgs(keyword?: string, page = 1): string[] {
  const args = ["project", "search"];
  if (keyword) args.push(`--project-key=${keyword}`);
  args.push(`--page-num=${page}`, "--format", "json");
  return args;
}

export function typesArgs(spaceKey: string): string[] {
  return ["workitem", "meta-types", `--project-key=${spaceKey}`, "--format", "json"];
}

export function viewSearchArgs(spaceKey: string, typeKey: string, keyword: string): string[] {
  return [
    "view",
    "search",
    `--project-key=${spaceKey}`,
    `--view-scope=${typeKey}`,
    `--key-word=${keyword}`,
    "--format",
    "json",
  ];
}

export function viewItemsArgs(spaceKey: string, viewId: string, page: number): string[] {
  return [
    "view",
    "get",
    `--project-key=${spaceKey}`,
    `--view-id=${viewId}`,
    `--page-num=${page}`,
    "--format",
    "json",
  ];
}

export function todoArgs(action: MeegleTodoAction, page: number): string[] {
  return ["mywork", "todo", `--action=${action}`, `--page-num=${page}`, "--format", "json"];
}

export function queryArgs(spaceKey: string, mql: string): string[] {
  return ["workitem", "query", `--project-key=${spaceKey}`, `--mql=${mql}`, "--format", "json"];
}

/** 全景视图（multiProjectView）里当前用户有权限看到的工作项 */
export function multiViewItemsArgs(spaceKey: string, viewId: string, page: number): string[] {
  return [
    "view",
    "list-multi-project-workitems",
    `--project-key=${spaceKey}`,
    `--view-id=${viewId}`,
    `--page-num=${page}`,
    "--format",
    "json",
  ];
}

/** 纯本地解析飞书项目链接，不走网络；路由表在 CLI 里，别自己拆路径 */
export function urlDecodeArgs(url: string): string[] {
  return ["url", "decode", `--url=${url}`, "--format", "json"];
}

/** 不传 --fields：默认就带 description / priority / current_status_operator，够详情页用 */
export function workItemArgs(spaceKey: string, id: string): string[] {
  return [
    "workitem",
    "get",
    `--project-key=${spaceKey}`,
    `--work-item-id=${id}`,
    "--format",
    "json",
  ];
}

// ---- MQL ----

const MQL_FIELDS = "`work_item_id`, `name`, `work_item_status`, `updated_at`";

/** 字符串字面量：单引号双写；控制字符没有任何合法用途，换成空格 */
export function mqlLiteral(s: string): string {
  // eslint-disable-next-line no-control-regex
  return "'" + s.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/'/g, "''") + "'";
}

/** 标识符：反引号包裹。key 都经过 isValidKey，这里只是兜底把反引号剥掉 */
export function mqlIdent(s: string): string {
  // eslint-disable-next-line no-control-regex
  return "`" + s.replace(/[`\u0000-\u001f\u007f]/g, "") + "`";
}

function mqlFrom(spaceKey: string, typeKey: string): string {
  return `FROM ${mqlIdent(spaceKey)}.${mqlIdent(typeKey)}`;
}

/** 按名称子串搜；`%` / `_` 是 LIKE 通配符，用户要是故意打了就让它当通配符 */
export function mqlSearch(spaceKey: string, typeKey: string, keyword: string, limit = 20): string {
  return (
    `SELECT ${MQL_FIELDS} ${mqlFrom(spaceKey, typeKey)} ` +
    `WHERE \`name\` LIKE ${mqlLiteral(`%${keyword}%`)} ` +
    `ORDER BY \`work_item_id\` DESC LIMIT ${clampLimit(limit)}`
  );
}

/** 类型下最新创建的工作项。id 单调递增，按它倒序就是"最近" */
export function mqlRecent(spaceKey: string, typeKey: string, limit = 30): string {
  return (
    `SELECT ${MQL_FIELDS} ${mqlFrom(spaceKey, typeKey)} ` +
    `ORDER BY \`work_item_id\` DESC LIMIT ${clampLimit(limit)}`
  );
}

/** 按 ID 批量补名字 / 状态。ids 已经过 isValidId，非法的直接丢 */
export function mqlByIds(spaceKey: string, typeKey: string, ids: string[]): string {
  const clean = ids.filter(isValidId);
  return (
    `SELECT ${MQL_FIELDS} ${mqlFrom(spaceKey, typeKey)} ` +
    `WHERE \`work_item_id\` IN (${clean.join(", ")}) LIMIT ${clampLimit(clean.length)}`
  );
}

function clampLimit(n: number): number {
  return Math.max(1, Math.min(CLI_PAGE_SIZE, Math.floor(n)));
}

export function chunk<T>(arr: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

// ---- 输出解析 ----

export type CliOutcome =
  | { ok: true; data: unknown }
  | { ok: false; code: string; message: string };

type Obj = Record<string, unknown>;

function isObj(v: unknown): v is Obj {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | undefined {
  if (typeof v === "string") return v || undefined;
  if (typeof v === "number") return String(v);
  return undefined;
}

/**
 * CLI 的两条输出流 → 结果或错误。
 *
 * 成功是 stdout 上任意 JSON（对象 / 数组 / 甚至 `null`——查不到空间时就回 null）；
 * 失败是 stderr 上的 `{data:null, error:{code,message}}`（stdout 为空）；打错命令 /
 * 未登录是 stdout 上一行纯文本 `unknown command "x" for "meegle"`。先看 stdout，
 * 空了再看 stderr；两边都不是 JSON 才按文本报。
 */
export function parseCliJson(stdout: string, stderr: string): CliOutcome {
  const text = stdout.trim() || stderr.trim();
  if (text) {
    try {
      const data: unknown = JSON.parse(text);
      if (isObj(data) && isObj(data.error)) {
        const err = data.error;
        return {
          ok: false,
          code: str(err.code) ?? "CLI_ERROR",
          message: cliErrorText(str(err.message) ?? ""),
        };
      }
      // stderr 上的 JSON 只可能是错误信封；不是就当异常输出报出去
      if (!stdout.trim()) return { ok: false, code: "BAD_OUTPUT", message: firstLine(text) };
      return { ok: true, data };
    } catch {
      // 不是 JSON，落到下面按文本处理
    }
  }
  const line = firstLine(text);
  if (/^unknown command/i.test(line)) return { ok: false, code: "UNKNOWN_COMMAND", message: line };
  return { ok: false, code: "BAD_OUTPUT", message: line || "meegle 没有输出" };
}

export function firstLine(text: string): string {
  return text.trim().split(/\r?\n/)[0]?.trim() ?? "";
}

/**
 * 服务端错误长这样：`error=ErrViewNotExist,message=view not exist,retriable=false\nlogid: …`，
 * 业务错误再套一层 `message=Service Internal Error,biz error: xxx,retriable=true`。
 * 给用户看的是最里层那句；抽不出来就原样给第一行。
 */
export function cliErrorText(raw: string): string {
  const line = firstLine(raw);
  const biz = /biz error:\s*(.+?)(?:,retriable=|$)/.exec(line);
  if (biz) return biz[1].trim();
  const msg = /message=(.+?)(?:,retriable=|$)/.exec(line);
  if (msg) return msg[1].trim();
  return line;
}

/** `auth login --device-code` 的提示：抓 URL 与授权码，二维码那堆块字符不要 */
export function parseLoginPrompt(text: string): Omit<MeegleLogin, "host"> | null {
  const url = /URL:\s*(https?:\/\/\S+)/.exec(text)?.[1];
  if (!url) return null;
  const code = /Authorization code:\s*(\S+)/.exec(text)?.[1] ?? "";
  return { url, code };
}

export interface ParsedStatus {
  authenticated: boolean;
  host: string | null;
  expiresInMinutes?: number;
}

export function parseStatus(data: unknown): ParsedStatus {
  if (!isObj(data)) return { authenticated: false, host: null };
  const host = str(data.host) ?? null;
  const mins = data.expires_in_minutes;
  return {
    authenticated: data.authenticated === true,
    host,
    expiresInMinutes: typeof mins === "number" ? mins : undefined,
  };
}

// ---- 归一化 ----

export function normalizeUser(data: unknown): MeegleUser | null {
  const row = Array.isArray(data) ? data[0] : data;
  if (!isObj(row)) return null;
  const key = str(row.user_key) ?? str(row.username);
  if (!key) return null;
  return {
    key,
    name: str(row.name_cn) ?? str(row.name_en) ?? key,
    email: str(row.email),
    avatarUrl: str(row.avatar_url),
  };
}

export function normalizeSpaces(data: unknown): { spaces: MeegleSpace[]; hasMore: boolean } {
  if (!isObj(data)) return { spaces: [], hasMore: false };
  const list = Array.isArray(data.projects) ? data.projects : [];
  const spaces: MeegleSpace[] = [];
  for (const row of list) {
    if (!isObj(row)) continue;
    const key = str(row.project_key);
    if (!key) continue;
    spaces.push({
      key,
      name: str(row.name) ?? key,
      simpleName: str(row.simple_name) ?? "",
    });
  }
  const pagination = isObj(data.pagination) ? data.pagination : {};
  return { spaces, hasMore: pagination.has_more === true };
}

/** is_disable：实测 2 = 启用、1 = 停用（与字段名的字面意思相反，别猜） */
export function normalizeTypes(data: unknown): MeegleWorkItemType[] {
  if (!isObj(data) || !Array.isArray(data.list)) return [];
  const out: MeegleWorkItemType[] = [];
  for (const row of data.list) {
    if (!isObj(row)) continue;
    const key = str(row.type_key);
    if (!key) continue;
    out.push({
      key,
      name: str(row.name) ?? key,
      apiName: str(row.api_name) ?? key,
      disabled: row.is_disable === 1,
    });
  }
  return out;
}

export function normalizeViews(data: unknown, type: { key: string; name: string }): MeegleView[] {
  if (!Array.isArray(data)) return [];
  const out: MeegleView[] = [];
  for (const row of data) {
    if (!isObj(row)) continue;
    const id = str(row.view_id);
    if (!id) continue;
    out.push({ id, name: str(row.view_name) ?? id, typeKey: type.key, typeName: type.name });
  }
  return out;
}

export interface MqlRow {
  id: string;
  name: string;
  status?: string;
  updatedAt?: string;
}

/**
 * MQL 的值是带类型标签的信封：`{value_type:"string_value", value:{string_value:"…"}}`。
 * 这里把常见几种压成一段文本；没见过的类型返回 undefined，别硬 String() 出 [object Object]。
 */
export function mqlValueText(cell: unknown): string | undefined {
  if (!isObj(cell) || !isObj(cell.value)) return undefined;
  const v = cell.value;
  if (typeof v.string_value === "string") return v.string_value || undefined;
  if (typeof v.long_value === "number") return String(v.long_value);
  if (typeof v.double_value === "number") return String(v.double_value);
  if (typeof v.bool_value === "boolean") return v.bool_value ? "true" : "false";
  if (isObj(v.key_label_value)) return str(v.key_label_value.label);
  if (Array.isArray(v.key_label_value_list)) {
    const labels = v.key_label_value_list
      .map((x) => (isObj(x) ? str(x.label) : undefined))
      .filter((x): x is string => Boolean(x));
    return labels.length ? labels.join("、") : undefined;
  }
  if (isObj(v.user_value)) return str(v.user_value.name_cn) ?? str(v.user_value.name_en);
  if (Array.isArray(v.user_value_list)) {
    const names = v.user_value_list
      .map((x) => (isObj(x) ? (str(x.name_cn) ?? str(x.name_en)) : undefined))
      .filter((x): x is string => Boolean(x));
    return names.length ? names.join("、") : undefined;
  }
  return undefined;
}

/** `workitem query` 的 data 是 分组 id → 行数组；每行是 moql_field_list */
export function normalizeMqlRows(data: unknown): MqlRow[] {
  if (!isObj(data) || !isObj(data.data)) return [];
  const rows: MqlRow[] = [];
  for (const group of Object.values(data.data)) {
    if (!Array.isArray(group)) continue;
    for (const row of group) {
      if (!isObj(row) || !Array.isArray(row.moql_field_list)) continue;
      const cells = new Map<string, unknown>();
      for (const cell of row.moql_field_list) {
        if (isObj(cell) && typeof cell.key === "string") cells.set(cell.key, cell);
      }
      const id = mqlValueText(cells.get("work_item_id"));
      if (!id) continue;
      rows.push({
        id,
        name: mqlValueText(cells.get("name")) ?? "",
        status: mqlValueText(cells.get("work_item_status")),
        updatedAt: mqlValueText(cells.get("updated_at")),
      });
    }
  }
  return rows;
}

export function workItemUrl(
  host: string | null,
  simpleName: string | undefined,
  typeKey: string,
  id: string
): string | undefined {
  if (!host || !simpleName) return undefined;
  return `https://${host}/${simpleName}/${typeKey}/detail/${id}`;
}

function personName(v: unknown): string | undefined {
  return isObj(v) ? (str(v.name) ?? str(v.name_cn) ?? str(v.name_en)) : undefined;
}

function personNames(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  return v.map(personName).filter((x): x is string => Boolean(x));
}

/** `work_item_attribute`（workitem get / view get 共用）→ 列表行 */
export function normalizeAttribute(attr: unknown, host: string | null): MeegleWorkItem | null {
  if (!isObj(attr)) return null;
  const id = str(attr.work_item_id);
  if (!id) return null;
  const project = isObj(attr.owned_project) ? attr.owned_project : {};
  const type = isObj(attr.work_item_type) ? attr.work_item_type : {};
  const typeKey = str(type.key) ?? "";
  return {
    id,
    name: str(attr.work_item_name) ?? "",
    spaceKey: str(project.key) ?? "",
    spaceName: str(project.name),
    typeKey,
    typeName: str(type.name),
    status: isObj(attr.work_item_status) ? str(attr.work_item_status.name) : undefined,
    url: workItemUrl(host, str(project.simple_name), typeKey, id),
    updatedAt: str(attr.update_time),
  };
}

export function normalizeViewItems(
  data: unknown,
  host: string | null,
  page: number
): MeeglePage<MeegleWorkItem> {
  if (!isObj(data)) return { items: [], page, hasMore: false };
  const list = Array.isArray(data.work_item_list) ? data.work_item_list : [];
  const items: MeegleWorkItem[] = [];
  for (const row of list) {
    const item = isObj(row) ? normalizeAttribute(row.work_item_attribute, host) : null;
    if (item) items.push(item);
  }
  const pagination = isObj(data.pagination) ? data.pagination : {};
  return {
    items,
    page,
    hasMore: pagination.has_more === true,
    total: typeof pagination.total === "number" ? pagination.total : undefined,
  };
}

/** mywork 的一行：名字是空的，得再补；这里先把它自己有的东西取干净 */
export function normalizeTodo(data: unknown, page: number): MeeglePage<MeegleTodoItem> {
  if (!isObj(data)) return { items: [], page, hasMore: false };
  const list = Array.isArray(data.list) ? data.list : [];
  const items: MeegleTodoItem[] = [];
  for (const row of list) {
    if (!isObj(row)) continue;
    const info = isObj(row.work_item_info) ? row.work_item_info : {};
    const id = str(info.work_item_id);
    if (!id) continue;
    const node = isObj(row.node_info) ? row.node_info : {};
    const state = isObj(row.state_info) ? row.state_info : {};
    const schedule = isObj(row.schedule) ? row.schedule : {};
    const finish = isObj(row.finish_time) ? row.finish_time : {};
    items.push({
      id,
      name: str(info.work_item_name) ?? "",
      spaceKey: str(row.project_key) ?? "",
      spaceName: str(row.project_name),
      typeKey: str(info.work_item_type_key) ?? "",
      nodeName: str(node.node_name),
      stateName: str(state.start_state_key_name),
      scheduleStart: str(schedule.start_time),
      scheduleEnd: str(schedule.end_time),
      finishedAt: str(finish.finish_time),
    });
  }
  const total = typeof data.total === "number" ? data.total : undefined;
  // 没有 has_more 字段：满一页且 total 说后面还有，才算有下一页
  const hasMore =
    list.length >= CLI_PAGE_SIZE && (total === undefined || page * CLI_PAGE_SIZE < total);
  return { items, page, hasMore, total };
}

/** `view list-multi-project-workitems`：每行只有名字 / 空间 / id / 类型 key，状态要另补 */
export function normalizeMultiViewItems(data: unknown, page: number): MeeglePage<MeegleWorkItem> {
  if (!isObj(data)) return { items: [], page, hasMore: false };
  const list = Array.isArray(data.data) ? data.data : [];
  const items: MeegleWorkItem[] = [];
  for (const row of list) {
    if (!isObj(row)) continue;
    const id = str(row.work_item_id);
    if (!id) continue;
    items.push({
      id,
      name: str(row.name) ?? "",
      spaceKey: str(row.project_key) ?? "",
      typeKey: str(row.work_item_type_key) ?? "",
    });
  }
  const pagination = isObj(data.pagination) ? data.pagination : {};
  return {
    items,
    page,
    hasMore: pagination.has_more === true,
    total: typeof pagination.total === "number" ? pagination.total : undefined,
  };
}

export type ParsedUrl =
  | { kind: "workitem"; host: string; simpleName: string; typeKey: string; id: string }
  | { kind: "view"; host: string; simpleName: string; viewId: string; typeKey?: string }
  | { kind: "multiProjectView"; host: string; simpleName: string; viewId: string };

/**
 * 图表 / 甘特 / 空间总览也带 view_id，但 `view get` 取不出工作项，不当视图开。
 * 面板能开的只有三类：工作项详情、按类型的视图（storyView / issueView / workObjectView）、全景视图。
 */
const VIEW_KINDS_NOT_OPENABLE = new Set(["view_chart", "view_user_gantt", "view_project_overview"]);

/** `url decode` 的输出 → 面板认识的三类目标；认不出或不支持就给一句能看懂的原因 */
export function parseUrlTarget(decoded: unknown): { ok: true; target: ParsedUrl } | { ok: false; message: string } {
  if (!isObj(decoded)) return { ok: false, message: "链接解析失败" };
  const kind = str(decoded.url_kind) ?? "unknown";
  const host = str(decoded.host) ?? "";
  const simpleName = str(decoded.simple_name);
  const viewId = str(decoded.view_id);
  const id = str(decoded.work_item_id);
  const typeKey = str(decoded.work_item_type);
  if (kind === "workitem_detail" && simpleName && id && typeKey) {
    return { ok: true, target: { kind: "workitem", host, simpleName, typeKey, id } };
  }
  if (kind === "view_multi_project" && simpleName && viewId) {
    return { ok: true, target: { kind: "multiProjectView", host, simpleName, viewId } };
  }
  if (kind.startsWith("view_") && simpleName && viewId && !VIEW_KINDS_NOT_OPENABLE.has(kind)) {
    return { ok: true, target: { kind: "view", host, simpleName, viewId, typeKey } };
  }
  if (kind === "unknown") return { ok: false, message: "认不出这个链接指向什么" };
  return { ok: false, message: `这类页面打不开（${kind}）` };
}

/** 按 空间 × 类型 分组，MQL 一次只能查一张表 */
export function groupForLookup(
  items: { spaceKey: string; typeKey: string; id: string }[]
): { spaceKey: string; typeKey: string; ids: string[] }[] {
  const groups = new Map<string, { spaceKey: string; typeKey: string; ids: string[] }>();
  for (const it of items) {
    if (!isValidKey(it.spaceKey) || !isValidKey(it.typeKey) || !isValidId(it.id)) continue;
    const k = `${it.spaceKey} ${it.typeKey}`;
    let g = groups.get(k);
    if (!g) {
      g = { spaceKey: it.spaceKey, typeKey: it.typeKey, ids: [] };
      groups.set(k, g);
    }
    if (!g.ids.includes(it.id)) g.ids.push(it.id);
  }
  return [...groups.values()];
}

/**
 * 描述是飞书富文本编辑器导出的 Markdown 方言：图片是 `![](url)` 后面紧跟一段
 * `<!-- image:{…} -->` 注释，面板窄、又不渲染 Markdown，原样显示就是一坨 URL。
 * 图片换成占位、注释剥掉、多余空行压一压；其余标记（加粗、链接）留着，看得懂。
 */
export function plainDescription(md: string): string {
  return md
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "[图片]")
    .replace(/[ \t]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function normalizeDetail(data: unknown, host: string | null): MeegleWorkItemDetail | null {
  if (!isObj(data)) return null;
  const attr = isObj(data.work_item_attribute) ? data.work_item_attribute : null;
  const base = normalizeAttribute(attr, host);
  if (!attr || !base) return null;

  const fields = new Map<string, unknown>();
  if (Array.isArray(data.work_item_fields)) {
    for (const f of data.work_item_fields) {
      if (isObj(f) && typeof f.key === "string") fields.set(f.key, f.value);
    }
  }
  const priority = fields.get("priority");
  const description = fields.get("description");

  const currentNodes: { name: string; owners: string[] }[] = [];
  if (Array.isArray(data.work_item_current_node)) {
    for (const n of data.work_item_current_node) {
      if (!isObj(n)) continue;
      const name = str(n.name);
      if (name) currentNodes.push({ name, owners: personNames(n.owners) });
    }
  }

  const roles: { name: string; members: string[] }[] = [];
  if (Array.isArray(attr.role_members)) {
    for (const r of attr.role_members) {
      if (!isObj(r)) continue;
      const members = personNames(r.members);
      const name = str(r.name);
      if (name && members.length) roles.push({ name, members });
    }
  }

  const project = isObj(attr.owned_project) ? attr.owned_project : {};
  return {
    ...base,
    simpleName: str(project.simple_name),
    mode: str(attr.work_item_mod),
    template: isObj(attr.template) ? str(attr.template.name) : undefined,
    priority: isObj(priority) ? str(priority.label) : str(priority),
    description: typeof description === "string" ? plainDescription(description) || undefined : undefined,
    createdAt: str(attr.create_time),
    createdBy: personName(attr.create_by),
    updatedBy: personName(attr.updated_by),
    currentNodes,
    operators: personNames(fields.get("current_status_operator")),
    roles,
  };
}
