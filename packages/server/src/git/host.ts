/**
 * 取得一个可以在宿主机上跑 git 的执行环境。
 *
 * 本地与 SSH 归一到同一个 GitHost：一套命令构造、一套错误分类、一套护栏。
 * 这与 zellij 那边"本地复用与远端完全相同的安装编排"是同一个取舍。
 */

import type { ProjectRow } from "../db.js";
import type { SessionManager } from "../sessions/manager.js";
import { encodePowerShell, type HostKind } from "../zellij/host.js";
import { homeDir, localExec, localKind } from "../zellij/exec.js";
import type { ExecFn } from "../zellij/install.js";
import { WorktreeError, worktreeFailureText } from "./error.js";

export interface GitHost {
  exec: ExecFn;
  kind: HostKind;
  /** git 的绝对路径（探测所得，见 resolveGit） */
  git: string;
  /** 宿主机家目录。删除护栏用它挡住"删到 home 头上" */
  home: string;
  /** 宿主机标识：缓存与并发锁的键 */
  key: string;
}

/** 宿主机标识。注意 SSH 按 user@host:port，不按 projectId——同一台机器共用一份探测结果 */
export function hostKeyOf(row: ProjectRow): string {
  if (row.type === "local") return "local";
  return `ssh:${row.ssh_username}@${row.ssh_host}:${row.ssh_port ?? 22}`;
}

/**
 * git 绝对路径的缓存，按宿主机标识。
 *
 * 探测一次就够，与 SshLink.probe() 的做法同构。缓存的是绝对路径而不是"能不能用"：
 * 之后所有调用都按绝对路径走，不再套登录 shell（见 resolveGit）。
 */
const gitPathCache = new Map<string, string>();

/** 用户改了主机配置时清掉缓存（目前只有测试与手动重试会用到） */
export function resetGitProbe(key?: string) {
  if (key) gitPathCache.delete(key);
  else gitPathCache.clear();
}

/**
 * 探测 git 的绝对路径。
 *
 * POSIX 侧必须套一层**登录 shell**：SSH exec 拿到的是非登录、非交互 shell，
 * `/etc/profile`、`~/.profile`、`~/.bash_profile` 里的 PATH 一概不读——而
 * Homebrew(/opt/homebrew/bin)、asdf、nix、~/.local/bin 恰恰大多写在那里。
 * 不套这一层，用户会发现"终端里 git 用得好好的，功能却说没装 git"，
 * 与 zellij/host.ts 里 buildPtyCommandLine 注释描述的是同一类症状。
 *
 * 但只在**探测**时套：拿到绝对路径之后一律直接调用，这样 stdout 干净（profile 里的
 * echo 不会混进 git 输出）、快（不必每条命令都重跑一遍 profile）。
 */
async function resolveGit(key: string, exec: ExecFn, kind: HostKind): Promise<string> {
  const cached = gitPathCache.get(key);
  if (cached) return cached;

  const probes =
    kind === "windows"
      ? [encodePowerShell(`$c = Get-Command git.exe -EA SilentlyContinue; if ($c) { $c.Source }`)]
      : // 先按登录 shell 找；万一宿主机没有可用的登录 shell，退回裸探测
        [`sh -l -c 'command -v git' 2>/dev/null`, `command -v git`];

  let detail: string | undefined;
  for (const probe of probes) {
    let res;
    try {
      res = await exec(probe);
    } catch (err) {
      throw new WorktreeError(
        "link-failed",
        worktreeFailureText("link-failed"),
        (err as Error).message
      );
    }
    // 登录 shell 里的 profile 可能往 stdout 打东西，取最后一行非空输出
    const found = res.stdout
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter((l) => l.length > 0)
      .pop();
    if (res.code === 0 && found) {
      gitPathCache.set(key, found);
      return found;
    }
    detail ??= res.stderr.trim() || undefined;
  }

  throw new WorktreeError("git-missing", worktreeFailureText("git-missing"), detail);
}

/**
 * 为一个项目取得 GitHost。
 *
 * SSH 侧复用 SessionManager 已有的链路（按 projectId 缓存），不另开连接；
 * 宿主机类型与 home 来自 SshLink 的探测缓存，重复调用不产生额外往返。
 */
export async function gitHostFor(
  row: ProjectRow,
  manager: SessionManager
): Promise<GitHost> {
  const key = hostKeyOf(row);
  if (row.type === "local") {
    const kind = localKind();
    return { exec: localExec, kind, git: await resolveGit(key, localExec, kind), home: homeDir(), key };
  }
  const link = manager.getLink(row);
  let facts: { kind: HostKind; home: string };
  try {
    facts = await link.hostFacts();
  } catch (err) {
    throw new WorktreeError(
      "link-failed",
      worktreeFailureText("link-failed"),
      (err as Error).message
    );
  }
  return {
    exec: link.exec,
    kind: facts.kind,
    git: await resolveGit(key, link.exec, facts.kind),
    home: facts.home,
    key,
  };
}
