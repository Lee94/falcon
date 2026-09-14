/**
 * 带 TTL 与 in-flight 合并的内存缓存。
 *
 * 飞书项目面板两边共用：服务端挡住重复的 meegle CLI 进程（一次待办 2–4s），
 * 前端挡住面板卸载后再挂上的重复 HTTP。同一 key 并发只跑一份 load；load 失败不入库，
 * 下次再试。`fresh` 跳过已有条目，但仍并入正在飞的那一次——刷新时别再起一条 CLI。
 *
 * `clear()` 会抬 generation：清之前已经在飞的 load 回来后不许写回，免得刚刷新又被旧结果盖住。
 */

export interface TtlCacheEntry<T> {
  at: number;
  value: T;
}

export interface TtlCacheLoadOpts {
  /** 忽略已有条目，强制跑 load（仍并入 in-flight） */
  fresh?: boolean;
  /** 覆盖构造时的默认 TTL */
  ttlMs?: number;
  /** 测试用：冻结"现在" */
  now?: number;
}

interface Slot {
  at: number;
  exp: number;
  value: unknown;
}

export class TtlCache {
  private values = new Map<string, Slot>();
  private inflight = new Map<string, Promise<unknown>>();
  private gen = 0;

  constructor(private readonly ttlMs: number) {}

  peek<T>(key: string, now = Date.now()): TtlCacheEntry<T> | undefined {
    const hit = this.values.get(key);
    if (!hit) return undefined;
    if (now >= hit.exp) {
      this.values.delete(key);
      return undefined;
    }
    return { at: hit.at, value: hit.value as T };
  }

  set<T>(key: string, value: T, opts?: { ttlMs?: number; now?: number }): void {
    const now = opts?.now ?? Date.now();
    const ttl = opts?.ttlMs ?? this.ttlMs;
    this.values.set(key, { at: now, exp: now + ttl, value });
  }

  delete(key: string): void {
    this.values.delete(key);
  }

  clear(): void {
    this.gen++;
    this.values.clear();
    this.inflight.clear();
  }

  async getOrLoad<T>(key: string, load: () => Promise<T>, opts?: TtlCacheLoadOpts): Promise<T> {
    const now = opts?.now ?? Date.now();
    const ttl = opts?.ttlMs ?? this.ttlMs;
    if (!opts?.fresh) {
      const hit = this.peek<T>(key, now);
      if (hit) return hit.value;
    }
    const pending = this.inflight.get(key);
    if (pending) return pending as Promise<T>;

    const gen = this.gen;
    let p!: Promise<T>;
    p = (async () => {
      try {
        const value = await load();
        if (this.gen === gen) this.set(key, value, { ttlMs: ttl, now: opts?.now ?? Date.now() });
        return value;
      } finally {
        if (this.inflight.get(key) === p) this.inflight.delete(key);
      }
    })();
    this.inflight.set(key, p);
    return p;
  }
}
