/**
 * 把 sudo 包装和 askpass helper 写到宿主机 <falcon 根>/bin。
 * 本地走 node:fs；远端 POSIX 走一条 printf + chmod（virtualdir 同款）。
 */
import fs from "node:fs";
import path from "node:path";
import { joinPath } from "../git/path.js";
import { quotePosix, type HostKind } from "../zellij/host.js";
import type { AskpassHub } from "./hub.js";
import {
  ASKPASS_CONF_NAME,
  ASKPASS_NAME,
  SUDO_SHIM_NAME,
  renderAskpassConf,
  renderAskpassHelper,
  renderSudoShim,
} from "./scripts.js";

export function askpassBinDir(kind: HostKind, root: string): string {
  return joinPath(kind, root, "bin");
}

export function localAskpassBinDir(dataDir: string): string {
  return path.join(dataDir, "bin");
}

export function writeLocalAskpass(dataDir: string, hub: AskpassHub): string {
  const dir = localAskpassBinDir(dataDir);
  fs.mkdirSync(dir, { recursive: true });
  const sudo = path.join(dir, SUDO_SHIM_NAME);
  const helper = path.join(dir, ASKPASS_NAME);
  const conf = path.join(dir, ASKPASS_CONF_NAME);
  fs.writeFileSync(sudo, renderSudoShim(), { encoding: "utf8" });
  fs.writeFileSync(helper, renderAskpassHelper(), { encoding: "utf8" });
  fs.writeFileSync(conf, renderAskpassConf(hub.helperUrl(), hub.token), {
    encoding: "utf8",
  });
  fs.chmodSync(sudo, 0o755);
  fs.chmodSync(helper, 0o755);
  fs.chmodSync(conf, 0o600);
  return dir;
}

/** 远端 POSIX：建 bin、写三个文件、chmod。退出码 = 链上第一个失败者。 */
export function posixWriteAskpassCommand(dir: string, hub: AskpassHub, helperUrl: string): string {
  const files: { name: string; body: string; mode: string }[] = [
    { name: SUDO_SHIM_NAME, body: renderSudoShim(), mode: "755" },
    { name: ASKPASS_NAME, body: renderAskpassHelper(), mode: "755" },
    { name: ASKPASS_CONF_NAME, body: renderAskpassConf(helperUrl, hub.token), mode: "600" },
  ];
  const writes = files.map(
    (f) =>
      `printf %s ${quotePosix(f.body)} > "$d"/${f.name} && chmod ${f.mode} "$d"/${f.name}`
  );
  return `d=${quotePosix(dir)}; mkdir -p "$d" && ${writes.join(" && ")}`;
}

export function prependPath(env: Record<string, string>, dir: string): Record<string, string> {
  const cur = env.PATH ?? "";
  return { ...env, PATH: cur ? `${dir}:${cur}` : dir };
}
