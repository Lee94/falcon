import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { matchCommand } from "../shortcuts.js";
import { routeKey, type KeyLike } from "./keyRoute.js";

interface FakeKey extends KeyLike {
  code?: string;
}

function key(partial: Partial<FakeKey> & { key: string }): FakeKey {
  return {
    type: "keydown",
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...partial,
  };
}

// 用真表：全局键的定义变了这里要跟着变，而不是各自维护一份
const macGlobal = (e: FakeKey) => matchCommand(e as unknown as KeyboardEvent, true) !== null;
const linuxGlobal = (e: FakeKey) => matchCommand(e as unknown as KeyboardEvent, false) !== null;

describe("routeKey", () => {
  it("IME 组合中的按键原样放过（Enter 确认候选不能变成真回车）", () => {
    assert.equal(routeKey(key({ key: "Enter", isComposing: true }), macGlobal, false), "ime");
    assert.equal(routeKey(key({ key: "Unidentified", keyCode: 229 }), macGlobal, false), "ime");
    assert.equal(routeKey(key({ key: "Enter", type: "keyup", isComposing: true }), macGlobal, false), "ime");
  });

  it("命中全局快捷键的按键放过，让它冒泡到 window", () => {
    assert.equal(routeKey(key({ key: "k", code: "KeyK", metaKey: true }), macGlobal, false), "global");
    assert.equal(routeKey(key({ key: "t", code: "KeyT", altKey: true }), macGlobal, false), "global");
    assert.equal(routeKey(key({ key: "`", code: "Backquote", ctrlKey: true, shiftKey: true }), linuxGlobal, false), "global");
    assert.equal(routeKey(key({ key: "F1", code: "F1" }), linuxGlobal, false), "global");
    assert.equal(routeKey(key({ key: "k", code: "KeyK", ctrlKey: true }), linuxGlobal, false), "global");
  });

  it("IME 优先于全局键（组合中按 ⌘K 也不该触发命令面板）", () => {
    assert.equal(routeKey(key({ key: "k", code: "KeyK", metaKey: true, isComposing: true }), macGlobal, false), "ime");
  });

  it("⌘C / Ctrl+Shift+C 只在有选区时复制", () => {
    assert.equal(routeKey(key({ key: "c", code: "KeyC", metaKey: true }), macGlobal, true), "copy");
    assert.equal(routeKey(key({ key: "c", code: "KeyC", metaKey: true }), macGlobal, false), "terminal");
    assert.equal(routeKey(key({ key: "C", code: "KeyC", ctrlKey: true, shiftKey: true }), linuxGlobal, true), "copy");
    assert.equal(routeKey(key({ key: "C", code: "KeyC", ctrlKey: true, shiftKey: true }), linuxGlobal, false), "terminal");
  });

  it("Ctrl+Shift+V 粘贴；⌘V 不经这里（浏览器 paste 事件）", () => {
    assert.equal(routeKey(key({ key: "V", code: "KeyV", ctrlKey: true, shiftKey: true }), linuxGlobal, false), "paste");
    assert.equal(routeKey(key({ key: "v", code: "KeyV", metaKey: true }), macGlobal, false), "terminal");
  });

  it("普通按键与 Esc / Ctrl+C 归终端", () => {
    assert.equal(routeKey(key({ key: "a", code: "KeyA" }), macGlobal, false), "terminal");
    assert.equal(routeKey(key({ key: "Escape", code: "Escape" }), macGlobal, false), "terminal");
    assert.equal(routeKey(key({ key: "c", code: "KeyC", ctrlKey: true }), macGlobal, true), "terminal");
  });

  it("keyup 不做剪贴板动作，其余规则相同", () => {
    assert.equal(routeKey(key({ key: "c", code: "KeyC", metaKey: true, type: "keyup" }), macGlobal, true), "terminal");
    assert.equal(routeKey(key({ key: "V", code: "KeyV", ctrlKey: true, shiftKey: true, type: "keyup" }), linuxGlobal, false), "terminal");
    assert.equal(routeKey(key({ key: "k", code: "KeyK", metaKey: true, type: "keyup" }), macGlobal, false), "global");
  });
});
