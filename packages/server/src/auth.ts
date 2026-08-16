import crypto from "node:crypto";
import type { FastifyReply, FastifyRequest } from "fastify";
import type { Db } from "./db.js";
import { hashPassword, verifyPassword } from "./crypto.js";

const COOKIE_NAME = "mojito_token";
const TOKEN_TTL_MS = 30 * 24 * 3600 * 1000;

export class Auth {
  private tokens = new Map<string, number>();

  constructor(
    private db: Db,
    private loopback: boolean
  ) {}

  passwordSet(): boolean {
    return this.db.getSetting("password_hash") != null;
  }

  /** 需要认证 = 设置过密码，或对外绑定 */
  required(): boolean {
    return this.passwordSet() || !this.loopback;
  }

  login(password: string): string | null {
    const stored = this.db.getSetting("password_hash");
    if (!stored || !verifyPassword(password, stored)) return null;
    const token = crypto.randomBytes(32).toString("hex");
    this.tokens.set(token, Date.now() + TOKEN_TTL_MS);
    return token;
  }

  logout(token: string | undefined) {
    if (token) this.tokens.delete(token);
  }

  setPassword(next: string, current?: string): boolean {
    const stored = this.db.getSetting("password_hash");
    if (stored && (!current || !verifyPassword(current, stored))) return false;
    this.db.setSetting("password_hash", hashPassword(next));
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
