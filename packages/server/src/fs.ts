/**
 * 目录浏览。本地走 node:fs；SSH 走宿主机 exec，命令行用 git/path 的规则拼——
 * 后端 Windows 也可能在列一台 Linux 远端，node:path 会用错分隔符。
 *
 * 不走 SFTP：跟 Zellij 安装同一个理由，sftp-server 被禁的机器并不少见。
 */
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { FsDirEntry, FsListing } from "@falcon/shared";
import {
  dirnameOf,
  isAbsolute,
  joinPath,
  normalizeSep,
} from "./git/path.js";
import {
  encodePowerShell,
  quotePosix,
  quotePowerShell,
  type HostKind,
} from "./zellij/host.js";
import type { ExecFn } from "./zellij/install.js";

function windowsDrives(): string[] {
  const drives: string[] = [];
  for (let code = 65; code <= 90; code++) {
    const root = `${String.fromCharCode(code)}:\\`;
    try {
      fs.statSync(root);
      drives.push(root);
    } catch {
      // 盘符不存在或不可访问
    }
  }
  return drives;
}

function roots(): string[] {
  return process.platform === "win32" ? windowsDrives() : ["/"];
}

function parentOf(dir: string): string | null {
  const parent = path.dirname(dir);
  if (parent === dir) return process.platform === "win32" ? "" : null;
  return parent;
}

function expandUser(input: string): string {
  const trimmed = input.trim();
  if (trimmed === "~") return os.homedir();
  if (trimmed.startsWith("~/") || trimmed.startsWith("~\\")) {
    return path.join(os.homedir(), trimmed.slice(2));
  }
  return trimmed;
}

function listingError(err: unknown, fallback: string): Error {
  const code = (err as NodeJS.ErrnoException).code;
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EACCES" || code === "EPERM") return new Error("没有权限访问该路径");
  return new Error(fallback);
}

function sortEntries(entries: FsDirEntry[]): FsDirEntry[] {
  return entries.sort((a, b) => {
    const ah = a.name.startsWith(".");
    const bh = b.name.startsWith(".");
    if (ah !== bh) return ah ? 1 : -1;
    return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  });
}

async function listOne(dir: string): Promise<FsListing> {
  let stat: fs.Stats;
  try {
    stat = await fs.promises.stat(dir);
  } catch (err) {
    throw listingError(err, "路径不存在或不可访问");
  }
  if (!stat.isDirectory()) throw new Error("不是文件夹");

  let dirents: fs.Dirent[];
  try {
    dirents = await fs.promises.readdir(dir, { withFileTypes: true });
  } catch (err) {
    throw listingError(err, "无法读取该目录");
  }

  const entries: FsDirEntry[] = [];
  for (const ent of dirents) {
    // 符号链接要跟过去看是不是目录：dirent.isDirectory() 对 symlink 是 false
    if (!ent.isDirectory() && !ent.isSymbolicLink()) continue;
    const full = path.join(dir, ent.name);
    try {
      if (!(await fs.promises.stat(full)).isDirectory()) continue;
    } catch {
      continue;
    }
    entries.push({ name: ent.name, path: full });
  }

  return {
    path: dir,
    parent: parentOf(dir),
    home: os.homedir(),
    roots: roots(),
    entries: sortEntries(entries),
  };
}

function listRoots(): FsListing {
  const list = roots();
  return {
    path: "",
    parent: null,
    home: os.homedir(),
    roots: list,
    entries: list.map((r) => ({
      name: process.platform === "win32" ? r.replace(/\\$/, "") : r,
      path: r,
    })),
  };
}

/**
 * `input === undefined`：打开浏览时的默认位置（家目录）。
 * `input === ""`：Windows 盘符列表；POSIX 上等价于列 `/`。
 * 其余：展开 `~` 后按绝对路径列。相对路径按后端 cwd 解析，和 validate 一样。
 */
export async function listDirectories(input: string | undefined): Promise<FsListing> {
  if (input === "") {
    return process.platform === "win32" ? listRoots() : listOne("/");
  }
  const raw = input == null || input.trim() === "" ? os.homedir() : expandUser(input);
  return listOne(path.resolve(raw));
}

const LIST_TIMEOUT_MS = 15_000;

function listDirsCommand(kind: HostKind, dir: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      `$d = ${quotePowerShell(dir)}; ` +
        `if (-not (Test-Path -LiteralPath $d)) { Write-Output 'ENOENT'; exit 1 }; ` +
        `if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOTDIR'; exit 1 }; ` +
        `Get-ChildItem -LiteralPath $d -Force | Where-Object { $_.PSIsContainer } | ForEach-Object { $_.Name }`
    );
  }
  const d = quotePosix(dir);
  return [
    `d=${d}`,
    `if [ ! -e "$d" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ ! -d "$d" ]; then printf '%s\\n' ENOTDIR; exit 1; fi`,
    `if [ ! -r "$d" ]; then printf '%s\\n' EACCES; exit 1; fi`,
    `ls -1A "$d" | while IFS= read -r name; do if [ -d "$d/$name" ]; then printf '%s\\n' "$name"; fi; done`,
  ].join("; ");
}

function listDrivesCommand(): string {
  return encodePowerShell(
    `Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Root }`
  );
}

function remoteError(stdout: string, fallback: string): Error {
  const code = stdout.trim().split(/\r?\n/)[0];
  if (code === "ENOENT") return new Error("路径不存在或不可访问");
  if (code === "ENOTDIR") return new Error("不是文件夹");
  if (code === "EACCES") return new Error("没有权限访问该路径");
  return new Error(fallback);
}

function parentOfRemote(kind: HostKind, dir: string): string | null {
  if (kind === "posix") return dir === "/" ? null : dirnameOf(kind, dir);
  const n = normalizeSep("windows", dir).replace(/\\+$/, "");
  if (/^[A-Za-z]:$/.test(n)) return "";
  return dirnameOf(kind, dir);
}

function expandRemote(kind: HostKind, home: string, input: string): string {
  const trimmed = input.trim();
  if (trimmed === "~") return home;
  if (trimmed.startsWith("~/") || (kind === "windows" && trimmed.startsWith("~\\"))) {
    return joinPath(kind, home, trimmed.slice(2));
  }
  if (!isAbsolute(kind, trimmed)) return joinPath(kind, home, trimmed);
  return normalizeSep(kind, trimmed);
}

async function execTimed(exec: ExecFn, command: string) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), LIST_TIMEOUT_MS);
  try {
    return await exec(command, ac.signal);
  } catch (err) {
    if (ac.signal.aborted) throw new Error("读取超时");
    throw new Error(`SSH 连接失败：${(err as Error).message}`);
  } finally {
    clearTimeout(timer);
  }
}

function namesFrom(stdout: string): string[] {
  return stdout
    .split(/\r?\n/)
    .map((l) => l.trimEnd())
    .filter((l) => l.length > 0);
}

async function listRemoteOne(
  exec: ExecFn,
  kind: HostKind,
  home: string,
  dir: string,
  roots: string[]
): Promise<FsListing> {
  const res = await execTimed(exec, listDirsCommand(kind, dir));
  if (res.code !== 0) {
    throw remoteError(res.stdout || res.stderr, res.stderr.trim() || "无法读取该目录");
  }
  const entries = sortEntries(
    namesFrom(res.stdout).map((name) => ({ name, path: joinPath(kind, dir, name) }))
  );
  return { path: dir, parent: parentOfRemote(kind, dir), home, roots, entries };
}

async function remoteRoots(exec: ExecFn, kind: HostKind): Promise<string[]> {
  if (kind !== "windows") return ["/"];
  const res = await execTimed(exec, listDrivesCommand());
  if (res.code !== 0) return [];
  return namesFrom(res.stdout)
    .map((r) => normalizeSep("windows", r))
    .filter((r) => /^[A-Za-z]:\\/.test(r));
}

/**
 * 列远端宿主机的子目录。`kind` / `home` 来自 SshLink.probe，这里不再猜平台。
 * `input` 语义与本地 listDirectories 相同。
 */
export async function listRemoteDirectories(
  exec: ExecFn,
  kind: HostKind,
  home: string,
  input: string | undefined
): Promise<FsListing> {
  const roots = await remoteRoots(exec, kind);
  if (input === "") {
    if (kind === "windows") {
      return {
        path: "",
        parent: null,
        home,
        roots,
        entries: roots.map((r) => ({ name: r.replace(/\\$/, ""), path: r })),
      };
    }
    return listRemoteOne(exec, kind, home, "/", roots);
  }
  const target =
    input == null || input.trim() === "" ? home : expandRemote(kind, home, input);
  return listRemoteOne(exec, kind, home, target, roots);
}
