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
import { startArchiveSweeper } from "./archive.js";
import { registerRoutes } from "./routes.js";
import { registerWs } from "./ws.js";
import { ZELLIJ_VERSION } from "./zellij/version.js";

const VERSION = "0.1.0";

async function main() {
  // `falcon service <install|…>`：注册/管理系统服务（launchd / systemd 守护），不启动服务器
  if (process.argv[2] === "service") {
    const { runServiceCli } = await import("./service.js");
    runServiceCli(process.argv.slice(3));
    return;
  }

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
  // 启用中的转发是服务，后端重启后应自己把隧道拉起来，不等用户再开一次面板
  void manager.forwards.restoreEnabled();
  // 持久会话在 DB 里被标成 unverified：自动接回，不要等用户挨个点
  void manager.resumeUnverified();

  const app = Fastify({ logger: { level: "info" } });
  await app.register(fastifyCookie);
  await app.register(fastifyWebsocket, { options: { maxPayload: 1024 * 1024 } });

  registerRoutes(app, { db, auth, manager, secrets, version: VERSION, dataDir: config.dataDir });
  registerWs(app, { auth, manager, db });

  // 存档到期的附属项目由后台清扫自动删除，不等用户下次打开界面
  const stopArchiveSweeper = startArchiveSweeper(db, manager, app.log);

  // 托管 web 构建产物（存在时）；单文件发布时由 SEA bootstrap 解压后经环境变量指入
  const here = path.dirname(fileURLToPath(import.meta.url));
  const webDist =
    process.env.FALCON_WEB_DIST ??
    process.env.MOJITO_WEB_DIST ??
    path.resolve(here, "../../web/dist");
  if (fs.existsSync(path.join(webDist, "index.html"))) {
    await app.register(fastifyStatic, {
      root: webDist,
      setHeaders(res, filePath) {
        // mime-db 不一定带 .webmanifest；Chrome 认这个类型才把清单当 PWA 清单
        if (filePath.endsWith(".webmanifest")) {
          res.setHeader("Content-Type", "application/manifest+json; charset=utf-8");
        }
      },
    });
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
  } else {
    // 静默跳过会让"UI 404"极难排查（多半是 FALCON_WEB_DIST 指了不存在的目录，
    // 比如从 falcon 终端里继承来的陈旧值），启动时明说
    app.log.warn(`web 静态资源目录不存在，未托管 UI: ${webDist}`);
  }

  await app.listen({ host: config.host, port: config.port });
  app.log.info(
    `falcon 已启动: http://${loopback ? "localhost" : config.host}:${config.port}` +
      `（数据目录 ${config.dataDir}，Zellij ${ZELLIJ_VERSION}）`
  );

  const shutdown = async () => {
    stopArchiveSweeper();
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
