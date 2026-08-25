import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { altChord, chord, matchCommand, type Command } from "./shortcuts.js";

function key(
  code: string,
  mods: {
    alt?: boolean;
    ctrl?: boolean;
    meta?: boolean;
    shift?: boolean;
    key?: string;
  } = {}
): KeyboardEvent {
  return {
    code,
    key: mods.key ?? "",
    altKey: !!mods.alt,
    ctrlKey: !!mods.ctrl,
    metaKey: !!mods.meta,
    shiftKey: !!mods.shift,
  } as KeyboardEvent;
}

const hit = (mac: boolean, code: string, mods?: Parameters<typeof key>[1]) =>
  matchCommand(key(code, mods), mac);

describe("chord", () => {
  it("命令面板跟 VS Code 一样", () => {
    assert.equal(chord("palette", true), "⌘⇧P");
    assert.equal(chord("palette", false), "Ctrl+Shift+P");
  });

  it("其余显示键位不随命令面板一起改", () => {
    assert.equal(chord("toggleSidebar", true), "⌘B");
    assert.equal(chord("toggleFilesPanel", true), "⌘⇧E");
    assert.equal(chord("nextTab", true), "⌘⇧]");
    assert.equal(chord("newTerminal", false), "Ctrl+Shift+T");
  });
});

describe("altChord", () => {
  it("命令面板额外入口是旧的 ⌘/Ctrl+K", () => {
    assert.equal(altChord("palette", true), "⌘K");
    assert.equal(altChord("palette", false), "Ctrl+K");
  });
});

describe("matchCommand: 命令面板", () => {
  it("Mac ⌘⇧P / Win Ctrl+Shift+P 打开面板", () => {
    assert.equal(hit(true, "KeyP", { meta: true, shift: true }), "palette");
    assert.equal(hit(false, "KeyP", { ctrl: true, shift: true }), "palette");
  });

  it("F1 两平台都能打开", () => {
    assert.equal(hit(true, "F1"), "palette");
    assert.equal(hit(false, "F1"), "palette");
    assert.equal(matchCommand(key("", { key: "F1" }), true), "palette");
  });

  it("旧的 ⌘K / Ctrl+K 仍可用", () => {
    assert.equal(hit(true, "KeyK", { meta: true }), "palette");
    assert.equal(hit(false, "KeyK", { ctrl: true }), "palette");
  });
});

describe("matchCommand: Quick Open", () => {
  it("Mac ⌘P 打开文件搜索；Win 不抢终端的 Ctrl+P", () => {
    assert.equal(hit(true, "KeyP", { meta: true }), "quickOpen");
    assert.equal(hit(false, "KeyP", { ctrl: true }), null);
    assert.equal(hit(true, "KeyP", { ctrl: true }), null);
    assert.equal(chord("quickOpen", true), "⌘P");
    assert.equal(chord("quickOpen", false), "Alt+P");
  });

  it("Alt+P 两平台都能打开文件搜索", () => {
    assert.equal(hit(true, "KeyP", { alt: true }), "quickOpen");
    assert.equal(hit(false, "KeyP", { alt: true }), "quickOpen");
  });

  it("⌘⇧P / Ctrl+Shift+P 仍是命令面板，不被 Quick Open 抢走", () => {
    assert.equal(hit(true, "KeyP", { meta: true, shift: true }), "palette");
    assert.equal(hit(false, "KeyP", { ctrl: true, shift: true }), "palette");
  });
});

describe("matchCommand: VS Code 新建终端 Ctrl+Shift+`", () => {
  it("两平台 Ctrl+Shift+` 都是新建终端（Mac 也用 Ctrl 不是 ⌘）", () => {
    assert.equal(hit(true, "Backquote", { ctrl: true, shift: true }), "newTerminal");
    assert.equal(hit(false, "Backquote", { ctrl: true, shift: true }), "newTerminal");
    assert.equal(
      matchCommand(key("", { key: "`", ctrl: true, shift: true }), true),
      "newTerminal"
    );
  });

  it("Mac ⌘⇧` 不是这条绑定", () => {
    assert.equal(hit(true, "Backquote", { meta: true, shift: true }), null);
  });
});

describe("matchCommand: 其余 VS Code 对齐的键仍在", () => {
  const cases: [boolean, string, Parameters<typeof key>[1], Command][] = [
    [true, "KeyB", { meta: true }, "toggleSidebar"],
    [true, "KeyE", { meta: true, shift: true }, "toggleFilesPanel"],
    [true, "KeyG", { meta: true, shift: true }, "toggleGitPanel"],
    [true, "BracketRight", { meta: true, shift: true }, "nextTab"],
    [true, "BracketLeft", { meta: true, shift: true }, "prevTab"],
    [true, "KeyT", { meta: true }, "newTerminal"],
    [true, "KeyW", { meta: true }, "closeTab"],
    [false, "KeyE", { ctrl: true, shift: true }, "toggleFilesPanel"],
    [false, "KeyG", { ctrl: true, shift: true }, "toggleGitPanel"],
    [false, "KeyB", { ctrl: true, shift: true }, "toggleSidebar"],
    [false, "Tab", { ctrl: true }, "nextTab"],
    [false, "Tab", { ctrl: true, shift: true }, "prevTab"],
  ];
  for (const [mac, code, mods, cmd] of cases) {
    it(`${mac ? "Mac" : "Win"} ${code} → ${cmd}`, () => {
      assert.equal(hit(mac, code, mods), cmd);
    });
  }

  it("不抢终端的裸 Ctrl+字母", () => {
    assert.equal(hit(false, "KeyC", { ctrl: true }), null);
    assert.equal(hit(false, "KeyR", { ctrl: true }), null);
    assert.equal(hit(false, "KeyW", { ctrl: true }), null);
    assert.equal(hit(false, "KeyB", { ctrl: true }), null);
  });
});
