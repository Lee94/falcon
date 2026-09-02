import { useEffect, useState } from "react";
import type { GrammarState, ThemedToken } from "shiki/core";

/**
 * 文件查看 / Markdown 代码块的语法高亮。
 *
 * 用 shiki 的 token API 而不是它的 HTML 输出：token 直接映射成 React 元素，
 * 文本永远是文本节点，不经过 dangerouslySetInnerHTML——与 Markdown.tsx
 * 拒绝消毒器的理由同源。逐行 token 也正好配 FileView 的虚拟滚动：
 * 窗口切到哪行取哪行的 token，互不牵扯。
 *
 * shiki 核心 + 主题 + 语言全部动态 import，主包一个字节都不多背，第一次
 * 打开代码文件才开始拉。正则引擎用纯 JS 版（省掉 oniguruma 的 ~600KB wasm）；
 * forgiving 模式下个别 grammar 里 JS RegExp 吃不下的规则会被静默跳过，
 * 表现为那一小段没颜色——比整个语言拒绝加载好。
 */

/** 渲染只认这两个字段：文本与一份 React style 对象 */
export interface HlToken {
  content: string;
  htmlStyle?: Record<string, string>;
}
export type HlLine = HlToken[];

/** shiki 的 FontStyle 位（@shikijs/vscode-textmate），只认前三个 */
const FONT_ITALIC = 1;
const FONT_BOLD = 2;
const FONT_UNDERLINE = 4;

/**
 * ThemedToken → React style。单主题模式下 shiki 只给 color / fontStyle，不给 htmlStyle
 * （那是多主题 + defaultColor:false 才有的），这里自己拼；color 是
 * `var(--shiki-token-xxx)`，原样透传给 style.color 就行。
 */
export function tokenStyle(t: Pick<ThemedToken, "color" | "bgColor" | "fontStyle">): Record<string, string> | undefined {
  const out: Record<string, string> = {};
  if (t.color) out.color = t.color;
  if (t.bgColor) out.backgroundColor = t.bgColor;
  const fs = t.fontStyle ?? 0;
  if (fs > 0) {
    if (fs & FONT_ITALIC) out.fontStyle = "italic";
    if (fs & FONT_BOLD) out.fontWeight = "bold";
    if (fs & FONT_UNDERLINE) out.textDecoration = "underline";
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

/**
 * 超过这个字符数不高亮（0.8ms/行的 tokenize，512KB ≈ 1.5 万行 ≈ 十几秒
 * 渐进跑完；再大的多半是生成物或日志，上色的收益撑不起这个开销）
 */
export const HIGHLIGHT_CAP = 512_000;

/** 每块行数。实测 JS 引擎约 0.8ms/行，64 行 ≈ 50ms，主线程不掉帧 */
const CHUNK_LINES = 64;

/** 超长行（minified、base64 内联……）整行按纯文本放行，不喂正则引擎 */
const MAX_TOKEN_LINE = 2000;

/**
 * 只装一个"主题"：shiki 的 css-variables 主题把每种 token 的颜色写成
 * `var(--shiki-token-xxx)`，变量值由当前应用主题派生（lib/theme/derive.ts）写在
 * <html> 上。于是高亮色跟着终端调色板走，换主题不用重新 tokenize，也不用再
 * 为明暗各装一套 github 主题。
 */
const CSS_THEME = "falcon-css-variables";

/**
 * 行切分的唯一定义。高亮结果按行下标对齐回原文，CodePane / CodeBlock
 * 与 useHighlight 内部必须用同一个切法，否则整屏颜色错位一行。
 * 末尾换行吞掉：POSIX 文本以 \n 结尾，split 会多出一个幽灵空行。
 */
export function splitCodeLines(text: string): string[] {
  return text.replace(/\n$/, "").split("\n");
}

// ---------------- 语言识别（纯函数，可测） ----------------

/**
 * 支持的语言 → import thunk。显式列举而不是模板字符串动态拼路径：
 * vite 对 import(`shiki/langs/${x}.mjs`) 会把全部 361 个语言各打一个 chunk。
 * key 是 shiki 的规范语言 id，别名归一在 ALIASES 里做。
 */
const LANGS: Record<string, () => Promise<{ default: unknown }>> = {
  typescript: () => import("shiki/langs/typescript.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  jsx: () => import("shiki/langs/jsx.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  jsonc: () => import("shiki/langs/jsonc.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  scss: () => import("shiki/langs/scss.mjs"),
  less: () => import("shiki/langs/less.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  shellscript: () => import("shiki/langs/shellscript.mjs"),
  powershell: () => import("shiki/langs/powershell.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  c: () => import("shiki/langs/c.mjs"),
  cpp: () => import("shiki/langs/cpp.mjs"),
  csharp: () => import("shiki/langs/csharp.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  vue: () => import("shiki/langs/vue.mjs"),
  svelte: () => import("shiki/langs/svelte.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  ruby: () => import("shiki/langs/ruby.mjs"),
  php: () => import("shiki/langs/php.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  swift: () => import("shiki/langs/swift.mjs"),
  lua: () => import("shiki/langs/lua.mjs"),
  graphql: () => import("shiki/langs/graphql.mjs"),
  dockerfile: () => import("shiki/langs/dockerfile.mjs"),
  make: () => import("shiki/langs/make.mjs"),
  dotenv: () => import("shiki/langs/dotenv.mjs"),
  // zellij 的配置格式，这个项目的用户大概率会打开 .kdl
  kdl: () => import("shiki/langs/kdl.mjs"),
};

/** 扩展名 / fence 别名 → 规范 id。两个入口共用：`ts` 既是扩展名也是常见 fence 写法 */
const ALIASES: Record<string, string> = {
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json5: "jsonc",
  htm: "html",
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  yml: "yaml",
  py: "python",
  pyi: "python",
  rs: "rust",
  sh: "shellscript",
  bash: "shellscript",
  zsh: "shellscript",
  shell: "shellscript",
  ps1: "powershell",
  psm1: "powershell",
  pwsh: "powershell",
  h: "c",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hh: "cpp",
  hxx: "cpp",
  "c++": "cpp",
  cs: "csharp",
  svg: "xml",
  plist: "xml",
  cfg: "ini",
  conf: "ini",
  patch: "diff",
  rb: "ruby",
  kt: "kotlin",
  kts: "kotlin",
  gql: "graphql",
  env: "dotenv",
};

/** 没有扩展名但一看名字就知道是什么的文件（比对时统一小写） */
const FILENAMES: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "make",
  gnumakefile: "make",
  ".env": "dotenv",
  ".bashrc": "shellscript",
  ".bash_profile": "shellscript",
  ".zshrc": "shellscript",
  ".zprofile": "shellscript",
  ".zshenv": "shellscript",
};

function resolveLang(word: string): string | null {
  const w = word.toLowerCase();
  const id = ALIASES[w] ?? w;
  return LANGS[id] ? id : null;
}

/** 文件路径 → 语言 id；认不出返回 null（保持纯文本，不瞎猜） */
export function langForPath(path: string): string | null {
  const base = path.split(/[/\\]/).pop()?.toLowerCase() ?? "";
  if (FILENAMES[base]) return FILENAMES[base];
  // .env.local / .env.production 这类带环境后缀的变体
  if (base.startsWith(".env.")) return "dotenv";
  const dot = base.lastIndexOf(".");
  // dot <= 0 同时排除无扩展名与 .gitignore 这类纯点开头（上面文件名表没接住的）
  if (dot <= 0 || dot === base.length - 1) return null;
  return resolveLang(base.slice(dot + 1));
}

/** Markdown fence 的 info string → 语言 id。marked 给的是整串（如 "ts title=x"），只认第一个词 */
export function langForFence(info: string | undefined): string | null {
  const word = info?.trim().split(/\s+/, 1)[0];
  return word ? resolveLang(word) : null;
}

// ---------------- shiki 加载与 tokenize ----------------

type Core = Awaited<ReturnType<typeof import("shiki/core").createHighlighterCore>>;

let corePromise: Promise<Core> | null = null;

function loadCore(): Promise<Core> {
  corePromise ??= (async () => {
    const [{ createHighlighterCore, createCssVariablesTheme }, { createJavaScriptRegexEngine }] =
      await Promise.all([import("shiki/core"), import("shiki/engine/javascript")]);
    return createHighlighterCore({
      themes: [createCssVariablesTheme({ name: CSS_THEME, variablePrefix: "--shiki-", fontStyle: true })],
      langs: [],
      engine: createJavaScriptRegexEngine({ forgiving: true }),
    });
  })();
  return corePromise;
}

const langReady = new Map<string, Promise<boolean>>();

function ensureLang(lang: string): Promise<boolean> {
  let ready = langReady.get(lang);
  if (!ready) {
    ready = (async () => {
      try {
        const hl = await loadCore();
        await hl.loadLanguage(LANGS[lang]() as Parameters<Core["loadLanguage"]>[0]);
        return true;
      } catch (err) {
        // 多半是 chunk 没拉下来（离线 / 部署换版本），下次打开同语言允许重试
        console.warn(`[highlight] 语言 ${lang} 加载失败`, err);
        langReady.delete(lang);
        return false;
      }
    })();
    langReady.set(lang, ready);
  }
  return ready;
}

/**
 * 高亮一段代码，返回与 splitCodeLines(text) 逐行对齐的 token 数组；
 * 没结果（语言不识别 / 文件太大 / shiki 还在路上）时返回 null，调用方照旧渲染纯文本。
 *
 * 分块 tokenize + grammarState 续接：一次喂整个文件会把主线程按住几百毫秒到几秒，
 * 分块间让出事件循环，结果逐块长出来——大文件从上往下渐进上色。
 */
export function useHighlight(text: string, lang: string | null): HlLine[] | null {
  const [lines, setLines] = useState<HlLine[] | null>(null);

  useEffect(() => {
    setLines(null);
    if (!lang || !text || text.length > HIGHLIGHT_CAP) return;
    let cancelled = false;
    (async () => {
      if (!(await ensureLang(lang)) || cancelled) return;
      const hl = await loadCore();
      const src = splitCodeLines(text);
      const out: HlLine[] = [];
      let state: GrammarState | undefined;
      for (let i = 0; i < src.length; i += CHUNK_LINES) {
        const res = hl.codeToTokens(src.slice(i, i + CHUNK_LINES).join("\n"), {
          lang,
          theme: CSS_THEME,
          grammarState: state,
          tokenizeMaxLineLength: MAX_TOKEN_LINE,
        });
        // 块与块之间必须续接语法状态：块尾正好切在模板字符串 / 块注释里时，
        // 下一块从干净状态起步会整段上错色
        state = res.grammarState;
        for (const line of res.tokens) {
          out.push(line.map((tok) => ({ content: tok.content, htmlStyle: tokenStyle(tok) })));
        }
        if (cancelled) return;
        setLines(out.slice());
        if (i + CHUNK_LINES < src.length) await new Promise((r) => setTimeout(r));
      }
    })().catch((err) => {
      console.warn("[highlight] tokenize 失败", err);
    });
    return () => {
      cancelled = true;
    };
  }, [text, lang]);

  return lines;
}
