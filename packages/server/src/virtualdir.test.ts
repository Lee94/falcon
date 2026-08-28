import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  CLAUDE_MD_BODY,
  FALCON_GENERATED_MARK,
  centralManifestFiles,
  centralManifestSweepCommand,
  containerManifestFiles,
  posixRemoveVirtualDirCommand,
  posixWriteManifestCommand,
  renderCentralAgentsMd,
  renderClaudeSettings,
  renderContainerAgentsMd,
  virtualProjectDir,
  windowsRemoveVirtualDirCommand,
  windowsWriteManifestCommands,
} from "./virtualdir.js";

/** -EncodedCommand 的 UTF-16LE base64 解回脚本原文（paste.test.ts 同款） */
function decode(cmd: string): string {
  const b64 = cmd.split(" ").at(-1)!;
  return Buffer.from(b64, "base64").toString("utf16le");
}

describe("virtualProjectDir", () => {
  it("lives under <falcon root>/projects/<id> on either platform", () => {
    assert.equal(
      virtualProjectDir("posix", "/home/u/.falcon", "p-1"),
      "/home/u/.falcon/projects/p-1"
    );
    assert.equal(
      virtualProjectDir("windows", "C:\\Users\\u\\.falcon", "p-1"),
      "C:\\Users\\u\\.falcon\\projects\\p-1"
    );
  });

  it("does not double separators when the root has a trailing one", () => {
    assert.equal(virtualProjectDir("posix", "/data/", "id"), "/data/projects/id");
  });
});

describe("renderContainerAgentsMd", () => {
  const members = [
    "/Users/fay/code with space/srv",
    "/home/o'brien/web",
    "/Users/fay/仓库/中文",
    "D:\\code\\repo",
  ];
  const md = renderContainerAgentsMd("我的项目", members);

  it("starts with the generated mark so the guarded sweep can recognize it", () => {
    assert.ok(md.split("\n")[0].includes(FALCON_GENERATED_MARK));
  });

  it("lists every member path verbatim and names the project", () => {
    for (const m of members) assert.ok(md.includes(`- ${m}`));
    assert.ok(md.includes("# 我的项目"));
  });

  it("tells agents there is no code in cwd and to search per member root", () => {
    assert.ok(md.includes("当前目录下没有代码"));
    assert.ok(md.includes("以每个成员目录为根"));
  });
});

describe("renderCentralAgentsMd", () => {
  const md = renderCentralAgentsMd("派生-feat", "feat/x", [
    { base: "srv", repoDir: "/Users/fay/code/srv" },
    { base: "web", repoDir: "/Users/fay/code/web" },
  ]);

  it("lists members as relative subdirs with their source repo roots", () => {
    assert.ok(md.includes("- ./srv/（源仓库 /Users/fay/code/srv）"));
    assert.ok(md.includes("- ./web/（源仓库 /Users/fay/code/web）"));
  });

  it("names the branch and carries the generated mark", () => {
    assert.ok(md.includes("feat/x"));
    assert.ok(md.split("\n")[0].includes(FALCON_GENERATED_MARK));
  });
});

describe("CLAUDE_MD_BODY", () => {
  it("is exactly the one-line import — the guarded sweep compares it literally", () => {
    assert.equal(CLAUDE_MD_BODY, "@AGENTS.md\n");
  });
});

describe("renderClaudeSettings", () => {
  it("round-trips through JSON.parse, backslashes intact", () => {
    const dirs = ["/a/b", "D:\\code\\仓库", "/home/o'brien/x"];
    const text = renderClaudeSettings(dirs);
    assert.deepEqual(JSON.parse(text), { permissions: { additionalDirectories: dirs } });
  });

  it("has no BOM and ends with a newline", () => {
    const text = renderClaudeSettings(["/a"]);
    assert.ok(text.startsWith("{"));
    assert.ok(text.endsWith("}\n"));
  });
});

describe("containerManifestFiles / centralManifestFiles", () => {
  it("container gets AGENTS.md + CLAUDE.md + .claude/settings.json", () => {
    const files = containerManifestFiles("p", ["/a"]);
    assert.deepEqual(
      files.map((f) => f.rel),
      [["AGENTS.md"], ["CLAUDE.md"], [".claude", "settings.json"]]
    );
  });

  it("central dir gets no settings.json — members are inside cwd already", () => {
    const files = centralManifestFiles("p", "b", [{ base: "x", repoDir: "/r/x" }]);
    assert.deepEqual(
      files.map((f) => f.rel),
      [["AGENTS.md"], ["CLAUDE.md"]]
    );
  });
});

describe("posixWriteManifestCommand", () => {
  const files = containerManifestFiles("p", ["/a"]);

  it("creates the dir and .claude, then writes each file via printf", () => {
    const cmd = posixWriteManifestCommand("/home/u/.falcon/projects/id", files);
    assert.ok(cmd.includes(`mkdir -p "$d" "$d"/.claude && `));
    assert.ok(cmd.includes(`> "$d"/AGENTS.md`));
    assert.ok(cmd.includes(`> "$d"/CLAUDE.md`));
    assert.ok(cmd.includes(`> "$d"/.claude/settings.json`));
  });

  it("quotes dir and content so quotes/newlines cannot split the command", () => {
    const cmd = posixWriteManifestCommand("/home/o'brien/.falcon/projects/id", files);
    assert.ok(cmd.startsWith(`d='/home/o'\\''brien/.falcon/projects/id'; `));
  });

  it("never deletes anything", () => {
    const cmd = posixWriteManifestCommand("/x", files);
    assert.ok(!cmd.includes("rm "));
    assert.ok(!cmd.includes("rmdir"));
  });
});

describe("windowsWriteManifestCommands", () => {
  const files = containerManifestFiles("项目", ["D:\\code\\仓库"]);
  const cmds = windowsWriteManifestCommands("C:\\Users\\u\\.falcon\\projects\\id", files);

  it("emits one -EncodedCommand per file", () => {
    assert.equal(cmds.length, files.length);
    for (const { cmd } of cmds) {
      assert.ok(cmd.startsWith("powershell -NoProfile -NonInteractive -EncodedCommand "));
    }
  });

  it("mkdirs the parent then WriteAllBytes from stdin base64", () => {
    const script = decode(cmds[2].cmd);
    assert.match(script, /New-Item -ItemType Directory -Force -Path /);
    assert.match(script, /\[Convert\]::FromBase64String\(\[Console\]::In\.ReadToEnd\(\)\)/);
    assert.ok(
      script.includes(
        `[IO.File]::WriteAllBytes('C:\\Users\\u\\.falcon\\projects\\id\\.claude\\settings.json', $b)`
      )
    );
  });

  it("stdin carries the exact UTF-8 bytes — CJK survives the trip", () => {
    for (const [i, { stdinBase64 }] of cmds.entries()) {
      assert.equal(Buffer.from(stdinBase64, "base64").toString("utf8"), files[i].content);
    }
  });
});

describe("posixRemoveVirtualDirCommand", () => {
  const cmd = posixRemoveVirtualDirCommand("/home/u/.falcon/projects/id");

  it("removes only the three fixed files, then rmdirs non-recursively", () => {
    assert.ok(cmd.includes(`rm -f -- "$d"/AGENTS.md "$d"/CLAUDE.md "$d"/.claude/settings.json; `));
    assert.ok(cmd.includes(`rmdir -- "$d"/.claude`));
    assert.ok(!cmd.includes("rm -r"));
  });

  it("reports gone|left on stdout, and gone when the dir never existed", () => {
    assert.ok(cmd.includes(`if [ ! -e "$d" ]; then printf gone; `));
    assert.ok(cmd.includes(`then printf gone; else printf left; fi`));
  });
});

describe("windowsRemoveVirtualDirCommand", () => {
  const script = decode(windowsRemoveVirtualDirCommand("C:\\u\\.falcon\\projects\\a[1]b"));

  it("uses literal-path IO.File/IO.Directory deletes, never Remove-Item or -Recurse", () => {
    assert.match(script, /\[IO\.File\]::Delete\(\(Join-Path \$d 'AGENTS\.md'\)\)/);
    assert.match(script, /\[IO\.File\]::Delete\(\(Join-Path \$d 'CLAUDE\.md'\)\)/);
    assert.match(script, /\[IO\.File\]::Delete\(\(Join-Path \$d '\.claude\\settings\.json'\)\)/);
    assert.match(script, /\[IO\.Directory\]::Delete\(\(Join-Path \$d '\.claude'\), \$false\)/);
    assert.match(script, /\[IO\.Directory\]::Delete\(\$d, \$false\); 'gone'/);
    assert.ok(!script.includes("Remove-Item"));
    assert.ok(!script.includes("-Recurse"));
  });

  it("short-circuits to gone when the dir does not exist", () => {
    assert.match(script, /if \(-not \(Test-Path -LiteralPath \$d\)\) \{ 'gone'; exit 0 \}/);
  });
});

describe("centralManifestSweepCommand", () => {
  it("posix: deletes AGENTS.md only when marked, CLAUDE.md only on exact match", () => {
    const cmd = centralManifestSweepCommand("posix", "/repos/multi-feat");
    assert.ok(cmd.includes(`head -n 1 "$d"/AGENTS.md | grep -qF 'generated by falcon'`));
    assert.ok(cmd.includes(`[ "$(cat "$d"/CLAUDE.md)" = '@AGENTS.md' ]`));
    assert.ok(!cmd.includes("rm -r"));
    // 只碰这两个 basename
    assert.equal((cmd.match(/rm -f/g) ?? []).length, 2);
  });

  it("windows: same guards via Get-Content, deletes via IO.File only", () => {
    const script = decode(centralManifestSweepCommand("windows", "C:\\repos\\multi-feat"));
    assert.match(script, /Get-Content -LiteralPath \$a -First 1\) -like '\*generated by falcon\*'/);
    assert.match(script, /Get-Content -LiteralPath \$c -Raw\)\.Trim\(\) -eq '@AGENTS\.md'/);
    assert.ok(!script.includes("Remove-Item"));
    assert.ok(!script.includes("-Recurse"));
  });

  it("quotes a central dir containing single quotes", () => {
    const cmd = centralManifestSweepCommand("posix", "/home/o'brien/multi-x");
    assert.ok(cmd.startsWith(`d='/home/o'\\''brien/multi-x'; `));
  });
});
