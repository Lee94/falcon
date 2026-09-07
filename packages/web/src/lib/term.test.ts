import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  BERKELEY_FONT_FAMILY,
  DEFAULT_TERM_PREF,
  IOSKELEY_FONT_FAMILY,
  MAPLE_FONT_FAMILY,
  NERD_FONT_FAMILY,
  sanitizeTermPref,
  termFontStack,
} from "./term.js";

describe("sanitizeTermPref", () => {
  it("空对象就是默认", () => {
    assert.deepEqual(sanitizeTermPref({}), DEFAULT_TERM_PREF);
  });

  it("旧版的 themeId 直接丢掉，其余字段照常", () => {
    const pref = sanitizeTermPref({ fontSize: 16, themeId: "dracula", engine: "rio" } as Partial<typeof DEFAULT_TERM_PREF>);
    assert.equal(pref.fontSize, 16);
    assert.equal(pref.engine, "rio");
    assert.ok(!("themeId" in pref));
  });

  it("越界值夹回范围，非法枚举回默认", () => {
    const pref = sanitizeTermPref({ fontSize: 99, lineHeight: 0.2, cursorStyle: "weird" as never, fontId: "nope" as never });
    assert.equal(pref.fontSize, 24);
    assert.equal(pref.lineHeight, 1);
    assert.equal(pref.cursorStyle, "block");
    assert.equal(pref.fontId, "berkeley");
  });
});

describe("termFontStack", () => {
  it("图标字体打头，默认正文字体紧随其后，内置的 Ioskeley / Maple 依次兜底", () => {
    const stack = termFontStack(DEFAULT_TERM_PREF);
    assert.ok(
      stack.startsWith(
        `"${NERD_FONT_FAMILY}", "${BERKELEY_FONT_FAMILY}", "${IOSKELEY_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}"`
      ),
      stack
    );
  });
  it("选内置的 Ioskeley 时它自己不重复列", () => {
    const stack = termFontStack({ ...DEFAULT_TERM_PREF, fontId: "ioskeley" });
    assert.ok(
      stack.startsWith(`"${NERD_FONT_FAMILY}", "${IOSKELEY_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}"`),
      stack
    );
    assert.equal(stack.split(IOSKELEY_FONT_FAMILY).length - 1, 1, stack);
  });
  it("选 Maple 时图标字体打头，Maple 紧随其后", () => {
    const stack = termFontStack({ ...DEFAULT_TERM_PREF, fontId: "maple" });
    assert.ok(stack.startsWith(`"${NERD_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}"`), stack);
  });
  it("自定义字体夹在图标字体与 Maple 之间，空的退回 Maple", () => {
    assert.ok(
      termFontStack({ ...DEFAULT_TERM_PREF, fontId: "custom", customFamily: "Sarasa Term SC" }).startsWith(
        `"${NERD_FONT_FAMILY}", "Sarasa Term SC", "${MAPLE_FONT_FAMILY}"`
      )
    );
    assert.ok(
      termFontStack({ ...DEFAULT_TERM_PREF, fontId: "custom", customFamily: "  " }).startsWith(
        `"${NERD_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}"`
      )
    );
  });
});
