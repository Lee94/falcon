/**
 * 宿主机可用 shell 侦测。
 *
 * 项目表单里的 shell 覆盖从"盲填路径"升级为"从侦测结果里选"：一次往返扫一遍
 * 常见 shell，存在的以绝对路径返回。`default` 与会话创建路径的取值严格一致——
 * POSIX 是探测到的登录 shell（见 POSIX_PROBE），Windows 一律 PowerShell
 * （见 WINDOWS_PROBE_SCRIPT / defaultLocalShell）——选"默认"就等于不设覆盖。
 */

import type { ShellsInfo } from "@falcon/shared";
import { encodePowerShell, type HostKind } from "./zellij/host.js";
import type { ExecFn } from "./zellij/install.js";

/** 常见 POSIX shell。bash/zsh/fish 覆盖绝大多数；pwsh 是跨平台 PowerShell */
const POSIX_CANDIDATES = ["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "nu", "pwsh"];

/**
 * Windows 候选。powershell 排最前——它是 Windows 宿主机的默认；
 * bash.exe 通常来自 Git for Windows，nu/pwsh 是用户自装的。
 */
const WINDOWS_CANDIDATES = ["powershell.exe", "pwsh.exe", "cmd.exe", "nu.exe", "bash.exe"];

/**
 * 用分号串接的 `command -v` 而不是 for 循环：这条命令由远端登录 shell 执行，
 * `;` 与 `command -v` 在 sh/bash/zsh/fish 里语义一致，for 的语法则各不相同。
 * 缺某个 shell 时该条 command -v 静默失败，所以**不看退出码**，只解析 stdout。
 */
export const POSIX_SHELLS_PROBE =
  POSIX_CANDIDATES.map((s) => `command -v ${s}`).join("; ") + "; true";

/** -CommandType Application：只要真实可执行文件，别把别名/函数当 shell 报出来 */
export const WINDOWS_SHELLS_SCRIPT = [
  `foreach ($n in ${WINDOWS_CANDIDATES.map((s) => `'${s}'`).join(",")}) {`,
  `$c = Get-Command $n -CommandType Application -ErrorAction SilentlyContinue;`,
  `if ($c) { @($c)[0].Source }`,
  `}`,
].join(" ");

export const WINDOWS_SHELLS_PROBE = encodePowerShell(WINDOWS_SHELLS_SCRIPT);

/**
 * 解析探测输出。POSIX 只认绝对路径行——`command -v` 对别名/函数可能吐出
 * 定义体而非路径；Windows 的 Get-Command .Source 恒为绝对路径，直接收。
 */
export function parseShellList(kind: HostKind, stdout: string): string[] {
  const lines = stdout
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (kind === "windows") return lines.filter((l) => /^([A-Za-z]:\\|\\\\)/.test(l));
  return lines.filter((l) => l.startsWith("/"));
}

/**
 * 把默认 shell 归入侦测结果并排到最前。
 *
 * Windows 上默认值可能是裸名字（本地的 defaultLocalShell 返回 "powershell.exe"）
 * 或与侦测结果大小写不同：按 basename 不区分大小写匹配，命中就用侦测到的
 * 绝对路径当默认——下拉里不该同时出现 "powershell.exe" 和它的绝对路径。
 */
export function mergeShells(
  kind: HostKind,
  defaultShell: string,
  found: string[]
): ShellsInfo {
  const norm = (s: string) => (kind === "windows" ? s.toLowerCase() : s);
  const base = (s: string) => s.split(/[\\/]/).pop() ?? s;

  let def = defaultShell;
  if (kind === "windows") {
    const hit = found.find((f) => norm(base(f)) === norm(base(defaultShell)));
    if (hit) def = hit;
  }

  const seen = new Set<string>([norm(def)]);
  const shells = [def];
  for (const f of found) {
    if (seen.has(norm(f))) continue;
    seen.add(norm(f));
    shells.push(f);
  }
  return { kind, default: def, shells };
}

/**
 * 侦测候选之外也常见的交互 shell。isShellCommand 的判定集合要比表单候选宽：
 * 表单漏列一个 shell 只是下拉里少一项，这里漏一个则每次关 tab 都误弹确认。
 */
const EXTRA_SHELLS = ["csh", "mksh", "ash", "elvish", "xonsh", "nushell", "powershell"];

/**
 * 判断一条前台命令是不是"就是 shell 自己在等输入"——关 tab 要不要拦的依据。
 *
 * basename 是已知 shell（或该会话配置的 shell）才算空闲：带参数的如
 * `bash deploy.sh` 是在跑东西，得拦；只跟登录 / 交互标志的 `/bin/zsh -l` 不是
 * ——agent 会话的启动脚本 exec 出来就长这样（sessions/agent.ts），CLI 退出后
 * 落回的就是它。登录 shell 的 `-zsh` 前缀与 Windows 的 `.exe` 后缀都要归一化掉。
 * 解析不出名字时按空闲放行——这套侦测是道保险，宁可放过不可把关 tab 变成每次两步。
 */
export function isShellCommand(command: string, sessionShell?: string | null): boolean {
  const trimmed = command.trim();
  if (!trimmed) return true;
  const norm = (s: string) =>
    (s.split(/[\\/]/).pop() ?? s).replace(/^-/, "").replace(/\.exe$/i, "").toLowerCase();
  const known = new Set(
    [...POSIX_CANDIDATES, ...WINDOWS_CANDIDATES, ...EXTRA_SHELLS].map(norm)
  );
  const isShell = (s: string) => {
    const base = norm(s);
    if (!base) return true;
    if (known.has(base)) return true;
    return sessionShell != null && norm(sessionShell) === base;
  };
  // Windows 全路径可能含空格（C:\Program Files\...\pwsh.exe），按词拆会误判成
  // "带参数"。整串以 .exe 结尾时先按整条路径试——这只可能放行 shell，
  // 不会把 `less /bin/bash` 这种真在跑的命令看漏。
  if (/\.exe$/i.test(trimmed) && isShell(trimmed)) return true;
  const tokens = trimmed.split(/\s+/);
  const first = tokens[0];
  if (first === undefined || !isShell(first)) return false;
  // `-l` / `--login` / `-i` / 组合的 `-il` 都只是"这个 shell 是登录 / 交互的"，
  // 不是在跑脚本
  return tokens.slice(1).every((a) => /^--(?:login|interactive)$|^-[li]+$/.test(a));
}

/** 一次往返侦测宿主机上可用的 shell。探测失败不抛：至少还有默认项可选。 */
export async function detectShells(
  exec: ExecFn,
  kind: HostKind,
  defaultShell: string
): Promise<ShellsInfo> {
  const probe = kind === "windows" ? WINDOWS_SHELLS_PROBE : POSIX_SHELLS_PROBE;
  const res = await exec(probe).catch(() => null);
  const found = res ? parseShellList(kind, res.stdout) : [];
  return mergeShells(kind, defaultShell, found);
}
