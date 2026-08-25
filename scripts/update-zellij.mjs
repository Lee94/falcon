#!/usr/bin/env node
/**
 * 更新 falcon 锁定的 Zellij 版本。
 *
 *   node scripts/update-zellij.mjs 0.44.4
 *   node scripts/update-zellij.mjs --latest
 *
 * 只改 packages/server/src/zellij/version.ts 里的版本常量，并校验该 tag 下
 * 我们需要的五个 target 产物都存在——不校验哈希，因为二进制由宿主机自己下载、
 * 后端不经手（详见 ADR 0001 的"供应链与授权"一节）。
 *
 * 升级版本是一次有意的发版行为：改完请跑一遍真实远端的手工验证再发。
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const VERSION_FILE = path.join(
  ROOT,
  "packages/server/src/zellij/version.ts"
);

const TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
];

async function resolveVersion(arg) {
  if (arg && arg !== "--latest") return arg.replace(/^v/, "");
  const res = await fetch(
    "https://api.github.com/repos/zellij-org/zellij/releases/latest",
    { headers: { accept: "application/vnd.github+json" } }
  );
  if (!res.ok) throw new Error(`查询最新版本失败：HTTP ${res.status}`);
  return (await res.json()).tag_name.replace(/^v/, "");
}

async function main() {
  const version = await resolveVersion(process.argv[2]);
  console.log(`目标版本：v${version}`);

  const res = await fetch(
    `https://api.github.com/repos/zellij-org/zellij/releases/tags/v${version}`,
    { headers: { accept: "application/vnd.github+json" } }
  );
  if (!res.ok) throw new Error(`找不到 release v${version}（HTTP ${res.status}）`);
  const names = new Set((await res.json()).assets.map((a) => a.name));

  // 我们用 no-web 变体：省约 8 MiB，也少一个不需要的监听端口
  const missing = TARGETS.filter((t) => {
    const ext = t.includes("windows") ? "zip" : "tar.gz";
    return !names.has(`zellij-no-web-${t}.${ext}`);
  });
  if (missing.length) {
    throw new Error(`v${version} 缺少这些 target 的 no-web 产物：\n  ${missing.join("\n  ")}`);
  }

  const src = fs.readFileSync(VERSION_FILE, "utf8");
  const next = src.replace(
    /export const ZELLIJ_VERSION = "[^"]+";/,
    `export const ZELLIJ_VERSION = "${version}";`
  );
  if (next === src) {
    console.log("版本未变化，无需修改。");
    return;
  }
  fs.writeFileSync(VERSION_FILE, next);
  console.log(`已更新 ${path.relative(ROOT, VERSION_FILE)}`);
  console.log(
    "\n提醒：Zellij 的 CLIENT_SERVER_CONTRACT_VERSION 若在此版本改变，" +
      "宿主机上已有的会话将无法接回。发版前请确认 CHANGELOG。"
  );
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
