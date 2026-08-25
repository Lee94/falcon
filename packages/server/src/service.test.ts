import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { installServiceBinary, serviceBinPath } from "./service.js";

describe("serviceBinPath", () => {
  it("is always <dataDir>/bin/falcon — never the versioned download name", () => {
    assert.equal(serviceBinPath("/tmp/data"), path.join("/tmp/data", "bin", "falcon"));
  });
});

function withTmp(fn: (dir: string) => void) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-svc-"));
  try {
    fn(dir);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

describe("installServiceBinary", () => {
  it("copies src to dest named falcon and makes it executable", () => {
    withTmp((dir) => {
      const src = path.join(dir, "falcon-v0.1.0-darwin-arm64");
      const dest = serviceBinPath(path.join(dir, "data"));
      fs.writeFileSync(src, "fake-bin");
      fs.chmodSync(src, 0o644);

      assert.equal(installServiceBinary(src, dest), dest);
      assert.equal(path.basename(dest), "falcon");
      assert.equal(fs.readFileSync(dest, "utf8"), "fake-bin");
      assert.equal(fs.statSync(dest).mode & 0o111, 0o111);
      assert.equal(fs.existsSync(src), true);
    });
  });

  it("replaces an existing dest via rename so a running service can be upgraded", () => {
    withTmp((dir) => {
      const src = path.join(dir, "falcon-v0.2.0-darwin-arm64");
      const dest = serviceBinPath(path.join(dir, "data"));
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(dest, "old");
      fs.writeFileSync(src, "new");

      installServiceBinary(src, dest);
      assert.equal(fs.readFileSync(dest, "utf8"), "new");
      assert.equal(fs.readdirSync(path.dirname(dest)).includes("falcon"), true);
      assert.ok(!fs.readdirSync(path.dirname(dest)).some((n) => n.startsWith("falcon.new-")));
    });
  });

  it("is a no-op when src is already dest", () => {
    withTmp((dir) => {
      const dest = serviceBinPath(dir);
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(dest, "self");
      const before = fs.statSync(dest).mtimeMs;

      assert.equal(installServiceBinary(dest, dest), dest);
      assert.equal(fs.readFileSync(dest, "utf8"), "self");
      assert.equal(fs.statSync(dest).mtimeMs, before);
    });
  });
});
