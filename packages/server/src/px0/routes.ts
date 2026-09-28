/**
 * `/px0/<项目 id>/*`：把请求反代到该项目的 px0（ADR 0017）。
 *
 * 鉴权是登录 cookie，由 routes.ts 的 onRequest 钩子统一挡（`/px0/` 与 `/api/` 同一口径）。
 *
 * 注册在一个独立的插件作用域里，换掉全部 body 解析器：请求体原样作为流交给上游，
 * 不经 fastify 的 JSON 解析、也没有 1MB 的 bodyLimit。响应走 hijack 直接写原始
 * ServerResponse——SSE（`/api/stream`）要边收边发，不能让 fastify 攒着。
 */

import http, { type IncomingMessage, type ServerResponse } from "node:http";
import type { Readable } from "node:stream";
import type { FastifyInstance } from "fastify";
import { px0BasePath } from "@falcon/shared";
import type { Db } from "../db.js";
import type { Px0Instance, Px0Manager } from "./manager.js";
import { downstreamResponseHeaders, px0StatusPage, upstreamRequestHeaders } from "./proxy.js";

export function registerPx0Routes(app: FastifyInstance, db: Db, px0: Px0Manager) {
  void app.register(async (scope) => {
    scope.removeAllContentTypeParsers();
    scope.addContentTypeParser("*", (_req, payload, done) => done(null, payload));

    // px0 自己也会把不带尾斜杠的前缀重定向过去，但没在跑时轮不到它
    scope.all("/px0/:projectId", async (req, reply) => {
      const { projectId } = req.params as { projectId: string };
      return reply.redirect(px0BasePath(projectId), 301);
    });

    scope.all("/px0/:projectId/*", async (req, reply) => {
      const { projectId, "*": rest } = req.params as { projectId: string; "*": string };
      const row = db.getProject(projectId);
      if (!row) return reply.code(404).send({ error: "项目不存在" });
      // 存档的附属项目到期连目录一起删，不能再往里开 px0（存档时已经停掉了）
      if (row.worktree_archived_at) {
        return reply.code(409).send({ error: "项目已存档，请先恢复再用 px0 打开" });
      }

      const inst = px0.acquire(projectId);
      if (!inst) {
        // 只有入口页会拉起 px0：静态资源、API、SSE 在它没跑时一律 503，免得页面上
        // 残留的轮询把一个刚被空闲回收的实例又叫起来
        const isEntry = rest === "" && (req.method === "GET" || req.method === "HEAD");
        if (!isEntry) return reply.code(503).send({ error: "px0 未在运行" });
        const query = req.query as { retry?: string };
        const state = px0.open(row, query.retry === "1");
        if (state) {
          return reply
            .code(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Cache-Control", "no-store")
            .send(px0StatusPage(state, px0BasePath(projectId), row.name));
        }
        // open 说已经在跑（并发的另一个请求刚把它拉起来）：再拿一次
        const late = px0.acquire(projectId);
        if (!late) return reply.code(503).send({ error: "px0 未在运行" });
        reply.hijack();
        forward(req.raw, reply.raw, req.body as Readable | undefined, late, px0);
        return;
      }
      reply.hijack();
      forward(req.raw, reply.raw, req.body as Readable | undefined, inst, px0);
    });
  });
}

/**
 * 转发一个请求。每个请求一条新连接（不经 agent，只给 createConnection）：远端的连接
 * 是一条 forwardOut 通道，ssh2 通道不适合进 keep-alive 池；本机回环建连的代价可以忽略。
 *
 * 不能写 `agent: false`：Node 见到 false 会现造一个 Agent，而 Agent 用它自己的
 * createConnection（直连 host:80），我们给的那个被无视。
 */
function forward(
  req: IncomingMessage,
  res: ServerResponse,
  body: Readable | undefined,
  inst: Px0Instance,
  px0: Px0Manager
) {
  let released = false;
  let upstreamDone = false;
  const release = () => {
    if (released) return;
    released = true;
    px0.release(inst);
  };

  const upstream = http.request(
    {
      method: req.method,
      path: req.url,
      headers: upstreamRequestHeaders(req.headers, "127.0.0.1"),
      createConnection: ((_opts: unknown, cb: (err: Error | null, socket?: unknown) => void) => {
        inst.connect().then(
          (socket) => cb(null, socket),
          (err: Error) => cb(err)
        );
        return undefined;
      }) as unknown as http.RequestOptions["createConnection"],
    },
    (up) => {
      res.writeHead(up.statusCode ?? 502, downstreamResponseHeaders(up.headers));
      // SSE 的头要立刻出去，浏览器才会认为流已建立
      res.flushHeaders();
      up.on("end", () => {
        upstreamDone = true;
      });
      up.pipe(res);
    }
  );

  upstream.on("error", (err) => {
    upstreamDone = true;
    if (!res.headersSent) {
      res.writeHead(502, { "Content-Type": "application/json; charset=utf-8" });
      // AggregateError（多地址建连全失败）的 message 是空串，退到 code
      const why = err.message || (err as NodeJS.ErrnoException).code || String(err);
      res.end(JSON.stringify({ error: `px0 连不上：${why}` }));
    } else {
      res.destroy();
    }
    release();
  });

  // 客户端断开（关标签页、EventSource 重连）或响应写完都会走到这里
  res.on("close", () => {
    if (!upstreamDone) upstream.destroy();
    release();
  });

  if (body && typeof body.pipe === "function") body.pipe(upstream);
  else upstream.end();
}
