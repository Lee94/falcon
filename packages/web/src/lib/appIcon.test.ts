import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";
import { APP_ICON_IDS, DEFAULT_APP_ICON, webManifest } from "@falcon/shared";
import { containRect } from "./appIcon.js";
import { meetsChromeInstallManifest } from "./install.js";

const here = dirname(fileURLToPath(import.meta.url));
const publicDir = join(here, "../../public");

describe("containRect", () => {
  it("横图：长边贴边、上下居中", () => {
    assert.deepEqual(containRect(1000, 500, 512), { x: 0, y: 128, w: 512, h: 256 });
  });

  it("竖图：左右居中", () => {
    assert.deepEqual(containRect(300, 600, 512), { x: 128, y: 0, w: 256, h: 512 });
  });

  it("小图也放大到 512", () => {
    assert.deepEqual(containRect(64, 64, 512), { x: 0, y: 0, w: 512, h: 512 });
  });

  it("宽高报 0（只写了 viewBox 的 SVG）按正方形铺满", () => {
    assert.deepEqual(containRect(0, 0, 512), { x: 0, y: 0, w: 512, h: 512 });
  });
});

describe("内置应用图标", () => {
  // 图形在 scripts/app-icons.mjs（gen-icons.mjs 出文件），id 在 shared 的 APP_ICON_IDS：两边分叉时这里先红
  it("每个 id 在 public/icons/<id>/ 下都有全套文件", () => {
    for (const id of APP_ICON_IDS) {
      for (const file of [
        "icon-192.png",
        "icon-512.png",
        "maskable-192.png",
        "maskable-512.png",
        "apple-touch-icon.png",
      ]) {
        assert.ok(existsSync(join(publicDir, "icons", id, file)), `缺 icons/${id}/${file}`);
      }
    }
  });

  it("默认图标与安装包脚本（scripts/app-icons.mjs）说的是同一个", () => {
    const src = readFileSync(join(here, "../../../../scripts/app-icons.mjs"), "utf8");
    assert.equal(/export const DEFAULT_ID = "([^"]+)"/.exec(src)?.[1], DEFAULT_APP_ICON);
  });

  it("选任何一个内置图标，清单都满足 Chrome 安装条件", () => {
    for (const id of APP_ICON_IDS) {
      assert.equal(meetsChromeInstallManifest(webManifest({ selected: id, custom: null })), true, id);
    }
  });
});
