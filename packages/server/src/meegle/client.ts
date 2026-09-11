/**
 * meegle CLI 的执行层：起进程、超时、缓存、扇出，把 command.ts 归一化出来的东西
 * 组装成面板要的形状。所有 I/O 都在这一个文件里。
 *
 * 只跑在 falcon 后端所在的机器上（见 command.ts 顶部说明），环境用本地 PTY 同一套
 * 登录环境（sessions/loginEnv）：后端由 launchd / 服务守护拉起时 PATH 里往往没有
 * /opt/homebrew/bin 或 npm 全局目录，不这么做 `meegle` 就"未安装"。
 */

import { spawn, type ChildProcess } from "node:child_process";
import {
  MEEGLE_PAGE_SIZE,
  TtlCache,
  type MeegleLogin,
  type MeegleSearchResult,
  type MeegleUrlTarget,
  type MeegleSpace,
  type MeegleStatus,
  type MeegleTodoAction,
  type MeegleTodoItem,
  type MeegleUnavailableReason,
  type MeegleUser,
  type MeegleView,
  type MeegleWorkItem,
  type MeegleWorkItemDetail,
  type MeegleWorkItemType,
  type MeeglePage,
} from "@falcon/shared";
import { resolveLocalBaseEnv } from "../sessions/loginEnv.js";
import { resolveMeegleBin } from "./bin.js";
import { logicalPage } from "./pagination.js";
import {
  CLI_ENV,
  CLI_PAGE_SIZE,
  businessText,
  chunk,
  detailContext,
  fieldsArgs,
  firstLine,
  groupForLookup,
  isContextField,
  loginArgs,
  meArgs,
  mqlByIds,
  mqlNextArgs,
  mqlRecent,
  mqlSearch,
  multiViewItemsArgs,
  normalizeDetail,
  normalizeFields,
  normalizeMultiViewItems,
  normalizeMqlRows,
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
  queryArgs,
  spacesArgs,
  statusArgs,
  todoArgs,
  typesArgs,
  urlDecodeArgs,
  versionArgs,
  viewItemsArgs,
  viewSearchArgs,
  workItemArgs,
  workItemUrl,
  type MqlRow,
  type FieldMetadata,
} from "./command.js";

/** 一次 CLI 调用的上限。每条命令都是一次到飞书的网络往返，实测 0.5–2s，30s 已是病态 */
const CLI_TIMEOUT_MS = 30_000;
/** 登录进程从启动到打出授权链接：要先去服务端申请 device code */
const LOGIN_PROMPT_TIMEOUT_MS = 20_000;
/** 用户迟迟不授权就把登录进程收掉，别让它一直挂着 */
const LOGIN_MAX_MS = 10 * 60_000;
/** 查询结果默认 5 分钟；空间/类型变动少，类型单独加长。刷新按钮带 fresh 跳过 */
const CACHE_MS = 5 * 60_000;
const STATUS_CACHE_MS = 30_000;
/**
 * 同时起几个 CLI 进程：视图 / 工作项搜索要按类型扇出，十来个类型串行太慢，全开又太重。
 * 实测 `view search` 按租户限 5 qps（超了报 `rate limit, … qps: 5`），视图与工作项两路
 * 并行各 3 个、外加下面的限流重试，十几个类型的空间搜一次刚好不撞。
 */
const FANOUT = 3;
/** 撞限流后等多久再试；重试次数见 call() */
const RATE_LIMIT_BACKOFF_MS = 700;
const RATE_LIMIT_RETRIES = 2;
/** 搜索与最近列表均最多 100 条；MQL 的两页传输不等于两页 REST。 */
const SEARCH_CAP = MEEGLE_PAGE_SIZE;

export class MeegleError extends Error {
  constructor(
    /** bad-input：用户给的东西本身不对（链接不支持等），路由回 400 */
    public reason: MeegleUnavailableReason | "cli-error" | "bad-input",
    message: string,
    public code?: string
  ) {
    super(message);
  }
}

interface ExecResult {
  code: number | null;
  stdout: string;
  stderr: string;
  /** spawn 本身失败（ENOENT / 超时）；有它就别看 stdout */
  spawnError?: "ENOENT" | "TIMEOUT" | string;
}

export type MeegleQueryOpts = { fresh?: boolean };

interface Logger {
  info(msg: string): void;
  warn(msg: string): void;
}

export class MeegleClient {
  private readonly bin = resolveMeegleBin();
  private version: string | null = null;
  private host: string | null = null;
  /** 用户 / 空间 / 类型 / 待办 / 搜索 / 详情共用；登录换人时整表清掉 */
  private readonly cache = new TtlCache(CACHE_MS);
  private login: { proc: ChildProcess; prompt: MeegleLogin } | null = null;
  private loginStarting: Promise<MeegleLogin> | null = null;

  constructor(private readonly log: Logger) {
    log.info(`meegle CLI: ${this.bin}`);
  }

  // ---- 进程 ----

  private async env(): Promise<NodeJS.ProcessEnv> {
    return { ...(await resolveLocalBaseEnv()), ...CLI_ENV };
  }

  private async exec(args: string[], timeoutMs = CLI_TIMEOUT_MS): Promise<ExecResult> {
    const env = await this.env();
    return new Promise((resolve) => {
      // 攒 Buffer、结束时一次解码：逐 chunk toString 会切坏跨包的 UTF-8 多字节字符
      const stdout: Buffer[] = [];
      const stderr: Buffer[] = [];
      const out = () => ({
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
      let proc: ChildProcess;
      try {
        proc = spawn(this.bin, args, { env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
      } catch (err) {
        resolve({ code: null, stdout: "", stderr: String(err), spawnError: String(err) });
        return;
      }
      let done = false;
      const finish = (res: ExecResult) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        resolve(res);
      };
      const timer = setTimeout(() => {
        proc.kill();
        finish({ code: null, ...out(), spawnError: "TIMEOUT" });
      }, timeoutMs);
      proc.stdout?.on("data", (d: Buffer) => stdout.push(d));
      proc.stderr?.on("data", (d: Buffer) => stderr.push(d));
      proc.on("error", (err: NodeJS.ErrnoException) =>
        finish({ code: null, ...out(), spawnError: err.code === "ENOENT" ? "ENOENT" : String(err) })
      );
      proc.on("close", (code) => finish({ code, ...out() }));
    });
  }

  /** 跑一条业务命令，把 CLI 的三种失败翻成 MeegleError；撞了服务端限流就退避重试 */
  private async call(args: string[]): Promise<unknown> {
    for (let attempt = 0; ; attempt++) {
      const res = await this.exec(args);
      if (res.spawnError === "ENOENT") throw new MeegleError("not-installed", "meegle CLI 未安装");
      if (res.spawnError === "TIMEOUT") throw new MeegleError("cli-error", "meegle 超时没有响应");
      if (res.spawnError) throw new MeegleError("cli-error", res.spawnError);
      const out = parseCliJson(res.stdout, res.stderr);
      if (out.ok) return out.data;
      if (out.code === "UNKNOWN_COMMAND") {
        // 命令清单要登录后才从服务端拉得到：未登录时任何业务命令都是 unknown command
        const st = await this.probeStatus();
        if (!st.authenticated) throw new MeegleError("not-authenticated", "尚未登录飞书项目");
      }
      if (attempt < RATE_LIMIT_RETRIES && /rate limit/i.test(out.message)) {
        await new Promise((r) => setTimeout(r, RATE_LIMIT_BACKOFF_MS * (attempt + 1)));
        continue;
      }
      throw new MeegleError("cli-error", out.message, out.code);
    }
  }

  // ---- 状态 / 登录 ----

  private async probeStatus() {
    const res = await this.exec(statusArgs(), 15_000);
    if (res.spawnError === "ENOENT") return { installed: false, authenticated: false, host: null };
    const parsed = parseCliJson(res.stdout, res.stderr);
    const st = parsed.ok ? parseStatus(parsed.data) : { authenticated: false, host: null };
    this.host = st.host;
    return { installed: true, ...st };
  }

  async status(opts?: MeegleQueryOpts): Promise<MeegleStatus> {
    // 登录进行中每 2s 会变，不能吃缓存；授权完成后 loadStatus 里会 dropDataCache
    if (this.login) return this.loadStatus();
    return this.cache.getOrLoad("status", () => this.loadStatus(), { ...opts, ttlMs: STATUS_CACHE_MS });
  }

  private async loadStatus(): Promise<MeegleStatus> {
    const st = await this.probeStatus();
    const status: MeegleStatus = {
      installed: st.installed,
      authenticated: st.authenticated,
      host: st.host,
      expiresInMinutes: st.expiresInMinutes,
      bin: this.bin,
    };
    if (!st.installed) return status;
    const [version, user] = await Promise.all([
      this.cliVersion(),
      st.authenticated ? this.me().catch(() => undefined) : Promise.resolve(undefined),
    ]);
    if (version) status.version = version;
    if (user) status.user = user;
    if (this.login) status.login = this.login.prompt;
    return status;
  }

  private async cliVersion(): Promise<string | null> {
    if (this.version) return this.version;
    const res = await this.exec(versionArgs(), 10_000);
    const line = firstLine(res.stdout);
    if (!res.spawnError && /^\d/.test(line)) this.version = line;
    return this.version;
  }

  private async me(): Promise<MeegleUser | undefined> {
    const hit = this.cache.peek<MeegleUser>("user");
    if (hit) return hit.value;
    const user = normalizeUser(await this.call(meArgs()));
    if (!user) return undefined;
    this.cache.set("user", user, { ttlMs: CACHE_MS * 2 });
    return user;
  }

  /**
   * 拉起 `auth login --device-code`，等它打出授权链接就返回；进程留着直到用户在
   * 浏览器里授权完成（CLI 自己轮询）。同一时刻只有一个登录进程：再点一次就把
   * 同一份链接再给一遍，别起第二个把第一个的 code 作废。
   */
  async startLogin(host: string): Promise<MeegleLogin> {
    if (this.login) return this.login.prompt;
    if (this.loginStarting) return this.loginStarting;
    this.loginStarting = this.spawnLogin(host).finally(() => {
      this.loginStarting = null;
    });
    return this.loginStarting;
  }

  private async spawnLogin(host: string): Promise<MeegleLogin> {
    const env = await this.env();
    return new Promise<MeegleLogin>((resolve, reject) => {
      const proc = spawn(this.bin, loginArgs(host), {
        env,
        windowsHide: true,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let text = "";
      let settled = false;
      const settle = (fn: () => void) => {
        if (settled) return;
        settled = true;
        clearTimeout(promptTimer);
        fn();
      };
      const promptTimer = setTimeout(() => {
        proc.kill();
        settle(() => reject(new MeegleError("cli-error", "等不到授权链接，登录进程已停止")));
      }, LOGIN_PROMPT_TIMEOUT_MS);
      const onData = (d: Buffer) => {
        text += d.toString("utf8");
        const prompt = parseLoginPrompt(text);
        if (!prompt) return;
        settle(() => {
          const entry = { proc, prompt: { host, ...prompt } };
          this.login = entry;
          resolve(entry.prompt);
        });
      };
      proc.stdout?.on("data", onData);
      proc.stderr?.on("data", onData);
      proc.on("error", (err: NodeJS.ErrnoException) =>
        settle(() =>
          reject(
            err.code === "ENOENT"
              ? new MeegleError("not-installed", "meegle CLI 未安装")
              : new MeegleError("cli-error", String(err))
          )
        )
      );
      proc.on("close", (code) => {
        if (this.login?.proc === proc) this.login = null;
        // 登录态变了：用户、空间、待办都可能换人
        this.cache.clear();
        this.log.info(`meegle auth login 退出（${code}）`);
        settle(() =>
          reject(new MeegleError("cli-error", firstLine(text) || `登录进程退出（${code}）`))
        );
      });
      setTimeout(() => {
        if (this.login?.proc === proc) {
          this.log.warn("meegle 登录等待超时，停止登录进程");
          proc.kill();
        }
      }, LOGIN_MAX_MS).unref();
    });
  }

  cancelLogin(): void {
    this.login?.proc.kill();
    this.login = null;
  }

  /** 面板刷新按钮：丢掉查询缓存，下一次请求重新打 CLI */
  clearCache(): void {
    this.cache.clear();
  }

  // ---- 空间 / 类型 ----

  async spaces(keyword?: string, opts?: MeegleQueryOpts): Promise<MeegleSpace[]> {
    const kw = keyword?.trim();
    // 带关键字是精确查找（粘贴链接换 project_key），不进最近访问列表的缓存
    if (kw) {
      const { spaces } = normalizeSpaces(await this.call(spacesArgs(kw)));
      return spaces;
    }
    return this.cache.getOrLoad(
      "spaces",
      async () => {
        const { spaces } = normalizeSpaces(await this.call(spacesArgs()));
        return spaces;
      },
      opts
    );
  }

  async types(spaceKey: string, opts?: MeegleQueryOpts): Promise<MeegleWorkItemType[]> {
    return this.cache.getOrLoad(
      `types:${spaceKey}`,
      async () => normalizeTypes(await this.call(typesArgs(spaceKey))),
      { ...opts, ttlMs: CACHE_MS * 2 }
    );
  }

  private async fields(spaceKey: string, typeKey: string, opts?: MeegleQueryOpts): Promise<FieldMetadata[]> {
    return this.cache.getOrLoad(`fields:${spaceKey}:${typeKey}`, async () => {
      const fields: FieldMetadata[] = [];
      for (let page = 1; ; page++) {
        const result = normalizeFields(await this.call(fieldsArgs(spaceKey, typeKey, page)), page);
        fields.push(...result.items);
        if (!result.hasMore) return fields;
      }
    }, { ...opts, ttlMs: CACHE_MS * 2 });
  }

  private async resolveRows(data: unknown, spaceKey: string, typeKey: string): Promise<MqlRow[]> {
    const rows = normalizeMqlRows(data);
    if (rows.some((row) => row.businessValue !== undefined)) {
      try {
        const options = (await this.fields(spaceKey, typeKey)).find((f) => f.key === "business")?.options;
        for (const row of rows) row.business ??= businessText(row.businessValue, options);
      } catch (err) {
        this.log.warn(`meegle 业务名称解析失败（${spaceKey}/${typeKey}）: ${(err as Error).message}`);
      }
    }
    return rows;
  }

  private async queryHundred(spaceKey: string, typeKey: string, mql: string): Promise<MqlRow[]> {
    const data = await this.call(queryArgs(spaceKey, mql));
    const rows = await this.resolveRows(data, spaceKey, typeKey);
    const next = rows.length === CLI_PAGE_SIZE ? mqlNextArgs(spaceKey, data) : undefined;
    if (next) rows.push(...await this.resolveRows(await this.call(next), spaceKey, typeKey));
    return rows.slice(0, SEARCH_CAP);
  }

  /** simple_name → 空间：先翻最近访问过的缓存，没有再按 simple_name 查一次（可能有同名无权限的，只认精确匹配） */
  private async spaceBySimpleName(simpleName: string): Promise<MeegleSpace | undefined> {
    const cached = (await this.spaces().catch(() => [] as MeegleSpace[])).find(
      (s) => s.simpleName === simpleName
    );
    if (cached) return cached;
    return (await this.spaces(simpleName)).find((s) => s.simpleName === simpleName);
  }

  /**
   * 粘贴的飞书项目链接 → 面板能开的目标。`url decode` 是 CLI 本地解析（路由表在它那边，
   * 别自己拆路径）；再把 simple_name 换成权威的 project_key。
   */
  async resolveUrl(url: string): Promise<MeegleUrlTarget> {
    const parsed = parseUrlTarget(await this.call(urlDecodeArgs(url)));
    if (!parsed.ok) throw new MeegleError("bad-input", parsed.message);
    const t = parsed.target;
    const host = await this.hostOrProbe();
    if (host && t.host && t.host !== host) {
      throw new MeegleError("bad-input", `链接的站点（${t.host}）与当前登录的站点（${host}）不一致`);
    }
    const space = await this.spaceBySimpleName(t.simpleName);
    if (!space) throw new MeegleError("bad-input", `找不到空间 ${t.simpleName}，可能没有权限`);
    const base = { spaceKey: space.key, spaceName: space.name };
    if (t.kind === "workitem") {
      return {
        kind: "workitem",
        ...base,
        typeKey: t.typeKey,
        id: t.id,
        url: workItemUrl(host, space.simpleName, t.typeKey, t.id) ?? url,
      };
    }
    if (t.kind === "multiProjectView") return { kind: "multiProjectView", ...base, viewId: t.viewId, url };
    return { kind: "view", ...base, viewId: t.viewId, typeKey: t.typeKey, url };
  }

  /** 拼详情页 URL 要空间的 simple_name；只认最近访问过的空间，查不到就没有链接 */
  private async simpleNameOf(spaceKey: string): Promise<string | undefined> {
    try {
      return (await this.spaces()).find((s) => s.key === spaceKey)?.simpleName || undefined;
    } catch {
      return undefined;
    }
  }

  private async hostOrProbe(): Promise<string | null> {
    if (this.host) return this.host;
    await this.probeStatus();
    return this.host;
  }

  private async enabledTypes(
    spaceKey: string,
    typeKey?: string,
    opts?: MeegleQueryOpts
  ): Promise<MeegleWorkItemType[]> {
    const types = (await this.types(spaceKey, opts)).filter((t) => !t.disabled);
    if (!typeKey) return types;
    const one = types.find((t) => t.key === typeKey);
    if (!one) throw new MeegleError("cli-error", "工作项类型不存在");
    return [one];
  }

  // ---- 搜索 / 浏览 ----

  /** 视图与工作项两路并行；一路里某个类型失败只记一条错误，其余照常出结果 */
  async search(
    spaceKey: string,
    keyword: string,
    typeKey?: string,
    opts?: MeegleQueryOpts
  ): Promise<MeegleSearchResult> {
    return this.cache.getOrLoad(
      `search:${spaceKey}:${keyword}:${typeKey ?? ""}`,
      () => this.searchUncached(spaceKey, keyword, typeKey, opts),
      opts
    );
  }

  private async searchUncached(
    spaceKey: string,
    keyword: string,
    typeKey: string | undefined,
    opts?: MeegleQueryOpts
  ): Promise<MeegleSearchResult> {
    const types = await this.enabledTypes(spaceKey, typeKey, opts);
    const errors: string[] = [];
    const [views, items] = await Promise.all([
      this.searchViews(spaceKey, types, keyword, errors),
      this.searchItems(spaceKey, types, keyword, errors),
    ]);
    return { views, items, errors };
  }

  private async searchViews(
    spaceKey: string,
    types: MeegleWorkItemType[],
    keyword: string,
    errors: string[]
  ): Promise<MeegleView[]> {
    const perType = await mapLimit(types, FANOUT, async (type) => {
      try {
        return normalizeViews(await this.call(viewSearchArgs(spaceKey, type.key, keyword)), type);
      } catch (err) {
        errors.push(`${type.name}：${(err as Error).message}`);
        return [];
      }
    });
    return perType.flat();
  }

  private async searchItems(
    spaceKey: string,
    types: MeegleWorkItemType[],
    keyword: string,
    errors: string[]
  ): Promise<MeegleWorkItem[]> {
    const [host, simpleName] = await Promise.all([this.hostOrProbe(), this.simpleNameOf(spaceKey)]);
    const perType = await mapLimit(types, FANOUT, async (type) => {
      try {
        const rows = await this.queryHundred(spaceKey, type.key, mqlSearch(spaceKey, type.key, keyword));
        return rows.map((r) => this.rowToItem(r, spaceKey, type, host, simpleName));
      } catch (err) {
        errors.push(`${type.name}：${(err as Error).message}`);
        return [];
      }
    });
    // id 单调递增，跨类型合并后仍按"最新在前"
    return perType
      .flat()
      .sort((a, b) => (a.id.length === b.id.length ? (a.id < b.id ? 1 : -1) : b.id.length - a.id.length))
      .slice(0, SEARCH_CAP);
  }

  async recent(spaceKey: string, typeKey: string, opts?: MeegleQueryOpts): Promise<MeegleWorkItem[]> {
    return this.cache.getOrLoad(
      `recent:${spaceKey}:${typeKey}`,
      async () => {
        const [type] = await this.enabledTypes(spaceKey, typeKey, opts);
        const [host, simpleName] = await Promise.all([this.hostOrProbe(), this.simpleNameOf(spaceKey)]);
        const rows = await this.queryHundred(spaceKey, type.key, mqlRecent(spaceKey, type.key));
        return rows.map((r) => this.rowToItem(r, spaceKey, type, host, simpleName));
      },
      opts
    );
  }

  private rowToItem(
    row: MqlRow,
    spaceKey: string,
    type: MeegleWorkItemType,
    host: string | null,
    simpleName: string | undefined
  ): MeegleWorkItem {
    return {
      id: row.id,
      name: row.name,
      spaceKey,
      typeKey: type.key,
      typeName: type.name,
      status: row.status,
      business: row.business,
      updatedAt: row.updatedAt,
      url: workItemUrl(host, simpleName, type.key, row.id),
    };
  }

  async viewItems(
    spaceKey: string,
    viewId: string,
    page: number,
    opts?: MeegleQueryOpts
  ): Promise<MeeglePage<MeegleWorkItem>> {
    return this.cache.getOrLoad(
      `view:${spaceKey}:${viewId}:${page}`,
      async () => {
        const host = await this.hostOrProbe();
        const result = await logicalPage(page, async (p) =>
          normalizeViewItems(await this.call(viewItemsArgs(spaceKey, viewId, p)), host, p));
        await this.enrich(result.items);
        return result;
      },
      opts
    );
  }

  /** 全景视图：CLI 只给名字 / 空间 / id / 类型，状态与外链按待办同一套补 */
  async multiViewItems(
    spaceKey: string,
    viewId: string,
    page: number,
    opts?: MeegleQueryOpts
  ): Promise<MeeglePage<MeegleWorkItem>> {
    return this.cache.getOrLoad(
      `mview:${spaceKey}:${viewId}:${page}`,
      async () => {
        const result = await logicalPage(page, async (p) =>
          normalizeMultiViewItems(await this.call(multiViewItemsArgs(spaceKey, viewId, p)), p));
        await this.enrich(result.items);
        return result;
      },
      opts
    );
  }

  async workItem(spaceKey: string, id: string, opts?: MeegleQueryOpts): Promise<MeegleWorkItemDetail> {
    return this.cache.getOrLoad(
      `item:${spaceKey}:${id}`,
      async () => {
        const host = await this.hostOrProbe();
        const detail = normalizeDetail(await this.call(workItemArgs(spaceKey, id)), host);
        if (!detail) throw new MeegleError("cli-error", "工作项不存在或没有权限");
        try {
          const fields = await this.fields(spaceKey, detail.typeKey, opts);
          const keys = fields.filter((f) => f.key === "business" || isContextField(f)).map((f) => f.key);
          if (keys.length) {
            const extra = await this.call(workItemArgs(spaceKey, id, keys));
            Object.assign(detail, detailContext(extra, fields));
          }
        } catch (err) {
          // 登录 / CLI 可用性错误仍须回 409，让面板切回登录提示。
          if (err instanceof MeegleError &&
              (err.reason === "not-authenticated" || err.reason === "not-installed")) throw err;
          // 可选字段配置 / 读取权限不应挡住基础详情；不拿 ID 假装业务名称。
          this.log.warn(`meegle 详情上下文补充失败: ${(err as Error).message}`);
          detail.contextFieldsUnavailable = true;
        }
        return detail;
      },
      opts
    );
  }

  /** 我的待办：mywork 给的行没有名字，见 enrich() */
  async todo(action: MeegleTodoAction, page: number, opts?: MeegleQueryOpts): Promise<MeeglePage<MeegleTodoItem>> {
    return this.cache.getOrLoad(
      `todo:${action}:${page}`,
      async () => {
        const result = await logicalPage(page, async (p) =>
          normalizeTodo(await this.call(todoArgs(action, p)), p));
        await this.enrich(result.items);
        return result;
      },
      opts
    );
  }

  /**
   * 给列表行补齐：名字（空的才补）、状态、业务、更新时间、类型名、外链。mywork 与全景视图
   * 给的行都只有 id 一类的骨架，按 空间 × 类型 分组用 MQL 一次补 50 个；补不到（没权限、
   * 类型停用）就留空，前端显示 #id，别让整页失败。
   */
  private async enrich(items: MeegleWorkItem[]): Promise<void> {
    if (items.length === 0) return;
    const host = await this.hostOrProbe();
    const groups = groupForLookup(items);
    const rows = new Map<string, MqlRow>();
    const typeNames = new Map<string, string>();
    const simpleNames = new Map<string, string | undefined>();
    await mapLimit(
      groups.flatMap((g) => chunk(g.ids, CLI_PAGE_SIZE).map((ids) => ({ ...g, ids }))),
      FANOUT,
      async (g) => {
        try {
          const data = await this.call(queryArgs(g.spaceKey, mqlByIds(g.spaceKey, g.typeKey, g.ids)));
          for (const row of await this.resolveRows(data, g.spaceKey, g.typeKey)) {
            rows.set(`${g.spaceKey} ${g.typeKey} ${row.id}`, row);
          }
        } catch (err) {
          this.log.warn(`meegle 补待办名称失败（${g.spaceKey}/${g.typeKey}）: ${(err as Error).message}`);
        }
      }
    );
    const spaceKeys = [...new Set(groups.map((g) => g.spaceKey))];
    await Promise.all(
      spaceKeys.map(async (key) => {
        simpleNames.set(key, await this.simpleNameOf(key));
        try {
          for (const t of await this.types(key)) typeNames.set(`${key} ${t.key}`, t.name);
        } catch {
          // 类型名只是装饰
        }
      })
    );
    for (const it of items) {
      const row = rows.get(`${it.spaceKey} ${it.typeKey} ${it.id}`);
      if (row) {
        if (!it.name && row.name) it.name = row.name;
        it.status = row.status ?? it.status;
        it.business = row.business ?? it.business;
        it.updatedAt = row.updatedAt ?? it.updatedAt;
      }
      it.typeName ??= typeNames.get(`${it.spaceKey} ${it.typeKey}`);
      it.url ??= workItemUrl(host, simpleNames.get(it.spaceKey), it.typeKey, it.id);
    }
  }
}

/** 有上限的并发 map，结果按输入顺序 */
async function mapLimit<T, R>(items: T[], limit: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return out;
}
