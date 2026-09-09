import * as pty from "@lydell/node-pty";
import { applyTermPtyEnv, type NonDurableReason, type TermAppearance } from "@falcon/shared";
import { prependPath } from "../askpass/install.js";
import * as zcmd from "../zellij/command.js";
import {
  isProcessInJob,
  localDownloader,
  localExec,
  localHasTar,
  localKind,
} from "../zellij/exec.js";
import { buildCommandLine, hostLayout, type HostLayout } from "../zellij/host.js";
import {
  ensureZellij,
  InstallError,
  type StageFn,
} from "../zellij/install.js";
import { localTarget } from "../zellij/version.js";
import type { AttachResult, Backend, BackendCallbacks } from "./backend.js";
import { normalizeCaptured, SessionGoneError } from "./backend.js";
import { resolveLocalBaseEnv } from "./loginEnv.js";

export type { NonDurableReason };

export function defaultLocalShell(): string {
  if (process.platform === "win32") return "powershell.exe";
  return process.env.SHELL || "/bin/bash";
}

/** 本地宿主的 Zellij 就绪状态。durable=false 时 reason 必定有值。 */
export interface LocalZellij {
  durable: boolean;
  reason?: NonDurableReason;
  /** 失败详情（命令 stderr 或异常消息），供用户判断该修什么再重试 */
  detail?: string;
  layout?: HostLayout;
}

let prepared: LocalZellij | null = null;

/**
 * 准备本地宿主的 Zellij。整个后端进程只做一次，结果缓存。
 *
 * 本地二进制落在后端的 --data-dir 下（不是硬编码 home）：用户指定了数据目录，
 * 依赖就该跟着走。一律用 falcon 锁定的版本，无视系统 PATH 里可能存在的 Zellij——
 * 版本锁定的全部价值就在于测试矩阵封闭，开一个"用户系统版本"的口子就等于放弃它。
 */
export async function prepareLocalZellij(
  dataDir: string,
  onStage?: StageFn,
  signal?: AbortSignal
): Promise<LocalZellij> {
  if (prepared) return prepared;

  // Windows 上先问 Job Object：在 Job 里就拿不到持久性，装了也白装
  if (await isProcessInJob()) {
    prepared = { durable: false, reason: "windows-job-object" };
    return prepared;
  }

  const target = localTarget();
  if (!target) {
    prepared = { durable: false, reason: "arch-unsupported" };
    return prepared;
  }

  try {
    const layout = await ensureZellij(localExec, {
      kind: localKind(),
      root: dataDir,
      target,
      downloader: await localDownloader(),
      hasTar: await localHasTar(),
      onStage,
      signal,
    });
    prepared = { durable: true, layout };
  } catch (err) {
    const failed: LocalZellij = {
      durable: false,
      reason: err instanceof InstallError ? err.reason : "verify-failed",
      detail: err instanceof InstallError ? err.detail : (err as Error).message,
    };
    // 取消是用户这一次的选择，不是这台机器的属性——缓存它会让后续会话
    // 全部莫名其妙地非持久，直到后端重启
    if (failed.reason === "cancelled") return failed;
    prepared = failed;
  }
  return prepared;
}

/**
 * 已探测到的本地状态；null = 还没探测过。
 * 供 /api/system 报告用——它不该触发下载，那是首次创建本地会话时才做的事。
 */
export function peekLocalZellij(): LocalZellij | null {
  return prepared;
}

/** 重试用：清掉缓存的准备结果 */
export function resetLocalZellij() {
  prepared = null;
}

// ---- Zellij 操作 ----

/**
 * PTY 环境的基底不是 process.env 而是 login 解析结果（见 loginEnv.ts）：
 * 后端可能由 launchd / IDE 启动，process.env 缺 PATH 补全与 LANG，而 pane
 * 里的 shell 是非 login 起的，修不回来。zellij 的路径变量与深浅线索照旧叠加。
 */
async function env(
  layout: HostLayout,
  appearance?: TermAppearance,
  askpass?: { bin: string; sessionId: string }
): Promise<Record<string, string>> {
  let out = applyTermPtyEnv(
    { ...(await resolveLocalBaseEnv()), ...zcmd.zellijEnv(layout) },
    appearance
  );
  if (askpass) {
    out = prependPath(out, askpass.bin);
    out.FALCON_SESSION_ID = askpass.sessionId;
  }
  return out;
}

async function zellij(layout: HostLayout, args: string[]) {
  return localExec(
    buildCommandLine(localKind(), [layout.bin, ...args], zcmd.zellijEnv(layout))
  );
}

export async function localHasSession(
  layout: HostLayout,
  sessionId: string
): Promise<boolean> {
  // `ls` 一个 session 都没有时退出码是 1（stderr 打 "No active zellij sessions found."），
  // 所以非零退出码不代表出错，直接按"没有"处理即可。
  const res = await zellij(layout, zcmd.listArgs(layout));
  if (res.code !== 0) return false;
  return zcmd
    .parseLiveSessions(res.stdout)
    .includes(zcmd.zellijSessionName(sessionId));
}

export async function localCapture(
  layout: HostLayout,
  sessionId: string
): Promise<string> {
  // 多一次调用换来的是接回时真能拿到内容，理由见 dumpScreenArgs
  const panes = await zellij(layout, zcmd.listPanesArgs(layout, sessionId));
  if (panes.code !== 0) return "";
  const paneId = zcmd.parseTerminalPaneId(panes.stdout);
  if (!paneId) return "";

  const res = await zellij(layout, zcmd.dumpScreenArgs(layout, sessionId, paneId));
  if (res.code !== 0) return "";
  return normalizeCaptured(res.stdout.replace(/\s+$/, "") + "\n");
}

/** 会话聚焦 pane 的前台命令；空闲或问不到时为 null。见 parseClientRunningCommand */
export async function localForeground(
  layout: HostLayout,
  sessionId: string
): Promise<string | null> {
  const res = await zellij(layout, zcmd.listClientsArgs(layout, sessionId));
  if (res.code !== 0) return null;
  return zcmd.parseClientRunningCommand(res.stdout);
}

export async function localKill(layout: HostLayout, sessionId: string): Promise<void> {
  await zellij(layout, zcmd.deleteSessionArgs(layout, sessionId)).catch(() => {});
}

/** 列出该宿主机上所有 falcon 建的会话名（孤儿会话检测用） */
export async function localListFalconSessions(
  layout: HostLayout
): Promise<string[]> {
  const res = await zellij(layout, zcmd.listArgs(layout));
  if (res.code !== 0) return [];
  return zcmd.parseLiveSessions(res.stdout).filter(zcmd.isFalconSession);
}

// ---- 附着 ----

export interface LocalAttachOptions {
  sessionId: string;
  cwd?: string;
  shell?: string;
  durable: boolean;
  /** 持久会话所用的 Zellij 布局；durable=true 时必须提供 */
  layout?: HostLayout;
  /** true = 只接回已存在的会话，不隐式新建 */
  reattach?: boolean;
  cols: number;
  rows: number;
  /** 当前 Viewer 的终端深浅；接回时内层 shell 的 env 已经冻住，只影响新会话 */
  appearance?: TermAppearance;
  /** sudo askpass 包装所在目录，会插到 PATH 最前；缺省不注入 */
  askpassBin?: string;
}

export async function attachLocal(
  opts: LocalAttachOptions,
  cb: BackendCallbacks
): Promise<AttachResult> {
  let proc: pty.IPty;
  let capturedHistory: string | undefined;
  const askpass = opts.askpassBin
    ? { bin: opts.askpassBin, sessionId: opts.sessionId }
    : undefined;

  if (opts.durable) {
    const layout = opts.layout;
    if (!layout) throw new Error("持久会话缺少 Zellij 布局");

    if (opts.reattach) {
      if (!(await localHasSession(layout, opts.sessionId))) throw new SessionGoneError();
      capturedHistory = await localCapture(layout, opts.sessionId);
    }

    proc = pty.spawn(
      layout.bin,
      zcmd.attachArgs(layout, opts.sessionId, {
        cwd: opts.cwd,
        // 始终显式给 shell：Zellij 在 $SHELL 缺失时 pane 起不来（见 POSIX_PROBE 注释）
        shell: opts.shell ?? defaultLocalShell(),
      }),
      {
        name: "xterm-256color",
        cols: opts.cols,
        rows: opts.rows,
        env: await env(layout, opts.appearance, askpass),
      }
    );
  } else {
    let spawnedEnv = applyTermPtyEnv(await resolveLocalBaseEnv(), opts.appearance);
    if (askpass) {
      spawnedEnv = prependPath(spawnedEnv, askpass.bin);
      spawnedEnv.FALCON_SESSION_ID = askpass.sessionId;
    }
    proc = pty.spawn(opts.shell ?? defaultLocalShell(), [], {
      name: "xterm-256color",
      cols: opts.cols,
      rows: opts.rows,
      cwd: opts.cwd,
      env: spawnedEnv,
    });
  }

  proc.onData((data) => cb.onData(data));
  proc.onExit(() => cb.onExit());

  const backend: Backend = {
    write: (data) => proc.write(data),
    // 仅非持久会话有意义：持久会话外层 PTY 的前台永远是 Zellij 客户端。
    // Windows 的 IPty.process 是静态标题不是前台进程，报出去只会造成误弹确认。
    processName:
      !opts.durable && process.platform !== "win32"
        ? () => {
            try {
              return proc.process || undefined;
            } catch {
              return undefined;
            }
          }
        : undefined,
    resize: (cols, rows) => {
      try {
        proc.resize(cols, rows);
      } catch {
        // PTY 已退出时 resize 会抛错，忽略
      }
    },
    destroy: () => {
      try {
        proc.kill();
      } catch {
        // 已退出
      }
    },
  };

  return { backend, durable: opts.durable, capturedHistory };
}
