import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { buildDetachedCommandLine } from "./host.js";

const PREFIX = "powershell -NoProfile -NonInteractive -EncodedCommand ";

function decode(cmd: string): string {
  assert.ok(cmd.startsWith(PREFIX), `not an EncodedCommand call: ${cmd.slice(0, 60)}`);
  return Buffer.from(cmd.slice(PREFIX.length), "base64").toString("utf16le");
}

describe("buildDetachedCommandLine", () => {
  it("wraps a single-encoded inner command in a WMI Win32_Process.Create call", () => {
    const cmd = buildDetachedCommandLine(
      ["C:\\Users\\a b\\.mojito\\bin\\zellij.exe", "attach", "mj-x", "--create-background"],
      { ZELLIJ_SOCKET_DIR: "C:\\Users\\a b\\.mojito\\zellij\\sock" }
    );
    const outer = decode(cmd);
    assert.match(outer, /Invoke-CimMethod -ClassName Win32_Process -MethodName Create/);

    // 内层是 -Command 明文（不再二次 base64），外层单引号里出现该整段。
    // 内层脚本本身只含单引号字面量，包在 -Command "..." 里没有裸双引号歧义。
    const m = outer.match(/CommandLine = '(.+?)' \}/);
    assert.ok(m, "inner CommandLine not found");
    const inner = m[1].replace(/''/g, "'"); // 还原外层单引号 doubling
    assert.match(inner, /^powershell -NoProfile -NonInteractive -Command "/);
    assert.match(inner, /\$env:ZELLIJ_SOCKET_DIR = 'C:\\Users\\a b\\.mojito\\zellij\\sock'/);
    assert.match(
      inner,
      /& 'C:\\Users\\a b\\.mojito\\bin\\zellij\.exe' 'attach' 'mj-x' '--create-background'/
    );
    // 内层不得再是 EncodedCommand——那正是撑爆命令行的双层编码
    assert.doesNotMatch(inner, /-EncodedCommand/);
  });

  it("stays well under the cmd.exe 8191 limit even with the full attach env", () => {
    // 复刻真实 attachSession 的最坏输入：长 shell 路径 + zellij + 终端深浅 env
    const bin = "C:\\Users\\fay\\.mojito\\bin\\zellij-0.44.3.exe";
    const root = "C:\\Users\\fay\\.mojito\\zellij";
    const cmd = buildDetachedCommandLine(
      [
        bin, "--data-dir", `${root}\\data`, "attach", "mj-0123456789abcdef",
        "--create-background", "options",
        "--default-layout", `${root}\\layouts\\mojito.kdl`,
        "--default-mode", "locked", "--pane-frames", "false",
        "--simplified-ui", "true", "--session-serialization", "false",
        "--scroll-buffer-size", "2000", "--show-startup-tips", "false",
        "--show-release-notes", "false", "--default-cwd", "C:\\Users\\fay",
        "--default-shell", "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
      ],
      {
        ZELLIJ_SOCKET_DIR: `${root}\\sock`,
        ZELLIJ_CONFIG_DIR: `${root}\\config`,
        XDG_CACHE_HOME: `${root}\\cache`,
        TERM: "xterm-256color", COLORTERM: "truecolor", TERM_PROGRAM: "mojito",
        COLORFGBG: "15;0", GROK_APPEARANCE: "dark", LC_GROK_APPEARANCE: "dark",
      }
    );
    assert.ok(cmd.length < 8000, `command line too long: ${cmd.length}`);
  });

  it("propagates WMI failure and the inner exit code, with a bounded wait", () => {
    const outer = decode(buildDetachedCommandLine(["x.exe"]));
    assert.match(outer, /if \(\$r\.ReturnValue -ne 0\)/);
    assert.match(outer, /WaitForExit\(60000\)/);
    assert.match(outer, /exit \$p\.ExitCode/);
  });
});
