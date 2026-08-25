import os from "node:os";
import path from "node:path";
import fs from "node:fs";

export interface ServerConfig {
  host: string;
  port: number;
  dataDir: string;
}

export function isLoopback(host: string): boolean {
  return host === "127.0.0.1" || host === "::1" || host === "localhost";
}

/**
 * 默认数据目录。产品从 Mojito 改名为 Falcon 后根目录是 `~/.falcon`；
 * 若新目录还不存在而旧的 `~/.mojito` 在，继续用旧的，避免用户丢库。
 * 本机不能 `mv`：SQLite 开在这个目录里，进程还活着就抽走会把库弄丢。
 * 远端由 SshLink.probe 在探测之后尝试把 `~/.mojito` 改名为 `~/.falcon`，
 * 迁不了则沿用旧路径（见 host.ts migrateRemoteRootCommand）。
 *
 * SEA bootstrap（scripts/build-binary.mjs）必须与这里同一套回退，改一处时两边一起看。
 */
export function defaultDataDir(home = os.homedir()): string {
  const next = path.join(home, ".falcon");
  const prev = path.join(home, ".mojito");
  if (!fs.existsSync(next) && fs.existsSync(prev)) return prev;
  return next;
}

export function parseArgs(argv: string[]): ServerConfig {
  const config: ServerConfig = {
    host: process.env.FALCON_HOST ?? process.env.MOJITO_HOST ?? "127.0.0.1",
    port: Number(process.env.FALCON_PORT ?? process.env.MOJITO_PORT ?? 4923),
    dataDir:
      process.env.FALCON_DATA_DIR ?? process.env.MOJITO_DATA_DIR ?? defaultDataDir(),
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--host") config.host = argv[++i];
    else if (arg === "--port") config.port = Number(argv[++i]);
    else if (arg === "--data-dir") config.dataDir = argv[++i];
  }
  fs.mkdirSync(config.dataDir, { recursive: true });
  return config;
}
