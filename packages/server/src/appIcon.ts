/**
 * 应用图标（ADR 0018）：选择存在 settings 表，自定义图片落在 <dataDir>/app-icon/custom.png。
 *
 * 公开（不要登录）的只有「取图」这几条：登录页的标签页图标、浏览器拉 PWA 清单与图标时
 * 都不带登录态。改选择、上传、删除照常要登录。
 */
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import type { FastifyInstance, FastifyReply } from "fastify";
import {
  appIconLinks,
  isAppIconChoice,
  resolveAppIcon,
  webManifest,
  type AppIconState,
} from "@falcon/shared";
import type { Db } from "./db.js";

const SELECTED_KEY = "app_icon";
const CUSTOM_KEY = "app_icon_custom";

/** 客户端上传的是规整过的 512 PNG，几百 KB 到头；给足余量也挡住塞大文件 */
export const CUSTOM_ICON_MAX_BYTES = 2 * 1024 * 1024;
const MIN_EDGE = 64;
const MAX_EDGE = 2048;

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/** 读 PNG 的 IHDR 拿宽高；不是 PNG（签名不对、首块不是 IHDR、太短）返回 null */
export function pngSize(buf: Buffer): { width: number; height: number } | null {
  if (buf.length < 24) return null;
  if (!buf.subarray(0, 8).equals(PNG_SIGNATURE)) return null;
  if (buf.toString("latin1", 12, 16) !== "IHDR") return null;
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
}

/**
 * 服务端不解码图片（没有图像库，也不想为此装一个），只做形状检查：
 * 必须是正方形 PNG、边长在合理范围。缩放 / 裁切是客户端上传前做的。
 */
export function checkCustomIcon(buf: Buffer): string | null {
  if (buf.length > CUSTOM_ICON_MAX_BYTES) return "图片太大（上限 2MB）";
  const size = pngSize(buf);
  if (!size) return "只收 PNG";
  if (size.width !== size.height) return "图标必须是正方形";
  if (size.width < MIN_EDGE || size.width > MAX_EDGE) {
    return `边长要在 ${MIN_EDGE}–${MAX_EDGE} 之间`;
  }
  return null;
}

export class AppIcons {
  private readonly dir: string;
  private readonly file: string;

  constructor(
    private readonly db: Db,
    dataDir: string
  ) {
    this.dir = path.join(dataDir, "app-icon");
    this.file = path.join(this.dir, "custom.png");
  }

  /** 自定义图的版本；库里记着但文件没了（被手动删掉）也当没有 */
  private customVersion(): string | null {
    const v = this.db.getSetting(CUSTOM_KEY);
    return v && fs.existsSync(this.file) ? v : null;
  }

  state(): AppIconState {
    const custom = this.customVersion();
    return { selected: resolveAppIcon(this.db.getSetting(SELECTED_KEY), custom), custom };
  }

  /** 返回 null = 这个选择不成立（未知 id，或选自定义但还没传图） */
  select(choice: unknown): AppIconState | null {
    if (!isAppIconChoice(choice)) return null;
    if (choice === "custom" && !this.customVersion()) return null;
    this.db.setSetting(SELECTED_KEY, choice);
    return this.state();
  }

  /** 存下自定义图并选中它。先写临时文件再改名：中途失败不留半张图 */
  saveCustom(buf: Buffer): AppIconState {
    const version = crypto.createHash("sha256").update(buf).digest("hex").slice(0, 16);
    fs.mkdirSync(this.dir, { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, buf);
    fs.renameSync(tmp, this.file);
    this.db.setSetting(CUSTOM_KEY, version);
    this.db.setSetting(SELECTED_KEY, "custom");
    return this.state();
  }

  /** 删掉自定义图；正选着它就回默认（state() 的 resolve 自己会回落，这里把库也改干净） */
  removeCustom(): AppIconState {
    fs.rmSync(this.file, { force: true });
    this.db.setSetting(CUSTOM_KEY, "");
    if (this.db.getSetting(SELECTED_KEY) === "custom") this.db.setSetting(SELECTED_KEY, "");
    return this.state();
  }

  customFile(): string | null {
    return this.customVersion() ? this.file : null;
  }
}

export function registerAppIconRoutes(app: FastifyInstance, icons: AppIcons) {
  // 公开路由在 routes.ts 的鉴权钩子里靠这个标记放行
  const publicAsset = { config: { publicAsset: true } };

  app.get("/api/app-icon", async (): Promise<AppIconState> => icons.state());

  app.put("/api/app-icon", async (req, reply) => {
    const { selected } = (req.body ?? {}) as { selected?: unknown };
    const next = icons.select(selected);
    if (!next) return reply.code(400).send({ error: "没有这个图标" });
    return next;
  });

  app.put("/api/app-icon/custom", async (req, reply) => {
    const body = req.body;
    if (!Buffer.isBuffer(body)) return reply.code(400).send({ error: "请求体必须是 image/png" });
    const problem = checkCustomIcon(body);
    if (problem) return reply.code(body.length > CUSTOM_ICON_MAX_BYTES ? 413 : 400).send({ error: problem });
    return icons.saveCustom(body);
  });

  app.delete("/api/app-icon/custom", async (): Promise<AppIconState> => icons.removeCustom());

  // 地址里的 v 是内容哈希：对上了就能永久缓存；对不上（旧页面拿着旧地址）给当前这张、不缓存
  app.get("/api/app-icon/custom.png", publicAsset, async (req, reply) => {
    const file = icons.customFile();
    if (!file) return reply.code(404).send({ error: "没有自定义图标" });
    const { v } = req.query as { v?: string };
    reply.header(
      "Cache-Control",
      v && v === icons.state().custom ? "public, max-age=31536000, immutable" : "no-cache"
    );
    reply.type("image/png");
    return fs.promises.readFile(file);
  });

  // index.html 的 <link rel="icon"> / apple-touch-icon 指向这两条：服务端按当前选择跳过去，
  // 页面不用等 JS 就是对的图标。跳转本身不缓存，换了图标下次加载就换
  const redirect = (pick: (links: ReturnType<typeof appIconLinks>) => string) =>
    async (_req: unknown, reply: FastifyReply) => {
      reply.header("Cache-Control", "no-cache");
      return reply.redirect(pick(appIconLinks(icons.state())));
    };
  app.get("/api/app-icon/favicon", publicAsset, redirect((l) => l.favicon));
  app.get("/api/app-icon/apple-touch-icon", publicAsset, redirect((l) => l.appleTouch));

  // PWA 清单：不在 /api 下（鉴权钩子不管），路径与以前的静态文件相同，已安装的应用不用换地址。
  // mime-db 不一定带 .webmanifest；Chrome 认这个类型才把清单当 PWA 清单
  app.get("/manifest.webmanifest", async (_req, reply) => {
    reply.header("Cache-Control", "no-cache");
    reply.type("application/manifest+json; charset=utf-8");
    return JSON.stringify(webManifest(icons.state()));
  });
}
