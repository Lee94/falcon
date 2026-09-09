/**
 * docker 执行层：把 argv 交给宿主机跑，把结果翻译成结构化事实或 DockerError。
 *
 * 核心约定同 git/repo.ts：**非零退出码是正常返回值，绝不 reject**。判错一律
 * 显式查 res.code；只有 exec 本身 reject 才是链路故障。
 */

import type {
  DockerComposeFile,
  DockerComposeService,
  DockerLogs,
  DockerOpInput,
  DockerOpResult,
  DockerSnapshot,
  DockerUnavailableReason,
} from "@falcon/shared";
import { relativizeIndexLine, resolveInside } from "../files.js";
import { dirnameOf } from "../git/path.js";
import type { ExecResult } from "../zellij/install.js";
import * as dc from "./command.js";
import { DockerError, dockerFailureText } from "./error.js";
import type { DockerHost } from "./host.js";

export const TIMEOUT_READ = 15_000;
export const TIMEOUT_MUTATE = 60_000;
export const TIMEOUT_LOGS = 20_000;
export const TIMEOUT_PRUNE = 120_000;
export const TIMEOUT_COMPOSE_UP = 300_000;

interface RunOpts {
  timeoutMs?: number;
  signal?: AbortSignal;
}

function deadline(opts?: RunOpts): { signal: AbortSignal; dispose(): void } {
  const ac = new AbortController();
  const ms = opts?.timeoutMs ?? TIMEOUT_READ;
  const timer = setTimeout(() => ac.abort(), ms);
  const onOuter = () => ac.abort();
  opts?.signal?.addEventListener("abort", onOuter, { once: true });
  return {
    signal: ac.signal,
    dispose() {
      clearTimeout(timer);
      opts?.signal?.removeEventListener("abort", onOuter);
    },
  };
}

export async function execRaw(
  host: DockerHost,
  commandLine: string,
  opts?: RunOpts
): Promise<ExecResult> {
  const d = deadline(opts);
  try {
    return await host.exec(commandLine, d.signal);
  } catch (err) {
    throw new DockerError("link-failed", dockerFailureText("link-failed"), (err as Error).message);
  } finally {
    d.dispose();
  }
}

async function runDocker(
  host: DockerHost,
  argv: string[],
  opts?: RunOpts
): Promise<ExecResult> {
  return execRaw(host, dc.buildDockerCommandLine(host.kind, argv), opts);
}

function failEnv(res: ExecResult): DockerError {
  const reason = dc.classifyDockerFailure(res.stderr, res.stdout, res.code);
  return new DockerError(reason, dockerFailureText(reason), dc.errorDetail(res.stderr, res.stdout, res.code));
}

function opFail(res: ExecResult): DockerOpResult {
  const reason = dc.classifyDockerFailure(res.stderr, res.stdout, res.code);
  return { ok: false, reason, detail: dc.errorDetail(res.stderr, res.stdout, res.code) };
}

function opOk(res: ExecResult): DockerOpResult {
  const detail = (res.stdout.trim() || res.stderr.trim()).slice(0, 2000);
  return { ok: true, detail };
}

export function unavailableSnapshot(
  reason: DockerUnavailableReason,
  detail?: string
): DockerSnapshot {
  return {
    available: false,
    reason,
    detail,
    containers: [],
    images: [],
    composeFiles: [],
    composeAvailable: false,
  };
}

export function unavailableLogs(
  reason: DockerUnavailableReason,
  detail?: string
): DockerLogs {
  return { available: false, reason, detail, text: "" };
}

async function listComposeFiles(host: DockerHost, workDir: string): Promise<DockerComposeFile[]> {
  const res = await execRaw(host, dc.findComposeFilesCommand(host.kind, workDir), {
    timeoutMs: TIMEOUT_READ,
  });
  if (res.code !== 0) return [];
  return dc.parseComposeFind(res.stdout, (line) =>
    relativizeIndexLine(line, workDir, host.kind)
  );
}

function composeFlavor(host: DockerHost): dc.ComposeFlavor | null {
  return host.compose.kind === "missing" ? null : host.compose;
}

function resolveComposePath(
  host: DockerHost,
  workDir: string,
  rel: string
): { abs: string; dir: string; rel: string } {
  if (!dc.isSafeComposeRel(rel)) {
    throw new DockerError("command-failed", "compose 文件路径不合法");
  }
  const abs = resolveInside(host.kind, workDir, rel);
  return { abs, dir: dirnameOf(host.kind, abs), rel };
}

async function composePs(
  host: DockerHost,
  flavor: dc.ComposeFlavor,
  file: string,
  dir: string
): Promise<{ services: DockerComposeService[]; error?: string }> {
  const res = await runDocker(host, dc.composePsArgs(host.docker, flavor, file, dir), {
    timeoutMs: TIMEOUT_READ,
  });
  if (res.code !== 0) {
    return { services: [], error: dc.errorDetail(res.stderr, res.stdout, res.code) };
  }
  return { services: dc.parseComposePs(res.stdout) };
}

/**
 * 右侧 Docker 面板快照。docker 本身不可用时 available=false；compose 缺失
 * 只把 composeAvailable 打 false，容器 / 镜像照样返回。
 */
export async function describeDocker(
  host: DockerHost,
  workDir: string | undefined,
  composeFile?: string
): Promise<DockerSnapshot> {
  const ps = await runDocker(host, dc.psArgs(host.docker), { timeoutMs: TIMEOUT_READ });
  if (ps.code !== 0) {
    const err = failEnv(ps);
    return unavailableSnapshot(err.reason, err.detail ?? err.message);
  }

  const imagesP = runDocker(host, dc.imagesArgs(host.docker), { timeoutMs: TIMEOUT_READ });
  const filesP = workDir ? listComposeFiles(host, workDir) : Promise.resolve([]);
  const [imagesRes, composeFiles] = await Promise.all([imagesP, filesP]);
  const images = imagesRes.code === 0 ? dc.parseImagesJson(imagesRes.stdout) : [];

  const flavor = composeFlavor(host);
  const snap: DockerSnapshot = {
    available: true,
    containers: dc.parsePsJson(ps.stdout),
    images,
    composeFiles,
    composeAvailable: flavor != null,
    composeReason: flavor ? undefined : "compose-missing",
    composeDetail: flavor ? undefined : dockerFailureText("compose-missing"),
  };

  if (!flavor || !workDir || composeFiles.length === 0) return snap;

  const wanted =
    composeFile && dc.isSafeComposeRel(composeFile)
      ? composeFile
      : composeFiles[0]!.path;
  try {
    const resolved = resolveComposePath(host, workDir, wanted);
    const { services, error } = await composePs(host, flavor, resolved.abs, resolved.dir);
    snap.compose = { file: resolved.rel, services };
    if (error) {
      snap.composeDetail = error;
    }
  } catch (err) {
    snap.composeDetail = (err as Error).message;
  }
  return snap;
}

export async function containerLogs(
  host: DockerHost,
  ref: string,
  tail: number
): Promise<DockerLogs> {
  if (!dc.isSafeDockerRef(ref)) return unavailableLogs("command-failed", "容器引用不合法");
  const res = await runDocker(host, dc.logsArgs(host.docker, ref, dc.clampTail(tail)), {
    timeoutMs: TIMEOUT_LOGS,
  });
  // docker logs 把日志打在 stderr（stdout 是容器自己的 stdout，stderr 是容器 stderr），
  // 两边都要收。非零码才是失败（容器不存在之类）。
  if (res.code !== 0) {
    const err = failEnv(res);
    return unavailableLogs(err.reason, err.detail ?? err.message);
  }
  const { text, truncated } = dc.truncateLogs(`${res.stdout}${res.stderr}`);
  return { available: true, text, truncated };
}

export async function composeLogs(
  host: DockerHost,
  workDir: string,
  file: string,
  tail: number
): Promise<DockerLogs> {
  const flavor = composeFlavor(host);
  if (!flavor) return unavailableLogs("compose-missing", dockerFailureText("compose-missing"));
  let resolved;
  try {
    resolved = resolveComposePath(host, workDir, file);
  } catch (err) {
    return unavailableLogs("command-failed", (err as Error).message);
  }
  const res = await runDocker(
    host,
    dc.composeLogsArgs(host.docker, flavor, resolved.abs, resolved.dir, dc.clampTail(tail)),
    { timeoutMs: TIMEOUT_LOGS }
  );
  if (res.code !== 0) {
    const err = failEnv(res);
    return unavailableLogs(err.reason, err.detail ?? err.message);
  }
  const { text, truncated } = dc.truncateLogs(`${res.stdout}${res.stderr}`);
  return { available: true, text, truncated };
}

export async function runDockerOp(
  host: DockerHost,
  workDir: string | undefined,
  input: DockerOpInput
): Promise<DockerOpResult> {
  switch (input.op) {
    case "start":
      return finish(await runDocker(host, dc.startArgs(host.docker, input.ref), { timeoutMs: TIMEOUT_MUTATE }));
    case "stop":
      return finish(await runDocker(host, dc.stopArgs(host.docker, input.ref), { timeoutMs: TIMEOUT_MUTATE }));
    case "restart":
      return finish(await runDocker(host, dc.restartArgs(host.docker, input.ref), { timeoutMs: TIMEOUT_MUTATE }));
    case "remove":
      return finish(
        await runDocker(host, dc.rmArgs(host.docker, input.ref, input.force === true), {
          timeoutMs: TIMEOUT_MUTATE,
        })
      );
    case "image-remove":
      return finish(await runDocker(host, dc.rmiArgs(host.docker, input.ref), { timeoutMs: TIMEOUT_MUTATE }));
    case "image-prune":
      return finish(await runDocker(host, dc.imagePruneArgs(host.docker), { timeoutMs: TIMEOUT_PRUNE }));
    case "compose-up":
    case "compose-down": {
      const flavor = composeFlavor(host);
      if (!flavor) {
        return { ok: false, reason: "compose-missing", detail: dockerFailureText("compose-missing") };
      }
      if (!workDir) {
        return { ok: false, reason: "no-working-dir", detail: dockerFailureText("no-working-dir") };
      }
      let resolved;
      try {
        resolved = resolveComposePath(host, workDir, input.file);
      } catch (err) {
        return { ok: false, reason: "command-failed", detail: (err as Error).message };
      }
      const argv =
        input.op === "compose-up"
          ? dc.composeUpArgs(host.docker, flavor, resolved.abs, resolved.dir)
          : dc.composeDownArgs(host.docker, flavor, resolved.abs, resolved.dir);
      const timeoutMs = input.op === "compose-up" ? TIMEOUT_COMPOSE_UP : TIMEOUT_MUTATE;
      return finish(await runDocker(host, argv, { timeoutMs }));
    }
  }
}

function finish(res: ExecResult): DockerOpResult {
  return res.code === 0 ? opOk(res) : opFail(res);
}
