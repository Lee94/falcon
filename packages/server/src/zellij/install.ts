/**
 * 宿主机上的 Zellij 安装编排。
 *
 * 二进制**由宿主机自己下载**，不经后端中转：不占后端带宽、不走 SFTP
 * （顺带绕开 sftp-server 被禁用的整类问题）、宿主机在海外时也不必绕道。
 * 代价是宿主机必须能出网——下不了的机器（内网、无出口）走手动预置：
 * 用户自己把二进制放到 binDir 即可，安装流程会先检查它。
 *
 * 不做完整性校验：走默认 GitHub 源时 HTTPS 已保证来源与完整性；
 * 配置了自定义 base URL 的用户会在 UI 上看到明确警告。
 */

import crypto from "node:crypto";
import { isAutoRetryable, type ZellijInstallFailure } from "@mojito/shared";
import {
  buildCommandLine,
  encodePowerShell,
  hostLayout,
  quotePosix,
  quotePowerShell,
  type HostKind,
  type HostLayout,
} from "./host.js";
import { CONFIG_BODY, LAYOUT_BODY, versionArgs, versionMatches } from "./command.js";
import { downloadUrl, type ZellijTarget } from "./version.js";

export interface ExecResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

/** 在宿主机上执行一条命令行。远端为 SSH exec，本地为经 shell 的 spawn。 */
export type ExecFn = (
  commandLine: string,
  signal?: AbortSignal
) => Promise<ExecResult>;

/** 安装阶段，用于向前端报告进度。没有百分比——宿主机自己下载，后端看不到字节数。 */
export type InstallStage = "probing" | "downloading" | "extracting" | "verifying";

/** 阶段回调。attempt 从 1 起，>1 表示这一轮是自动重试。 */
export type StageFn = (stage: InstallStage, attempt: number) => void;

/** 与前端共用同一份定义，避免两边各改一处后悄悄漂移 */
export type InstallFailure = ZellijInstallFailure;

export class InstallError extends Error {
  constructor(
    readonly reason: InstallFailure,
    message: string,
    readonly detail?: string
  ) {
    super(message);
  }
}

const FAILURE_TEXT: Record<InstallFailure, string> = {
  "arch-unsupported": "该架构没有官方 Zellij 构建",
  "no-downloader": "宿主机缺少 curl / wget",
  "no-tar": "宿主机缺少 tar",
  "dir-not-writable": "无法在宿主机家目录创建 .mojito 目录",
  "probe-failed": "无法探测宿主机（命令未跑通或系统未识别）",
  "download-failed": "下载 Zellij 失败",
  "extract-failed": "解压 Zellij 失败",
  "verify-failed": "Zellij 无法在宿主机上执行（可能是 noexec 挂载或架构不兼容）",
  cancelled: "已取消",
};

/** 单次安装请求内的自动重试次数上限（含首次尝试） */
export const MAX_ATTEMPTS = 3;

/** 各次重试前的退避。够长到熬过一次网络抖动，又不至于让用户以为卡死了。 */
const BACKOFF_MS = [1500, 4000];

export function failureText(reason: InstallFailure): string {
  return FAILURE_TEXT[reason];
}

export interface InstallOptions {
  kind: HostKind;
  /** mojito 根目录：远端为 remoteRoot(kind, home)，本地为后端的 --data-dir */
  root: string;
  target: ZellijTarget;
  /** 该主机配置的下载源，缺省为 GitHub 官方地址 */
  baseUrl?: string;
  downloader: "curl" | "wget" | "none";
  /** Windows 才需要判断；POSIX 上 tar 视为必然存在 */
  hasTar?: boolean;
  onStage?: StageFn;
  signal?: AbortSignal;
}

/**
 * 确保宿主机上有可用的 Zellij，返回其布局。
 * 已存在（含用户手动预置）则只做一次 --version 握手就返回。
 *
 * 下载/解压这一段会自动重试（见 withInstallRetry）：远端安装最常见的失败
 * 就是网络抖一下或 SSH 通道半路断开，让用户回去点重试是把机器该干的活推给人。
 */
export async function ensureZellij(
  exec: ExecFn,
  opts: InstallOptions
): Promise<HostLayout> {
  const layout = hostLayout(opts.kind, opts.root, opts.target);

  // 目录与 layout 文件先就位——二进制装没装好都需要它们
  await run(exec, ensureDirs(opts.kind, layout), "dir-not-writable", opts.signal);

  // 已装或已预置：握手通过就直接用，省掉整个下载流程
  opts.onStage?.("verifying", 1);
  if (await verify(exec, opts.kind, layout.bin, opts.signal)) return layout;

  // 前置条件不随重试改变，放在重试循环外先挡掉
  if (opts.downloader === "none") {
    throw new InstallError(
      "no-downloader",
      failureText("no-downloader"),
      `可手动将 Zellij 二进制放到 ${layout.bin}`
    );
  }
  if (opts.kind === "windows" && opts.hasTar === false) {
    throw new InstallError(
      "no-tar",
      failureText("no-tar"),
      `可手动将 Zellij 二进制放到 ${layout.bin}`
    );
  }

  const url = downloadUrl(opts.target, opts.baseUrl);
  const downloader = opts.downloader;
  return withInstallRetry(
    (attempt) => installOnce(exec, opts, layout, url, downloader, attempt),
    opts.signal
  );
}

/** 一轮完整的下载 → 解压 → 验证 → 就位。任一步失败即整轮作废，由调用方决定要不要再来。 */
async function installOnce(
  exec: ExecFn,
  opts: InstallOptions,
  layout: HostLayout,
  url: string,
  downloader: "curl" | "wget",
  attempt: number
): Promise<HostLayout> {
  const sep = opts.kind === "windows" ? "\\" : "/";
  // 每轮都用全新的临时目录：上一轮留下的半截文件绝不会被下一轮当成好包
  const tmpDir = `${layout.binDir}${sep}.tmp-${crypto.randomUUID().slice(0, 8)}`;

  try {
    opts.onStage?.("downloading", attempt);
    await run(
      exec,
      download(opts.kind, tmpDir, url, downloader),
      "download-failed",
      opts.signal
    );

    opts.onStage?.("extracting", attempt);
    await run(exec, extract(opts.kind, tmpDir), "extract-failed", opts.signal);

    // 原子替换：先在临时目录里验证，通过了才 rename 到正式路径。
    // 半截文件、并发安装都不会污染正式路径（后到者覆盖，内容相同无害）。
    opts.onStage?.("verifying", attempt);
    const staged = `${tmpDir}${sep}${opts.kind === "windows" ? "zellij.exe" : "zellij"}`;
    if (!(await verify(exec, opts.kind, staged, opts.signal, true))) {
      throw new InstallError("verify-failed", failureText("verify-failed"));
    }
    await run(exec, promote(opts.kind, staged, layout.bin), "extract-failed", opts.signal);
  } finally {
    // 不带 signal：取消时更要清干净，否则临时目录会一直留在用户机器上
    await exec(cleanup(opts.kind, tmpDir)).catch(() => {});
  }

  return layout;
}

/**
 * 有限次自动重试，只对瞬时故障生效（见 shared 的 AUTO_RETRY_FAILURES）。
 *
 * 非 InstallError 的异常一律按不可重试处理：那多半是代码问题，重试只会把它藏起来。
 * 取消（signal）在退避等待期间立即生效——用户点了取消就不该再等几秒。
 */
export async function withInstallRetry<T>(
  attemptFn: (attempt: number) => Promise<T>,
  signal?: AbortSignal
): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await attemptFn(attempt);
    } catch (err) {
      const retryable =
        err instanceof InstallError && isAutoRetryable(err.reason) && !signal?.aborted;
      if (!retryable || attempt >= MAX_ATTEMPTS) throw err;
      await sleep(BACKOFF_MS[attempt - 1] ?? BACKOFF_MS[BACKOFF_MS.length - 1]!, signal);
    }
  }
}

/** 可被 signal 打断的等待；打断时抛 cancelled，与安装流程其余部分一致 */
function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) {
    return Promise.reject(new InstallError("cancelled", failureText("cancelled")));
  }
  return new Promise<void>((resolve, reject) => {
    const done = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
    };
    const onAbort = () => {
      done();
      reject(new InstallError("cancelled", failureText("cancelled")));
    };
    const timer = setTimeout(() => {
      done();
      resolve();
    }, ms);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

async function run(
  exec: ExecFn,
  commandLine: string,
  reason: InstallFailure,
  signal?: AbortSignal
): Promise<ExecResult> {
  if (signal?.aborted) throw new InstallError("cancelled", failureText("cancelled"));
  let res: ExecResult;
  try {
    res = await exec(commandLine, signal);
  } catch (err) {
    if (signal?.aborted) throw new InstallError("cancelled", failureText("cancelled"));
    throw new InstallError(reason, failureText(reason), (err as Error).message);
  }
  if (res.code !== 0) {
    // 退出码兜底：curl/wget 在某些远端上一个字都不往 stderr 写，只留一个退出码
    // （curl 6=域名解析不了、7=连不上、22=HTTP 错误、28=超时），
    // 而"该改网络还是该换下载源"全靠这一句话。空着等于让用户去猜。
    throw new InstallError(
      reason,
      failureText(reason),
      res.stderr.trim() || res.stdout.trim() || `远端命令退出码 ${res.code}`
    );
  }
  return res;
}

/**
 * 握手验证：跑得起来且版本对得上才算数。能同时挡住 noexec、架构不符、文件截断。
 *
 * strict = 命令本身没跑起来时抛错而不是当作"验证不通过"。装完那一次必须用它：
 * 链路在握手瞬间断掉与"二进制跑不动"是两码事，前者重试有救，后者重试白搭，
 * 混成一个 false 会让用户收到一句冤枉的"noexec 挂载或架构不兼容"。
 */
async function verify(
  exec: ExecFn,
  kind: HostKind,
  bin: string,
  signal?: AbortSignal,
  strict = false
): Promise<boolean> {
  try {
    const res = await exec(buildCommandLine(kind, [bin, ...versionArgs()]), signal);
    return res.code === 0 && versionMatches(res.stdout);
  } catch (err) {
    if (!strict) return false;
    if (signal?.aborted) throw new InstallError("cancelled", failureText("cancelled"));
    throw new InstallError(
      "probe-failed",
      failureText("probe-failed"),
      `验证握手时与宿主机失去联系：${(err as Error).message}`
    );
  }
}

// ---------------- 各阶段命令 ----------------

/**
 * 下载超时。没有这几个开关，重试就是空头支票：curl 默认既不限连接时间也不管
 * 传输停滞，遇上黑洞路由（连得上、握完手、然后一个字节都不来——被墙时最常见的
 * 表现）会一直挂着不返回，失败根本不会发生，用户看到的是一个转到天荒地老的进度条。
 *
 * 连接 20 秒放弃；连上之后若 30 秒内平均速率低于 1 KiB/s 也放弃。不设总时长上限：
 * 14 MiB 在慢线路上跑十几分钟是正常的，只要还在动就不该被打断。
 *
 * wget 侧还要显式 --tries=1：它自带 20 次重试，会把我们的退避与取消全都架空。
 */
const CURL_LIMITS = "--connect-timeout 20 --speed-limit 1024 --speed-time 30";
const WGET_LIMITS = "--tries=1 --connect-timeout=20 --read-timeout=30";

/**
 * Windows 上建目录（含中间层，语义同 `mkdir -p`）。
 *
 * 这里用 `-Path` 是可以的，尽管 New-Item 没有 `-LiteralPath` 参数：实测含 `[]`
 * 的路径照样建得出来（创建路径不像 Remove-Item 那样先做通配符匹配），
 * 而 `[ ]` 是 Windows 文件名里唯一合法的通配字符（`* ?` 本就非法）。
 * **删除**那边则完全不同，必须 `-LiteralPath`——见 cleanup。
 */
function mkdirPS(dir: string): string {
  return `New-Item -ItemType Directory -Force -Path ${quotePowerShell(dir)} | Out-Null`;
}

function download(
  kind: HostKind,
  tmpDir: string,
  url: string,
  downloader: "curl" | "wget"
): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        mkdirPS(tmpDir),
        `curl.exe -fsSL ${CURL_LIMITS} ${quotePowerShell(url)} -o ${quotePowerShell(`${tmpDir}\\a.zip`)}`,
        `if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }`,
      ].join("; ")
    );
  }
  const dir = quotePosix(tmpDir);
  const out = quotePosix(`${tmpDir}/a.tar.gz`);
  const u = quotePosix(url);
  const fetch =
    downloader === "curl"
      ? `curl -fsSL ${CURL_LIMITS} ${u} -o ${out}`
      : `wget -q ${WGET_LIMITS} ${u} -O ${out}`;
  return `mkdir -p ${dir} && ${fetch}`;
}

/** 包内只有单个可执行文件（POSIX 为 zellij，Windows 为 zellij.exe） */
function extract(kind: HostKind, tmpDir: string): string {
  if (kind === "windows") {
    // tar.exe 是 bsdtar，能解 zip
    return encodePowerShell(
      [
        `tar.exe -xf ${quotePowerShell(`${tmpDir}\\a.zip`)} -C ${quotePowerShell(tmpDir)}`,
        `if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }`,
      ].join("; ")
    );
  }
  const dir = quotePosix(tmpDir);
  return `tar -xzf ${quotePosix(`${tmpDir}/a.tar.gz`)} -C ${dir} && chmod 700 ${quotePosix(`${tmpDir}/zellij`)}`;
}

function promote(kind: HostKind, staged: string, bin: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      `Move-Item -Force -LiteralPath ${quotePowerShell(staged)} -Destination ${quotePowerShell(bin)}`
    );
  }
  return `mv -f ${quotePosix(staged)} ${quotePosix(bin)}`;
}

function cleanup(kind: HostKind, tmpDir: string): string {
  if (kind === "windows") {
    // **必须 -LiteralPath**：`-Path` 会先把路径当通配符去匹配已存在的项，路径里有一个
    // `[` 就会被当成字符类而匹配不到任何东西。实测 `Remove-Item -Recurse -Force
    // -EA SilentlyContinue -Path 'C:\...\rm[1]old'` 退出码 0、目录纹丝不动——
    // 静默报成功是最坏的一种失败。远端 home 含 `[]` 并不罕见（`C:\Users\a[1]b`）。
    //
    // -EA SilentlyContinue 保留：对象是我们自己造的 .tmp-<uuid>，清不掉最多留点
    // 垃圾，不该盖住真正的安装错误（这是在 finally 里跑的）。
    return encodePowerShell(
      `Remove-Item -Recurse -Force -EA SilentlyContinue -LiteralPath ${quotePowerShell(tmpDir)}`
    );
  }
  return `rm -rf ${quotePosix(tmpDir)}`;
}

/**
 * 建目录并写入 layout 文件。
 *
 * 单独一步执行，好把"家目录不可写"跟"下载失败"区分开来报给用户。
 * 每次都跑（哪怕二进制已经装好）：成本是一次往返，换来的是用户误删
 * layout 文件后能自愈——少了它 Zellij 会直接以 IoError 退出。
 */
export function ensureDirs(kind: HostKind, layout: HostLayout): string {
  const dirs = [
    layout.binDir,
    layout.socketDir,
    layout.configDir,
    layout.dataDir,
    layout.cacheDir,
    layout.layoutDir,
  ];
  if (kind === "windows") {
    const mk = dirs.map(mkdirPS);
    mk.push(
      `Set-Content -LiteralPath ${quotePowerShell(layout.layoutFile)} -Value ${quotePowerShell(LAYOUT_BODY.trim())}`,
      `Set-Content -LiteralPath ${quotePowerShell(layout.configFile)} -Value ${quotePowerShell(CONFIG_BODY.trim())}`
    );
    return encodePowerShell(mk.join("; "));
  }
  return (
    `mkdir -p ${dirs.map(quotePosix).join(" ")} && ` +
    `printf %s ${quotePosix(LAYOUT_BODY)} > ${quotePosix(layout.layoutFile)} && ` +
    `printf %s ${quotePosix(CONFIG_BODY)} > ${quotePosix(layout.configFile)}`
  );
}
