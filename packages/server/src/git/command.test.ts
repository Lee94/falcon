import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { countStatusChanges, parseStatusEntries, truncateDiff } from "./command.js";

describe("countStatusChanges", () => {
  it("counts new and untracked files as added, deletions as deleted", () => {
    const entries = parseStatusEntries(
      ["A  added.ts", "?? scratch.ts", " D gone.ts", "D  staged-gone.ts"].join("\n")
    );
    assert.deepEqual(countStatusChanges(entries), { added: 2, deleted: 2 });
  });

  it("counts modified and renamed files as added — they are still there", () => {
    const entries = parseStatusEntries(
      [" M dirty.ts", "M  staged.ts", "MM both.ts", "R  old.ts -> new.ts"].join("\n")
    );
    assert.deepEqual(countStatusChanges(entries), { added: 4, deleted: 0 });
  });

  it("does not count a delete-then-add rewrite as deleted", () => {
    const entries = parseStatusEntries("AD rewritten.ts\n");
    assert.deepEqual(countStatusChanges(entries), { added: 1, deleted: 0 });
  });

  it("returns zeros for a clean tree", () => {
    assert.deepEqual(countStatusChanges(parseStatusEntries("")), { added: 0, deleted: 0 });
  });
});

describe("truncateDiff", () => {
  it("passes short text through untouched", () => {
    assert.deepEqual(truncateDiff("+a\n-b\n", 10), { text: "+a\n-b\n", truncated: false });
  });

  it("cuts at a line boundary, not mid-line", () => {
    const { text, truncated } = truncateDiff("+aaaa\n+bbbb\n+cccc\n", 14);
    assert.equal(text, "+aaaa\n+bbbb\n");
    assert.equal(truncated, true);
  });

  it("keeps the raw cut when there is no newline to fall back to", () => {
    const { text, truncated } = truncateDiff("x".repeat(20), 5);
    assert.equal(text, "xxxxx");
    assert.equal(truncated, true);
  });
});
