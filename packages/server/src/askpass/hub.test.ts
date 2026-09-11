import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { AskpassCancelled, AskpassHub } from "./hub.js";

describe("AskpassHub", () => {
  it("bearer token matches only the exact token", () => {
    const hub = new AskpassHub();
    assert.equal(hub.tokenMatches(`Bearer ${hub.token}`), true);
    assert.equal(hub.tokenMatches(`bearer ${hub.token}`), true);
    assert.equal(hub.tokenMatches(`Bearer nope`), false);
    assert.equal(hub.tokenMatches(undefined), false);
  });

  it("answer resolves the helper and cancel rejects", async () => {
    const hub = new AskpassHub();
    const seen: string[] = [];
    hub.onPrompt = (p) => seen.push(p.id);

    const p1 = hub.request({ prompt: "Password:", sessionId: "s1" });
    assert.equal(seen.length, 1);
    assert.equal(hub.answer(seen[0]!, "secret"), true);
    assert.equal(await p1, "secret");
    assert.equal(hub.answer(seen[0]!, "again"), false);

    const p2 = hub.request({ prompt: "pw" });
    assert.equal(seen.length, 2);
    assert.equal(hub.cancel(seen[1]!), true);
    await assert.rejects(p2, AskpassCancelled);
  });

  it("lists prompts still waiting so a late viewer can catch up", async () => {
    const hub = new AskpassHub();
    const request = hub.request({ prompt: "pw", sessionId: "s1" });
    const listed = hub.pendingPrompts();
    assert.equal(listed.length, 1);
    assert.equal(listed[0]!.prompt, "pw");
    assert.equal(listed[0]!.sessionId, "s1");
    hub.cancel(listed[0]!.id);
    await assert.rejects(request, AskpassCancelled);
    assert.equal(hub.pendingPrompts().length, 0);
  });
});
