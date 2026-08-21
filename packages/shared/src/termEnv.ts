/**
 * 终端深浅线索：写进 PTY 环境，并用来应答 OSC 10/11/12 查询。
 *
 * 主题画在浏览器的 xterm.js 上，进程只看得到 PTY。grok / vim / less 靠
 * COLORFGBG、GROK_APPEARANCE，或启动时问一声 OSC 11。这边给出统一的值。
 */

export type TermAppearance = "light" | "dark";

export function isTermAppearance(v: unknown): v is TermAppearance {
  return v === "light" || v === "dark";
}

/** 合法的 #rgb / #rrggbb（忽略 alpha）。 */
const HEX = /^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i;

export function parseHexRgb(hex: string): { r: number; g: number; b: number } | undefined {
  const m = HEX.exec(hex.trim());
  if (!m) return undefined;
  let h = m[1]!;
  if (h.length === 3) {
    h = `${h[0]}${h[0]}${h[1]}${h[1]}${h[2]}${h[2]}`;
  }
  return {
    r: parseInt(h.slice(0, 2), 16),
    g: parseInt(h.slice(2, 4), 16),
    b: parseInt(h.slice(4, 6), 16),
  };
}

/** 相对亮度 0–1（sRGB / WCAG）。解析失败当纯黑。 */
export function hexLuminance(hex: string): number {
  const n = parseHexRgb(hex);
  if (!n) return 0;
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(n.r) + 0.7152 * lin(n.g) + 0.0722 * lin(n.b);
}

export function appearanceFromHex(hex: string | undefined): TermAppearance {
  return hexLuminance(hex ?? "#000000") > 0.5 ? "light" : "dark";
}

/** 跟随界面时的默认底/字，跟 web MATCH_THEMES 对齐。 */
const FALLBACK: Record<TermAppearance, { bg: string; fg: string }> = {
  dark: { bg: "#0a0a0a", fg: "#fafafa" },
  light: { bg: "#ffffff", fg: "#171717" },
};

/**
 * 写入 PTY 的环境。
 *
 * TERM_PROGRAM=mojito 是实话，不冒充 vscode——grok 对 vscode 的键盘特例
 * 不该被我们误触发。COLORTERM 与 TERM 分开：TERM 仍是 xterm-256color
 * （terminfo 最稳的名字），truecolor 靠 COLORTERM 声明。
 *
 * appearance 缺省时不写 COLORFGBG / GROK_*：调用方必须先清掉宿主进程
 * 带来的同名变量，否则会继承「启动 mojito 的那个 iTerm」的深浅。
 */
export function termPtyEnv(appearance?: TermAppearance): Record<string, string> {
  const env: Record<string, string> = {
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
    TERM_PROGRAM: "mojito",
  };
  if (appearance === "light") {
    env.COLORFGBG = "0;15";
    env.GROK_APPEARANCE = "light";
    env.LC_GROK_APPEARANCE = "light";
  } else if (appearance === "dark") {
    env.COLORFGBG = "15;0";
    env.GROK_APPEARANCE = "dark";
    env.LC_GROK_APPEARANCE = "dark";
  }
  return env;
}

export const TERM_APPEARANCE_KEYS = [
  "COLORFGBG",
  "GROK_APPEARANCE",
  "LC_GROK_APPEARANCE",
] as const;

/**
 * 叠到已有 env 上：先丢掉宿主的深浅线索，再写入我们的。
 * base 里的 undefined 会被扔掉，node-pty 不接受。
 */
export function applyTermPtyEnv(
  base: Record<string, string | undefined>,
  appearance?: TermAppearance
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(base)) {
    if (v == null) continue;
    env[k] = v;
  }
  for (const k of TERM_APPEARANCE_KEYS) delete env[k];
  Object.assign(env, termPtyEnv(appearance));
  return env;
}

/** #rrggbb → xterm `rgb:rrrr/gggg/bbbb`（每通道 16 bit，重复字节）。 */
export function hexToOscRgb(hex: string): string | undefined {
  const n = parseHexRgb(hex);
  if (!n) return undefined;
  const c = (v: number) => v.toString(16).padStart(2, "0").repeat(2);
  return `rgb:${c(n.r)}/${c(n.g)}/${c(n.b)}`;
}

export interface OscColorHint {
  appearance?: TermAppearance;
  background?: string;
  foreground?: string;
}

/** REST / WS 入口用：非法值整项丢掉，不当成 4xx——深浅缺失只是退回不注入。 */
export function sanitizeColorHint(raw: {
  appearance?: unknown;
  background?: unknown;
  foreground?: unknown;
}): OscColorHint {
  const hint: OscColorHint = {};
  if (isTermAppearance(raw.appearance)) hint.appearance = raw.appearance;
  if (typeof raw.background === "string" && parseHexRgb(raw.background)) {
    hint.background = raw.background;
  }
  if (typeof raw.foreground === "string" && parseHexRgb(raw.foreground)) {
    hint.foreground = raw.foreground;
  }
  return hint;
}

function oscColor(hex: string | undefined, fallback: string): string {
  return hexToOscRgb(hex ?? "") ?? hexToOscRgb(fallback)!;
}

/** OSC 10/11/12 查询的答复（ST 结尾，与 xterm.js 一致）。 */
export function oscColorReplies(hint: OscColorHint): Record<"10" | "11" | "12", string> {
  const appearance: TermAppearance = hint.appearance ?? "dark";
  const fb = FALLBACK[appearance];
  const fg = oscColor(hint.foreground, fb.fg);
  const bg = oscColor(hint.background, fb.bg);
  const st = "\x1b\\";
  return {
    "10": `\x1b]10;${fg}${st}`,
    "11": `\x1b]11;${bg}${st}`,
    "12": `\x1b]12;${fg}${st}`,
  };
}

const QUERY_RE = /\x1b\](1[012]);\?(?:\x07|\x1b\\)/g;

const QUERY_PREFIXES = [
  "\x1b]10;?\x07",
  "\x1b]11;?\x07",
  "\x1b]12;?\x07",
  "\x1b]10;?\x1b\\",
  "\x1b]11;?\x1b\\",
  "\x1b]12;?\x1b\\",
];

function isQueryPrefix(s: string): boolean {
  return s.length > 0 && QUERY_PREFIXES.some((t) => t.startsWith(s));
}

function holdBack(s: string): { emit: string; hold: string } {
  const max = 8;
  const start = Math.max(0, s.length - max);
  for (let i = start; i < s.length; i++) {
    if (isQueryPrefix(s.slice(i))) {
      return { emit: s.slice(0, i), hold: s.slice(i) };
    }
  }
  return { emit: s, hold: "" };
}

/**
 * 从 PTY 输出里抽出 OSC 10/11/12 查询，自己答、不转给 viewer。
 *
 * 交给 xterm.js 答有两个坑：回放历史会再答一次（键入一串 ESC 垃圾），
 * 多个 Viewer 会每人答一次。Zellij 0.44 实测会把 pane 里的查询实时
 * 转发到外层并把答复带回，所以这里的答复就是内层程序看到的底色；
 * zellij client 自己 attach 时也会查一次。
 *
 * appearance 还没到时原样放过，让 xterm.js 兜底。
 */
export class OscColorGate {
  private pending = "";

  constructor(private hint: () => OscColorHint) {}

  push(data: string): { visible: string; replies: string[] } {
    const hint = this.hint();
    if (!hint.appearance) {
      const visible = this.pending + data;
      this.pending = "";
      return { visible, replies: [] };
    }

    const combined = this.pending + data;
    // 快路径：查询极罕见（Zellij 持久会话里通常到不了外层），而这段跑在
    // PTY 输出的热路径上，不该让每个 chunk 都过一遍正则。所有查询前缀都以
    // \x1b] 开头，唯一不含 \x1b] 的"可能是前缀"的形态是结尾的孤立 ESC
    // （下个 chunk 可能以 ] 续上），这两种都没有时直接原样放行。
    if (
      !combined.includes("\x1b]") &&
      combined.charCodeAt(combined.length - 1) !== 0x1b
    ) {
      this.pending = "";
      return { visible: combined, replies: [] };
    }
    const { emit, hold } = holdBack(combined);
    this.pending = hold;

    const replies: string[] = [];
    const table = oscColorReplies(hint);
    const visible = emit.replace(QUERY_RE, (_all, id: string) => {
      const reply = table[id as "10" | "11" | "12"];
      if (reply) replies.push(reply);
      return "";
    });
    return { visible, replies };
  }
}
