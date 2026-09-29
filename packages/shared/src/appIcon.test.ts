import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  APP_ICON_IDS,
  DEFAULT_APP_ICON,
  appIconLinks,
  isAppIconChoice,
  resolveAppIcon,
  webManifest,
} from "./appIcon.js";

describe("resolveAppIcon", () => {
  it("认得的内置 id 原样生效", () => {
    assert.equal(resolveAppIcon("flash", null), "flash");
  });

  it("没存过、认不出的 id 回默认", () => {
    assert.equal(resolveAppIcon(undefined, null), DEFAULT_APP_ICON);
    assert.equal(resolveAppIcon("falcon-classic", null), DEFAULT_APP_ICON);
  });

  it("选了自定义但图没了，回默认", () => {
    assert.equal(resolveAppIcon("custom", null), DEFAULT_APP_ICON);
    assert.equal(resolveAppIcon("custom", "ab12"), "custom");
  });
});

describe("isAppIconChoice", () => {
  it("内置 id 与 custom 以外都不收", () => {
    for (const id of APP_ICON_IDS) assert.equal(isAppIconChoice(id), true);
    assert.equal(isAppIconChoice("custom"), true);
    assert.equal(isAppIconChoice("../etc"), false);
    assert.equal(isAppIconChoice(1), false);
  });
});

describe("appIconLinks", () => {
  it("内置图标：标签页用圆角的 192 png，iOS 用满版 png", () => {
    assert.deepEqual(appIconLinks({ selected: "glyph", custom: null }), {
      favicon: "/icons/glyph/icon-192.png",
      appleTouch: "/icons/glyph/apple-touch-icon.png",
    });
  });

  it("自定义图标的地址带版本，换图就换地址", () => {
    const links = appIconLinks({ selected: "custom", custom: "0f3c" });
    assert.equal(links.favicon, "/api/app-icon/custom.png?v=0f3c");
    assert.equal(links.appleTouch, links.favicon);
  });

  it("上传过自定义但选的是内置，用内置的", () => {
    assert.equal(appIconLinks({ selected: "flash", custom: "0f3c" }).favicon, "/icons/flash/icon-192.png");
  });
});

describe("webManifest", () => {
  it("内置图标给齐 any / maskable 各两档", () => {
    const m = webManifest({ selected: "voltwing-night", custom: null });
    assert.deepEqual(
      m.icons.map((i) => `${i.purpose} ${i.sizes} ${i.src}`),
      [
        "any 192x192 /icons/voltwing-night/icon-192.png",
        "any 512x512 /icons/voltwing-night/icon-512.png",
        "maskable 192x192 /icons/voltwing-night/maskable-192.png",
        "maskable 512x512 /icons/voltwing-night/maskable-512.png",
      ]
    );
  });

  it("自定义图标只有一张 512 的 any", () => {
    const m = webManifest({ selected: "custom", custom: "0f3c" });
    assert.deepEqual(m.icons, [
      { src: "/api/app-icon/custom.png?v=0f3c", sizes: "512x512", type: "image/png", purpose: "any" },
    ]);
  });
});
