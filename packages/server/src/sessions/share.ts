/**
 * 公网发布运行时。规则在 DB，活着的 cloudflared 进程在内存。
 *
 * cloudflared 只在 falcon 后端本机跑。远端目标先在本机听一个临时端口、经
 * SSH forwardOut 打到远端，再把 Quick Tunnel 指到这个临时端口——远端宿主机
 * 上既没有 cloudflared，也不需要出网到 Cloudflare。
 *
 * URL 是 Quick Tunnel 的随机子域，不入库：进程一重启就会变，存下来只会骗人。
 */

import crypto from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import type { Duplex } from "node:stream";
import type { ForwardState, PublicShare, PublicShareInput, ShareOrigin } from "@falcon/shared";
import { CloudflaredBinError, ensureCloudflared } from "../cloudflared/bin.js";
import {
  extractMetricsAddr,
  extractQuickTunnelUrl,
  httpHostHeader,
  lastLogLines,
  originUrl,
  parseQuickTunnelMetrics,
  tunnelArgs,
} from "../cloudflared/command.js";
import type { Db, ProjectRow, PublicShareRow } from "../db.js";
import { resolveLocalBaseEnv } from "./loginEnv.js";
import { validateShareInput } from "./shareSpec.js";
import type { SshLink } from "./ssh.js";

const URL_WAIT_MS = 45_000;
const METRICS_POLL_MS = 300;
const STOP_GRACE_MS = 2_000;

interface LiveShare {
  id: string;
  projectId: string;
  origin: ShareOrigin;
  destHost: string;
  destPort: number;
  child?: ChildProcess;
  /** 远端目标的本机桥：listen(0) → SSH forwardOut */
  bridge?: net.Server;
  publicUrl?: string;
  /** 我们主动杀的，exit 时不要写成 error */
  stopping?: boolean;
}

export class ShareConflictError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ShareConflictError";
  }
}

export class ShareManager {
  private live = new Map<string, LiveShare>();
  private starting = new Map<string, Promise<void>>();
  private errors = new Map<string, string>();
  private urls = new Map<string, string>();

  constructor(
    private db: Db,
    private dataDir: string,
    private linkFor: (projectId: string) => SshLink | null
  ) {}

  list(projectId: string): PublicShare[] {
    return this.db.listShares(projectId).map((row) => this.toShare(row));
  }

  get(id: string): PublicShare | undefined {
    const row = this.db.getShare(id);
    return row ? this.toShare(row) : undefined;
  }

  hasRemoteEnabled(projectId: string): boolean {
    return this.db.listShares(projectId).some((r) => r.enabled === 1 && r.origin === "remote");
  }

  async create(project: ProjectRow, input: PublicShareInput): Promise<PublicShare> {
    const parsed = validateShareInput(input, project.type);
    if (!parsed.ok) throw new Error(parsed.error);
    this.assertDestFree(project.id, parsed.value, undefined);

    const row: PublicShareRow = {
      id: crypto.randomUUID(),
      project_id: project.id,
      name: parsed.value.name ?? null,
      origin: parsed.value.origin,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
      created_at: Date.now(),
    };
    this.db.insertShare(row);
    // 第一次会下载 ~20MB 的 cloudflared，await 会让 POST 卡一两分钟，
    // 面板以为表单死了。丢到后台，列表立刻是 starting，轮询接到 URL。
    if (row.enabled === 1) void this.start(row.id).catch(() => {});
    return this.toShare(this.db.getShare(row.id)!);
  }

  async update(
    project: ProjectRow,
    id: string,
    patch: Partial<PublicShareInput>
  ): Promise<PublicShare> {
    const existing = this.db.getShare(id);
    if (!existing || existing.project_id !== project.id) throw new Error("发布规则不存在");
    const parsed = validateShareInput(
      {
        name: patch.name !== undefined ? patch.name : (existing.name ?? undefined),
        origin: patch.origin ?? (existing.origin as ShareOrigin),
        destHost: patch.destHost ?? existing.dest_host,
        destPort: patch.destPort ?? existing.dest_port,
        enabled: patch.enabled ?? existing.enabled === 1,
      },
      project.type
    );
    if (!parsed.ok) throw new Error(parsed.error);
    this.assertDestFree(project.id, parsed.value, id);

    const next: PublicShareRow = {
      ...existing,
      name: parsed.value.name ?? null,
      origin: parsed.value.origin,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
    };
    this.db.updateShare(next);

    await this.stop(id);
    if (next.enabled === 1) void this.start(id).catch(() => {});
    return this.toShare(this.db.getShare(id)!);
  }

  async remove(project: ProjectRow, id: string): Promise<void> {
    const existing = this.db.getShare(id);
    if (!existing || existing.project_id !== project.id) throw new Error("发布规则不存在");
    await this.stop(id);
    this.db.deleteShare(id);
    this.errors.delete(id);
    this.urls.delete(id);
  }

  async start(id: string): Promise<void> {
    if (this.live.has(id)) return;
    const inflight = this.starting.get(id);
    if (inflight) return inflight;
    const run = this.startNow(id).finally(() => {
      this.starting.delete(id);
    });
    this.starting.set(id, run);
    return run;
  }

  async stop(id: string): Promise<void> {
    const inflight = this.starting.get(id);
    if (inflight) await inflight.catch(() => {});
    const live = this.live.get(id);
    if (live) {
      await this.teardown(live);
      this.live.delete(id);
    }
    this.urls.delete(id);
  }

  stopAll(projectId: string) {
    for (const live of [...this.live.values()]) {
      if (live.projectId !== projectId) continue;
      void this.teardown(live);
      this.live.delete(live.id);
      this.urls.delete(live.id);
    }
    for (const [id, run] of this.starting) {
      if (this.db.getShare(id)?.project_id === projectId) {
        void run.catch(() => {});
        this.starting.delete(id);
      }
    }
  }

  onLinkDown(projectId: string) {
    const remote = [...this.live.values()].filter(
      (l) => l.projectId === projectId && l.origin === "remote"
    );
    for (const live of remote) {
      void this.teardown(live);
      this.live.delete(live.id);
      this.urls.delete(live.id);
    }
    for (const row of this.db.listShares(projectId)) {
      if (row.enabled === 1 && row.origin === "remote") {
        this.errors.set(row.id, "SSH 链路断开");
      }
    }
  }

  async onLinkUp(projectId: string) {
    for (const row of this.db.listShares(projectId)) {
      if (row.enabled === 1 && row.origin === "remote") void this.start(row.id);
    }
  }

  async shutdown(): Promise<void> {
    const lives = [...this.live.values()];
    this.live.clear();
    this.urls.clear();
    this.starting.clear();
    await Promise.all(lives.map((live) => this.teardown(live)));
  }

  async restoreEnabled(): Promise<void> {
    for (const projectId of this.db.listEnabledShareProjectIds()) {
      const project = this.db.getProject(projectId);
      if (!project) continue;
      for (const row of this.db.listShares(projectId)) {
        if (row.enabled !== 1) continue;
        if (row.origin === "remote") {
          const link = this.linkFor(projectId);
          if (!link) {
            this.errors.set(row.id, "项目不存在");
            continue;
          }
          try {
            await link.getClient();
          } catch (err) {
            this.errors.set(row.id, (err as Error).message);
            continue;
          }
        }
        void this.start(row.id);
      }
    }
  }

  private async startNow(id: string): Promise<void> {
    const row = this.db.getShare(id);
    if (!row || row.enabled !== 1) return;
    if (this.live.has(id)) return;

    this.errors.delete(id);
    this.urls.delete(id);

    const live: LiveShare = {
      id: row.id,
      projectId: row.project_id,
      origin: row.origin as ShareOrigin,
      destHost: row.dest_host,
      destPort: row.dest_port,
    };

    try {
      const bin = await ensureCloudflared(this.dataDir);
      let tunnelHost = row.dest_host;
      let tunnelPort = row.dest_port;
      if (row.origin === "remote") {
        const link = this.linkFor(row.project_id);
        if (!link) throw new Error("项目不存在");
        live.bridge = await this.listenBridge(link, live);
        const addr = live.bridge.address();
        if (typeof addr !== "object" || !addr) throw new Error("本机桥没有绑到端口");
        tunnelHost = "127.0.0.1";
        tunnelPort = addr.port;
      }

      const metricsPort = await freePort();
      const env = {
        ...(await resolveLocalBaseEnv()),
        NO_AUTOUPDATE: "true",
      };
      const args = tunnelArgs({
        originUrl: originUrl(tunnelHost, tunnelPort),
        // Host 按真正的 origin 写，不是本机桥的临时端口——vite 认的是它自己的端口
        httpHostHeader: httpHostHeader(row.dest_host, row.dest_port),
        metrics: `127.0.0.1:${metricsPort}`,
      });
      const child = spawn(bin, args, {
        env,
        windowsHide: true,
        stdio: ["ignore", "pipe", "pipe"],
      });
      live.child = child;
      this.live.set(id, live);

      const url = await waitForUrl(child, metricsPort, () => live.stopping === true);
      if (live.stopping) return;
      live.publicUrl = url;
      this.urls.set(id, url);

      child.once("exit", (code, signal) => {
        if (live.stopping) return;
        if (this.live.get(id) !== live) return;
        this.live.delete(id);
        this.urls.delete(id);
        this.errors.set(
          id,
          `cloudflared 已退出${code != null ? `（${code}）` : signal ? `（${signal}）` : ""}`
        );
        if (live.bridge) void closeServer(live.bridge);
      });
    } catch (err) {
      await this.teardown(live);
      this.live.delete(id);
      this.urls.delete(id);
      this.errors.set(id, err instanceof CloudflaredBinError ? err.message : (err as Error).message);
      throw err;
    }
  }

  private listenBridge(link: SshLink, live: LiveShare): Promise<net.Server> {
    return new Promise((resolve, reject) => {
      const server = net.createServer((socket) => {
        void this.openBridge(link, live, socket);
      });
      server.on("error", reject);
      server.listen(0, "127.0.0.1", () => {
        server.off("error", reject);
        server.on("error", (err) => {
          this.errors.set(live.id, err.message);
          this.live.delete(live.id);
          this.urls.delete(live.id);
          server.close();
        });
        resolve(server);
      });
    });
  }

  private async openBridge(link: SshLink, live: LiveShare, socket: net.Socket) {
    try {
      const client = await link.getClient();
      client.forwardOut(
        socket.remoteAddress ?? "127.0.0.1",
        socket.remotePort ?? 0,
        live.destHost,
        live.destPort,
        (err, stream) => {
          if (err || !stream) {
            socket.destroy();
            return;
          }
          pipeSockets(socket, stream);
        }
      );
    } catch {
      socket.destroy();
    }
  }

  private async teardown(live: LiveShare): Promise<void> {
    live.stopping = true;
    if (live.child && live.child.exitCode == null && live.child.signalCode == null) {
      const child = live.child;
      await killChild(child);
    }
    live.child = undefined;
    if (live.bridge) {
      await closeServer(live.bridge);
      live.bridge = undefined;
    }
  }

  private assertDestFree(
    projectId: string,
    value: { origin: ShareOrigin; destHost: string; destPort: number },
    exceptId: string | undefined
  ) {
    if (this.db.findShareDest(projectId, value.origin, value.destHost, value.destPort, exceptId)) {
      throw new ShareConflictError(
        `该项目已发布 ${value.origin === "local" ? "本机" : "远端"} ${value.destHost}:${value.destPort}`
      );
    }
  }

  private toShare(row: PublicShareRow): PublicShare {
    const enabled = row.enabled === 1;
    let state: ForwardState = "stopped";
    if (enabled && this.live.has(row.id) && this.urls.has(row.id)) state = "active";
    else if (enabled && (this.starting.has(row.id) || this.live.has(row.id))) state = "starting";
    else if (enabled && this.errors.has(row.id)) state = "error";
    else if (enabled) state = "stopped";
    return {
      id: row.id,
      projectId: row.project_id,
      name: row.name ?? undefined,
      origin: row.origin as ShareOrigin,
      destHost: row.dest_host,
      destPort: row.dest_port,
      enabled,
      state,
      publicUrl: this.urls.get(row.id),
      error: this.errors.get(row.id),
      createdAt: row.created_at,
    };
  }
}

function pipeSockets(a: Duplex, b: Duplex) {
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    a.unpipe(b);
    b.unpipe(a);
    a.destroy();
    b.destroy();
  };
  a.pipe(b);
  b.pipe(a);
  a.on("error", close);
  b.on("error", close);
  a.on("close", close);
  b.on("close", close);
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      server.close((err) => (err ? reject(err) : resolve(port)));
    });
  });
}

function closeServer(server: net.Server): Promise<void> {
  return new Promise((resolve) => server.close(() => resolve()));
}

function killChild(child: ChildProcess): Promise<void> {
  return new Promise((resolve) => {
    if (child.exitCode != null || child.signalCode != null) return resolve();
    const timer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch {
        // 已经没了
      }
    }, STOP_GRACE_MS);
    child.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
    try {
      child.kill("SIGTERM");
    } catch {
      clearTimeout(timer);
      resolve();
    }
  });
}

/**
 * 等公网 URL。日志解析与 /quicktunnel 轮询并行：有的版本框打得晚、metrics 先好，
 * 反过来也有。进程先退出就立刻失败，别空等到超时。
 */
function waitForUrl(
  child: ChildProcess,
  metricsPort: number,
  isStopping: () => boolean
): Promise<string> {
  return new Promise((resolve, reject) => {
    let log = "";
    let settled = false;
    let pollTimer: NodeJS.Timeout | null = null;
    let timeout: NodeJS.Timeout | null = null;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      if (pollTimer) clearTimeout(pollTimer);
      if (timeout) clearTimeout(timeout);
      child.stdout?.off("data", onData);
      child.stderr?.off("data", onData);
      child.off("exit", onExit);
      child.off("error", onError);
      fn();
    };
    const onData = (d: Buffer) => {
      log += d.toString("utf8");
      const url = extractQuickTunnelUrl(log);
      if (url) finish(() => resolve(url));
    };
    const onExit = (code: number | null) => {
      if (isStopping()) {
        finish(() => reject(new Error("已取消")));
        return;
      }
      const detail = lastLogLines(log);
      finish(() =>
        reject(new Error(detail ? `cloudflared 退出：${detail}` : `cloudflared 退出（${code ?? "?"}）`))
      );
    };
    const onError = (err: Error) => {
      finish(() => reject(err));
    };
    const poll = async () => {
      if (settled) return;
      const fromLog = extractMetricsAddr(log);
      const addr = fromLog ?? `127.0.0.1:${metricsPort}`;
      try {
        const res = await fetch(`http://${addr}/quicktunnel`, { signal: AbortSignal.timeout(800) });
        if (res.ok) {
          const url = parseQuickTunnelMetrics(await res.text());
          if (url) {
            finish(() => resolve(url));
            return;
          }
        }
      } catch {
        // metrics 还没起来
      }
      if (!settled) pollTimer = setTimeout(() => void poll(), METRICS_POLL_MS);
    };
    timeout = setTimeout(() => {
      const detail = lastLogLines(log);
      finish(() =>
        reject(new Error(detail ? `等不到公网地址：${detail}` : "等不到公网地址（Quick Tunnel 超时）"))
      );
      void killChild(child);
    }, URL_WAIT_MS);

    child.stdout?.on("data", onData);
    child.stderr?.on("data", onData);
    child.once("exit", onExit);
    child.once("error", onError);
    pollTimer = setTimeout(() => void poll(), METRICS_POLL_MS);
  });
}
