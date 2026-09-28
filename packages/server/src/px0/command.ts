/**
 * px0（ADR 0017）的资产映射、argv 与输出解析。
 *
 * 纯函数，零 I/O。本机直接 spawn argv 数组；远端只支持 POSIX，命令行在这里用
 * quotePosix 拼好交给 SshLink（Windows 宿主机 v1 不做，见 ADR）。
 *
 * 脾气（v0.1.10 实测 / 源码对过）：
 * - **没有任何鉴权**。会改东西的 POST（派 agent 编辑、commit / push、写会话、PR 评论、
 *   装语言服务器）过它的 localPost：Host 必须是 IP 或 localhost、Origin 的 host 必须
 *   等于 Host；读文件 / 搜索 / diff 这些 GET 什么都不查。只能听 127.0.0.1，前面必须
 *   挡着 falcon 的登录（反代怎么对付 localPost 见 proxy.ts）。
 * - `-port 0` 让系统挑端口；端口只能从 stdout 的 `url: http://127.0.0.1:PORT/...`
 *   那一行读。`-quiet` 会连这一行一起吞掉，不能加。
 * - 遥测默认开（PostHog），`-no-telemetry` 关。每天还会查一次 GitHub 上的新版本，
 *   只打印提示、不自己换二进制，不用管。
 * - 不带 `-no-open` 会去开宿主机上的浏览器。
 * - `-base-path` 之下的一切（静态资源、`/api/*`、SSE 的 `/api/stream`）都挂在这个前缀里，
 *   反代原样转发路径即可，不用改写。
 * - `-version` 打印 `px0 0.1.10 (linux/amd64)`。
 */

import { joinPath } from "../git/path.js";
import { quotePosix } from "../zellij/host.js";

export const PX0_VERSION = "0.1.10";

export const DEFAULT_BASE_URL = "https://github.com/px0-ai/px0/releases/download";

export type Px0Os = "linux" | "darwin" | "windows";
export type Px0Arch = "amd64" | "arm64";

export interface Px0Target {
  os: Px0Os;
  arch: Px0Arch;
}

/**
 * 锁定版本各资产的 sha256，取自该 release 的 checksums.txt。
 *
 * 写死在源码里而不是运行时去拉 checksums.txt：px0 的服务端没有鉴权、能直接驱动
 * agent 改代码，值得钉死字节，而不是信 release 页面上当下挂着的东西。
 * 升级 PX0_VERSION 时一起换。只列我们支持的六个平台。
 */
export const PX0_SHA256: Readonly<Record<string, string>> = {
  "px0-0.1.10-darwin-amd64": "1d24a458d4ad5c2bda0d3af2b29ad4f7481e786267d4a414593bff0625e82e13",
  "px0-0.1.10-darwin-arm64": "703a22ed9cc51172b033ac6747b7c61d711f812a271732b0a0832da374c9063a",
  "px0-0.1.10-linux-amd64": "8abe9591f3c6b5277a97837ad8f20df873b6f67b44075cf2acb059e97a86731a",
  "px0-0.1.10-linux-arm64": "b021e0d416d393cd3008facbee61addd80764af0c62a18feba2f2e6a05f39c60",
  "px0-0.1.10-windows-amd64.exe":
    "8d81cfb5eff135c82f73a8e15cec7a5a6ea4987580df0a5ea4b312ec58339766",
  "px0-0.1.10-windows-arm64.exe":
    "c1cc0d4026ff34643247d3f13503d720c4b66fcd230bacbf376ad81c3ea9f5d4",
};

function archOf(machine: string): Px0Arch | null {
  if (machine === "x86_64" || machine === "amd64" || machine === "x64") return "amd64";
  if (machine === "aarch64" || machine === "arm64") return "arm64";
  return null;
}

/** 远端 `uname -sm` → 资产平台。认不出返回 null，调用方报「没有这个平台的构建」 */
export function px0TargetFromUname(uname: string): Px0Target | null {
  const [os, machine] = uname.trim().split(/\s+/);
  if (!os || !machine) return null;
  const arch = archOf(machine);
  if (!arch) return null;
  if (os === "Linux") return { os: "linux", arch };
  if (os === "Darwin") return { os: "darwin", arch };
  return null;
}

/** 后端本机的资产平台 */
export function localPx0Target(
  platform: NodeJS.Platform = process.platform,
  arch: string = process.arch
): Px0Target | null {
  const a = archOf(arch);
  if (!a) return null;
  if (platform === "linux") return { os: "linux", arch: a };
  if (platform === "darwin") return { os: "darwin", arch: a };
  if (platform === "win32") return { os: "windows", arch: a };
  return null;
}

/** GitHub release 资产名，如 px0-0.1.10-linux-amd64 / px0-0.1.10-windows-amd64.exe */
export function px0AssetName(target: Px0Target, version = PX0_VERSION): string {
  return `px0-${version}-${target.os}-${target.arch}${target.os === "windows" ? ".exe" : ""}`;
}

export function px0DownloadUrl(
  asset: string,
  version = PX0_VERSION,
  baseUrl = DEFAULT_BASE_URL
): string {
  return `${baseUrl.replace(/\/+$/, "")}/v${version}/${asset}`;
}

/** `px0 -version` → 0.1.10。对不上就当没装好，重推一份 */
export function parsePx0Version(text: string): string | null {
  const m = text.match(/px0\s+v?(\d+\.\d+\.\d+)/);
  return m?.[1] ?? null;
}

const ANSI_RE = /\x1b\[[0-9;?]*[A-Za-z]/g;

/**
 * 从 px0 的启动输出里找监听端口。输出可能带颜色、可能是 pty 的 \r\n，
 * 也可能在 url 行之前夹着登录 shell 的 motd——只认 `url:` 后面那个回环地址。
 */
export function parseListenPort(text: string): number | null {
  const plain = text.replace(ANSI_RE, "");
  const m = plain.match(/url:\s*http:\/\/(?:127\.0\.0\.1|localhost|\[::1\]):(\d{1,5})\//);
  if (!m) return null;
  const port = Number(m[1]);
  return port > 0 && port < 65536 ? port : null;
}

export interface Px0ArgsInput {
  /** `/px0/<项目 id>/`，见 shared 的 px0BasePath */
  basePath: string;
  /** 工作目录（宿主机上的绝对路径） */
  dir: string;
}

export function px0Args(input: Px0ArgsInput): string[] {
  return [
    "-host",
    "127.0.0.1",
    "-port",
    "0",
    "-no-open",
    "-no-telemetry",
    "-no-color",
    "-base-path",
    input.basePath,
    input.dir,
  ];
}

/** 远端的 px0 放在 <falcon 根>/bin，带版本号，与 Zellij 同一个目录 */
export function remotePx0Path(root: string, version = PX0_VERSION): string {
  return joinPath("posix", root, "bin", `px0-${version}`);
}

/** 远端的工作目录允许写成 `~/x`：px0 收到的是引号里的字面量，不会替我们展开 */
export function expandHome(dir: string, home: string): string {
  if (dir === "~") return home;
  if (dir.startsWith("~/")) return joinPath("posix", home, dir.slice(2));
  return dir;
}

/** 远端 px0 的版本。文件不在时 shell 报 not found，解析不出版本，调用方据此重推 */
export function posixVersionCommand(bin: string): string {
  return `${quotePosix(bin)} -version 2>/dev/null`;
}

/**
 * 从 stdin 收二进制写到远端。先写 .partial 再改名：推到一半断了不会留下一个
 * 截断的可执行文件，下次 -version 对不上会重推。
 */
export function posixInstallCommand(file: string): string {
  const dir = file.replace(/\/[^/]*$/, "") || "/";
  return (
    `d=${quotePosix(dir)}; f=${quotePosix(file)}; ` +
    `mkdir -p "$d" && cat > "$f.partial" && chmod 755 "$f.partial" && mv -f "$f.partial" "$f"`
  );
}

/**
 * 远端启动命令。要配合带 pty 的 exec 通道用（SshLink.execStream 的 pty 选项）：
 *
 * - 外层 `exec <登录 shell> -i -l -c`：理由同 agent 启动脚本（ADR 0013）——px0 派编辑
 *   给 claude / codex，查 PR 要 gh / GITHUB_TOKEN，这些多半只在交互登录 shell 的
 *   rc 文件里才进 PATH / 环境。三个 flag 分开写。
 * - 内层再 `exec` 一次，px0 顶替 shell 成为 pty 的会话首进程：通道一关它就收到
 *   SIGHUP 退出，不会在远端留孤儿。
 */
export function posixLaunchCommand(shell: string, bin: string, args: string[]): string {
  const inner = `exec ${[bin, ...args].map(quotePosix).join(" ")}`;
  return `exec ${quotePosix(shell)} -i -l -c ${quotePosix(inner)}`;
}

/** 起不来时给用户看的最后几行：剥颜色、剥空行、截长度 */
export function tailLines(text: string, n = 6, max = 600): string {
  const lines = text
    .replace(ANSI_RE, "")
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  const tail = lines.slice(-n).join("\n");
  return tail.length <= max ? tail : tail.slice(tail.length - max);
}
