import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { TermModeTracker } from "./termModes.js";

function tracked(...chunks: string[]): string {
  const t = new TermModeTracker();
  for (const c of chunks) t.track(c);
  return t.prefix();
}

describe("TermModeTracker", () => {
  it("默认状态不产生前缀", () => {
    assert.equal(tracked(""), "");
    assert.equal(tracked("plain text\r\nmore"), "");
  });

  it("重建 zellij attach 时设置的 alt-screen / 鼠标 / 光标模式", () => {
    const prefix = tracked("\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?25l");
    // alt-screen 必须排最前，快照内容才会画进正确的缓冲区
    assert.ok(prefix.startsWith("\x1b[?1049h"));
    assert.ok(prefix.includes("\x1b[?1002h"));
    assert.ok(prefix.includes("\x1b[?1006h"));
    assert.ok(prefix.includes("\x1b[?25l"));
  });

  it("模式关掉后不再出现在前缀里", () => {
    assert.equal(tracked("\x1b[?2004h\x1b[?2004l"), "");
    assert.equal(tracked("\x1b[?1049h\x1b[?1049l"), "");
  });

  it("默认为开的模式（DECAWM/DECTCEM）只在被关掉时才发 l", () => {
    assert.equal(tracked("\x1b[?7h"), "");
    assert.equal(tracked("\x1b[?7l"), "\x1b[?7l");
    assert.equal(tracked("\x1b[?25l\x1b[?25h"), "");
  });

  it("鼠标协议是单值状态：后设的赢，关任意一个号都整体清空", () => {
    assert.equal(tracked("\x1b[?1000h\x1b[?1002h"), "\x1b[?1002h");
    // xterm.js 的 resetModePrivate 对 9/1000/1002/1003 一律置 NONE
    assert.equal(tracked("\x1b[?1002h\x1b[?1000l"), "");
    assert.equal(tracked("\x1b[?1006h\x1b[?1016h"), "\x1b[?1016h");
    assert.equal(tracked("\x1b[?1016h\x1b[?1006l"), "");
  });

  it("多参数一条序列（CSI ? Pm;Pm h）逐个生效", () => {
    const prefix = tracked("\x1b[?1049;1002;1006h");
    assert.ok(prefix.startsWith("\x1b[?1049h"));
    assert.ok(prefix.includes("\x1b[?1002h"));
    assert.ok(prefix.includes("\x1b[?1006h"));
  });

  it("序列被 chunk 边界任意切开也能跟踪", () => {
    assert.equal(tracked("\x1b", "[?20", "04h"), "\x1b[?2004h");
    assert.equal(tracked("text\x1b[", "?1002", "h tail"), "\x1b[?1002h");
  });

  it("alt-screen 记住打开时用的变体号", () => {
    assert.equal(tracked("\x1b[?47h"), "\x1b[?47h");
    assert.equal(tracked("\x1b[?1047h"), "\x1b[?1047h");
  });

  it("ESC = / ESC > 折算到应用小键盘（?66）", () => {
    assert.equal(tracked("\x1b="), "\x1b[?66h");
    assert.equal(tracked("\x1b=\x1b>"), "");
  });

  it("RIS（ESC c）把一切回到默认", () => {
    assert.equal(tracked("\x1b[?1049h\x1b[?1002h\x1b[?7l\x1bc"), "");
  });

  it("DECSTR（CSI ! p）只重置键盘/光标类模式，不碰鼠标与 alt-screen", () => {
    const prefix = tracked("\x1b[?1049h\x1b[?1002h\x1b[?1h\x1b[?2004h\x1b[!p");
    assert.ok(prefix.includes("\x1b[?1049h"));
    assert.ok(prefix.includes("\x1b[?1002h"));
    assert.ok(!prefix.includes("\x1b[?1h"));
    assert.ok(!prefix.includes("\x1b[?2004h"));
  });

  it("OSC / DCS 载荷里的模式序列不算数", () => {
    // BEL 结尾与 ST 结尾都要吃干净；载荷里出现的 CSI 属于载荷本身
    assert.equal(tracked("\x1b]0;title with \x1b[?1002h inside\x07"), "");
    assert.equal(tracked("\x1bPq payload \x1b[?1002h more\x1b\\"), "");
    // 载荷横跨 chunk 边界同样不算
    assert.equal(tracked("\x1b]52;c;", "\x1b[?1002h", "\x07"), "");
  });

  it("非私有 CSI 与普通序列被忽略且不破坏后续解析", () => {
    assert.equal(tracked("\x1b[1h\x1b[2J\x1b[38;5;196m\x1b(0\x1b[?2004h"), "\x1b[?2004h");
  });

  it("被 ESC 打断的残缺 CSI 不吞掉后面的序列", () => {
    assert.equal(tracked("\x1b[?10\x1b[?2004h"), "\x1b[?2004h");
  });

  it("跟踪列表之外的私有模式不进前缀", () => {
    assert.equal(tracked("\x1b[?12h\x1b[?2026h\x1b[?1048h"), "");
  });

  it("?2031（亮暗通知订阅）记进 themeNotify，不进前缀", () => {
    const t = new TermModeTracker();
    assert.equal(t.themeNotify, false);
    t.track("\x1b[?2031h");
    assert.equal(t.themeNotify, true);
    assert.equal(t.prefix(), "");
    t.track("\x1b[?2031l");
    assert.equal(t.themeNotify, false);
  });

  it("2031 订阅被 RIS 清掉，DECSTR 不碰", () => {
    const t = new TermModeTracker();
    t.track("\x1b[?2031h\x1b[!p");
    assert.equal(t.themeNotify, true);
    t.track("\x1bc");
    assert.equal(t.themeNotify, false);
  });
});
