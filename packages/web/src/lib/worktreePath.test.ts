import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { previewDir, previewMultiDir } from "./worktreePath.js";

describe("previewDir", () => {
  it("mirrors siblingWorktreePath: sibling of repo root, slugged branch", () => {
    assert.equal(previewDir("/home/u/code/web", "feat/x"), "/home/u/code/web-feat-x");
    assert.equal(previewDir("D:\\code\\web\\", "feat/x"), "D:\\code\\web-feat-x");
  });

  it("keeps CJK, strips illegal chars, truncates by code point", () => {
    assert.equal(previewDir("/a/repo", "功能/中文"), "/a/repo-功能-中文");
    const long = "x".repeat(60);
    assert.equal(previewDir("/a/repo", long), `/a/repo-${"x".repeat(48)}`);
  });

  it("returns empty until both inputs exist", () => {
    assert.equal(previewDir("", "feat"), "");
    assert.equal(previewDir("/a/repo", ""), "");
  });
});

describe("previewMultiDir", () => {
  it("mirrors multiCentralPath: <baseDir>/<nameSlug>-<branchSlug>", () => {
    assert.equal(
      previewMultiDir("/home/u/code", "我的组合", "feat/x"),
      "/home/u/code/我的组合-feat-x"
    );
    assert.equal(
      previewMultiDir("D:\\code\\", "App Suite", "feat/x"),
      "D:\\code\\App-Suite-feat-x"
    );
  });

  it("name slug is harsher than branch slug (windows-illegal chars in names)", () => {
    assert.equal(previewMultiDir("/a", "s:u*ite?", "b"), "/a/s-u-ite-b");
  });

  it("falls back to 'multi' when the name has nothing usable", () => {
    assert.equal(previewMultiDir("/a", "///", "b"), "/a/multi-b");
  });
});
