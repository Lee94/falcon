import type {
  DeadReason,
  NonDurableReason,
  ServerMessage,
  Session,
  SessionForeground,
  TermAppearance,
} from "@falcon/shared";
import {
  OscColorGate,
  TERM_FRAME_OUTPUT,
  TERM_FRAME_REPLAY,
  isTermAppearance,
  parseHexRgb,
} from "@falcon/shared";
import type { Db, ProjectRow, SessionRow, SshHostRow } from "../db.js";
import { Db as DbStatics } from "../db.js";
import type { SecretBox } from "../crypto.js";
import { RingBuffer } from "../ringbuffer.js";
import type { HostLayout } from "../zellij/host.js";
import type { StageFn } from "../zellij/install.js";
import type { Backend } from "./backend.js";
import { SessionGoneError } from "./backend.js";
import { isShellCommand } from "../shells.js";
import {
  attachLocal,
  localForeground,
  localHasSession,
  localKill,
  peekLocalZellij,
  prepareLocalZellij,
  resetLocalZellij,
} from "./local.js";
import { ForwardManager } from "./forward.js";
import { SshLink } from "./ssh.js";
import {
  decideViewerAttach,
  fallbackTermSize,
  parseStoredTermSize,
} from "./termSize.js";
import { TermModeTracker } from "./termModes.js";

export interface Viewer {
  /** 控制类消息（state / reconnecting / error），JSON 文本帧 */
  send(msg: ServerMessage): void;
  /**
   * 终端数据帧（TERM_FRAME_*，二进制）。返回 false 表示对端发送缓冲
   * 已堆积到阈值、本帧被丢弃——数据都在 RingBuffer 里，调用方据此把
   * Viewer 标记为落后，等 drained() 后用 replay 重新同步。
   */
  sendBytes(frame: Uint8Array): boolean;
  /** 发送缓冲已排空到可以重新同步 */
  drained(): boolean;
}

/** 终端数据帧：1 字节类型 + UTF-8 载荷，广播前只序列化一次 */
function encodeTermFrame(kind: number, data: string): Buffer {
  const frame = Buffer.allocUnsafe(1 + Buffer.byteLength(data));
  frame[0] = kind;
  frame.write(data, 1, "utf8");
  return frame;
}

/**
 * 输出合并窗口。node-pty / ssh channel 在高吞吐下（`yes`、构建日志）每秒
 * 触发上千次 onData，逐 chunk 一帧就是每秒上千次序列化 + syscall。窗口内
 * 攒起来一次发，把帧率封在 ~60/s，肉眼无感知延迟。
 */
const OUTPUT_FLUSH_MS = 16;

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
  /** 合并窗口内攒下的输出（已过 OSC 网关、已进 RingBuffer），flush 时一次广播 */
  pendingOut: string[];
  flushTimer: NodeJS.Timeout | null;
  /** null = 从未被 Viewer 量过；接回时不能用 80×24 顶替 */
  cols: number | null;
  rows: number | null;
  attaching: Promise<void> | null;
  terminating: boolean;
  /** 当前 Viewer 报上来的终端深浅；接回后的内层 env 冻住了，只影响 OSC 答复和新会话 */
  appearance?: TermAppearance;
  background?: string;
  foreground?: string;
  osc: OscColorGate;
  /** 输出流里的 VT 模式跟踪，replay 时重建（模式序列早被 RingBuffer 挤掉了） */
  modes: TermModeTracker;
}

interface ReconnectState {
  attempt: number;
  timer: NodeJS.Timeout | null;
}

const RECONNECT_MAX_DELAY = 30_000;

/** 浏览远端目录时把已保存主机构成一条假 ProjectRow，好复用 SshLink。 */
export function hostAsProject(host: SshHostRow): ProjectRow {
  return {
    id: `host:${host.id}`,
    name: host.name,
    type: "ssh",
    working_dir: null,
    shell: null,
    ssh_host: host.host,
    ssh_port: host.port,
    ssh_username: host.username,
    ssh_auth_method: host.auth_method,
    ssh_key_path: host.key_path,
    ssh_secret_enc: host.secret_enc,
    host_id: host.id,
    created_at: host.created_at,
    source_project_id: null,
    worktree_branch: null,
    worktree_repo_dir: null,
    worktree_created_by_mojito: null,
    worktree_archived_at: null,
    multi_repos: null,
  };
}

export class SessionManager {
  private links = new Map<string, SshLink>();
  /** 按 hostId 缓存，给还没有项目的「浏览远端目录」复用，避免每点一层就重连 */
  private hostLinks = new Map<string, SshLink>();
  private entries = new Map<string, LiveEntry>();
  private reconnects = new Map<string, ReconnectState>();
  /** 本地持久会话的自动接回退避，键是 sessionId */
  private localReattach = new Map<string, ReconnectState>();
  private lastTouch = new Map<string, number>();
  /** resize 落库的合并窗口：拖窗时前端逐帧发 resize，不能每次都同步 fsync */
  private sizeFlush = new Map<string, NodeJS.Timeout>();

  readonly forwards: ForwardManager;

  constructor(
    private db: Db,
    private secrets: SecretBox,
    private dataDir: string
  ) {
    this.db.recoverSessionsOnStartup();
    this.forwards = new ForwardManager(db, (projectId) => {
      const project = this.db.getProject(projectId);
      return project?.type === "ssh" ? this.getLink(project) : null;
    });
  }

  /**
   * 后端刚起来时，上次 active 的持久会话都在 DB 里停成 unverified。
   * 以前要等用户打开 tab 或点「接回」才验证—— falcon 一重启侧栏就一片黄。
   * 这里按库存尺寸自动接回（跟总览里点接回同一条路，见 termSize.ts）。
   */
  resumeUnverified(): void {
    for (const row of this.db.listSessions()) {
      if (row.state !== "unverified" || row.durable !== 1) continue;
      const entry = this.hydrateEntry(row);
      this.kickAutoReattach(entry);
    }
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
      link.on("up", () => void this.forwards.onLinkUp(project.id));
      this.links.set(project.id, link);
    } else {
      link.updateProject(project);
    }
    return link;
  }

  disposeLink(projectId: string) {
    this.forwards.stopAll(projectId);
    this.links.get(projectId)?.dispose();
    this.links.delete(projectId);
    const rec = this.reconnects.get(projectId);
    if (rec?.timer) clearTimeout(rec.timer);
    this.reconnects.delete(projectId);
  }

  /**
   * 已保存主机的浏览链路。改凭据或删主机时必须 dispose，否则会拿着旧密钥连。
   * 不挂 reconnect：浏览不是会话，断了下次点再连。
   */
  getHostLink(host: SshHostRow): SshLink {
    let link = this.hostLinks.get(host.id);
    if (!link) {
      link = new SshLink(hostAsProject(host), this.db, this.secrets);
      this.hostLinks.set(host.id, link);
    } else {
      link.updateProject(hostAsProject(host));
    }
    return link;
  }

  disposeHostLink(hostId: string) {
    this.hostLinks.get(hostId)?.dispose();
    this.hostLinks.delete(hostId);
  }

  /**
   * 试连一组 SSH 凭据。用一次性链路，测完就拆——
   * 表单里可能是还没保存的草稿，不能写进 hostLinks 污染浏览缓存。
   */
  async probeSsh(project: ProjectRow): Promise<{ kind: "posix" | "windows"; home: string }> {
    const link = new SshLink(project, this.db, this.secrets);
    try {
      return await link.hostFacts();
    } finally {
      link.dispose();
    }
  }

  // ---------- 会话生命周期 ----------

  async createSession(
    project: ProjectRow,
    name: string,
    hint?: { appearance?: TermAppearance; background?: string; foreground?: string }
  ): Promise<Session> {
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
      cols: null,
      rows: null,
    };

    const entry = this.liveEntry({
      sessionId: id,
      projectId: project.id,
      durable: prep.durable,
      layout: prep.layout ?? null,
      appearance: hint?.appearance,
      background: hint?.background,
      foreground: hint?.foreground,
    });
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
    let backend: Backend | null = entry.backend;
    const queuedReplies: string[] = [];
    const cb = {
      onData: (data: string) => {
        const { visible, replies } = entry.osc.push(data);
        for (const reply of replies) {
          if (backend) backend.write(reply);
          else queuedReplies.push(reply);
        }
        if (!visible) return;
        entry.modes.track(visible);
        entry.buffer.append(visible);
        this.queueOutput(entry, visible);
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
      ...fallbackTermSize(
        entry.cols != null && entry.rows != null
          ? { cols: entry.cols, rows: entry.rows }
          : null
      ),
      appearance: entry.appearance,
    };

    const result =
      project.type === "local"
        ? await attachLocal(opts, cb)
        : await this.getLink(project).attachSession(opts, cb);

    backend = result.backend;
    entry.backend = result.backend;
    for (const reply of queuedReplies) backend.write(reply);
    // attach 期间 Viewer 可能已经报了真实格子，按最新的再 resize 一次
    if (entry.cols != null && entry.rows != null) {
      backend.resize(entry.cols, entry.rows);
    }
    if (reattach && result.capturedHistory != null) {
      entry.buffer.reset(result.capturedHistory);
    }
  }

  /**
   * 确保会话有活的 backend。
   * unverified 的持久会话在此被验证并恢复为 active；Zellij 不在了则标记 dead。
   *
   * force：用户点了「接回」，或后台自动接回时没有 Viewer 在量格子。
   * 必须马上 attach 并把 DB 写成 active，不能卡在 unverified。
   */
  async ensureAttached(
    sessionId: string,
    opts?: { force?: boolean }
  ): Promise<SessionRow> {
    const row = this.db.getSession(sessionId);
    if (!row) throw new NotFoundError("会话不存在");
    if (row.state === "dead") return row;

    const entry = this.hydrateEntry(row);
    if (entry.backend) {
      // PTY 已挂上但 DB 还停在 unverified：点接回必须把状态扳回来
      if (row.state !== "active") return this.markActive(entry);
      return row;
    }

    if (!entry.durable) {
      // 非持久会话丢了 backend 即死亡（理论上已在别处标记）
      this.markDead(entry, "backend-restart");
      return this.db.getSession(sessionId)!;
    }

    // 有 Viewer 在量格子时等它报尺寸，避免用 80×24 抢跑把 TUI 挤扁。
    // 后台自动接回 / 用户点接回走 force，不能卡在 unverified。
    if (
      !opts?.force &&
      (entry.cols == null || entry.rows == null) &&
      entry.viewers.size > 0
    ) {
      return row;
    }

    if (!entry.attaching) {
      entry.attaching = (async () => {
        const project = this.db.getProject(row.project_id);
        if (!project) throw new NotFoundError("项目不存在");
        try {
          await this.attachBackend(entry, project, row, true);
          this.markActive(entry, { replay: true });
        } catch (err) {
          if (err instanceof SessionGoneError) {
            this.markDead(entry, "session-gone");
          }
          throw err;
        }
      })().finally(() => {
        entry.attaching = null;
      });
    }
    await entry.attaching;
    return this.db.getSession(sessionId)!;
  }

  /**
   * 会话前台是否有程序在跑（关 tab 前的确认依据）。
   *
   * 持久会话问 Zellij（list-clients 的 RUNNING_COMMAND 就是聚焦 pane 的前台
   * 命令）；非持久本地会话问 PTY 自己。侦测不到的场景一律按空闲放行：
   * 非持久 SSH（channel 里问不到远端 shell 的进程树）、Windows 远端
   * （Zellij 没有 /proc 可读，恒报 N/A）、unverified/断链（拿到的答案没有意义）。
   * 这是道保险，探测失败不能把关 tab 拦下来，所以也不抛错。
   */
  async foreground(sessionId: string): Promise<SessionForeground> {
    const idle: SessionForeground = { busy: false, command: null };
    const row = this.db.getSession(sessionId);
    if (!row || row.state !== "active") return idle;
    const project = this.db.getProject(row.project_id);
    if (!project) return idle;
    const entry = this.entries.get(sessionId);

    try {
      let command: string | null = null;
      if (row.durable !== 1) {
        command = entry?.backend?.processName?.() ?? null;
      } else if (entry?.layout) {
        // 没有活的附着就没有 Zellij 客户端，list-clients 必为空，不必跑
        if (project.type === "local") {
          command = await localForeground(entry.layout, sessionId);
        } else {
          const link = this.links.get(project.id);
          if (!link?.isConnected()) return idle; // 不为一次探测去重建链路
          command = await link.foreground(entry.layout, sessionId);
        }
      }
      if (!command || isShellCommand(command, project.shell)) return idle;
      return { busy: true, command };
    } catch {
      return idle;
    }
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
      this.dropOutput(entry);
      this.entries.delete(sessionId);
    }
    this.db.deleteSession(sessionId);
  }

  /** 清除 dead 会话记录 */
  deleteDead(sessionId: string): boolean {
    const row = this.db.getSession(sessionId);
    if (!row || row.state !== "dead") return false;
    const entry = this.entries.get(sessionId);
    if (entry) this.dropOutput(entry);
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

    const entry = this.hydrateEntry(row);

    const alreadyLive = !!entry.backend;
    // 先冲掉旧 Viewer 的合并窗口再加入新人：pending 里的数据已经进了
    // RingBuffer，不冲的话新 Viewer 会在 replay 之后再收到重复段
    this.flushOutput(entry);
    entry.viewers.add(viewer);

    const gate = decideViewerAttach({
      durable: entry.durable,
      hasBackend: alreadyLive,
    });

    if (gate === "dead") {
      this.markDead(entry, "backend-restart");
      viewer.send({
        type: "state",
        state: "dead",
        deadReason: "backend-restart",
      });
      return;
    }

    if (gate === "wait-size") {
      // 等这条连接自己的 resize 再 attach，见 decideViewerAttach。
      // 先把当前状态告诉 Viewer，不然 tab 会假装还在 active，标黄只出现在侧栏。
      viewer.send({
        type: "state",
        state: row.state === "active" ? "unverified" : row.state,
        deadReason: (row.dead_reason as DeadReason) ?? undefined,
      });
      return;
    }

    this.sendReplay(entry, viewer);
    viewer.send({ type: "state", state: "active" });
    this.touch(sessionId, true);
  }

  removeViewer(sessionId: string, viewer: Viewer) {
    this.entries.get(sessionId)?.viewers.delete(viewer);
    this.lagged.delete(viewer);
  }

  input(sessionId: string, data: string) {
    const entry = this.entries.get(sessionId);
    entry?.backend?.write(data);
    this.touch(sessionId, false);
  }

  resize(sessionId: string, cols: number, rows: number) {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    // 尺寸没变时只跳过落库与 backend.resize，attach 分支必须照走：
    // decideViewerAttach 的 wait-size 门控靠这条 resize 触发接回，
    // 而重连的 Viewer 报上来的尺寸很可能与存量一致。
    const changed = entry.cols !== cols || entry.rows !== rows;
    entry.cols = cols;
    entry.rows = rows;
    if (changed) this.scheduleSizePersist(sessionId);
    if (entry.backend) {
      if (changed) entry.backend.resize(cols, rows);
      return;
    }
    if (entry.durable && entry.viewers.size > 0 && !entry.terminating) {
      void this.ensureAttached(sessionId).catch((err) => {
        if (this.db.getSession(sessionId)?.state === "dead") return;
        this.broadcast(entry, {
          type: "error",
          message: `接回失败：${(err as Error).message}`,
        });
        this.broadcast(entry, { type: "state", state: "unverified" });
        this.scheduleReconnect(entry.projectId);
      });
    }
  }

  /**
   * Viewer 报上来的终端深浅。接回已有 Zellij 会话改不了内层 env，
   * 但 OSC 10/11/12 答复跟这份走，主题切换后新启动的查询能拿到新底色。
   *
   * 深浅真的翻转且流里见过 DECSET 2031 订阅时，再注入 CSI ?997;1/2 n：
   * Claude Code 这类 auto 主题程序运行中只认这个通知。Zellij client 自己
   * 就会订并把通知转发给订阅的 pane（0.44 实测）；没人订阅时绝不能注入，
   * 这串字节会被前台程序当键盘输入吃掉。
   */
  setAppearance(
    sessionId: string,
    appearance: TermAppearance,
    colors?: { background?: string; foreground?: string }
  ) {
    if (!isTermAppearance(appearance)) return;
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    const flipped = entry.appearance !== undefined && entry.appearance !== appearance;
    entry.appearance = appearance;
    if (colors?.background && parseHexRgb(colors.background)) {
      entry.background = colors.background;
    }
    if (colors?.foreground && parseHexRgb(colors.foreground)) {
      entry.foreground = colors.foreground;
    }
    if (flipped && entry.modes.themeNotify) {
      entry.backend?.write(appearance === "light" ? "\x1b[?997;2n" : "\x1b[?997;1n");
    }
  }

  // ---------- 内部 ----------

  /** 从 DB 行拿到（或新建）LiveEntry。库存尺寸只在还没被 Viewer 量过时用。 */
  private hydrateEntry(row: SessionRow): LiveEntry {
    const existing = this.entries.get(row.id);
    if (existing) return existing;
    const size = parseStoredTermSize(row.cols, row.rows);
    const entry = this.liveEntry({
      sessionId: row.id,
      projectId: row.project_id,
      durable: row.durable === 1,
      layout: null,
      cols: size?.cols ?? null,
      rows: size?.rows ?? null,
    });
    this.entries.set(row.id, entry);
    return entry;
  }

  private liveEntry(init: {
    sessionId: string;
    projectId: string;
    durable: boolean;
    layout: HostLayout | null;
    cols?: number | null;
    rows?: number | null;
    appearance?: TermAppearance;
    background?: string;
    foreground?: string;
  }): LiveEntry {
    const entry: LiveEntry = {
      sessionId: init.sessionId,
      projectId: init.projectId,
      durable: init.durable,
      layout: init.layout,
      backend: null,
      buffer: new RingBuffer(),
      viewers: new Set(),
      pendingOut: [],
      flushTimer: null,
      cols: init.cols ?? null,
      rows: init.rows ?? null,
      attaching: null,
      terminating: false,
      appearance: init.appearance,
      background: init.background,
      foreground: init.foreground,
      osc: null as unknown as OscColorGate,
      modes: new TermModeTracker(),
    };
    entry.osc = new OscColorGate(() => ({
      appearance: entry.appearance,
      background: entry.background,
      foreground: entry.foreground,
    }));
    return entry;
  }

  /** sendBytes 因背压丢过帧的 Viewer：等它排空后用 replay 整体重同步 */
  private lagged = new WeakSet<Viewer>();

  private queueOutput(entry: LiveEntry, data: string) {
    entry.pendingOut.push(data);
    if (entry.flushTimer) return;
    entry.flushTimer = setTimeout(() => {
      entry.flushTimer = null;
      this.flushOutput(entry);
    }, OUTPUT_FLUSH_MS);
  }

  private flushOutput(entry: LiveEntry) {
    if (entry.flushTimer) {
      clearTimeout(entry.flushTimer);
      entry.flushTimer = null;
    }
    if (entry.pendingOut.length === 0) return;
    const data =
      entry.pendingOut.length === 1 ? entry.pendingOut[0]! : entry.pendingOut.join("");
    entry.pendingOut = [];
    // 序列化一次，N 个 Viewer 共享同一个帧——不再是每人一份 JSON.stringify
    const frame = encodeTermFrame(TERM_FRAME_OUTPUT, data);
    for (const v of entry.viewers) {
      if (this.lagged.has(v)) {
        // 落后的 Viewer 不追增量（那正是它堆积的原因），排空后整体重放对齐
        if (v.drained()) this.sendReplay(entry, v);
        continue;
      }
      if (!v.sendBytes(frame)) this.lagged.add(v);
    }
  }

  private dropOutput(entry: LiveEntry) {
    if (entry.flushTimer) {
      clearTimeout(entry.flushTimer);
      entry.flushTimer = null;
    }
    entry.pendingOut = [];
  }

  private broadcast(entry: LiveEntry, msg: ServerMessage) {
    // 控制消息与输出保持时序：先把合并窗口里的输出冲出去
    this.flushOutput(entry);
    for (const v of entry.viewers) v.send(msg);
  }

  private sendReplay(entry: LiveEntry, viewer: Viewer) {
    // 前端对 replay 帧先 term.reset() 再写入，reset 会清掉全部 VT 模式；
    // 快照里往往已没有当初的模式序列（4MB 环挤掉了），这里用跟踪到的
    // 当前模式作前缀重建，否则重连后的 Viewer 永久丢失 mouse tracking
    // （滚轮失效）和 bracketed paste（多行粘贴被逐行执行）。
    const payload = entry.modes.prefix() + entry.buffer.snapshot();
    if (!viewer.sendBytes(encodeTermFrame(TERM_FRAME_REPLAY, payload))) {
      this.lagged.add(viewer);
    } else {
      this.lagged.delete(viewer);
    }
  }

  private markActive(entry: LiveEntry, opts?: { replay?: boolean }): SessionRow {
    this.db.updateSessionState(entry.sessionId, "active");
    if (opts?.replay) {
      this.flushOutput(entry);
      for (const v of entry.viewers) this.sendReplay(entry, v);
    }
    this.broadcast(entry, { type: "state", state: "active" });
    return this.db.getSession(entry.sessionId)!;
  }

  private markDead(entry: LiveEntry, reason: DeadReason) {
    entry.backend = null;
    // 先把还没广播的尾巴发出去（shell 的告别输出），再释放
    this.flushOutput(entry);
    // dead 会话不可能再回放（addViewer 对 dead 行早退），立刻释放
    // Scrollback——每个会话最多占 4MB 字节（V8 里最高 8MB 堆）
    entry.buffer.reset();
    this.lastTouch.delete(entry.sessionId);
    this.db.updateSessionState(entry.sessionId, "dead", reason);
    this.broadcast(entry, { type: "state", state: "dead", deadReason: reason });
  }

  private markUnverified(entry: LiveEntry) {
    entry.backend = null;
    this.db.updateSessionState(entry.sessionId, "unverified");
    this.broadcast(entry, { type: "state", state: "unverified" });
    if (!entry.terminating) this.kickAutoReattach(entry);
  }

  /** 标黄之后自己去接，不再等用户点。SSH 走链路重连，本地直接再 attach。 */
  private kickAutoReattach(entry: LiveEntry) {
    if (!entry.durable) return;
    const project = this.db.getProject(entry.projectId);
    if (!project) return;
    if (project.type === "ssh") this.scheduleReconnect(project.id);
    else this.scheduleLocalReattach(entry.sessionId);
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
    for (const entry of this.entries.values()) {
      if (entry.projectId !== projectId) continue;
      if (entry.terminating) continue;
      const row = this.db.getSession(entry.sessionId);
      if (!row || row.state === "dead") continue;
      if (entry.durable) {
        this.markUnverified(entry);
      } else {
        this.markDead(entry, "link-lost");
      }
    }
    this.forwards.onLinkDown(projectId);
    if (this.forwards.hasEnabled(projectId)) this.scheduleReconnect(projectId);
  }

  /**
   * SSH 断线自动重连：指数退避。
   * 有待接回的持久会话，或还有启用的端口转发，就坚持——不再要求有人正在看。
   */
  private scheduleReconnect(projectId: string) {
    const existing = this.reconnects.get(projectId);
    if (existing?.timer) return;

    const state: ReconnectState = existing ?? { attempt: 0, timer: null };
    this.reconnects.set(projectId, state);

    const targets = () =>
      [...this.entries.values()].filter(
        (e) =>
          e.projectId === projectId &&
          e.durable &&
          !e.backend &&
          !e.terminating &&
          this.db.getSession(e.sessionId)?.state === "unverified"
      );
    const wantsForward = () => this.forwards.hasEnabled(projectId);

    const tick = async () => {
      state.timer = null;
      const waiting = targets();
      if (waiting.length === 0 && !wantsForward()) {
        this.reconnects.delete(projectId);
        return;
      }
      state.attempt++;
      for (const e of waiting) {
        this.broadcast(e, { type: "reconnecting", attempt: state.attempt });
      }
      const project = this.db.getProject(projectId);
      if (!project) {
        this.reconnects.delete(projectId);
        return;
      }
      try {
        await this.getLink(project).getClient();
        for (const e of targets()) {
          // 有 Viewer 还没报格子：等它自己的 resize，别用库存尺寸抢跑
          if (e.viewers.size > 0 && (e.cols == null || e.rows == null)) continue;
          try {
            await this.ensureAttached(e.sessionId, {
              force: e.viewers.size === 0,
            });
          } catch {
            // 单个会话接回失败（如 session-gone），已在 ensureAttached 中标记
          }
        }
        const still = targets().filter(
          (e) => !(e.viewers.size > 0 && (e.cols == null || e.rows == null))
        );
        if (still.length === 0 && !wantsForward()) {
          this.reconnects.delete(projectId);
          return;
        }
      } catch {
        // 链路还没通，继续退避
      }
      const delay = Math.min(
        RECONNECT_MAX_DELAY,
        1000 * 2 ** Math.min(state.attempt - 1, 10)
      );
      state.timer = setTimeout(tick, delay);
    };

    state.timer = setTimeout(tick, 1000);
  }

  /** 本地持久会话的自动接回。PTY 掉了但 Zellij 还在时，不必等用户点。 */
  private scheduleLocalReattach(sessionId: string) {
    const existing = this.localReattach.get(sessionId);
    if (existing?.timer) return;

    const state: ReconnectState = existing ?? { attempt: 0, timer: null };
    this.localReattach.set(sessionId, state);

    const tick = async () => {
      state.timer = null;
      const entry = this.entries.get(sessionId);
      const row = this.db.getSession(sessionId);
      if (
        !entry ||
        !row ||
        row.state !== "unverified" ||
        entry.backend ||
        entry.terminating
      ) {
        this.localReattach.delete(sessionId);
        return;
      }
      // 有 Viewer 还没报格子：resize() 会触发 ensureAttached，这里空转没有意义
      if (entry.viewers.size > 0 && (entry.cols == null || entry.rows == null)) {
        this.localReattach.delete(sessionId);
        return;
      }
      state.attempt++;
      this.broadcast(entry, { type: "reconnecting", attempt: state.attempt });
      try {
        await this.ensureAttached(sessionId, { force: entry.viewers.size === 0 });
        this.localReattach.delete(sessionId);
      } catch {
        if (this.db.getSession(sessionId)?.state === "dead") {
          this.localReattach.delete(sessionId);
          return;
        }
        const delay = Math.min(
          RECONNECT_MAX_DELAY,
          1000 * 2 ** Math.min(state.attempt - 1, 10)
        );
        state.timer = setTimeout(tick, delay);
      }
    };

    state.timer = setTimeout(tick, 250);
  }

  /**
   * 500ms 合并窗口内最多落库一次，到点时取 entry 上的最新值，
   * 所以窗口内的后续变化不丢——只是推迟。进程退出最多丢 500ms 内
   * 的最后一次尺寸，对 cols/rows 这种数据完全可接受（unref 保证
   * 不阻塞退出）。
   */
  private scheduleSizePersist(sessionId: string) {
    if (this.sizeFlush.has(sessionId)) return;
    const timer = setTimeout(() => {
      this.sizeFlush.delete(sessionId);
      const entry = this.entries.get(sessionId);
      if (entry && entry.cols != null && entry.rows != null) {
        this.db.updateSessionSize(sessionId, entry.cols, entry.rows);
      }
    }, 500);
    timer.unref?.();
    this.sizeFlush.set(sessionId, timer);
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
