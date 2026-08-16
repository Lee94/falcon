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

export function parseArgs(argv: string[]): ServerConfig {
  const config: ServerConfig = {
    host: process.env.MOJITO_HOST ?? "127.0.0.1",
    port: Number(process.env.MOJITO_PORT ?? 4923),
    dataDir: process.env.MOJITO_DATA_DIR ?? path.join(os.homedir(), ".mojito"),
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
