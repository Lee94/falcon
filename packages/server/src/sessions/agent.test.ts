import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  agentBin,
  launcherBody,
  launcherName,
  remoteLauncherPath,
  writeRemoteLauncherCommand,
} from "./agent.js";

describe("launcherBody（POSIX）", () => {
  it("经交互登录 shell 跑 CLI，退出后 exec 回登录 shell", () => {
    const body = launcherBody("claude", "posix", "/bin/zsh");
    assert.ok(body.startsWith("#!/bin/sh\n"));
    assert.ok(body.includes("command -v claude"));
    // 两处 shell：-i -l -c 外壳，与 CLI 退出后接手的那个
    assert.equal(body.split("'/bin/zsh'").length - 1, 2);
    // 纯 -l -c 是非交互，zsh 不读 .zshrc，远端 ~/.local/bin 里的 claude 会找不到
    assert.ok(body.includes("-i -l -c "));
    assert.equal(body.split("-i -l").length - 1, 2);
  });

  it("绝不读 $SHELL——Windows 那条路径上它就是这个脚本自己，会递归", () => {
    for (const agent of ["claude", "codex", "grok"] as const) {
      assert.ok(!launcherBody(agent, "posix", "/bin/bash").includes("$SHELL"));
    }
  });

  it("shell 路径里的单引号照 POSIX 规则转义", () => {
    const body = launcherBody("grok", "posix", "/home/o'brien/bin/fish");
    assert.ok(body.includes(`'/home/o'\\''brien/bin/fish'`));
  });

  it("CLI 缺失只提示不失败：仍然落回 shell", () => {
    const body = launcherBody("codex", "posix", "/bin/sh");
    assert.ok(body.includes("else printf"));
    assert.ok(body.includes("exec '/bin/sh' -i -l"));
  });
});

describe("launcherBody（Windows）", () => {
  const body = launcherBody("claude", "windows", "C:\\Windows\\System32\\powershell.exe");

  it("是 CRLF 的 .cmd，末尾交给 shell 接手", () => {
    assert.ok(body.includes("\r\n"));
    assert.ok(body.startsWith("@echo off"));
    assert.ok(body.includes(`"C:\\Windows\\System32\\powershell.exe"`));
  });

  it("提示语只用 ASCII：cmd 按 OEM 代码页读脚本，中文必乱码", () => {
    // eslint-disable-next-line no-control-regex
    assert.ok(!/[^\x00-\x7f]/.test(body));
  });
});

describe("launcherName / remoteLauncherPath", () => {
  it("按宿主机类型给后缀", () => {
    assert.equal(launcherName("claude", "posix"), "falcon-claude.sh");
    assert.equal(launcherName("claude", "windows"), "falcon-claude.cmd");
  });

  it("远端路径落在 <root>/agents 下", () => {
    assert.equal(
      remoteLauncherPath("posix", "/home/fay/.falcon", "grok"),
      "/home/fay/.falcon/agents/falcon-grok.sh"
    );
    assert.equal(
      remoteLauncherPath("windows", "C:\\Users\\fay\\.falcon", "codex"),
      "C:\\Users\\fay\\.falcon\\agents\\falcon-codex.cmd"
    );
  });
});

describe("writeRemoteLauncherCommand", () => {
  it("POSIX：建目录、写文件、加可执行位，一条 && 链", () => {
    const cmd = writeRemoteLauncherCommand("posix", "/home/fay/.falcon/agents", "claude", "/bin/zsh");
    assert.ok(cmd.startsWith("d='/home/fay/.falcon/agents'; mkdir -p \"$d\" && "));
    assert.ok(cmd.includes(`chmod 755 "$d"/falcon-claude.sh`));
  });

  it("Windows：走 -EncodedCommand，输出只有 base64 字符", () => {
    const cmd = writeRemoteLauncherCommand(
      "windows",
      "C:\\Users\\fay\\.falcon\\agents",
      "codex",
      "powershell.exe"
    );
    const b64 = cmd.split("-EncodedCommand ")[1]!;
    assert.match(b64, /^[A-Za-z0-9+/=]+$/);
    const script = Buffer.from(b64, "base64").toString("utf16le");
    assert.ok(script.includes("New-Item -ItemType Directory"));
    assert.ok(script.includes("falcon-codex.cmd"));
  });
});

describe("agentBin", () => {
  it("三个 CLI 的可执行名", () => {
    assert.equal(agentBin("claude"), "claude");
    assert.equal(agentBin("codex"), "codex");
    assert.equal(agentBin("grok"), "grok");
  });
});
