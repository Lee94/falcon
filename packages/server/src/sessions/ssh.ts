import crypto from "node:crypto";
import { EventEmitter } from "node:events";
import fs from "node:fs";
import { StringDecoder } from "node:string_decoder";
import { Client, type ClientChannel, type TcpConnectionDetails } from "ssh2";
import type { ProjectRow } from "../db.js";
import type { Db } from "../db.js";
import type { SecretBox } from "../crypto.js";
import * as zcmd from "../zellij/command.js";
import {
  buildCommandLine,
  buildDetachedCommandLine,
  buildPtyCommandLine,
  encodePowerShell,
  parsePosixProbe,
  parseWindowsProbe,
  POSIX_PROBE,
  POSIX_PROBE_LINES,
  quotePosix,
  quotePowerShell,
  remoteRoot,
  WINDOWS_PROBE,
  WINDOWS_PROBE_SCRIPT,
  type HostKind,
  type HostLayout,
} from "../zellij/host.js";
import {
  ensureZellij,
  failureText,
  InstallError,
  withInstallRetry,
  type ExecFn,
  type ExecResult,
  type StageFn,
} from "../zellij/install.js";
import {
  targetFromUname,
  targetFromWindowsArch,
  ZELLIJ_VERSION,
  type ZellijTarget,
} from "../zellij/version.js";
import { applyTermPtyEnv, type TermAppearance } from "@mojito/shared";
import type { AttachResult, Backend, BackendCallbacks } from "./backend.js";
import { normalizeCaptured, SessionGoneError } from "./backend.js";
import type { NonDurableReason } from "./local.js";

export { SessionGoneError };

export class HostKeyMismatchError extends Error {
  constructor(host: string) {
    super(`主机密钥指纹与首次连接时记录的不一致（${host}），已拒绝连接`);
  }
}

/** 远端宿主机探测结果 */
interface RemoteProbe {
  kind: HostKind;
  home: string;
  target: ZellijTarget | null;
  downloader: "curl" | "wget" | "none";
  hasTar: boolean;
  /** 远端登录 shell。项目没指定 shell 时用它，绝不留给 Zellij 自己猜——见 POSIX_PROBE */
  shell: string;
}

/** 远端 Zellij 就绪状态。durable=false 时 reason 必定有值。 */
export interface RemoteZellij {
  durable: boolean;
  reason?: NonDurableReason;
  /** 失败详情（远端 stderr 或异常消息），供用户判断该修什么再重试 */
  detail?: string;
  layout?: HostLayout;
  kind: HostKind;
}

/**
 * 一个 SSH 项目与远端主机之间的连接。同项目的多个会话复用同一条连接（多 channel）。
 * 连接断开时触发 "down" 事件，重连由 SessionManager 编排。
 */
export class SshLink extends EventEmitter {
  private client: Client | null = null;
  private connecting: Promise<Client> | null = null;
  private hostKeyMismatch = false;
  private probed: RemoteProbe | null = null;
  private zellij: RemoteZellij | null = null;
  /**
   * 远端转发按监听端口分发。同一条链路上端口不能重复（校验层已经挡住），
   * 用端口当键而不是 destIP：sshd 回报的 destIP 可能是 127.0.0.1 / 0.0.0.0 / 实际网卡。
   */
  private remoteAcceptors = new Map<
    number,
    (info: TcpConnectionDetails, accept: () => ClientChannel, reject: () => void) => void
  >();

  constructor(
    private project: ProjectRow,
    private db: Db,
    private secrets: SecretBox
  ) {
    super();
  }

  updateProject(row: ProjectRow) {
    this.project = row;
  }

  isConnected(): boolean {
    return this.client != null;
  }

  async getClient(): Promise<Client> {
    if (this.client) return this.client;
    if (this.connecting) return this.connecting;
    this.connecting = this.connect().finally(() => {
      this.connecting = null;
    });
    return this.connecting;
  }

  private connect(): Promise<Client> {
    const p = this.project;
    this.hostKeyMismatch = false;

    const auth: Record<string, unknown> = {};
    const secret = p.ssh_secret_enc ? this.secrets.decrypt(p.ssh_secret_enc) : undefined;
    if (p.ssh_auth_method === "key") {
      auth.privateKey = fs.readFileSync(p.ssh_key_path!);
      if (secret) auth.passphrase = secret;
    } else if (p.ssh_auth_method === "password") {
      auth.password = secret;
    } else if (p.ssh_auth_method === "agent") {
      auth.agent =
        process.env.SSH_AUTH_SOCK ??
        (process.platform === "win32" ? "\\\\.\\pipe\\openssh-ssh-agent" : undefined);
    }

    return new Promise<Client>((resolve, reject) => {
      const client = new Client();
      let ready = false;

      client.on("ready", () => {
        ready = true;
        this.client = client;
        this.emit("up");
        resolve(client);
      });
      client.on("tcp connection", (info, accept, reject) => {
        const fn = this.remoteAcceptors.get(info.destPort);
        if (!fn) {
          reject();
          return;
        }
        fn(info, accept, reject);
      });
      client.on("error", (err) => {
        if (!ready) {
          reject(this.hostKeyMismatch ? new HostKeyMismatchError(p.ssh_host!) : err);
        }
      });
      client.on("close", () => {
        if (this.client === client) {
          this.client = null;
          this.emit("down");
        } else if (!ready) {
          reject(new Error("SSH 连接已关闭"));
        }
      });

      client.connect({
        host: p.ssh_host!,
        port: p.ssh_port ?? 22,
        username: p.ssh_username!,
        readyTimeout: 15000,
        keepaliveInterval: 15000,
        keepaliveCountMax: 3,
        hostHash: "sha256",
        hostVerifier: (hash: string) => {
          const known = this.db.getKnownHost(p.ssh_host!, p.ssh_port ?? 22);
          if (!known) {
            // TOFU：首次连接记录指纹
            this.db.saveKnownHost(p.ssh_host!, p.ssh_port ?? 22, hash);
            return true;
          }
          if (known !== hash) {
            this.hostKeyMismatch = true;
            return false;
          }
          return true;
        },
        ...auth,
      });
    });
  }

  dispose() {
    const c = this.client;
    this.client = null;
    this.remoteAcceptors.clear();
    c?.end();
  }

  /**
   * 远端监听 bindHost:bindPort，进来的连接交给 onStream。
   * 必须先登记 acceptor 再 forwardIn，否则握手窗口里的连接会被拒。
   */
  async addRemoteForward(
    bindHost: string,
    bindPort: number,
    onStream: (stream: ClientChannel) => void
  ): Promise<void> {
    const client = await this.getClient();
    this.remoteAcceptors.set(bindPort, (_info, accept, reject) => {
      try {
        onStream(accept());
      } catch {
        reject();
      }
    });
    await new Promise<void>((resolve, reject) => {
      client.forwardIn(bindHost, bindPort, (err) => {
        if (err) {
          this.remoteAcceptors.delete(bindPort);
          reject(err);
        } else {
          resolve();
        }
      });
    });
  }

  async removeRemoteForward(bindHost: string, bindPort: number): Promise<void> {
    this.remoteAcceptors.delete(bindPort);
    const client = this.client;
    if (!client) return;
    await new Promise<void>((resolve) => {
      client.unforwardIn(bindHost, bindPort, () => resolve());
    });
  }

  // ---- 远端命令 ----

  readonly exec: ExecFn = (commandLine, signal) =>
    this.getClient().then(
      (client) =>
        new Promise<ExecResult>((resolve, reject) => {
          client.exec(commandLine, (err, stream) => {
            if (err) return reject(err);
            // 攒 Buffer、close 时一次解码：逐 chunk toString 会把跨包的
            // UTF-8 多字节序列切成 U+FFFD（中文路径 / dump-screen 必踩）
            const stdout: Buffer[] = [];
            const stderr: Buffer[] = [];
            const abort = () => stream.close();
            signal?.addEventListener("abort", abort, { once: true });
            stream.on("data", (d: Buffer) => stdout.push(d));
            stream.stderr.on("data", (d: Buffer) => stderr.push(d));
            stream.on("close", (code: number | null) => {
              signal?.removeEventListener("abort", abort);
              resolve({
                code,
                stdout: Buffer.concat(stdout).toString("utf8"),
                stderr: Buffer.concat(stderr).toString("utf8"),
              });
            });
          });
        })
    );

  /** 同 exec，但把 input 写进远端命令的 stdin（粘贴图片等场景），写完即 EOF */
  async execWithInput(commandLine: string, input: Buffer | string): Promise<ExecResult> {
    const client = await this.getClient();
    return new Promise<ExecResult>((resolve, reject) => {
      client.exec(commandLine, (err, stream) => {
        if (err) return reject(err);
        const stdout: Buffer[] = [];
        const stderr: Buffer[] = [];
        stream.on("data", (d: Buffer) => stdout.push(d));
        stream.stderr.on("data", (d: Buffer) => stderr.push(d));
        stream.on("close", (code: number | null) =>
          resolve({
            code,
            stdout: Buffer.concat(stdout).toString("utf8"),
            stderr: Buffer.concat(stderr).toString("utf8"),
          })
        );
        stream.end(input);
      });
    });
  }

  /**
   * 宿主机类型、家目录与默认 shell，供 git 层与 shell 侦测使用。
   * probe() 自带缓存，重复调用不产生往返；RemoteProbe 本身不外泄，只给出这几项事实。
   */
  async hostFacts(): Promise<{ kind: HostKind; home: string; shell: string }> {
    const p = await this.probe();
    return { kind: p.kind, home: p.home, shell: p.shell };
  }

  // ---- 探测与安装 ----

  /**
   * 一次往返拿齐远端信息。先按 POSIX 试，失败再按 Windows 试——
   * Windows 上没有 uname，POSIX 探测必然失败，这本身就是最可靠的判据。
   *
   * 失败一律抛 InstallError("probe-failed")：探测跑不通最常见的原因是 SSH 连接
   * 本身出了问题（可重试），跟"系统认不出"（不可重试）混成一句"无法识别远端操作系统"
   * 会让用户查错方向。两者用 detail 区分。
   */
  async probe(onStage?: StageFn, attempt = 1): Promise<RemoteProbe> {
    if (this.probed) return this.probed;

    let linkError: string | undefined;
    onStage?.("probing", attempt, POSIX_PROBE_LINES.join("\n"));
    const posix = await this.exec(POSIX_PROBE).catch((err: Error) => {
      linkError = err.message;
      return null;
    });
    const p = posix && posix.code === 0 ? parsePosixProbe(posix.stdout) : null;
    if (p) {
      this.probed = {
        kind: "posix",
        home: p.home,
        target: targetFromUname(p.uname),
        downloader: p.downloader,
        hasTar: true,
        shell: p.shell,
      };
      return this.probed;
    }

    onStage?.("probing", attempt, WINDOWS_PROBE_SCRIPT);
    const win = await this.exec(WINDOWS_PROBE).catch((err: Error) => {
      linkError ??= err.message;
      return null;
    });
    const w = win && win.code === 0 ? parseWindowsProbe(win.stdout) : null;
    if (w) {
      this.probed = {
        kind: "windows",
        home: w.home,
        target: targetFromWindowsArch(w.arch),
        downloader: w.downloader,
        hasTar: w.hasTar,
        shell: w.shell,
      };
      return this.probed;
    }

    throw new InstallError(
      "probe-failed",
      failureText("probe-failed"),
      linkError ?? "远端既不是 POSIX 也不是 Windows，或探测命令被 shell 改写"
    );
  }

  /**
   * 确保远端有可用的 Zellij，返回持久能力。
   *
   * 已授权是前提——调用方（SessionManager）负责先拿到用户对该主机的授权，
   * 因为这会往用户的服务器上写入可执行文件。
   */
  async prepareZellij(
    onStage?: StageFn,
    signal?: AbortSignal
  ): Promise<RemoteZellij> {
    if (this.zellij) return this.zellij;

    const host = this.project.ssh_host!;
    const port = this.project.ssh_port ?? 22;
    const user = this.project.ssh_username!;
    const saved = this.db.getZellijHost(host, port, user);

    try {
      // 探测也走重试：这一步全靠 SSH 通道，抖一下就整个安装流程失败太亏
      const probe = await withInstallRetry((attempt) => {
        return this.probe(onStage, attempt);
      }, signal);

      if (!probe.target) {
        this.zellij = { durable: false, reason: "arch-unsupported", kind: probe.kind };
        return this.zellij;
      }

      const layout = await ensureZellij(this.exec, {
        kind: probe.kind,
        root: remoteRoot(probe.kind, probe.home),
        target: probe.target,
        baseUrl: saved?.base_url ?? undefined,
        downloader: probe.downloader,
        hasTar: probe.hasTar,
        onStage,
        signal,
      });

      this.db.upsertZellijHost(host, port, user, {
        installed_version: ZELLIJ_VERSION,
      });

      // Windows 远端：装好不等于能持久。Zellij server 没有脱离父 Job 的能力
      // （PR #5195 未合并），sshd 在 exec 通道关闭时就会清掉整个进程树，所以我们
      // 经 WMI 把 server 生到 sshd 树之外（见 buildDetachedCommandLine）。这条路
      // 依赖宿主机的 DCOM/WMI 权限，只能实测，不能靠文档判断。
      //
      // 判定只在"当前锁定版本"上有效：换过 Zellij 版本（或检测手段变了）之后，
      // 旧的"非持久"结论不该把主机永远钉死在非持久上。
      if (probe.kind === "windows") {
        let ok =
          saved?.installed_version === ZELLIJ_VERSION ? saved?.verified_durable : null;
        if (ok == null) {
          ok = (await this.verifyDurability(layout)) ? 1 : 0;
          this.db.upsertZellijHost(host, port, user, { verified_durable: ok });
        }
        if (ok !== 1) {
          this.zellij = {
            durable: false,
            reason: "verify-failed",
            detail:
              "后台 Zellij 会话没能熬过 SSH 断开（宿主机可能限制了 WMI 进程创建）",
            kind: probe.kind,
          };
          return this.zellij;
        }
      }

      this.zellij = { durable: true, layout, kind: probe.kind };
    } catch (err) {
      const failed: RemoteZellij = {
        durable: false,
        reason: err instanceof InstallError ? err.reason : "verify-failed",
        detail: err instanceof InstallError ? err.detail : (err as Error).message,
        kind: this.probed?.kind ?? "posix",
      };
      // 取消不是这台主机的属性，是用户这一次的选择——缓存它会让后续建会话
      // 全部莫名其妙地非持久，直到后端重启
      if (failed.reason === "cancelled") return failed;
      this.zellij = failed;
    }
    return this.zellij;
  }

  /**
   * 重试安装前清掉缓存的判定。
   *
   * verified_durable 必须连 DB 一起清：它是持久化的，只清内存的话用户点一百次
   * 重试都是同一个秒回的旧结论——比如修好了 WMI 权限之后仍被钉在"非持久"上。
   */
  resetZellij() {
    this.zellij = null;
    this.probed = null;
    const p = this.project;
    if (p.ssh_host && p.ssh_username) {
      this.db.upsertZellijHost(p.ssh_host, p.ssh_port ?? 22, p.ssh_username, {
        verified_durable: null,
      });
    }
  }

  /**
   * 真实断线验证：建一个后台会话 → 断开 SSH → 重连 → 看它还在不在。
   *
   * 这是唯一能回答"这台机器上的会话到底能不能熬过断线"的办法。只在每台主机上
   * 跑一次，结果持久化到 zellij_hosts。
   *
   * 创建走 WMI（buildDetachedCommandLine），与真实会话的创建路径完全一致——
   * 验证的就是这条路本身：普通 exec 拉起的 server 会随 exec 通道关闭被 sshd
   * 连坐杀掉，根本活不到断线那一步。
   */
  private async verifyDurability(layout: HostLayout): Promise<boolean> {
    const probe = this.probed!;
    const name = `mj-verify-${crypto.randomUUID().slice(0, 8)}`;
    const env = zcmd.zellijEnv(layout);
    const run = (args: string[]) =>
      this.exec(buildCommandLine(probe.kind, [layout.bin, ...args], env));

    // 后台建一个 detached session，无需 PTY；经 WMI 生到 sshd 进程树之外
    const created = await this.exec(
      buildDetachedCommandLine(
        [layout.bin, "--data-dir", layout.dataDir, "attach", name, "--create-background"],
        env
      )
    ).catch(() => null);
    if (!created || created.code !== 0) return false;

    this.dispose();
    await new Promise((r) => setTimeout(r, 500));

    let alive = false;
    try {
      const ls = await run(["--data-dir", layout.dataDir, "list-sessions", "--no-formatting"]);
      alive = ls.code === 0 && zcmd.parseLiveSessions(ls.stdout).includes(name);
    } catch {
      alive = false;
    }

    await run([
      "--data-dir",
      layout.dataDir,
      "delete-session",
      name,
      "--force",
    ]).catch(() => {});

    return alive;
  }

  // ---- 会话查询 ----

  private async zellijExec(layout: HostLayout, args: string[]) {
    const kind = this.probed?.kind ?? "posix";
    return this.exec(
      buildCommandLine(kind, [layout.bin, ...args], zcmd.zellijEnv(layout))
    );
  }

  async hasSession(layout: HostLayout, sessionId: string): Promise<boolean> {
    // 一个 session 都没有时 `ls` 退出码为 1，不代表出错
    const res = await this.zellijExec(layout, zcmd.listArgs(layout));
    if (res.code !== 0) return false;
    return zcmd
      .parseLiveSessions(res.stdout)
      .includes(zcmd.zellijSessionName(sessionId));
  }

  /** 会话聚焦 pane 的前台命令；空闲或问不到时为 null。见 parseClientRunningCommand */
  async foreground(layout: HostLayout, sessionId: string): Promise<string | null> {
    const res = await this.zellijExec(layout, zcmd.listClientsArgs(layout, sessionId));
    if (res.code !== 0) return null;
    return zcmd.parseClientRunningCommand(res.stdout);
  }

  async capture(layout: HostLayout, sessionId: string): Promise<string> {
    // 多一次往返换来的是接回时真能拿到内容，理由见 dumpScreenArgs
    const panes = await this.zellijExec(layout, zcmd.listPanesArgs(layout, sessionId));
    if (panes.code !== 0) return "";
    const paneId = zcmd.parseTerminalPaneId(panes.stdout);
    if (!paneId) return "";

    const res = await this.zellijExec(
      layout,
      zcmd.dumpScreenArgs(layout, sessionId, paneId)
    );
    if (res.code !== 0) return "";
    return normalizeCaptured(res.stdout.replace(/\s+$/, "") + "\n");
  }

  async killSession(layout: HostLayout, sessionId: string): Promise<void> {
    await this.zellijExec(layout, zcmd.deleteSessionArgs(layout, sessionId)).catch(
      () => {}
    );
  }

  /** 该主机上所有 mojito 建的会话名（孤儿会话检测用） */
  async listMojitoSessions(layout: HostLayout): Promise<string[]> {
    const res = await this.zellijExec(layout, zcmd.listArgs(layout));
    if (res.code !== 0) return [];
    return zcmd.parseLiveSessions(res.stdout).filter(zcmd.isMojitoSession);
  }

  // ---- 附着 ----

  async attachSession(
    opts: {
      sessionId: string;
      cwd?: string;
      shell?: string;
      durable: boolean;
      layout?: HostLayout;
      reattach?: boolean;
      cols: number;
      rows: number;
      appearance?: TermAppearance;
    },
    cb: BackendCallbacks
  ): Promise<AttachResult> {
    const client = await this.getClient();
    const kind = this.probed?.kind ?? "posix";

    let capturedHistory: string | undefined;
    if (opts.durable && opts.reattach) {
      const layout = opts.layout!;
      if (!(await this.hasSession(layout, opts.sessionId))) throw new SessionGoneError();
      capturedHistory = await this.capture(layout, opts.sessionId);
    }

    const ptyOpts = { rows: opts.rows, cols: opts.cols, term: "xterm-256color" };
    const termEnv = applyTermPtyEnv({}, opts.appearance);

    // Windows 持久会话：server 必须生在 sshd 进程树之外，否则创建它的 PTY 通道
    // 一关（关标签、断网）server 就被 sshd 连坐杀掉，"持久"名存实亡。先经 WMI
    // 后台建好——带全部会话级 options，它们只在创建时生效——下面的 PTY attach
    // 就是纯附着，attach 客户端死掉不影响 server。reattach 时会话已存在
    // （上面刚用 hasSession 验过），跳过。
    if (opts.durable && kind === "windows" && !opts.reattach) {
      const layout = opts.layout!;
      if (!(await this.hasSession(layout, opts.sessionId))) {
        const created = await this.exec(
          buildDetachedCommandLine(
            [
              layout.bin,
              ...zcmd.createBackgroundArgs(layout, opts.sessionId, {
                cwd: opts.cwd,
                shell: opts.shell ?? this.probed?.shell,
              }),
            ],
            { ...zcmd.zellijEnv(layout), ...termEnv }
          )
        );
        // WMI 的退出码只是尽力而为（见 buildDetachedCommandLine），
        // 会话真建出来没有以 hasSession 的事实为准
        if (!(await this.hasSession(layout, opts.sessionId))) {
          throw new Error(
            `无法在 Windows 远端后台创建 Zellij 会话（退出码 ${created.code}）：` +
              (created.stderr.trim() || created.stdout.trim() || "无输出")
          );
        }
      }
    }

    const stream = await new Promise<ClientChannel>((resolve, reject) => {
      if (opts.durable) {
        const layout = opts.layout!;
        const cmd = buildPtyCommandLine(
          kind,
          [
            layout.bin,
            ...zcmd.attachArgs(layout, opts.sessionId, {
              cwd: opts.cwd,
              // 项目没指定就用探测到的登录 shell，不能不传
              shell: opts.shell ?? this.probed?.shell,
            }),
          ],
          { ...zcmd.zellijEnv(layout), ...termEnv },
          // 包一层登录 shell，否则 ~/.profile 里的 PATH 全丢——详见 buildPtyCommandLine
          this.probed?.shell
        );
        client.exec(cmd, { pty: ptyOpts }, (err, s) => (err ? reject(err) : resolve(s)));
      } else {
        // 非持久：直接起 shell。一律走 exec + env，不能用 client.shell()——
        // ssh2 的 shell() 塞不进 COLORFGBG / COLORTERM。
        let cmd: string;
        if (kind === "windows") {
          const parts: string[] = [
            ...Object.entries(termEnv).map(([k, v]) => `$env:${k} = ${quotePowerShell(v)}`),
          ];
          if (opts.cwd) parts.push(`Set-Location -LiteralPath ${quotePowerShell(opts.cwd)}`);
          parts.push(opts.shell ? `& ${quotePowerShell(opts.shell)}` : "powershell");
          cmd = encodePowerShell(parts.join("; "));
        } else {
          // 非持久会话同样要走登录 shell，否则 PATH 与持久会话不一致
          const assigns = Object.entries(termEnv).map(([k, v]) => `${k}=${quotePosix(v)}`);
          const prefix = assigns.length ? `env ${assigns.join(" ")} ` : "";
          const cd = opts.cwd ? `cd ${quotePosix(opts.cwd)} && ` : "";
          const sh = quotePosix(opts.shell ?? this.probed?.shell ?? "/bin/sh");
          cmd = `${cd}exec ${prefix}${sh} -l`;
        }
        client.exec(cmd, { pty: ptyOpts }, (err, s) => (err ? reject(err) : resolve(s)));
      }
    });

    // 流式安全解码：TCP 分包会把 UTF-8 多字节字符切开，直接 toString 会产生
    // U+FFFD——终端里大量中文输出时表现为偶发乱码。StringDecoder 把跨包的
    // 半个字符留到下一个 chunk。
    const outDecoder = new StringDecoder("utf8");
    const errDecoder = new StringDecoder("utf8");
    stream.on("data", (d: Buffer) => cb.onData(outDecoder.write(d)));
    stream.stderr.on("data", (d: Buffer) => cb.onData(errDecoder.write(d)));
    stream.on("close", () => cb.onExit());

    const backend: Backend = {
      write: (data) => stream.write(data),
      resize: (cols, rows) => stream.setWindow(rows, cols, 0, 0),
      destroy: () => stream.close(),
    };

    return { backend, durable: opts.durable, capturedHistory };
  }
}
