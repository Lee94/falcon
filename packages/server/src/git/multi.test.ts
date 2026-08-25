import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MULTI_REPO_MAX } from "@falcon/shared";
import { validateMemberList } from "./multi.js";

describe("validateMemberList", () => {
  it("trims and strips trailing separators", () => {
    const res = validateMemberList(["  /a/web/  ", "D:\\code\\srv\\"]);
    assert.deepEqual(res, { ok: true, repos: ["/a/web", "D:\\code\\srv"] });
  });

  it("rejects non-arrays and non-string items", () => {
    assert.equal(validateMemberList("not-an-array").ok, false);
    assert.equal(validateMemberList([1]).ok, false);
    assert.equal(validateMemberList([null]).ok, false);
  });

  it("rejects empty entries and empty lists", () => {
    assert.equal(validateMemberList(["  "]).ok, false);
    assert.equal(validateMemberList([]).ok, false);
  });

  it("rejects exact duplicates (after normalization)", () => {
    assert.equal(validateMemberList(["/a/web", "/a/web/"]).ok, false);
  });

  it("keeps case-different paths — alias detection needs the host kind, so it waits for derive", () => {
    assert.equal(validateMemberList(["/a/Web", "/a/web"]).ok, true);
  });

  it("allows nested members: an independent repo inside another member is a real layout", () => {
    assert.equal(validateMemberList(["/a/web", "/a/web/vendor/lib"]).ok, true);
  });

  it("caps the member count at MULTI_REPO_MAX", () => {
    const many = Array.from({ length: MULTI_REPO_MAX + 1 }, (_, i) => `/r/${i}`);
    assert.equal(validateMemberList(many).ok, false);
    assert.equal(validateMemberList(many.slice(0, MULTI_REPO_MAX)).ok, true);
  });

  it("keeps a bare '/' intact instead of stripping it to nothing", () => {
    const res = validateMemberList(["/"]);
    assert.deepEqual(res, { ok: true, repos: ["/"] });
  });
});
