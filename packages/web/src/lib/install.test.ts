import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";
import {
  canInstall,
  handleAppInstalled,
  handleBeforeInstallPrompt,
  isStandalone,
  meetsChromeInstallManifest,
  promptInstall,
  resetInstallPrompt,
} from "./install.js";

const here = dirname(fileURLToPath(import.meta.url));

describe("isStandalone", () => {
  it("display-mode: standalone 算已安装", () => {
    assert.equal(
      isStandalone({
        matchMedia: (q) => ({ matches: q === "(display-mode: standalone)" }),
      }),
      true
    );
  });

  it("iOS navigator.standalone 也算", () => {
    assert.equal(isStandalone({ navigator: { standalone: true } }), true);
  });

  it("普通标签页不算", () => {
    assert.equal(
      isStandalone({
        matchMedia: () => ({ matches: false }),
        navigator: { standalone: false },
      }),
      false
    );
  });
});

describe("meetsChromeInstallManifest", () => {
  const ok = {
    name: "Falcon",
    start_url: "/",
    display: "standalone",
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", purpose: "any" },
    ],
  };

  it("完整字段通过", () => {
    assert.equal(meetsChromeInstallManifest(ok), true);
  });

  it("缺 512 图标不通过", () => {
    assert.equal(
      meetsChromeInstallManifest({
        ...ok,
        icons: [{ src: "/a.png", sizes: "192x192", purpose: "any" }],
      }),
      false
    );
  });

  it("只有 maskable 图标不通过（Chrome 要 any）", () => {
    assert.equal(
      meetsChromeInstallManifest({
        ...ok,
        icons: [
          { src: "/a.png", sizes: "192x192", purpose: "maskable" },
          { src: "/b.png", sizes: "512x512", purpose: "maskable" },
        ],
      }),
      false
    );
  });

  it("display: browser 不通过", () => {
    assert.equal(meetsChromeInstallManifest({ ...ok, display: "browser" }), false);
  });

  it("仓库里的 manifest 满足 Chrome 安装条件", () => {
    const raw = readFileSync(join(here, "../../public/manifest.webmanifest"), "utf8");
    assert.equal(meetsChromeInstallManifest(JSON.parse(raw)), true);
  });
});

describe("promptInstall", () => {
  it("没有捕获过事件时不可用", async () => {
    resetInstallPrompt();
    assert.equal(canInstall(), false);
    assert.equal(await promptInstall(), "unavailable");
  });

  it("捕获 beforeinstallprompt 后可以调起，接受则清空", async () => {
    resetInstallPrompt();
    let prompted = false;
    handleBeforeInstallPrompt({
      preventDefault() {},
      async prompt() {
        prompted = true;
      },
      userChoice: Promise.resolve({ outcome: "accepted" }),
    });
    assert.equal(canInstall(), true);
    assert.equal(await promptInstall(), "accepted");
    assert.equal(prompted, true);
    assert.equal(canInstall(), false);
  });

  it("appinstalled 清掉待安装事件", () => {
    resetInstallPrompt();
    handleBeforeInstallPrompt({
      preventDefault() {},
      async prompt() {},
      userChoice: Promise.resolve({ outcome: "dismissed" }),
    });
    handleAppInstalled();
    assert.equal(canInstall(), false);
  });
});
