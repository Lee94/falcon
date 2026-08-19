import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  TERM_THEME_GROUPS,
  appearanceFromTheme,
  resolveTermTheme,
  type TermThemeId,
} from "./term.js";

describe("appearanceFromTheme", () => {
  it("match 跟随界面", () => {
    assert.equal(appearanceFromTheme(resolveTermTheme("match", "dark")), "dark");
    assert.equal(appearanceFromTheme(resolveTermTheme("match", "light")), "light");
  });

  it("固定配色按底色判深浅，不跟界面走", () => {
    const darkIds = TERM_THEME_GROUPS.find((g) => g.id === "dark")!.themes;
    const lightIds = TERM_THEME_GROUPS.find((g) => g.id === "light")!.themes;
    for (const id of darkIds) {
      assert.equal(appearanceFromTheme(resolveTermTheme(id, "light")), "dark", id);
    }
    for (const id of lightIds) {
      assert.equal(appearanceFromTheme(resolveTermTheme(id, "dark")), "light", id);
    }
  });

  it("covers every catalogued theme id", () => {
    const ids = TERM_THEME_GROUPS.flatMap((g) => g.themes);
    assert.ok(ids.includes("match"));
    for (const id of ids as TermThemeId[]) {
      const appearance = appearanceFromTheme(resolveTermTheme(id, "dark"));
      assert.ok(appearance === "light" || appearance === "dark", id);
    }
  });
});
