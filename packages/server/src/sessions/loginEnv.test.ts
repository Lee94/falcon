import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { ExecFn, ExecResult } from "../zellij/install.js";
import {
  appleLocaleToPosix,
  detectFallbackLang,
  LOGIN_ENV_BEGIN,
  LOGIN_ENV_END,
  loginEnvProbeArgs,
  mergeBaseEnv,
  parseLoginEnv,
  pickUtf8Locale,
} from "./loginEnv.js";

describe("loginEnvProbeArgs", () => {
  it("分开传 -i -l -c，不合并短参（fish 等的支持不一）", () => {
    const args = loginEnvProbeArgs();
    assert.deepEqual(args.slice(0, 3), ["-i", "-l", "-c"]);
    assert.equal(args.length, 4);
  });

  it("脚本带前后标记与 env -0 回退，且没有 csh 会炸的 2> 重定向", () => {
    const script = loginEnvProbeArgs()[3]!;
    assert.ok(script.includes(LOGIN_ENV_BEGIN));
    assert.ok(script.includes(LOGIN_ENV_END));
    assert.ok(script.includes("/usr/bin/env -0 || /usr/bin/env"));
    assert.ok(!script.includes("2>"));
  });
});

describe("parseLoginEnv", () => {
  it("NUL 模式：取标记之间的部分，rc 与 zlogout 的污染都被丢掉", () => {
    const stdout =
      "Last login: today\nnvm is slow...\n" +
      `${LOGIN_ENV_BEGIN}\n` +
      "PATH=/opt/homebrew/bin:/usr/bin\0HOME=/Users/fay\0" +
      `${LOGIN_ENV_END}\n` +
      "goodbye from zlogout\n";
    assert.deepEqual(parseLoginEnv(stdout), {
      PATH: "/opt/homebrew/bin:/usr/bin",
      HOME: "/Users/fay",
    });
  });

  it("NUL 模式：值里的换行与等号原样保留", () => {
    const stdout =
      `${LOGIN_ENV_BEGIN}\n` +
      "MULTI=line1\nline2\0EQ=a=b=c\0" +
      `${LOGIN_ENV_END}\n`;
    assert.deepEqual(parseLoginEnv(stdout), {
      MULTI: "line1\nline2",
      EQ: "a=b=c",
    });
  });

  it("按行回退（env 不认 -0）：非 KEY= 开头的行并入上一个值", () => {
    const stdout =
      `${LOGIN_ENV_BEGIN}\n` +
      "PATH=/usr/bin\nMULTI=line1\nline2\nLANG=en_US.UTF-8\n" +
      `${LOGIN_ENV_END}\n`;
    assert.deepEqual(parseLoginEnv(stdout), {
      PATH: "/usr/bin",
      MULTI: "line1\nline2",
      LANG: "en_US.UTF-8",
    });
  });

  it("标记缺失或解不出变量时返回 null，让调用方退回 process.env", () => {
    assert.equal(parseLoginEnv("no markers at all"), null);
    assert.equal(parseLoginEnv(`${LOGIN_ENV_BEGIN}\n`), null);
    assert.equal(parseLoginEnv(`${LOGIN_ENV_BEGIN}\n\n${LOGIN_ENV_END}\n`), null);
  });
});

describe("mergeBaseEnv", () => {
  it("login env 覆盖 process.env（PATH 以登录环境为准），process.env 独有的键保留", () => {
    const merged = mergeBaseEnv(
      { PATH: "/usr/bin", FOO: "bar" },
      { PATH: "/opt/homebrew/bin:/usr/bin", EDITOR: "vim" },
      null
    );
    assert.equal(merged.PATH, "/opt/homebrew/bin:/usr/bin");
    assert.equal(merged.FOO, "bar");
    assert.equal(merged.EDITOR, "vim");
  });

  it("探测 shell 的运行痕迹（SHLVL/PWD/…）与解析标记不进基底", () => {
    const merged = mergeBaseEnv(
      { PWD: "/srv/falcon" },
      {
        SHLVL: "2",
        PWD: "/Users/fay",
        OLDPWD: "/",
        _: "/usr/bin/env",
        FALCON_RESOLVING_ENV: "1",
        PATH: "/usr/bin",
      },
      null
    );
    assert.equal(merged.SHLVL, undefined);
    assert.equal(merged.PWD, "/srv/falcon");
    assert.equal(merged.OLDPWD, undefined);
    assert.equal(merged._, undefined);
    assert.equal(merged.FALCON_RESOLVING_ENV, undefined);
  });

  it("locale 兜底只在 LANG/LC_ALL/LC_CTYPE 全缺时注入", () => {
    assert.equal(mergeBaseEnv({}, null, "zh_CN.UTF-8").LANG, "zh_CN.UTF-8");
    // 显式 LANG=C 是用户的选择，不覆盖
    assert.equal(mergeBaseEnv({ LANG: "C" }, null, "zh_CN.UTF-8").LANG, "C");
    assert.equal(
      mergeBaseEnv({ LC_CTYPE: "UTF-8" }, null, "zh_CN.UTF-8").LANG,
      undefined
    );
    // login env 带来的 locale 同样挡住兜底
    assert.equal(
      mergeBaseEnv({}, { LANG: "ja_JP.UTF-8" }, "zh_CN.UTF-8").LANG,
      "ja_JP.UTF-8"
    );
    assert.equal(mergeBaseEnv({}, null, null).LANG, undefined);
  });
});

describe("appleLocaleToPosix", () => {
  it("裸 语言_地区 与带脚本段 / 区域修饰的形态都归一到 语言_地区", () => {
    assert.equal(appleLocaleToPosix("zh_CN"), "zh_CN");
    assert.equal(appleLocaleToPosix("en_US"), "en_US");
    assert.equal(appleLocaleToPosix("zh-Hans_CN"), "zh_CN");
    assert.equal(appleLocaleToPosix("zh_CN@rg=uszzzz"), "zh_CN");
    assert.equal(appleLocaleToPosix("yue-Hant_HK"), "yue_HK");
  });

  it("解析不出就 null（AppleLocale 也可能只有语言段）", () => {
    assert.equal(appleLocaleToPosix("zh"), null);
    assert.equal(appleLocaleToPosix(""), null);
    assert.equal(appleLocaleToPosix("garbage"), null);
  });
});

describe("pickUtf8Locale", () => {
  it("UTF-8 与 utf8 两种拼写互认，返回 locale -a 的原始拼写", () => {
    // macOS 的列表拼写
    assert.equal(
      pickUtf8Locale(["zh_CN.UTF-8"], "en_US.UTF-8\nzh_CN.UTF-8\nzh_CN.GB18030\n"),
      "zh_CN.UTF-8"
    );
    // glibc 的列表拼写：候选写 UTF-8 也要命中，且返回列表原文
    assert.equal(
      pickUtf8Locale(["en_US.UTF-8"], "C\nC.utf8\nen_US.utf8\n"),
      "en_US.utf8"
    );
  });

  it("按候选顺序取第一个可用项，null 候选跳过，全不可用返回 null", () => {
    assert.equal(
      pickUtf8Locale([null, "xx_XX.UTF-8", "en_US.UTF-8"], "en_US.UTF-8\n"),
      "en_US.UTF-8"
    );
    assert.equal(pickUtf8Locale(["zh_CN.UTF-8"], ""), null);
  });
});

describe("detectFallbackLang", () => {
  const fake = (table: Record<string, ExecResult>): ExecFn => {
    return (commandLine) =>
      Promise.resolve(
        table[commandLine as string] ?? { code: 1, stdout: "", stderr: "" }
      );
  };

  it("darwin：AppleLocale 推导优先，locale -a 验证通过才用", async () => {
    const exec = fake({
      "defaults read -g AppleLocale": { code: 0, stdout: "zh_CN\n", stderr: "" },
      "locale -a": { code: 0, stdout: "en_US.UTF-8\nzh_CN.UTF-8\n", stderr: "" },
    });
    assert.equal(await detectFallbackLang(exec, "darwin"), "zh_CN.UTF-8");
  });

  it("darwin：区域 locale 不在列表里时退 en_US.UTF-8", async () => {
    const exec = fake({
      "defaults read -g AppleLocale": { code: 0, stdout: "xx_XX\n", stderr: "" },
      "locale -a": { code: 0, stdout: "en_US.UTF-8\n", stderr: "" },
    });
    assert.equal(await detectFallbackLang(exec, "darwin"), "en_US.UTF-8");
  });

  it("linux：首选语言中性的 C.UTF-8", async () => {
    const exec = fake({
      "locale -a": { code: 0, stdout: "C\nC.utf8\nPOSIX\nen_US.utf8\n", stderr: "" },
    });
    assert.equal(await detectFallbackLang(exec, "linux"), "C.utf8");
  });

  it("locale -a 失败时按平台给硬值，绝不返回 null", async () => {
    const exec = fake({});
    assert.equal(await detectFallbackLang(exec, "darwin"), "en_US.UTF-8");
    assert.equal(await detectFallbackLang(exec, "linux"), "C.UTF-8");
  });
});
