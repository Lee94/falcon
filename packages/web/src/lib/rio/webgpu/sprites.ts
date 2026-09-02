/**
 * 程序化 sprite：盒线、块元素、braille、powerline 不用字体字形，按 cell 尺寸自己画。
 * 字体里这些字形是按字体自己的行盒设计的：Maple Mono 的 ┼ 比 1.0 倍行高的格子高
 * 50%，上下溢进邻行；回退字体又常常与格宽对不齐，zellij 边框就会出缝或粗细不一。
 * 自己画的位图正好一格大，相邻格天然相接，与字体无关（sugarloaf / Ghostty 同款做法）。
 *
 * 本文件是纯函数：每个码位产出一组绘图 op（cell 像素坐标），spriteRaster.ts 用离屏
 * 2D canvas 解释 op 取 alpha。这样盒线表与几何在 node 里可测，画线抗锯齿交给浏览器。
 *
 * 盒线两套模型：
 * - 无双线：每条臂是一个矩形（light / heavy 各自粗细，同轴居中），延伸到中心方块的远侧，
 *   交叉处靠重叠成形；
 * - 含双线：3×3 的 junction 网格（列 / 行 0、2 是双线的两条线，1 是单线 / 双线间隙，
 *   宽度都等于 light 粗细）。每条臂上的每条线延伸到"该停下的"网格索引：对面有臂就穿过；
 *   否则遇到垂直臂的线停下——同侧臂取最近一条，异侧臂取最远一条（外角），单线遇到
 *   拐角的双线延伸到远线、遇到穿过的双线停在近线。Unicode 图表里所有 ╒ ╤ ╔ ╬ 组合
 *   都能由这几条规则复原，见测试。
 */

import type { CellMetrics } from "./metrics.js";

export type SpriteOp =
  /** 轴对齐矩形，alpha 给阴影块用 */
  | { op: "rect"; x: number; y: number; w: number; h: number; alpha?: number }
  /** 填充多边形 */
  | { op: "poly"; points: [number, number][] }
  /** 描边折线，方头 */
  | { op: "stroke"; points: [number, number][]; width: number };

export interface SpriteMetrics {
  cellW: number;
  cellH: number;
  /** light 线粗；取 underlineThick，与下划线同粗 */
  thick: number;
}

export function spriteMetrics(m: CellMetrics): SpriteMetrics {
  return { cellW: m.cellW, cellH: m.cellH, thick: Math.max(1, m.underlineThick) };
}

/** 一次比较过滤掉 ASCII 与绝大多数文字；powerline 在 0xE0B0 以上不受影响 */
export function isSprite(cp: number): boolean {
  if (cp < 0x2500) return false;
  return (cp >= 0x2500 && cp <= 0x259f) || (cp >= 0x2800 && cp <= 0x28ff) || (cp >= 0xe0b0 && cp <= 0xe0bf);
}

export function spriteOps(cp: number, m: SpriteMetrics): SpriteOp[] | null {
  if (m.cellW <= 0 || m.cellH <= 0) return null;
  if (cp >= 0x2500 && cp <= 0x257f) return boxOps(cp, m);
  if (cp >= 0x2580 && cp <= 0x259f) return blockOps(cp, m);
  if (cp >= 0x2800 && cp <= 0x28ff) return brailleOps(cp, m);
  if (cp >= 0xe0b0 && cp <= 0xe0bf) return powerlineOps(cp, m);
  return null;
}

// ---------------- 盒线 U+2500..257F ----------------

/** 每格 4 个字符：上 右 下 左；N 无 L 细 H 粗 D 双。空串 = 特殊字形（虚线 / 圆角 / 对角线） */
const BOX_ARMS: readonly string[] = [
  "NLNL", "NHNH", "LNLN", "HNHN", "", "", "", "", "", "", "", "", // 2500..250B
  "NLLN", "NHLN", "NLHN", "NHHN", "NNLL", "NNLH", "NNHL", "NNHH", // 250C..2513
  "LLNN", "LHNN", "HLNN", "HHNN", "LNNL", "LNNH", "HNNL", "HNNH", // 2514..251B
  "LLLN", "LHLN", "HLLN", "LLHN", "HLHN", "HHLN", "LHHN", "HHHN", // 251C..2523
  "LNLL", "LNLH", "HNLL", "LNHL", "HNHL", "HNLH", "LNHH", "HNHH", // 2524..252B
  "NLLL", "NLLH", "NHLL", "NHLH", "NLHL", "NLHH", "NHHL", "NHHH", // 252C..2533
  "LLNL", "LLNH", "LHNL", "LHNH", "HLNL", "HLNH", "HHNL", "HHNH", // 2534..253B
  "LLLL", "LLLH", "LHLL", "LHLH", "HLLL", "LLHL", "HLHL", "HLLH", // 253C..2543
  "HHLL", "LLHH", "LHHL", "HHLH", "LHHH", "HLHH", "HHHL", "HHHH", // 2544..254B
  "", "", "", "", // 254C..254F 双段虚线
  "NDND", "DNDN", "NDLN", "NLDN", "NDDN", "NNLD", "NNDL", "NNDD", // 2550..2557
  "LDNN", "DLNN", "DDNN", "LNND", "DNNL", "DNND", "LDLN", "DLDN", // 2558..255F
  "DDDN", "LNLD", "DNDL", "DNDD", "NDLD", "NLDL", "NDDD", "LDND", // 2560..2567
  "DLNL", "DDND", "LDLD", "DLDL", "DDDD", // 2568..256C
  "", "", "", "", // 256D..2570 圆角
  "", "", "", // 2571..2573 对角线
  "NNNL", "LNNN", "NLNN", "NNLN", "NNNH", "HNNN", "NHNN", "NNHN", // 2574..257B
  "NHNL", "LNHN", "NLNH", "HNLN", // 257C..257F
];

type Arm = "N" | "L" | "H" | "D";
interface Arms {
  up: Arm;
  right: Arm;
  down: Arm;
  left: Arm;
}

function heavyOf(m: SpriteMetrics): number {
  return Math.max(m.thick + 1, m.thick * 2);
}

function boxOps(cp: number, m: SpriteMetrics): SpriteOp[] | null {
  const spec = BOX_ARMS[cp - 0x2500];
  if (spec === undefined) return null;
  if (spec === "") return boxSpecial(cp, m);
  const arms: Arms = {
    up: spec[0] as Arm,
    right: spec[1] as Arm,
    down: spec[2] as Arm,
    left: spec[3] as Arm,
  };
  const hasDouble = spec.includes("D");
  return hasDouble ? doubleJunction(arms, m) : simpleArms(arms, m);
}

/** 无双线：臂 = 同轴居中的矩形，延伸到中心方块远侧 */
function simpleArms(a: Arms, m: SpriteMetrics): SpriteOp[] {
  const { cellW, cellH } = m;
  const th = (s: Arm) => (s === "H" ? heavyOf(m) : m.thick);
  const bandX = (k: number) => Math.floor((cellW - k) / 2);
  const bandY = (k: number) => Math.floor((cellH - k) / 2);
  const horiz = [a.left, a.right].filter((s) => s !== "N");
  const vert = [a.up, a.down].filter((s) => s !== "N");
  // 中心方块按最粗的那条臂算，另一方向的臂延伸到它的远侧才能接上
  const hMax = horiz.length ? Math.max(...horiz.map(th)) : 0;
  const vMax = vert.length ? Math.max(...vert.map(th)) : 0;
  const ops: SpriteOp[] = [];
  if (a.up !== "N") {
    const k = th(a.up);
    const yEnd = hMax ? bandY(hMax) + hMax : bandY(k) + k;
    ops.push({ op: "rect", x: bandX(k), y: 0, w: k, h: yEnd });
  }
  if (a.down !== "N") {
    const k = th(a.down);
    const yStart = hMax ? bandY(hMax) : bandY(k);
    ops.push({ op: "rect", x: bandX(k), y: yStart, w: k, h: cellH - yStart });
  }
  if (a.left !== "N") {
    const k = th(a.left);
    const xEnd = vMax ? bandX(vMax) + vMax : bandX(k) + k;
    ops.push({ op: "rect", x: 0, y: bandY(k), w: xEnd, h: k });
  }
  if (a.right !== "N") {
    const k = th(a.right);
    const xStart = vMax ? bandX(vMax) : bandX(k);
    ops.push({ op: "rect", x: xStart, y: bandY(k), w: cellW - xStart, h: k });
  }
  return ops;
}

/** 双线 → 线索引 {0,2}，单线 → {1}，无 → [] */
function lineIdx(s: Arm): number[] {
  if (s === "D") return [0, 2];
  if (s === "N") return [];
  return [1];
}

/**
 * 一条线（来自 from 方向的臂、在 3×3 网格里的绝对索引 line）向格心行进时停在哪个
 * 网格索引（行进坐标：0 近 2 远）。见文件头的规则说明。
 */
export function junctionStop(
  line: number,
  hasOpposite: boolean,
  /** 与该线同侧的垂直臂的线索引（行进坐标）；单线（line 1）时是任意一侧 */
  near: number[],
  /** 异侧垂直臂的线索引（行进坐标）；单线时是另一侧 */
  far: number[]
): number {
  if (hasOpposite) return 2;
  const all = [...near, ...far];
  if (all.length === 0) return line === 1 ? 1 : 2;
  if (line === 1) {
    // 单线：垂直的双线穿过 → 停在近线；拐角 → 延伸到远线
    const passes = near.length > 0 && far.length > 0;
    return passes ? Math.min(...all) : Math.max(...all);
  }
  return near.length > 0 ? Math.min(...near) : Math.max(...far);
}

function doubleJunction(a: Arms, m: SpriteMetrics): SpriteOp[] {
  const { cellW, cellH, thick: t } = m;
  const xg = Math.floor((cellW - 3 * t) / 2);
  const yg = Math.floor((cellH - 3 * t) / 2);
  const col = (k: number) => xg + k * t;
  const row = (k: number) => yg + k * t;
  const up = lineIdx(a.up);
  const down = lineIdx(a.down);
  const left = lineIdx(a.left);
  const right = lineIdx(a.right);
  const flip = (xs: number[]) => xs.map((x) => 2 - x);
  const ops: SpriteOp[] = [];

  // 水平臂：line 是绝对行；同侧 = 行 0 对应上臂、行 2 对应下臂；行 1（单线）两边各一组，
  // junctionStop 靠"两边都有"判断垂直的双线是穿过还是拐角
  const nearFarH = (line: number): [number[], number[]] =>
    line === 0 ? [up, down] : line === 2 ? [down, up] : [up, down];
  for (const line of left) {
    const [near, far] = nearFarH(line);
    const stop = junctionStop(line, right.length > 0, near, far);
    ops.push({ op: "rect", x: 0, y: row(line), w: col(stop + 1), h: t });
  }
  for (const line of right) {
    const [near, far] = nearFarH(line);
    // 从右向左行进：列索引翻转
    const stop = 2 - junctionStop(line, left.length > 0, flip(near), flip(far));
    ops.push({ op: "rect", x: col(stop), y: row(line), w: cellW - col(stop), h: t });
  }
  // 垂直臂：line 是绝对列；同侧 = 列 0 对应左臂、列 2 对应右臂
  const nearFarV = (line: number): [number[], number[]] =>
    line === 0 ? [left, right] : line === 2 ? [right, left] : [left, right];
  for (const line of up) {
    const [near, far] = nearFarV(line);
    const stop = junctionStop(line, down.length > 0, near, far);
    ops.push({ op: "rect", x: col(line), y: 0, w: t, h: row(stop + 1) });
  }
  for (const line of down) {
    const [near, far] = nearFarV(line);
    const stop = 2 - junctionStop(line, up.length > 0, flip(near), flip(far));
    ops.push({ op: "rect", x: col(line), y: row(stop), w: t, h: cellH - row(stop) });
  }
  return ops;
}

function dashes(m: SpriteMetrics, count: number, heavy: boolean, vertical: boolean): SpriteOp[] {
  const k = heavy ? heavyOf(m) : m.thick;
  const len = vertical ? m.cellH : m.cellW;
  const gap = Math.max(1, Math.round(m.thick));
  const seg = Math.max(1, Math.floor((len - gap * (count - 1)) / count));
  const ops: SpriteOp[] = [];
  const band = vertical ? Math.floor((m.cellW - k) / 2) : Math.floor((m.cellH - k) / 2);
  for (let i = 0; i < count; i++) {
    const start = i * (seg + gap);
    const end = i === count - 1 ? len : Math.min(len, start + seg);
    if (vertical) ops.push({ op: "rect", x: band, y: start, w: k, h: end - start });
    else ops.push({ op: "rect", x: start, y: band, w: end - start, h: k });
  }
  return ops;
}

function boxSpecial(cp: number, m: SpriteMetrics): SpriteOp[] | null {
  const { cellW: w, cellH: h, thick: t } = m;
  const cx = w / 2;
  const cy = h / 2;
  switch (cp) {
    case 0x2504:
      return dashes(m, 3, false, false);
    case 0x2505:
      return dashes(m, 3, true, false);
    case 0x2506:
      return dashes(m, 3, false, true);
    case 0x2507:
      return dashes(m, 3, true, true);
    case 0x2508:
      return dashes(m, 4, false, false);
    case 0x2509:
      return dashes(m, 4, true, false);
    case 0x250a:
      return dashes(m, 4, false, true);
    case 0x250b:
      return dashes(m, 4, true, true);
    case 0x254c:
      return dashes(m, 2, false, false);
    case 0x254d:
      return dashes(m, 2, true, false);
    case 0x254e:
      return dashes(m, 2, false, true);
    case 0x254f:
      return dashes(m, 2, true, true);
    case 0x256d: // ╭ 下 + 右
      return [{ op: "stroke", points: arcCorner(w, h, "dr"), width: t }];
    case 0x256e: // ╮ 下 + 左
      return [{ op: "stroke", points: arcCorner(w, h, "dl"), width: t }];
    case 0x256f: // ╯ 上 + 左
      return [{ op: "stroke", points: arcCorner(w, h, "ul"), width: t }];
    case 0x2570: // ╰ 上 + 右
      return [{ op: "stroke", points: arcCorner(w, h, "ur"), width: t }];
    case 0x2571: // ╱
      return [{ op: "stroke", points: [[w, 0], [0, h]], width: t }];
    case 0x2572: // ╲
      return [{ op: "stroke", points: [[0, 0], [w, h]], width: t }];
    case 0x2573: // ╳
      return [
        { op: "stroke", points: [[w, 0], [0, h]], width: t },
        { op: "stroke", points: [[0, 0], [w, h]], width: t },
      ];
    default:
      return cx > 0 && cy > 0 ? null : null;
  }
}

/** 圆角：从一条边的中点直线走到圆弧，四分之一圆，再直线到另一条边的中点 */
function arcCorner(w: number, h: number, kind: "dr" | "dl" | "ul" | "ur"): [number, number][] {
  const cx = w / 2;
  const cy = h / 2;
  const r = Math.min(cx, cy);
  // 圆心相对格心的方向：dr 的圆心在右下
  const sx = kind === "dr" || kind === "ur" ? 1 : -1;
  const sy = kind === "dr" || kind === "dl" ? 1 : -1;
  const ox = cx + sx * r;
  const oy = cy + sy * r;
  const pts: [number, number][] = [];
  // 竖直边中点 → 弧起点
  pts.push([cx, sy > 0 ? h : 0]);
  pts.push([cx, oy]);
  const steps = 12;
  for (let i = 0; i <= steps; i++) {
    const a = (Math.PI / 2) * (i / steps);
    // 从竖直方向转到水平方向
    pts.push([ox - sx * r * Math.cos(a), oy - sy * r * Math.sin(a)]);
  }
  pts.push([ox, cy]);
  pts.push([sx > 0 ? w : 0, cy]);
  return pts;
}

// ---------------- 块元素 U+2580..259F ----------------

function blockOps(cp: number, m: SpriteMetrics): SpriteOp[] | null {
  const { cellW: w, cellH: h } = m;
  const lower = (frac: number): SpriteOp[] => {
    const hh = Math.max(1, Math.round(h * frac));
    return [{ op: "rect", x: 0, y: h - hh, w, h: hh }];
  };
  const left = (frac: number): SpriteOp[] => [{ op: "rect", x: 0, y: 0, w: Math.max(1, Math.round(w * frac)), h }];
  const halfW = Math.round(w / 2);
  const halfH = Math.round(h / 2);
  const quad = (tl: boolean, tr: boolean, bl: boolean, br: boolean): SpriteOp[] => {
    const ops: SpriteOp[] = [];
    if (tl) ops.push({ op: "rect", x: 0, y: 0, w: halfW, h: halfH });
    if (tr) ops.push({ op: "rect", x: halfW, y: 0, w: w - halfW, h: halfH });
    if (bl) ops.push({ op: "rect", x: 0, y: halfH, w: halfW, h: h - halfH });
    if (br) ops.push({ op: "rect", x: halfW, y: halfH, w: w - halfW, h: h - halfH });
    return ops;
  };
  switch (cp) {
    case 0x2580:
      return [{ op: "rect", x: 0, y: 0, w, h: halfH }];
    case 0x2581:
      return lower(1 / 8);
    case 0x2582:
      return lower(2 / 8);
    case 0x2583:
      return lower(3 / 8);
    case 0x2584:
      return lower(4 / 8);
    case 0x2585:
      return lower(5 / 8);
    case 0x2586:
      return lower(6 / 8);
    case 0x2587:
      return lower(7 / 8);
    case 0x2588:
      return [{ op: "rect", x: 0, y: 0, w, h }];
    case 0x2589:
      return left(7 / 8);
    case 0x258a:
      return left(6 / 8);
    case 0x258b:
      return left(5 / 8);
    case 0x258c:
      return left(4 / 8);
    case 0x258d:
      return left(3 / 8);
    case 0x258e:
      return left(2 / 8);
    case 0x258f:
      return left(1 / 8);
    case 0x2590:
      return [{ op: "rect", x: halfW, y: 0, w: w - halfW, h }];
    case 0x2591:
      return [{ op: "rect", x: 0, y: 0, w, h, alpha: 0.25 }];
    case 0x2592:
      return [{ op: "rect", x: 0, y: 0, w, h, alpha: 0.5 }];
    case 0x2593:
      return [{ op: "rect", x: 0, y: 0, w, h, alpha: 0.75 }];
    case 0x2594:
      return [{ op: "rect", x: 0, y: 0, w, h: Math.max(1, Math.round(h / 8)) }];
    case 0x2595: {
      const ww = Math.max(1, Math.round(w / 8));
      return [{ op: "rect", x: w - ww, y: 0, w: ww, h }];
    }
    case 0x2596:
      return quad(false, false, true, false);
    case 0x2597:
      return quad(false, false, false, true);
    case 0x2598:
      return quad(true, false, false, false);
    case 0x2599:
      return quad(true, false, true, true);
    case 0x259a:
      return quad(true, false, false, true);
    case 0x259b:
      return quad(true, true, true, false);
    case 0x259c:
      return quad(true, true, false, true);
    case 0x259d:
      return quad(false, true, false, false);
    case 0x259e:
      return quad(false, true, true, false);
    case 0x259f:
      return quad(false, true, true, true);
    default:
      return null;
  }
}

// ---------------- braille U+2800..28FF ----------------

/**
 * 2×4 点阵；码位低 8 位是点的掩码：bit0-2 左列 0-2 行，bit3-5 右列 0-2 行，bit6 左列
 * 第 3 行，bit7 右列第 3 行。点画成方块（圆点在小字号下会糊成一团），居中在各自的子格里。
 */
export function brailleDots(cp: number): [number, number][] {
  const mask = cp & 0xff;
  const dots: [number, number][] = [];
  const at = (bit: number, col: number, row: number) => {
    if (mask & (1 << bit)) dots.push([col, row]);
  };
  at(0, 0, 0);
  at(1, 0, 1);
  at(2, 0, 2);
  at(3, 1, 0);
  at(4, 1, 1);
  at(5, 1, 2);
  at(6, 0, 3);
  at(7, 1, 3);
  return dots;
}

function brailleOps(cp: number, m: SpriteMetrics): SpriteOp[] {
  const { cellW: w, cellH: h } = m;
  const subW = w / 2;
  const subH = h / 4;
  const size = Math.max(1, Math.round(Math.min(subW, subH) * 0.55));
  return brailleDots(cp).map(([c, r]) => ({
    op: "rect",
    x: Math.round(c * subW + (subW - size) / 2),
    y: Math.round(r * subH + (subH - size) / 2),
    w: size,
    h: size,
  }));
}

// ---------------- powerline U+E0B0..E0BF ----------------

function halfDisc(w: number, h: number, right: boolean): [number, number][] {
  // 右半圆：圆心在左边中点，横向半径 w、纵向半径 h/2（格子不是正方形，用椭圆填满）
  const ox = right ? 0 : w;
  const oy = h / 2;
  const sx = right ? 1 : -1;
  const pts: [number, number][] = [];
  const steps = 16;
  for (let i = 0; i <= steps; i++) {
    const a = -Math.PI / 2 + Math.PI * (i / steps);
    pts.push([ox + sx * w * Math.cos(a), oy + (h / 2) * Math.sin(a)]);
  }
  return pts;
}

function powerlineOps(cp: number, m: SpriteMetrics): SpriteOp[] | null {
  const { cellW: w, cellH: h, thick: t } = m;
  const mid = h / 2;
  switch (cp) {
    case 0xe0b0: // 右三角
      return [{ op: "poly", points: [[0, 0], [w, mid], [0, h]] }];
    case 0xe0b1: // 右 chevron
      return [{ op: "stroke", points: [[0, 0], [w, mid], [0, h]], width: t }];
    case 0xe0b2: // 左三角
      return [{ op: "poly", points: [[w, 0], [0, mid], [w, h]] }];
    case 0xe0b3:
      return [{ op: "stroke", points: [[w, 0], [0, mid], [w, h]], width: t }];
    case 0xe0b4: // 右半圆
      return [{ op: "poly", points: [[0, 0], ...halfDisc(w, h, true), [0, h]] }];
    case 0xe0b5:
      return [{ op: "stroke", points: halfDisc(w, h, true), width: t }];
    case 0xe0b6: // 左半圆
      return [{ op: "poly", points: [[w, 0], ...halfDisc(w, h, false), [w, h]] }];
    case 0xe0b7:
      return [{ op: "stroke", points: halfDisc(w, h, false), width: t }];
    case 0xe0b8: // 左下三角
      return [{ op: "poly", points: [[0, 0], [w, h], [0, h]] }];
    case 0xe0b9:
      return [{ op: "stroke", points: [[0, 0], [w, h]], width: t }];
    case 0xe0ba: // 右下三角
      return [{ op: "poly", points: [[w, 0], [w, h], [0, h]] }];
    case 0xe0bb:
      return [{ op: "stroke", points: [[w, 0], [0, h]], width: t }];
    case 0xe0bc: // 左上三角
      return [{ op: "poly", points: [[0, 0], [w, 0], [0, h]] }];
    case 0xe0bd:
      return [{ op: "stroke", points: [[w, 0], [0, h]], width: t }];
    case 0xe0be: // 右上三角
      return [{ op: "poly", points: [[0, 0], [w, 0], [w, h]] }];
    case 0xe0bf:
      return [{ op: "stroke", points: [[0, 0], [w, h]], width: t }];
    default:
      return null;
  }
}
