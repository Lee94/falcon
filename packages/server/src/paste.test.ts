import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  imageExt,
  pasteDir,
  pasteFileName,
  posixWriteCommand,
  windowsWriteCommand,
} from "./paste.js";

describe("imageExt", () => {
  it("maps the supported image types", () => {
    assert.equal(imageExt("image/png"), "png");
    assert.equal(imageExt("image/jpeg"), "jpg");
    assert.equal(imageExt("image/gif"), "gif");
    assert.equal(imageExt("image/webp"), "webp");
  });

  it("ignores parameters and case", () => {
    assert.equal(imageExt("image/PNG; charset=binary"), "png");
  });

  it("rejects everything else — unknown bytes must not land on the host", () => {
    assert.equal(imageExt("image/svg+xml"), null);
    assert.equal(imageExt("application/octet-stream"), null);
    assert.equal(imageExt(undefined), null);
  });
});

describe("pasteDir / pasteFileName", () => {
  it("lives under the mojito root on either platform", () => {
    assert.equal(pasteDir("posix", "/home/u/.mojito"), "/home/u/.mojito/paste");
    assert.equal(
      pasteDir("windows", "C:\\Users\\u\\.mojito"),
      "C:\\Users\\u\\.mojito\\paste"
    );
  });

  it("generates short whitespace-free names so the pasted path never needs quoting", () => {
    const name = pasteFileName("png");
    assert.match(name, /^img-[0-9a-f]{8}\.png$/);
  });
});

describe("posixWriteCommand", () => {
  it("creates the dir, sweeps stale files, then cats stdin into the target", () => {
    const cmd = posixWriteCommand("/home/u/.mojito/paste", "/home/u/.mojito/paste/img-1.png");
    assert.match(cmd, /mkdir -p "\$d"/);
    assert.match(cmd, /find "\$d" -maxdepth 1 -type f -mmin \+1440 -delete 2>\/dev\/null; /);
    assert.ok(cmd.endsWith(`cat > '/home/u/.mojito/paste/img-1.png'`));
  });

  it("quotes paths so a home dir with spaces or quotes cannot split the command", () => {
    const cmd = posixWriteCommand("/home/o'brien/.mojito/paste", "/home/o'brien/.mojito/paste/i.png");
    assert.ok(cmd.startsWith(`d='/home/o'\\''brien/.mojito/paste'`));
    assert.ok(cmd.endsWith(`cat > '/home/o'\\''brien/.mojito/paste/i.png'`));
  });
});

describe("windowsWriteCommand", () => {
  function decode(cmd: string): string {
    const b64 = cmd.split(" ").at(-1)!;
    return Buffer.from(b64, "base64").toString("utf16le");
  }

  it("is an -EncodedCommand invocation immune to the remote DefaultShell", () => {
    const cmd = windowsWriteCommand("C:\\u\\.mojito\\paste", "C:\\u\\.mojito\\paste\\i.png");
    assert.ok(cmd.startsWith("powershell -NoProfile -NonInteractive -EncodedCommand "));
  });

  it("reads base64 from stdin and writes decoded bytes to the target path", () => {
    const script = decode(
      windowsWriteCommand("C:\\Users\\a b\\.mojito\\paste", "C:\\Users\\a b\\.mojito\\paste\\i.png")
    );
    assert.match(script, /New-Item -ItemType Directory -Force -Path \$d/);
    assert.match(script, /\[Convert\]::FromBase64String\(\[Console\]::In\.ReadToEnd\(\)\)/);
    assert.ok(script.includes(`[IO.File]::WriteAllBytes('C:\\Users\\a b\\.mojito\\paste\\i.png', $b)`));
    assert.ok(script.includes(`$d = 'C:\\Users\\a b\\.mojito\\paste'`));
  });

  it("sweeps files older than 24h before writing", () => {
    const script = decode(windowsWriteCommand("C:\\u\\p", "C:\\u\\p\\i.png"));
    assert.match(script, /AddHours\(-24\)/);
    assert.match(script, /Remove-Item -Force -EA SilentlyContinue/);
  });
});
