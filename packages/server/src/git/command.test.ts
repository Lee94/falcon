import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  branchExistsArgs,
  countStatusChanges,
  logPageArgs,
  lsFilesIndexArgs,
  parseCommitFiles,
  parseBranchList,
  parseCommitMeta,
  parseLogPage,
  parseLsFiles,
  parseRefLabels,
  parseStatusEntries,
  rankAuthors,
  truncateDiff,
} from "./command.js";

describe("lsFilesIndexArgs", () => {
  it("lists cached + others, excludes gitignore, stays relative to -C", () => {
    const argv = lsFilesIndexArgs("/usr/bin/git", "/home/u/repo");
    assert.deepEqual(argv.slice(-3), ["ls-files", "-co", "--exclude-standard"]);
    assert.equal(argv[argv.indexOf("-C") + 1], "/home/u/repo");
  });
});

describe("parseLsFiles", () => {
  it("drops empty lines and normalizes backslashes", () => {
    assert.deepEqual(parseLsFiles("src/a.ts\r\n\nREADME.md\npackages\\web\\x.ts\n"), [
      "src/a.ts",
      "README.md",
      "packages/web/x.ts",
    ]);
  });
});

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

describe("logPageArgs", () => {
  it("looks at branches/remotes/tags rather than --all", () => {
    // --all 会把 IDE 写在 refs/jb/* 下的 Local History 提交也算进来
    const args = logPageArgs("git", "/repo");
    assert.ok(args.includes("--branches"));
    assert.ok(args.includes("--remotes"));
    assert.ok(args.includes("--tags"));
    assert.ok(!args.includes("--all"));
  });

  it("takes one extra row so the caller can tell whether there is a next page", () => {
    const args = logPageArgs("git", "/repo", { limit: 20 });
    assert.equal(args[args.indexOf("-n") + 1], "21");
  });

  it("treats the search box as a literal, not a regex", () => {
    const args = logPageArgs("git", "/repo", { grep: "fix(" });
    assert.ok(args.includes("--fixed-strings"));
    assert.ok(args.includes("--regexp-ignore-case"));
    assert.ok(args.includes("--grep=fix("));
  });

  it("terminates a branch filter with -- so a leading dash cannot become an option", () => {
    const args = logPageArgs("git", "/repo", { rev: "-weird-branch" });
    assert.deepEqual(args.slice(-2), ["-weird-branch", "--"]);
    assert.ok(!args.includes("--branches"));
  });
});

describe("parseRefLabels", () => {
  it("splits decoration into local, remote and tag labels", () => {
    const refs = parseRefLabels("HEAD -> main, origin/main, tag: v1.0", ["origin"]);
    assert.deepEqual(refs, [
      { name: "main", kind: "local", head: true },
      { name: "origin/main", kind: "remote" },
      { name: "v1.0", kind: "tag" },
    ]);
  });

  it("drops origin/HEAD and a detached bare HEAD", () => {
    assert.deepEqual(parseRefLabels("HEAD, origin/HEAD, origin/main", ["origin"]), [
      { name: "origin/main", kind: "remote" },
    ]);
  });

  it("returns nothing for an undecorated commit", () => {
    assert.deepEqual(parseRefLabels("", ["origin"]), []);
  });
});

describe("parseLogPage", () => {
  const row = (...cols: string[]) => cols.join("\t");

  it("parses parents, refs and a subject containing tabs", () => {
    const out = parseLogPage(
      row("a".repeat(40), "aaaaaaa", "fay", "f@x.io", "1700000000", "", "HEAD -> main", "fix:\tthing")
    );
    assert.equal(out.length, 1);
    assert.deepEqual(out[0]!.parents, []);
    assert.equal(out[0]!.subject, "fix:\tthing");
    assert.equal(out[0]!.authoredAt, 1_700_000_000_000);
    assert.equal(out[0]!.decoration, "HEAD -> main");
  });

  it("keeps both parents of a merge in git order", () => {
    const out = parseLogPage(
      row("m".repeat(40), "mmmmmmm", "fay", "f@x.io", "1", `${"p".repeat(40)} ${"q".repeat(40)}`, "", "Merge")
    );
    assert.deepEqual(out[0]!.parents, ["p".repeat(40), "q".repeat(40)]);
  });

  it("drops short rows instead of drawing a wrong graph", () => {
    assert.deepEqual(parseLogPage("deadbeef\tfay\tbroken"), []);
  });
});

describe("rankAuthors", () => {
  it("orders by commit count, most active first", () => {
    assert.deepEqual(rankAuthors("fay\nlee\nfay\nfay\nlee\nzhu\n"), ["fay", "lee", "zhu"]);
  });
});

describe("parseCommitMeta", () => {
  it("reads author and committer timestamps separately", () => {
    const meta = parseCommitMeta(
      ["s".repeat(40), "sssssss", "fay", "f@x.io", "1700000000", "lee", "1700000060", "", ""].join("\t")
    );
    assert.equal(meta?.authoredAt, 1_700_000_000_000);
    assert.equal(meta?.committedAt, 1_700_000_060_000);
    assert.equal(meta?.committer, "lee");
  });

  it("returns null when the row is malformed", () => {
    assert.equal(parseCommitMeta("nope"), null);
  });
});

describe("parseCommitFiles", () => {
  it("pairs the raw section with the numstat section by position", () => {
    const files = parseCommitFiles(
      [
        ":000000 100644 0000000 353b768 A\tCLAUDE.md",
        ":100644 100644 f35f9b5 e5a4e35 M\tpackage.json",
        "75\t0\tCLAUDE.md",
        "2\t1\tpackage.json",
      ].join("\n")
    );
    assert.deepEqual(files, [
      { path: "CLAUDE.md", status: "A", added: 75, deleted: 0 },
      { path: "package.json", status: "M", added: 2, deleted: 1 },
    ]);
  });

  it("keeps both paths of a rename and strips the similarity score", () => {
    const files = parseCommitFiles(
      [":100644 100644 aaa bbb R100\told.ts\tnew.ts", "0\t0\tsrc/{old.ts => new.ts}"].join("\n")
    );
    assert.deepEqual(files, [
      { path: "new.ts", origPath: "old.ts", status: "R", added: 0, deleted: 0 },
    ]);
  });

  it("records a binary file as unknown line counts, not zero", () => {
    const files = parseCommitFiles(
      [":100644 100644 aaa bbb M\tlogo.png", "-\t-\tlogo.png"].join("\n")
    );
    assert.deepEqual(files[0], { path: "logo.png", status: "M", added: null, deleted: null });
  });

  it("drops every line count when the two sections disagree, rather than misattributing them", () => {
    const files = parseCommitFiles(
      [
        ":000000 100644 0000000 353b768 A\ta.ts",
        ":100644 100644 f35f9b5 e5a4e35 M\tb.ts",
        "5\t1\tb.ts",
      ].join("\n")
    );
    assert.deepEqual(
      files.map((f) => [f.path, f.added, f.deleted]),
      [
        ["a.ts", null, null],
        ["b.ts", null, null],
      ]
    );
  });

  it("returns nothing for a commit with no diff", () => {
    assert.deepEqual(parseCommitFiles(""), []);
  });
});

describe("parseBranchList", () => {
  const row = (name: string, upstream = "", head = "", symref = "") =>
    [name, upstream, head, symref].join("\t");

  it("drops origin/HEAD, which git shortens to a bare remote name", () => {
    // refs/remotes/origin/HEAD 的 %(refname:short) 是 "origin"，不是 "origin/HEAD"
    const out = parseBranchList(
      [
        row("main", "origin/main", "*"),
        row("origin", "", "", "refs/remotes/origin/main"),
        row("origin/main"),
      ].join("\n"),
      ["origin"]
    );
    assert.deepEqual(
      out.map((b) => b.name),
      ["main", "origin/main"]
    );
  });

  it("marks the checked-out branch and keeps its upstream", () => {
    const out = parseBranchList(row("main", "origin/main", "*"), ["origin"]);
    assert.deepEqual(out, [
      { name: "main", remote: false, upstream: "origin/main", head: true },
    ]);
  });
});

describe("branchExistsArgs", () => {
  it("verifies refs/heads/<branch> quietly, anchored to -C <repo>", () => {
    const argv = branchExistsArgs("/usr/bin/git", "/home/u/repo", "feat/x");
    assert.deepEqual(argv.slice(-4), ["rev-parse", "--verify", "--quiet", "refs/heads/feat/x"]);
    assert.equal(argv[argv.indexOf("-C") + 1], "/home/u/repo");
  });
});
