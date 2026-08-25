import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { commonParentDir } from "./multiPath.js";

describe("commonParentDir", () => {
  it("posix: nearest common ancestor", () => {
    assert.equal(commonParentDir(["/home/u/code/web", "/home/u/code/srv"]), "/home/u/code");
    assert.equal(commonParentDir(["/home/u/code/web"]), "/home/u/code/web");
  });

  it("a member that is the ancestor of another is itself the answer", () => {
    assert.equal(commonParentDir(["/a/web", "/a/web/vendor/lib"]), "/a/web");
  });

  it("windows: backslash join, case-insensitive compare, keeps first spelling", () => {
    assert.equal(commonParentDir(["D:\\Code\\Web", "d:\\code\\srv"]), "D:\\Code");
  });

  it("returns empty when only the fs root (or nothing) is shared", () => {
    assert.equal(commonParentDir(["/a/x", "/b/y"]), "");
    assert.equal(commonParentDir(["C:\\a", "D:\\b"]), "");
    assert.equal(commonParentDir([]), "");
    assert.equal(commonParentDir(["  "]), "");
  });

  it("tolerates trailing separators", () => {
    assert.equal(commonParentDir(["/a/b/", "/a/c/"]), "/a");
  });
});
