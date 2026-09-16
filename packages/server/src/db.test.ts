import assert from "node:assert/strict";
import { describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { Db, type ProjectRow } from "./db.js";

function tmpDb(): Db {
  return new Db(fs.mkdtempSync(path.join(os.tmpdir(), "falcon-db-")));
}

function projectRow(overrides: Partial<ProjectRow> = {}): ProjectRow {
  return {
    id: "p1",
    name: "p",
    type: "local",
    working_dir: "/w",
    shell: null,
    ssh_host: null,
    ssh_port: null,
    ssh_username: null,
    ssh_auth_method: null,
    ssh_key_path: null,
    ssh_secret_enc: null,
    host_id: null,
    created_at: 1,
    source_project_id: null,
    worktree_branch: null,
    worktree_repo_dir: null,
    worktree_created_by_mojito: null,
    worktree_archived_at: null,
    multi_repos: null,
    default_worktree_branch: null,
    ...overrides,
  };
}

describe("upsertZellijHost", () => {
  it("keeps omitted fields but clears fields explicitly set to null", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-db-"));
    const db = new Db(dir);

    db.upsertZellijHost("h", 22, "u", {
      authorized: 1,
      installed_version: "0.44.3",
      verified_durable: 0,
    });

    // 省略的字段保持原值
    db.upsertZellijHost("h", 22, "u", { base_url: "https://mirror.example" });
    let row = db.getZellijHost("h", 22, "u")!;
    assert.equal(row.verified_durable, 0);
    assert.equal(row.installed_version, "0.44.3");
    assert.equal(row.base_url, "https://mirror.example");

    // 显式 null = 清空：重试前作废持久性判定、换下载源作废安装记录。
    // 用 ?? 合并的话这里会被旧值顶回去、清空静默失效——这正是曾经的 bug。
    db.upsertZellijHost("h", 22, "u", {
      verified_durable: null,
      installed_version: null,
      base_url: null,
    });
    row = db.getZellijHost("h", 22, "u")!;
    assert.equal(row.verified_durable, null);
    assert.equal(row.installed_version, null);
    assert.equal(row.base_url, null);
    assert.equal(row.authorized, 1);
  });
});

describe("parseMultiRepos", () => {
  it("parses a healthy member list and round-trips through insert/select", () => {
    const db = tmpDb();
    const members = [{ dir: "/a/web" }, { dir: "/c/app-x/web", repoDir: "/a/web" }];
    db.insertProject(projectRow({ multi_repos: JSON.stringify(members) }));
    const row = db.getProject("p1")!;
    assert.deepEqual(Db.parseMultiRepos(row), members);
    assert.deepEqual(Db.toProject(row).multi, { repos: members });
  });

  it("returns null on broken JSON or wrong shapes (degrades to plain-project look)", () => {
    assert.equal(Db.parseMultiRepos({ multi_repos: "not json" }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: "{}" }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: '["a"]' }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: '[{"dir":""}]' }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: '[{"dir":1}]' }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: '[{"dir":"/a","repoDir":2}]' }), null);
    assert.equal(Db.parseMultiRepos({ multi_repos: null }), null);
  });
});

describe("updateMultiRepos", () => {
  it("updates a container row", () => {
    const db = tmpDb();
    db.insertProject(projectRow({ multi_repos: JSON.stringify([{ dir: "/a" }]) }));
    db.updateMultiRepos("p1", [{ dir: "/a" }, { dir: "/b" }]);
    assert.deepEqual(Db.parseMultiRepos(db.getProject("p1")!), [{ dir: "/a" }, { dir: "/b" }]);
  });

  it("cannot reach a derived row — the SQL guard, not the route, is the proof", () => {
    const db = tmpDb();
    const stored = [{ dir: "/c/app-x/web", repoDir: "/a/web" }];
    db.insertProject(
      projectRow({
        source_project_id: "src",
        worktree_created_by_mojito: 1,
        multi_repos: JSON.stringify(stored),
      })
    );
    db.updateMultiRepos("p1", [{ dir: "/tmp/evil" }]);
    assert.deepEqual(Db.parseMultiRepos(db.getProject("p1")!), stored);
  });
});

describe("defaultWorktreeBranch", () => {
  it("round-trips through insert / toProject / update", () => {
    const db = tmpDb();
    db.insertProject(projectRow({ default_worktree_branch: "main" }));
    assert.equal(Db.toProject(db.getProject("p1")!).defaultWorktreeBranch, "main");

    const row = db.getProject("p1")!;
    db.updateProject({ ...row, default_worktree_branch: "origin/main" });
    assert.equal(db.getProject("p1")!.default_worktree_branch, "origin/main");

    db.updateProject({ ...db.getProject("p1")!, default_worktree_branch: null });
    assert.equal(db.getProject("p1")!.default_worktree_branch, null);
    assert.equal(Db.toProject(db.getProject("p1")!).defaultWorktreeBranch, undefined);
  });
});

describe("guardDirsOf", () => {
  it("collects working_dir plus member dirs and repo roots", () => {
    const derived = projectRow({
      working_dir: "/c/app-x",
      source_project_id: "src",
      multi_repos: JSON.stringify([{ dir: "/c/app-x/web", repoDir: "/a/web" }]),
    });
    assert.deepEqual(Db.guardDirsOf(derived), ["/c/app-x", "/c/app-x/web", "/a/web"]);
  });

  it("plain projects contribute just their working_dir; broken JSON contributes nothing extra", () => {
    assert.deepEqual(Db.guardDirsOf(projectRow()), ["/w"]);
    assert.deepEqual(Db.guardDirsOf(projectRow({ working_dir: null })), []);
    assert.deepEqual(Db.guardDirsOf(projectRow({ multi_repos: "broken" })), ["/w"]);
  });
});
