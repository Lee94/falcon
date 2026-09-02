import { strict as assert } from "node:assert";
import test from "node:test";
import { langForFence, langForPath, splitCodeLines, tokenStyle } from "./highlight.js";

test("langForPath 按扩展名认语言，别名归一到规范 id", () => {
  assert.equal(langForPath("packages/web/src/store.ts"), "typescript");
  assert.equal(langForPath("src/App.tsx"), "tsx");
  assert.equal(langForPath("a/b/mod.MJS"), "javascript");
  assert.equal(langForPath("styles.scss"), "scss");
  assert.equal(langForPath("Cargo.toml"), "toml");
  assert.equal(langForPath("layout.kdl"), "kdl");
  assert.equal(langForPath("include/foo.hpp"), "cpp");
  assert.equal(langForPath("scripts\\build.ps1"), "powershell");
});

test("langForPath 认无扩展名的特殊文件名", () => {
  assert.equal(langForPath("Dockerfile"), "dockerfile");
  assert.equal(langForPath("deploy/Makefile"), "make");
  assert.equal(langForPath(".env"), "dotenv");
  assert.equal(langForPath(".env.local"), "dotenv");
  assert.equal(langForPath(".zshrc"), "shellscript");
});

test("langForPath 认不出就 null，不瞎猜", () => {
  assert.equal(langForPath("notes.txt"), null);
  assert.equal(langForPath("LICENSE"), null);
  assert.equal(langForPath(".gitignore"), null);
  assert.equal(langForPath("archive.tar.gz"), null);
  // 以点结尾的怪名字不应该抛异常
  assert.equal(langForPath("weird."), null);
});

test("langForFence 只认 info string 的第一个词", () => {
  assert.equal(langForFence("ts"), "typescript");
  assert.equal(langForFence("bash"), "shellscript");
  assert.equal(langForFence("ts title=demo.ts"), "typescript");
  assert.equal(langForFence("  Python  "), "python");
  assert.equal(langForFence(""), null);
  assert.equal(langForFence(undefined), null);
  assert.equal(langForFence("no-such-lang"), null);
});

test("splitCodeLines 吞掉末尾换行的幽灵空行，保留中间空行", () => {
  assert.deepEqual(splitCodeLines("a\nb\n"), ["a", "b"]);
  assert.deepEqual(splitCodeLines("a\n\nb"), ["a", "", "b"]);
  assert.deepEqual(splitCodeLines("a"), ["a"]);
});

test("tokenStyle 把 shiki 单主题 token 拼成 React style，颜色是 CSS 变量引用原样透传", () => {
  assert.deepEqual(tokenStyle({ color: "var(--shiki-token-keyword)", fontStyle: 0 }), {
    color: "var(--shiki-token-keyword)",
  });
  assert.deepEqual(tokenStyle({ color: "var(--shiki-token-link)", fontStyle: 1 | 2 | 4 }), {
    color: "var(--shiki-token-link)",
    fontStyle: "italic",
    fontWeight: "bold",
    textDecoration: "underline",
  });
  assert.equal(tokenStyle({ fontStyle: -1 }), undefined);
  assert.equal(tokenStyle({}), undefined);
});
