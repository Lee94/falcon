/**
 * sudo askpass 的会合点：helper 长轮询 POST /api/askpass，网页对话框
 * POST /api/askpass/:id/answer 把密码送回来。
 *
 * 密码只在 pending Promise 里过一遭，不落盘、不打日志。
 */
import crypto from "node:crypto";
import { ASKPASS_TIMEOUT_MS } from "./scripts.js";

export interface AskpassPrompt {
  id: string;
  prompt: string;
  sessionId?: string;
}

export class AskpassCancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "AskpassCancelled";
  }
}

export class AskpassTimeout extends Error {
  constructor() {
    super("timeout");
    this.name = "AskpassTimeout";
  }
}

interface Pending {
  prompt: AskpassPrompt;
  resolve: (password: string) => void;
  reject: (err: Error) => void;
  timer: NodeJS.Timeout;
}

export class AskpassHub {
  readonly token: string;
  /** helper 要 POST 的完整 URL，listen 之后才能定 port */
  origin = "http://127.0.0.1:4923";
  onPrompt: ((p: AskpassPrompt) => void) | null = null;

  private pending = new Map<string, Pending>();

  constructor() {
    this.token = crypto.randomBytes(24).toString("hex");
  }

  setOrigin(origin: string) {
    this.origin = origin.replace(/\/$/, "");
  }

  helperUrl(): string {
    return `${this.origin}/api/askpass`;
  }

  tokenMatches(header: string | undefined): boolean {
    if (!header) return false;
    const m = /^Bearer\s+(\S+)/i.exec(header.trim());
    return m != null && m[1] === this.token;
  }

  request(input: { prompt: string; sessionId?: string }): Promise<string> {
    const id = crypto.randomUUID();
    const prompt: AskpassPrompt = {
      id,
      prompt: input.prompt || "Password:",
      sessionId: input.sessionId || undefined,
    };
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new AskpassTimeout());
      }, ASKPASS_TIMEOUT_MS);
      this.pending.set(id, { prompt, resolve, reject, timer });
      try {
        this.onPrompt?.(prompt);
      } catch {
        // 通知 UI 失败不该把 helper 的请求打掉——用户还可能从别的入口答
      }
    });
  }

  answer(id: string, password: string): boolean {
    const p = this.pending.get(id);
    if (!p) return false;
    this.pending.delete(id);
    clearTimeout(p.timer);
    p.resolve(password);
    return true;
  }

  cancel(id: string): boolean {
    const p = this.pending.get(id);
    if (!p) return false;
    this.pending.delete(id);
    clearTimeout(p.timer);
    p.reject(new AskpassCancelled());
    return true;
  }

  peek(id: string): AskpassPrompt | undefined {
    return this.pending.get(id)?.prompt;
  }

  /** 新 Viewer 连上来时补发还在等的弹窗，避免打开页面时已经错过 onPrompt */
  pendingPrompts(): AskpassPrompt[] {
    return [...this.pending.values()].map((p) => p.prompt);
  }
}
