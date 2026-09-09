/**
 * docker / compose 命令构造与输出解析。纯函数，零 I/O。
 *
 * 与 git/command.ts、zellij/command.ts 同一条规矩：**只产出 argv 数组**（找
 * compose 文件那条除外，find 不是 docker 子命令），转义交给 buildCommandLine
 * 一处完成。禁止把用户输入拼进 shell 字符串。
 *
 * 子命令是闭集白名单：面板能点的只有 ps / images / logs / start / stop /
 * restart / rm / rmi / image prune / compose up|down|ps|logs。容器名、镜像
 * 引用、compose 相对路径在进 argv 之前都过一遍 isSafe*，过不了的请求直接 400。
 */

import type {
  DockerComposeFile,
  DockerComposeService,
  DockerContainer,
  DockerImage,
  DockerOpInput,
  DockerUnavailableReason,
} from "@falcon/shared";
import {
  buildCommandLine,
  encodePowerShell,
  quotePosix,
  quotePowerShell,
  type HostKind,
} from "../zellij/host.js";

/** 错误分类靠解析 stderr，锁死 C 以免中文系统把 "permission denied" 翻掉 */
export const DOCKER_ENV: Record<string, string> = { LC_ALL: "C" };

export const DOCKER_LIST_CAP = 500;
export const COMPOSE_FILE_CAP = 40;
export const LOGS_DEFAULT_TAIL = 200;
export const LOGS_MAX_TAIL = 2000;
export const LOGS_MAX_BYTES = 512 * 1024;

export const COMPOSE_FILENAMES = [
  "compose.yaml",
  "compose.yml",
  "docker-compose.yaml",
  "docker-compose.yml",
] as const;

/**
 * 找 compose 文件时跳过的目录。比 files.ts 的 INDEX_SKIP_DIRS 短一截：
 * 这里 maxdepth 只有 3，node_modules 才是真会把 find 拖死的那个。
 */
export const COMPOSE_SKIP_DIRS = [
  "node_modules",
  ".git",
  ".svn",
  ".hg",
  "dist",
  "build",
  "out",
  "vendor",
  ".venv",
  "venv",
  "__pycache__",
  ".next",
  "target",
] as const;

export function buildDockerCommandLine(kind: HostKind, argv: string[]): string {
  return buildCommandLine(kind, argv, DOCKER_ENV);
}

// ---------------- 白名单 ----------------

/**
 * 容器名 / 容器 ID / 镜像引用。
 *
 * 允许 docker 自己认的字符：字母数字、`. _ : / @ + -`（digest 的 `name@sha256:…`、
 * 仓库路径 `ghcr.io/foo/bar:tag`）。**禁止以 `-` 开头**，否则会被 docker 当成
 * 开关；也禁止空白和 shell 元字符——argv 已经 quote，这一层是双保险，挡的是
 * 我们自己以后不小心把 ref 拼进别的字符串。
 */
export function isSafeDockerRef(s: string): boolean {
  if (!s || s.length > 256) return false;
  if (s.startsWith("-")) return false;
  if (s.includes("..")) return false;
  return /^[A-Za-z0-9][A-Za-z0-9._:/@+-]*$/.test(s);
}

export function isComposeFileName(name: string): boolean {
  const n = name.toLowerCase();
  return (COMPOSE_FILENAMES as readonly string[]).includes(n);
}

/**
 * 工作目录相对的 compose 路径。分隔符按 `/`（协议如此）。
 * 段数卡在 4：发现层 maxdepth 3，再宽一格给手工指定，不能变成任意路径。
 */
export function isSafeComposeRel(rel: string): boolean {
  if (!rel || rel.length > 512) return false;
  if (rel.startsWith("/") || rel.startsWith("\\")) return false;
  if (rel.includes("..") || rel.includes("\0")) return false;
  const segs = rel.split("/").filter((s) => s.length > 0);
  if (segs.length === 0 || segs.length > 4) return false;
  if (!isComposeFileName(segs[segs.length - 1]!)) return false;
  return segs.every((s) => s !== "." && s !== ".." && !/[\x00-\x1f\\]/.test(s));
}

export function clampTail(n: unknown): number {
  if (typeof n !== "number" || !Number.isFinite(n)) return LOGS_DEFAULT_TAIL;
  return Math.min(LOGS_MAX_TAIL, Math.max(1, Math.round(n)));
}

export function truncateLogs(text: string): { text: string; truncated: boolean } {
  if (text.length <= LOGS_MAX_BYTES) return { text, truncated: false };
  return { text: text.slice(text.length - LOGS_MAX_BYTES), truncated: true };
}

// ---------------- argv ----------------

export function versionArgs(docker: string): string[] {
  return [docker, "version", "--format", "{{.Client.Version}}"];
}

export function psArgs(docker: string): string[] {
  // `{{json .}}` 比 `--format json` 老得多（1.13 就有），一行一个对象。
  return [docker, "ps", "-a", "--format", "{{json .}}"];
}

export function imagesArgs(docker: string): string[] {
  return [docker, "images", "--format", "{{json .}}"];
}

export function logsArgs(docker: string, ref: string, tail: number): string[] {
  return [docker, "logs", "--tail", String(tail), "--timestamps", ref];
}

export function startArgs(docker: string, ref: string): string[] {
  return [docker, "start", ref];
}

export function stopArgs(docker: string, ref: string): string[] {
  return [docker, "stop", ref];
}

export function restartArgs(docker: string, ref: string): string[] {
  return [docker, "restart", ref];
}

export function rmArgs(docker: string, ref: string, force: boolean): string[] {
  return force ? [docker, "rm", "-f", ref] : [docker, "rm", ref];
}

export function rmiArgs(docker: string, ref: string): string[] {
  return [docker, "rmi", ref];
}

/** 未用镜像，含未被容器引用的非 dangling。面板确认文案必须写清楚。 */
export function imagePruneArgs(docker: string): string[] {
  return [docker, "image", "prune", "-a", "-f"];
}

export type ComposeFlavor =
  | { kind: "plugin" }
  | { kind: "standalone"; bin: string };

export function composeVersionArgs(docker: string): string[] {
  return [docker, "compose", "version"];
}

export function standaloneComposeVersionArgs(bin: string): string[] {
  return [bin, "version"];
}

/**
 * compose 子命令 argv。`-f` 与 `--project-directory` 都给绝对路径：
 * 后者决定 .env 从哪读，前者钉死文件，避免 cwd 漂移（SSH exec 没有 cwd）。
 */
export function composeArgs(
  docker: string,
  compose: ComposeFlavor,
  file: string,
  projectDir: string,
  ...args: string[]
): string[] {
  const prefix = compose.kind === "plugin" ? [docker, "compose"] : [compose.bin];
  return [...prefix, "-f", file, "--project-directory", projectDir, ...args];
}

export function composePsArgs(
  docker: string,
  compose: ComposeFlavor,
  file: string,
  projectDir: string
): string[] {
  return composeArgs(docker, compose, file, projectDir, "ps", "-a", "--format", "json");
}

export function composeUpArgs(
  docker: string,
  compose: ComposeFlavor,
  file: string,
  projectDir: string
): string[] {
  return composeArgs(docker, compose, file, projectDir, "up", "-d");
}

export function composeDownArgs(
  docker: string,
  compose: ComposeFlavor,
  file: string,
  projectDir: string
): string[] {
  return composeArgs(docker, compose, file, projectDir, "down");
}

export function composeLogsArgs(
  docker: string,
  compose: ComposeFlavor,
  file: string,
  projectDir: string,
  tail: number
): string[] {
  return composeArgs(
    docker,
    compose,
    file,
    projectDir,
    "logs",
    "--no-color",
    "--timestamps",
    "--tail",
    String(tail)
  );
}

// ---------------- 发现 compose 文件 ----------------

/**
 * 在工作目录及往下两层子目录里找 compose 文件名。
 *
 * 不是 docker 子命令，所以走独立的命令行而不是 argv：POSIX 用 find，Windows
 * 用 Get-ChildItem。目录一律 quote，skip 名单是常量。
 */
export function findComposeFilesCommand(kind: HostKind, dir: string): string {
  if (kind === "windows") {
    const skipMap = COMPOSE_SKIP_DIRS.map((n) => `'${n}' = 1`).join("; ");
    const nameMap = COMPOSE_FILENAMES.map((n) => `'${n}' = 1`).join("; ");
    return encodePowerShell(
      [
        `$d = ${quotePowerShell(dir)}`,
        `if (-not (Test-Path -LiteralPath $d -PathType Container)) { exit 0 }`,
        `$skip = @{ ${skipMap} }`,
        `$names = @{ ${nameMap} }`,
        `$n = 0`,
        `function Walk($p, $depth) {`,
        `  if ($script:n -ge ${COMPOSE_FILE_CAP}) { return }`,
        `  Get-ChildItem -LiteralPath $p -Force -ErrorAction SilentlyContinue | ForEach-Object {`,
        `    if ($script:n -ge ${COMPOSE_FILE_CAP}) { return }`,
        `    if ($_.PSIsContainer) {`,
        `      if ($depth -lt 2 -and -not $skip.ContainsKey($_.Name)) { Walk $_.FullName ($depth + 1) }`,
        `    } elseif ($names.ContainsKey($_.Name)) {`,
        `      Write-Output $_.FullName`,
        `      $script:n++`,
        `    }`,
        `  }`,
        `}`,
        `Walk $d 0`,
      ].join("; ")
    );
  }
  const d = quotePosix(dir);
  const prune = COMPOSE_SKIP_DIRS.map((n) => `-name ${quotePosix(n)}`).join(" -o ");
  const names = COMPOSE_FILENAMES.map((n) => `-name ${quotePosix(n)}`).join(" -o ");
  // maxdepth 3 = 工作目录 + 两层子目录（find 把起点算 depth 0，直接子项是 1）
  return [
    `d=${d}`,
    `if [ ! -d "$d" ] || [ ! -r "$d" ]; then exit 0; fi`,
    `find "$d" -maxdepth 3 \\( -type d \\( ${prune} \\) -prune \\) -o \\( -type f \\( ${names} \\) -print \\)`,
  ].join("; ");
}

export function parseComposeFind(
  stdout: string,
  relativize: (line: string) => string | null
): DockerComposeFile[] {
  const seen = new Set<string>();
  const out: DockerComposeFile[] = [];
  for (const raw of stdout.split(/\r?\n/)) {
    const rel = relativize(raw.trim());
    if (!rel || !isSafeComposeRel(rel)) continue;
    if (seen.has(rel)) continue;
    seen.add(rel);
    out.push({ path: rel });
    if (out.length >= COMPOSE_FILE_CAP) break;
  }
  return out;
}

// ---------------- 输出解析 ----------------

export function parseJsonLines(stdout: string): unknown[] {
  const t = stdout.trim();
  if (!t) return [];
  if (t.startsWith("[")) {
    try {
      const v = JSON.parse(t) as unknown;
      return Array.isArray(v) ? v : [];
    } catch {
      return [];
    }
  }
  const out: unknown[] = [];
  for (const line of t.split(/\r?\n/)) {
    const s = line.trim();
    if (!s.startsWith("{")) continue;
    try {
      out.push(JSON.parse(s));
    } catch {
      // docker 偶尔往 stdout 打警告，混在 NDJSON 里就跳过这一行
    }
  }
  return out;
}

function asString(v: unknown): string {
  return typeof v === "string" ? v : v == null ? "" : String(v);
}

function field(o: Record<string, unknown>, ...keys: string[]): string {
  for (const k of keys) {
    if (k in o) return asString(o[k]);
  }
  return "";
}

export function inferContainerState(state: string, status: string): string {
  const s = state.trim().toLowerCase();
  if (s) return s;
  const st = status.trim().toLowerCase();
  if (st.startsWith("up")) return "running";
  if (st.startsWith("exited")) return "exited";
  if (st.startsWith("created")) return "created";
  if (st.startsWith("restarting")) return "restarting";
  if (st.startsWith("paused")) return "paused";
  if (st.startsWith("dead")) return "dead";
  return "unknown";
}

export function isContainerRunning(state: string): boolean {
  const s = state.toLowerCase();
  return s === "running" || s === "restarting" || s === "paused";
}

function splitNames(raw: string): string[] {
  return raw
    .split(/[,\s]+/)
    .map((n) => n.replace(/^\//, "").trim())
    .filter(Boolean);
}

export function parsePsJson(stdout: string): DockerContainer[] {
  const out: DockerContainer[] = [];
  for (const row of parseJsonLines(stdout)) {
    if (!row || typeof row !== "object") continue;
    const o = row as Record<string, unknown>;
    const id = field(o, "ID", "Id");
    if (!id) continue;
    const status = field(o, "Status");
    const state = inferContainerState(field(o, "State"), status);
    out.push({
      id,
      names: splitNames(field(o, "Names", "Name")),
      image: field(o, "Image"),
      state,
      status,
      ports: field(o, "Ports"),
      created: field(o, "CreatedAt", "Created", "RunningFor"),
      command: field(o, "Command").replace(/^"|"$/g, ""),
    });
    if (out.length >= DOCKER_LIST_CAP) break;
  }
  return out;
}

export function parseImagesJson(stdout: string): DockerImage[] {
  const out: DockerImage[] = [];
  for (const row of parseJsonLines(stdout)) {
    if (!row || typeof row !== "object") continue;
    const o = row as Record<string, unknown>;
    const id = field(o, "ID", "Id");
    if (!id) continue;
    const repository = field(o, "Repository");
    const tag = field(o, "Tag");
    out.push({
      id,
      repository,
      tag,
      size: field(o, "Size"),
      created: field(o, "CreatedSince", "CreatedAt"),
      dangling: repository === "<none>" || tag === "<none>" || repository === "",
    });
    if (out.length >= DOCKER_LIST_CAP) break;
  }
  return out;
}

function formatPublishers(v: unknown): string {
  if (typeof v === "string") return v;
  if (!Array.isArray(v)) return "";
  const parts: string[] = [];
  for (const p of v) {
    if (!p || typeof p !== "object") continue;
    const o = p as Record<string, unknown>;
    const tgt = o.TargetPort ?? o.Target;
    const pub = o.PublishedPort ?? o.Published;
    const proto = asString(o.Protocol) || "tcp";
    const host = asString(o.URL) || asString(o.HostIP) || "0.0.0.0";
    if (pub && tgt) parts.push(`${host}:${pub}->${tgt}/${proto}`);
    else if (tgt) parts.push(`${tgt}/${proto}`);
  }
  return parts.join(", ");
}

export function parseComposePs(stdout: string): DockerComposeService[] {
  const out: DockerComposeService[] = [];
  for (const row of parseJsonLines(stdout)) {
    if (!row || typeof row !== "object") continue;
    const o = row as Record<string, unknown>;
    const service = field(o, "Service", "service");
    const name = field(o, "Name") || service;
    if (!name && !service) continue;
    const status = field(o, "Status", "status");
    const state = inferContainerState(field(o, "State", "state"), status);
    const ports = field(o, "Ports") || formatPublishers(o.Publishers);
    out.push({ name, service: service || name, state, status, ports });
    if (out.length >= DOCKER_LIST_CAP) break;
  }
  return out;
}

// ---------------- 错误分类 ----------------

/**
 * 把 docker 的非零退出翻译成闭集 reason。
 *
 * 顺序要紧：permission 的原文里也带 "docker daemon socket"，必须先于 daemon
 * 那条匹配，否则"没权限"会被报成"daemon 没在跑"，用户去空查服务状态。
 */
export function classifyDockerFailure(
  stderr: string,
  stdout: string,
  code: number | null
): DockerUnavailableReason {
  const text = `${stderr}\n${stdout}`;
  const lower = text.toLowerCase();
  if (code === 127) return "docker-missing";
  if (
    /command not found/i.test(text) ||
    /not recognized as an internal/i.test(text) ||
    /不是内部或外部命令/.test(text) ||
    /executable file not found/i.test(text)
  ) {
    return "docker-missing";
  }
  if (
    /permission denied while trying to connect to the docker daemon socket/i.test(text) ||
    /got permission denied while trying to connect/i.test(text) ||
    /permission denied.*docker\.sock/i.test(lower) ||
    /dial unix .*docker\.sock: connect: permission denied/i.test(text)
  ) {
    return "docker-permission";
  }
  if (
    /cannot connect to the docker daemon/i.test(text) ||
    /is the docker daemon running/i.test(text) ||
    /error during connect/i.test(text) ||
    /docker desktop is not running/i.test(text) ||
    /failed to connect to the docker api/i.test(text) ||
    /open \/\/\.\/pipe\/docker_engine/i.test(text) ||
    (/cannot find the file specified/i.test(lower) && /docker/i.test(lower))
  ) {
    return "docker-daemon";
  }
  if (
    /unknown command.*compose/i.test(lower) ||
    /'compose' is not a docker command/i.test(lower) ||
    /docker-compose.*command not found/i.test(lower)
  ) {
    return "compose-missing";
  }
  return "command-failed";
}

export function errorDetail(stderr: string, stdout: string, code: number | null): string {
  const line =
    stderr
      .split(/\r?\n/)
      .map((l) => l.trim())
      .find((l) => l.length > 0) ||
    stdout
      .split(/\r?\n/)
      .map((l) => l.trim())
      .find((l) => l.length > 0);
  if (line) return line.slice(0, 2000);
  return `退出码 ${code}`;
}

// ---------------- POST /docker/op 入参 ----------------

export function parseDockerOpInput(body: unknown): DockerOpInput | { error: string } {
  if (!body || typeof body !== "object") return { error: "缺少操作" };
  const o = body as Record<string, unknown>;
  switch (o.op) {
    case "start":
    case "stop":
    case "restart": {
      const ref = asRef(o.ref);
      if (!ref) return { error: "容器引用不合法" };
      return { op: o.op, ref };
    }
    case "remove": {
      const ref = asRef(o.ref);
      if (!ref) return { error: "容器引用不合法" };
      return { op: "remove", ref, force: o.force === true };
    }
    case "image-remove": {
      const ref = asRef(o.ref);
      if (!ref) return { error: "镜像引用不合法" };
      return { op: "image-remove", ref };
    }
    case "image-prune":
      return { op: "image-prune" };
    case "compose-up":
    case "compose-down": {
      const file = asComposeFile(o.file);
      if (!file) return { error: "compose 文件路径不合法" };
      return { op: o.op, file };
    }
    default:
      return { error: "未知操作" };
  }
}

function asRef(v: unknown): string | null {
  return typeof v === "string" && isSafeDockerRef(v) ? v : null;
}

function asComposeFile(v: unknown): string | null {
  return typeof v === "string" && isSafeComposeRel(v) ? v : null;
}
