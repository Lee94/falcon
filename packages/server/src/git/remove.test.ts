import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { MultiRepoMember } from "@falcon/shared";
import type { ProjectRow } from "../db.js";
import type { HostKind } from "../zellij/host.js";
import { vetoMultiRemoval, vetoRemoval } from "./remove.js";

/** 手工构造脏数据行。缺省是一条"应该允许删"的健康多仓库派生行 */
function row(overrides: Partial<ProjectRow> = {}): ProjectRow {
  return {
    id: "p1",
    name: "组合-feat-x",
    type: "local",
    working_dir: "/home/u/code/app-feat-x",
    shell: null,
    ssh_host: null,
    ssh_port: null,
    ssh_username: null,
    ssh_auth_method: null,
    ssh_key_path: null,
    ssh_secret_enc: null,
    host_id: null,
    created_at: 0,
    source_project_id: "src1",
    worktree_branch: "feat/x",
    worktree_repo_dir: null,
    worktree_created_by_mojito: 1,
    worktree_archived_at: null,
    multi_repos: null,
    default_worktree_branch: null,
    ...overrides,
  };
}

const MEMBERS: MultiRepoMember[] = [
  { dir: "/home/u/code/app-feat-x/web", repoDir: "/home/u/code/web" },
  { dir: "/home/u/code/app-feat-x/server", repoDir: "/home/u/code/server" },
];

const HOME = "/home/u";

function veto(
  overrides: Partial<ProjectRow> = {},
  members: MultiRepoMember[] | null = MEMBERS,
  otherDirs: string[] = [],
  kind: HostKind = "posix"
): string | null {
  return vetoMultiRemoval(kind, row(overrides), members, HOME, otherDirs);
}

describe("vetoMultiRemoval", () => {
  it("allows a healthy derived row", () => {
    assert.equal(veto(), null);
  });

  it("① rejects a non-derived row (a container must never reach deletion)", () => {
    assert.match(veto({ source_project_id: null })!, /不是附属项目/);
  });

  it("② rejects when the dir was not created by falcon", () => {
    assert.match(veto({ worktree_created_by_mojito: 0 })!, /不是 Falcon 创建/);
    assert.match(veto({ worktree_created_by_mojito: null })!, /不是 Falcon 创建/);
  });

  it("③ rejects a broken/empty/oversized member list", () => {
    assert.match(veto({}, null)!, /成员记录损坏/);
    assert.match(veto({}, [])!, /成员记录损坏/);
    const many = Array.from({ length: 17 }, (_, i) => ({
      dir: `/home/u/code/app-feat-x/r${i}`,
      repoDir: `/home/u/code/r${i}`,
    }));
    assert.match(veto({}, many)!, /成员记录损坏/);
  });

  it("④ rejects a relative / UNC / too-shallow central dir", () => {
    assert.match(veto({ working_dir: "code/app-feat-x" })!, /绝对路径/);
    assert.match(veto({ working_dir: null })!, /绝对路径/);
    assert.match(
      vetoMultiRemoval("windows", row({ working_dir: "\\\\srv\\share\\x" }), MEMBERS, HOME, [])!,
      /UNC/
    );
    assert.match(veto({ working_dir: "/app-feat-x" })!, /过浅/);
  });

  it("⑤ rejects home and ancestors of home, but allows living inside home", () => {
    assert.match(veto({ working_dir: "/home/u" })!, /家目录/);
    // central = /home 是 home 的祖先——同时也过浅，但先撞哪条都必须拒
    assert.notEqual(veto({ working_dir: "/home" }), null);
    // 健康行本来就在 home 里面（MEMBERS 的 central 是 /home/u/code/app-feat-x）
    assert.equal(veto(), null);
  });

  it("⑥ rejects members outside the central dir (the bounding box)", () => {
    const out = [
      { dir: "/home/u/code/elsewhere/web", repoDir: "/home/u/code/web" },
      MEMBERS[1]!,
    ];
    assert.match(veto({}, out)!, /不在集中目录/);
    // 成员等于集中目录本身也不行——isAncestor 是严格的
    const same = [{ dir: "/home/u/code/app-feat-x", repoDir: "/home/u/code/web" }];
    assert.match(veto({}, same)!, /不在集中目录/);
  });

  it("⑥ rejects relative member dirs", () => {
    const rel = [{ dir: "web", repoDir: "/home/u/code/web" }];
    assert.match(veto({}, rel)!, /绝对路径/);
  });

  it("⑦ rejects a missing repoDir record", () => {
    const noRepo = [{ dir: "/home/u/code/app-feat-x/web" }];
    assert.match(veto({}, noRepo)!, /缺少仓库根/);
  });

  it("⑦ rejects a member that equals or contains any repo root", () => {
    const evil = [
      {
        dir: "/home/u/code/app-feat-x/web",
        repoDir: "/home/u/code/app-feat-x/web",
      },
    ];
    assert.match(veto({}, evil)!, /覆盖仓库根/);
  });

  it("⑧ rejects a central dir that contains a repo root", () => {
    const inside = [{ dir: "/home/u/code/app-feat-x/web", repoDir: "/home/u/code/app-feat-x/web/upstream" }];
    // 成员 dir 是 repoDir 的祖先 → ⑦ 先拦；换一个只触发 ⑧ 的形状：
    assert.notEqual(veto({}, inside), null);
    const central = [
      { dir: "/home/u/code/app-feat-x/other", repoDir: "/home/u/code/app-feat-x/repo" },
    ];
    assert.match(veto({}, central)!, /集中目录 .* 包含仓库根/);
  });

  it("⑨ rejects clashes with other projects' guarded dirs, member- and central-level", () => {
    assert.match(veto({}, MEMBERS, ["/home/u/code/app-feat-x/web/sub"])!, /另一个项目/);
    assert.match(veto({}, MEMBERS, ["/home/u/code/app-feat-x"])!, /另一个项目/);
  });

  it("windows: mixed separators and case still match (canonKey everywhere)", () => {
    const winRow = row({ working_dir: "D:\\code\\App-feat-x" });
    const winMembers = [{ dir: "D:/code/app-feat-x/Web", repoDir: "D:\\repos\\Web" }];
    assert.equal(vetoMultiRemoval("windows", winRow, winMembers, "C:\\Users\\u", []), null);
    // 集中目录大小写别名踩到别的项目目录上
    assert.match(
      vetoMultiRemoval("windows", winRow, winMembers, "C:\\Users\\u", ["d:\\code\\app-feat-x"])!,
      /另一个项目/
    );
  });

  it("posix: a backslash is a legal filename char, not a separator", () => {
    // 成员名里带反斜杠，不该被归一化成路径层级
    const weird = [{ dir: "/home/u/code/app-feat-x/a\\b", repoDir: "/home/u/code/ab" }];
    assert.equal(veto({}, weird), null);
  });
});

describe("vetoRemoval (single) still rejects multi-shaped misuse", () => {
  it("a multi derived row must never reach the single-path guard with repoDir null", () => {
    // cleanupWorktree 按 multi_repos 分派，理论上到不了这里；万一到了，
    // 单版护栏的 ③（worktree_repo_dir 缺失）也要能兜住
    const r = row({ multi_repos: "[]" });
    assert.match(vetoRemoval("posix", r, HOME, [])!, /仓库根记录缺失/);
  });
});
