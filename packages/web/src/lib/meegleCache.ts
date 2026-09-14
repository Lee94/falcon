import { TtlCache } from "@falcon/shared";

/**
 * 飞书项目面板的前端内存缓存。面板卸载（切到 Git、收起右侧栏）后数据还在，
 * 再打开立刻画出上次的列表；TTL 内不打网络。刷新按钮 / 换人登录会清空。
 *
 * 不进 localStorage：工作项是内部数据，换浏览器也不该带过去。后端 client.ts
 * 另有一份同样 TTL 的 CLI 缓存，整页刷新后 HTTP 也是毫秒级，不必在浏览器里持久化。
 */

export const MEEGLE_CACHE_MS = 5 * 60_000;
/** 超过这个时间仍画出缓存，但后台静默再拉一次 */
export const MEEGLE_REVALIDATE_MS = 30_000;

const cache = new TtlCache(MEEGLE_CACHE_MS);

export function peekMeegleCache<T>(key: string): T | undefined {
  return cache.peek<T>(key)?.value;
}

export function meegleCacheStale(key: string, now = Date.now()): boolean {
  const hit = cache.peek(key, now);
  if (!hit) return true;
  return now - hit.at >= MEEGLE_REVALIDATE_MS;
}

export function writeMeegleCache<T>(key: string, value: T): void {
  cache.set(key, value);
}

export function loadMeegleCache<T>(key: string, load: () => Promise<T>, fresh = false): Promise<T> {
  return cache.getOrLoad(key, load, { fresh });
}

export function clearMeegleCache(): void {
  cache.clear();
}
