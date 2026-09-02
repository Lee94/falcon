import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { GLYPH_EMPTY, GLYPH_FLAG_COLOR, GLYPH_FULL, GlyphTable } from "./atlas.js";
import { buildPalette, channels, COLOR_NAMED, COLOR_RGB, dimColor, over, rgba } from "./colors.js";
import { toGpuTheme } from "../theme.js";
import {
  buildRow,
  CELL_HAS_CLUSTER,
  CELL_WORDS,
  computeDamage,
  FLAG_COLOR_ATLAS,
  FLAG_SOLID,
  INSTANCE_WORDS,
  MAX_INSTANCES_PER_CELL,
  selectionRows,
  STYLE_BOLD,
  STYLE_DIM,
  STYLE_DOUBLE_UNDERLINE,
  STYLE_HIDDEN,
  STYLE_INVERSE,
  STYLE_STRIKEOUT,
  STYLE_UNDERCURL,
  STYLE_UNDERLINE,
  WIDE_SPACER,
  WIDE_WIDE,
  type FrameKey,
  type RowCtx,
} from "./frame.js";

const COLS = 6;
const theme = toGpuTheme({ foreground: "#ffffff", background: "#000000", selectionBackground: "#ffffff80" });
const palette = buildPalette(theme);
const NAMED_FG = ((COLOR_NAMED << 24) | 256) >>> 0;
const NAMED_BG = ((COLOR_NAMED << 24) | 257) >>> 0;
const RED = ((COLOR_RGB << 24) | 0xff0000) >>> 0;

interface Cell {
  cp?: number;
  wide?: number;
  cluster?: boolean;
  fg?: number;
  bg?: number;
  flags?: number;
}

function grid(rows: Cell[][]): Uint32Array {
  const cells = new Uint32Array(rows.length * COLS * CELL_WORDS);
  rows.forEach((row, r) => {
    for (let c = 0; c < COLS; c++) {
      const cell = row[c] ?? {};
      const i = (r * COLS + c) * CELL_WORDS;
      cells[i] = ((cell.cp ?? 32) & 0x1fffff) | ((cell.wide ?? 0) << 21) | (cell.cluster ? CELL_HAS_CLUSTER : 0);
      cells[i + 1] = cell.fg ?? NAMED_FG;
      cells[i + 2] = cell.bg ?? NAMED_BG;
      cells[i + 3] = cell.flags ?? 0;
    }
  });
  return cells;
}

function ctxWith(overrides: Partial<RowCtx> = {}): RowCtx & { calls: string[] } {
  const glyphs = new GlyphTable();
  // id 0：普通字形；id 1：彩色；id 2..4：undercurl/dotted/dashed sprite
  glyphs.push(10, 20, 7, 12, -1, 3, 0);
  glyphs.push(100, 200, 14, 14, 0, 0, GLYPH_FLAG_COLOR);
  glyphs.push(0, 0, 8, 4, 0, 13, 0);
  glyphs.push(8, 0, 8, 2, 0, 13, 0);
  glyphs.push(16, 0, 8, 2, 0, 13, 0);
  const calls: string[] = [];
  return {
    calls,
    cols: COLS,
    palette,
    glyphs,
    lookup(cp, text, bold, italic, wide) {
      calls.push(`${text ?? String.fromCodePoint(cp)}|${bold ? "b" : ""}${italic ? "i" : ""}${wide ? "w" : ""}`);
      if (cp === 0x1f642) return 1;
      if (cp === 0xffff) return GLYPH_FULL;
      return 0;
    },
    decor: { undercurl: 2, dotted: 3, dashed: 4 },
    solid: {
      underline: [{ x: 0, y: 13, w: 8, h: 1 }],
      double: [
        { x: 0, y: 12, w: 8, h: 1 },
        { x: 0, y: 14, w: 8, h: 1 },
      ],
      strikeout: [{ x: 0, y: 7, w: 8, h: 1 }],
    },
    clusterText: () => "é",
    selection: null,
    hover: null,
    ...overrides,
  };
}

function build(cells: Uint32Array, row: number, ctx: RowCtx) {
  const bg = new Uint32Array(COLS);
  const fg = new Uint32Array(COLS * MAX_INSTANCES_PER_CELL * INSTANCE_WORDS);
  const n = buildRow(cells, row, ctx, bg, fg);
  const inst = (i: number) => {
    const w = fg.subarray(i * INSTANCE_WORDS, (i + 1) * INSTANCE_WORDS);
    return {
      gx: w[0]! & 0xffff,
      gy: w[0]! >>> 16,
      gw: w[1]! & 0xffff,
      gh: w[1]! >>> 16,
      bx: (w[2]! << 16) >> 16,
      by: w[2]! >> 16,
      col: w[3]! & 0xffff,
      row: w[3]! >>> 16,
      color: w[4]!,
      flags: w[5]!,
    };
  };
  return { n, bg, inst };
}

describe("buildRow", () => {
  it("空行：全默认底色，0 实例", () => {
    const { n, bg } = build(grid([[]]), 0, ctxWith());
    assert.equal(n, 0);
    assert.deepEqual([...bg], new Array(COLS).fill(palette.bg));
  });

  it("字形实例：回读 6 个词逐字段对得上", () => {
    const { n, inst } = build(grid([[], [], [], [{}, { cp: 0x41, fg: RED, flags: STYLE_BOLD }]]), 3, ctxWith());
    assert.equal(n, 1);
    assert.deepEqual(inst(0), { gx: 10, gy: 20, gw: 7, gh: 12, bx: -1, by: 3, col: 1, row: 3, color: rgba(255, 0, 0), flags: 0 });
  });

  it("彩色字形带 COLOR_ATLAS 标志；宽字符 1 个字形，spacer 无输出；下划线两格都发", () => {
    const cells = grid([[{ cp: 0x1f642, wide: WIDE_WIDE, flags: STYLE_UNDERLINE }, { cp: 0x1f642, wide: WIDE_SPACER, flags: STYLE_UNDERLINE }]]);
    const ctx = ctxWith();
    const { n, inst } = build(cells, 0, ctx);
    assert.equal(n, 3);
    assert.equal(inst(0).flags, FLAG_COLOR_ATLAS);
    assert.equal(inst(1).flags, FLAG_SOLID);
    assert.equal(inst(1).col, 0);
    assert.equal(inst(2).flags, FLAG_SOLID);
    assert.equal(inst(2).col, 1);
    assert.deepEqual(ctx.calls, ["🙂|w"]);
  });

  it("带下划线的空格发装饰不发字形；删除线；双下划线两段；undercurl 走 sprite", () => {
    const cells = grid([[
      { flags: STYLE_UNDERLINE },
      { cp: 0x41, flags: STYLE_STRIKEOUT },
      { cp: 0x41, flags: STYLE_DOUBLE_UNDERLINE },
      { cp: 0x41, flags: STYLE_UNDERCURL },
    ]]);
    const { n, inst } = build(cells, 0, ctxWith());
    assert.equal(n, 1 + 2 + 3 + 2);
    assert.deepEqual([inst(0).flags, inst(0).gw, inst(0).gh, inst(0).bx, inst(0).by], [FLAG_SOLID, 8, 1, 0, 13]);
    assert.equal(inst(2).by, 7);
    assert.equal(inst(4).by, 12);
    assert.equal(inst(5).by, 14);
    assert.deepEqual([inst(7).gx, inst(7).gh, inst(7).flags], [0, 4, 0]);
  });

  it("INVERSE 交换 fg/bg 且 named 按翻转后的 isFg 兜底；DIM 只改 fg；HIDDEN 保留 bg 不发任何实例", () => {
    const cells = grid([[
      { cp: 0x41, fg: RED, flags: STYLE_INVERSE },
      { cp: 0x41, fg: RED, flags: STYLE_DIM },
      { cp: 0x41, fg: RED, bg: RED, flags: STYLE_HIDDEN | STYLE_UNDERLINE },
    ]]);
    const { n, bg, inst } = build(cells, 0, ctxWith());
    assert.equal(bg[0], rgba(255, 0, 0));
    assert.equal(inst(0).color, palette.bg);
    assert.equal(inst(1).color, dimColor(rgba(255, 0, 0)));
    assert.equal(bg[1], palette.bg);
    assert.equal(bg[2], rgba(255, 0, 0));
    assert.equal(n, 2);
  });

  it("选区：bg 合成、无 selFg 时 fg 不变；block 与多行 simple 选区", () => {
    const cells = grid([
      [{ cp: 0x41, fg: RED }, { cp: 0x41, fg: RED }, { cp: 0x41, fg: RED }],
      [{ cp: 0x41, fg: RED }, { cp: 0x41, fg: RED }, { cp: 0x41, fg: RED }],
    ]);
    const simple = ctxWith({ selection: { startLine: 0, startCol: 1, endLine: 1, endCol: 0, isBlock: false } });
    const r0 = build(cells, 0, simple);
    assert.equal(r0.bg[0], palette.bg);
    assert.equal(r0.bg[1], over(palette.selBg, palette.bg));
    assert.equal(r0.bg[5], over(palette.selBg, palette.bg));
    assert.equal(r0.inst(1).color, rgba(255, 0, 0));
    const r1 = build(cells, 1, simple);
    assert.equal(r1.bg[0], over(palette.selBg, palette.bg));
    assert.equal(r1.bg[1], palette.bg);
    const block = ctxWith({ selection: { startLine: 0, startCol: 1, endLine: 1, endCol: 1, isBlock: true } });
    const b1 = build(cells, 1, block);
    assert.deepEqual([b1.bg[0], b1.bg[1], b1.bg[2]], [palette.bg, over(palette.selBg, palette.bg), palette.bg]);
    const withFg = ctxWith({
      palette: buildPalette(toGpuTheme({ foreground: "#ffffff", background: "#000000", selectionForeground: "#00ff00" })),
      selection: { startLine: 0, startCol: 0, endLine: 0, endCol: 5, isBlock: false },
    });
    assert.equal(build(cells, 0, withFg).inst(0).color, rgba(0, 255, 0));
    assert.deepEqual(channels(over(palette.selBg, palette.bg)), [128, 128, 128, 255]);
  });

  it("hover 只在该行区间加单下划线，与已有下划线不重复", () => {
    const cells = grid([[{ cp: 0x41 }, { cp: 0x41, flags: STYLE_UNDERLINE }, { cp: 0x41 }]]);
    const { n, inst } = build(cells, 0, ctxWith({ hover: { line: 0, startCol: 0, endCol: 1 } }));
    assert.equal(n, 5);
    assert.equal(inst(1).flags, FLAG_SOLID);
    assert.equal(inst(3).flags, FLAG_SOLID);
    assert.equal(build(cells, 0, ctxWith({ hover: { line: 1, startCol: 0, endCol: 1 } })).n, 4);
  });

  it("cluster 格走 clusterText；GLYPH_FULL 返回 -1", () => {
    const ctx = ctxWith();
    build(grid([[{ cp: 0x65, cluster: true }]]), 0, ctx);
    assert.deepEqual(ctx.calls, ["é|"]);
    assert.equal(build(grid([[{ cp: 0xffff }]]), 0, ctxWith()).n, -1);
  });

  it("实例数上限 cols × MAX_INSTANCES_PER_CELL", () => {
    const worst = grid([new Array(COLS).fill({ cp: 0x41, flags: STYLE_DOUBLE_UNDERLINE | STYLE_STRIKEOUT })]);
    const { n } = build(worst, 0, ctxWith());
    assert.equal(n, COLS * MAX_INSTANCES_PER_CELL);
    assert.notEqual(GLYPH_EMPTY, GLYPH_FULL);
  });
});

describe("computeDamage", () => {
  const key = (o: Partial<FrameKey> = {}): FrameKey => ({
    cols: 6,
    rows: 4,
    displayOffset: 0,
    altScreen: false,
    selection: null,
    hover: null,
    cursorLine: 0,
    cursorCol: 0,
    cursorVisible: true,
    ...o,
  });
  const none = () => new Uint8Array(4);

  it("首帧 / forceFull / 尺寸 / displayOffset / altScreen → full", () => {
    assert.equal(computeDamage(null, key(), none(), false).kind, "full");
    assert.equal(computeDamage(key(), key(), none(), true).kind, "full");
    assert.equal(computeDamage(key(), key({ rows: 5 }), none(), false).kind, "full");
    assert.equal(computeDamage(key(), key({ displayOffset: 3 }), none(), false).kind, "full");
    assert.equal(computeDamage(key({ displayOffset: 3 }), key({ displayOffset: 3 }), none(), false).kind, "full");
    assert.equal(computeDamage(key(), key({ altScreen: true }), none(), false).kind, "full");
  });

  it("pending 行 → partial；选区出现 / 移动 / 消失 → 新旧区间并集并裁剪；hover 换行 → 两行", () => {
    const pending = none();
    pending[2] = 1;
    const d = computeDamage(key(), key(), pending, false);
    assert.equal(d.kind, "partial");
    assert.deepEqual([...(d as { rows: Uint8Array }).rows], [0, 0, 1, 0]);

    const sel = { startLine: 1, startCol: 0, endLine: 9, endCol: 0, isBlock: false };
    const appear = computeDamage(key(), key({ selection: sel }), none(), false);
    assert.deepEqual([...(appear as { rows: Uint8Array }).rows], [0, 1, 1, 1]);
    const moved = computeDamage(key({ selection: sel }), key({ selection: { ...sel, startLine: 0, endLine: 0 } }), none(), false);
    assert.deepEqual([...(moved as { rows: Uint8Array }).rows], [1, 1, 1, 1]);
    const gone = computeDamage(key({ selection: { ...sel, endLine: 1 } }), key(), none(), false);
    assert.deepEqual([...(gone as { rows: Uint8Array }).rows], [0, 1, 0, 0]);
    assert.equal(computeDamage(key({ selection: sel }), key({ selection: { ...sel } }), none(), false).kind, "noop");

    const h = computeDamage(key({ hover: { line: 0, startCol: 0, endCol: 1 } }), key({ hover: { line: 3, startCol: 0, endCol: 1 } }), none(), false);
    assert.deepEqual([...(h as { rows: Uint8Array }).rows], [1, 0, 0, 1]);
  });

  it("只动光标 → cursor；无变化 → noop", () => {
    assert.equal(computeDamage(key(), key({ cursorCol: 3 }), none(), false).kind, "cursor");
    assert.equal(computeDamage(key(), key({ cursorVisible: false }), none(), false).kind, "cursor");
    assert.equal(computeDamage(key(), key(), none(), false).kind, "noop");
  });

  it("selectionRows 裁剪与倒置", () => {
    assert.deepEqual(selectionRows({ startLine: 5, startCol: 0, endLine: 2, endCol: 0, isBlock: false }, 4), [2, 3]);
    assert.equal(selectionRows({ startLine: 9, startCol: 0, endLine: 9, endCol: 0, isBlock: false }, 4), null);
    assert.equal(selectionRows(null, 4), null);
  });
});
