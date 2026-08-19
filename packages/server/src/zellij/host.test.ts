import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { buildDetachedCommandLine } from "./host.js";

const PREFIX = "powershell -NoProfile -NonInteractive -EncodedCommand ";

function decode(cmd: string): string {
  assert.ok(cmd.startsWith(PREFIX), `not an EncodedCommand call: ${cmd.slice(0, 60)}`);
  return Buffer.from(cmd.slice(PREFIX.length), "base64").toString("utf16le");
}

describe("buildDetachedCommandLine", () => {
  it("wraps the inner command in a WMI Win32_Process.Create call", () => {
    const cmd = buildDetachedCommandLine(
      ["C:\\Users\\a b\\.mojito\\bin\\zellij.exe", "attach", "mj-x", "--create-background"],
      { ZELLIJ_SOCKET_DIR: "C:\\Users\\a b\\.mojito\\zellij\\sock" }
    );
    const outer = decode(cmd);
    assert.match(outer, /Invoke-CimMethod -ClassName Win32_Process -MethodName Create/);

    // 内层命令整体嵌在外层脚本的单引号里，必须只含 base64 安全字符——
    // 一旦混进引号或 $ 就会被外层 PowerShell 改写
    const m = outer.match(/CommandLine = '([^']+)'/);
    assert.ok(m, "inner CommandLine not found");
    assert.match(m[1], /^[A-Za-z0-9+/= -]+$/);

    const inner = decode(m[1]);
    assert.match(inner, /\$env:ZELLIJ_SOCKET_DIR = 'C:\\Users\\a b\\.mojito\\zellij\\sock'/);
    assert.match(
      inner,
      /& 'C:\\Users\\a b\\.mojito\\bin\\zellij\.exe' 'attach' 'mj-x' '--create-background'/
    );
  });

  it("propagates WMI failure and the inner exit code, with a bounded wait", () => {
    const outer = decode(buildDetachedCommandLine(["x.exe"]));
    assert.match(outer, /if \(\$r\.ReturnValue -ne 0\)/);
    assert.match(outer, /WaitForExit\(60000\)/);
    assert.match(outer, /exit \$p\.ExitCode/);
  });
});
