import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { describe, it } from "node:test";
import {
  expandHome,
  localPx0Target,
  parseListenPort,
  parsePx0Version,
  posixInstallCommand,
  posixLaunchCommand,
  posixVersionCommand,
  PX0_SHA256,
  PX0_VERSION,
  px0Args,
  px0AssetName,
  px0DownloadUrl,
  px0TargetFromUname,
  remotePx0Path,
  tailLines,
} from "./command.js";

describe("px0TargetFromUname", () => {
  it("maps Linux / Darwin on x86_64 and arm64", () => {
    assert.deepEqual(px0TargetFromUname("Linux x86_64"), { os: "linux", arch: "amd64" });
    assert.deepEqual(px0TargetFromUname("Linux aarch64\n"), { os: "linux", arch: "arm64" });
    assert.deepEqual(px0TargetFromUname("Darwin arm64"), { os: "darwin", arch: "arm64" });
    assert.deepEqual(px0TargetFromUname("Darwin x86_64"), { os: "darwin", arch: "amd64" });
  });

  it("returns null for platforms we do not ship", () => {
    assert.equal(px0TargetFromUname("FreeBSD amd64"), null);
    assert.equal(px0TargetFromUname("Linux armv7l"), null);
    assert.equal(px0TargetFromUname(""), null);
  });
});

describe("localPx0Target", () => {
  it("maps node platform / arch", () => {
    assert.deepEqual(localPx0Target("darwin", "arm64"), { os: "darwin", arch: "arm64" });
    assert.deepEqual(localPx0Target("linux", "x64"), { os: "linux", arch: "amd64" });
    assert.deepEqual(localPx0Target("win32", "x64"), { os: "windows", arch: "amd64" });
    assert.equal(localPx0Target("linux", "ia32"), null);
    assert.equal(localPx0Target("freebsd", "x64"), null);
  });
});

describe("px0AssetName / PX0_SHA256", () => {
  it("names assets like the GitHub release", () => {
    assert.equal(px0AssetName({ os: "linux", arch: "amd64" }), `px0-${PX0_VERSION}-linux-amd64`);
    assert.equal(
      px0AssetName({ os: "windows", arch: "arm64" }),
      `px0-${PX0_VERSION}-windows-arm64.exe`
    );
  });

  it("pins a sha256 for every supported target of the locked version", () => {
    for (const os of ["linux", "darwin", "windows"] as const) {
      for (const arch of ["amd64", "arm64"] as const) {
        const hash = PX0_SHA256[px0AssetName({ os, arch })];
        assert.match(hash ?? "", /^[0-9a-f]{64}$/, `${os}/${arch}`);
      }
    }
  });

  it("downloads the locked tag, not latest", () => {
    assert.equal(
      px0DownloadUrl("px0-0.1.10-linux-amd64", "0.1.10"),
      "https://github.com/px0-ai/px0/releases/download/v0.1.10/px0-0.1.10-linux-amd64"
    );
  });
});

describe("parsePx0Version", () => {
  it("reads `px0 -version`", () => {
    assert.equal(parsePx0Version("px0 0.1.10 (linux/amd64)\n"), "0.1.10");
    assert.equal(parsePx0Version("sh: 1: /x/px0-0.1.10: not found"), null);
    assert.equal(parsePx0Version(""), null);
  });
});

describe("parseListenPort", () => {
  it("reads the url line of px0's banner", () => {
    const banner = [
      "",
      "px0 0.1.16",
      "  workspace:  /Users/fay/Code/mojito",
      "  url:        http://127.0.0.1:55342/px0/abc/",
      "",
      "ctrl-c to stop",
    ].join("\n");
    assert.equal(parseListenPort(banner), 55342);
  });

  it("copes with pty CRLF, colors and a motd before the banner", () => {
    const text =
      "Welcome to Ubuntu 24.04\r\nLast login: today\r\n" +
      "  url:        \x1b[36mhttp://127.0.0.1:41234/px0/p1/\x1b[0m\r\n";
    assert.equal(parseListenPort(text), 41234);
  });

  it("ignores unrelated URLs and an incomplete line", () => {
    assert.equal(parseListenPort("see https://px0.ai/docs"), null);
    assert.equal(parseListenPort("  url:        http://127.0.0.1:41"), null);
    assert.equal(parseListenPort("url: http://0.0.0.0:7777/"), null);
  });
});

describe("px0Args", () => {
  it("binds loopback on a free port, no browser, no telemetry, no self-update", () => {
    const args = px0Args({ basePath: "/px0/p1/", dir: "/srv/app" });
    const flag = (name: string) => args[args.indexOf(name) + 1];
    assert.equal(flag("-host"), "127.0.0.1");
    assert.equal(flag("-port"), "0");
    assert.equal(flag("-base-path"), "/px0/p1/");
    assert.ok(args.includes("-no-open"));
    assert.ok(args.includes("-no-telemetry"));
    // 0.1.11 起默认启动时自我更新、原地重新 exec，换掉钉死哈希的二进制
    assert.ok(args.includes("-no-update"));
    // -quiet 会把 url 行也吞掉，端口就解析不出来了
    assert.ok(!args.includes("-quiet"));
    assert.equal(args.at(-1), "/srv/app");
  });
});

describe("remote paths", () => {
  it("puts the binary next to zellij under <root>/bin with the version", () => {
    assert.equal(remotePx0Path("/home/u/.falcon/"), `/home/u/.falcon/bin/px0-${PX0_VERSION}`);
  });

  it("expands ~ in the working dir", () => {
    assert.equal(expandHome("~", "/home/u"), "/home/u");
    assert.equal(expandHome("~/code/app", "/home/u"), "/home/u/code/app");
    assert.equal(expandHome("/srv/~x", "/home/u"), "/srv/~x");
  });
});

/** 在本机 sh 上真跑一遍拼出来的命令行，验引号与退出码 */
function sh(cmd: string, input?: Buffer | string): string {
  return execFileSync("/bin/sh", ["-c", cmd], { input, encoding: "utf8" });
}

describe("posix commands", { skip: process.platform === "win32" }, () => {
  it("install writes stdin atomically and makes it executable", async () => {
    const fs = await import("node:fs");
    const os = await import("node:os");
    const path = await import("node:path");
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "falcon px0 '"));
    try {
      const file = path.join(tmp, "bin", "px0-x");
      sh(posixInstallCommand(file), "#!/bin/sh\necho 'px0 9.9.9 (test)'\n");
      assert.equal(fs.existsSync(`${file}.partial`), false);
      assert.equal(parsePx0Version(sh(posixVersionCommand(file))), "9.9.9");
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("version probe of a missing binary yields no version (and no throw on stdout)", () => {
    let out = "";
    try {
      out = sh(posixVersionCommand("/nonexistent/px0 it's"));
    } catch (err) {
      out = String((err as { stdout?: string }).stdout ?? "");
    }
    assert.equal(parsePx0Version(out), null);
  });

  it("launch runs the binary with args intact through a login shell", () => {
    const out = sh(
      posixLaunchCommand("/bin/sh", "/usr/bin/printf", ["%s|", "a b", "it's", "$HOME"])
    );
    assert.equal(out, "a b|it's|$HOME|");
  });
});

describe("tailLines", () => {
  it("keeps the last lines without colors or blanks", () => {
    assert.equal(tailLines("a\r\n\r\n\x1b[31mb\x1b[0m\nc\n", 2), "b\nc");
  });
});
