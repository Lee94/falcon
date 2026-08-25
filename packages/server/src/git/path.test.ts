import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  memberWorktreePath,
  multiCentralPath,
  nameSlug,
  vetoMultiRoots,
} from "./path.js";

describe("nameSlug", () => {
  it("keeps CJK and legal punctuation", () => {
    assert.equal(nameSlug("我的组合项目"), "我的组合项目");
    assert.equal(nameSlug("app_v2.core"), "app_v2.core");
  });

  it("replaces separators, windows-illegal chars and whitespace with dashes", () => {
    assert.equal(nameSlug("a/b\\c:d*e?f\"g<h>i|j k"), "a-b-c-d-e-f-g-h-i-j-k");
    assert.equal(nameSlug("tab\there"), "tab-here");
  });

  it("strips leading/trailing dots and dashes (windows swallows them silently)", () => {
    assert.equal(nameSlug("..proj.."), "proj");
    assert.equal(nameSlug("--proj--"), "proj");
    assert.equal(nameSlug("proj. "), "proj");
  });

  it("truncates by code point without splitting surrogate pairs", () => {
    const emoji = "😀".repeat(60);
    const slug = nameSlug(emoji);
    assert.equal(Array.from(slug).length, 48);
    // 没有半个代理对：能无损地 round-trip
    assert.equal(Array.from(slug).every((c) => c === "😀"), true);
  });

  it("falls back to 'multi' when nothing survives", () => {
    assert.equal(nameSlug("///"), "multi");
    assert.equal(nameSlug("  "), "multi");
    assert.equal(nameSlug(""), "multi");
  });
});

describe("multiCentralPath", () => {
  it("posix: sibling of the first repo root, named <nameSlug>-<branchSlug>", () => {
    assert.equal(
      multiCentralPath("posix", "/home/u/code/web", "我的组合", "feat/x"),
      "/home/u/code/我的组合-feat-x"
    );
  });

  it("windows: backslash join, tolerates trailing separator on the root", () => {
    assert.equal(
      multiCentralPath("windows", "D:\\code\\web\\", "App Suite", "feat/x"),
      "D:\\code\\App-Suite-feat-x"
    );
  });
});

describe("memberWorktreePath", () => {
  it("appends the repo root basename verbatim", () => {
    assert.equal(
      memberWorktreePath("posix", "/home/u/code/app-feat-x", "/srv/repos/中文仓库"),
      "/home/u/code/app-feat-x/中文仓库"
    );
    assert.equal(
      memberWorktreePath("windows", "D:\\code\\app-feat-x", "D:\\repos\\Web"),
      "D:\\code\\app-feat-x\\Web"
    );
  });
});

describe("vetoMultiRoots", () => {
  it("passes distinct absolute roots with distinct basenames", () => {
    assert.equal(vetoMultiRoots("posix", ["/a/web", "/a/server", "/b/shared"]), null);
  });

  it("rejects relative roots", () => {
    assert.match(vetoMultiRoots("posix", ["/a/web", "code/server"])!, /绝对路径/);
  });

  it("rejects the same repo appearing twice (two members were subdirs of one repo)", () => {
    assert.match(vetoMultiRoots("posix", ["/a/web", "/a/web"])!, /同一个仓库/);
  });

  it("windows: same repo via case alias and slash direction", () => {
    assert.match(vetoMultiRoots("windows", ["D:\\code\\Web", "d:/code/web"])!, /同一个仓库/);
  });

  it("rejects basename collisions (cannot tile into one central dir)", () => {
    assert.match(vetoMultiRoots("posix", ["/a/web", "/b/web"])!, /同名/);
  });

  it("windows: basename collision is case-insensitive", () => {
    assert.match(vetoMultiRoots("windows", ["C:\\a\\Web", "C:\\b\\web"])!, /同名/);
  });

  it("posix: basenames differing only by case are distinct", () => {
    assert.equal(vetoMultiRoots("posix", ["/a/Web", "/b/web"]), null);
  });
});
