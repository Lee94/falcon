import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { Project } from "@falcon/shared";
import { checkoutLabel, groupServers } from "./projectTree.js";

function project(overrides: Partial<Project> & Pick<Project, "id" | "name">): Project {
  return { type: "local", createdAt: 0, ...overrides };
}

describe("groupServers with multi-repo projects", () => {
  it("a container is a source folder; its derived multi project nests under it", () => {
    const container = project({
      id: "c1",
      name: "组合",
      multi: { repos: [{ dir: "/a/web" }, { dir: "/a/srv" }] },
    });
    const derived = project({
      id: "d1",
      name: "feat-x",
      workingDir: "/a/组合-feat-x",
      multi: {
        repos: [
          { dir: "/a/组合-feat-x/web", repoDir: "/a/web" },
          { dir: "/a/组合-feat-x/srv", repoDir: "/a/srv" },
        ],
      },
      worktree: {
        sourceProjectId: "c1",
        branch: "feat/x",
        repoDir: "",
        createdByFalcon: true,
      },
    });
    const servers = groupServers([container, derived], [], "本机", false);
    const local = servers.find((s) => s.kind === "local")!;
    assert.equal(local.folders.length, 1);
    assert.equal(local.folders[0]!.project.id, "c1");
    assert.deepEqual(
      local.folders[0]!.worktrees.map((w) => w.id),
      ["d1"]
    );
  });

  it("an orphan derived multi project still renders as its own folder", () => {
    const derived = project({
      id: "d1",
      name: "feat-x",
      multi: { repos: [{ dir: "/x/web", repoDir: "/a/web" }] },
      worktree: { sourceProjectId: "gone", branch: "feat/x", repoDir: "", createdByFalcon: true },
    });
    const servers = groupServers([derived], [], "本机", false);
    const local = servers.find((s) => s.kind === "local")!;
    assert.equal(local.folders.length, 1);
    assert.equal(local.folders[0]!.worktrees.length, 0);
  });
});

describe("checkoutLabel", () => {
  it("derived rows label by branch, even when they are multi", () => {
    const derived = project({
      id: "d",
      name: "n",
      multi: { repos: [] },
      worktree: { sourceProjectId: "s", branch: "feat/x", repoDir: "", createdByFalcon: true },
    });
    assert.equal(checkoutLabel(derived), "feat/x");
  });

  it("containers return empty — the caller supplies the repo-count label", () => {
    const container = project({ id: "c", name: "组合", multi: { repos: [{ dir: "/a" }] } });
    assert.equal(checkoutLabel(container, { branch: "main" }), "");
  });

  it("plain sources keep head branch / sha / name fallback", () => {
    const p = project({ id: "p", name: "web" });
    assert.equal(checkoutLabel(p, { branch: "main" }), "main");
    assert.equal(checkoutLabel(p, { sha: "abc123" }), "abc123");
    assert.equal(checkoutLabel(p), "web");
  });
});
