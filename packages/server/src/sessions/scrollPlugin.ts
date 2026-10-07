/**
 * 滚动位置插件（ADR 0019）在宿主机上的部署：插件本体 + zellij 的预授权。
 *
 * 插件是随服务打包的 .wasm（源码 packages/server/zellij-plugin，产物提交在
 * packages/server/assets/），由后端推到宿主机——不让宿主机自己下载，内网主机常常
 * 出不了网（与 px0 同理）。宿主机上的路径固定（HostLayout.scrollPluginFile），旁边
 * 一个 .sha256 记着内容，对得上就不再推。升级时原子替换文件即可：zellij 的插件缓存
 * 只在会话进程内存里，新会话读到新文件，老会话继续用内存里的旧实例（所以插件协议
 * 只能向后兼容地改）。
 *
 * 只在 POSIX 宿主机上做。Windows 远端没有测试机、推二进制也得另走 base64 行协议，
 * 先不支持——那边的会话照旧用 config.kdl，没有滚动条。
 *
 * 部署失败不挡开会话：返回 false，新会话退回老配置（没有滚动条）而已。
 */

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  scrollPermissionsEntry,
  scrollPermissionsFile,
} from "../zellij/command.js";
import { quotePosix, type HostLayout } from "../zellij/host.js";
import type { ZellijTarget } from "../zellij/version.js";
import type { ExecResult } from "../zellij/install.js";

export interface ScrollPluginAsset {
  bytes: Buffer;
  sha256: string;
}

/**
 * 插件产物的位置：
 * 1. FALCON_ZELLIJ_PLUGIN —— 单文件发布时 SEA bootstrap 把它释放到 runtime 目录后指入
 *    （scripts/build-binary.mjs），bundle 里没有源码目录可供相对定位；
 * 2. 相对本文件找 packages/server/assets/ —— src（tsx）与 dist 都在 packages/server 下
 *    两层，相对路径相同。
 * 都没有就是 null：滚动条整个关掉，不影响别的。
 */
export function resolveScrollPlugin(env: NodeJS.ProcessEnv = process.env): string | null {
  if (env.FALCON_ZELLIJ_PLUGIN) {
    return fs.existsSync(env.FALCON_ZELLIJ_PLUGIN) ? env.FALCON_ZELLIJ_PLUGIN : null;
  }
  try {
    const file = fileURLToPath(new URL("../../assets/falcon-scroll.wasm", import.meta.url));
    return fs.existsSync(file) ? file : null;
  } catch {
    // bundle 里 import.meta.url 不可用：只能靠上面的环境变量
    return null;
  }
}

let assetCache: ScrollPluginAsset | null | undefined;

export function scrollPluginAsset(): ScrollPluginAsset | null {
  if (assetCache !== undefined) return assetCache;
  const file = resolveScrollPlugin();
  if (!file) return (assetCache = null);
  const bytes = fs.readFileSync(file);
  assetCache = { bytes, sha256: crypto.createHash("sha256").update(bytes).digest("hex") };
  return assetCache;
}

/** 宿主机的系统，决定 permissions.kdl 在哪（见 scrollPermissionsFile） */
export function targetOs(target: ZellijTarget): "linux" | "darwin" | null {
  if (target.endsWith("-apple-darwin")) return "darwin";
  if (target.includes("-linux-")) return "linux";
  return null;
}

// ---------------- 命令构造（远端 POSIX） ----------------

/** 读宿主机上插件旁边的 .sha256；没有就是空输出 */
export function posixPluginDigestCommand(pluginFile: string): string {
  return `cat ${quotePosix(`${pluginFile}.sha256`)} 2>/dev/null || true`;
}

/**
 * 从 stdin 收插件本体：先落 .partial 再改名，中途断开不留半截文件；改名之后才写
 * .sha256——摘要文件在，就说明本体是完整的那一份。
 */
export function posixPluginInstallCommand(pluginFile: string, sha256: string): string {
  const dir = pluginFile.replace(/\/[^/]*$/, "") || "/";
  return (
    `d=${quotePosix(dir)}; f=${quotePosix(pluginFile)}; ` +
    `mkdir -p "$d" && cat > "$f.partial" && mv -f "$f.partial" "$f" && ` +
    `printf %s ${quotePosix(sha256)} > "$f.sha256"`
  );
}

/**
 * 往 permissions.kdl 追加预授权条目，已有就不动。macOS 上这个文件与用户自己的 zellij
 * 共用，只能追加；条目前垫一个换行，防止原文件末尾没有换行时粘到上一行。
 */
export function posixPermissionsCommand(permFile: string, pluginFile: string): string {
  const entry = scrollPermissionsEntry(pluginFile);
  const key = entry.slice(0, entry.indexOf("\n"));
  const dir = permFile.replace(/\/[^/]*$/, "") || "/";
  return (
    `f=${quotePosix(permFile)}; mkdir -p ${quotePosix(dir)} && ` +
    `{ grep -qsF ${quotePosix(key)} "$f" || printf '\\n%s' ${quotePosix(entry)} >> "$f"; }`
  );
}

// ---------------- 部署 ----------------

/** 远端执行所需的最小接口（SshLink 的子集），便于测试替换 */
export interface RemoteExec {
  exec(commandLine: string): Promise<ExecResult>;
  execWithInput(commandLine: string, input: Buffer | string): Promise<ExecResult>;
}

/** 部署到 SSH 宿主机。返回插件是否就位且已预授权 */
export async function ensureRemoteScrollPlugin(
  link: RemoteExec,
  layout: HostLayout,
  target: ZellijTarget,
  home: string
): Promise<boolean> {
  const asset = scrollPluginAsset();
  const hostOs = targetOs(target);
  if (!asset || !hostOs) return false;
  try {
    const digest = await link.exec(posixPluginDigestCommand(layout.scrollPluginFile));
    if (digest.stdout.trim() !== asset.sha256) {
      const res = await link.execWithInput(
        posixPluginInstallCommand(layout.scrollPluginFile, asset.sha256),
        asset.bytes
      );
      if (res.code !== 0) return false;
    }
    const perm = await link.exec(
      posixPermissionsCommand(
        scrollPermissionsFile(layout, hostOs, home),
        layout.scrollPluginFile
      )
    );
    return perm.code === 0;
  } catch {
    return false;
  }
}

/** 部署到后端本机。返回插件是否就位且已预授权 */
export function ensureLocalScrollPlugin(layout: HostLayout, target: ZellijTarget): boolean {
  const asset = scrollPluginAsset();
  const hostOs = targetOs(target);
  if (!asset || !hostOs) return false;
  try {
    const file = layout.scrollPluginFile;
    const shaFile = `${file}.sha256`;
    const current = fs.existsSync(shaFile) ? fs.readFileSync(shaFile, "utf8").trim() : "";
    if (current !== asset.sha256 || !fs.existsSync(file)) {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(`${file}.partial`, asset.bytes);
      fs.renameSync(`${file}.partial`, file);
      fs.writeFileSync(shaFile, asset.sha256);
    }
    const permFile = scrollPermissionsFile(layout, hostOs, os.homedir());
    const entry = scrollPermissionsEntry(file);
    const key = entry.slice(0, entry.indexOf("\n"));
    const existing = fs.existsSync(permFile) ? fs.readFileSync(permFile, "utf8") : "";
    if (!existing.includes(key)) {
      fs.mkdirSync(path.dirname(permFile), { recursive: true });
      fs.appendFileSync(permFile, `\n${entry}`);
    }
    return true;
  } catch {
    return false;
  }
}
