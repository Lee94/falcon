/**
 * 存档附属项目的到期清扫：archived_at 超过 WORKTREE_ARCHIVE_TTL_MS 的行，
 * 自动删除项目并清理 worktree 目录。启动时跑一次，此后每小时一轮。
 *
 * 与手动删除（DELETE /api/projects/:id）刻意不同的一点：那边"DB 行无条件删，
 * 文件系统清理 best-effort"，因为 warning 会原样摆到用户面前；这边没人在看——
 * **目录还在就把行留着**，下一轮再试。链路抖动（SSH 不在线）自愈；删不掉的
 * 目录（worktree lock、只读文件）让项目留在"已存档"列表里，用户手动删除时
 * 能看到具体原因。绝不静默地把目录变成孤儿。
 */

import { WORKTREE_ARCHIVE_TTL_MS } from "@mojito/shared";
import type { Db, ProjectRow } from "./db.js";
import { cleanupWorktree } from "./git/remove.js";
import { gitHostFor } from "./git/host.js";
import { pathExists, TIMEOUT_REMOVE } from "./git/repo.js";
import type { SessionManager } from "./sessions/manager.js";

const SWEEP_INTERVAL_MS = 60 * 60 * 1000;

interface SweepLog {
  info(msg: string): void;
  warn(msg: string): void;
}

/** 启动清扫。返回停止函数，供优雅关闭调用。 */
export function startArchiveSweeper(
  db: Db,
  manager: SessionManager,
  log: SweepLog
): () => void {
  let sweeping = false;

  const sweepOne = async (row: ProjectRow, otherDirs: string[]) => {
    // 存档时会话已随手清掉；这里再兜一遍——绝不带着活进程删目录
    for (const s of db.listSessionsByProject(row.id)) {
      if (s.state === "dead") manager.deleteDead(s.id);
      else await manager.terminate(s.id, { waitGone: true });
    }

    // 没有目录可清的脏数据行，直接删掉了事
    if (!row.working_dir) {
      manager.disposeLink(row.id);
      db.deleteProject(row.id);
      return;
    }

    const host = await gitHostFor(row, manager);
    const warnings = await cleanupWorktree(row, host, otherDirs);
    const gone = !(await pathExists(host, row.working_dir, {
      timeoutMs: TIMEOUT_REMOVE,
    }).catch(() => true));
    if (!gone) {
      log.warn(
        `存档到期的附属项目「${row.name}」目录未能清理，保留待下一轮重试：` +
          `${row.working_dir}${warnings.length ? `（${warnings.join("；")}）` : ""}`
      );
      return;
    }
    manager.disposeLink(row.id);
    db.deleteProject(row.id);
    log.info(`已自动删除存档到期的附属项目「${row.name}」（${row.working_dir}）`);
  };

  const sweep = async () => {
    if (sweeping) return;
    sweeping = true;
    try {
      const expired = db.listArchivedExpired(Date.now() - WORKTREE_ARCHIVE_TTL_MS);
      if (expired.length === 0) return;
      const doomed = new Set(expired.map((p) => p.id));
      const otherDirs = db
        .listProjects()
        .filter((p) => !doomed.has(p.id))
        .map((p) => p.working_dir)
        .filter((d): d is string => !!d);
      for (const row of expired) {
        try {
          await sweepOne(row, otherDirs);
        } catch (err) {
          // 一台宿主机连不上不能拖垮整轮清扫，也不能删行——目录清没清成还不知道
          log.warn(
            `清扫存档附属项目「${row.name}」失败，保留待下一轮重试：${(err as Error).message}`
          );
        }
      }
    } finally {
      sweeping = false;
    }
  };

  void sweep();
  const timer = setInterval(() => void sweep(), SWEEP_INTERVAL_MS);
  timer.unref();
  return () => clearInterval(timer);
}
