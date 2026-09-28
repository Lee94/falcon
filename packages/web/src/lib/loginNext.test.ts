import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { loginNext } from "./loginNext.js";

describe("loginNext", () => {
  it("returns the px0 entry the server sent us away from", () => {
    assert.equal(loginNext("?next=%2Fpx0%2Fp1%2F"), "/px0/p1/");
    assert.equal(loginNext("?next=%2Fpx0%2Fp1%2F%3Fretry%3D1"), "/px0/p1/?retry=1");
  });

  it("ignores anything that could leave the site or is not px0", () => {
    for (const next of ["//evil.example/px0/", "/\\evil.example", "https://evil.example/px0/", "javascript:alert(1)", "/api/sessions", "/"]) {
      assert.equal(loginNext(`?next=${encodeURIComponent(next)}`), null, next);
    }
    assert.equal(loginNext(""), null);
  });
});
