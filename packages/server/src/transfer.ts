/**
 * 工作目录文件的下载与上传（ADR 0008）。
 *
 * 与 files.ts 的分工：那边是只读浏览，字节攒成 Buffer 回给 JSON 或原始字节路由，
 * 有 16MB 的上限；这边是**按流搬运整个文件**——下载把宿主机上的字节直接接到
 * HTTP 响应上，上传把请求体直接接到宿主机上的写入端，中途不落 Buffer，几百 MB
 * 的构建产物也过得去。
 *
 * 远端仍然不走 SFTP（理由同 fs.ts）：POSIX 的 exec 通道对原始字节是可靠的，
 * 下载就是 `cat`、上传就是 `cat >`；Windows 的 OpenSSH 会按代码页改写通道上的
 * 字节（见 paste.ts），两个方向都改走 base64——但不是整文件一坨，而是**每行独立
 * 可解的 base64 行**（Base64LineEncoder / Base64LineDecoder），两端都能流式处理。
 *
 * 上传的落盘规则：先写同目录下的临时文件，收满**声明的字节数**才改名到位。
 * 浏览器中途关掉标签页时，宿主机那头收到的只是 EOF，`cat` 照样退 0——不比字节数
 * 就会留下一个悄悄截断的文件，比上传失败糟得多。
 *
 * 命令构造与编解码是纯函数 / 纯 Transform，测试打这一层。
 */

import crypto from "node:crypto";
import fs from "node:fs";
import { Readable, Transform, type TransformCallback } from "node:stream";
import { pipeline } from "node:stream/promises";
import { joinPath } from "./git/path.js";
import { encodePowerShell, quotePosix, quotePowerShell, type HostKind } from "./zellij/host.js";
import {
  localError,
  readWorkspaceBytes,
  relSegments,
  remoteError,
  resolveInside,
  validateEntryName,
  type ExecChannel,
  type FileHost,
} from "./files.js";

// ---------------- base64 行编解码 ----------------

/**
 * 每行编码多少原始字节。3 的倍数，于是每一行都是一段完整的 base64，解码端
 * 逐行解即可，不必等到整个文件到齐。48KB 是 ssh2 通道包大小的整数倍，
 * 也让 PowerShell 那边一次 Read 就够填一行。
 */
export const B64_LINE_BYTES = 48 * 1024;

/** 原始字节 → base64 行（每行 B64_LINE_BYTES 个输入字节，最后一行不足照发） */
export class Base64LineEncoder extends Transform {
  private pending: Buffer[] = [];
  private pendingLen = 0;

  override _transform(chunk: Buffer, _enc: BufferEncoding, cb: TransformCallback) {
    this.pending.push(chunk);
    this.pendingLen += chunk.length;
    if (this.pendingLen >= B64_LINE_BYTES) {
      const all = Buffer.concat(this.pending);
      let off = 0;
      while (all.length - off >= B64_LINE_BYTES) {
        this.push(`${all.subarray(off, off + B64_LINE_BYTES).toString("base64")}\n`);
        off += B64_LINE_BYTES;
      }
      const rest = all.subarray(off);
      this.pending = rest.length ? [rest] : [];
      this.pendingLen = rest.length;
    }
    cb();
  }

  override _flush(cb: TransformCallback) {
    if (this.pendingLen > 0) this.push(`${Buffer.concat(this.pending).toString("base64")}\n`);
    this.pending = [];
    this.pendingLen = 0;
    cb();
  }
}

/** base64 行 → 原始字节。空行与行尾的 \r 忽略（PowerShell 的 WriteLine 是 CRLF） */
export class Base64LineDecoder extends Transform {
  private tail = "";

  override _transform(chunk: Buffer, _enc: BufferEncoding, cb: TransformCallback) {
    // base64 只有 ASCII，latin1 解码不会把跨包的字节切坏
    const lines = (this.tail + chunk.toString("latin1")).split("\n");
    this.tail = lines.pop() ?? "";
    for (const line of lines) this.pushLine(line);
    cb();
  }

  override _flush(cb: TransformCallback) {
    this.pushLine(this.tail);
    this.tail = "";
    cb();
  }

  private pushLine(line: string) {
    const text = line.trim();
    if (text) this.push(Buffer.from(text, "base64"));
  }
}

// ---------------- 下载 ----------------

/**
 * 把文件按原始字节写到 stdout。POSIX 直接 cat；Windows 分块读、每块一行 base64，
 * `[Console]::Out.WriteLine` 绕开 PowerShell 的对象管道（Write-Output 一行行过
 * 格式化器慢得多）。Read 一次没给满也无所谓——每行独立解码，行长不必固定。
 */
export function downloadCommand(kind: HostKind, file: string): string {
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$p = ${quotePowerShell(file)}`,
        `$fs = [IO.File]::OpenRead($p)`,
        `try { ` +
          `$buf = New-Object byte[] ${B64_LINE_BYTES}; ` +
          `while (($n = $fs.Read($buf, 0, $buf.Length)) -gt 0) { ` +
          `[Console]::Out.WriteLine([Convert]::ToBase64String($buf, 0, $n)) } ` +
          `} finally { $fs.Close() }`,
      ].join("; ")
    );
  }
  return `cat ${quotePosix(file)}`;
}

/**
 * Content-Disposition。`filename*` 是 RFC 5987 的 UTF-8 形式，中文文件名靠它；
 * `filename=` 是给不认 5987 的老客户端的 ASCII 兜底——引号与反斜杠会破坏
 * quoted-string，非 ASCII 字符各家解法不一，统一换成下划线。
 */
export function contentDisposition(name: string): string {
  const ascii = name.replace(/[^\x20-\x7e]/g, "_").replace(/["\\]/g, "_");
  return `attachment; filename="${ascii}"; filename*=UTF-8''${encodeRfc5987(name)}`;
}

/** encodeURIComponent 放过的 `'()*` 在 5987 的 attr-char 里是不允许的 */
function encodeRfc5987(s: string): string {
  return encodeURIComponent(s).replace(
    /['()*]/g,
    (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`
  );
}

export interface DownloadSource {
  name: string;
  /** 真实字节数，回给浏览器当 Content-Length，下载进度条靠它 */
  size: number;
  stream: Readable;
}

/**
 * 打开一个工作目录文件的下载流。
 *
 * 先按 cap 0 读一次（只取大小）：存在性、权限、"是个文件夹"这些判定与查看 tab
 * 共用同一套代码与同一套错误文案，而且都发生在响应头发出之前——流开始后再发现
 * 问题就只能掐断连接了。远端多一次往返，对一次下载来说不算什么。
 */
export async function openDownload(host: FileHost, root: string, rel: string): Promise<DownloadSource> {
  const { name, size } = await readWorkspaceBytes(host, root, rel, 0);
  const file = resolveInside(host.kind, root, rel);
  if (host.local) return { name, size, stream: fs.createReadStream(file) };

  const channel = await execStreamOrLinkError(host, downloadCommand(host.kind, file));
  const stderr: Buffer[] = [];
  channel.stderr.on("data", (d: Buffer) => stderr.push(d));
  const out: Readable = host.kind === "windows" ? channel.pipe(new Base64LineDecoder()) : channel;
  channel.on("close", (code: number | null) => {
    // 流已经在响应上了，状态码改不了；把流打断，浏览器会如实报"下载失败"，
    // 总好过收到一个看起来完整、其实少了一截的文件
    if (code !== 0) {
      const detail = Buffer.concat(stderr).toString("utf8").trim();
      out.destroy(new Error(detail || `远端读取失败（exit ${code}）`));
    }
  });
  return { name, size, stream: out };
}

// ---------------- 上传 ----------------

/**
 * 上传中途被掐断（浏览器关标签页、SSH 断线、Windows 的 sshd 关通道时会把进程树
 * 整个杀掉——脚本自己的清理跑不到）会留下 `.<名字>.<随机>.falcon-upload`。
 * 下一次往同一目录上传时顺手把超过这个岁数的扫掉；正在传的那些都比它年轻。
 */
export const STALE_TMP_MINUTES = 6 * 60;
const TMP_SUFFIX = ".falcon-upload";

export interface UploadTarget {
  /** 工作目录相对路径（`/` 分隔），回给前端 */
  rel: string;
  /** 宿主机上的目标绝对路径 */
  file: string;
  /** 目标所在目录 */
  parent: string;
  /** 同目录下的临时文件，收满字节后改名成 file */
  tmp: string;
}

/**
 * 上传文件名的护栏。浏览器给的 File.name 不会含分隔符，会含的只能是构造出来的
 * 请求；控制字符与 `..` 同 relSegments 的理由。Windows 的保留字符在远端也会
 * 失败，但那边的报错是一段 .NET 异常文本，不如在这里直接说清楚。
 */
export function validateUploadName(kind: HostKind, name: string): void {
  validateEntryName(kind, name);
}

export function uploadTarget(
  kind: HostKind,
  root: string,
  dirRel: string | undefined,
  name: string,
  tag: string = crypto.randomBytes(4).toString("hex")
): UploadTarget {
  validateUploadName(kind, name);
  const dirSegs = relSegments(dirRel);
  const rel = [...dirSegs, name].join("/");
  const parent = resolveInside(kind, root, dirSegs.join("/"));
  const file = resolveInside(kind, root, rel);
  const tmp = joinPath(kind, parent, `.${name}.${tag}${TMP_SUFFIX}`);
  return { rel, file, parent, tmp };
}

/**
 * stdin → 临时文件 → 核对字节数 → 改名到位。
 *
 * 检查顺序里 EEXIST 排在最后：目标是文件夹、上级目录不存在这些"覆盖也救不了"
 * 的情况先报，前端问过用户"要覆盖吗"之后带 overwrite 重发才不会撞第二个错。
 * 每条失败分支都自己 rm 临时文件——脚本没有 trap 可依赖（Windows 那边没有）。
 */
export function uploadCommand(
  kind: HostKind,
  target: UploadTarget,
  size: number,
  overwrite: boolean
): string {
  if (!Number.isInteger(size) || size < 0) throw new Error("字节数不合法");
  if (kind === "windows") {
    return encodePowerShell(
      [
        `$ErrorActionPreference = 'Stop'`,
        `$f = ${quotePowerShell(target.file)}`,
        `$t = ${quotePowerShell(target.tmp)}`,
        `$p = ${quotePowerShell(target.parent)}`,
        `if (Test-Path -LiteralPath $f -PathType Container) { Write-Output 'EISDIR'; exit 1 }`,
        `if (-not (Test-Path -LiteralPath $p -PathType Container)) { Write-Output 'ENOENT'; exit 1 }`,
        ...(overwrite ? [] : [`if (Test-Path -LiteralPath $f) { Write-Output 'EEXIST'; exit 1 }`]),
        `Get-ChildItem -LiteralPath $p -Filter '*${TMP_SUFFIX}' -File -Force -EA SilentlyContinue | ` +
          `Where-Object { $_.LastWriteTime -lt (Get-Date).AddMinutes(-${STALE_TMP_MINUTES}) } | ` +
          `Remove-Item -Force -EA SilentlyContinue`,
        `$fs = [IO.File]::Create($t)`,
        // ReadLine 到 EOF 给 $null；空行是编码端不会发的，跳过只是稳妥
        `try { while ($null -ne ($line = [Console]::In.ReadLine())) { ` +
          `if ($line.Length -gt 0) { $b = [Convert]::FromBase64String($line); $fs.Write($b, 0, $b.Length) } } ` +
          `} finally { $fs.Close() }`,
        `if ((Get-Item -LiteralPath $t -Force).Length -ne ${size}) { ` +
          `Remove-Item -LiteralPath $t -Force; Write-Output 'ESHORT'; exit 1 }`,
        `Move-Item -LiteralPath $t -Destination $f -Force`,
      ].join("; ")
    );
  }
  return [
    `f=${quotePosix(target.file)}`,
    `t=${quotePosix(target.tmp)}`,
    `p=${quotePosix(target.parent)}`,
    `if [ -d "$f" ]; then printf '%s\\n' EISDIR; exit 1; fi`,
    `if [ ! -d "$p" ]; then printf '%s\\n' ENOENT; exit 1; fi`,
    `if [ ! -w "$p" ]; then printf '%s\\n' EACCES; exit 1; fi`,
    ...(overwrite ? [] : [`if [ -e "$f" ]; then printf '%s\\n' EEXIST; exit 1; fi`]),
    // 极老的 busybox 没有 -mmin，2>/dev/null 吞掉报错，`;` 让 cat 照常跑
    `find "$p" -maxdepth 1 -type f -name ${quotePosix(`.*${TMP_SUFFIX}`)} -mmin +${STALE_TMP_MINUTES} -delete 2>/dev/null`,
    `cat > "$t" || { rm -f "$t"; exit 1; }`,
    `n=$(wc -c < "$t" | tr -d ' ')`,
    // 先 rm 再 printf：客户端已断开时 stdout 可能是关着的，printf 会吃 SIGPIPE 把
    // shell 带走，清理必须排在它前面（Windows 那边同样的顺序）
    `if [ "$n" -ne ${size} ]; then rm -f "$t"; printf '%s\\n' ESHORT; exit 1; fi`,
    `mv -f "$t" "$f" || { rm -f "$t"; exit 1; }`,
  ].join("; ");
}

const SHORT_MESSAGE = "上传中断，收到的字节数与声明的不一致";

async function receiveLocal(target: UploadTarget, size: number, overwrite: boolean, body: Readable) {
  let parent: fs.Stats;
  try {
    parent = await fs.promises.stat(target.parent);
  } catch (err) {
    throw localError(err, "上级目录不可访问");
  }
  if (!parent.isDirectory()) throw new Error("不是文件夹");
  try {
    const existing = await fs.promises.stat(target.file);
    if (existing.isDirectory()) throw new Error("这是一个文件夹");
    if (!overwrite) throw new Error("同名文件已存在");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") throw err;
  }

  await sweepStaleTmp(target.parent);
  // wx：临时名带随机段，撞上只可能是同一毫秒的并发上传，宁可失败也别互相覆盖
  const ws = fs.createWriteStream(target.tmp, { flags: "wx" });
  try {
    await pipeline(body, ws);
    if (ws.bytesWritten !== size) throw new Error(SHORT_MESSAGE);
    await fs.promises.rename(target.tmp, target.file);
  } catch (err) {
    await fs.promises.unlink(target.tmp).catch(() => {});
    if (err instanceof Error && err.message === SHORT_MESSAGE) throw err;
    throw localError(err, `写入失败：${(err as Error).message}`);
  }
}

/** 本地版的陈旧临时文件清理，规则同远端脚本；清不掉不挡上传 */
async function sweepStaleTmp(parent: string): Promise<void> {
  const cutoff = Date.now() - STALE_TMP_MINUTES * 60_000;
  const names = await fs.promises.readdir(parent).catch(() => [] as string[]);
  for (const name of names) {
    if (!name.startsWith(".") || !name.endsWith(TMP_SUFFIX)) continue;
    const full = joinPath(process.platform === "win32" ? "windows" : "posix", parent, name);
    try {
      const st = await fs.promises.stat(full);
      if (st.isFile() && st.mtimeMs < cutoff) await fs.promises.unlink(full);
    } catch {
      // 顺手的事
    }
  }
}

async function receiveRemote(
  host: Extract<FileHost, { local: false }>,
  target: UploadTarget,
  size: number,
  overwrite: boolean,
  body: Readable
) {
  const channel = await execStreamOrLinkError(host, uploadCommand(host.kind, target, size, overwrite));
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  // 必须把 stdout 读起来：ssh2 要等读端 end 之后才发 close（带退出码）
  channel.on("data", (d: Buffer) => stdout.push(d));
  channel.stderr.on("data", (d: Buffer) => stderr.push(d));
  const closed = new Promise<number | null>((resolve) => {
    channel.once("close", (code: number | null) => resolve(code ?? null));
  });

  // 远端脚本在前置检查失败时会立刻退出（EEXIST 等），这时 pipeline 会以
  // "写入已关闭的流"失败——那不是链路故障，退出码与 stdout 才是真相，先记下来
  let pipeError: unknown;
  try {
    if (host.kind === "windows") await pipeline(body, new Base64LineEncoder(), channel);
    else await pipeline(body, channel);
  } catch (err) {
    pipeError = err;
  }

  // 请求体那头中断时 pipeline 已把通道 destroy 掉，close 通常还会到（远端回
  // CHANNEL_CLOSE），但 SSH 连接本身死了就等不到——兜一个超时
  const code = pipeError ? await withTimeout(closed, 10_000) : await closed;
  if (code === 0) return;
  const out = Buffer.concat(stdout).toString("utf8");
  const err = Buffer.concat(stderr).toString("utf8").trim();
  if (out.trim().split(/\r?\n/)[0] === "ESHORT") throw new Error(SHORT_MESSAGE);
  if (code === null && pipeError) throw new Error("上传中断");
  throw remoteError(out || err, err || `远端写入失败（exit ${code}）`);
}

/**
 * 把请求体写成工作目录里的一个文件。`size` 是请求声明的 Content-Length，
 * 落盘前要与实收字节数核对（见文件头）。同名文件存在且没给 overwrite 时抛
 * "同名文件已存在"，路由把它映射成 409，前端问过用户再重发。
 */
export async function receiveUpload(
  host: FileHost,
  root: string,
  dirRel: string | undefined,
  name: string,
  size: number,
  overwrite: boolean,
  body: Readable
): Promise<{ path: string; size: number }> {
  const target = uploadTarget(host.kind, root, dirRel, name);
  if (host.local) await receiveLocal(target, size, overwrite, body);
  else await receiveRemote(host, target, size, overwrite, body);
  return { path: target.rel, size };
}

// ---------------- 工具 ----------------

async function execStreamOrLinkError(
  host: Extract<FileHost, { local: false }>,
  command: string
): Promise<ExecChannel> {
  try {
    return await host.execStream(command);
  } catch (err) {
    throw new Error(`SSH 连接失败：${(err as Error).message}`);
  }
}

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T | null> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), ms);
    p.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      () => {
        clearTimeout(timer);
        resolve(null);
      }
    );
  });
}
