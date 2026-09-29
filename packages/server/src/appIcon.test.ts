import assert from "node:assert/strict";
import { describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { AppIcons, CUSTOM_ICON_MAX_BYTES, checkCustomIcon, pngSize } from "./appIcon.js";
import { Db } from "./db.js";

/** 只有签名 + IHDR 头的"PNG"：服务端只看这两样 */
function fakePng(width: number, height: number): Buffer {
  const buf = Buffer.alloc(33);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(buf, 0);
  buf.writeUInt32BE(13, 8);
  buf.write("IHDR", 12, "latin1");
  buf.writeUInt32BE(width, 16);
  buf.writeUInt32BE(height, 20);
  return buf;
}

function tmpIcons(): { icons: AppIcons; dataDir: string } {
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-icon-"));
  return { icons: new AppIcons(new Db(dataDir), dataDir), dataDir };
}

describe("pngSize", () => {
  it("从 IHDR 读宽高", () => {
    assert.deepEqual(pngSize(fakePng(512, 256)), { width: 512, height: 256 });
  });

  it("不是 PNG 返回 null", () => {
    assert.equal(pngSize(Buffer.from("GIF89a...........................")), null);
    assert.equal(pngSize(fakePng(1, 1).subarray(0, 20)), null);
  });
});

describe("checkCustomIcon", () => {
  it("正方形 PNG 通过", () => {
    assert.equal(checkCustomIcon(fakePng(512, 512)), null);
  });

  it("非正方形、太小、太大、不是 PNG 都拒", () => {
    assert.match(checkCustomIcon(fakePng(512, 500)) ?? "", /正方形/);
    assert.match(checkCustomIcon(fakePng(32, 32)) ?? "", /边长/);
    assert.match(checkCustomIcon(fakePng(4096, 4096)) ?? "", /边长/);
    assert.match(checkCustomIcon(Buffer.from("<svg/>")) ?? "", /PNG/);
    assert.match(checkCustomIcon(Buffer.alloc(CUSTOM_ICON_MAX_BYTES + 1)) ?? "", /太大/);
  });
});

describe("AppIcons", () => {
  it("没设置过是默认图标", () => {
    const { icons } = tmpIcons();
    assert.deepEqual(icons.state(), { selected: "emberwing", custom: null });
  });

  it("选内置图标；未知 id、还没传图时选自定义都不成立", () => {
    const { icons } = tmpIcons();
    assert.deepEqual(icons.select("flash"), { selected: "flash", custom: null });
    assert.equal(icons.select("nope"), null);
    assert.equal(icons.select("custom"), null);
    assert.equal(icons.state().selected, "flash");
  });

  it("上传即选中，版本是内容哈希；删掉后回默认", () => {
    const { icons, dataDir } = tmpIcons();
    const png = fakePng(512, 512);
    const saved = icons.saveCustom(png);
    assert.equal(saved.selected, "custom");
    assert.match(saved.custom ?? "", /^[0-9a-f]{16}$/);
    assert.deepEqual(fs.readFileSync(path.join(dataDir, "app-icon", "custom.png")), png);

    // 改选内置再选回自定义，图还在
    icons.select("glyph");
    assert.equal(icons.select("custom")?.selected, "custom");

    assert.deepEqual(icons.removeCustom(), { selected: "emberwing", custom: null });
    assert.equal(icons.customFile(), null);
  });

  it("库里记着但文件被删了，当没有", () => {
    const { icons, dataDir } = tmpIcons();
    icons.saveCustom(fakePng(256, 256));
    fs.rmSync(path.join(dataDir, "app-icon", "custom.png"));
    assert.deepEqual(icons.state(), { selected: "emberwing", custom: null });
  });
});
