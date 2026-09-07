import { strict as assert } from "node:assert";
import test from "node:test";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import {
  B64_LINE_BYTES,
  Base64LineDecoder,
  Base64LineEncoder,
  contentDisposition,
  downloadCommand,
  uploadCommand,
  uploadTarget,
  validateUploadName,
} from "./transfer.js";

function decodeWin(cmd: string): string {
  return Buffer.from(cmd.split(" ").pop()!, "base64").toString("utf16le");
}

async function collect(stream: Readable): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const c of stream) chunks.push(Buffer.isBuffer(c) ? c : Buffer.from(c));
  return Buffer.concat(chunks);
}

test("Base64LineEncoder：每行编码 B64_LINE_BYTES 个字节，跨包也能对齐", async () => {
  const data = Buffer.alloc(B64_LINE_BYTES * 2 + 7, 0xab);
  // 故意按奇怪的边界切包，行边界不能跟着包边界走
  const src = Readable.from([data.subarray(0, 100), data.subarray(100, B64_LINE_BYTES + 5), data.subarray(B64_LINE_BYTES + 5)]);
  const enc = new Base64LineEncoder();
  const out = (await collect(src.pipe(enc))).toString("latin1");
  const lines = out.split("\n");
  assert.equal(lines.pop(), "", "以换行结尾");
  assert.equal(lines.length, 3);
  assert.equal(Buffer.from(lines[0]!, "base64").length, B64_LINE_BYTES);
  assert.equal(Buffer.from(lines[1]!, "base64").length, B64_LINE_BYTES);
  assert.equal(Buffer.from(lines[2]!, "base64").length, 7);
  assert.ok(lines.every((l) => /^[A-Za-z0-9+/=]+$/.test(l)));
});

test("Base64LineDecoder：逐行解码，忽略 CRLF 与空行，行长不必固定", async () => {
  const parts = [Buffer.from("hello "), Buffer.from("世界"), Buffer.alloc(1000, 7)];
  const text = `${parts[0]!.toString("base64")}\r\n\r\n${parts[1]!.toString("base64")}\n${parts[2]!.toString("base64")}`;
  // 按单字节切包，模拟最坏的网络分片
  const src = Readable.from(Array.from(text).map((ch) => Buffer.from(ch, "latin1")));
  const out = await collect(src.pipe(new Base64LineDecoder()));
  assert.deepEqual(out, Buffer.concat(parts));
});

test("编码 → 解码 往返无损", async () => {
  const data = Buffer.from(Array.from({ length: 200_001 }, (_, i) => (i * 7919) & 0xff));
  const chunks: Buffer[] = [];
  await pipeline(
    Readable.from([data]),
    new Base64LineEncoder(),
    new Base64LineDecoder(),
    async function* (src) {
      for await (const c of src) chunks.push(c as Buffer);
    }
  );
  assert.deepEqual(Buffer.concat(chunks), data);
});

test("downloadCommand：posix 直接 cat，windows 分块 base64 行", () => {
  assert.equal(downloadCommand("posix", "/home/u/repo/a b'c.bin"), `cat '/home/u/repo/a b'\\''c.bin'`);
  const win = decodeWin(downloadCommand("windows", "C:\\repo\\it's.bin"));
  assert.match(win, /\$p = 'C:\\repo\\it''s\.bin'/);
  assert.match(win, /OpenRead\(\$p\)/);
  assert.match(win, /ToBase64String\(\$buf, 0, \$n\)/);
  assert.match(win, /WriteLine/);
});

test("contentDisposition：ASCII 兜底 + RFC 5987 的 UTF-8 形式", () => {
  assert.equal(
    contentDisposition("报告 (final)*.pdf"),
    `attachment; filename="__ (final)*.pdf"; filename*=UTF-8''%E6%8A%A5%E5%91%8A%20%28final%29%2A.pdf`
  );
  // 引号与反斜杠会破坏 quoted-string
  assert.equal(
    contentDisposition('a"b\\c.txt'),
    `attachment; filename="a_b_c.txt"; filename*=UTF-8''a%22b%5Cc.txt`
  );
});

test("validateUploadName 挡分隔符、控制字符与 Windows 保留字符", () => {
  validateUploadName("posix", "a:b?.txt");
  for (const bad of ["", ".", "..", "a/b", "a\\b", "a\nb", "x".repeat(256)]) {
    assert.throws(() => validateUploadName("posix", bad), /文件名/, JSON.stringify(bad));
  }
  assert.throws(() => validateUploadName("windows", "a:b.txt"), /Windows/);
});

test("uploadTarget：目标、上级目录与同目录临时文件", () => {
  const t = uploadTarget("posix", "/home/u/repo", "docs/img", "logo.png", "abcd");
  assert.deepEqual(t, {
    rel: "docs/img/logo.png",
    file: "/home/u/repo/docs/img/logo.png",
    parent: "/home/u/repo/docs/img",
    tmp: "/home/u/repo/docs/img/.logo.png.abcd.falcon-upload",
  });
  const root = uploadTarget("windows", "C:/code/repo", "", "a.txt", "ff");
  assert.equal(root.rel, "a.txt");
  assert.equal(root.file, "C:\\code\\repo\\a.txt");
  assert.equal(root.parent, "C:\\code\\repo");
  assert.equal(root.tmp, "C:\\code\\repo\\.a.txt.ff.falcon-upload");
  assert.throws(() => uploadTarget("posix", "/home/u/repo", "../x", "a.txt"), /路径不合法/);
});

test("uploadCommand：posix 先检查再 cat 到临时文件，核对字节数后 mv", () => {
  const t = uploadTarget("posix", "/r", "d", "a.txt", "t1");
  const cmd = uploadCommand("posix", t, 1234, false);
  assert.match(cmd, /f='\/r\/d\/a\.txt'; t='\/r\/d\/\.a\.txt\.t1\.falcon-upload'; p='\/r\/d'/);
  assert.match(cmd, /if \[ -e "\$f" \]; then printf '%s\\n' EEXIST; exit 1; fi/);
  assert.match(cmd, /find "\$p" -maxdepth 1 -type f -name '\.\*\.falcon-upload' -mmin \+360 -delete 2>\/dev\/null/);
  assert.match(cmd, /cat > "\$t" \|\| \{ rm -f "\$t"; exit 1; \}/);
  assert.match(cmd, /-ne 1234 \]; then rm -f "\$t"; printf '%s\\n' ESHORT; exit 1/);
  assert.match(cmd, /mv -f "\$t" "\$f"/);
  // 检查顺序：目标是目录 / 上级不存在先于 EEXIST，覆盖也救不了的先报
  assert.ok(cmd.indexOf("EISDIR") < cmd.indexOf("ENOENT"));
  assert.ok(cmd.indexOf("ENOENT") < cmd.indexOf("EEXIST"));
  // overwrite 时不再拦 EEXIST
  assert.doesNotMatch(uploadCommand("posix", t, 1234, true), /EEXIST/);
  assert.throws(() => uploadCommand("posix", t, -1, false), /字节数/);
});

test("uploadCommand：windows 逐行解 base64 写临时文件，核对长度后 Move-Item", () => {
  const t = uploadTarget("windows", "C:\\r", "d", "a.txt", "t1");
  const win = decodeWin(uploadCommand("windows", t, 99, false));
  assert.match(win, /\$ErrorActionPreference = 'Stop'/);
  assert.match(win, /\$t = 'C:\\r\\d\\\.a\.txt\.t1\.falcon-upload'/);
  assert.match(win, /Write-Output 'EEXIST'/);
  assert.match(win, /-Filter '\*\.falcon-upload' .*AddMinutes\(-360\)/);
  assert.match(win, /\[Console\]::In\.ReadLine\(\)/);
  assert.match(win, /FromBase64String\(\$line\)/);
  assert.match(win, /\.Length -ne 99\)/);
  assert.match(win, /Move-Item -LiteralPath \$t -Destination \$f -Force/);
  assert.doesNotMatch(decodeWin(uploadCommand("windows", t, 99, true)), /EEXIST/);
});
