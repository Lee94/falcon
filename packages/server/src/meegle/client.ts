/**
 * meegle CLI 的执行层：起进程、超时、缓存、扇出，把 command.ts 归一化出来的东西
 * 组装成面板要的形状。所有 I/O 都在这一个文件里。
 *
 * 只跑在 falcon 后端所在的机器上（见 command.ts 顶部说明），环境用本地 PTY 同一套
 * 登录环境（sessions/loginEnv）：后端由 launchd / 服务守护拉起时 PATH 里往往没有
 * /opt/homebrew/bin 或 npm 全局目录，不这么做 `meegle` 就"未安装"。
 */

import { spawn, type ChildProcess } from "node:child_process";
import type {
  MeegleLogin,
  MeegleSearchResult,
  MeegleUrlTarget,
  MeegleSpace,
  MeegleStatus,
  MeegleTodoAction,
  MeegleTodoItem,
  MeegleUnavailableReason,
  MeegleUser,
  MeegleView,
  MeegleWorkItem,
  MeegleWorkItemDetail,
  MeegleWorkItemType,
  MeeglePage,
} from "@falcon/shared";
import { resolveLocalBaseEnv } from "../sessions/loginEnv.js";
import { resolveMeegleBin } from "./bin.js";
import {
  CLI_ENV,
  CLI_PAGE_SIZE,
  chunk,
  firstLine,
  groupForLookup,
  loginArgs,
  meArgs,
  mqlByIds,
  mqlRecent,
  mqlSearch,
  multiViewItemsArgs,
  normalizeDetail,
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
} from "./command.js";

/** 一次 CLI 调用的上限。每条命令都是一次到飞书的网络往返，实测 0.5–2s，30s 已是病态 */
const CLI_TIMEOUT_MS = 30_000;
/** 登录进程从启动到打出授权链接：要先去服务端申请 device code */
const LOGIN_PROMPT_TIMEOUT_MS = 20_000;
/** 用户迟迟不授权就把登录进程收掉，别让它一直挂着 */
const LOGIN_MAX_MS = 10 * 60_000;
const CACHE_MS = 5 * 60_000;
/**
 * 同时起几个 CLI 进程：视图 / 工作项搜索要按类型扇出，十来个类型串行太慢，全开又太重。
 * 实测 `view search` 按租户限 5 qps（超了报 `rate limit, … qps: 5`），视图与工作项两路
 * 并行各 3 个、外加下面的限流重试，十几个类型的空间搜一次刚好不撞。
 */
const FANOUT = 3;
/** 撞限流后等多久再试；重试次数见 call() */
const RATE_LIMIT_BACKOFF_MS = 700;
const RATE_LIMIT_RETRIES = 2;
/** 搜索结果上限：面板就那么窄，翻不到几十条以外 */
const SEARCH_CAP = 60;

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

interface Cached<T> {
  at: number;
  value: T;
}

interface Logger {
  info(msg: string): void;
  warn(msg: string): void;
}

export class MeegleClient {
  private readonly bin = resolveMeegleBin();
  private version: string | null = null;
  private host: string | null = null;
  private userCache: Cached<MeegleUser> | null = null;
  private spacesCache: Cached<MeegleSpace[]> | null = null;
  private typesCache = new Map<string, Cached<MeegleWorkItemType[]>>();
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

  async status(): Promise<MeegleStatus> {
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
    if (this.userCache && Date.now() - this.userCache.at < CACHE_MS * 2) return this.userCache.value;
    const user = normalizeUser(await this.call(meArgs()));
    if (!user) return undefined;
    this.userCache = { at: Date.now(), value: user };
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
        // 登录态变了：用户、空间列表都可能换人
        this.userCache = null;
        this.spacesCache = null;
        this.typesCache.clear();
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

  // ---- 空间 / 类型 ----

  async spaces(keyword?: string): Promise<MeegleSpace[]> {
    const kw = keyword?.trim();
    if (!kw && this.spacesCache && Date.now() - this.spacesCache.at < CACHE_MS) {
      return this.spacesCache.value;
    }
    const { spaces } = normalizeSpaces(await this.call(spacesArgs(kw)));
    if (!kw) this.spacesCache = { at: Date.now(), value: spaces };
    return spaces;
  }

  async types(spaceKey: string): Promise<MeegleWorkItemType[]> {
    const hit = this.typesCache.get(spaceKey);
    if (hit && Date.now() - hit.at < CACHE_MS * 2) return hit.value;
    const types = normalizeTypes(await this.call(typesArgs(spaceKey)));
    this.typesCache.set(spaceKey, { at: Date.now(), value: types });
    return types;
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

  private async enabledTypes(spaceKey: string, typeKey?: string): Promise<MeegleWorkItemType[]> {
    const types = (await this.types(spaceKey)).filter((t) => !t.disabled);
    if (!typeKey) return types;
    const one = types.find((t) => t.key === typeKey);
    if (!one) throw new MeegleError("cli-error", "工作项类型不存在");
    return [one];
  }

  // ---- 搜索 / 浏览 ----

  /** 视图与工作项两路并行；一路里某个类型失败只记一条错误，其余照常出结果 */
  async search(spaceKey: string, keyword: string, typeKey?: string): Promise<MeegleSearchResult> {
    const types = await this.enabledTypes(spaceKey, typeKey);
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
        const rows = normalizeMqlRows(await this.call(queryArgs(spaceKey, mqlSearch(spaceKey, type.key, keyword))));
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

  async recent(spaceKey: string, typeKey: string): Promise<MeegleWorkItem[]> {
    const [type] = await this.enabledTypes(spaceKey, typeKey);
    const [host, simpleName] = await Promise.all([this.hostOrProbe(), this.simpleNameOf(spaceKey)]);
    const rows = normalizeMqlRows(await this.call(queryArgs(spaceKey, mqlRecent(spaceKey, type.key))));
    return rows.map((r) => this.rowToItem(r, spaceKey, type, host, simpleName));
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
      updatedAt: row.updatedAt,
      url: workItemUrl(host, simpleName, type.key, row.id),
    };
  }

  async viewItems(spaceKey: string, viewId: string, page: number): Promise<MeeglePage<MeegleWorkItem>> {
    const host = await this.hostOrProbe();
    return normalizeViewItems(await this.call(viewItemsArgs(spaceKey, viewId, page)), host, page);
  }

  /** 全景视图：CLI 只给名字 / 空间 / id / 类型，状态与外链按待办同一套补 */
  async multiViewItems(
    spaceKey: string,
    viewId: string,
    page: number
  ): Promise<MeeglePage<MeegleWorkItem>> {
    const result = normalizeMultiViewItems(await this.call(multiViewItemsArgs(spaceKey, viewId, page)), page);
    await this.enrich(result.items);
    return result;
  }

  async workItem(spaceKey: string, id: string): Promise<MeegleWorkItemDetail> {
    const host = await this.hostOrProbe();
    const detail = normalizeDetail(await this.call(workItemArgs(spaceKey, id)), host);
    if (!detail) throw new MeegleError("cli-error", "工作项不存在或没有权限");
    return detail;
  }

  /** 我的待办：mywork 给的行没有名字，见 enrich() */
  async todo(action: MeegleTodoAction, page: number): Promise<MeeglePage<MeegleTodoItem>> {
    const result = normalizeTodo(await this.call(todoArgs(action, page)), page);
    await this.enrich(result.items);
    return result;
  }

  /**
   * 给"半成品"的行补齐：名字（空的才补）、状态、更新时间、类型名、空间名、外链。mywork 与
   * 全景视图给的行都只有 id 一类的骨架，按 空间 × 类型 分组用 MQL 一次补 50 个；补不到
   * （没权限、类型停用）就留空，前端显示 #id，别让整页失败。
   */
  private async enrich(items: MeegleWorkItem[]): Promise<void> {
    if (items.length === 0) return;
    const host = await this.hostOrProbe();
    const groups = groupForLookup(items);
    const rows = new Map<string, MqlRow>();
    const typeNames = new Map<string, string>();
    const spaceNames = new Map<string, string>();
    const simpleNames = new Map<string, string | undefined>();
    await mapLimit(
      groups.flatMap((g) => chunk(g.ids, CLI_PAGE_SIZE).map((ids) => ({ ...g, ids }))),
      FANOUT,
      async (g) => {
        try {
          const data = await this.call(queryArgs(g.spaceKey, mqlByIds(g.spaceKey, g.typeKey, g.ids)));
          for (const row of normalizeMqlRows(data)) rows.set(`${g.spaceKey} ${row.id}`, row);
        } catch (err) {
          this.log.warn(`meegle 补待办名称失败（${g.spaceKey}/${g.typeKey}）: ${(err as Error).message}`);
        }
      }
    );
    const spaceKeys = [...new Set(groups.map((g) => g.spaceKey))];
    // 空间名：全景视图的行只给 project_key，"这条属于哪个空间"全靠这里补（待办自带 project_name，
    // 不覆盖）。spaces() 只有最近访问过的空间，跨到没访问过的空间就补不到，留空即可。
    for (const s of await this.spaces().catch(() => [])) spaceNames.set(s.key, s.name);
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
      const row = rows.get(`${it.spaceKey} ${it.id}`);
      if (row) {
        if (row.name) it.name = row.name;
        it.status = row.status;
        it.updatedAt = row.updatedAt;
      }
      it.typeName = typeNames.get(`${it.spaceKey} ${it.typeKey}`);
      it.spaceName ??= spaceNames.get(it.spaceKey);
      it.url = workItemUrl(host, simpleNames.get(it.spaceKey), it.typeKey, it.id);
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
