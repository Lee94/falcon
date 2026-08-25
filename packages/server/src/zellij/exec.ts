/**
 * 本地宿主的命令执行与探测。
 *
 * 本地复用与远端完全相同的安装编排（ensureZellij）：只要提供一个把命令行
 * 交给本地 shell 的 ExecFn 即可。代价是本地下载也依赖 curl/wget——但换来的是
 * 一套逻辑、一套失败分类、一套 UI 状态，而"跑得起 Node 却没有 curl"的机器很罕见，
 * 真遇上了也有手动预置这条路。
 */

import { spawn } from "node:child_process";
import os from "node:os";
import type { ExecFn, ExecResult } from "./install.js";
import { encodePowerShell, type HostKind } from "./host.js";

export function localKind(): HostKind {
  return process.platform === "win32" ? "windows" : "posix";
}

/**
 * 经本地 shell 执行。Windows 上 Node 会用 cmd.exe 承接，而我们发的是
 * `powershell ... -EncodedCommand <base64>`——base64 里没有 cmd 会改写的字符，
 * 所以外层是 cmd 还是 PowerShell 都无所谓。
 */
export const localExec: ExecFn = (commandLine, signal) =>
  new Promise<ExecResult>((resolve) => {
    // 攒 Buffer、结束时一次解码：逐 chunk toString 会切坏跨包的 UTF-8 多字节字符
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    const out = () => ({
      stdout: Buffer.concat(stdout).toString("utf8"),
      stderr: Buffer.concat(stderr).toString("utf8"),
    });
    const proc = spawn(commandLine, {
      shell: true,
      signal,
      windowsHide: true,
    });
    proc.stdout?.on("data", (d: Buffer) => stdout.push(d));
    proc.stderr?.on("data", (d: Buffer) => stderr.push(d));
    proc.on("error", (err) => resolve({ code: null, stdout: out().stdout, stderr: String(err) }));
    proc.on("close", (code) => resolve({ code, ...out() }));
  });

/** 本地是否有可用的下载工具 */
export async function localDownloader(): Promise<"curl" | "wget" | "none"> {
  const probe =
    localKind() === "windows"
      ? "where curl.exe"
      : "command -v curl || command -v wget";
  const res = await localExec(probe);
  if (res.code !== 0) return "none";
  const out = res.stdout.toLowerCase();
  if (out.includes("curl")) return "curl";
  if (out.includes("wget")) return "wget";
  return "none";
}

/** Windows 10 1803 起内置 tar.exe（bsdtar，能解 zip） */
export async function localHasTar(): Promise<boolean> {
  if (localKind() !== "windows") return true;
  return (await localExec("where tar.exe")).code === 0;
}

/**
 * 后端进程是否处于 Job Object 中。
 *
 * Zellij 在 Windows 上还没有让 server 脱离父进程 Job 的能力
 * （PR #5195 至今未合并），所以后端若身处 Job 中，后端一退出 Zellij server
 * 会被连坐杀掉——持久性直接归零，而且是**静默**失效：会话建得成、UI 显示"持久"、
 * 用户把长任务放进去，后端重启后全没了。这比诚实标注"非持久"更坏。
 *
 * 用 PowerShell 的 P/Invoke 问一次 IsProcessInJob，避免为一个布尔值引入 FFI
 * 依赖或需要跨架构预编译的原生模块。每个后端进程只跑一次，结果缓存。
 */
let inJobCache: boolean | null = null;

export async function isProcessInJob(): Promise<boolean> {
  if (process.platform !== "win32") return false;
  if (inJobCache !== null) return inJobCache;

  const script = [
    "$sig = '[DllImport(\"kernel32.dll\", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr h, IntPtr job, out bool result);'",
    "$k = Add-Type -MemberDefinition $sig -Name Win32Job -Namespace FalconNative -PassThru",
    "$r = $false",
    "$ok = $k::IsProcessInJob([System.Diagnostics.Process]::GetCurrentProcess().Handle, [IntPtr]::Zero, [ref]$r)",
    "if ($ok) { if ($r) { 'yes' } else { 'no' } } else { 'unknown' }",
  ].join("; ");

  const res = await localExec(encodePowerShell(script));

  // 注意这条 PowerShell 查的是它自己的进程，而不是 falcon 后端进程——
  // 但子进程默认继承父进程的 Job，所以"子进程在 Job 中"等价于"后端在 Job 中"。
  // 查不出来时按最坏情况处理：宁可标非持久，也不要骗用户。
  const out = res.stdout.trim();
  inJobCache = out !== "no";
  return inJobCache;
}

export function homeDir(): string {
  return os.homedir();
}
