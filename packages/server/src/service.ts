import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { isSea } from "node:sea";
import { isLoopback, parseArgs } from "./config.js";

/**
 * `falcon service <install|uninstall|start|stop|status>` —— 把 falcon 注册成系统服务，
 * 由 init 守护：崩溃自动拉起、登出不退、开机自启。
 *
 * 刻意不自己写守护父进程：launchd / systemd 在这件事上比任何应用内实现都可靠
 * （自身不会挂、接管日志、处理开机时序），我们只负责生成配置并调用它们。
 *
 * install 之后的启动参数（--host / --port / --data-dir）原样写进服务配置，
 * 服务启动时仍由 parseArgs 解析——这里不做校验，两边永远一致。
 */

const LAUNCHD_LABEL = "com.falcon.server";
const SYSTEMD_NAME = "falcon";
const SERVICE_BIN_NAME = "falcon";

export function runServiceCli(argv: string[]): void {
  const cmd = argv[0];
  const serverArgs = argv.slice(1);
  const commands = ["install", "uninstall", "start", "stop", "restart", "status"];
  if (!cmd || !commands.includes(cmd)) {
    console.log(
      `用法: falcon service <${commands.join("|")}> [--host ..] [--port ..] [--data-dir ..]\n` +
        `启动参数仅 install 时生效，会原样写进服务配置。`
    );
    process.exit(cmd ? 1 : 0);
  }
  if (cmd !== "install" && serverArgs.length > 0) {
    console.error(`service ${cmd} 不接受额外参数（启动参数在 install 时指定）`);
    process.exit(1);
  }

  if (process.platform === "darwin") {
    launchd(cmd, serverArgs);
  } else if (process.platform === "linux") {
    systemd(cmd, serverArgs);
  } else {
    console.error(`service 子命令不支持 ${process.platform}`);
    process.exit(1);
  }
}

/** 服务进程的固定入口：`<dataDir>/bin/falcon` */
export function serviceBinPath(dataDir: string): string {
  return path.join(dataDir, "bin", SERVICE_BIN_NAME);
}

/**
 * 把当前 SEA 二进制拷到 dest（固定名为 falcon）。
 *
 * 发布产物叫 `falcon-v0.1.0-darwin-arm64` 这类带版本的文件名，launchd / ps /
 * 活动监视器都拿文件名当进程名。拷到固定路径后进程就叫 falcon，升级也只是
 * 覆盖同一路径。用临时文件 + rename：覆盖正在跑的同路径不会 ETXTBSY，也不会
 * 让 macOS 因改了正在映射的签名文件而 SIGKILL。
 */
export function installServiceBinary(src: string, dest: string): string {
  if (samePath(src, dest)) return dest;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const tmp = `${dest}.new-${process.pid}`;
  try {
    fs.copyFileSync(src, tmp);
    fs.chmodSync(tmp, 0o755);
    fs.renameSync(tmp, dest);
  } catch (err) {
    fs.rmSync(tmp, { force: true });
    throw err;
  }
  return dest;
}

function samePath(a: string, b: string): boolean {
  try {
    return fs.realpathSync(a) === fs.realpathSync(b);
  } catch {
    return path.resolve(a) === path.resolve(b);
  }
}

/**
 * 被守护进程的启动命令。
 * SEA：跑 `<dataDir>/bin/falcon`（没有拷过就用当前 execPath）。
 * node 跑 dist：node + 入口脚本。
 */
function programArguments(serverArgs: string[], seaBin?: string): string[] {
  const self = isSea()
    ? [seaBin ?? process.execPath]
    : [process.execPath, path.resolve(process.argv[1])];
  return [...self, ...serverArgs];
}

/** install 时：SEA 先落到固定名，再生成服务配置 */
function installProgramArguments(serverArgs: string[]): string[] {
  if (!isSea()) return programArguments(serverArgs);
  const dest = serviceBinPath(parseArgs(serverArgs).dataDir);
  return programArguments(serverArgs, installServiceBinary(process.execPath, dest));
}

function run(cmd: string, args: string[], opts: { canFail?: boolean } = {}): string {
  try {
    return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  } catch (err) {
    if (opts.canFail) return "";
    throw err;
  }
}

/** 绑非 localhost 且尚未设密码时服务会启动即退，被 init 反复拉起——提前把话说明白 */
function warnIfNonLoopback(serverArgs: string[]) {
  const config = parseArgs(serverArgs);
  if (!isLoopback(config.host)) {
    console.log(
      `注意：绑定 ${config.host}（非 localhost）要求已设置访问密码，否则服务会反复启动失败。\n` +
        `如未设置，请先在 localhost 启动并通过界面设置密码（数据目录需一致）。`
    );
  }
  return config;
}

// ---- macOS: launchd（用户级 LaunchAgent）----

function sleepMs(ms: number) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/**
 * bootout 是异步的：job 尚在卸载时立刻 bootstrap 会报 "Bootstrap failed: 5:
 * Input/output error"。先等旧 job 真正消失，bootstrap 再留少量重试兜底。
 */
function relaunchJob(domain: string, target: string, plistPath: string) {
  run("launchctl", ["bootout", target], { canFail: true });
  for (let i = 0; i < 50 && run("launchctl", ["print", target], { canFail: true }); i++) {
    sleepMs(100);
  }
  for (let i = 0; ; i++) {
    try {
      run("launchctl", ["bootstrap", domain, plistPath]);
      return;
    } catch (err) {
      if (i >= 4) throw err;
      sleepMs(300);
    }
  }
}

function launchd(cmd: string, serverArgs: string[]) {
  const plistPath = path.join(os.homedir(), "Library", "LaunchAgents", `${LAUNCHD_LABEL}.plist`);
  const domain = `gui/${process.getuid!()}`;
  const target = `${domain}/${LAUNCHD_LABEL}`;

  switch (cmd) {
    case "install": {
      const config = warnIfNonLoopback(serverArgs);
      const logDir = path.join(config.dataDir, "logs");
      fs.mkdirSync(logDir, { recursive: true }); // launchd 不会替我们建日志目录
      const logFile = path.join(logDir, "falcon.log");
      const xml = (s: string) =>
        s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
      const args = installProgramArguments(serverArgs)
        .map((a) => `    <string>${xml(a)}</string>`)
        .join("\n");
      fs.mkdirSync(path.dirname(plistPath), { recursive: true });
      fs.writeFileSync(
        plistPath,
        `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
${args}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>${xml(logFile)}</string>
  <key>StandardErrorPath</key><string>${xml(logFile)}</string>
</dict>
</plist>
`
      );
      relaunchJob(domain, target, plistPath); // 覆盖安装：旧实例（含升级前的旧二进制）先移除
      console.log(
        `已安装并启动（launchd，登录后自启，崩溃自动拉起）。\n` +
          (isSea() ? `程序: ${serviceBinPath(config.dataDir)}\n` : "") +
          `日志: ${logFile}`
      );
      break;
    }
    case "uninstall":
      run("launchctl", ["bootout", target], { canFail: true });
      fs.rmSync(plistPath, { force: true });
      console.log("已停止并移除服务。");
      break;
    case "start":
    case "restart":
      // 都是"按当前 plist 重新拉起"，覆盖二进制被替换过的场景
      if (!fs.existsSync(plistPath)) fail("尚未安装，先执行 falcon service install");
      relaunchJob(domain, target, plistPath);
      console.log(cmd === "start" ? "已启动。" : "已重启。");
      break;
    case "stop":
      // KeepAlive=true 的 job 只能 bootout 才不会被拉起；plist 保留，start 可再拉起
      run("launchctl", ["bootout", target], { canFail: true });
      console.log("已停止。");
      break;
    case "status": {
      if (!fs.existsSync(plistPath)) fail("未安装。");
      const out = run("launchctl", ["print", target], { canFail: true });
      if (!out) {
        console.log("已安装，未在运行。");
        break;
      }
      // launchctl print 输出很长且子段落里键会重复，只留每个键的首次出现
      const seen = new Set<string>();
      const picks = out
        .split("\n")
        .map((l) => l.trim())
        .filter((l) => /^(state|pid|last exit code)\s*=/.test(l))
        .filter((l) => {
          const key = l.split("=")[0].trim();
          if (seen.has(key)) return false;
          seen.add(key);
          return true;
        });
      console.log(["运行中。", ...picks].join("\n"));
      break;
    }
  }
}

// ---- Linux: systemd（root 装 system 级，普通用户装 user 级）----

function systemd(cmd: string, serverArgs: string[]) {
  if (!fs.existsSync("/run/systemd/system")) {
    if (cmd === "install") {
      // 没有 systemd（容器、alpine 等）：把 unit 打出来，交给用户接到自己的 init
      console.log("未检测到 systemd。请把以下 unit 安装到你的 init 系统：\n");
      console.log(systemdUnit(serverArgs, true));
      process.exit(1);
    }
    fail("未检测到 systemd。");
  }
  const rootMode = process.getuid!() === 0;
  const ctl = rootMode ? ["systemctl"] : ["systemctl", "--user"];
  const unitPath = rootMode
    ? `/etc/systemd/system/${SYSTEMD_NAME}.service`
    : path.join(
        process.env.XDG_CONFIG_HOME ?? path.join(os.homedir(), ".config"),
        "systemd",
        "user",
        `${SYSTEMD_NAME}.service`
      );

  switch (cmd) {
    case "install": {
      const config = warnIfNonLoopback(serverArgs);
      fs.mkdirSync(path.dirname(unitPath), { recursive: true });
      fs.writeFileSync(unitPath, systemdUnit(serverArgs, rootMode));
      run(ctl[0], [...ctl.slice(1), "daemon-reload"]);
      run(ctl[0], [...ctl.slice(1), "enable", SYSTEMD_NAME]);
      // 不用 enable --now：它对已在运行的服务是 no-op，升级重装时会继续跑旧二进制。
      // restart 对未运行的服务等于 start，首装/升级两个场景都对。
      run(ctl[0], [...ctl.slice(1), "restart", SYSTEMD_NAME]);
      console.log(
        `已安装并启动（systemd${rootMode ? "" : " --user"}，开机自启，崩溃自动拉起）。\n` +
          (isSea() ? `程序: ${serviceBinPath(config.dataDir)}\n` : "") +
          `日志: journalctl ${rootMode ? "" : "--user "}-u ${SYSTEMD_NAME} -f`
      );
      if (!rootMode) {
        console.log(
          `注意：user 级服务默认登出即停、开机不起，需执行一次:\n  sudo loginctl enable-linger ${os.userInfo().username}`
        );
      }
      break;
    }
    case "uninstall":
      run(ctl[0], [...ctl.slice(1), "disable", "--now", SYSTEMD_NAME], { canFail: true });
      fs.rmSync(unitPath, { force: true });
      run(ctl[0], [...ctl.slice(1), "daemon-reload"], { canFail: true });
      console.log("已停止并移除服务。");
      break;
    case "start":
    case "stop":
    case "restart":
      run(ctl[0], [...ctl.slice(1), cmd, SYSTEMD_NAME]);
      console.log({ start: "已启动。", stop: "已停止。", restart: "已重启。" }[cmd]);
      break;
    case "status":
      process.stdout.write(
        run(ctl[0], [...ctl.slice(1), "status", "--no-pager", SYSTEMD_NAME], { canFail: true }) ||
          "未在运行。\n"
      );
      break;
  }
}

function systemdUnit(serverArgs: string[], rootMode: boolean): string {
  // systemd 的 ExecStart 按空白切词，含空格/引号的路径必须加双引号转义
  const quote = (a: string) =>
    /[\s"\\]/.test(a) ? `"${a.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"` : a;
  const exec = installProgramArguments(serverArgs).map(quote).join(" ");
  return `[Unit]
Description=falcon terminal server
After=network.target

[Service]
ExecStart=${exec}
Restart=always
RestartSec=2

[Install]
WantedBy=${rootMode ? "multi-user.target" : "default.target"}
`;
}

function fail(msg: string): never {
  console.error(msg);
  process.exit(1);
}
