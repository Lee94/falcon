/**
 * Agent 会话：把会话的"开场命令"从 shell 换成 claude / codex / grok 这类 CLI。
 *
 * 实现上给 Zellij / PTY 的仍然是一个 shell——只不过这个 shell 是我们写在宿主机上的
 * 一段启动脚本。不能把 CLI 本身当 shell 传下去：
 * - `--default-shell` 是 PathBuf，不接受参数（见 zellij/command.ts 的 sessionOptions），
 *   "先跑 CLI 再落回 shell"这件事只能由脚本自己完成；
 * - CLI 一退出就没人占着 pane 了，Zellij 会把 pane 连同会话一起收掉——用户按一次
 *   Ctrl-D 整个会话就没了。脚本最后 exec 真实登录 shell 正是为此。
 *
 * 脚本里的登录 shell 是**生成时写死的绝对路径**，不读 $SHELL：Windows 远端那条路径上
 * 我们自己把 SHELL 设成了传给 Zellij 的 shell（见 ssh.ts 的 WMI 兜底），脚本再去读
 * $SHELL 就会递归调用自己。
 *
 * CLI 不在 PATH 上不算错误：提示一句然后照常落回 shell——把会话开起来比报错有用，
 * 用户可以就地 `npm i -g` 装完再开一个。
 */
import fs from "node:fs";
import path from "node:path";
import { SESSION_AGENTS, type SessionAgent } from "@falcon/shared";
import { joinPath } from "../git/path.js";
import { encodePowerShell, quotePosix, quotePowerShell, type HostKind } from "../zellij/host.js";

export type { SessionAgent };
export { SESSION_AGENTS };

/** CLI 的可执行名。三个都是 npm 全局包，装完就在 PATH 上。 */
export function agentBin(agent: SessionAgent): string {
  switch (agent) {
    case "claude":
      return "claude";
    case "codex":
      return "codex";
    case "grok":
      return "grok";
  }
}

/** 启动脚本文件名。同一个 agent 在同一台宿主机上只有一份，内容幂等覆盖。 */
export function launcherName(agent: SessionAgent, kind: HostKind): string {
  return `falcon-${agent}${kind === "windows" ? ".cmd" : ".sh"}`;
}

/** 宿主机上放启动脚本的目录：<falcon 根>/agents */
export function launcherDir(kind: HostKind, root: string): string {
  return joinPath(kind, root, "agents");
}

/** 本地宿主：脚本落在后端的 --data-dir 下，与 askpass 包装同级 */
export function localLauncherDir(dataDir: string): string {
  return path.join(dataDir, "agents");
}

/**
 * POSIX 启动脚本。真正干活的是 `<shell> -l -c`：
 * 登录 shell 才有用户 ~/.profile / ~/.zshrc 里的 PATH，CLI 装在 ~/.local/bin、
 * ~/.grok/bin、nvm 的 shim 目录下的情况全靠它。
 */
function posixLauncherBody(agent: SessionAgent, shell: string): string {
  const bin = agentBin(agent);
  const inner =
    `if command -v ${bin} >/dev/null 2>&1; then ${bin}; ` +
    `else printf '%s\\n' ${quotePosix(`falcon: PATH 里找不到 ${bin}，先装好它再开这种会话。`)} >&2; fi; ` +
    `exec ${quotePosix(shell)} -l`;
  return [
    "#!/bin/sh",
    `# falcon: 以 ${bin} 开场的会话。CLI 退出后落回登录 shell，会话不跟着结束。`,
    `exec ${quotePosix(shell)} -l -c ${quotePosix(inner)}`,
    "",
  ].join("\n");
}

/**
 * Windows 启动脚本（.cmd）。提示语只用 ASCII：cmd 按 OEM 代码页读脚本文件，
 * 中文在默认 936 / 437 下必乱码。
 */
function windowsLauncherBody(agent: SessionAgent, shell: string): string {
  const bin = agentBin(agent);
  return [
    "@echo off",
    `rem falcon: session that opens with ${bin}; falls back to the shell on exit`,
    `where ${bin} >nul 2>&1`,
    "if errorlevel 1 (",
    `  echo falcon: ${bin} was not found on PATH; install it first.`,
    ") else (",
    `  call ${bin}`,
    ")",
    `"${shell}"`,
    "",
  ].join("\r\n");
}

export function launcherBody(agent: SessionAgent, kind: HostKind, shell: string): string {
  return kind === "windows"
    ? windowsLauncherBody(agent, shell)
    : posixLauncherBody(agent, shell);
}

/** 本地宿主：写脚本并返回绝对路径。幂等，每次开会话都跑（一次本地写，毫秒级） */
export function writeLocalLauncher(
  dataDir: string,
  agent: SessionAgent,
  shell: string
): string {
  const dir = localLauncherDir(dataDir);
  fs.mkdirSync(dir, { recursive: true });
  const kind: HostKind = process.platform === "win32" ? "windows" : "posix";
  const file = path.join(dir, launcherName(agent, kind));
  fs.writeFileSync(file, launcherBody(agent, kind, shell), { encoding: "utf8" });
  if (kind !== "windows") fs.chmodSync(file, 0o755);
  return file;
}

/**
 * 远端：建目录 + 写脚本的一条命令。退出码 = 链上第一个失败者。
 * Windows 走 -EncodedCommand，与其它远端命令同一套（CLAUDE.md 的约定）。
 */
export function writeRemoteLauncherCommand(
  kind: HostKind,
  dir: string,
  agent: SessionAgent,
  shell: string
): string {
  const name = launcherName(agent, kind);
  const body = launcherBody(agent, kind, shell);
  if (kind === "windows") {
    const file = joinPath(kind, dir, name);
    return encodePowerShell(
      `New-Item -ItemType Directory -Force -Path ${quotePowerShell(dir)} | Out-Null; ` +
        `Set-Content -LiteralPath ${quotePowerShell(file)} -Value ${quotePowerShell(body)}`
    );
  }
  return (
    `d=${quotePosix(dir)}; mkdir -p "$d" && ` +
    `printf %s ${quotePosix(body)} > "$d"/${name} && chmod 755 "$d"/${name}`
  );
}

/** 远端脚本的绝对路径（写入成功后交给 --default-shell） */
export function remoteLauncherPath(kind: HostKind, root: string, agent: SessionAgent): string {
  return joinPath(kind, launcherDir(kind, root), launcherName(agent, kind));
}
