/**
 * 宿主机差异：路径构造与命令行拼装。
 *
 * 四种执行环境（本地 Unix / 本地 Windows / SSH Unix 远端 / SSH Windows 远端）
 * 归结为两类 host：posix 与 windows。本地执行走 spawn 的 argv 数组、不经 shell，
 * 因此只有 SSH 远端需要把 argv 拼成命令行字符串。
 */

import type { ZellijPaths } from "./command.js";
import { binaryName, type ZellijTarget } from "./version.js";

export type HostKind = "posix" | "windows";

/** falcon 在宿主机上的全部落脚点，删掉它即完成卸载（macOS 的 Zellij cache 除外） */
const ROOT = ".falcon";
/** 产品曾名 Mojito 时的根目录。远端探测时若新目录还不在，会尝试改名过来。 */
const LEGACY_ROOT = ".mojito";

// ---------------- 路径 ----------------

export interface HostLayout extends ZellijPaths {
  /** 二进制所在目录 */
  binDir: string;
  /** falcon 根目录 */
  root: string;
  /** layout 文件所在目录 */
  layoutDir: string;
}

function joinHome(kind: HostKind, home: string, name: string): string {
  const sep = kind === "windows" ? "\\" : "/";
  return `${home.replace(/[\\/]+$/, "")}${sep}${name}`;
}

/**
 * 远端的 falcon 根目录：宿主机 home 下的 .falcon。
 *
 * 一律用探测阶段拿到的**真实 home 绝对路径**，不用 `~` 或 `$HOME`：
 * 让 shell 展开变量意味着路径不能整体加引号，含空格的用户名（Windows 上很常见）
 * 会直接把命令行拆散。
 */
export function remoteRoot(kind: HostKind, home: string): string {
  return joinHome(kind, home, ROOT);
}

export function legacyRemoteRoot(kind: HostKind, home: string): string {
  return joinHome(kind, home, LEGACY_ROOT);
}

/**
 * 把旧的 `~/.mojito` 迁到 `~/.falcon`。打印最终该用的根目录（一行）。
 *
 * 逻辑：新目录还不在而旧目录是个文件夹时尝试 `mv`；无论成败，再按「新的在就用新的，
 * 否则旧的在就用旧的，否则用新路径」选取。并发两次探测时只有一个 mv 能成功，
 * 失败者会看到新目录已经在，不会退到一个已经搬走的旧路径。
 *
 * 本机不能这么做：后端自己的 SQLite 开在数据目录里，进程还活着就 mv 会把库从
 * 自己脚下抽走。远端根目录里没有后端打开的文件，Zellij 的 socket 跟着目录一起
 * 改名，客户端按新路径 connect 仍是同一个 inode。
 */
export function posixMigrateRootScript(next: string, prev: string): string {
  const n = quotePosix(next);
  const p = quotePosix(prev);
  return (
    `n=${n}; p=${p}; ` +
    `if [ ! -e "$n" ] && [ -d "$p" ]; then mv "$p" "$n" || true; fi; ` +
    `if [ -d "$n" ]; then printf '%s\\n' "$n"; ` +
    `elif [ -d "$p" ]; then printf '%s\\n' "$p"; ` +
    `else printf '%s\\n' "$n"; fi`
  );
}

export function windowsMigrateRootScript(next: string, prev: string): string {
  const n = quotePowerShell(next);
  const p = quotePowerShell(prev);
  return (
    `$n = ${n}; $p = ${p}; ` +
    `if (-not (Test-Path -LiteralPath $n) -and (Test-Path -LiteralPath $p -PathType Container)) { ` +
    `try { Move-Item -LiteralPath $p -Destination $n -ErrorAction Stop } catch {} }; ` +
    `if (Test-Path -LiteralPath $n -PathType Container) { $n } ` +
    `elseif (Test-Path -LiteralPath $p -PathType Container) { $p } ` +
    `else { $n }`
  );
}

export function migrateRemoteRootCommand(kind: HostKind, next: string, prev: string): string {
  return kind === "windows"
    ? encodePowerShell(windowsMigrateRootScript(next, prev))
    : posixMigrateRootScript(next, prev);
}

/** 迁移脚本的 stdout：取第一行非空路径。 */
export function parseMigratedRoot(stdout: string): string | null {
  const line = stdout.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  return line || null;
}

/**
 * 基于 falcon 根目录的绝对路径构造。
 *
 * 远端的 root 是探测后解析出来的路径（优先 `<home>/.falcon`，必要时从
 * `.mojito` 迁过来），本地的 root 是后端的 `--data-dir`——本地不该硬编码
 * home，用户指定了数据目录就该落在那儿。
 */
export function hostLayout(
  kind: HostKind,
  root: string,
  target: ZellijTarget
): HostLayout {
  const sep = kind === "windows" ? "\\" : "/";
  const j = (...parts: string[]) => parts.join(sep);
  const base = root.replace(/[\\/]+$/, "");
  const zellij = j(base, "zellij");
  return {
    root: base,
    binDir: j(base, "bin"),
    bin: j(base, "bin", binaryName(target)),
    socketDir: j(zellij, "sock"),
    configDir: j(zellij, "config"),
    configFile: j(zellij, "config", "config.kdl"),
    dataDir: j(zellij, "data"),
    cacheDir: j(zellij, "cache"),
    layoutDir: j(zellij, "layouts"),
    layoutFile: j(zellij, "layouts", "falcon.kdl"),
  };
}

// ---------------- 转义 ----------------

/** POSIX sh 单引号转义：内部的单引号写成 '\'' */
export function quotePosix(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

/** PowerShell 单引号字面量：内部的单引号写成两个单引号 */
export function quotePowerShell(s: string): string {
  return `'${s.replace(/'/g, "''")}'`;
}

// ---------------- 命令行拼装 ----------------

/**
 * 拼一条可交给远端默认 shell 执行的命令。
 *
 * Windows 用 `-EncodedCommand`（UTF-16LE + base64）而不是 `-Command "..."`：
 * OpenSSH for Windows 的默认 shell 由注册表 DefaultShell 决定，可能是 cmd.exe、
 * PowerShell 5.1 或 7，三者的引号与转义规则各不相同。base64 串里只有
 * A-Za-z0-9+/= ，任何一层 shell 都不会改写它——这样我们只维护一套 PowerShell
 * 语法，且完全不受远端 DefaultShell 配置影响。
 */
export function buildCommandLine(
  kind: HostKind,
  argv: string[],
  env: Record<string, string> = {},
  unset: readonly string[] = []
): string {
  if (kind === "windows") {
    return encodePowerShell(powerShellScript(argv, env, unset));
  }
  const parts = [
    ...unset.map((k) => `-u ${k}`),
    ...Object.entries(env).map(([k, v]) => `${k}=${quotePosix(v)}`),
  ];
  const cmd = argv.map(quotePosix).join(" ");
  return parts.length ? `env ${parts.join(" ")} ${cmd}` : cmd;
}

/**
 * 生成 PowerShell 脚本片段：设环境变量后用调用运算符执行，并**如实传出退出码**。
 *
 * 退出码这件事有两个坑，都是实测出来的（`powershell -EncodedCommand` + 重定向）：
 *
 * 1. 不写 `exit` 时 PowerShell 只给 0 / 1：原生命令退 3 也会被压成 1。想区分
 *    `curl` 的 6（域名解析不了）/ 7（连不上）/ 22（HTTP 错）/ 28（超时），
 *    或 `git diff --quiet` 的 1（有改动）与 128（不是仓库），就必须自己 exit。
 *    更糟的是：只要原生命令**不是最后一条语句**，它的失败会被完全吞掉退 0。
 *
 * 2. 但裸写 `exit $LASTEXITCODE` 更危险。`& 'C:\缺失.exe'` 抛的是
 *    CommandNotFoundException——原生命令根本没跑，`$LASTEXITCODE` 从未被赋值，
 *    `exit $null` 等价于 `exit 0`，于是"命令跑不起来"被报成成功。实测确认。
 *
 * 所以预置哨兵 127（POSIX 的 command-not-found 约定）：命令跑起来了 PowerShell
 * 会用真实退出码覆盖它，没跑起来就留着 127。
 *
 * `unset` 是"删掉这些环境变量"，与 env 的"设成这个值"严格分开——**绝不能用空串
 * 代替删除**。PowerShell 里 `$env:X = ''` 恰好等价于删除，但 POSIX 侧
 * `env X='' cmd` 是"设成空串"，两者天差地别：实测 `GIT_DIR=''` 直接
 * `fatal: not a git repository: ''`，`GIT_INDEX_FILE=''` 更糟——git 会拿 `.lock`
 * 当索引，`status` 报出一堆并不存在的删除。
 */
export function powerShellScript(
  argv: string[],
  env: Record<string, string> = {},
  unset: readonly string[] = []
): string {
  const lines = [
    "$LASTEXITCODE = 127",
    ...unset.map((k) => `$env:${k} = $null`),
    ...Object.entries(env).map(([k, v]) => `$env:${k} = ${quotePowerShell(v)}`),
  ];
  const [exe, ...rest] = argv;
  lines.push([`& ${quotePowerShell(exe)}`, ...rest.map(quotePowerShell)].join(" "));
  lines.push("exit $LASTEXITCODE");
  return lines.join("; ");
}

/**
 * 把 PowerShell 脚本包成 -EncodedCommand 调用。
 *
 * 统一关掉进度流：`-NonInteractive` 下 PowerShell 会把进度记录序列化成 CLIXML
 * 写进 stderr（`Add-Type`、`Invoke-WebRequest` 之类都会触发）。stdout 不受影响，
 * 但我们在安装失败时把 stderr 当错误详情报给用户，夹带一堆 XML 就没法看了。
 *
 * 统一把输出编码钉成 UTF-8：PowerShell 按 `[Console]::OutputEncoding`（默认是
 * 系统的 OEM 代码页，简体中文机器上是 936/GBK）解码原生命令的字节，而我们两侧的
 * 读取端——`SshLink.exec` 与 `localExec`——都是 `toString("utf8")`。在 936 机器上
 * 实测：`echo 中文路径` 不设这一行时拿到的是 `d6d0cec4...`（GBK），解码出来是乱码；
 * 设了就是干净的 UTF-8。含中文的路径、`dump-screen` 抓回的 Scrollback 全靠它。
 * 重定向到管道时不会写 BOM（同样实测过），所以解析端不用防前导 ﻿。
 */
export function encodePowerShell(script: string): string {
  const full =
    `$ProgressPreference = 'SilentlyContinue'; ` +
    `[Console]::OutputEncoding = [Text.Encoding]::UTF8; ` +
    script;
  const b64 = Buffer.from(full, "utf16le").toString("base64");
  return `powershell -NoProfile -NonInteractive -EncodedCommand ${b64}`;
}

/**
 * 远端建立 PTY 会话时执行的命令。
 *
 * 与上面不同的是这条会跑在交互式 PTY 里、进程要一直活着，
 * 因此 POSIX 侧用 exec 顶掉外层 shell，少一层进程。
 *
 * `loginShell` 会把整条命令包进一个**登录 shell**（`sh -lc '...'`）。这是必须的：
 * SSH exec 拿到的是非登录、非交互 shell，只有 `/etc/profile` 与
 * `~/.profile`、`~/.bash_profile` 里的 PATH 一概不读——而 nvm、pyenv、cargo、
 * Homebrew、Go、`~/.local/bin` 恰恰大多写在那里。不套这一层的话，用户会发现
 * 终端里"很多已经装好的命令找不到"，但 `ssh` 进去却好好的。
 *
 * 套在最外层而不是只包最终 shell，是因为 Zellij server 也由这条命令拉起：
 * server 的环境会被它 spawn 的每一个 shell 继承，一次修到底。
 */
export function buildPtyCommandLine(
  kind: HostKind,
  argv: string[],
  env: Record<string, string> = {},
  loginShell?: string
): string {
  if (kind === "windows") {
    return encodePowerShell(powerShellScript(argv, env));
  }
  const assigns = Object.entries(env).map(([k, v]) => `${k}=${quotePosix(v)}`);
  const cmd = argv.map(quotePosix).join(" ");
  const inner = assigns.length ? `exec env ${assigns.join(" ")} ${cmd}` : `exec ${cmd}`;
  return loginShell
    ? `exec ${quotePosix(loginShell)} -l -c ${quotePosix(inner)}`
    : inner;
}

/**
 * 把一条命令包成"生在 sshd 进程树之外"的调用。仅 Windows 需要也仅 Windows 可用。
 *
 * Win32-OpenSSH 在 exec 通道关闭时会终结该通道的整个进程树（Job Object），而
 * Zellij 的 server 进程没有脱离父 Job 的能力（上游 PR #5195 未合并）。实测比
 * 文档说的更严酷：不用等 SSH 断开，创建会话的那条 exec 通道一关 server 就被杀。
 * 经 WMI Win32_Process.Create 拉起的进程父进程是 WmiPrvSE，完全在 sshd 的
 * Job 之外，实测能熬过整条 SSH 连接的断开与重连。
 *
 * 内层用 `-Command "<脚本>"` 而**不是** `-EncodedCommand`：后者会把内层再做一次
 * base64(UTF-16LE)，叠加外层的同样一层，整体膨胀约 7 倍——带上终端深浅那几个
 * 环境变量后，最终命令行会顶穿远端 DefaultShell（cmd.exe）8191 字符的上限，
 * 报 "The command line is too long."（实测踩到）。powerShellScript 产出的脚本
 * 全部用单引号字面量、不含双引号，可整体塞进 `-Command "..."`，再作为字符串
 * 嵌进外层的单引号（doubling 转义）——只有外层做一次编码，体积减半有余。
 *
 * 外层等内层进程退出并尽力转出退出码。两个实测出来的细节：
 * 1. Get-Process 拿到的对象要先摸一次 .Handle，进程退出后才读得到 ExitCode；
 * 2. 进程在 Get-Process 之前就退掉的话既等不到也拿不到退出码，只能按 0 处理。
 * 所以退出码只是尽力而为——调用方必须用 list-sessions 之类的事实核验结果，
 * 不能只信退出码。WMI 本身的失败（DCOM 被禁、权限不足）会如实退非零。
 *
 * `cwd` 走 Win32_Process.Create 的 CurrentDirectory 参数：不传时新进程继承
 * 调用方 WmiPrvSE 的目录（System32）。Zellij 的 `--default-cwd` 在
 * create-background 路径上会被上游丢掉（见 attachSession 处的注释），初始 pane
 * 的 cwd 退化为继承 server 进程的目录，所以必须在这里把 server 生在项目目录里。
 */
export function buildDetachedCommandLine(
  argv: string[],
  env: Record<string, string> = {},
  cwd?: string
): string {
  const innerCmd = `powershell -NoProfile -NonInteractive -Command "${powerShellScript(argv, env)}"`;
  const cimArgs = [`CommandLine = ${quotePowerShell(innerCmd)}`];
  if (cwd) cimArgs.push(`CurrentDirectory = ${quotePowerShell(cwd)}`);
  const script = [
    `$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ ${cimArgs.join("; ")} }`,
    `if ($r.ReturnValue -ne 0) { Write-Error ('Win32_Process.Create failed: ' + $r.ReturnValue); exit 1 }`,
    `$p = Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue`,
    // 60s 兜底：内层命令挂死时不能让 SSH exec 永远不返回
    `if ($p) { $null = $p.Handle; if (-not $p.WaitForExit(60000)) { exit 124 }; exit $p.ExitCode }`,
    `exit 0`,
  ].join("; ");
  return encodePowerShell(script);
}

// ---------------- 探测 ----------------

/**
 * 一次往返拿齐宿主机信息，避免为 home / 架构 / 下载工具 / shell 各发一次命令。
 * 输出四行：home、`uname -sm`、可用的下载工具、登录 shell。
 *
 * 在 Windows 远端上这条命令会失败（没有 uname），调用方据此改走 Windows 探测。
 *
 * 登录 shell 必须探测，不能指望 Zellij 自己 fallback：SSH exec 是非登录、
 * 非交互的，环境里**没有 `$SHELL`**，而实测 Zellij 在 `$SHELL` 为空时 pane
 * 根本起不来（不是文档说的退到 /bin/sh），表现为会话建成了但屏幕全空。
 */
export const POSIX_PROBE_LINES = [
  'printf "%s\\n" "$HOME"',
  "uname -sm",
  'if command -v curl >/dev/null 2>&1; then echo curl; elif command -v wget >/dev/null 2>&1; then echo wget; else echo none; fi',
  'S="$SHELL"; [ -n "$S" ] || S=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7); [ -n "$S" ] || S=/bin/sh; printf "%s\\n" "$S"',
];

export const POSIX_PROBE = POSIX_PROBE_LINES.join("; ");

export interface PosixProbe {
  home: string;
  uname: string;
  downloader: "curl" | "wget" | "none";
  shell: string;
}

export function parsePosixProbe(stdout: string): PosixProbe | null {
  const lines = stdout.split("\n").map((l) => l.trim());
  const [home, uname, downloader, shell] = lines.filter((l) => l.length > 0);
  if (!home || !uname) return null;
  return {
    home,
    uname,
    downloader:
      downloader === "curl" ? "curl" : downloader === "wget" ? "wget" : "none",
    shell: shell || "/bin/sh",
  };
}

/**
 * Windows 探测。tar.exe 自 Windows 10 1803 起内置（bsdtar，能解 zip），
 * curl.exe 同期内置；老系统上两者可能缺失，据此降级。
 */
export const WINDOWS_PROBE_SCRIPT = [
  "$env:USERPROFILE",
  "$env:PROCESSOR_ARCHITECTURE",
  "if (Get-Command curl.exe -EA SilentlyContinue) { 'curl' } else { 'none' }",
  "if (Get-Command tar.exe -EA SilentlyContinue) { 'tar' } else { 'none' }",
  // 绝对路径而非裸名字：Zellij 在 Windows 上解析 shell 名有已知问题（#4964）
  "$s = (Get-Command powershell.exe -EA SilentlyContinue).Source; if ($s) { $s } else { $env:COMSPEC }",
].join("\n");

export const WINDOWS_PROBE = encodePowerShell(WINDOWS_PROBE_SCRIPT.replace(/\n/g, "; "));

export interface WindowsProbe {
  home: string;
  arch: string;
  downloader: "curl" | "none";
  hasTar: boolean;
  shell: string;
}

export function parseWindowsProbe(stdout: string): WindowsProbe | null {
  const lines = stdout
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  const [home, arch, downloader, tar, shell] = lines;
  if (!home || !arch) return null;
  return {
    home,
    arch,
    downloader: downloader === "curl" ? "curl" : "none",
    hasTar: tar === "tar",
    shell: shell || "powershell.exe",
  };
}
