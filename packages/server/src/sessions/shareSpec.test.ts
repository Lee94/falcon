import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { shareDestKey, validateShareInput } from "./shareSpec.js";

describe("validateShareInput", () => {
  it("fills loopback and enables by default", () => {
    const got = validateShareInput({ origin: "local", destPort: 5173 }, "local");
    assert.equal(got.ok, true);
    if (!got.ok) return;
    assert.deepEqual(got.value, {
      name: undefined,
      origin: "local",
      destHost: "127.0.0.1",
      destPort: 5173,
      enabled: true,
    });
  });

  it("rejects remote origin on a local project — there is no SSH link to bridge", () => {
    const got = validateShareInput({ origin: "remote", destPort: 80 }, "local");
    assert.equal(got.ok, false);
    if (got.ok) return;
    assert.match(got.error, /SSH/);
  });

  it("allows remote origin on an SSH project and trims the name", () => {
    const got = validateShareInput(
      { origin: "remote", destPort: 3000, name: "  vite  ", enabled: false },
      "ssh"
    );
    assert.equal(got.ok, true);
    if (!got.ok) return;
    assert.equal(got.value.name, "vite");
    assert.equal(got.value.enabled, false);
    assert.equal(got.value.origin, "remote");
  });

  it("rejects junk origin and port", () => {
    assert.equal(validateShareInput({ destPort: 80 }, "local").ok, false);
    assert.equal(
      validateShareInput({ origin: "local", destPort: 0 }, "local").ok,
      false
    );
  });
});

describe("shareDestKey", () => {
  it("treats local and remote of the same port as distinct", () => {
    assert.notEqual(
      shareDestKey("local", "127.0.0.1", 3000),
      shareDestKey("remote", "127.0.0.1", 3000)
    );
  });
});
