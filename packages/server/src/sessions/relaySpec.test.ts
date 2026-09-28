import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  displacedBy,
  excessEnabled,
  forwardSlot,
  legacyForwardHost,
  shareSlot,
  type ForwardSlotRow,
  type ShareSlotRow,
} from "./relaySpec.js";

const fwd = (
  id: string,
  host_id: string,
  kind: "local" | "remote",
  bind_port: number,
  enabled = 1,
  created_at = 0
): ForwardSlotRow => ({ id, host_id, kind, bind_port, enabled, created_at });

const share = (
  id: string,
  host_id: string | null,
  dest_port: number,
  enabled = 1,
  created_at = 0
): ShareSlotRow => ({ id, host_id, dest_port, enabled, created_at });

describe("forwardSlot", () => {
  it("local forwards share one port table across hosts — they all listen on the backend", () => {
    assert.equal(forwardSlot(fwd("a", "h1", "local", 5432)), forwardSlot(fwd("b", "h2", "local", 5432)));
  });

  it("remote forwards only clash on the same host", () => {
    assert.notEqual(
      forwardSlot(fwd("a", "h1", "remote", 8080)),
      forwardSlot(fwd("b", "h2", "remote", 8080))
    );
    assert.equal(
      forwardSlot(fwd("a", "h1", "remote", 8080)),
      forwardSlot(fwd("b", "h1", "remote", 8080))
    );
  });

  it("local and remote of the same port never clash", () => {
    assert.notEqual(forwardSlot(fwd("a", "h1", "local", 80)), forwardSlot(fwd("b", "h1", "remote", 80)));
  });
});

describe("shareSlot", () => {
  it("keys by machine: 本机 and a host with the same port are distinct", () => {
    assert.notEqual(shareSlot(share("a", null, 3000)), shareSlot(share("b", "h1", 3000)));
    assert.equal(shareSlot(share("a", "h1", 3000)), shareSlot(share("b", "h1", 3000)));
  });
});

describe("displacedBy", () => {
  it("returns other enabled rows on the same slot, never the target", () => {
    const rows = [
      fwd("a", "h1", "local", 5432, 1),
      fwd("b", "h2", "local", 5432, 0),
      fwd("c", "h2", "local", 5433, 1),
      fwd("d", "h3", "local", 5432, 1),
    ];
    assert.deepEqual(displacedBy(rows, rows[1]!, forwardSlot), ["a", "d"]);
    assert.deepEqual(displacedBy(rows, rows[0]!, forwardSlot), ["d"]);
  });

  it("ignores disabled rows — they are not holding the port", () => {
    const rows = [fwd("a", "h1", "local", 1, 0), fwd("b", "h1", "local", 1, 0)];
    assert.deepEqual(displacedBy(rows, rows[0]!, forwardSlot), []);
  });
});

describe("excessEnabled", () => {
  it("keeps the earliest enabled row per slot", () => {
    const rows = [
      share("late", "h1", 3000, 1, 20),
      share("early", "h1", 3000, 1, 10),
      share("off", "h1", 3000, 0, 5),
      share("other", null, 3000, 1, 30),
    ];
    assert.deepEqual(excessEnabled(rows, shareSlot), ["late"]);
  });
});

describe("legacyForwardHost", () => {
  const hosts = [
    { id: "h1", host: "10.0.0.1", port: 22, username: "fay" },
    { id: "h2", host: "10.0.0.1", port: 22, username: "root" },
  ];

  it("prefers the project's bound host", () => {
    assert.equal(
      legacyForwardHost({ host_id: "h2", ssh_host: "10.0.0.1", ssh_port: 22, ssh_username: "fay" }, hosts),
      "h2"
    );
  });

  it("falls back to the exact host/port/user triple when unbound or the host is gone", () => {
    assert.equal(
      legacyForwardHost({ host_id: null, ssh_host: "10.0.0.1", ssh_port: null, ssh_username: "fay" }, hosts),
      "h1"
    );
    assert.equal(
      legacyForwardHost({ host_id: "gone", ssh_host: "10.0.0.1", ssh_port: 22, ssh_username: "root" }, hosts),
      "h2"
    );
  });

  it("does not guess across usernames or ports", () => {
    assert.equal(
      legacyForwardHost({ host_id: null, ssh_host: "10.0.0.1", ssh_port: 22, ssh_username: "bob" }, hosts),
      null
    );
    assert.equal(
      legacyForwardHost({ host_id: null, ssh_host: "10.0.0.1", ssh_port: 2222, ssh_username: "fay" }, hosts),
      null
    );
  });
});
