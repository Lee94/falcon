import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { AskpassHub } from "./hub.js";
import { posixWriteAskpassCommand, prependPath } from "./install.js";

describe("prependPath", () => {
  it("puts the shim dir first", () => {
    assert.equal(prependPath({ PATH: "/usr/bin" }, "/falcon/bin").PATH, "/falcon/bin:/usr/bin");
    assert.equal(prependPath({}, "/falcon/bin").PATH, "/falcon/bin");
  });
});

describe("posixWriteAskpassCommand", () => {
  it("writes sudo, helper, conf and chmods them", () => {
    const hub = new AskpassHub();
    hub.setOrigin("http://127.0.0.1:4923");
    const cmd = posixWriteAskpassCommand("/home/u/.falcon/bin", hub, hub.helperUrl());
    assert.ok(cmd.includes(`mkdir -p "$d"`));
    assert.ok(cmd.includes(`> "$d"/sudo && chmod 755 "$d"/sudo`));
    assert.ok(cmd.includes(`> "$d"/falcon-askpass && chmod 755 "$d"/falcon-askpass`));
    assert.ok(cmd.includes(`> "$d"/askpass.conf && chmod 600 "$d"/askpass.conf`));
    assert.ok(cmd.includes(hub.token));
    assert.ok(cmd.includes("http://127.0.0.1:4923/api/askpass"));
  });
});
