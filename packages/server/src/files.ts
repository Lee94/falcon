/**
 * 项目工作目录的浏览与改动：列一层目录、取一个文件的内容、新建文件夹、
 * 重命名、删除（ADR 0009）。
 *
 * 与 fs.ts 是两件不同的事。那边服务于项目表单——只列文件夹、能一路往上走到根；
 * 这边只在**项目工作目录内部**打转，文件和目录都要列，还得把文件内容取回来，
 * 以及在用户确认之后改这一层（mkdir / rename / rm）。下载 / 上传按流走，在
 * transfer.ts。相同的是执行环境的取舍：本地走 node:fs，远端走宿主机 exec 而
 * 不是 SFTP（理由同 fs.ts：sftp-server 被禁的机器并不少见）。
 *
 * 命令构造是纯函数（listCommand / readCommand / mkdirCommand 等）、输出解析
 * 也是（parseEntries / parseRead / classify），照 git 与 zellij 那两套的分层
 * 来，测试只打这一层。
 */

import fs from "node:fs";
import type { Duplex, Readable } from "node:stream";
import type {
  FileOpResult,
  FilePreview,
  FileRemoveResult,
  WorkspaceEntry,
  WorkspaceIndex,
  WorkspaceListing,
} from "@falcon/shared";
import {
  WORKSPACE_FILE_CAP,
  WORKSPACE_INDEX_CAP,
  WORKSPACE_LIST_CAP,
  WORKSPACE_RAW_CAP,
} from "@falcon/shared";
import { dirnameOf, isAncestor, joinPath, normalizeSep, samePath } from "./git/path.js";
import { encodePowerShell, quotePosix, quotePowerShell, type HostKind } from "./zellij/host.js";
import type { ExecFn } from "./zellij/install.js";

/**
 * 远端命令的流式通道：写入端是命令的 stdin，读取端是 stdout，`close` 事件带退出码
 * （ssh2 的 ClientChannel 正是这个形状）。下载 / 上传（transfer.ts）按流走，
 * exec 那种"攒成字符串"的形状装不下一个几百 MB 的文件。
 */
export interface ExecChannel extends Duplex {
  stderr: Readable;
}
export type ExecStreamFn = (commandLine: string) => Promise<ExecChannel>;

/** 执行环境。本地不走 exec——node_modules 那种目录用 shell 循环列会慢到没法用 */
export type FileHost =
  | { local: true; kind: HostKind }
  | { local: false; kind: HostKind; exec: ExecFn; execStream: ExecStreamFn };

const LIST_TIMEOUT_MS = 15_000;
const READ_TIMEOUT_MS = 30_000;
/** 原始字节上限是文本的 8 倍，慢一点的 SSH 链路上 base64 回传 16MB 要不少时间 */
const RAW_TIMEOUT_MS = 90_000;
const MKDIR_TIMEOUT_MS = 15_000;
const RENAME_TIMEOUT_MS = 15_000;
/** 递归删 node_modules 那种目录，远端可能要一会儿 */
const REMOVE_TIMEOUT_MS = 120_000;

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
 * 每行 `d <size> <mtime> <名字>` 或 `f <size> <mtime> <名字>`。
 *
 * 目录的 size 恒 0（前端画成 —）；mtime 是 Unix 秒。名字是行里第三个空格之后的
 * 全部——文件名可以含空格，不能含换行。换行文件名会被拆成两条错行，与 fs.ts
 * 的既有取舍一致：真实仓库里基本不存在，而为它引入 NUL 分隔会牺牲 busybox。
 *
 * POSIX 的 size / mtime 先试 `stat -c`（GNU / busybox），没有再试 `stat -f`
 * （BSD / macOS 远端）。列一层多一次 stat 比再开一条命令划算。
 */
export function listCommand(kind: HostKind, dir: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$d = ${quotePowerShell(dir)}`,
        `if (-not (Test-Path -LiteralPath $d)) { Write-Output 'ENOENT'; exit 1 }`,
        `if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOTDIR'; exit 1 }`,
        `$epoch = [datetime]::new(1970, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)`,
        `Get-ChildItem -LiteralPath $d -Force | ForEach-Object { ` +
          `$k = if ($_.PSIsContainer) { 'd' } else { 'f' }; ` +
          `$s = if ($_.PSIsContainer) { 0 } else { $_.Length }; ` +
          `$m = [int64]($_.LastWriteTimeUtc - $epoch).TotalSeconds; ` +
          `Write-Output ($k + ' ' + $s + ' ' + $m + ' ' + $_.Name) }`,
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
      `if [ -d "$d/$n" ]; then k=d; s=0; ` +
      `m=$(stat -c %Y "$d/$n" 2>/dev/null || stat -f %m "$d/$n" 2>/dev/null || echo 0); ` +
      `else k=f; ` +
      `info=$(stat -c '%s %Y' "$d/$n" 2>/dev/null || stat -f '%z %m' "$d/$n" 2>/dev/null || echo '0 0'); ` +
      `s=\${info%% *}; m=\${info#* }; fi; ` +
      `printf '%s %s %s %s\\n' "$k" "$s" "$m" "$n"; done`,
  ].join("; ");
}

export function parseEntries(stdout: string, parentRel: string): WorkspaceEntry[] {
  const out: WorkspaceEntry[] = [];
  for (const raw of stdout.split(/\r?\n/)) {
    const line = raw.replace(/\r$/, "");
    const m = /^([df]) (\d+) (\d+) (.*)$/.exec(line);
    if (!m) continue;
    const name = m[4]!;
    if (!name || name === "." || name === "..") continue;
    const kind = m[1] === "d" ? "dir" : "file";
    const size = Number(m[2]);
    const mtime = Number(m[3]);
    const entry: WorkspaceEntry = { name, path: relJoin(parentRel, name), kind };
    if (kind === "file" && Number.isFinite(size)) entry.size = size;
    if (Number.isFinite(mtime)) entry.mtime = mtime;
    out.push(entry);
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
    const full = joinPath(kind, dir, ent.name);
    // 跟过去看：指向目录的链接算 dir。断掉的链接当文件，size / mtime 用 lstat
    let st: fs.Stats;
    try {
      st = await fs.promises.stat(full);
    } catch {
      try {
        st = await fs.promises.lstat(full);
      } catch {
        continue;
      }
    }
    const isDir = st.isDirectory();
    const entry: WorkspaceEntry = {
      name: ent.name,
      path: relJoin(rel, ent.name),
      kind: isDir ? "dir" : "file",
      mtime: Math.floor(st.mtimeMs / 1000),
    };
    if (!isDir) entry.size = st.size;
    entries.push(entry);
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

/**
 * 扩展名 → Content-Type。原始字节路由靠它告诉浏览器怎么处理一个文件：
 * HTML 当文档渲染、CSS / JS 当子资源、图片当图片。表里没有的一律
 * application/octet-stream——配合 nosniff，浏览器不会把它猜成可执行的东西。
 *
 * 不引入 mime-db：这里只需要 web 预览会碰到的那几十种，一整张表大而无当。
 */
const MIME: Record<string, string> = {
  // 图片：`<img>` 里的 SVG 不执行脚本，当图片渲染是安全的
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  avif: "image/avif",
  svg: "image/svg+xml",
  // 文档与子资源
  html: "text/html; charset=utf-8",
  htm: "text/html; charset=utf-8",
  css: "text/css; charset=utf-8",
  js: "text/javascript; charset=utf-8",
  mjs: "text/javascript; charset=utf-8",
  cjs: "text/javascript; charset=utf-8",
  json: "application/json; charset=utf-8",
  map: "application/json; charset=utf-8",
  xml: "application/xml; charset=utf-8",
  txt: "text/plain; charset=utf-8",
  md: "text/plain; charset=utf-8",
  csv: "text/csv; charset=utf-8",
  wasm: "application/wasm",
  pdf: "application/pdf",
  // 字体
  woff: "font/woff",
  woff2: "font/woff2",
  ttf: "font/ttf",
  otf: "font/otf",
  // 音视频
  mp3: "audio/mpeg",
  wav: "audio/wav",
  ogg: "audio/ogg",
  mp4: "video/mp4",
  webm: "video/webm",
};

export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i <= 0 ? "" : name.slice(i + 1).toLowerCase();
}

/** 原始字节路由用的 Content-Type，认不出给 octet-stream */
export function mimeOf(name: string): string {
  return MIME[extOf(name)] ?? "application/octet-stream";
}

/** 查看 tab 把这些扩展名当图片：走原始字节路由给 `<img>`，不读文本 */
export function imageMimeOf(name: string): string | undefined {
  const mime = MIME[extOf(name)];
  return mime?.startsWith("image/") ? mime : undefined;
}

/**
 * 字节 → 前端能直接渲染的形状。
 *
 * 图片不看字节只看扩展名与大小：字节由浏览器自己经原始字节路由取，这里
 * 收到的 bytes 通常是空的（readWorkspaceFile 对图片按 cap 0 读）。
 *
 * 二进制判定跟 git 一样看 NUL：前 8KB 里出现 NUL 就当二进制。这条规则会把
 * UTF-16 文本也判成二进制，但那在源码仓库里几乎不出现，而反过来把真二进制
 * 当文本渲染会当场喷出几兆乱码。
 */
export function classify(name: string, size: number, bytes: Buffer): FilePreview {
  const mime = imageMimeOf(name);
  if (mime) {
    if (size > WORKSPACE_RAW_CAP) return { kind: "too-large", size };
    return { kind: "image", mime, size };
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

async function readLocal(file: string, cap: number): Promise<ReadResult> {
  let stat: fs.Stats;
  try {
    stat = await fs.promises.stat(file);
  } catch (err) {
    throw localError(err, "读不到该文件");
  }
  if (stat.isDirectory()) throw new Error("这是一个文件夹");
  const size = stat.size;
  const want = Math.min(size, cap);
  // 只要大小（图片）就不开文件了
  if (want === 0) return { size, bytes: Buffer.alloc(0) };
  let handle: fs.promises.FileHandle;
  try {
    handle = await fs.promises.open(file, "r");
  } catch (err) {
    throw localError(err, "读不到该文件");
  }
  try {
    const buf = Buffer.alloc(want);
    const { bytesRead } = await handle.read(buf, 0, want, 0);
    return { size, bytes: buf.subarray(0, bytesRead) };
  } finally {
    await handle.close();
  }
}

/**
 * 读工作目录里一个文件的前 cap 个字节，连同真实大小。查看 tab 与原始字节路由
 * 共用：前者对文本取 WORKSPACE_FILE_CAP、对图片取 0（只要大小），后者取
 * WORKSPACE_RAW_CAP。
 */
export async function readWorkspaceBytes(
  host: FileHost,
  root: string,
  rel: string,
  cap: number
): Promise<ReadResult & { name: string }> {
  const segs = relSegments(rel);
  if (segs.length === 0) throw new Error("路径不合法");
  const file = resolveInside(host.kind, root, rel);
  const name = segs[segs.length - 1]!;

  if (host.local) {
    const { size, bytes } = await readLocal(file, cap);
    return { name, size, bytes };
  }
  const res = await execTimed(
    host.exec,
    readCommand(host.kind, file, cap),
    cap > WORKSPACE_FILE_CAP ? RAW_TIMEOUT_MS : READ_TIMEOUT_MS
  );
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "读不到该文件");
  }
  const { size, bytes } = parseRead(res.stdout);
  return { name, size, bytes };
}

/** 读工作目录里的一个文件，供查看 tab 渲染 */
export async function readWorkspaceFile(
  host: FileHost,
  root: string,
  rel: string
): Promise<FilePreview> {
  const segs = relSegments(rel);
  const name = segs[segs.length - 1] ?? "";
  // 图片的字节浏览器会自己去原始字节路由取，这里只要大小；`head -c 0` 与
  // PowerShell 那个 0 字节循环都是合法的空读
  const cap = imageMimeOf(name) ? 0 : WORKSPACE_FILE_CAP;
  const { size, bytes } = await readWorkspaceBytes(host, root, rel, cap);
  return classify(name, size, bytes);
}

// ---------------- 改目录（mkdir / rename / remove） ----------------

/**
 * 单段文件名的护栏。mkdir / rename / 上传共用。
 *
 * 浏览器给的 File.name 不会含分隔符，会含的只能是构造出来的请求；控制字符与
 * `..` 同 relSegments 的理由。Windows 的保留字符在远端也会失败，但那边的报错
 * 是一段 .NET 异常文本，不如在这里直接说清楚。
 */
export function validateEntryName(kind: HostKind, name: string): void {
  if (!name || name === "." || name === "..") throw new Error("文件名不合法");
  if (/[\u0000-\u001f/\\]/.test(name)) throw new Error("文件名不合法");
  if (name.length > 255) throw new Error("文件名太长");
  if (kind === "windows" && /[<>:"|?*]/.test(name)) {
    throw new Error("文件名含 Windows 不允许的字符");
  }
}

function validateRelSegments(kind: HostKind, rel: string): string[] {
  const segs = relSegments(rel);
  if (segs.length === 0) throw new Error("路径不合法");
  for (const seg of segs) validateEntryName(kind, seg);
  return segs;
}

/**
 * 批量删除时把子孙路径收进祖先：删 `src` 就不必再删 `src/a.ts`。
 * 空串（工作目录本身）丢掉——那是护栏，不是省略。
 */
export function collapseRemovePaths(paths: string[]): string[] {
  const norm = [
    ...new Set(paths.map((p) => p.replace(/\/+$/, "")).filter((p) => p.length > 0)),
  ].sort();
  const out: string[] = [];
  for (const p of norm) {
    if (out.some((parent) => p.startsWith(`${parent}/`))) continue;
    out.push(p);
  }
  return out;
}

export function mkdirCommand(
  kind: HostKind,
  dir: string,
  parent: string,
  recursive: boolean
): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$p = ${quotePowerShell(dir)}`,
        `$g = ${quotePowerShell(parent)}`,
        `if (Test-Path -LiteralPath $p -PathType Leaf) { Write-Output 'EEXIST'; exit 1 }`,
        recursive
          ? `if (Test-Path -LiteralPath $p -PathType Container) { exit 0 }`
          : [
              `if (Test-Path -LiteralPath $p) { Write-Output 'EEXIST'; exit 1 }`,
              `if (-not (Test-Path -LiteralPath $g -PathType Container)) { Write-Output 'ENOENT'; exit 1 }`,
            ].join("; "),
        `New-Item -ItemType Directory -LiteralPath $p${recursive ? " -Force" : ""} | Out-Null`,
      ].join("; ")
    );
  }
  const p = quotePosix(dir);
  const g = quotePosix(parent);
  if (recursive) {
    return [
      `p=${p}`,
      `if [ -e "$p" ] && [ ! -d "$p" ]; then printf '%s\\n' EEXIST; exit 1; fi`,
      `if [ -d "$p" ]; then exit 0; fi`,
      `mkdir -p -- "$p" || { printf '%s\\n' EACCES; exit 1; }`,
    ].join("; ");
  }
  return [
    `p=${p}`,
    `g=${g}`,
    `if [ -e "$p" ]; then printf '%s\\n' EEXIST; exit 1; fi`,
    `if [ ! -d "$g" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `mkdir -- "$p" || { printf '%s\\n' EACCES; exit 1; }`,
  ].join("; ");
}

export function renameCommand(kind: HostKind, src: string, dst: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$s = ${quotePowerShell(src)}`,
        `$d = ${quotePowerShell(dst)}`,
        `if (-not (Test-Path -LiteralPath $s)) { Write-Output 'ENOENT'; exit 1 }`,
        `if (Test-Path -LiteralPath $d) { Write-Output 'EEXIST'; exit 1 }`,
        `Move-Item -LiteralPath $s -Destination $d`,
      ].join("; ")
    );
  }
  return [
    `s=${quotePosix(src)}`,
    `d=${quotePosix(dst)}`,
    `if [ ! -e "$s" ] && [ ! -L "$s" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ -e "$d" ] || [ -L "$d" ]; then printf '%s\\n' EEXIST; exit 1; fi`,
    `mv -- "$s" "$d" || { printf '%s\\n' EACCES; exit 1; }`,
  ].join("; ");
}

/**
 * 递归删除文件或目录。
 *
 * POSIX `rm -rf --` 对作为参数的符号链接只删链接本身，不跟过去——这是我们要的：
 * 工作目录里一条指向 /etc 的链接被删掉，不该把 /etc 带走。Windows 走
 * `[IO.Directory]::Delete` / `[IO.File]::Delete`：不走 Remove-Item 的通配展开，
 * 也不穿越 reparse point（理由同 ADR 0002）。
 */
export function removeCommand(kind: HostKind, full: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$p = ${quotePowerShell(full)}`,
        `if (-not (Test-Path -LiteralPath $p)) { Write-Output 'ENOENT'; exit 1 }`,
        `$item = Get-Item -LiteralPath $p -Force`,
        `if ($item.PSIsContainer) { [IO.Directory]::Delete($p, $true) } else { [IO.File]::Delete($p) }`,
      ].join("; ")
    );
  }
  return [
    `p=${quotePosix(full)}`,
    `if [ ! -e "$p" ] && [ ! -L "$p" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `rm -rf -- "$p" || { printf '%s\\n' EACCES; exit 1; }`,
  ].join("; ");
}

export async function mkdirWorkspace(
  host: FileHost,
  root: string,
  rel: string,
  recursive: boolean
): Promise<FileOpResult> {
  const segs = validateRelSegments(host.kind, rel);
  const path = segs.join("/");
  const dir = resolveInside(host.kind, root, path);
  const parent = dirnameOf(host.kind, dir);
  if (host.local) {
    await mkdirLocal(dir, parent, recursive);
    return { path };
  }
  const res = await execTimed(
    host.exec,
    mkdirCommand(host.kind, dir, parent, recursive),
    MKDIR_TIMEOUT_MS,
    "操作超时"
  );
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法创建文件夹");
  }
  return { path };
}

async function mkdirLocal(dir: string, parent: string, recursive: boolean): Promise<void> {
  if (!recursive) {
    let pst: fs.Stats;
    try {
      pst = await fs.promises.stat(parent);
    } catch (err) {
      throw localError(err, "上级目录不可访问");
    }
    if (!pst.isDirectory()) throw new Error("不是文件夹");
  }
  try {
    await fs.promises.mkdir(dir, { recursive });
  } catch (err) {
    throw localError(err, "无法创建文件夹");
  }
}

export async function renameWorkspace(
  host: FileHost,
  root: string,
  rel: string,
  name: string
): Promise<FileOpResult> {
  const segs = validateRelSegments(host.kind, rel);
  validateEntryName(host.kind, name);
  const parentRel = segs.slice(0, -1).join("/");
  const destRel = parentRel ? `${parentRel}/${name}` : name;
  const src = resolveInside(host.kind, root, segs.join("/"));
  const dst = resolveInside(host.kind, root, destRel);
  if (samePath(host.kind, src, dst)) return { path: destRel };
  if (host.local) {
    await renameLocal(src, dst);
    return { path: destRel };
  }
  const res = await execTimed(
    host.exec,
    renameCommand(host.kind, src, dst),
    RENAME_TIMEOUT_MS,
    "操作超时"
  );
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法重命名");
  }
  return { path: destRel };
}

async function renameLocal(src: string, dst: string): Promise<void> {
  try {
    await fs.promises.lstat(src);
  } catch (err) {
    throw localError(err, "路径不存在或不可访问");
  }
  try {
    await fs.promises.lstat(dst);
    throw new Error("同名文件已存在");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") {
      if (err instanceof Error && err.message === "同名文件已存在") throw err;
      throw localError(err, "无法重命名");
    }
  }
  try {
    await fs.promises.rename(src, dst);
  } catch (err) {
    throw localError(err, "无法重命名");
  }
}

export async function removeWorkspace(
  host: FileHost,
  root: string,
  rels: string[]
): Promise<FileRemoveResult> {
  const rootFull = resolveInside(host.kind, root, "");
  const collapsed = collapseRemovePaths(rels);
  const removed: string[] = [];
  const errors: { path: string; error: string }[] = [];
  for (const rel of collapsed) {
    try {
      const segs = relSegments(rel);
      if (segs.length === 0) throw new Error("不能删除工作目录本身");
      const path = segs.join("/");
      const full = resolveInside(host.kind, root, path);
      if (samePath(host.kind, full, rootFull)) throw new Error("不能删除工作目录本身");
      if (host.local) await removeLocal(full);
      else {
        const res = await execTimed(
          host.exec,
          removeCommand(host.kind, full),
          REMOVE_TIMEOUT_MS,
          "操作超时"
        );
        if (res.code !== 0) {
          throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法删除");
        }
      }
      removed.push(path);
    } catch (err) {
      errors.push({ path: rel, error: (err as Error).message });
    }
  }
  return { removed, errors };
}

async function removeLocal(full: string): Promise<void> {
  try {
    await fs.promises.rm(full, { recursive: true, force: false });
  } catch (err) {
    throw localError(err, "无法删除");
  }
}

// ---------------- 错误 ----------------

export function localError(err: unknown, fallback: string): Error {
  const code = (err as NodeJS.ErrnoException).code;
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EISDIR") return new Error("这是一个文件夹");
  if (code === "EACCES" || code === "EPERM") return new Error("没有权限访问该路径");
  if (code === "EEXIST") return new Error("同名文件已存在");
  return new Error(fallback);
}

/** 远端脚本约定：失败时 stdout 第一行是错误码（ENOENT 等），没有约定码的走 fallback */
export function remoteError(stdout: string, fallback: string): Error {
  const code = stdout.trim().split(/\r?\n/)[0];
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EISDIR") return new Error("这是一个文件夹");
  if (code === "EACCES") return new Error("没有权限访问该路径");
  if (code === "EEXIST") return new Error("同名文件已存在");
  return new Error(fallback);
}

async function execTimed(
  exec: ExecFn,
  command: string,
  timeoutMs: number,
  timeoutMsg = "读取超时"
) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), timeoutMs);
  try {
    return await exec(command, ac.signal);
  } catch (err) {
    if (ac.signal.aborted) throw new Error(timeoutMsg);
    throw new Error(`SSH 连接失败：${(err as Error).message}`);
  } finally {
    clearTimeout(timer);
  }
}
