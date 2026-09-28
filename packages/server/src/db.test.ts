import assert from "node:assert/strict";
import { describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
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

describe("relays: 旧的按项目规则迁到主机", () => {
  /**
   * 先让 Db 建好全套 schema，再用第二条原始连接补出旧版的两张表与数据，
   * 最后重新 new Db 触发迁移——与老用户升级时的顺序一致。
   */
  function legacyDir(): string {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-db-"));
    const seed = new Db(dir);
    seed.insertHost({
      id: "h1",
      name: "linux",
      host: "10.0.0.1",
      port: 22,
      username: "fay",
      auth_method: "agent",
      key_path: null,
      secret_enc: null,
      created_at: 1,
    });
    // p1 绑了主机；p2 没绑但连接三元组对得上；p3 哪台都对不上
    const ssh = { type: "ssh" as const, ssh_host: "10.0.0.1", ssh_port: 22, ssh_username: "fay" };
    seed.insertProject(projectRow({ id: "p1", host_id: "h1", ...ssh }));
    seed.insertProject(projectRow({ id: "p2", ...ssh }));
    seed.insertProject(projectRow({ id: "p3", ...ssh, ssh_username: "bob" }));

    const raw = new DatabaseSync(path.join(dir, "falcon.db"));
    raw.exec(`
      CREATE TABLE ssh_forwards (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT, kind TEXT NOT NULL,
        bind_host TEXT NOT NULL, bind_port INTEGER NOT NULL, dest_host TEXT NOT NULL, dest_port INTEGER NOT NULL,
        enabled INTEGER NOT NULL, created_at INTEGER NOT NULL);
      CREATE TABLE public_shares (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT, origin TEXT NOT NULL,
        dest_host TEXT NOT NULL, dest_port INTEGER NOT NULL, enabled INTEGER NOT NULL, created_at INTEGER NOT NULL);
      INSERT INTO ssh_forwards VALUES
        ('f1', 'p1', 'db', 'local', '127.0.0.1', 5432, '127.0.0.1', 5432, 1, 10),
        ('f2', 'p2', NULL, 'local', '127.0.0.1', 5432, '127.0.0.1', 15432, 1, 20),
        ('f3', 'p3', NULL, 'local', '127.0.0.1', 9000, '127.0.0.1', 9000, 1, 30);
      INSERT INTO public_shares VALUES
        ('s1', 'p1', NULL, 'remote', '127.0.0.1', 3000, 1, 10),
        ('s2', 'p3', NULL, 'local', '127.0.0.1', 6789, 1, 20),
        ('s3', 'p3', NULL, 'remote', '127.0.0.1', 3000, 1, 30);
    `);
    raw.close();
    return dir;
  }

  it("moves rules onto hosts, keeps one enabled per port, drops what has no host", () => {
    const db = new Db(legacyDir());
    const forwards = db.listForwards();
    assert.deepEqual(
      forwards.map((f) => [f.id, f.host_id, f.enabled]),
      [
        ["f1", "h1", 1],
        // 并到同一台机器后与 f1 抢本机 5432：留最早的 f1
        ["f2", "h1", 0],
      ]
    );
    const shares = db.listShares();
    assert.deepEqual(
      shares.map((s) => [s.id, s.host_id, s.enabled]),
      [
        ["s1", "h1", 1],
        // origin=local 本来就是后端本机，与项目挂哪台主机无关
        ["s2", null, 1],
      ]
    );
  });

  it("drops the legacy tables so it runs once, and is harmless on a fresh db", () => {
    const dir = legacyDir();
    new Db(dir);
    const again = new Db(dir);
    assert.equal(again.listForwards().length, 2);
    const raw = new DatabaseSync(path.join(dir, "falcon.db"));
    const legacy = raw
      .prepare("SELECT name FROM sqlite_master WHERE name IN ('ssh_forwards', 'public_shares')")
      .all();
    raw.close();
    assert.deepEqual(legacy, []);
    assert.deepEqual(tmpDb().listForwards(), []);
  });
});

describe("deleteRelaysOfHost", () => {
  it("removes that host's forwards and shares, leaves 本机 and other hosts alone", () => {
    const db = tmpDb();
    const fwd = (id: string, host_id: string) => ({
      id,
      host_id,
      name: null,
      kind: "local",
      bind_host: "127.0.0.1",
      bind_port: 1,
      dest_host: "127.0.0.1",
      dest_port: 1,
      enabled: 0,
      created_at: 1,
    });
    const share = (id: string, host_id: string | null) => ({
      id,
      host_id,
      name: null,
      dest_host: "127.0.0.1",
      dest_port: 1,
      enabled: 0,
      created_at: 1,
    });
    db.insertForward(fwd("f1", "h1"));
    db.insertForward(fwd("f2", "h2"));
    db.insertShare(share("s1", "h1"));
    db.insertShare(share("s2", null));
    db.deleteRelaysOfHost("h1");
    assert.deepEqual(db.listForwards().map((f) => f.id), ["f2"]);
    assert.deepEqual(db.listShares().map((s) => s.id), ["s2"]);
  });
});
