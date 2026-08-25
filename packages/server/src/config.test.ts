import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { defaultDataDir } from "./config.js";

describe("defaultDataDir", () => {
  it("uses ~/.falcon when nothing exists yet", () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-home-"));
    try {
      assert.equal(defaultDataDir(home), path.join(home, ".falcon"));
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  });

  it("falls back to ~/.mojito when that is the only existing dir", () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-home-"));
    try {
      const prev = path.join(home, ".mojito");
      fs.mkdirSync(prev);
      assert.equal(defaultDataDir(home), prev);
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  });

  it("prefers ~/.falcon once it exists", () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-home-"));
    try {
      const next = path.join(home, ".falcon");
      fs.mkdirSync(path.join(home, ".mojito"));
      fs.mkdirSync(next);
      assert.equal(defaultDataDir(home), next);
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  });
});
