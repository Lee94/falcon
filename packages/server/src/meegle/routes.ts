import crypto from "node:crypto";
import type { FastifyInstance, FastifyReply } from "fastify";
import type {
  MeegleLogin,
  MeeglePage,
  MeeglePin,
  MeeglePinInput,
  MeeglePinKind,
  MeegleSearchResult,
  MeegleSpace,
  MeegleStatus,
  MeegleTodoItem,
  MeegleUrlTarget,
  MeegleWorkItem,
  MeegleWorkItemDetail,
  MeegleWorkItemType,
} from "@falcon/shared";
import { Db, type MeeglePinRow } from "../db.js";
import { MeegleClient, MeegleError } from "./client.js";
import { isTodoAction, isValidHost, isValidId, isValidKey, isValidUrl } from "./command.js";

/**
 * `/api/meegle/*`：右侧「飞书项目」面板的后端。都是只读查询加一条登录。
 *
 * 错误码约定：CLI 没装 / 没登录是 409（带 reason，前端据此切到安装 / 登录提示，
 * 用户动手就能过）；CLI 跑了但飞书那边报错是 502；参数不合法 / 链接不支持是 400。
 * 鉴权由 routes.ts 的全局 onRequest 钩子管，这里不再查 cookie。
 *
 * 固定列表存 SQLite（db.meegle_pins）：只读的 CLI 查询走 meegle，固定是 falcon 自己的数据。
 * 查询默认走 client 的 TTL 缓存；`?fresh=1` 跳过（面板刷新按钮）。
 */
export function registerMeegleRoutes(app: FastifyInstance, meegle: MeegleClient, db: Db) {
  const fail = (reply: FastifyReply, err: unknown) => {
    if (err instanceof MeegleError) {
      const status = err.reason === "cli-error" ? 502 : err.reason === "bad-input" ? 400 : 409;
      return reply.code(status).send({ error: err.message, reason: err.reason, code: err.code });
    }
    throw err;
  };

  app.get("/api/meegle/status", async (req): Promise<MeegleStatus> =>
    meegle.status(queryOpts(req.query as FreshQuery))
  );

  app.post("/api/meegle/login", async (req, reply): Promise<MeegleLogin | void> => {
    const { host } = (req.body ?? {}) as { host?: unknown };
    if (!isValidHost(host)) return reply.code(400).send({ error: "站点域名不合法" });
    try {
      return await meegle.startLogin(host);
    } catch (err) {
      return fail(reply, err);
    }
  });

  app.post("/api/meegle/login/cancel", async () => {
    meegle.cancelLogin();
    return { ok: true };
  });

  app.post("/api/meegle/cache/clear", async () => {
    meegle.clearCache();
    return { ok: true };
  });

  app.get("/api/meegle/spaces", async (req, reply): Promise<MeegleSpace[] | void> => {
    const { q, fresh } = req.query as { q?: string; fresh?: string };
    try {
      return await meegle.spaces(q, queryOpts({ fresh }));
    } catch (err) {
      return fail(reply, err);
    }
  });

  app.get(
    "/api/meegle/spaces/:key/types",
    async (req, reply): Promise<MeegleWorkItemType[] | void> => {
      const { key } = req.params as { key: string };
      if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
      try {
        return await meegle.types(key, queryOpts(req.query as FreshQuery));
      } catch (err) {
        return fail(reply, err);
      }
    }
  );

  app.get(
    "/api/meegle/spaces/:key/search",
    async (req, reply): Promise<MeegleSearchResult | void> => {
      const { key } = req.params as { key: string };
      const { q, type, fresh } = req.query as { q?: string; type?: string; fresh?: string };
      const keyword = q?.trim() ?? "";
      if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
      if (!keyword) return reply.code(400).send({ error: "缺少关键字" });
      if (type && !isValidKey(type)) return reply.code(400).send({ error: "类型 key 不合法" });
      try {
        return await meegle.search(key, keyword, type || undefined, queryOpts({ fresh }));
      } catch (err) {
        return fail(reply, err);
      }
    }
  );

  app.get("/api/meegle/spaces/:key/recent", async (req, reply): Promise<MeegleWorkItem[] | void> => {
    const { key } = req.params as { key: string };
    const { type, fresh } = req.query as { type?: string; fresh?: string };
    if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
    if (!isValidKey(type)) return reply.code(400).send({ error: "类型 key 不合法" });
    try {
      return await meegle.recent(key, type, queryOpts({ fresh }));
    } catch (err) {
      return fail(reply, err);
    }
  });

  app.get(
    "/api/meegle/spaces/:key/views/:viewId/items",
    async (req, reply): Promise<MeeglePage<MeegleWorkItem> | void> => {
      const { key, viewId } = req.params as { key: string; viewId: string };
      const q = req.query as { page?: string; fresh?: string };
      const page = pageOf(q.page);
      if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
      if (!isValidKey(viewId)) return reply.code(400).send({ error: "视图 id 不合法" });
      try {
        return await meegle.viewItems(key, viewId, page, queryOpts(q));
      } catch (err) {
        return fail(reply, err);
      }
    }
  );

  app.get(
    "/api/meegle/spaces/:key/items/:id",
    async (req, reply): Promise<MeegleWorkItemDetail | void> => {
      const { key, id } = req.params as { key: string; id: string };
      if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
      if (!isValidId(id)) return reply.code(400).send({ error: "工作项 id 不合法" });
      try {
        return await meegle.workItem(key, id, queryOpts(req.query as FreshQuery));
      } catch (err) {
        return fail(reply, err);
      }
    }
  );

  app.get(
    "/api/meegle/spaces/:key/multi-views/:viewId/items",
    async (req, reply): Promise<MeeglePage<MeegleWorkItem> | void> => {
      const { key, viewId } = req.params as { key: string; viewId: string };
      const q = req.query as { page?: string; fresh?: string };
      const page = pageOf(q.page);
      if (!isValidKey(key)) return reply.code(400).send({ error: "空间 key 不合法" });
      if (!isValidKey(viewId)) return reply.code(400).send({ error: "视图 id 不合法" });
      try {
        return await meegle.multiViewItems(key, viewId, page, queryOpts(q));
      } catch (err) {
        return fail(reply, err);
      }
    }
  );

  app.post("/api/meegle/resolve-url", async (req, reply): Promise<MeegleUrlTarget | void> => {
    const { url } = (req.body ?? {}) as { url?: unknown };
    if (!isValidUrl(url)) return reply.code(400).send({ error: "不是合法的链接" });
    try {
      return await meegle.resolveUrl(url);
    } catch (err) {
      return fail(reply, err);
    }
  });

  // ---- 固定列表 ----

  app.get("/api/meegle/pins", async (): Promise<MeeglePin[]> =>
    db.listMeeglePins().map((row) => Db.toMeeglePin(row))
  );

  /** 同一个东西再固定一次就回已有的那条，前端不用先查 */
  app.post("/api/meegle/pins", async (req, reply): Promise<MeeglePin | void> => {
    const input = parsePinInput(req.body);
    if (!input) return reply.code(400).send({ error: "固定项不合法" });
    const existing = db.findMeeglePin(input.kind, input.spaceKey, input.targetId);
    if (existing) return Db.toMeeglePin(existing);
    const row: MeeglePinRow = {
      id: crypto.randomUUID(),
      kind: input.kind,
      space_key: input.spaceKey,
      space_name: input.spaceName ?? null,
      target_id: input.targetId,
      type_key: input.typeKey ?? null,
      label: input.label,
      url: input.url ?? null,
      created_at: Date.now(),
    };
    db.insertMeeglePin(row);
    return Db.toMeeglePin(row);
  });

  app.patch("/api/meegle/pins/:id", async (req, reply): Promise<MeeglePin | void> => {
    const { id } = req.params as { id: string };
    const label = pinLabel((req.body as { label?: unknown } | undefined)?.label);
    if (!label) return reply.code(400).send({ error: "名称不能为空" });
    const row = db.getMeeglePin(id);
    if (!row) return reply.code(404).send({ error: "固定项不存在" });
    db.renameMeeglePin(id, label);
    return Db.toMeeglePin({ ...row, label });
  });

  app.delete("/api/meegle/pins/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    if (!db.getMeeglePin(id)) return reply.code(404).send({ error: "固定项不存在" });
    db.deleteMeeglePin(id);
    return { ok: true };
  });

  app.get("/api/meegle/todo", async (req, reply): Promise<MeeglePage<MeegleTodoItem> | void> => {
    const { action, page, fresh } = req.query as { action?: string; page?: string; fresh?: string };
    if (!isTodoAction(action)) return reply.code(400).send({ error: "action 不合法" });
    try {
      return await meegle.todo(action, pageOf(page), queryOpts({ fresh }));
    } catch (err) {
      return fail(reply, err);
    }
  });
}

type FreshQuery = { fresh?: string };

function queryOpts(q: FreshQuery): { fresh?: boolean } {
  return q.fresh === "1" || q.fresh === "true" ? { fresh: true } : {};
}

const PIN_KINDS: readonly MeeglePinKind[] = ["view", "multiProjectView", "workitem"];

function pinLabel(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const s = v.trim();
  return s && s.length <= 200 ? s : null;
}

function optionalText(v: unknown, max: number): string | undefined | null {
  if (v == null || v === "") return undefined;
  return typeof v === "string" && v.length <= max ? v : null;
}

/** 固定项各字段都过白名单：它们之后会原样进 CLI 的 argv */
function parsePinInput(body: unknown): MeeglePinInput | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as Record<string, unknown>;
  const kind = b.kind;
  if (typeof kind !== "string" || !(PIN_KINDS as readonly string[]).includes(kind)) return null;
  if (!isValidKey(b.spaceKey) || !isValidKey(b.targetId)) return null;
  if (kind === "workitem" && !isValidId(b.targetId)) return null;
  if (b.typeKey != null && b.typeKey !== "" && !isValidKey(b.typeKey)) return null;
  const label = pinLabel(b.label);
  if (!label) return null;
  const spaceName = optionalText(b.spaceName, 200);
  if (spaceName === null) return null;
  if (b.url != null && b.url !== "" && !isValidUrl(b.url)) return null;
  return {
    kind: kind as MeeglePinKind,
    spaceKey: b.spaceKey,
    spaceName,
    targetId: b.targetId,
    typeKey: b.typeKey ? (b.typeKey as string) : undefined,
    label,
    url: b.url ? (b.url as string) : undefined,
  };
}

function pageOf(raw: string | undefined): number {
  const n = Number(raw ?? 1);
  return Number.isInteger(n) && n >= 1 && n <= 10_000 ? n : 1;
}
