import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  detectShells,
  isShellCommand,
  mergeShells,
  parseShellList,
  POSIX_SHELLS_PROBE,
  WINDOWS_SHELLS_PROBE,
} from "./shells.js";

const PREFIX = "powershell -NoProfile -NonInteractive -EncodedCommand ";

describe("shells probe", () => {
  it("POSIX 探测不用 for：分号串接在 sh/bash/zsh/fish 里语义一致", () => {
    assert.ok(!POSIX_SHELLS_PROBE.includes("for "));
    assert.match(POSIX_SHELLS_PROBE, /command -v bash; command -v zsh/);
    // 缺 shell 时最后一条 command -v 失败，true 兜住整体退出码
    assert.ok(POSIX_SHELLS_PROBE.endsWith("; true"));
  });

  it("Windows 探测走 EncodedCommand，不受远端 DefaultShell 影响", () => {
    assert.ok(WINDOWS_SHELLS_PROBE.startsWith(PREFIX));
    const script = Buffer.from(
      WINDOWS_SHELLS_PROBE.slice(PREFIX.length),
      "base64"
    ).toString("utf16le");
    assert.match(script, /Get-Command \$n -CommandType Application/);
    // powershell 排最前 = Windows 默认
    assert.match(script, /'powershell\.exe','pwsh\.exe'/);
  });
});

describe("parseShellList", () => {
  it("POSIX 只认绝对路径行（command -v 对别名可能吐定义体）", () => {
    const out = "/bin/bash\n/usr/bin/zsh\nalias fish='fish -l'\n\n/bin/sh\n";
    assert.deepEqual(parseShellList("posix", out), [
      "/bin/bash",
      "/usr/bin/zsh",
      "/bin/sh",
    ]);
  });

  it("Windows 只认盘符 / UNC 路径行", () => {
    const out =
      "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\r\n" +
      "C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\n" +
      "warning: something\r\n";
    assert.deepEqual(parseShellList("windows", out), [
      "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
      "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
    ]);
  });
});

describe("mergeShells", () => {
  it("POSIX：默认 shell 排最前并去重", () => {
    const info = mergeShells("posix", "/usr/bin/zsh", [
      "/bin/bash",
      "/usr/bin/zsh",
      "/bin/sh",
    ]);
    assert.equal(info.default, "/usr/bin/zsh");
    assert.deepEqual(info.shells, ["/usr/bin/zsh", "/bin/bash", "/bin/sh"]);
  });

  it("Windows：裸名字默认（powershell.exe）按 basename 归到侦测出的绝对路径", () => {
    const ps = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
    const info = mergeShells("windows", "powershell.exe", [ps, "C:\\Windows\\System32\\cmd.exe"]);
    assert.equal(info.default, ps);
    assert.deepEqual(info.shells, [ps, "C:\\Windows\\System32\\cmd.exe"]);
  });

  it("Windows：去重不区分大小写", () => {
    const info = mergeShells("windows", "C:\\WINDOWS\\system32\\cmd.exe", [
      "C:\\Windows\\System32\\cmd.exe",
    ]);
    assert.equal(info.shells.length, 1);
  });

  it("侦测结果为空时仍有默认项", () => {
    const info = mergeShells("posix", "/bin/sh", []);
    assert.deepEqual(info.shells, ["/bin/sh"]);
  });
});

describe("isShellCommand", () => {
  it("裸 shell 名与绝对路径都算空闲", () => {
    assert.ok(isShellCommand("zsh"));
    assert.ok(isShellCommand("/bin/bash"));
    assert.ok(isShellCommand("fish"));
  });

  it("登录 shell 的 - 前缀与 Windows 的 .exe 后缀要归一化", () => {
    assert.ok(isShellCommand("-zsh"));
    assert.ok(isShellCommand("powershell.exe"));
    assert.ok(isShellCommand("C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
  });

  it("前台真在跑东西时不算空闲", () => {
    assert.ok(!isShellCommand("vim main.rs"));
    assert.ok(!isShellCommand("sleep 300"));
    assert.ok(!isShellCommand("claude"));
  });

  it("shell 带参数是在跑脚本，不是在等输入", () => {
    assert.ok(!isShellCommand("bash deploy.sh"));
    assert.ok(!isShellCommand("zsh -c 'make'"));
  });

  it("只跟登录 / 交互标志仍是 shell 在等输入（agent 会话 exec 出来的样子）", () => {
    assert.ok(isShellCommand("/bin/zsh -l"));
    assert.ok(isShellCommand("bash --login"));
    assert.ok(isShellCommand("/bin/zsh -il"));
  });

  it("会话配置的非常见 shell 也算空闲", () => {
    assert.ok(!isShellCommand("myshell"));
    assert.ok(isShellCommand("myshell", "/opt/bin/myshell"));
  });

  it("解析不出名字按空闲放行——侦测是保险，不该拦住关 tab", () => {
    assert.ok(isShellCommand(""));
    assert.ok(isShellCommand("   "));
  });
});

describe("detectShells", () => {
  it("探测命令失败不抛，退回只有默认项", async () => {
    const info = await detectShells(
      () => Promise.reject(new Error("link down")),
      "posix",
      "/usr/bin/zsh"
    );
    assert.deepEqual(info, { kind: "posix", default: "/usr/bin/zsh", shells: ["/usr/bin/zsh"] });
  });

  it("正常路径：解析 stdout、无视退出码", async () => {
    const info = await detectShells(
      () => Promise.resolve({ code: 1, stdout: "/bin/bash\n/usr/bin/zsh\n", stderr: "" }),
      "posix",
      "/usr/bin/zsh"
    );
    assert.deepEqual(info.shells, ["/usr/bin/zsh", "/bin/bash"]);
  });
});
