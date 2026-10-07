import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  attachArgs,
  CONFIG_BODY,
  createBackgroundArgs,
  parseClientRunningCommand,
  parseScrollReply,
  parseTerminalPaneId,
  parseTerminalPaneNumber,
  scrollConfigBody,
  scrollPermissionsEntry,
  scrollPermissionsFile,
  scrollPipeArgs,
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

describe("parseClientRunningCommand", () => {
  const header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND";

  it("命令行含空格时第三列起整段拼回", () => {
    const stdout = [header, "1         terminal_0     sleep 300"].join("\n");
    assert.equal(parseClientRunningCommand(stdout), "sleep 300");
  });

  it("空闲 shell 的 N/A 按'没有程序'处理", () => {
    const stdout = [header, "1         terminal_0     N/A"].join("\n");
    assert.equal(parseClientRunningCommand(stdout), null);
  });

  it("没有客户端连接时只有表头", () => {
    assert.equal(parseClientRunningCommand(`${header}\n`), null);
    assert.equal(parseClientRunningCommand(""), null);
  });

  it("聚焦在 plugin pane 上没有可言的前台命令", () => {
    const stdout = [header, "1         plugin_3       zellij:configuration"].join("\n");
    assert.equal(parseClientRunningCommand(stdout), null);
  });
});

describe("createBackgroundArgs", () => {
  it("creates detached with the full session options", () => {
    const layout = hostLayout(
      "windows",
      "C:\\Users\\fay\\.falcon",
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

describe("滚动位置插件的会话配置（ADR 0019）", () => {
  const layout = hostLayout("posix", "/home/fay/.falcon", "x86_64-unknown-linux-musl");
  const sessionId = "0f0e0d0c-0b0a-0908-0706-050403020100";

  it("老配置照旧关边框，不碰插件", () => {
    assert.match(CONFIG_BODY, /^pane_frames false$/m);
    assert.doesNotMatch(CONFIG_BODY, /load_plugins|pane_frame_style/);
  });

  it("带插件的配置用 titles 样式并随会话加载插件，其余与老配置相同", () => {
    const body = scrollConfigBody(layout.scrollPluginFile);
    // pane_frames false 会把样式压成 None，插件就收不到 ActivePaneScroll
    assert.doesNotMatch(body, /pane_frames/);
    assert.match(body, /^pane_frame_style "titles"$/m);
    assert.ok(
      body.includes(`load_plugins {\n    "file:/home/fay/.falcon/zellij/plugins/falcon-scroll.wasm"\n}`)
    );
    const common = (b: string) =>
      b.split("\n").filter((l) => !/pane_frame|load_plugins|falcon-scroll|^}$/.test(l));
    assert.deepEqual(common(body), common(CONFIG_BODY));
  });

  it("插件路径里的引号与反斜杠按 KDL 转义", () => {
    assert.ok(scrollConfigBody('/a "b"\\c.wasm').includes('"file:/a \\"b\\"\\\\c.wasm"'));
  });

  it("老会话 attach：关边框、读默认 config.kdl", () => {
    const args = attachArgs(layout, sessionId, { shell: "/bin/zsh" });
    assert.ok(!args.includes("--config"));
    assert.equal(args[args.indexOf("--pane-frames") + 1], "false");
    assert.ok(!args.includes("--pane-frame-style"));
  });

  it("带插件的会话 attach：--config 指 scroll.kdl 且在 attach 之前，titles 样式，不传 --pane-frames", () => {
    const args = attachArgs(layout, sessionId, { shell: "/bin/zsh", scroll: true });
    assert.equal(args[args.indexOf("--config") + 1], layout.scrollConfigFile);
    assert.ok(args.indexOf("--config") < args.indexOf("attach"));
    assert.equal(args[args.indexOf("--pane-frame-style") + 1], "titles");
    assert.ok(!args.includes("--pane-frames"));
    // scroll_mode_sync 两套都要关（0.45 默认开着会吞按键）
    assert.equal(args[args.indexOf("--scroll-mode-sync") + 1], "false");
  });
});

describe("scrollPipeArgs", () => {
  const layout = hostLayout("posix", "/home/fay/.falcon", "x86_64-unknown-linux-musl");
  const sessionId = "0f0e0d0c-0b0a-0908-0706-050403020100";

  it("按名字广播、不带 --plugin（没插件的会话不能被现拉一个浮动实例）", () => {
    const args = scrollPipeArgs(layout, sessionId, 0);
    assert.ok(!args.includes("--plugin"));
    assert.equal(args[args.indexOf("--session") + 1], zellijSessionName(sessionId));
    assert.equal(args[args.indexOf("--name") + 1], "falcon-scroll");
    assert.deepEqual(args.slice(-2), ["--", "get 0"]);
  });

  it("seek 取整并夹到非负", () => {
    assert.equal(scrollPipeArgs(layout, sessionId, 3, 120.6).at(-1), "seek 3 121");
    assert.equal(scrollPipeArgs(layout, sessionId, 3, -5).at(-1), "seek 3 0");
  });
});

describe("parseScrollReply", () => {
  it("一行两个数", () => {
    assert.deepEqual(parseScrollReply("97 173\n"), { position: 97, length: 173 });
    assert.deepEqual(parseScrollReply("0 0\n"), { position: 0, length: 0 });
  });

  it("空回话（会话里没有插件）与乱码都是 null", () => {
    assert.equal(parseScrollReply(""), null);
    assert.equal(parseScrollReply("hello"), null);
    assert.equal(parseScrollReply("1 2 3"), null);
  });

  it("position 不可能大于 length", () => {
    assert.equal(parseScrollReply("9 3"), null);
  });
});

describe("parseTerminalPaneNumber", () => {
  it("只认 terminal pane", () => {
    assert.equal(parseTerminalPaneNumber("terminal_0"), 0);
    assert.equal(parseTerminalPaneNumber("terminal_12"), 12);
    assert.equal(parseTerminalPaneNumber("plugin_0"), null);
  });
});

describe("插件预授权", () => {
  const layout = hostLayout("posix", "/home/fay/.falcon", "x86_64-unknown-linux-musl");

  it("Linux 跟 XDG_CACHE_HOME（falcon 自己的 cache 目录），macOS 是写死的 Library/Caches", () => {
    assert.equal(
      scrollPermissionsFile(layout, "linux", "/home/fay"),
      "/home/fay/.falcon/zellij/cache/zellij/permissions.kdl"
    );
    assert.equal(
      scrollPermissionsFile(layout, "darwin", "/Users/fay/"),
      "/Users/fay/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl"
    );
  });

  it("键是插件路径本身，不带 file: 前缀", () => {
    const entry = scrollPermissionsEntry("/p/falcon-scroll.wasm");
    assert.equal(entry.split("\n")[0], '"/p/falcon-scroll.wasm" {');
    for (const p of ["ReadApplicationState", "ChangeApplicationState", "ReadCliPipes", "ReadPaneContents"]) {
      assert.match(entry, new RegExp(`^    ${p}$`, "m"));
    }
    assert.ok(entry.endsWith("}\n"));
  });
});
