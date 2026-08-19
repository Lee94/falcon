import crypto from "node:crypto";
import net from "node:net";
import type { Duplex } from "node:stream";
import type { ForwardKind, ForwardState, PortForward, PortForwardInput } from "@mojito/shared";
import type { Db, ProjectRow, SshForwardRow } from "../db.js";
import { validateForwardInput } from "./forwardSpec.js";
import type { SshLink } from "./ssh.js";

interface LiveForward {
  id: string;
  projectId: string;
  kind: ForwardKind;
  bindHost: string;
  bindPort: number;
  destHost: string;
  destPort: number;
  /** local 转发的本机监听器 */
  server?: net.Server;
}

export class ForwardConflictError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ForwardConflictError";
  }
}

/**
 * 一个项目上的端口转发运行时。规则在 DB，活着的隧道在内存。
 *
 * 链路断开只拆隧道、不删规则：重连成功后按 enabled 再拉起来。
 * 本地监听器也跟着拆——断链期间占着端口会让人以为还通着。
 */
export class ForwardManager {
  private live = new Map<string, LiveForward>();
  private starting = new Map<string, Promise<void>>();
  private errors = new Map<string, string>();

  constructor(
    private db: Db,
    private linkFor: (projectId: string) => SshLink | null
  ) {}

  list(projectId: string): PortForward[] {
    return this.db.listForwards(projectId).map((row) => this.toPortForward(row));
  }

  get(id: string): PortForward | undefined {
    const row = this.db.getForward(id);
    return row ? this.toPortForward(row) : undefined;
  }

  hasEnabled(projectId: string): boolean {
    return this.db.listForwards(projectId).some((r) => r.enabled === 1);
  }

  async create(project: ProjectRow, input: PortForwardInput): Promise<PortForward> {
    if (project.type !== "ssh") throw new Error("只有 SSH 项目能做端口转发");
    const parsed = validateForwardInput(input);
    if (!parsed.ok) throw new Error(parsed.error);
    this.assertBindFree(project.id, parsed.value, undefined);

    const row: SshForwardRow = {
      id: crypto.randomUUID(),
      project_id: project.id,
      name: parsed.value.name ?? null,
      kind: parsed.value.kind,
      bind_host: parsed.value.bindHost,
      bind_port: parsed.value.bindPort,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
      created_at: Date.now(),
    };
    this.db.insertForward(row);
    if (row.enabled === 1) {
      await this.start(row.id).catch(() => {});
    }
    return this.toPortForward(this.db.getForward(row.id)!);
  }

  async update(
    project: ProjectRow,
    id: string,
    patch: Partial<PortForwardInput>
  ): Promise<PortForward> {
    const existing = this.db.getForward(id);
    if (!existing || existing.project_id !== project.id) throw new Error("转发规则不存在");
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
    this.assertBindFree(project.id, parsed.value, id);

    const next: SshForwardRow = {
      ...existing,
      name: parsed.value.name ?? null,
      kind: parsed.value.kind,
      bind_host: parsed.value.bindHost,
      bind_port: parsed.value.bindPort,
      dest_host: parsed.value.destHost,
      dest_port: parsed.value.destPort,
      enabled: parsed.value.enabled ? 1 : 0,
    };
    this.db.updateForward(next);

    await this.stop(id);
    if (next.enabled === 1) await this.start(id).catch(() => {});
    return this.toPortForward(this.db.getForward(id)!);
  }

  async remove(project: ProjectRow, id: string): Promise<void> {
    const existing = this.db.getForward(id);
    if (!existing || existing.project_id !== project.id) throw new Error("转发规则不存在");
    await this.stop(id);
    this.db.deleteForward(id);
    this.errors.delete(id);
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
  }

  stopAll(projectId: string) {
    for (const live of [...this.live.values()]) {
      if (live.projectId !== projectId) continue;
      void this.teardown(live);
      this.live.delete(live.id);
    }
    for (const [id, row] of this.starting) {
      if (this.db.getForward(id)?.project_id === projectId) {
        void row.catch(() => {});
        this.starting.delete(id);
      }
    }
  }

  onLinkDown(projectId: string) {
    const had = [...this.live.values()].some((l) => l.projectId === projectId);
    this.stopAll(projectId);
    if (!had && !this.hasEnabled(projectId)) return;
    for (const row of this.db.listForwards(projectId)) {
      if (row.enabled === 1) this.errors.set(row.id, "SSH 链路断开");
    }
  }

  async onLinkUp(projectId: string) {
    for (const row of this.db.listForwards(projectId)) {
      if (row.enabled === 1) void this.start(row.id);
    }
  }

  async restoreEnabled(): Promise<void> {
    for (const projectId of this.db.listEnabledForwardProjectIds()) {
      const project = this.db.getProject(projectId);
      if (!project || project.type !== "ssh") continue;
      const link = this.linkFor(projectId);
      if (!link) continue;
      try {
        await link.getClient();
        await this.onLinkUp(projectId);
      } catch (err) {
        const msg = (err as Error).message;
        for (const row of this.db.listForwards(projectId)) {
          if (row.enabled === 1) this.errors.set(row.id, msg);
        }
      }
    }
  }

  private async startNow(id: string): Promise<void> {
    const row = this.db.getForward(id);
    if (!row || row.enabled !== 1) return;
    if (this.live.has(id)) return;

    const link = this.linkFor(row.project_id);
    if (!link) {
      this.errors.set(id, "项目不存在");
      return;
    }

    this.errors.delete(id);
    try {
      const live: LiveForward = {
        id: row.id,
        projectId: row.project_id,
        kind: row.kind as ForwardKind,
        bindHost: row.bind_host,
        bindPort: row.bind_port,
        destHost: row.dest_host,
        destPort: row.dest_port,
      };
      if (row.kind === "local") {
        live.server = await this.listenLocal(link, live);
      } else {
        await this.listenRemote(link, live);
      }
      this.live.set(id, live);
    } catch (err) {
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
      const link = this.linkFor(live.projectId);
      await link?.removeRemoteForward(live.bindHost, live.bindPort);
    }
  }

  private assertBindFree(
    projectId: string,
    value: { kind: ForwardKind; bindHost: string; bindPort: number },
    exceptId: string | undefined
  ) {
    if (this.db.findForwardBind(projectId, value.kind, value.bindHost, value.bindPort, exceptId)) {
      throw new ForwardConflictError(
        `该项目已有 ${value.kind === "local" ? "本地" : "远端"} ${value.bindHost}:${value.bindPort}`
      );
    }
    if (value.kind === "local") {
      const clash = this.db.findLocalBindConflict(value.bindHost, value.bindPort, exceptId);
      if (clash) {
        throw new ForwardConflictError(
          `本机 ${value.bindHost}:${value.bindPort} 已被另一个项目占用`
        );
      }
    }
  }

  private toPortForward(row: SshForwardRow): PortForward {
    const enabled = row.enabled === 1;
    let state: ForwardState = "stopped";
    if (enabled && this.live.has(row.id)) state = "active";
    else if (enabled && this.starting.has(row.id)) state = "starting";
    else if (enabled && this.errors.has(row.id)) state = "error";
    else if (enabled) state = "stopped";
    return {
      id: row.id,
      projectId: row.project_id,
      name: row.name ?? undefined,
      kind: row.kind as ForwardKind,
      bindHost: row.bind_host,
      bindPort: row.bind_port,
      destHost: row.dest_host,
      destPort: row.dest_port,
      enabled,
      state,
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
