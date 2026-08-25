/**
 * 项目工作目录的只读浏览：列一层目录、取一个文件的内容用于查看。
 *
 * 与 fs.ts 是两件不同的事。那边服务于项目表单——只列文件夹、能一路往上走到根；
 * 这边只在**项目工作目录内部**打转，文件和目录都要列，还得把文件内容取回来。
 * 相同的是执行环境的取舍：本地走 node:fs，远端走宿主机 exec 而不是 SFTP
 * （理由同 fs.ts：sftp-server 被禁的机器并不少见）。
 *
 * 命令构造是纯函数（listCommand / readCommand）、输出解析也是（parseEntries /
 * parseRead / classify），照 git 与 zellij 那两套的分层来，测试只打这一层。
 */

import fs from "node:fs";
import type {
  FilePreview,
  WorkspaceEntry,
  WorkspaceIndex,
  WorkspaceListing,
} from "@falcon/shared";
import { WORKSPACE_FILE_CAP, WORKSPACE_INDEX_CAP, WORKSPACE_LIST_CAP } from "@falcon/shared";
import { isAncestor, joinPath, normalizeSep, samePath } from "./git/path.js";
import { encodePowerShell, quotePosix, quotePowerShell, type HostKind } from "./zellij/host.js";
import type { ExecFn } from "./zellij/install.js";

/** 执行环境。本地不走 exec——node_modules 那种目录用 shell 循环列会慢到没法用 */
export type FileHost =
  | { local: true; kind: HostKind }
  | { local: false; kind: HostKind; exec: ExecFn };

const LIST_TIMEOUT_MS = 15_000;
const READ_TIMEOUT_MS = 30_000;

// ---------------- 路径 ----------------

/**
 * 前端给的工作目录相对路径 → 路径段。
 *
 * 前端一律用 `/` 分隔（协议里就这么定的，见 WorkspaceEntry），所以这里只按 `/` 切。
 * `..` 与空段直接拒绝：这是唯一能凭一个 query 参数跑出工作目录的方式，挡在最外层
 * 比在下游各处补判断可靠。
 */
export function relSegments(rel: string | undefined): string[] {
  if (!rel) return [];
  const segs = rel.split("/").filter((s) => s.length > 0);
  for (const seg of segs) {
    if (seg === "." || seg === "..") throw new Error("路径不合法");
    // 控制字符只可能来自构造出来的请求，真实文件名里没有。命令是用 quotePosix /
    // -EncodedCommand 拼的，本就注不进去，挡掉只是少一层担心
    if (/[\u0000-\u001f]/.test(seg)) throw new Error("路径不合法");
  }
  return segs;
}

/**
 * 拼出宿主机上的绝对路径，并复核它确实在工作目录里。
 *
 * relSegments 已经挡掉了 `..`，这里再比一次是双保险——两处用的是同一套路径原语
 * （git/path.ts），比较语义不会悄悄漂移。
 *
 * 注意护栏挡的是**路径拼接**，不是符号链接：工作目录里一条指向 /etc 的链接照样
 * 能被点开。这不构成越权——用户对这台宿主机本来就有 shell（终端里 cat 就行），
 * 而挡下来反而会让 monorepo 里常见的软链目录变成一堆打不开的死项。
 */
export function resolveInside(kind: HostKind, root: string, rel: string | undefined): string {
  const segs = relSegments(rel);
  const base = normalizeSep(kind, root);
  if (segs.length === 0) return base;
  const full = joinPath(kind, base, ...segs);
  if (!isAncestor(kind, base, full) && !samePath(kind, base, full)) {
    throw new Error("路径不合法");
  }
  return full;
}

/** 相对路径拼接，始终用 `/`——这是给前端的形式，与宿主机分隔符无关 */
function relJoin(parent: string, name: string): string {
  return parent ? `${parent}/${name}` : name;
}

// ---------------- 列目录 ----------------

/**
 * 每行 `d <名字>` 或 `f <名字>`。
 *
 * 名字里含换行的文件会被拆成两条错行——与 fs.ts 的既有取舍一致：
 * 换行文件名在真实仓库里基本不存在，而为它引入 NUL 分隔会牺牲 busybox 兼容性。
 */
export function listCommand(kind: HostKind, dir: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$d = ${quotePowerShell(dir)}`,
        `if (-not (Test-Path -LiteralPath $d)) { Write-Output 'ENOENT'; exit 1 }`,
        `if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOTDIR'; exit 1 }`,
        `Get-ChildItem -LiteralPath $d -Force | ForEach-Object { ` +
          `if ($_.PSIsContainer) { Write-Output ('d ' + $_.Name) } else { Write-Output ('f ' + $_.Name) } }`,
      ].join("; ")
    );
  }
  const d = quotePosix(dir);
  return [
    `d=${d}`,
    `if [ ! -e "$d" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ ! -d "$d" ]; then printf '%s\\n' ENOTDIR; exit 1; fi`,
    `if [ ! -r "$d" ]; then printf '%s\\n' EACCES; exit 1; fi`,
    // -d 会跟随符号链接：指向目录的链接算目录，点开进去是用户的预期
    `ls -1A "$d" | while IFS= read -r n; do ` +
      `if [ -d "$d/$n" ]; then printf 'd %s\\n' "$n"; else printf 'f %s\\n' "$n"; fi; done`,
  ].join("; ");
}

export function parseEntries(stdout: string, parentRel: string): WorkspaceEntry[] {
  const out: WorkspaceEntry[] = [];
  for (const raw of stdout.split(/\r?\n/)) {
    const line = raw.replace(/\r$/, "");
    if (line.length < 2) continue;
    const tag = line[0];
    if ((tag !== "d" && tag !== "f") || line[1] !== " ") continue;
    const name = line.slice(2);
    if (!name || name === "." || name === "..") continue;
    out.push({ name, path: relJoin(parentRel, name), kind: tag === "d" ? "dir" : "file" });
  }
  return out;
}

/**
 * 目录在前、文件在后，各自按名字排（localeCompare，中文文件名才不会按码位乱序）。
 * 点开头的不像 fs.ts 那样沉底——仓库里的 .github / .claude 是要看的东西，
 * 把它们压到 node_modules 后面反而找不着。
 */
export function sortEntries(entries: WorkspaceEntry[]): WorkspaceEntry[] {
  return entries.sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  });
}

function capEntries(entries: WorkspaceEntry[], path: string): WorkspaceListing {
  const sorted = sortEntries(entries);
  return {
    path,
    entries: sorted.slice(0, WORKSPACE_LIST_CAP),
    truncated: sorted.length > WORKSPACE_LIST_CAP,
  };
}

async function listLocal(kind: HostKind, dir: string, rel: string): Promise<WorkspaceListing> {
  let dirents: fs.Dirent[];
  try {
    dirents = await fs.promises.readdir(dir, { withFileTypes: true });
  } catch (err) {
    throw localError(err, "无法读取该目录");
  }
  const entries: WorkspaceEntry[] = [];
  for (const ent of dirents) {
    let isDir = ent.isDirectory();
    if (ent.isSymbolicLink()) {
      // dirent.isDirectory() 对符号链接恒为 false，得跟过去看一眼
      try {
        isDir = (await fs.promises.stat(joinPath(kind, dir, ent.name))).isDirectory();
      } catch {
        isDir = false;
      }
    }
    entries.push({
      name: ent.name,
      path: relJoin(rel, ent.name),
      kind: isDir ? "dir" : "file",
    });
  }
  return capEntries(entries, rel);
}

/** 列工作目录里的一层。`rel` 为空表示工作目录本身 */
export async function listWorkspace(
  host: FileHost,
  root: string,
  rel: string | undefined
): Promise<WorkspaceListing> {
  const relPath = relSegments(rel).join("/");
  const dir = resolveInside(host.kind, root, rel);
  if (host.local) return listLocal(host.kind, dir, relPath);
  const res = await execTimed(host.exec, listCommand(host.kind, dir), LIST_TIMEOUT_MS);
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法读取该目录");
  }
  return capEntries(parseEntries(res.stdout, relPath), relPath);
}

// ---------------- 文件索引（Quick Open） ----------------

/**
 * 目录遍历时跳过的名字。git 仓库走 ls-files（尊重 gitignore），只有非 git
 * 项目才落到这里——node_modules / dist 那种生成物会把索引撑爆，列出来也搜不到。
 */
export const INDEX_SKIP_DIRS: readonly string[] = [
  "node_modules",
  ".git",
  ".svn",
  ".hg",
  "dist",
  "build",
  "out",
  ".next",
  ".nuxt",
  ".output",
  "target",
  "vendor",
  "__pycache__",
  ".venv",
  "venv",
  "coverage",
  ".cache",
  ".turbo",
];

const INDEX_TIMEOUT_MS = 20_000;
const INDEX_TRUNCATED = "__TRUNCATED__";

function capIndex(paths: string[], truncated = false): WorkspaceIndex {
  if (paths.length <= WORKSPACE_INDEX_CAP) return { paths, truncated };
  return { paths: paths.slice(0, WORKSPACE_INDEX_CAP), truncated: true };
}

/**
 * 把宿主机绝对路径收成工作目录相对、一律 `/` 分隔。对不上前缀的行丢掉
 * （find 偶尔会打一条警告到 stdout 混进来）。
 */
export function relativizeIndexLine(line: string, root: string, kind: HostKind): string | null {
  const raw = line.replace(/\r$/, "");
  if (!raw || raw === INDEX_TRUNCATED) return null;
  const full = normalizeSep(kind, raw);
  const base = normalizeSep(kind, root).replace(/[\\/]+$/, "");
  if (kind === "windows") {
    const fullL = full.toLowerCase();
    const baseL = base.toLowerCase();
    if (fullL === baseL) return null;
    if (!fullL.startsWith(`${baseL}\\`)) return null;
    return full.slice(base.length + 1).replaceAll("\\", "/");
  }
  if (full === base) return null;
  if (!full.startsWith(`${base}/`)) return null;
  return full.slice(base.length + 1);
}

export function parseIndexLines(stdout: string, root: string, kind: HostKind): WorkspaceIndex {
  const skip = new Set<string>(INDEX_SKIP_DIRS);
  const paths: string[] = [];
  let truncated = stdout.includes(INDEX_TRUNCATED);
  for (const raw of stdout.split(/\r?\n/)) {
    const rel = relativizeIndexLine(raw, root, kind);
    if (!rel) continue;
    if (rel.split("/").some((seg) => skip.has(seg))) continue;
    paths.push(rel);
    if (paths.length >= WORKSPACE_INDEX_CAP) {
      truncated = true;
      break;
    }
  }
  return { paths, truncated };
}

/** 远端遍历。`-prune` 掉 INDEX_SKIP_DIRS，避免在 node_modules 里转一圈 */
export function indexCommand(kind: HostKind, dir: string): string {
  if (kind === "windows") {
    const skipMap = INDEX_SKIP_DIRS.map((n) => `'${n}' = 1`).join("; ");
    return encodePowerShell(
      [
        `$d = ${quotePowerShell(dir)}`,
        `if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOENT'; exit 1 }`,
        `$skip = @{ ${skipMap} }`,
        `$n = 0`,
        `$cap = ${WORKSPACE_INDEX_CAP}`,
        `function Walk($p) {`,
        `  if ($script:n -ge $cap) { return }`,
        `  Get-ChildItem -LiteralPath $p -Force -ErrorAction SilentlyContinue | ForEach-Object {`,
        `    if ($script:n -ge $cap) { return }`,
        `    if ($_.PSIsContainer) {`,
        `      if (-not $skip.ContainsKey($_.Name)) { Walk $_.FullName }`,
        `    } else {`,
        `      Write-Output $_.FullName`,
        `      $script:n++`,
        `    }`,
        `  }`,
        `}`,
        `Walk $d`,
        `if ($n -ge $cap) { Write-Output '${INDEX_TRUNCATED}' }`,
      ].join("; ")
    );
  }
  const d = quotePosix(dir);
  const prune = INDEX_SKIP_DIRS.map((n) => `-name ${quotePosix(n)}`).join(" -o ");
  return [
    `d=${d}`,
    `if [ ! -d "$d" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ ! -r "$d" ]; then printf '%s\\n' EACCES; exit 1; fi`,
    `find "$d" \\( -type d \\( ${prune} \\) \\) -prune -o -type f -print` +
      ` | awk -v cap=${WORKSPACE_INDEX_CAP} 'NR<=cap{print} NR==cap+1{print "${INDEX_TRUNCATED}"; exit}'`,
  ].join("; ");
}

async function indexLocal(kind: HostKind, root: string): Promise<WorkspaceIndex> {
  const skip = new Set<string>(INDEX_SKIP_DIRS);
  const paths: string[] = [];
  const stack: string[] = [""];
  while (stack.length) {
    const rel = stack.pop()!;
    const dir = rel ? joinPath(kind, root, ...rel.split("/")) : root;
    let dirents: fs.Dirent[];
    try {
      dirents = await fs.promises.readdir(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const ent of dirents) {
      if (ent.isSymbolicLink()) continue;
      if (skip.has(ent.name)) continue;
      const child = relJoin(rel, ent.name);
      if (ent.isDirectory()) {
        stack.push(child);
        continue;
      }
      paths.push(child);
      if (paths.length >= WORKSPACE_INDEX_CAP) return { paths, truncated: true };
    }
  }
  return { paths, truncated: false };
}

/** 非 git 项目的兜底：遍历工作目录，跳过 INDEX_SKIP_DIRS */
export async function indexWorkspace(host: FileHost, root: string): Promise<WorkspaceIndex> {
  const dir = resolveInside(host.kind, root, "");
  if (host.local) return indexLocal(host.kind, dir);
  const res = await execTimed(host.exec, indexCommand(host.kind, dir), INDEX_TIMEOUT_MS);
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法读取该目录");
  }
  return parseIndexLines(res.stdout, dir, host.kind);
}

export function capFileIndex(paths: string[]): WorkspaceIndex {
  return capIndex(paths);
}

// ---------------- 读文件 ----------------

/**
 * 第一行是字节数，其余是内容的 base64（只取前 WORKSPACE_FILE_CAP 字节）。
 *
 * 内容一律 base64 过一道：exec 通道上原始字节靠不住（Windows 的 OpenSSH 会按
 * 代码页改写，见 paste.ts），而且 stdout 是当 UTF-8 解码的，二进制过不去。
 */
export function readCommand(kind: HostKind, file: string, cap: number): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$p = ${quotePowerShell(file)}`,
        `if (-not (Test-Path -LiteralPath $p)) { Write-Output 'ENOENT'; exit 1 }`,
        `$i = Get-Item -LiteralPath $p -Force`,
        `if ($i.PSIsContainer) { Write-Output 'EISDIR'; exit 1 }`,
        `Write-Output $i.Length`,
        `$fsr = [IO.File]::OpenRead($p)`,
        `try { ` +
          `$ms = New-Object IO.MemoryStream; ` +
          `$buf = New-Object byte[] 65536; ` +
          // Read 不保证一次给满，循环到读够 cap 或文件结束
          `while ($ms.Length -lt ${cap}) { ` +
          `$want = [Math]::Min($buf.Length, ${cap} - $ms.Length); ` +
          `$n = $fsr.Read($buf, 0, $want); ` +
          `if ($n -le 0) { break }; ` +
          `$ms.Write($buf, 0, $n) }; ` +
          `Write-Output ([Convert]::ToBase64String($ms.ToArray())) ` +
          `} finally { $fsr.Close() }`,
      ].join("; ")
    );
  }
  const f = quotePosix(file);
  return [
    `f=${f}`,
    `if [ ! -e "$f" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ -d "$f" ]; then printf '%s\\n' EISDIR; exit 1; fi`,
    `if [ ! -r "$f" ]; then printf '%s\\n' EACCES; exit 1; fi`,
    `wc -c < "$f" | tr -d ' '`,
    // busybox / 精简镜像里 base64 未必有，openssl 是最常见的替补
    `if command -v base64 >/dev/null 2>&1; then b64=base64; else b64="openssl base64"; fi`,
    `head -c ${cap} "$f" | $b64`,
  ].join("; ");
}

export interface ReadResult {
  /** 文件真实字节数（不是取回来的字节数） */
  size: number;
  bytes: Buffer;
}

export function parseRead(stdout: string): ReadResult {
  const lines = stdout.split(/\r?\n/);
  const size = Number((lines[0] ?? "").trim());
  if (!Number.isFinite(size) || size < 0) throw new Error("读不到文件大小");
  // base64 输出按 76 字符折行（openssl 与 GNU base64 都是），拼回一整串再解
  const b64 = lines.slice(1).join("").replace(/\s+/g, "");
  return { size, bytes: Buffer.from(b64, "base64") };
}

const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  avif: "image/avif",
  // SVG 本身是文本，但用户点开它想看的是图。<img> 里的 SVG 不执行脚本，
  // 当图片渲染是安全的
  svg: "image/svg+xml",
};

export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i <= 0 ? "" : name.slice(i + 1).toLowerCase();
}

/**
 * 字节 → 前端能直接渲染的形状。
 *
 * 二进制判定跟 git 一样看 NUL：前 8KB 里出现 NUL 就当二进制。这条规则会把
 * UTF-16 文本也判成二进制，但那在源码仓库里几乎不出现，而反过来把真二进制
 * 当文本渲染会当场喷出几兆乱码。
 */
export function classify(name: string, size: number, bytes: Buffer): FilePreview {
  const mime = IMAGE_MIME[extOf(name)];
  if (mime) {
    if (size > WORKSPACE_FILE_CAP) return { kind: "too-large", size };
    return { kind: "image", mime, base64: bytes.toString("base64"), size };
  }
  const head = bytes.subarray(0, 8192);
  if (head.includes(0)) return { kind: "binary", size };
  return {
    kind: "text",
    text: bytes.toString("utf8"),
    size,
    truncated: size > bytes.length,
  };
}

async function readLocal(file: string): Promise<ReadResult> {
  let stat: fs.Stats;
  try {
    stat = await fs.promises.stat(file);
  } catch (err) {
    throw localError(err, "读不到该文件");
  }
  if (stat.isDirectory()) throw new Error("这是一个文件夹");
  const size = stat.size;
  let handle: fs.promises.FileHandle;
  try {
    handle = await fs.promises.open(file, "r");
  } catch (err) {
    throw localError(err, "读不到该文件");
  }
  try {
    const want = Math.min(size, WORKSPACE_FILE_CAP);
    const buf = Buffer.alloc(want);
    const { bytesRead } = await handle.read(buf, 0, want, 0);
    return { size, bytes: buf.subarray(0, bytesRead) };
  } finally {
    await handle.close();
  }
}

/** 读工作目录里的一个文件，供查看 tab 渲染 */
export async function readWorkspaceFile(
  host: FileHost,
  root: string,
  rel: string
): Promise<FilePreview> {
  const segs = relSegments(rel);
  if (segs.length === 0) throw new Error("路径不合法");
  const file = resolveInside(host.kind, root, rel);
  const name = segs[segs.length - 1]!;

  if (host.local) {
    const { size, bytes } = await readLocal(file);
    return classify(name, size, bytes);
  }
  const res = await execTimed(
    host.exec,
    readCommand(host.kind, file, WORKSPACE_FILE_CAP),
    READ_TIMEOUT_MS
  );
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "读不到该文件");
  }
  const { size, bytes } = parseRead(res.stdout);
  return classify(name, size, bytes);
}

// ---------------- 错误 ----------------

function localError(err: unknown, fallback: string): Error {
  const code = (err as NodeJS.ErrnoException).code;
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EISDIR") return new Error("这是一个文件夹");
  if (code === "EACCES" || code === "EPERM") return new Error("没有权限访问该路径");
  return new Error(fallback);
}

function remoteError(stdout: string, fallback: string): Error {
  const code = stdout.trim().split(/\r?\n/)[0];
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EISDIR") return new Error("这是一个文件夹");
  if (code === "EACCES") return new Error("没有权限访问该路径");
  return new Error(fallback);
}

async function execTimed(exec: ExecFn, command: string, timeoutMs: number) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), timeoutMs);
  try {
    return await exec(command, ac.signal);
  } catch (err) {
    if (ac.signal.aborted) throw new Error("读取超时");
    throw new Error(`SSH 连接失败：${(err as Error).message}`);
  } finally {
    clearTimeout(timer);
  }
}
