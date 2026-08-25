/**
 * 按仓库串行化 worktree 操作。
 *
 * 并发 `worktree add` 会争 .git 下的锁，报出来的
 * `fatal: Unable to create ... File exists` 对用户毫无意义；两个请求算出同一个目标
 * 目录时还会撞 TOCTOU。服务端是单进程，一个进程内的 Promise 链就够了——
 * 形状照抄 SessionManager 里 `entry.attaching: Promise | null` 的做法。
 *
 * 键必须是「宿主机 + 仓库根」，**不是 projectId**：两个 Project 完全可能指向同一个
 * 仓库，按 projectId 加锁等于没加。
 */

import { canonKey } from "./path.js";
import type { GitHost } from "./host.js";

/**
 * 仓库锁的唯一键构造。此前三处调用点手拼：派生用 `::`、commit 与 pull/push 用
 * `:`——两个键空间不相交，同一仓库上的派生与 pull/push 根本不互斥（潜伏 bug）。
 * 统一从这里出；分隔符取 `::`，因为 ssh 的 host.key 本身就含 `:`。
 */
export function repoLockKey(host: Pick<GitHost, "key" | "kind">, repoRoot: string): string {
  return `${host.key}::${canonKey(host.kind, repoRoot)}`;
}

const chains = new Map<string, Promise<unknown>>();

export async function withRepoLock<T>(key: string, fn: () => Promise<T>): Promise<T> {
  const prev = chains.get(key) ?? Promise.resolve();
  // 前一个失败也要让后一个跑起来，所以两个分支都接 fn
  const next = prev.then(fn, fn);
  const guarded = next.catch(() => {});
  chains.set(key, guarded);
  try {
    return await next;
  } finally {
    // 自己是队尾才清理，否则会把后来者的链头擦掉
    if (chains.get(key) === guarded) chains.delete(key);
  }
}
