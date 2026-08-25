import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { RepoBranch, RepoInfo } from "@falcon/shared";
import {
  actionBlocksSubmit,
  commonLocalBranches,
  evaluateMember,
  memberBasename,
} from "./multiDerive.js";

function branch(name: string, over: Partial<RepoBranch> = {}): RepoBranch {
  return { name, remote: false, head: false, suggestedDir: "", dirOccupied: false, ...over };
}

function repo(branches: RepoBranch[]): RepoInfo {
  return { derivable: true, repoDir: "/r", headBranch: "main", branches };
}

const BLOCKED: RepoInfo = { derivable: false, reason: "not-a-repo", branches: [] };

describe("evaluateMember", () => {
  it("blocked members block regardless of mode", () => {
    const a = evaluateMember(BLOCKED, "auto", "feat");
    assert.equal(a.kind, "blocked");
    assert.equal(actionBlocksSubmit(a), true);
  });

  it("auto: checkout when the branch exists, create otherwise", () => {
    assert.equal(evaluateMember(repo([branch("feat")]), "auto", "feat").kind, "checkout");
    assert.equal(evaluateMember(repo([]), "auto", "feat").kind, "create");
  });

  it("auto: an existing-but-checked-out branch is in use", () => {
    const a = evaluateMember(repo([branch("feat", { checkedOutAt: "/w" })]), "auto", "feat");
    assert.deepEqual(a, { kind: "branch-in-use", at: "/w" });
    assert.equal(actionBlocksSubmit(a), true);
  });

  it("new-branch: existing branch is a conflict; remote branches don't count", () => {
    assert.equal(evaluateMember(repo([branch("feat")]), "new-branch", "feat").kind, "branch-exists");
    assert.equal(
      evaluateMember(repo([branch("origin/feat", { remote: true })]), "new-branch", "feat").kind,
      "create"
    );
  });

  it("existing-branch: missing branch blocks", () => {
    assert.equal(evaluateMember(repo([]), "existing-branch", "feat").kind, "branch-missing");
  });
});

describe("commonLocalBranches", () => {
  it("intersects local branches across members, marks in-use ones", () => {
    const out = commonLocalBranches([
      { info: repo([branch("main"), branch("feat"), branch("origin/x", { remote: true })]) },
      { info: repo([branch("feat", { checkedOutAt: "/w" }), branch("main")]) },
    ]);
    assert.deepEqual(out, [{ name: "main" }, { name: "feat", usedAt: "/w" }]);
  });

  it("empty input yields empty", () => {
    assert.deepEqual(commonLocalBranches([]), []);
  });
});

describe("memberBasename", () => {
  it("handles both separators and trailing slashes", () => {
    assert.equal(memberBasename("/a/b/web/"), "web");
    assert.equal(memberBasename("D:\\code\\Web"), "Web");
    assert.equal(memberBasename("web"), "web");
  });
});
