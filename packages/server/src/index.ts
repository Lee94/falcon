#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import Fastify from "fastify";
import fastifyCookie from "@fastify/cookie";
import fastifyStatic from "@fastify/static";
import fastifyWebsocket from "@fastify/websocket";
import { isLoopback, parseArgs } from "./config.js";
import { Db } from "./db.js";
import { SecretBox } from "./crypto.js";
import { Auth } from "./auth.js";
import { SessionManager } from "./sessions/manager.js";
import { registerRoutes } from "./routes.js";
import { registerWs } from "./ws.js";
import { ZELLIJ_VERSION } from "./zellij/version.js";

const VERSION = "0.1.0";

async function main() {
  const config = parseArgs(process.argv.slice(2));
  const loopback = isLoopback(config.host);

  const db = new Db(config.dataDir);
  const secrets = new SecretBox(config.dataDir);
  const auth = new Auth(db, loopback);

  if (!loopback && !auth.passwordSet()) {
    console.error(
      `拒绝启动：绑定 ${config.host}（非 localhost）但尚未设置访问密码。\n` +
        `请先在 localhost 上启动并通过界面设置密码，再对外绑定。`
    );
    process.exit(1);
  }

  const manager = new SessionManager(db, secrets, config.dataDir);

  const app = Fastify({ logger: { level: "info" } });
  await app.register(fastifyCookie);
  await app.register(fastifyWebsocket, { options: { maxPayload: 1024 * 1024 } });

  registerRoutes(app, { db, auth, manager, secrets, version: VERSION });
  registerWs(app, { auth, manager, db });

  // 托管 web 构建产物（存在时）
  const here = path.dirname(fileURLToPath(import.meta.url));
  const webDist = path.resolve(here, "../../web/dist");
  if (fs.existsSync(path.join(webDist, "index.html"))) {
    await app.register(fastifyStatic, { root: webDist });
    app.setNotFoundHandler((req, reply) => {
      if (
        req.method === "GET" &&
        !req.url.startsWith("/api/") &&
        !req.url.startsWith("/ws/")
      ) {
        return reply.sendFile("index.html");
      }
      return reply.code(404).send({ error: "Not Found" });
    });
  }

  await app.listen({ host: config.host, port: config.port });
  app.log.info(
    `mojito 已启动: http://${loopback ? "localhost" : config.host}:${config.port}` +
      `（数据目录 ${config.dataDir}，Zellij ${ZELLIJ_VERSION}）`
  );

  const shutdown = async () => {
    // 会话在 DB 中保持 active，下次启动由 recoverSessionsOnStartup 归类
    await app.close();
    process.exit(0);
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
