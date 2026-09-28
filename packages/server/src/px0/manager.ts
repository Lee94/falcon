/**
 * px0 实例的生命周期（ADR 0017 决定三、五、六）。一个项目最多一个实例，只在内存里。
 *
 * 两边都让 px0 挂在一个 pty 上，靠挂断收尸：px0 没有鉴权，一个没人管的孤儿就是
 * 一个谁都能读仓库的服务，而 falcon 被 kill -9、崩溃、断网时我们没有机会去杀它。
 * - 本地项目：node-pty 起在后端本机，听本机 127.0.0.1 的随机端口。后端进程一没，
 *   pty 主端随之关闭，px0 作为会话首进程收到 SIGHUP 退出（实测普通 spawn 会留孤儿）。
 * - SSH 项目：经项目链路开一条**带 pty 的** exec 通道跑 px0，通道一关（含断链）
 *   远端 sshd 挂断 pty。px0 听远端 127.0.0.1 的随机端口；反代的每条连接直接经
 *   forwardOut 打过去，后端本机不另开监听端口——本机再多开一个回环端口就多一个
 *   能绕过 falcon 登录的口子。
 *
 * 打开入口页才拉起（open），没有在途请求且 IDLE_MS 内没有新请求就停：px0 页面开着
 * 时总挂着一条 SSE，所以这等于标签页关了。
 */

import fs from "node:fs";
import net from "node:net";
import type { Duplex } from "node:stream";
import * as pty from "@lydell/node-pty";
import type { ClientChannel } from "ssh2";
import { px0BasePath } from "@falcon/shared";
import type { ProjectRow } from "../db.js";
import { resolveLocalBaseEnv } from "../sessions/loginEnv.js";
import type { SshLink } from "../sessions/ssh.js";
import { ensureLocalPx0, ensurePx0Asset, px0AssetPresent } from "./bin.js";
import {
  expandHome,
  localPx0Target,
  parseListenPort,
  parsePx0Version,
  posixInstallCommand,
  posixLaunchCommand,
  posixVersionCommand,
  PX0_VERSION,
  px0Args,
  px0TargetFromUname,
  remotePx0Path,
  tailLines,
} from "./command.js";
import type { Px0PageState, Px0Stage } from "./proxy.js";

const LISTEN_WAIT_MS = 30_000;
const STOP_GRACE_MS = 2_000;
const IDLE_MS = 15 * 60_000;
const SWEEP_MS = 60_000;
/**
 * 起不来的原因留多久。入口页每秒刷新一次，留一分钟足够让用户看到；过了这个窗口
 * 再打开就当新的一次尝试，不让一次陈年失败永远挡在那里。
 */
const ERROR_TTL_MS = 60_000;
/** 启动输出只留尾巴：起不来时给用户看最后几行，px0 之后的日志没人看 */
const LOG_KEEP = 16_384;

/** 一个跑着的 px0。反代经 connect() 拿到一条到它监听端口的连接 */
export interface Px0Instance {
  readonly projectId: string;
  connect(): Promise<Duplex>;
}

interface Live extends Px0Instance {
  inflight: number;
  lastUsed: number;
  /** 我们主动停的：退出时不要记成故障 */
  stopping: boolean;
  /** 已经退出了（可能早于登记进 live） */
  dead: boolean;
  teardown(): Promise<void>;
}

interface Starting {
  stage: Px0Stage;
  cancelled: boolean;
}

class Cancelled extends Error {}

export class Px0Manager {
  private live = new Map<string, Live>();
  private starting = new Map<string, Starting>();
  private errors = new Map<string, { message: string; at: number }>();
  private sweeper: NodeJS.Timeout;

  constructor(
    private dataDir: string,
    private linkFor: (row: ProjectRow) => SshLink
  ) {
    this.sweeper = setInterval(() => void this.sweep(), SWEEP_MS);
    this.sweeper.unref();
  }

  /**
   * 入口页调用。跑着就返回 null（调用方直接反代）；否则按需拉起，返回该给用户看的
   * 页面状态。retry：用户点了重试，无视还没过期的失败。
   */
  open(row: ProjectRow, retry: boolean): Px0PageState | null {
    if (this.live.has(row.id)) return null;
    const st = this.starting.get(row.id);
    if (st) return { kind: "starting", stage: st.stage };
    const err = this.errors.get(row.id);
    if (err && !retry && Date.now() - err.at < ERROR_TTL_MS) {
      return { kind: "error", message: err.message };
    }
    this.errors.delete(row.id);

    const state: Starting = { stage: "preparing", cancelled: false };
    this.starting.set(row.id, state);
    void this.startNow(row, state)
      .catch((e: Error) => {
        if (!state.cancelled && !(e instanceof Cancelled)) {
          this.errors.set(row.id, { message: e.message, at: Date.now() });
        }
      })
      .finally(() => {
        if (this.starting.get(row.id) === state) this.starting.delete(row.id);
      });
    return { kind: "starting", stage: state.stage };
  }

  /** 反代每个请求开始时调用：跑着就记一次使用并交出实例，没跑返回 undefined */
  acquire(projectId: string): Px0Instance | undefined {
    const live = this.live.get(projectId);
    if (!live) return undefined;
    live.inflight++;
    live.lastUsed = Date.now();
    return live;
  }

  /** 与 acquire 成对：响应结束（含客户端断开）时调用 */
  release(inst: Px0Instance) {
    const live = inst as Live;
    live.inflight = Math.max(0, live.inflight - 1);
    live.lastUsed = Date.now();
  }

  /** 停掉某个项目的 px0（项目删了 / 工作目录改了）。没在跑也不报错 */
  async stop(projectId: string): Promise<void> {
    const st = this.starting.get(projectId);
    if (st) {
      st.cancelled = true;
      this.starting.delete(projectId);
    }
    this.errors.delete(projectId);
    const live = this.live.get(projectId);
    if (!live) return;
    this.live.delete(projectId);
    await live.teardown();
  }

  async shutdown(): Promise<void> {
    clearInterval(this.sweeper);
    for (const st of this.starting.values()) st.cancelled = true;
    this.starting.clear();
    const lives = [...this.live.values()];
    this.live.clear();
    await Promise.all(lives.map((l) => l.teardown()));
  }

  private async sweep() {
    const now = Date.now();
    for (const [id, live] of this.live) {
      if (live.inflight === 0 && now - live.lastUsed > IDLE_MS) void this.stop(id);
    }
  }

  private async startNow(row: ProjectRow, state: Starting): Promise<void> {
    const dir = row.working_dir?.trim();
    if (!dir) {
      throw new Error("这个项目没有工作目录。多仓库容器请在成员或附属项目上打开 px0。");
    }
    const basePath = px0BasePath(row.id);
    const live =
      row.type === "local"
        ? await this.startLocal(row.id, dir, basePath, state)
        : await this.startRemote(row, dir, basePath, state);
    if (state.cancelled) {
      await live.teardown();
      throw new Cancelled();
    }
    // 刚起来就退出了：onDied 已经把原因记下，别拿一句更含糊的覆盖掉
    if (live.dead) return;
    this.live.set(row.id, live);
  }

  private async startLocal(
    projectId: string,
    dir: string,
    basePath: string,
    state: Starting
  ): Promise<Live> {
    const target = localPx0Target();
    state.stage = target && px0AssetPresent(this.dataDir, target) ? "launching" : "downloading";
    const bin = await ensureLocalPx0(this.dataDir);
    checkCancelled(state);
    state.stage = "launching";
    // 与 cloudflared 同一份基底环境：login shell 解析出来的 PATH，px0 才找得到 claude / gh
    const env = { ...(await resolveLocalBaseEnv()), NO_COLOR: "1" };
    checkCancelled(state);
    const log = new LogTail();
    let exit: () => void = () => {};
    const exited = new Promise<void>((resolve) => (exit = resolve));
    let proc: pty.IPty;
    try {
      proc = pty.spawn(bin, px0Args({ basePath, dir }), {
        name: "dumb",
        cols: 200,
        rows: 50,
        env,
      });
    } catch (err) {
      throw new Error(`px0 没能启动：${(err as Error).message}`);
    }
    const onData = (cb: () => void) => {
      const sub = proc.onData(cb);
      return () => sub.dispose();
    };
    proc.onData((d) => log.push(d));
    proc.onExit(() => exit());

    let port: number;
    try {
      port = await waitForPort(log, onData, exited);
    } catch (err) {
      await killPty(proc, exited);
      throw err;
    }

    const live: Live = {
      projectId,
      inflight: 0,
      lastUsed: Date.now(),
      stopping: false,
      dead: false,
      connect: () => connectLocal(port),
      teardown: async () => {
        live.stopping = true;
        await killPty(proc, exited);
      },
    };
    void exited.then(() => this.onDied(live, log));
    return live;
  }

  private async startRemote(
    row: ProjectRow,
    dir: string,
    basePath: string,
    state: Starting
  ): Promise<Live> {
    const link = this.linkFor(row);
    const facts = await link.hostFacts();
    checkCancelled(state);
    if (facts.kind !== "posix") {
      throw new Error("px0 暂不支持 Windows 宿主机。");
    }
    const target = px0TargetFromUname(facts.uname ?? "");
    if (!target) {
      throw new Error(`px0 没有这台宿主机的构建（${facts.uname ?? "未知平台"}）。`);
    }

    const bin = remotePx0Path(facts.root);
    const probe = await link.exec(posixVersionCommand(bin));
    checkCancelled(state);
    if (parsePx0Version(probe.stdout) !== PX0_VERSION) {
      state.stage = px0AssetPresent(this.dataDir, target, {}) ? "installing" : "downloading";
      const local = await ensurePx0Asset(this.dataDir, target);
      checkCancelled(state);
      state.stage = "installing";
      const bytes = await fs.promises.readFile(local);
      const res = await link.execWithInput(posixInstallCommand(bin), bytes);
      if (res.code !== 0) {
        throw new Error(`往宿主机写 px0 失败：${res.stderr.trim() || `exit ${res.code}`}`);
      }
      checkCancelled(state);
    }

    state.stage = "launching";
    const shell = row.shell?.trim() || facts.shell;
    const args = px0Args({ basePath, dir: expandHome(dir, facts.home) });
    const channel = await link.execStream(posixLaunchCommand(shell, bin, args), { pty: true });
    const log = new LogTail();
    // 有 pty 时 stderr 已经并进 stdout，stderr 那条基本不会来数据，照听无妨
    channel.on("data", (d: Buffer) => log.push(d));
    channel.stderr.on("data", (d: Buffer) => log.push(d));
    const onData = (cb: () => void) => {
      channel.on("data", cb);
      channel.stderr.on("data", cb);
      return () => {
        channel.off("data", cb);
        channel.stderr.off("data", cb);
      };
    };
    let closed = false;
    const exited = new Promise<void>((resolve) =>
      channel.once("close", () => {
        closed = true;
        resolve();
      })
    );

    let port: number;
    try {
      port = await waitForPort(log, onData, exited);
    } catch (err) {
      await closeChannel(channel, () => closed);
      throw err;
    }

    const live: Live = {
      projectId: row.id,
      inflight: 0,
      lastUsed: Date.now(),
      stopping: false,
      dead: false,
      connect: () => connectRemote(link, port),
      teardown: async () => {
        live.stopping = true;
        await closeChannel(channel, () => closed);
      },
    };
    void exited.then(() => this.onDied(live, log));
    return live;
  }

  /** px0 进程 / 通道没了。我们自己停的不算故障；否则留下原因给入口页 */
  private onDied(live: Live, log: LogTail) {
    live.dead = true;
    if (this.live.get(live.projectId) === live) this.live.delete(live.projectId);
    if (live.stopping) return;
    const tail = tailLines(log.toString());
    this.errors.set(live.projectId, {
      message: tail ? `px0 已退出：\n${tail}` : "px0 已退出（SSH 链路断开或进程被杀）",
      at: Date.now(),
    });
  }
}

function checkCancelled(state: Starting) {
  if (state.cancelled) throw new Cancelled();
}

class LogTail {
  private text = "";
  push(d: Buffer | string) {
    this.text += typeof d === "string" ? d : d.toString("utf8");
    if (this.text.length > LOG_KEEP * 2) this.text = this.text.slice(-LOG_KEEP);
  }
  toString() {
    return this.text;
  }
}

/**
 * 等 px0 打出监听地址。onData 订阅的是「又来了一批输出」（内容已由 LogTail 收走），
 * 返回退订函数。之后 LogTail 的监听还挂着，输出继续被读走——不读的话缓冲写满，
 * px0 下一次打日志就会卡住。
 */
function waitForPort(
  log: LogTail,
  onData: (cb: () => void) => () => void,
  exited: Promise<void>
): Promise<number> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unsubscribe();
      fn();
    };
    const detail = () => {
      const tail = tailLines(log.toString());
      return tail ? `：\n${tail}` : "";
    };
    const check = () => {
      const port = parseListenPort(log.toString());
      if (port) finish(() => resolve(port));
    };
    const timer = setTimeout(
      () => finish(() => reject(new Error(`等不到 px0 的监听地址${detail()}`))),
      LISTEN_WAIT_MS
    );
    const unsubscribe = onData(check);
    void exited.then(() => {
      // 退出前最后一批输出可能刚到，先再看一眼
      check();
      finish(() => reject(new Error(`px0 没能启动${detail()}`)));
    });
    check();
  });
}

function connectLocal(port: number): Promise<Duplex> {
  return new Promise((resolve, reject) => {
    const socket = net.connect(port, "127.0.0.1");
    socket.once("connect", () => {
      socket.off("error", reject);
      resolve(socket);
    });
    socket.once("error", reject);
  });
}

/**
 * 经项目链路 forwardOut 到远端 px0。ssh2 的通道不是 net.Socket，http 客户端在
 * 某些路径上会调 setNoDelay / setTimeout 之类，补成空操作（ssh2 自带的 HTTPAgent
 * 也是这么干的）。
 */
async function connectRemote(link: SshLink, port: number): Promise<Duplex> {
  const client = await link.getClient();
  const stream = await new Promise<ClientChannel>((resolve, reject) => {
    client.forwardOut("127.0.0.1", 0, "127.0.0.1", port, (err, ch) =>
      err ? reject(err) : resolve(ch)
    );
  });
  const noop = function (this: unknown) {
    return this;
  };
  const shim = stream as unknown as Record<string, unknown>;
  for (const name of ["setNoDelay", "setKeepAlive", "setTimeout", "ref", "unref"]) {
    if (typeof shim[name] !== "function") shim[name] = noop;
  }
  return stream;
}

/** 停本机 px0：先 TERM，宽限期过了再 KILL。Windows 上 node-pty 的 kill 不收信号名 */
function killPty(proc: pty.IPty, exited: Promise<void>): Promise<void> {
  let gone = false;
  void exited.then(() => (gone = true));
  const kill = (signal: string) => {
    try {
      if (process.platform === "win32") proc.kill();
      else proc.kill(signal);
    } catch {
      // 已经没了
    }
  };
  kill("SIGTERM");
  return Promise.race([
    exited,
    new Promise<void>((resolve) =>
      setTimeout(() => {
        if (!gone) kill("SIGKILL");
        resolve();
      }, STOP_GRACE_MS)
    ),
  ]);
}

/**
 * 停远端 px0：先发 TERM（它会顺手关掉拉起的语言服务器），宽限期过了不管结果都关通道——
 * pty 一挂断 px0 就收到 SIGHUP。sshd 不支持 signal 请求时就只剩后一条路。
 */
function closeChannel(channel: ClientChannel, isClosed: () => boolean): Promise<void> {
  if (isClosed()) return Promise.resolve();
  return new Promise((resolve) => {
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      clearTimeout(grace);
      resolve();
    };
    channel.once("close", finish);
    try {
      channel.signal("TERM");
    } catch {
      // 通道已经在关了
    }
    const grace = setTimeout(() => {
      try {
        channel.close();
      } catch {
        // 同上
      }
      setTimeout(finish, 500);
    }, STOP_GRACE_MS);
  });
}
