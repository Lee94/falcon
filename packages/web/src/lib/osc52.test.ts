import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { decodeOsc52Base64, osc52ClipboardText } from "./osc52.js";

function b64(text: string): string {
  return Buffer.from(text, "utf8").toString("base64");
}

describe("decodeOsc52Base64", () => {
  it("decodes ASCII", () => {
    assert.equal(decodeOsc52Base64(b64("hello")), "hello");
  });

  it("decodes CJK / emoji as UTF-8, not Latin-1", () => {
    assert.equal(decodeOsc52Base64(b64("要复制这段")), "要复制这段");
    assert.equal(decodeOsc52Base64(b64("ok ✅")), "ok ✅");
  });

  it("treats empty payload as clear", () => {
    assert.equal(decodeOsc52Base64(""), "");
    assert.equal(decodeOsc52Base64("   "), "");
  });

  it("ignores whitespace inside base64 (split OSC)", () => {
    const encoded = b64("ab");
    assert.equal(decodeOsc52Base64(`${encoded.slice(0, 2)}\n${encoded.slice(2)}`), "ab");
  });

  it("returns undefined on garbage", () => {
    assert.equal(decodeOsc52Base64("@@@@"), undefined);
  });
});

describe("osc52ClipboardText", () => {
  it("reads clipboard / primary / default selection the same", () => {
    const payload = b64("x");
    assert.equal(osc52ClipboardText(`c;${payload}`), "x");
    assert.equal(osc52ClipboardText(`p;${payload}`), "x");
    assert.equal(osc52ClipboardText(`s;${payload}`), "x");
    assert.equal(osc52ClipboardText(`;${payload}`), "x");
  });

  it("ignores clipboard queries", () => {
    assert.equal(osc52ClipboardText("c;?"), undefined);
    assert.equal(osc52ClipboardText("p;?"), undefined);
  });

  it("ignores missing payload separator", () => {
    assert.equal(osc52ClipboardText("c"), undefined);
    assert.equal(osc52ClipboardText(""), undefined);
  });
});
