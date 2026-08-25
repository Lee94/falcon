import { strict as assert } from "node:assert";
import test from "node:test";
import {
  classify,
  extOf,
  indexCommand,
  INDEX_SKIP_DIRS,
  listCommand,
  parseEntries,
  parseIndexLines,
  parseRead,
  readCommand,
  relativizeIndexLine,
  relSegments,
  resolveInside,
  sortEntries,
} from "./files.js";

test("relSegments 拒绝越界路径", () => {
  assert.deepEqual(relSegments(undefined), []);
  assert.deepEqual(relSegments(""), []);
  assert.deepEqual(relSegments("src/git/path.ts"), ["src", "git", "path.ts"]);
  // 多余的分隔符是无害的，直接吃掉
  assert.deepEqual(relSegments("/src//a/"), ["src", "a"]);
  for (const bad of ["..", "a/../../etc", "./x", "a/\u0000b", "a/\nb"]) {
    assert.throws(() => relSegments(bad), /路径不合法/, bad);
  }
});

test("resolveInside 按宿主机规则拼路径", () => {
  assert.equal(resolveInside("posix", "/home/u/repo", "src/a.ts"), "/home/u/repo/src/a.ts");
  assert.equal(resolveInside("posix", "/home/u/repo", ""), "/home/u/repo");
  assert.equal(resolveInside("windows", "C:\\code\\repo", "src/a.ts"), "C:\\code\\repo\\src\\a.ts");
  // 前端一律用 /，Windows 远端也是——分隔符只在这一步落成平台形式
  assert.equal(resolveInside("windows", "C:/code/repo", "a"), "C:\\code\\repo\\a");
  assert.throws(() => resolveInside("posix", "/home/u/repo", "../other"), /路径不合法/);
});

test("listCommand：posix 逐条判目录，windows 走 EncodedCommand", () => {
  const posix = listCommand("posix", "/home/u/re po");
  assert.match(posix, /ls -1A "\$d"/);
  assert.match(posix, /'\/home\/u\/re po'/);
  assert.match(posix, /printf 'd %s/);
  assert.match(posix, /printf 'f %s/);

  const win = listCommand("windows", "C:\\code\\repo");
  assert.match(win, /^powershell/i);
  assert.match(win, /-EncodedCommand /);
  const script = Buffer.from(win.split(" ").pop()!, "base64").toString("utf16le");
  assert.match(script, /Get-ChildItem -LiteralPath \$d -Force/);
  assert.match(script, /PSIsContainer/);
});

test("parseEntries 认 d/f 前缀，忽略杂行", () => {
  const entries = parseEntries("d src\r\nf README.md\nf\n\nnoise\nd .github\n", "packages");
  assert.deepEqual(entries, [
    { name: "src", path: "packages/src", kind: "dir" },
    { name: "README.md", path: "packages/README.md", kind: "file" },
    { name: ".github", path: "packages/.github", kind: "dir" },
  ]);
  // 根目录下不带前缀
  assert.equal(parseEntries("f a.txt\n", "")[0]!.path, "a.txt");
});

test("relativizeIndexLine 收成工作目录相对路径", () => {
  assert.equal(relativizeIndexLine("/home/u/repo/src/a.ts", "/home/u/repo", "posix"), "src/a.ts");
  assert.equal(relativizeIndexLine("/home/u/repo", "/home/u/repo", "posix"), null);
  assert.equal(relativizeIndexLine("/etc/passwd", "/home/u/repo", "posix"), null);
  assert.equal(
    relativizeIndexLine("C:\\code\\repo\\src\\a.ts", "C:\\code\\repo", "windows"),
    "src/a.ts"
  );
  // Windows 大小写不敏感
  assert.equal(
    relativizeIndexLine("c:\\code\\repo\\b.ts", "C:\\code\\repo", "windows"),
    "b.ts"
  );
});

test("parseIndexLines 丢掉越界行，跳过 node_modules 段", () => {
  const { paths, truncated } = parseIndexLines(
    [
      "/home/u/repo/src/a.ts",
      "/home/u/repo/node_modules/x/index.js",
      "/etc/passwd",
      "/home/u/repo/README.md",
      "__TRUNCATED__",
    ].join("\n"),
    "/home/u/repo",
    "posix"
  );
  assert.deepEqual(paths, ["src/a.ts", "README.md"]);
  assert.equal(truncated, true);
});

test("indexCommand：posix 用 find prune，windows 走 EncodedCommand", () => {
  const posix = indexCommand("posix", "/home/u/re po");
  assert.match(posix, /find "\$d"/);
  assert.match(posix, /-name 'node_modules'/);
  assert.match(posix, /__TRUNCATED__/);

  const win = indexCommand("windows", "C:\\code\\repo");
  assert.match(win, /-EncodedCommand /);
  const script = Buffer.from(win.split(" ").pop()!, "base64").toString("utf16le");
  assert.match(script, /Get-ChildItem -LiteralPath \$p/);
  assert.match(script, /node_modules/);
  assert.ok(INDEX_SKIP_DIRS.includes("node_modules"));
});

test("sortEntries 目录在前，点开头的不沉底", () => {
  const sorted = sortEntries([
    { name: "b.ts", path: "b.ts", kind: "file" },
    { name: "node_modules", path: "node_modules", kind: "dir" },
    { name: ".github", path: ".github", kind: "dir" },
    { name: ".env", path: ".env", kind: "file" },
  ]);
  assert.deepEqual(
    sorted.map((e) => e.name),
    [".github", "node_modules", ".env", "b.ts"]
  );
});

test("readCommand 带上限，windows 循环读到 cap", () => {
  const posix = readCommand("posix", "/repo/a.ts", 1024);
  assert.match(posix, /wc -c < "\$f"/);
  assert.match(posix, /head -c 1024 "\$f" \| \$b64/);
  // base64 不一定存在，得有替补
  assert.match(posix, /openssl base64/);

  const script = Buffer.from(
    readCommand("windows", "C:\\repo\\a.ts", 1024).split(" ").pop()!,
    "base64"
  ).toString("utf16le");
  assert.match(script, /\$ms\.Length -lt 1024/);
  assert.match(script, /ToBase64String/);
});

test("parseRead：首行是真实大小，其余拼回 base64", () => {
  const body = Buffer.from("hello world").toString("base64");
  const res = parseRead(`11\n${body.slice(0, 4)}\n${body.slice(4)}\n`);
  assert.equal(res.size, 11);
  assert.equal(res.bytes.toString("utf8"), "hello world");
  assert.throws(() => parseRead("not-a-number\nzzz"), /读不到文件大小/);
});

test("classify：图片按扩展名，NUL 判二进制，其余当文本", () => {
  const png = classify("logo.png", 3, Buffer.from([1, 2, 3]));
  assert.equal(png.kind, "image");
  assert.equal(png.kind === "image" && png.mime, "image/png");

  const bin = classify("a.out", 4, Buffer.from([1, 0, 2, 3]));
  assert.equal(bin.kind, "binary");

  const text = classify("README.md", 5, Buffer.from("hello"));
  assert.equal(text.kind, "text");
  assert.equal(text.kind === "text" && text.truncated, false);

  // 取回来的字节比真实大小少 = 被 cap 截断了
  const cut = classify("big.log", 999_999, Buffer.from("head"));
  assert.equal(cut.kind === "text" && cut.truncated, true);

  // 超上限的图片给不出 data URL，只报大小
  const huge = classify("big.png", 99_000_000, Buffer.alloc(0));
  assert.equal(huge.kind, "too-large");
});

test("extOf 忽略点开头的无扩展名文件", () => {
  assert.equal(extOf("a.tar.gz"), "gz");
  assert.equal(extOf(".gitignore"), "");
  assert.equal(extOf("Makefile"), "");
});
