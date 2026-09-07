import { strict as assert } from "node:assert";
import test from "node:test";
import {
  deepestUploadDirs,
  formatSize,
  isHiddenName,
  joinHostPath,
  parentRel,
  parseNavPath,
  relDir,
} from "./filePath.js";

test("joinHostPath 按 workingDir 的分隔符拼", () => {
  assert.equal(joinHostPath("/home/u/repo", ""), "/home/u/repo");
  assert.equal(joinHostPath("/home/u/repo", "src/a.ts"), "/home/u/repo/src/a.ts");
  assert.equal(joinHostPath("C:\\code\\repo", "src/a.ts"), "C:\\code\\repo\\src\\a.ts");
  assert.equal(joinHostPath(undefined, "src"), "src");
  assert.equal(joinHostPath(undefined, ""), "/");
});

test("parseNavPath：绝对路径去掉工作目录前缀，越界拒绝", () => {
  assert.equal(parseNavPath("/home/u/repo", "/home/u/repo"), "");
  assert.equal(parseNavPath("/home/u/repo/", "/home/u/repo"), "");
  assert.equal(parseNavPath("/home/u/repo/src", "/home/u/repo"), "src");
  assert.equal(parseNavPath("/home/u/repo/src/a.ts", "/home/u/repo"), "src/a.ts");
  assert.equal(parseNavPath("src/foo", "/home/u/repo"), "src/foo");
  assert.equal(parseNavPath("/etc", "/home/u/repo"), null);
  assert.equal(parseNavPath("../x", "/home/u/repo"), null);
  assert.equal(parseNavPath("src/../etc", "/home/u/repo"), null);
  // 没有 workingDir 时，开头的 / 当成工作目录根
  assert.equal(parseNavPath("/src/a", undefined), "src/a");
  assert.equal(parseNavPath("src", undefined), "src");
});

test("parseNavPath Windows 大小写不敏感", () => {
  assert.equal(parseNavPath("c:\\code\\repo\\src", "C:\\code\\repo"), "src");
  assert.equal(parseNavPath("C:/code/repo/src", "C:\\code\\repo"), "src");
  assert.equal(parseNavPath("D:\\other", "C:\\code\\repo"), null);
});

test("parentRel / relDir / hidden", () => {
  assert.equal(parentRel(""), null);
  assert.equal(parentRel("src"), "");
  assert.equal(parentRel("src/a.ts"), "src");
  assert.equal(relDir("src/a.ts"), "src");
  assert.equal(relDir("a.ts"), "");
  assert.equal(isHiddenName(".env"), true);
  assert.equal(isHiddenName("env"), false);
});

test("deepestUploadDirs 只留最深的一层", () => {
  assert.deepEqual(deepestUploadDirs("", ["src/a.ts", "src/lib/b.ts", "README.md"]), ["src/lib"]);
  assert.deepEqual(deepestUploadDirs("pkg", ["foo/a.ts", "foo/bar/b.ts"]), ["pkg/foo/bar"]);
  assert.deepEqual(deepestUploadDirs("pkg", ["a.ts"]), []);
});

test("formatSize", () => {
  assert.equal(formatSize(undefined), "—");
  assert.equal(formatSize(370), "370 B");
  assert.equal(formatSize(10.42 * 1024), "10.4 KB");
});
