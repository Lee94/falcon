import crypto from "node:crypto";
import net from "node:net";
import type { Duplex } from "node:stream";
import type { ForwardKind, ForwardState, PortForward, PortForwardInput } from "@falcon/shared";
import type { Db, HostForwardRow } from "../db.js";
import { validateForwardInput } from "./forwardSpec.js";
import { displacedBy, forwardSlot } from "./relaySpec.js";
import type { SshLink } from "./ssh.js";

interface LiveForward {
  id: string;
  hostId: string;
  kind: ForwardKind;
  bindHost: string;
  bindPort: number;
  destHost: string;
  destPort: number;
  /** local 转发的本机监听器 */
  server?: net.Server;
}

/**
 * 一次启动尝试。gen 是发起时该规则的代数，stop() 让代数 +1 就等于作废它；
 * dialing = 还在等主机链路连上（最长是 SSH 握手超时），这段不值得等。
 */
interface Attempt {
  gen: number;
  dialing: boolean;
  done: Promise<void>;
}

/** 规则或主机不存在。路由据此回 404 */
export class ForwardNotFoundError extends Error {
  constructor(message = "转发规则不存在") {
    super(message);
    this.name = "ForwardNotFoundError";
  }
}

/**
 * 端口转发运行时，按 SSH Host 挂（ADR 0016）。规则在 DB，活着的隧道在内存。
 *
 * 隧道走主机自己的那条 SshLink（SessionManager.getHostLink），不是哪个项目的：
 * 同一台机器上开着几个项目，转发也只有一份。
 *
 * 链路断开只拆隧道、不删规则：重连成功后（link 的 "up"）按 enabled 再拉起来。
 * 本地监听器也跟着拆——断链期间占着端口会让人以为还通着。
 *
 * 同端口的规则可以有多条，同时只有一条 enabled：启用一条之前先把同槽位
 * （relaySpec.forwardSlot）的其它规则落库成 disabled 并停掉，再起这一条——
 * 顺序反了新监听器会撞上旧的还没释放的端口。
 */
export class ForwardManager {
  private live = new Map<string, LiveForward>();
  private attempts = new Map<string, Attempt>();
  private gens = new Map<string, number>();
  private errors = new Map<string, string>();

  constructor(
    private db: Db,
    private linkFor: (hostId: string) => SshLink | null,
    /** 连不上主机时请 SessionManager 退避重连；连上后它的 "up" 会回调 onLinkUp */
    private wantReconnect: (hostId: string) => void
  ) {}

  list(): PortForward[] {
    return this.db.listForwards().map((row) => this.toPortForward(row));
  }

  hasEnabled(hostId: string): boolean {
    return this.db.listForwards().some((r) => r.host_id === hostId && r.enabled === 1);
  }

  enabledHostIds(): string[] {
    return [...new Set(this.db.listForwards().filter((r) => r.enabled === 1).map((r) => r.host_id))];
  }

  async create(input: PortForwardInput): Promise<PortForward> {
    const hostId = typeof input?.hostId === "string" ? input.hostId : "";
    if (!hostId || !this.db.getHost(hostId)) throw new ForwardNotFoundError("主机不存在");
    const parsed = validateForwardInput(input);
    if (!parsed.ok) throw new Error(parsed.error);

    const row: HostForwardRow = {
      id: crypto.randomUUID(),
      host_id: hostId,
      name: parsed.value.name ?? null,
      kind: parsed.value.kind,
      bind_host: parsed.value.bindHost,
      bind_port: parsed.value.bindPort,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
      created_at: Date.now(),
    };
    const displaced = row.enabled === 1 ? this.displace(row) : [];
    this.db.insertForward(row);
    if (row.enabled === 1) this.startAfter(displaced, row.id);
    return this.toPortForward(this.db.getForward(row.id)!);
  }

  async update(id: string, patch: Partial<PortForwardInput>): Promise<PortForward> {
    const existing = this.db.getForward(id);
    if (!existing) throw new ForwardNotFoundError();
    const parsed = validateForwardInput({
      name: patch.name !== undefined ? patch.name : (existing.name ?? undefined),
      kind: patch.kind ?? (existing.kind as ForwardKind),
      bindHost: patch.bindHost ?? existing.bind_host,
      bindPort: patch.bindPort ?? existing.bind_port,
      destHost: patch.destHost ?? existing.dest_host,
      destPort: patch.destPort ?? existing.dest_port,
      enabled: patch.enabled ?? existing.enabled === 1,
    });
    if (!parsed.ok) throw new Error(parsed.error);

    const next: HostForwardRow = {
      ...existing,
      name: parsed.value.name ?? null,
      kind: parsed.value.kind,
      bind_host: parsed.value.bindHost,
      bind_port: parsed.value.bindPort,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
    };
    // 先同步落库（让位的那几条 + 自己），再做异步的停与起：两个并发请求各自启用
    // 同端口的两条时，库里任何时刻都只有一条 enabled，后到的赢
    const displaced = next.enabled === 1 ? this.displace(next) : [];
    this.db.updateForward(next);

    await this.stop(id);
    if (next.enabled === 1) this.startAfter(displaced, id);
    return this.toPortForward(this.db.getForward(id)!);
  }

  async remove(id: string): Promise<void> {
    if (!this.db.getForward(id)) throw new ForwardNotFoundError();
    await this.stop(id);
    this.db.deleteForward(id);
    this.errors.delete(id);
  }

  /** 删主机前调用：停掉该主机的全部隧道。规则由 Db.deleteRelaysOfHost 删 */
  async forgetHost(hostId: string): Promise<void> {
    const ids = this.db.listForwards().filter((r) => r.host_id === hostId).map((r) => r.id);
    await this.stopMany(ids);
    for (const id of ids) this.errors.delete(id);
  }

  async start(id: string): Promise<void> {
    if (this.live.has(id)) return;
    const inflight = this.attempts.get(id);
    if (inflight) return inflight.done;
    const attempt: Attempt = { gen: this.gen(id), dialing: false, done: Promise.resolve() };
    attempt.done = this.startNow(id, attempt).finally(() => {
      if (this.attempts.get(id) === attempt) this.attempts.delete(id);
    });
    this.attempts.set(id, attempt);
    return attempt.done;
  }

  /**
   * 作废在途的尝试并拆掉活着的隧道。还在连主机的尝试不等：它连上后见代数变了
   * 自己退出、不会再开监听——同端口从连不上的 A 机切到 B 机时，B 不必陪着等满
   * A 的握手超时。已经连上、正在开监听的那段很短，等它开完再拆，端口才交接得干净。
   */
  async stop(id: string): Promise<void> {
    this.gens.set(id, this.gen(id) + 1);
    const attempt = this.attempts.get(id);
    if (attempt) {
      if (attempt.dialing) this.attempts.delete(id);
      else await attempt.done.catch(() => {});
    }
    const live = this.live.get(id);
    if (live) {
      this.live.delete(id);
      await this.teardown(live);
    }
  }

  /** 链路断了 / 要被换掉：同步拆掉该主机的全部隧道并作废在途尝试，不写错误 */
  stopAll(hostId: string) {
    for (const live of [...this.live.values()]) {
      if (live.hostId !== hostId) continue;
      this.live.delete(live.id);
      void this.teardown(live);
    }
    for (const id of [...this.attempts.keys()]) {
      if (this.db.getForward(id)?.host_id !== hostId) continue;
      this.gens.set(id, this.gen(id) + 1);
      this.attempts.delete(id);
    }
  }

  private gen(id: string): number {
    return this.gens.get(id) ?? 0;
  }

  onLinkDown(hostId: string) {
    this.stopAll(hostId);
    for (const row of this.db.listForwards()) {
      if (row.host_id === hostId && row.enabled === 1) this.errors.set(row.id, "SSH 链路断开");
    }
  }

  onLinkUp(hostId: string) {
    for (const row of this.db.listForwards()) {
      if (row.host_id === hostId && row.enabled === 1) void this.start(row.id).catch(() => {});
    }
  }

  /** 主机连不上时（启动恢复 / 退避重连中）把原因挂到每条 enabled 规则上 */
  markUnreachable(hostId: string, message: string) {
    for (const row of this.db.listForwards()) {
      if (row.host_id === hostId && row.enabled === 1 && !this.live.has(row.id)) {
        this.errors.set(row.id, message);
      }
    }
  }

  /**
   * 把同槽位的其它 enabled 规则落库成 disabled，返回它们的 id 供调用方停掉。
   * 只写库、不 await：调用方紧接着写自己那一行，两步之间不让出事件循环。
   */
  private displace(target: HostForwardRow): string[] {
    const ids = displacedBy(this.db.listForwards(), target, forwardSlot);
    for (const id of ids) {
      this.db.setForwardEnabled(id, false);
      this.errors.delete(id);
    }
    return ids;
  }

  private async stopMany(ids: string[]) {
    await Promise.all(ids.map((id) => this.stop(id)));
  }

  /**
   * 停掉让位的规则之后再起这一条，丢到后台：起的时候要先把主机链路连上，主机不通
   * 就要等满 SSH 握手超时（15s），await 的话添加按钮 / 勾选框会一直转。响应里是
   * starting，轮询接到结果。停必须排在起之前，新监听器才不会撞上旧的端口。
   */
  private startAfter(displaced: string[], id: string) {
    const run = displaced.length === 0 ? this.start(id) : this.stopMany(displaced).then(() => this.start(id));
    void run.catch(() => {});
  }

  private async startNow(id: string, attempt: Attempt): Promise<void> {
    const stale = () => this.gen(id) !== attempt.gen;
    const row = this.db.getForward(id);
    if (!row || row.enabled !== 1) return;
    if (this.live.has(id)) return;

    const link = this.linkFor(row.host_id);
    if (!link) {
      this.errors.set(id, "主机不存在");
      return;
    }

    this.errors.delete(id);
    try {
      // 先把链路连上再开监听：本地转发的监听器本身不碰 SSH，不先连的话主机明明
      // 连不上，规则却显示「运行中」，要等第一条 TCP 连进来才露馅
      attempt.dialing = true;
      try {
        await link.getClient();
      } catch (err) {
        if (stale()) return;
        this.wantReconnect(row.host_id);
        throw err;
      } finally {
        attempt.dialing = false;
      }
      // 连接期间规则可能已被停用 / 让位 / 删掉 / 重启
      if (stale()) return;
      const fresh = this.db.getForward(id);
      if (!fresh || fresh.enabled !== 1) return;
      const live: LiveForward = {
        id: fresh.id,
        hostId: fresh.host_id,
        kind: fresh.kind as ForwardKind,
        bindHost: fresh.bind_host,
        bindPort: fresh.bind_port,
        destHost: fresh.dest_host,
        destPort: fresh.dest_port,
      };
      if (live.kind === "local") {
        live.server = await this.listenLocal(link, live);
      } else {
        await this.listenRemote(link, live);
      }
      // stopAll 不等在途尝试（它是同步的），开监听这段里被作废了就自己拆掉
      if (stale()) {
        await this.teardown(live);
        return;
      }
      this.live.set(id, live);
    } catch (err) {
      if (stale()) return;
      this.errors.set(id, (err as Error).message);
      throw err;
    }
  }

  private listenLocal(link: SshLink, live: LiveForward): Promise<net.Server> {
    return new Promise((resolve, reject) => {
      const server = net.createServer((socket) => {
        void this.openLocal(link, live, socket);
      });
      server.on("error", reject);
      server.listen(live.bindPort, live.bindHost, () => {
        server.off("error", reject);
        server.on("error", (err) => {
          this.errors.set(live.id, err.message);
          this.live.delete(live.id);
          server.close();
        });
        resolve(server);
      });
    });
  }

  private async openLocal(link: SshLink, live: LiveForward, socket: net.Socket) {
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

  private async listenRemote(link: SshLink, live: LiveForward): Promise<void> {
    await link.addRemoteForward(live.bindHost, live.bindPort, (stream) => {
      const socket = net.connect(live.destPort, live.destHost);
      socket.once("error", () => stream.destroy());
      socket.once("connect", () => pipeSockets(socket, stream));
    });
  }

  private async teardown(live: LiveForward): Promise<void> {
    if (live.server) {
      await new Promise<void>((resolve) => live.server!.close(() => resolve()));
      live.server = undefined;
    }
    if (live.kind === "remote") {
      const link = this.linkFor(live.hostId);
      await link?.removeRemoteForward(live.bindHost, live.bindPort);
    }
  }

  private toPortForward(row: HostForwardRow): PortForward {
    const enabled = row.enabled === 1;
    let state: ForwardState = "stopped";
    if (enabled && this.live.has(row.id)) state = "active";
    else if (enabled && this.attempts.has(row.id)) state = "starting";
    else if (enabled && this.errors.has(row.id)) state = "error";
    return {
      id: row.id,
      hostId: row.host_id,
      name: row.name ?? undefined,
      kind: row.kind as ForwardKind,
      bindHost: row.bind_host,
      bindPort: row.bind_port,
      destHost: row.dest_host,
      destPort: row.dest_port,
      enabled,
      state,
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
