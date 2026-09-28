import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { validateShareInput } from "./shareSpec.js";

describe("validateShareInput", () => {
  it("fills loopback and enables by default", () => {
    const got = validateShareInput({ destPort: 5173 });
    assert.equal(got.ok, true);
    if (!got.ok) return;
    assert.deepEqual(got.value, {
      name: undefined,
      destHost: "127.0.0.1",
      destPort: 5173,
      enabled: true,
    });
  });

  it("trims the name and keeps enabled=false", () => {
    const got = validateShareInput({ destPort: 3000, name: "  vite  ", enabled: false });
    assert.equal(got.ok, true);
    if (!got.ok) return;
    assert.equal(got.value.name, "vite");
    assert.equal(got.value.enabled, false);
  });

  it("rejects junk port", () => {
    assert.equal(validateShareInput({} as never).ok, false);
    assert.equal(validateShareInput({ destPort: 0 }).ok, false);
  });
});
