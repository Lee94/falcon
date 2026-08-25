import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import {
  buildDetachedCommandLine,
  legacyRemoteRoot,
  migrateRemoteRootCommand,
  parseMigratedRoot,
  posixMigrateRootScript,
  remoteRoot,
  windowsMigrateRootScript,
} from "./host.js";

const PREFIX = "powershell -NoProfile -NonInteractive -EncodedCommand ";

function decode(cmd: string): string {
  assert.ok(cmd.startsWith(PREFIX), `not an EncodedCommand call: ${cmd.slice(0, 60)}`);
  return Buffer.from(cmd.slice(PREFIX.length), "base64").toString("utf16le");
}

describe("remoteRoot / legacyRemoteRoot", () => {
  it("joins under home with the host separator", () => {
    assert.equal(remoteRoot("posix", "/home/u"), "/home/u/.falcon");
    assert.equal(legacyRemoteRoot("posix", "/home/u"), "/home/u/.mojito");
    assert.equal(remoteRoot("windows", "C:\\Users\\a b"), "C:\\Users\\a b\\.falcon");
    assert.equal(legacyRemoteRoot("windows", "C:\\Users\\a b"), "C:\\Users\\a b\\.mojito");
  });

  it("strips a trailing separator so we do not get .falcon nested under empty", () => {
    assert.equal(remoteRoot("posix", "/home/u/"), "/home/u/.falcon");
    assert.equal(remoteRoot("windows", "C:\\Users\\u\\"), "C:\\Users\\u\\.falcon");
  });
});

describe("migrateRemoteRoot", () => {
  it("posix: quotes both paths and mv-falls-back so a failed rename still has a root", () => {
    const script = posixMigrateRootScript("/home/o'brien/.falcon", "/home/o'brien/.mojito");
    assert.match(script, /n='\/home\/o'\\''brien\/\.falcon'/);
    assert.match(script, /p='\/home\/o'\\''brien\/\.mojito'/);
    assert.match(script, /mv "\$p" "\$n" \|\| true/);
    assert.match(script, /\[ -d "\$n" \]/);
    assert.match(script, /\[ -d "\$p" \]/);
  });

  it("windows: Move-Item with -LiteralPath, catch keeps the old dir if locked", () => {
    const script = windowsMigrateRootScript("C:\\Users\\a b\\.falcon", "C:\\Users\\a b\\.mojito");
    assert.ok(script.includes("$n = 'C:\\Users\\a b\\.falcon'"));
    assert.ok(script.includes("Move-Item -LiteralPath $p -Destination $n -ErrorAction Stop"));
    assert.ok(script.includes("Test-Path -LiteralPath $p -PathType Container"));
    assert.ok(script.includes("catch {}"));
  });

  it("windows command is a single EncodedCommand so DefaultShell cannot rewrite it", () => {
    const cmd = migrateRemoteRootCommand(
      "windows",
      "C:\\Users\\a b\\.falcon",
      "C:\\Users\\a b\\.mojito"
    );
    assert.ok(cmd.startsWith(PREFIX));
    const inner = decode(cmd);
    assert.match(inner, /Move-Item -LiteralPath \$p -Destination \$n/);
    assert.doesNotMatch(inner, /-EncodedCommand/);
  });

  it("posix command is the raw script — no extra wrapping", () => {
    const next = "/home/u/.falcon";
    const prev = "/home/u/.mojito";
    assert.equal(migrateRemoteRootCommand("posix", next, prev), posixMigrateRootScript(next, prev));
  });

  it("parseMigratedRoot takes the first non-empty line and ignores CR", () => {
    assert.equal(parseMigratedRoot("/home/u/.falcon\n"), "/home/u/.falcon");
    assert.equal(parseMigratedRoot("\r\nC:\\Users\\u\\.mojito\r\n"), "C:\\Users\\u\\.mojito");
    assert.equal(parseMigratedRoot("   \n  \n"), null);
    assert.equal(parseMigratedRoot(""), null);
  });

  function withHome(fn: (home: string) => void) {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-mig-"));
    try {
      fn(home);
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  }

  function runPosix(next: string, prev: string): string {
    return execFileSync("sh", ["-c", posixMigrateRootScript(next, prev)], {
      encoding: "utf8",
    });
  }

  it("posix live: mv ~/.mojito to ~/.falcon when the new dir is absent", () => {
    withHome((home) => {
      const next = path.join(home, ".falcon");
      const prev = path.join(home, ".mojito");
      fs.mkdirSync(prev);
      fs.writeFileSync(path.join(prev, "marker"), "ok");
      assert.equal(runPosix(next, prev).trim(), next);
      assert.equal(fs.existsSync(prev), false);
      assert.equal(fs.readFileSync(path.join(next, "marker"), "utf8"), "ok");
    });
  });

  it("posix live: keeps ~/.falcon when both dirs exist, does not merge", () => {
    withHome((home) => {
      const next = path.join(home, ".falcon");
      const prev = path.join(home, ".mojito");
      fs.mkdirSync(next);
      fs.mkdirSync(prev);
      fs.writeFileSync(path.join(next, "new"), "1");
      fs.writeFileSync(path.join(prev, "old"), "2");
      assert.equal(runPosix(next, prev).trim(), next);
      assert.equal(fs.existsSync(path.join(prev, "old")), true);
      assert.equal(fs.existsSync(path.join(next, "old")), false);
    });
  });

  it("posix live: after a successful mv, a second run is a no-op on ~/.falcon", () => {
    withHome((home) => {
      const next = path.join(home, ".falcon");
      const prev = path.join(home, ".mojito");
      fs.mkdirSync(next);
      fs.writeFileSync(path.join(next, "marker"), "ok");
      assert.equal(runPosix(next, prev).trim(), next);
      assert.equal(fs.readFileSync(path.join(next, "marker"), "utf8"), "ok");
    });
  });

  it("posix live: prints the new path when neither dir exists (fresh host)", () => {
    withHome((home) => {
      const next = path.join(home, ".falcon");
      const prev = path.join(home, ".mojito");
      assert.equal(runPosix(next, prev).trim(), next);
      assert.equal(fs.existsSync(next), false);
    });
  });
});

describe("buildDetachedCommandLine", () => {
  it("wraps a single-encoded inner command in a WMI Win32_Process.Create call", () => {
    const cmd = buildDetachedCommandLine(
      ["C:\\Users\\a b\\.falcon\\bin\\zellij.exe", "attach", "mj-x", "--create-background"],
      { ZELLIJ_SOCKET_DIR: "C:\\Users\\a b\\.falcon\\zellij\\sock" }
    );
    const outer = decode(cmd);
    assert.match(outer, /Invoke-CimMethod -ClassName Win32_Process -MethodName Create/);

    // 内层是 -Command 明文（不再二次 base64），外层单引号里出现该整段。
    // 内层脚本本身只含单引号字面量，包在 -Command "..." 里没有裸双引号歧义。
    const m = outer.match(/CommandLine = '(.+?)' \}/);
    assert.ok(m, "inner CommandLine not found");
    const inner = m[1].replace(/''/g, "'"); // 还原外层单引号 doubling
    assert.match(inner, /^powershell -NoProfile -NonInteractive -Command "/);
    assert.match(inner, /\$env:ZELLIJ_SOCKET_DIR = 'C:\\Users\\a b\\.falcon\\zellij\\sock'/);
    assert.match(
      inner,
      /& 'C:\\Users\\a b\\.falcon\\bin\\zellij\.exe' 'attach' 'mj-x' '--create-background'/
    );
    // 内层不得再是 EncodedCommand——那正是撑爆命令行的双层编码
    assert.doesNotMatch(inner, /-EncodedCommand/);
  });

  it("stays well under the cmd.exe 8191 limit even with the full attach env", () => {
    // 复刻真实 attachSession 的最坏输入：长 shell 路径 + zellij + 终端深浅 env
    const bin = "C:\\Users\\fay\\.falcon\\bin\\zellij-0.44.3.exe";
    const root = "C:\\Users\\fay\\.falcon\\zellij";
    const cmd = buildDetachedCommandLine(
      [
        bin, "--data-dir", `${root}\\data`, "attach", "mj-0123456789abcdef",
        "--create-background", "options",
        "--default-layout", `${root}\\layouts\\falcon.kdl`,
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
        TERM: "xterm-256color", COLORTERM: "truecolor", TERM_PROGRAM: "falcon",
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
