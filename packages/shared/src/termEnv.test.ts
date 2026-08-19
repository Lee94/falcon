import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  appearanceFromHex,
  applyTermPtyEnv,
  hexLuminance,
  hexToOscRgb,
  isTermAppearance,
  OscColorGate,
  oscColorReplies,
  parseHexRgb,
  sanitizeColorHint,
  termPtyEnv,
} from "./termEnv.js";

describe("isTermAppearance", () => {
  it("accepts light / dark only", () => {
    assert.equal(isTermAppearance("light"), true);
    assert.equal(isTermAppearance("dark"), true);
    assert.equal(isTermAppearance("auto"), false);
    assert.equal(isTermAppearance(undefined), false);
  });
});

describe("parseHexRgb / luminance", () => {
  it("parses #rgb and #rrggbb", () => {
    assert.deepEqual(parseHexRgb("#fff"), { r: 255, g: 255, b: 255 });
    assert.deepEqual(parseHexRgb("#0a0a0a"), { r: 10, g: 10, b: 10 });
    assert.equal(parseHexRgb("red"), undefined);
  });

  it("treats white as light and near-black as dark", () => {
    assert.ok(hexLuminance("#ffffff") > 0.9);
    assert.ok(hexLuminance("#0a0a0a") < 0.05);
    assert.equal(appearanceFromHex("#ffffff"), "light");
    assert.equal(appearanceFromHex("#0a0a0a"), "dark");
    assert.equal(appearanceFromHex("#fdf6e3"), "light");
    assert.equal(appearanceFromHex("#002b36"), "dark");
  });
});

describe("termPtyEnv", () => {
  it("always declares truecolor, and stamps polarity only when known", () => {
    const bare = termPtyEnv();
    assert.equal(bare.COLORTERM, "truecolor");
    assert.equal(bare.TERM_PROGRAM, "mojito");
    assert.equal(bare.COLORFGBG, undefined);
    assert.equal(bare.GROK_APPEARANCE, undefined);

    assert.equal(termPtyEnv("dark").COLORFGBG, "15;0");
    assert.equal(termPtyEnv("dark").GROK_APPEARANCE, "dark");
    assert.equal(termPtyEnv("dark").LC_GROK_APPEARANCE, "dark");
    assert.equal(termPtyEnv("light").COLORFGBG, "0;15");
    assert.equal(termPtyEnv("light").GROK_APPEARANCE, "light");
  });

  it("drops the host terminal's COLORFGBG so iTerm 不污染网页会话", () => {
    const env = applyTermPtyEnv(
      { PATH: "/bin", COLORFGBG: "0;15", GROK_APPEARANCE: "light", EMPTY: undefined },
      "dark"
    );
    assert.equal(env.PATH, "/bin");
    assert.equal(env.COLORFGBG, "15;0");
    assert.equal(env.GROK_APPEARANCE, "dark");
    assert.equal(env.EMPTY, undefined);
  });

  it("clears inherited polarity when the client did not send one", () => {
    const env = applyTermPtyEnv({ COLORFGBG: "0;15", GROK_APPEARANCE: "light" });
    assert.equal(env.COLORFGBG, undefined);
    assert.equal(env.GROK_APPEARANCE, undefined);
    assert.equal(env.COLORTERM, "truecolor");
  });
});

describe("OscColorGate", () => {
  it("answers OSC 11 and strips the query from visible output", () => {
    const gate = new OscColorGate(() => ({ appearance: "dark", background: "#0a0a0a" }));
    const { visible, replies } = gate.push("hello\x1b]11;?\x07world");
    assert.equal(visible, "helloworld");
    assert.deepEqual(replies, [oscColorReplies({ appearance: "dark", background: "#0a0a0a" })["11"]]);
    assert.equal(hexToOscRgb("#0a0a0a"), "rgb:0a0a/0a0a/0a0a");
  });

  it("holds a split query across chunks", () => {
    const gate = new OscColorGate(() => ({ appearance: "light" }));
    const a = gate.push("pre\x1b]11;");
    assert.equal(a.visible, "pre");
    assert.deepEqual(a.replies, []);
    const b = gate.push("?\x1b\\post");
    assert.equal(b.visible, "post");
    assert.equal(b.replies.length, 1);
    assert.match(b.replies[0]!, /^\x1b]11;rgb:ffff\/ffff\/ffff\x1b\\$/);
  });

  it("passes through when appearance is unknown", () => {
    const gate = new OscColorGate(() => ({}));
    const { visible, replies } = gate.push("\x1b]11;?\x07");
    assert.equal(visible, "\x1b]11;?\x07");
    assert.deepEqual(replies, []);
  });
});

describe("sanitizeColorHint", () => {
  it("drops illegal values instead of throwing", () => {
    assert.deepEqual(sanitizeColorHint({ appearance: "auto", background: "red" }), {});
    assert.deepEqual(
      sanitizeColorHint({ appearance: "light", background: "#fff", foreground: "#171717" }),
      { appearance: "light", background: "#fff", foreground: "#171717" }
    );
  });
});
