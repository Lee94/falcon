import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createBackgroundArgs,
  parseTerminalPaneId,
  zellijSessionName,
} from "./command.js";
import { hostLayout } from "./host.js";

describe("parseTerminalPaneId", () => {
  it("skips the header and plugin panes", () => {
    const stdout = [
      "PANE_ID  TYPE  TITLE",
      "plugin_0  plugin  (.) - zellij:link",
      "terminal_0  terminal  Chrome PWA Install App Feature Support - grok",
    ].join("\n");
    assert.equal(parseTerminalPaneId(stdout), "terminal_0");
  });

  it("returns null when there is no terminal pane", () => {
    assert.equal(parseTerminalPaneId("PANE_ID  TYPE  TITLE\n"), null);
    assert.equal(parseTerminalPaneId(""), null);
  });
});

describe("createBackgroundArgs", () => {
  it("creates detached with the full session options", () => {
    const layout = hostLayout(
      "windows",
      "C:\\Users\\fay\\.mojito",
      "x86_64-pc-windows-msvc"
    );
    const sessionId = "0f0e0d0c-0b0a-0908-0706-050403020100";
    const args = createBackgroundArgs(layout, sessionId, {
      cwd: "C:\\code",
      shell: "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    });

    assert.ok(args.includes(zellijSessionName(sessionId)));
    assert.ok(args.includes("--create-background"));
    // 会话级 options 只在创建时生效，必须全量出现在这里（而不是只在 attach 时给）
    assert.ok(args.indexOf("options") > args.indexOf("--create-background"));
    assert.equal(args[args.indexOf("--default-layout") + 1], layout.layoutFile);
    assert.equal(args[args.indexOf("--default-cwd") + 1], "C:\\code");
    assert.equal(
      args[args.indexOf("--default-shell") + 1],
      "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
    );
  });
});
