/**
 * cloudflared 可执行文件的定位与按需下载。
 *
 * 只在 falcon 后端本机跑（见 ADR 0014），所以二进制也只落在本机
 * `<dataDir>/bin/cloudflared`，不往远端宿主机装。
 *
 * 解析顺序：
 * 1. FALCON_CLOUDFLARED_BIN —— 显式指定，不校验版本（用户有意覆盖）；
 * 2. `<dataDir>/bin/cloudflared` 且 `--version` 对得上锁定版本；
 * 3. 从 GitHub release 下载锁定版本（darwin 是 tgz，linux 是裸二进制）；
 * 4. PATH 上的 `cloudflared` —— 下载失败时的兜底，版本可能对不上。
 *
 * 不做完整性校验：走默认 GitHub 源时 HTTPS 已保证来源。Zellij 同一套权衡。
 */

import { execFile, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import {
  CLOUDFLARED_VERSION,
  cloudflaredAsset,
  downloadUrl,
  parseCloudflaredVersion,
} from "./command.js";

const execFileAsync = promisify(execFile);

const DOWNLOAD_TIMEOUT_MS = 120_000;
const VERSION_TIMEOUT_MS = 8_000;

export function bundledBinName(platform = process.platform): string {
  return platform === "win32" ? "cloudflared.exe" : "cloudflared";
}

export function installPath(dataDir: string, platform = process.platform): string {
  return path.join(dataDir, "bin", bundledBinName(platform));
}

export class CloudflaredBinError extends Error {
  constructor(
    readonly reason: "arch-unsupported" | "download-failed" | "extract-failed" | "verify-failed",
    message: string
  ) {
    super(message);
    this.name = "CloudflaredBinError";
  }
}

let inflight: Promise<string> | null = null;

/**
 * 拿到一个能跑的 cloudflared 路径。并发调用合并成一次下载。
 * 失败后清掉 inflight，下次再试。
 */
export function ensureCloudflared(
  dataDir: string,
  env: NodeJS.ProcessEnv = process.env
): Promise<string> {
  if (inflight) return inflight;
  inflight = ensureNow(dataDir, env).finally(() => {
    inflight = null;
  });
  return inflight;
}

async function ensureNow(dataDir: string, env: NodeJS.ProcessEnv): Promise<string> {
  if (env.FALCON_CLOUDFLARED_BIN) {
    const p = env.FALCON_CLOUDFLARED_BIN;
    if (!fs.existsSync(p)) {
      throw new CloudflaredBinError("verify-failed", `FALCON_CLOUDFLARED_BIN 指向的文件不存在：${p}`);
    }
    return p;
  }

  const dest = installPath(dataDir);
  if (fs.existsSync(dest) && (await versionOf(dest)) === CLOUDFLARED_VERSION) {
    return dest;
  }

  try {
    await downloadLocked(dest);
    return dest;
  } catch (err) {
    const pathBin = await whichCloudflared();
    if (pathBin) return pathBin;
    throw err;
  }
}

async function downloadLocked(dest: string): Promise<void> {
  const asset = cloudflaredAsset();
  if (!asset) {
    throw new CloudflaredBinError("arch-unsupported", "该架构没有官方 cloudflared 构建");
  }
  const url = downloadUrl(asset);
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-cloudflared-"));
  try {
    const downloadTo = path.join(tmpRoot, asset.name);
    await fetchToFile(url, downloadTo);

    let binary = downloadTo;
    if (asset.kind === "tgz") {
      try {
        await execFileAsync("tar", ["-xzf", downloadTo, "-C", tmpRoot], { timeout: 30_000 });
      } catch (err) {
        throw new CloudflaredBinError(
          "extract-failed",
          `解压 cloudflared 失败：${(err as Error).message}`
        );
      }
      const found = findExtractedBinary(tmpRoot);
      if (!found) {
        throw new CloudflaredBinError("extract-failed", "压缩包里没有 cloudflared 二进制");
      }
      binary = found;
    }

    fs.chmodSync(binary, 0o755);
    const ver = await versionOf(binary);
    if (ver !== CLOUDFLARED_VERSION) {
      throw new CloudflaredBinError(
        "verify-failed",
        ver
          ? `cloudflared 版本是 ${ver}，期望 ${CLOUDFLARED_VERSION}`
          : "cloudflared 无法执行（可能是 noexec 挂载或架构不兼容）"
      );
    }

    fs.mkdirSync(path.dirname(dest), { recursive: true });
    const partial = dest + ".partial";
    fs.copyFileSync(binary, partial);
    fs.chmodSync(partial, 0o755);
    fs.renameSync(partial, dest);
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
}

function findExtractedBinary(dir: string): string | null {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const e of entries) {
    if (e.isFile() && (e.name === "cloudflared" || e.name === "cloudflared.exe")) {
      return path.join(dir, e.name);
    }
  }
  return null;
}

async function fetchToFile(url: string, dest: string): Promise<void> {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), DOWNLOAD_TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      signal: ac.signal,
      redirect: "follow",
      headers: { "User-Agent": "falcon" },
    });
    if (!res.ok) {
      throw new CloudflaredBinError("download-failed", `下载 cloudflared 失败: HTTP ${res.status}`);
    }
    const buf = Buffer.from(await res.arrayBuffer());
    if (buf.length === 0) {
      throw new CloudflaredBinError("download-failed", "下载 cloudflared 失败: 空响应");
    }
    fs.writeFileSync(dest, buf);
  } catch (err) {
    if (err instanceof CloudflaredBinError) throw err;
    const msg = ac.signal.aborted ? "下载超时" : (err as Error).message;
    throw new CloudflaredBinError("download-failed", `下载 cloudflared 失败：${msg}`);
  } finally {
    clearTimeout(timer);
  }
}

function versionOf(bin: string): Promise<string | null> {
  return new Promise((resolve) => {
    const proc = spawn(bin, ["--version"], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    const chunks: Buffer[] = [];
    proc.stdout?.on("data", (d: Buffer) => chunks.push(d));
    proc.stderr?.on("data", (d: Buffer) => chunks.push(d));
    const timer = setTimeout(() => {
      proc.kill();
      resolve(null);
    }, VERSION_TIMEOUT_MS);
    proc.on("error", () => {
      clearTimeout(timer);
      resolve(null);
    });
    proc.on("close", () => {
      clearTimeout(timer);
      resolve(parseCloudflaredVersion(Buffer.concat(chunks).toString("utf8")));
    });
  });
}

function whichCloudflared(): Promise<string | null> {
  // `command` 是 shell 内置，不能直接 spawn；跟 localExec 一样走 shell。
  const cmd = process.platform === "win32" ? "where cloudflared" : "command -v cloudflared";
  return new Promise((resolve) => {
    const proc = spawn(cmd, {
      shell: true,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const chunks: Buffer[] = [];
    proc.stdout?.on("data", (d: Buffer) => chunks.push(d));
    proc.on("error", () => resolve(null));
    proc.on("close", (code) => {
      if (code !== 0) return resolve(null);
      const line = Buffer.concat(chunks).toString("utf8").split(/\r?\n/).find((l) => l.trim());
      resolve(line?.trim() || "cloudflared");
    });
  });
}
