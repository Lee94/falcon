import assert from "node:assert/strict";
import path from "node:path";
import { describe, it } from "node:test";
import { bundledBinName, installPath } from "./bin.js";

describe("installPath", () => {
  it("puts the binary under dataDir/bin, with .exe on Windows", () => {
    assert.equal(installPath("/data", "darwin"), path.join("/data", "bin", "cloudflared"));
    assert.equal(bundledBinName("win32"), "cloudflared.exe");
    assert.equal(bundledBinName("linux"), "cloudflared");
  });
});

