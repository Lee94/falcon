/**
 * 本地 PTY 的基底环境：login shell 环境解析 + locale 兜底。
 *
 * 背景：PTY 环境此前直接摊开后端进程的 process.env，而 process.env 取决于
 * 后端被谁启动——launchd / IDE / 精简 shell 启动时 PATH 往往缺
 * /opt/homebrew/bin 等，且完全没有 LANG（GUI / 服务进程的常态）。pane 里的
 * shell 又是非 login 方式起的（Zellij 的 --default-shell 是 PathBuf，带不了
 * -l；非持久会话也是 `pty.spawn(shell, [])`），不会执行 ~/.zprofile 与
 * /etc/zprofile（path_helper），PATH 无从自我修复；没有 UTF-8 locale 时
 * zsh 的行编辑把中文按单字节回显，输入中文即乱码。
 *
 * 做法学 VS Code 的 shell environment resolution：起一个 login + interactive
 * 的 shell 跑 env，拿它的环境覆盖到 process.env 上作为本地 PTY 的基底；
 * 合并后仍无 locale 时按平台探测出一个 UTF-8 locale 注入 LANG。
 * 任一环节失败都静默退回 process.env——这套是增强，不是门禁。
 *
 * 每个后端进程只解析一次，结果缓存（与 prepareLocalZellij 同节奏）。代价：
 * 用户改了 rc 文件要重启后端才生效——和改了 rc 要重开终端窗口是同一个心智。
 * 注意持久会话的 env 在 Zellij server 首次 attach 时就冻住了，本模块的修正
 * 只影响之后新建的会话，已存在的持久会话要删掉重建。
 */

import { spawn } from "node:child_process";
import os from "node:os";
import { localExec } from "../zellij/exec.js";
import type { ExecFn } from "../zellij/install.js";

export const LOGIN_ENV_BEGIN = "__FALCON_LOGIN_ENV_BEGIN__";
export const LOGIN_ENV_END = "__FALCON_LOGIN_ENV_END__";

/**
 * 探测命令的 argv（跟在 shell 路径后面）。
 *
 * - `-i` 有意为之：不少人把 PATH / nvm 写在 .zshrc 且用 `[[ $- == *i* ]]`
 *   守卫，纯 login 非交互会漏掉它们。
 * - 分开传 `-i -l -c` 而不是合并 `-ilc`：短参合并的支持各 shell 不一。
 * - `env -0` 用 NUL 分隔，值里的换行不会破坏解析；`||` 兜住不认 -0 的老
 *   平台退回按行输出。stderr 不参与解析所以**不加** 2>/dev/null——那是
 *   POSIX 语法，会把 csh 系的整条命令炸掉。
 * - 前后 echo 标记：交互 rc（greeting、nvm 提示）会污染 stdout，zlogout
 *   还可能在命令之后再打一段，只取两个标记之间的部分。
 * - tcsh（-l 必须单独出现）、老 nushell（不认 ||）等会整条失败——探测失败
 *   只是退回 process.env，不用为它们做特化。
 */
export function loginEnvProbeArgs(): string[] {
  return [
    "-i",
    "-l",
    "-c",
    `echo ${LOGIN_ENV_BEGIN}; /usr/bin/env -0 || /usr/bin/env; echo ${LOGIN_ENV_END}`,
  ];
}

/**
 * 探测用的登录 shell。优先 passwd 条目而不是 $SHELL：launchd / 服务环境里
 * SHELL 往往缺失，process.env.SHELL 缺时 defaultLocalShell 会退到 /bin/bash，
 * 而 zsh 用户的 PATH 都写在 zsh 的 rc 里——用错 shell 探出来的环境是空的。
 * userInfo() 在极端环境（容器里无 passwd 条目）会抛，兜住。
 */
export function loginProbeShell(): string {
  let pw: string | undefined;
  try {
    pw = os.userInfo().shell || undefined;
  } catch {
    // 读不到 passwd 就顺延下一级
  }
  return pw || process.env.SHELL || "/bin/bash";
}

/**
 * 从探测 stdout 里解析 env。标记缺失或没解出任何变量时返回 null。
 * begin 取第一次出现、end 取最后一次：rc 输出在前、zlogout 输出在后。
 */
export function parseLoginEnv(stdout: string): Record<string, string> | null {
  const begin = stdout.indexOf(LOGIN_ENV_BEGIN);
  if (begin < 0) return null;
  const start = begin + LOGIN_ENV_BEGIN.length;
  const end = stdout.lastIndexOf(LOGIN_ENV_END);
  if (end <= start) return null;

  let body = stdout.slice(start, end);
  // 去掉 begin 标记 echo 补的换行；env 输出自身以分隔符（\0 或 \n）结尾
  if (body.startsWith("\n")) body = body.slice(1);

  const env: Record<string, string> = {};
  if (body.includes("\0")) {
    // env -0：NUL 分隔，值可以含换行
    for (const entry of body.split("\0")) {
      if (!entry) continue;
      const eq = entry.indexOf("=");
      if (eq <= 0) continue;
      env[entry.slice(0, eq)] = entry.slice(eq + 1);
    }
  } else {
    // 退化的按行输出：不像 `KEY=` 开头的行当作上一个值的续行
    const lines = body.split("\n");
    if (lines[lines.length - 1] === "") lines.pop();
    let key: string | null = null;
    for (const line of lines) {
      const m = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(line);
      if (m) {
        key = m[1]!;
        env[key] = m[2]!;
      } else if (key) {
        env[key] += "\n" + line;
      }
    }
  }
  return Object.keys(env).length > 0 ? env : null;
}

/**
 * login env 里对新 PTY 无意义或有害的键，合并时丢弃：
 * SHLVL/PWD/OLDPWD/_ 是探测 shell 自己的运行痕迹，
 * FALCON_RESOLVING_ENV 是我们打给 rc 的标记（见 runLoginShell）。
 */
const PROBE_NOISE = new Set(["SHLVL", "PWD", "OLDPWD", "_", "FALCON_RESOLVING_ENV"]);

function hasLocale(env: Record<string, string | undefined>): boolean {
  return Boolean(env.LANG || env.LC_ALL || env.LC_CTYPE);
}

/**
 * 组合基底环境：process.env 打底，login env 覆盖（PATH / LANG 以登录环境
 * 为准，这正是解析的目的；只存在于 process.env 的键——比如用户
 * `FOO=bar falcon` 启动时注入的——原样保留）。合并后仍无任何 locale 线索
 * 才注入 fallbackLang；LANG=C 这类显式设置视为用户的选择，不动。
 */
export function mergeBaseEnv(
  processEnv: Record<string, string | undefined>,
  loginEnv: Record<string, string> | null,
  fallbackLang: string | null
): Record<string, string | undefined> {
  const merged: Record<string, string | undefined> = { ...processEnv };
  if (loginEnv) {
    for (const [k, v] of Object.entries(loginEnv)) {
      if (PROBE_NOISE.has(k)) continue;
      merged[k] = v;
    }
  }
  if (fallbackLang && !hasLocale(merged)) merged.LANG = fallbackLang;
  return merged;
}

/**
 * AppleLocale → POSIX locale 名。形如 `zh_CN`、`en_US`，还可能带脚本段
 * （`zh-Hans_CN`）或区域修饰（`zh_CN@rg=uszzzz`），只取 语言_地区。
 */
export function appleLocaleToPosix(raw: string): string | null {
  const m = /^([a-z]{2,3})(?:-[A-Za-z]+)?_([A-Z]{2})/.exec(raw.trim());
  return m ? `${m[1]}_${m[2]}` : null;
}

/**
 * 在 `locale -a` 的输出里找第一个可用候选。返回列表里的原始拼写：
 * macOS 列出的是 `zh_CN.UTF-8`，glibc 列出的是 `zh_CN.utf8`，
 * 两种拼法系统各自都认，但用列表原文最稳。
 */
export function pickUtf8Locale(
  candidates: (string | null)[],
  localeListing: string
): string | null {
  const norm = (s: string) => s.trim().toLowerCase().replace("utf-8", "utf8");
  const available = new Map<string, string>();
  for (const line of localeListing.split("\n")) {
    const t = line.trim();
    if (t) available.set(norm(t), t);
  }
  for (const c of candidates) {
    if (!c) continue;
    const hit = available.get(norm(c));
    if (hit) return hit;
  }
  return null;
}

/**
 * 挑一个用于 LANG 兜底的 UTF-8 locale。
 *
 * macOS 学 Terminal.app：从系统区域设置（AppleLocale）推导——Terminal 里
 * 用户看到的 LANG 本来就是它注入的，login shell 解析拿不到（这正是 locale
 * 兜底存在的原因）；推不出或不可用时退 en_US.UTF-8（macOS 恒有）。
 * Linux 首选 C.UTF-8（glibc ≥ 2.35 恒有，更老的发行版也普遍预置），
 * 语言中性、不赌 locale-gen 生成过什么。`locale -a` 本身失败时按平台
 * 直接给硬值——错杀（系统真没有该 locale）的后果只是 setlocale 失败退回
 * C，不会比不注入更糟。
 */
export async function detectFallbackLang(
  exec: ExecFn,
  platform: NodeJS.Platform = process.platform
): Promise<string | null> {
  const candidates: (string | null)[] = [];
  if (platform === "darwin") {
    const res = await exec("defaults read -g AppleLocale");
    const posix = res.code === 0 ? appleLocaleToPosix(res.stdout) : null;
    candidates.push(posix ? `${posix}.UTF-8` : null, "en_US.UTF-8");
  } else {
    candidates.push("C.UTF-8", "en_US.UTF-8");
  }

  const listing = await exec("locale -a");
  const picked = pickUtf8Locale(candidates, listing.code === 0 ? listing.stdout : "");
  if (picked) return picked;
  return platform === "darwin" ? "en_US.UTF-8" : "C.UTF-8";
}

/** rc 疯狂输出（死循环、二进制喷屏）时的止损上限 */
const PROBE_OUTPUT_CAP = 8 * 1024 * 1024;

/**
 * 跑一次 login shell 探测，拿 stdout。任何失败（spawn 报错、超时）都返回
 * null；退出码不看——rc 末尾一句 `exit 1` 或失败的命令不该否定已经打印
 * 出来的 env，标记在不在由 parseLoginEnv 判断。
 *
 * FALCON_RESOLVING_ENV=1 打给用户的 rc：想跳过重活（nvm、耗时的补全初始化）
 * 的可以用它判断，等价于 VS Code 的 VSCODE_RESOLVING_ENVIRONMENT。
 */
function runLoginShell(shell: string, timeoutMs: number): Promise<string | null> {
  return new Promise((resolve) => {
    // 攒 Buffer、结束时一次解码：逐 chunk toString 会切坏跨包的 UTF-8 多字节字符
    const chunks: Buffer[] = [];
    let size = 0;
    let proc: ReturnType<typeof spawn>;
    try {
      proc = spawn(shell, loginEnvProbeArgs(), {
        // rc 文件普遍假设从 home 起步；也避免探测把仓库目录当 cwd 产生副作用
        cwd: os.homedir(),
        env: { ...process.env, FALCON_RESOLVING_ENV: "1" },
        stdio: ["ignore", "pipe", "ignore"],
        windowsHide: true,
      });
    } catch {
      resolve(null);
      return;
    }
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      proc.kill("SIGKILL");
    }, timeoutMs);
    timer.unref();
    proc.stdout?.on("data", (d: Buffer) => {
      size += d.length;
      if (size > PROBE_OUTPUT_CAP) {
        timedOut = true;
        proc.kill("SIGKILL");
        return;
      }
      chunks.push(d);
    });
    proc.on("error", () => {
      clearTimeout(timer);
      resolve(null);
    });
    proc.on("close", () => {
      clearTimeout(timer);
      resolve(timedOut ? null : Buffer.concat(chunks).toString("utf8"));
    });
  });
}

/** rc 慢的大头是 nvm / conda 一类初始化，实测多在 1s 内；10s 已是病态 */
const PROBE_TIMEOUT_MS = 10_000;

let baseEnvPromise: Promise<Record<string, string | undefined>> | null = null;

/**
 * 本地 PTY 的基底环境（缓存）。attachLocal 每次 await 它；index.ts 启动时
 * 预热一次，首个本地会话就不用等 login shell 起完。
 */
export function resolveLocalBaseEnv(): Promise<Record<string, string | undefined>> {
  if (!baseEnvPromise) baseEnvPromise = doResolve();
  return baseEnvPromise;
}

/** 重试用：清掉缓存的解析结果 */
export function resetLocalBaseEnv() {
  baseEnvPromise = null;
}

async function doResolve(): Promise<Record<string, string | undefined>> {
  // Windows 没有 login shell / rc 这套机制，编码走的也是 PowerShell 的路
  if (process.platform === "win32") return { ...process.env };

  // 两条探测并行：login shell 要几百毫秒，locale 探测是两条毫秒级命令
  const [stdout, fallbackLang] = await Promise.all([
    runLoginShell(loginProbeShell(), PROBE_TIMEOUT_MS),
    hasLocale(process.env) ? Promise.resolve(null) : detectFallbackLang(localExec),
  ]);
  const loginEnv = stdout ? parseLoginEnv(stdout) : null;
  return mergeBaseEnv(process.env, loginEnv, fallbackLang);
}
