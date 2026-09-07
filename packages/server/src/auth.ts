import crypto from "node:crypto";
import type { FastifyReply, FastifyRequest } from "fastify";
import type { Db } from "./db.js";
import { hashPassword, verifyPassword } from "./crypto.js";

const COOKIE_NAME = "falcon_token";
const TOKEN_TTL_MS = 30 * 24 * 3600 * 1000;
/**
 * 原始字节令牌的寿命。HTML 预览里懒加载的图片、滚到底才请求的字体，都拿着
 * 页面打开时那枚令牌，太短会让"开着不动一会儿"的预览悄悄缺图；再长则一个
 * 泄露的 URL 能读项目文件的窗口也跟着长。每次读文件都会换新的，6 小时够用。
 */
const RAW_TOKEN_TTL_MS = 6 * 3600 * 1000;

export class Auth {
  private tokens = new Map<string, number>();
  /**
   * 原始字节路由的作用域令牌：token → { 项目, 到期 }。与登录 token 完全无关——
   * 它会出现在 `<iframe src>` 的 URL 里、被沙箱内的脚本读到，所以只能授予
   * "读这个项目的文件"这一件事，不能反推出登录态。按项目复用（同一项目短时间
   * 内连续读文件不该攒出一堆），logout 时整批作废。
   */
  private rawTokens = new Map<string, { projectId: string; expiry: number }>();
  private rawByProject = new Map<string, string>();
  /**
   * password_hash 的内存缓存（undefined = 还没读过）。每个 HTTP 请求和
   * WS 建连都要过 required()，不该每次都打一遍 SQLite；唯一的写入方是
   * setPassword，同步更新缓存即可。
   */
  private hashCache: string | null | undefined;

  constructor(
    private db: Db,
    private loopback: boolean
  ) {}

  private passwordHash(): string | null {
    if (this.hashCache === undefined) {
      this.hashCache = this.db.getSetting("password_hash") ?? null;
    }
    return this.hashCache;
  }

  passwordSet(): boolean {
    return this.passwordHash() != null;
  }

  /** 需要认证 = 设置过密码，或对外绑定 */
  required(): boolean {
    return this.passwordSet() || !this.loopback;
  }

  login(password: string): string | null {
    const stored = this.passwordHash();
    if (!stored || !verifyPassword(password, stored)) return null;
    const token = crypto.randomBytes(32).toString("hex");
    this.tokens.set(token, Date.now() + TOKEN_TTL_MS);
    return token;
  }

  logout(token: string | undefined) {
    if (token) this.tokens.delete(token);
    this.rawTokens.clear();
    this.rawByProject.clear();
  }

  /**
   * 给一个项目签发（或复用）原始字节令牌。剩余寿命不足一半就换新的：
   * 拿到手的令牌至少还能活 3 小时，前端不必关心到期。
   */
  rawToken(projectId: string): string {
    const now = Date.now();
    const existing = this.rawByProject.get(projectId);
    if (existing) {
      const entry = this.rawTokens.get(existing);
      if (entry && entry.expiry - now > RAW_TOKEN_TTL_MS / 2) return existing;
      this.rawTokens.delete(existing);
    }
    this.pruneRawTokens(now);
    const token = crypto.randomBytes(24).toString("hex");
    this.rawTokens.set(token, { projectId, expiry: now + RAW_TOKEN_TTL_MS });
    this.rawByProject.set(projectId, token);
    return token;
  }

  /** 令牌对得上项目且没过期。不认证的部署下调用方不该走到这里（先看 isAuthenticated） */
  rawTokenValid(token: string | undefined, projectId: string): boolean {
    if (!token) return false;
    const entry = this.rawTokens.get(token);
    if (!entry) return false;
    if (entry.expiry < Date.now()) {
      this.rawTokens.delete(token);
      return false;
    }
    return entry.projectId === projectId;
  }

  private pruneRawTokens(now: number) {
    for (const [token, entry] of this.rawTokens) {
      if (entry.expiry < now) {
        this.rawTokens.delete(token);
        if (this.rawByProject.get(entry.projectId) === token) this.rawByProject.delete(entry.projectId);
      }
    }
  }

  setPassword(next: string, current?: string): boolean {
    const stored = this.passwordHash();
    if (stored && (!current || !verifyPassword(current, stored))) return false;
    const hash = hashPassword(next);
    this.db.setSetting("password_hash", hash);
    this.hashCache = hash;
    return true;
  }

  isAuthenticated(req: FastifyRequest): boolean {
    if (!this.required()) return true;
    const token = (req.cookies as Record<string, string | undefined>)?.[COOKIE_NAME];
    if (!token) return false;
    const expiry = this.tokens.get(token);
    if (!expiry || expiry < Date.now()) {
      if (token) this.tokens.delete(token);
      return false;
    }
    // 滑动过期
    this.tokens.set(token, Date.now() + TOKEN_TTL_MS);
    return true;
  }

  setCookie(reply: FastifyReply, token: string) {
    reply.setCookie(COOKIE_NAME, token, {
      httpOnly: true,
      sameSite: "lax",
      path: "/",
      maxAge: TOKEN_TTL_MS / 1000,
    });
  }

  clearCookie(reply: FastifyReply) {
    reply.clearCookie(COOKIE_NAME, { path: "/" });
  }

  cookieName() {
    return COOKIE_NAME;
  }
}
