/**
 * 装饰当作字形处理（sugarloaf / Ghostty 的做法）：直线类（单/双下划线、删除线、
 * bar / underline 光标）是 SOLID 矩形实例，像素精确、零图集占用；只有波浪 / 点 /
 * 虚线需要真的位图，按 cell 宽光栅化一格，进灰度图集，宽字符发两份。
 *
 * undercurl 用 raised cosine：一格恰好一个周期、两端导数为零，相邻格拼起来无缝。
 */

import type { Raster } from "./atlas.js";
import type { CellMetrics } from "./metrics.js";

export type SpriteDecor = "undercurl" | "dotted" | "dashed";
export type SolidDecor = "underline" | "double" | "strikeout" | "cursorBar" | "cursorUnderline";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 直线类装饰在 cell 内的矩形（物理 px）；double 返回两段 */
export function solidRects(kind: SolidDecor, m: CellMetrics): Rect[] {
  const t = m.underlineThick;
  switch (kind) {
    case "underline":
      return [{ x: 0, y: m.underlineY, w: m.cellW, h: t }];
    case "double": {
      // 两条各 t 厚、隔 1px；放不下就整体上移
      let y0 = m.underlineY;
      const need = t * 2 + 1;
      if (y0 + need > m.cellH) y0 = Math.max(0, m.cellH - need);
      return [
        { x: 0, y: y0, w: m.cellW, h: t },
        { x: 0, y: Math.min(m.cellH - t, y0 + t + 1), w: m.cellW, h: t },
      ];
    }
    case "strikeout":
      return [{ x: 0, y: m.strikeY, w: m.cellW, h: t }];
    case "cursorBar":
      return [{ x: 0, y: 0, w: Math.min(m.barW, m.cellW), h: m.cellH }];
    case "cursorUnderline": {
      const h = Math.min(m.cellH, Math.max(2, t * 2));
      return [{ x: 0, y: m.cellH - h, w: m.cellW, h }];
    }
  }
}

/** 波浪 / 点 / 虚线：R8 位图，宽 = cellW */
export function decorationSprite(kind: SpriteDecor, m: CellMetrics): Raster {
  const w = m.cellW;
  const t = m.underlineThick;
  switch (kind) {
    case "dotted": {
      // 点与空各 t 宽；从"亮"开始，两格拼接仍是周期序列
      const data = new Uint8Array(w * t);
      for (let x = 0; x < w; x++) {
        const on = Math.floor(x / t) % 2 === 0;
        if (!on) continue;
        for (let y = 0; y < t; y++) data[y * w + x] = 255;
      }
      return { data, width: w, height: t, bearingX: 0, bearingY: m.underlineY, color: false };
    }
    case "dashed": {
      // 段长 = 半格，空 = 与段等长的 1/2；从"亮"开始
      const seg = Math.max(2, Math.round(w / 2));
      const gap = Math.max(1, Math.round(seg / 2));
      const data = new Uint8Array(w * t);
      for (let x = 0; x < w; x++) {
        const on = x % (seg + gap) < seg;
        if (!on) continue;
        for (let y = 0; y < t; y++) data[y * w + x] = 255;
      }
      return { data, width: w, height: t, bearingX: 0, bearingY: m.underlineY, color: false };
    }
    case "undercurl": {
      // 振幅 ≈ cellW / π（与 sugarloaf 一致），至少一个笔画厚；总高 = 振幅 + 笔画 + 1
      const amp = Math.max(t, Math.ceil(w / Math.PI));
      const h = amp + t + 1;
      const data = new Uint8Array(w * h);
      const half = t / 2;
      const baseline = h - half - 0.5;
      for (let x = 0; x < w; x++) {
        const s = 0.5 * (1 - Math.cos(((x + 0.5) / w) * 2 * Math.PI));
        const yc = baseline - s * amp;
        const y0 = Math.max(0, Math.floor(yc - half));
        const y1 = Math.min(h, Math.ceil(yc + half));
        for (let y = y0; y < y1; y++) data[y * w + x] = 255;
      }
      // 波峰顶到下划线位置，整块不出格
      const top = Math.max(0, Math.min(m.underlineY, m.cellH - h));
      return { data, width: w, height: h, bearingX: 0, bearingY: top, color: false };
    }
  }
}
