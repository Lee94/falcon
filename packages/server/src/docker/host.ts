/**
 * 取得一个可以在宿主机上跑 docker 的执行环境。
 *
 * 本地与 SSH 归一到同一个 DockerHost：一套命令构造、一套错误分类。
 * SSH 侧复用 SessionManager 已有的链路，不另开连接。
 */

import type { ProjectRow } from "../db.js";
import type { SessionManager } from "../sessions/manager.js";
import { encodePowerShell, type HostKind } from "../zellij/host.js";
import { localExec, localKind } from "../zellij/exec.js";
import type { ExecFn } from "../zellij/install.js";
import { DockerError, dockerFailureText } from "./error.js";
import {
  buildDockerCommandLine,
  composeVersionArgs,
  standaloneComposeVersionArgs,
  type ComposeFlavor,
} from "./command.js";

export interface DockerHost {
  exec: ExecFn;
  kind: HostKind;
  docker: string;
  compose: ComposeFlavor | { kind: "missing" };
  key: string;
}

function hostKeyOf(row: ProjectRow): string {
  if (row.type === "local") return "local";
  return `ssh:${row.ssh_username}@${row.ssh_host}:${row.ssh_port ?? 22}`;
}

const dockerPathCache = new Map<string, string>();
const composeCache = new Map<string, ComposeFlavor | { kind: "missing" }>();

export function resetDockerProbe(key?: string) {
  if (key) {
    dockerPathCache.delete(key);
    composeCache.delete(key);
  } else {
    dockerPathCache.clear();
    composeCache.clear();
  }
}

/**
 * 探测 docker 的绝对路径。只在探测时套登录 shell——拿到绝对路径之后一律
 * 直接调用，stdout 干净、也不必每条命令重跑一遍 profile。理由同 git/host.ts。
 */
async function resolveDocker(key: string, exec: ExecFn, kind: HostKind): Promise<string> {
  const cached = dockerPathCache.get(key);
  if (cached) return cached;

  const probes =
    kind === "windows"
      ? [
          encodePowerShell(
            `$c = Get-Command docker.exe -EA SilentlyContinue; if (-not $c) { $c = Get-Command docker -EA SilentlyContinue }; if ($c) { $c.Source }`
          ),
        ]
      : [`sh -l -c 'command -v docker' 2>/dev/null`, `command -v docker`];

  let detail: string | undefined;
  for (const probe of probes) {
    let res;
    try {
      res = await exec(probe);
    } catch (err) {
      throw new DockerError("link-failed", dockerFailureText("link-failed"), (err as Error).message);
    }
    const found = res.stdout
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter((l) => l.length > 0)
      .pop();
    if (res.code === 0 && found) {
      dockerPathCache.set(key, found);
      return found;
    }
    detail ??= res.stderr.trim() || undefined;
  }

  throw new DockerError("docker-missing", dockerFailureText("docker-missing"), detail);
}

async function resolveCompose(
  key: string,
  exec: ExecFn,
  kind: HostKind,
  docker: string
): Promise<ComposeFlavor | { kind: "missing" }> {
  const cached = composeCache.get(key);
  if (cached) return cached;

  try {
    const plugin = await exec(buildDockerCommandLine(kind, composeVersionArgs(docker)));
    if (plugin.code === 0) {
      const flavor: ComposeFlavor = { kind: "plugin" };
      composeCache.set(key, flavor);
      return flavor;
    }
  } catch {
    // 探测失败不当成链路故障：docker 本身已经能用，只是 compose 可能没有
  }

  const probes =
    kind === "windows"
      ? [
          encodePowerShell(
            `$c = Get-Command docker-compose.exe -EA SilentlyContinue; if (-not $c) { $c = Get-Command docker-compose -EA SilentlyContinue }; if ($c) { $c.Source }`
          ),
        ]
      : [`sh -l -c 'command -v docker-compose' 2>/dev/null`, `command -v docker-compose`];

  for (const probe of probes) {
    try {
      const res = await exec(probe);
      const found = res.stdout
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => l.length > 0)
        .pop();
      if (res.code === 0 && found) {
        const ver = await exec(
          buildDockerCommandLine(kind, standaloneComposeVersionArgs(found))
        );
        if (ver.code === 0) {
          const flavor: ComposeFlavor = { kind: "standalone", bin: found };
          composeCache.set(key, flavor);
          return flavor;
        }
      }
    } catch {
      // 继续下一种探测
    }
  }

  const missing = { kind: "missing" as const };
  composeCache.set(key, missing);
  return missing;
}

export async function dockerHostFor(
  row: ProjectRow,
  manager: SessionManager
): Promise<DockerHost> {
  const key = hostKeyOf(row);
  if (row.type === "local") {
    const kind = localKind();
    const docker = await resolveDocker(key, localExec, kind);
    return {
      exec: localExec,
      kind,
      docker,
      compose: await resolveCompose(key, localExec, kind, docker),
      key,
    };
  }
  const link = manager.getLink(row);
  let facts: { kind: HostKind };
  try {
    facts = await link.hostFacts();
  } catch (err) {
    throw new DockerError("link-failed", dockerFailureText("link-failed"), (err as Error).message);
  }
  const docker = await resolveDocker(key, link.exec, facts.kind);
  return {
    exec: link.exec,
    kind: facts.kind,
    docker,
    compose: await resolveCompose(key, link.exec, facts.kind, docker),
    key,
  };
}
