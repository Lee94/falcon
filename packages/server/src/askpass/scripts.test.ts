import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { describe, it } from "node:test";
import {
  parseAskpassConf,
  renderAskpassConf,
  renderSudoShim,
  sudoNeedsAskpassFlag,
} from "./scripts.js";

describe("sudoNeedsAskpassFlag", () => {
  it("plain sudo command needs -A", () => {
    assert.equal(sudoNeedsAskpassFlag(["true"]), true);
    assert.equal(sudoNeedsAskpassFlag(["-u", "root", "id"]), true);
    assert.equal(sudoNeedsAskpassFlag(["-E", "env"]), true);
  });

  it("does not stack -A on -n / -S / -A", () => {
    assert.equal(sudoNeedsAskpassFlag(["-n", "true"]), false);
    assert.equal(sudoNeedsAskpassFlag(["--non-interactive", "true"]), false);
    assert.equal(sudoNeedsAskpassFlag(["-S", "true"]), false);
    assert.equal(sudoNeedsAskpassFlag(["-A", "true"]), false);
    assert.equal(sudoNeedsAskpassFlag(["-nv", "true"]), false);
    assert.equal(sudoNeedsAskpassFlag(["-An", "true"]), false);
  });

  it("stops at --", () => {
    assert.equal(sudoNeedsAskpassFlag(["--", "-n"]), true);
  });
});

describe("askpass.conf", () => {
  it("round-trips url and token", () => {
    const text = renderAskpassConf("http://127.0.0.1:4923/api/askpass", "abc");
    assert.deepEqual(parseAskpassConf(text), {
      url: "http://127.0.0.1:4923/api/askpass",
      token: "abc",
    });
  });

  it("rejects incomplete conf", () => {
    assert.equal(parseAskpassConf("URL=http://x\n"), null);
    assert.equal(parseAskpassConf(""), null);
  });
});

describe("sudo shim (no tty)", () => {
  it("injects -A for a plain command and leaves -n alone", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-sudo-"));
    try {
      const shim = path.join(dir, "sudo");
      const fake = path.join(dir, "real-sudo");
      fs.writeFileSync(shim, renderSudoShim(), { mode: 0o755 });
      fs.chmodSync(shim, 0o755);
      fs.writeFileSync(fake, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n", { mode: 0o755 });
      fs.chmodSync(fake, 0o755);

      const run = (args: string[]) =>
        spawnSync(
          "python3",
          ["-c", "import os,sys; os.execvp(sys.argv[1], sys.argv[1:])", shim, ...args],
          {
            encoding: "utf8",
            env: { ...process.env, FALCON_REAL_SUDO: fake },
            timeout: 5000,
            stdio: ["pipe", "pipe", "pipe"],
          }
        );

      const plain = run(["true"]);
      assert.equal(plain.status, 0, plain.stderr);
      assert.equal(plain.stdout, "-A\ntrue\n");

      const nonint = run(["-n", "true"]);
      assert.equal(nonint.status, 0, nonint.stderr);
      assert.equal(nonint.stdout, "-n\ntrue\n");
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
