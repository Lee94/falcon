import type {
  DeadReason,
  NonDurableReason,
  ServerMessage,
  Session,
} from "@mojito/shared";
import type { Db, ProjectRow, SessionRow } from "../db.js";
import { Db as DbStatics } from "../db.js";
import type { SecretBox } from "../crypto.js";
import { RingBuffer } from "../ringbuffer.js";
import type { HostLayout } from "../zellij/host.js";
import type { StageFn } from "../zellij/install.js";
import type { Backend } from "./backend.js";
import { SessionGoneError } from "./backend.js";
import {
  attachLocal,
  localHasSession,
  localKill,
  peekLocalZellij,
  prepareLocalZellij,
  resetLocalZellij,
} from "./local.js";
import { SshLink } from "./ssh.js";

export interface Viewer {
  send(msg: ServerMessage): void;
}

/** 某个项目的宿主机能否提供持久会话 */
export interface DurableState {
  durable: boolean;
  reason?: NonDurableReason;
  /** 失败详情，供 UI 显示"到底卡在哪"，用户据此决定是修环境还是直接重试 */
  detail?: string;
  layout?: HostLayout;
}

interface LiveEntry {
  sessionId: string;
  projectId: string;
  durable: boolean;
  /** 持久会话所用的 Zellij 布局，接回时要用 */
  layout: HostLayout | null;
  backend: Backend | null;
  buffer: RingBuffer;
  viewers: Set<Viewer>;
  cols: number;
  rows: number;
  attaching: Promise<void> | null;
  terminating: boolean;
}

interface ReconnectState {
  attempt: number;
  timer: NodeJS.Timeout | null;
}

const RECONNECT_MAX_DELAY = 30_000;

export class SessionManager {
  private links = new Map<string, SshLink>();
  private entries = new Map<string, LiveEntry>();
  private reconnects = new Map<string, ReconnectState>();
  private lastTouch = new Map<string, number>();

  constructor(
    private db: Db,
    private secrets: SecretBox,
    private dataDir: string
  ) {
    this.db.recoverSessionsOnStartup();
  }

  /**
   * 判定某项目的宿主机能否提供持久会话，必要时触发 Zellij 安装。
   *
   * SSH 项目要求用户先授权——这会往用户的服务器上写入可执行文件，属于该问的那一类。
   * 授权按主机记（host+port+username），不按项目：同一台机器上的第二个项目
   * 不该再问一遍，二进制本来就已经装好了。
   */
  async prepare(
    project: ProjectRow,
    onStage?: StageFn,
    signal?: AbortSignal,
    /**
     * fresh：丢掉上一次的判定重新来过。用户显式发起安装/重试时必须带上——
     * 失败判定是缓存的，不清掉的话点一百次重试都是同一个秒回的失败。
     */
    opts?: { fresh?: boolean }
  ): Promise<DurableState> {
    if (project.type === "local") {
      if (opts?.fresh) resetLocalZellij();
      const local = await prepareLocalZellij(this.dataDir, onStage, signal);
      return {
        durable: local.durable,
        reason: local.reason,
        detail: local.detail,
        layout: local.layout,
      };
    }

    const saved = this.db.getZellijHost(
      project.ssh_host!,
      project.ssh_port ?? 22,
      project.ssh_username!
    );
    if (saved?.authorized !== 1) {
      return { durable: false, reason: "not-authorized" };
    }

    const link = this.getLink(project);
    if (opts?.fresh) link.resetZellij();
    const remote = await link.prepareZellij(onStage, signal);
    return {
      durable: remote.durable,
      reason: remote.reason,
      detail: remote.detail,
      layout: remote.layout,
    };
  }

  /** 用户改了授权或下载源之后，清掉缓存的判定以便重试 */
  resetPrepare(projectId: string) {
    this.links.get(projectId)?.resetZellij();
  }

  /** 本地宿主的持久能力；null = 尚未探测（不主动触发安装） */
  localDurableState(): DurableState | null {
    const local = peekLocalZellij();
    if (!local) return null;
    return { durable: local.durable, reason: local.reason, layout: local.layout };
  }

  // ---------- 项目链路 ----------

  getLink(project: ProjectRow): SshLink {
    let link = this.links.get(project.id);
    if (!link) {
      link = new SshLink(project, this.db, this.secrets);
      link.on("down", () => this.handleLinkDown(project.id));
      this.links.set(project.id, link);
    } else {
      link.updateProject(project);
    }
    return link;
  }

  disposeLink(projectId: string) {
    this.links.get(projectId)?.dispose();
    this.links.delete(projectId);
    const rec = this.reconnects.get(projectId);
    if (rec?.timer) clearTimeout(rec.timer);
    this.reconnects.delete(projectId);
  }

  // ---------- 会话生命周期 ----------

  async createSession(project: ProjectRow, name: string): Promise<Session> {
    const id = crypto.randomUUID();
    const now = Date.now();

    const prep = await this.prepare(project);

    const row: SessionRow = {
      id,
      project_id: project.id,
      name,
      state: "active",
      durable: prep.durable ? 1 : 0,
      dead_reason: null,
      non_durable_reason: prep.durable ? null : (prep.reason ?? null),
      created_at: now,
      last_active_at: now,
    };

    const entry: LiveEntry = {
      sessionId: id,
      projectId: project.id,
      durable: prep.durable,
      layout: prep.layout ?? null,
      backend: null,
      buffer: new RingBuffer(),
      viewers: new Set(),
      cols: 80,
      rows: 24,
      attaching: null,
      terminating: false,
    };
    this.entries.set(id, entry);

    try {
      await this.attachBackend(entry, project, row, false);
    } catch (err) {
      this.entries.delete(id);
      throw err;
    }

    this.db.insertSession(row);
    return DbStatics.toSession(row);
  }

  private async attachBackend(
    entry: LiveEntry,
    project: ProjectRow,
    row: SessionRow,
    reattach: boolean
  ): Promise<void> {
    const cb = {
      onData: (data: string) => {
        entry.buffer.append(data);
        this.broadcast(entry, { type: "output", data });
      },
      onExit: () => this.handleBackendExit(entry.sessionId),
    };

    // 接回时 entry.layout 可能是空的（后端重启后重建的 entry），重新准备一次
    if (entry.durable && !entry.layout) {
      const prep = await this.prepare(project);
      if (!prep.durable || !prep.layout) throw new SessionGoneError();
      entry.layout = prep.layout;
    }

    const opts = {
      sessionId: entry.sessionId,
      cwd: project.working_dir ?? undefined,
      shell: project.shell ?? undefined,
      durable: entry.durable,
      layout: entry.layout ?? undefined,
      reattach,
      cols: entry.cols,
      rows: entry.rows,
    };

    const result =
      project.type === "local"
        ? await attachLocal(opts, cb)
        : await this.getLink(project).attachSession(opts, cb);

    entry.backend = result.backend;
    if (reattach && result.capturedHistory != null) {
      entry.buffer.reset(result.capturedHistory);
    }
  }

  /**
   * 懒惰接回：确保会话有活的 backend。
   * unverified 的持久会话在此被验证并恢复为 active；tmux 不在了则标记 dead。
   */
  async ensureAttached(sessionId: string): Promise<SessionRow> {
    const row = this.db.getSession(sessionId);
    if (!row) throw new NotFoundError("会话不存在");
    if (row.state === "dead") return row;

    let entry = this.entries.get(sessionId);
    if (!entry) {
      entry = {
        sessionId,
        projectId: row.project_id,
        durable: row.durable === 1,
        layout: null,
        backend: null,
        buffer: new RingBuffer(),
        viewers: new Set(),
        cols: 80,
        rows: 24,
        attaching: null,
        terminating: false,
      };
      this.entries.set(sessionId, entry);
    }
    if (entry.backend) return row;

    if (!entry.durable) {
      // 非持久会话丢了 backend 即死亡（理论上已在别处标记）
      this.markDead(entry, "backend-restart");
      return this.db.getSession(sessionId)!;
    }

    if (!entry.attaching) {
      entry.attaching = (async () => {
        const project = this.db.getProject(row.project_id);
        if (!project) throw new NotFoundError("项目不存在");
        try {
          await this.attachBackend(entry!, project, row, true);
          this.db.updateSessionState(sessionId, "active");
          this.broadcast(entry!, { type: "state", state: "active" });
        } catch (err) {
          if (err instanceof SessionGoneError) {
            this.markDead(entry!, "session-gone");
          }
          throw err;
        }
      })().finally(() => {
        entry!.attaching = null;
      });
    }
    await entry.attaching;
    return this.db.getSession(sessionId)!;
  }

  /**
   * 终止一个会话。
   *
   * waitGone：终止后等到宿主机上的 Zellij session 真的消失。删除 worktree 目录之前
   * 必须等——Zellij pane 的 cwd 就在那个目录里，Windows 上会被句柄占住直接删不掉；
   * POSIX 上删得掉，但没死透的会话 cwd 变成已删 inode，pwd 报错、任何碰 `.` 的命令
   * 行为诡异，比 Windows 隐蔽得多。
   *
   * 用 Zellij 侧的会话存在性当信号，而不是等 backend 退出：Backend.destroy() 返回
   * void（本地是 proc.kill()、SSH 是 stream.close()，都是异步的），接口层面根本拿不到
   * "已经退出"。改那个接口要动本地与 SSH 两条实现，不在这个功能的范围里。
   * 非持久会话没有可观测的信号，等不了——由调用方的退避重试兜底。
   */
  async terminate(sessionId: string, opts?: { waitGone?: boolean }): Promise<void> {
    const row = this.db.getSession(sessionId);
    if (!row) return;
    const entry = this.entries.get(sessionId);
    if (entry) entry.terminating = true;

    if (row.durable === 1) {
      const project = this.db.getProject(row.project_id);
      const layout = entry?.layout ?? (project ? (await this.prepare(project)).layout : null);
      if (project && layout) {
        if (project.type === "local") {
          await localKill(layout, sessionId);
          if (opts?.waitGone) {
            await waitGone(() => localHasSession(layout, sessionId).catch(() => false));
          }
        } else {
          const link = this.getLink(project);
          await link.killSession(layout, sessionId).catch(() => {});
          if (opts?.waitGone) {
            await waitGone(() => link.hasSession(layout, sessionId).catch(() => false));
          }
        }
      }
    }
    entry?.backend?.destroy();

    if (entry) {
      this.broadcast(entry, { type: "state", state: "dead", deadReason: "exited" });
      this.entries.delete(sessionId);
    }
    this.db.deleteSession(sessionId);
  }

  /** 清除 dead 会话记录 */
  deleteDead(sessionId: string): boolean {
    const row = this.db.getSession(sessionId);
    if (!row || row.state !== "dead") return false;
    this.entries.delete(sessionId);
    this.db.deleteSession(sessionId);
    return true;
  }

  // ---------- Viewer ----------

  async addViewer(sessionId: string, viewer: Viewer): Promise<void> {
    const row = this.db.getSession(sessionId);
    if (!row) {
      viewer.send({ type: "error", message: "会话不存在" });
      return;
    }
    if (row.state === "dead") {
      viewer.send({
        type: "state",
        state: "dead",
        deadReason: (row.dead_reason as DeadReason) ?? undefined,
      });
      return;
    }

    try {
      await this.ensureAttached(sessionId);
    } catch (err) {
      const fresh = this.db.getSession(sessionId);
      if (fresh?.state === "dead") {
        viewer.send({
          type: "state",
          state: "dead",
          deadReason: (fresh.dead_reason as DeadReason) ?? undefined,
        });
      } else {
        viewer.send({ type: "error", message: `接回失败：${(err as Error).message}` });
        viewer.send({ type: "state", state: "unverified" });
        // 让自动重连接管（若是 SSH 链路问题）
        const entry = this.entries.get(sessionId);
        if (entry) {
          entry.viewers.add(viewer);
          this.scheduleReconnect(entry.projectId);
        }
        return;
      }
      return;
    }

    const entry = this.entries.get(sessionId)!;
    entry.viewers.add(viewer);
    viewer.send({ type: "replay", data: entry.buffer.snapshot() });
    viewer.send({ type: "state", state: "active" });
    this.touch(sessionId, true);
  }

  removeViewer(sessionId: string, viewer: Viewer) {
    this.entries.get(sessionId)?.viewers.delete(viewer);
  }

  input(sessionId: string, data: string) {
    const entry = this.entries.get(sessionId);
    entry?.backend?.write(data);
    this.touch(sessionId, false);
  }

  resize(sessionId: string, cols: number, rows: number) {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    entry.cols = cols;
    entry.rows = rows;
    entry.backend?.resize(cols, rows);
  }

  // ---------- 内部 ----------

  private broadcast(entry: LiveEntry, msg: ServerMessage) {
    for (const v of entry.viewers) v.send(msg);
  }

  private markDead(entry: LiveEntry, reason: DeadReason) {
    entry.backend = null;
    this.db.updateSessionState(entry.sessionId, "dead", reason);
    this.broadcast(entry, { type: "state", state: "dead", deadReason: reason });
  }

  private markUnverified(entry: LiveEntry) {
    entry.backend = null;
    this.db.updateSessionState(entry.sessionId, "unverified");
    this.broadcast(entry, { type: "state", state: "unverified" });
  }

  /** backend 附着结束：区分真退出 / 手动 detach / 链路断开 */
  private async handleBackendExit(sessionId: string) {
    const entry = this.entries.get(sessionId);
    if (!entry || entry.terminating) return;
    entry.backend = null;

    const row = this.db.getSession(sessionId);
    if (!row) return;
    const project = this.db.getProject(entry.projectId);

    if (!entry.durable) {
      const reason: DeadReason =
        project?.type === "ssh" && !this.links.get(entry.projectId)?.isConnected()
          ? "link-lost"
          : "exited";
      this.markDead(entry, reason);
      return;
    }

    // 持久会话：Zellij session 还在 → unverified（可接回）；不在 → 真退出
    if (project?.type === "local") {
      if (entry.layout && (await localHasSession(entry.layout, sessionId))) {
        this.markUnverified(entry);
      } else {
        this.markDead(entry, "exited");
      }
      return;
    }

    const link = this.links.get(entry.projectId);
    if (!link?.isConnected() || !entry.layout) {
      // 链路断开，交给 handleLinkDown / 重连流程
      this.markUnverified(entry);
      return;
    }
    try {
      if (await link.hasSession(entry.layout, sessionId)) {
        this.markUnverified(entry);
      } else {
        this.markDead(entry, "exited");
      }
    } catch {
      this.markUnverified(entry);
    }
  }

  private handleLinkDown(projectId: string) {
    let needReconnect = false;
    for (const entry of this.entries.values()) {
      if (entry.projectId !== projectId) continue;
      if (entry.terminating) continue;
      const row = this.db.getSession(entry.sessionId);
      if (!row || row.state === "dead") continue;
      if (entry.durable) {
        this.markUnverified(entry);
        if (entry.viewers.size > 0) needReconnect = true;
      } else {
        this.markDead(entry, "link-lost");
      }
    }
    if (needReconnect) this.scheduleReconnect(projectId);
  }

  /** SSH 断线自动重连：指数退避，只在还有 Viewer 观看时坚持 */
  private scheduleReconnect(projectId: string) {
    const existing = this.reconnects.get(projectId);
    if (existing?.timer) return;

    const state: ReconnectState = existing ?? { attempt: 0, timer: null };
    this.reconnects.set(projectId, state);

    const watchers = () =>
      [...this.entries.values()].filter(
        (e) =>
          e.projectId === projectId &&
          e.durable &&
          !e.backend &&
          e.viewers.size > 0 &&
          this.db.getSession(e.sessionId)?.state === "unverified"
      );

    const tick = async () => {
      state.timer = null;
      const targets = watchers();
      if (targets.length === 0) {
        this.reconnects.delete(projectId);
        return;
      }
      state.attempt++;
      for (const e of targets) {
        this.broadcast(e, { type: "reconnecting", attempt: state.attempt });
      }
      const project = this.db.getProject(projectId);
      if (!project) {
        this.reconnects.delete(projectId);
        return;
      }
      try {
        await this.getLink(project).getClient();
        for (const e of watchers()) {
          try {
            await this.ensureAttached(e.sessionId);
          } catch {
            // 单个会话接回失败（如 tmux-gone），已在 ensureAttached 中标记
          }
        }
        this.reconnects.delete(projectId);
      } catch {
        const delay = Math.min(
          RECONNECT_MAX_DELAY,
          1000 * 2 ** Math.min(state.attempt - 1, 10)
        );
        state.timer = setTimeout(tick, delay);
      }
    };

    state.timer = setTimeout(tick, 1000);
  }

  private touch(sessionId: string, force: boolean) {
    const now = Date.now();
    const last = this.lastTouch.get(sessionId) ?? 0;
    if (force || now - last > 30_000) {
      this.lastTouch.set(sessionId, now);
      this.db.touchSession(sessionId, now);
    }
  }
}

export class NotFoundError extends Error {}

/**
 * 轮询到 still() 返回 false 为止，或用完次数。
 * 超时不算失败：调用方还有退避重试，实在删不掉会如实报给用户。
 */
async function waitGone(still: () => Promise<boolean>, tries = 10, delayMs = 300) {
  for (let i = 0; i < tries; i++) {
    if (!(await still())) return;
    await new Promise((r) => setTimeout(r, delayMs));
  }
}
