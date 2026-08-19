import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  formatForwardEndpoint,
  forwardBindKey,
  isValidPort,
  parseHost,
  parsePort,
  validateForwardInput,
} from "./forwardSpec.js";

describe("parsePort", () => {
  it("accepts integers and numeric strings", () => {
    assert.equal(parsePort(3000), 3000);
    assert.equal(parsePort("5432"), 5432);
  });

  it("rejects junk so we do not listen on NaN", () => {
    assert.equal(parsePort(undefined), null);
    assert.equal(parsePort(""), null);
    assert.equal(parsePort("22.5"), null);
    assert.equal(parsePort(22.5), null);
  });
});

describe("isValidPort", () => {
  it("only allows 1–65535", () => {
    assert.equal(isValidPort(1), true);
    assert.equal(isValidPort(65535), true);
    assert.equal(isValidPort(0), false);
    assert.equal(isValidPort(65536), false);
  });
});

describe("parseHost", () => {
  it("defaults empty to loopback — do not bind 0.0.0.0 by accident", () => {
    assert.equal(parseHost(undefined), "127.0.0.1");
    assert.equal(parseHost(""), "127.0.0.1");
    assert.equal(parseHost("   "), "127.0.0.1");
  });

  it("rejects whitespace inside a host", () => {
    assert.equal(parseHost("127.0.0.1 extra"), null);
  });

  it("keeps explicit addresses", () => {
    assert.equal(parseHost("0.0.0.0"), "0.0.0.0");
    assert.equal(parseHost("db.internal"), "db.internal");
    assert.equal(parseHost("::1"), "::1");
  });
});

describe("validateForwardInput", () => {
  it("fills defaults for a local tunnel", () => {
    const got = validateForwardInput({ kind: "local", bindPort: 3000, destPort: 3000 });
    assert.equal(got.ok, true);
    if (!got.ok) return;
    assert.deepEqual(got.value, {
      name: undefined,
      kind: "local",
      bindHost: "127.0.0.1",
      bindPort: 3000,
      destHost: "127.0.0.1",
      destPort: 3000,
      enabled: true,
    });
  });

  it("trims an optional name and rejects an empty kind", () => {
    const named = validateForwardInput({
      kind: "remote",
      name: "  vite  ",
      bindPort: 5173,
      destPort: 5173,
      enabled: false,
    });
    assert.equal(named.ok, true);
    if (named.ok) {
      assert.equal(named.value.name, "vite");
      assert.equal(named.value.enabled, false);
    }
    assert.equal(validateForwardInput({ bindPort: 1, destPort: 1 }).ok, false);
    assert.equal(validateForwardInput({ kind: "local", bindPort: 0, destPort: 80 }).ok, false);
  });
});

describe("forwardBindKey / formatForwardEndpoint", () => {
  it("treats same bind on different kinds as distinct", () => {
    assert.notEqual(
      forwardBindKey("local", "127.0.0.1", 3000),
      forwardBindKey("remote", "127.0.0.1", 3000)
    );
  });

  it("brackets IPv6 so the port is readable", () => {
    assert.equal(formatForwardEndpoint("::1", 80), "[::1]:80");
    assert.equal(formatForwardEndpoint("127.0.0.1", 80), "127.0.0.1:80");
  });
});
