/**
 * 公网发布运行时，挂在本机或 SSH Host 上（ADR 0016）。规则在 DB，活着的
 * cloudflared 进程在内存。
 *
 * cloudflared 只在 falcon 后端本机跑。远端目标先在本机听一个临时端口、经
 * SSH forwardOut 打到远端，再把 Quick Tunnel 指到这个临时端口——远端宿主机
 * 上既没有 cloudflared，也不需要出网到 Cloudflare。桥走主机自己的那条
 * SshLink（SessionManager.getHostLink），与端口转发共用。
 *
 * URL 是 Quick Tunnel 的随机子域，不入库：进程一重启就会变，存下来只会骗人。
 *
 * 同一台机器上发同一个端口的规则可以有多条，同时只有一条 enabled，
 * 做法与 ForwardManager 相同（见 relaySpec.shareSlot）。
 */

import crypto from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import type { Duplex } from "node:stream";
import type { ForwardState, PublicShare, PublicShareInput } from "@falcon/shared";
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
import type { Db, HostShareRow } from "../db.js";
import { resolveLocalBaseEnv } from "./loginEnv.js";
import { displacedBy, shareSlot } from "./relaySpec.js";
import { validateShareInput } from "./shareSpec.js";
import type { SshLink } from "./ssh.js";

const URL_WAIT_MS = 45_000;
const METRICS_POLL_MS = 300;
const STOP_GRACE_MS = 2_000;

interface LiveShare {
  id: string;
  /** null = 本机 */
  hostId: string | null;
  destHost: string;
  destPort: number;
  child?: ChildProcess;
  /** 远端目标的本机桥：listen(0) → SSH forwardOut */
  bridge?: net.Server;
  publicUrl?: string;
  /** 我们主动杀的，exit 时不要写成 error */
  stopping?: boolean;
}

/** 规则或主机不存在。路由据此回 404 */
export class ShareNotFoundError extends Error {
  constructor(message = "发布规则不存在") {
    super(message);
    this.name = "ShareNotFoundError";
  }
}

export class ShareManager {
  private live = new Map<string, LiveShare>();
  private starting = new Map<string, Promise<void>>();
  private errors = new Map<string, string>();
  private urls = new Map<string, string>();
  /** stop() 进行中的规则：startNow 在 spawn 前看一眼，别再起一个马上要杀的进程 */
  private cancelled = new Set<string>();

  constructor(
    private db: Db,
    private dataDir: string,
    private linkFor: (hostId: string) => SshLink | null,
    /** 远端目标连不上主机时请 SessionManager 退避重连 */
    private wantReconnect: (hostId: string) => void
  ) {}

  list(): PublicShare[] {
    return this.db.listShares().map((row) => this.toShare(row));
  }

  /** 该主机上有没有要靠 SSH 链路维持的发布（本机的不算） */
  hasEnabled(hostId: string): boolean {
    return this.db.listShares().some((r) => r.host_id === hostId && r.enabled === 1);
  }

  enabledHostIds(): string[] {
    const ids = this.db
      .listShares()
      .filter((r) => r.enabled === 1 && r.host_id != null)
      .map((r) => r.host_id!);
    return [...new Set(ids)];
  }

  async create(input: PublicShareInput): Promise<PublicShare> {
    let hostId: string | null = null;
    if (input?.hostId != null && input.hostId !== "") {
      if (typeof input.hostId !== "string" || !this.db.getHost(input.hostId)) {
        throw new ShareNotFoundError("主机不存在");
      }
      hostId = input.hostId;
    }
    const parsed = validateShareInput(input);
    if (!parsed.ok) throw new Error(parsed.error);

    const row: HostShareRow = {
      id: crypto.randomUUID(),
      host_id: hostId,
      name: parsed.value.name ?? null,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
      created_at: Date.now(),
    };
    const displaced = row.enabled === 1 ? this.displace(row) : [];
    this.db.insertShare(row);
    // 第一次会下载 ~20MB 的 cloudflared，await 会让 POST 卡一两分钟，
    // 面板以为表单死了。丢到后台，列表立刻是 starting，轮询接到 URL。
    void this.stopMany(displaced).then(() => {
      if (row.enabled === 1) return this.start(row.id).catch(() => {});
    });
    return this.toShare(this.db.getShare(row.id)!);
  }

  async update(id: string, patch: Partial<PublicShareInput>): Promise<PublicShare> {
    const existing = this.db.getShare(id);
    if (!existing) throw new ShareNotFoundError();
    const parsed = validateShareInput({
      name: patch.name !== undefined ? patch.name : (existing.name ?? undefined),
      destHost: patch.destHost ?? existing.dest_host,
      destPort: patch.destPort ?? existing.dest_port,
      enabled: patch.enabled ?? existing.enabled === 1,
    });
    if (!parsed.ok) throw new Error(parsed.error);

    const next: HostShareRow = {
      ...existing,
      name: parsed.value.name ?? null,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
    };
    const displaced = next.enabled === 1 ? this.displace(next) : [];
    this.db.updateShare(next);

    await this.stop(id);
    // 与新建同理丢到后台：起的时候可能要下载 cloudflared、连主机链路
    if (next.enabled === 1) {
      void this.stopMany(displaced)
        .then(() => this.start(id))
        .catch(() => {});
    }
    return this.toShare(this.db.getShare(id)!);
  }

  async remove(id: string): Promise<void> {
    if (!this.db.getShare(id)) throw new ShareNotFoundError();
    await this.stop(id);
    this.db.deleteShare(id);
    this.errors.delete(id);
    this.urls.delete(id);
  }

  /** 删主机前调用：停掉该主机的全部发布。规则由 Db.deleteRelaysOfHost 删 */
  async forgetHost(hostId: string): Promise<void> {
    const ids = this.db.listShares().filter((r) => r.host_id === hostId).map((r) => r.id);
    await this.stopMany(ids);
    for (const id of ids) this.errors.delete(id);
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
    this.cancelled.add(id);
    try {
      // 已经 spawn 了就先杀：waitForUrl 见进程退出立刻放弃，不必干等最长 45s 的 URL
      const live = this.live.get(id);
      if (live) {
        this.live.delete(id);
        await this.teardown(live);
      }
      // 下载 cloudflared / 连 SSH 的那段打断不了，等它走到 spawn 前的检查点自己退出
      const inflight = this.starting.get(id);
      if (inflight) await inflight.catch(() => {});
      const late = this.live.get(id);
      if (late) {
        this.live.delete(id);
        await this.teardown(late);
      }
      this.urls.delete(id);
    } finally {
      this.cancelled.delete(id);
    }
  }

  /** 链路要被换掉（改了凭据 / 删主机）：同步拆掉该主机的全部发布，不写错误 */
  stopAll(hostId: string) {
    for (const live of [...this.live.values()]) {
      if (live.hostId !== hostId) continue;
      this.live.delete(live.id);
      this.urls.delete(live.id);
      void this.teardown(live);
    }
    for (const [id, run] of this.starting) {
      if (this.db.getShare(id)?.host_id === hostId) {
        void run.catch(() => {});
        this.starting.delete(id);
      }
    }
  }

  onLinkDown(hostId: string) {
    this.stopAll(hostId);
    for (const row of this.db.listShares()) {
      if (row.host_id === hostId && row.enabled === 1) this.errors.set(row.id, "SSH 链路断开");
    }
  }

  onLinkUp(hostId: string) {
    for (const row of this.db.listShares()) {
      if (row.host_id === hostId && row.enabled === 1) void this.start(row.id).catch(() => {});
    }
  }

  markUnreachable(hostId: string, message: string) {
    for (const row of this.db.listShares()) {
      if (row.host_id === hostId && row.enabled === 1 && !this.live.has(row.id)) {
        this.errors.set(row.id, message);
      }
    }
  }

  async shutdown(): Promise<void> {
    const lives = [...this.live.values()];
    this.live.clear();
    this.urls.clear();
    this.starting.clear();
    await Promise.all(lives.map((live) => this.teardown(live)));
  }

  /** 启动时拉起本机的发布。远端的等主机链路连上（"up"）时由 onLinkUp 拉 */
  restoreLocal() {
    for (const row of this.db.listShares()) {
      if (row.host_id == null && row.enabled === 1) void this.start(row.id).catch(() => {});
    }
  }

  private displace(target: HostShareRow): string[] {
    const ids = displacedBy(this.db.listShares(), target, shareSlot);
    for (const id of ids) {
      this.db.setShareEnabled(id, false);
      this.errors.delete(id);
    }
    return ids;
  }

  private async stopMany(ids: string[]) {
    await Promise.all(ids.map((id) => this.stop(id)));
  }

  private async startNow(id: string): Promise<void> {
    const row = this.db.getShare(id);
    if (!row || row.enabled !== 1) return;
    if (this.live.has(id)) return;

    this.errors.delete(id);
    this.urls.delete(id);

    const live: LiveShare = {
      id: row.id,
      hostId: row.host_id,
      destHost: row.dest_host,
      destPort: row.dest_port,
    };

    try {
      const bin = await ensureCloudflared(this.dataDir);
      let tunnelHost = row.dest_host;
      let tunnelPort = row.dest_port;
      if (row.host_id != null) {
        const link = this.linkFor(row.host_id);
        if (!link) throw new Error("主机不存在");
        // 先连上再开桥：桥的监听器本身不碰 SSH，不先连的话主机连不上也能拿到
        // 一条公网 URL，点开才是 502
        try {
          await link.getClient();
        } catch (err) {
          this.wantReconnect(row.host_id);
          throw err;
        }
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
      if (this.cancelled.has(id)) {
        live.stopping = true;
        await this.teardown(live);
        return;
      }
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
      const cancelled = live.stopping === true;
      await this.teardown(live);
      if (this.live.get(id) === live) this.live.delete(id);
      this.urls.delete(id);
      // 我们自己杀的（停用 / 让位 / 重启）不是故障
      if (cancelled) return;
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

  private toShare(row: HostShareRow): PublicShare {
    const enabled = row.enabled === 1;
    let state: ForwardState = "stopped";
    if (enabled && this.live.has(row.id) && this.urls.has(row.id)) state = "active";
    else if (enabled && (this.starting.has(row.id) || this.live.has(row.id))) state = "starting";
    else if (enabled && this.errors.has(row.id)) state = "error";
    return {
      id: row.id,
      hostId: row.host_id ?? undefined,
      name: row.name ?? undefined,
      destHost: row.dest_host,
      destPort: row.dest_port,
      enabled,
      state,
      publicUrl: enabled ? this.urls.get(row.id) : undefined,
      error: enabled ? this.errors.get(row.id) : undefined,
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
