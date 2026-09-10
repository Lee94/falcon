/**
 * meegle 可执行文件的定位。
 *
 * CLI 随服务内置：`@lark-project/meegle` 是 server 的锁定版本依赖，npm 包里带着六个
 * 平台的静态二进制（bin/meegle-<platform>-<arch>[.exe]），我们直接 spawn 本平台那一个，
 * 不经它的 meegle.js 包装——省一个 node 进程，也躲开包装脚本里的更新提示逻辑。
 *
 * 解析顺序：
 * 1. FALCON_MEEGLE_BIN —— 显式指定。单文件发布时 SEA bootstrap 把二进制释放到
 *    runtime 目录后也用它指入（scripts/build-binary.mjs），因为 bundle 里没有
 *    node_modules 可供解析；
 * 2. 依赖包里本平台的二进制 —— pnpm 安装的开发 / dist 运行方式；
 * 3. PATH 上的 `meegle` —— 用户自己 npm -g 装的，兜底。
 */

import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

/** 与 @lark-project/meegle 的 bin/ 目录命名一致 */
export function bundledBinName(platform = process.platform, arch = process.arch): string {
  return `meegle-${platform}-${arch}${platform === "win32" ? ".exe" : ""}`;
}

export function resolveMeegleBin(env: NodeJS.ProcessEnv = process.env): string {
  if (env.FALCON_MEEGLE_BIN) return env.FALCON_MEEGLE_BIN;
  try {
    const require = createRequire(import.meta.url);
    const pkgDir = path.dirname(require.resolve("@lark-project/meegle/package.json"));
    const bin = path.join(pkgDir, "bin", bundledBinName());
    if (fs.existsSync(bin)) return bin;
  } catch {
    // 依赖没装或不在这个平台的清单里：落到 PATH
  }
  return "meegle";
}
