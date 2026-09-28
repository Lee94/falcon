/**
 * px0 二进制的下载与校验（ADR 0017 决定四）。
 *
 * 所有平台的资产都由后端本机下载，落在 `<dataDir>/bin/<资产名>`：本地项目直接跑它，
 * SSH 项目再经 stdin 推到远端。不让远端自己下载——内网主机常常出不了网。
 *
 * 每个资产的 sha256 钉死在 command.ts（PX0_SHA256），下载后与已在的文件都要对上
 * 才用。后端所在机器下不了 GitHub 时，用户自己把同名文件放进去也行（照样校验）。
 *
 * 本机可用 FALCON_PX0_BIN 指定自己的 px0，不校验版本——用户有意覆盖。
 */

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import {
  localPx0Target,
  PX0_SHA256,
  px0AssetName,
  px0DownloadUrl,
  type Px0Target,
} from "./command.js";

const DOWNLOAD_TIMEOUT_MS = 180_000;

export class Px0BinError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "Px0BinError";
  }
}

/** 同一资产的并发请求合并成一次下载；失败后清掉，下次再试 */
const inflight = new Map<string, Promise<string>>();
/** 本进程里已经校验过的文件：12 MB 算一次 sha256 虽然只要几十毫秒，也不必每次打开都算 */
const verified = new Set<string>();

export function assetPath(dataDir: string, asset: string): string {
  return path.join(dataDir, "bin", asset);
}

/** 本机已经有这个资产的文件（还没校验）。只用来决定入口页上写「下载」还是「启动」 */
export function px0AssetPresent(
  dataDir: string,
  target: Px0Target,
  env: NodeJS.ProcessEnv = process.env
): boolean {
  return fs.existsSync(assetPath(dataDir, px0AssetName(target))) || !!env.FALCON_PX0_BIN;
}

/** 拿到某个平台资产在本机的路径（已校验）。没有就下载。 */
export function ensurePx0Asset(dataDir: string, target: Px0Target): Promise<string> {
  const asset = px0AssetName(target);
  const running = inflight.get(asset);
  if (running) return running;
  const run = ensureNow(dataDir, asset).finally(() => inflight.delete(asset));
  inflight.set(asset, run);
  return run;
}

/** 本机跑的 px0：FALCON_PX0_BIN 优先，否则本平台的锁定资产 */
export async function ensureLocalPx0(
  dataDir: string,
  env: NodeJS.ProcessEnv = process.env
): Promise<string> {
  if (env.FALCON_PX0_BIN) {
    if (!fs.existsSync(env.FALCON_PX0_BIN)) {
      throw new Px0BinError(`FALCON_PX0_BIN 指向的文件不存在：${env.FALCON_PX0_BIN}`);
    }
    return env.FALCON_PX0_BIN;
  }
  const target = localPx0Target();
  if (!target) {
    throw new Px0BinError(`px0 没有 ${process.platform}/${process.arch} 的构建`);
  }
  return ensurePx0Asset(dataDir, target);
}

async function ensureNow(dataDir: string, asset: string): Promise<string> {
  const expected = PX0_SHA256[asset];
  if (!expected) throw new Px0BinError(`px0 没有 ${asset} 这个资产`);
  const dest = assetPath(dataDir, asset);
  if (verified.has(dest) && fs.existsSync(dest)) return dest;
  if (fs.existsSync(dest) && (await sha256File(dest)) === expected) {
    ensureExecutable(dest);
    verified.add(dest);
    return dest;
  }

  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const partial = `${dest}.partial`;
  try {
    const buf = await download(px0DownloadUrl(asset));
    const got = crypto.createHash("sha256").update(buf).digest("hex");
    if (got !== expected) {
      throw new Px0BinError(`px0 下载内容的 sha256 对不上（${asset}）：得到 ${got}`);
    }
    fs.writeFileSync(partial, buf, { mode: 0o755 });
    fs.renameSync(partial, dest);
  } finally {
    fs.rmSync(partial, { force: true });
  }
  ensureExecutable(dest);
  verified.add(dest);
  return dest;
}

function ensureExecutable(file: string) {
  if (process.platform !== "win32") fs.chmodSync(file, 0o755);
}

function sha256File(file: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash("sha256");
    fs.createReadStream(file)
      .on("error", reject)
      .on("data", (d) => hash.update(d))
      .on("end", () => resolve(hash.digest("hex")));
  });
}

async function download(url: string): Promise<Buffer> {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), DOWNLOAD_TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      signal: ac.signal,
      redirect: "follow",
      headers: { "User-Agent": "falcon" },
    });
    if (!res.ok) throw new Px0BinError(`下载 px0 失败：HTTP ${res.status}（${url}）`);
    const buf = Buffer.from(await res.arrayBuffer());
    if (buf.length === 0) throw new Px0BinError("下载 px0 失败：空响应");
    return buf;
  } catch (err) {
    if (err instanceof Px0BinError) throw err;
    const msg = ac.signal.aborted ? "下载超时" : (err as Error).message;
    throw new Px0BinError(`下载 px0 失败：${msg}（${url}）`);
  } finally {
    clearTimeout(timer);
  }
}
