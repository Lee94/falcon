/**
 * 把 sprites.ts 的 op 列表画到离屏 2D canvas 上，取 alpha 通道当 R8 位图。
 * 矩形走整数坐标不抗锯齿（盒线要一像素不差地相接）；多边形与描边交给浏览器抗锯齿。
 */

import { extractAlpha, type Raster } from "./atlas.js";
import type { SpriteMetrics, SpriteOp } from "./sprites.js";

export interface SpriteRasterizer {
  raster(ops: SpriteOp[]): Raster | null;
  dispose(): void;
}

export function createSpriteRasterizer(m: SpriteMetrics): SpriteRasterizer {
  const { cellW: w, cellH: h } = m;
  const canvas: OffscreenCanvas | HTMLCanvasElement =
    typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(w, h) : document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d", { willReadFrequently: true }) as
    | OffscreenCanvasRenderingContext2D
    | CanvasRenderingContext2D
    | null;
  if (!ctx) throw new Error("2d context unavailable");
  return {
    raster(ops) {
      if (ops.length === 0) return null;
      ctx.clearRect(0, 0, w, h);
      ctx.fillStyle = "#ffffff";
      ctx.strokeStyle = "#ffffff";
      ctx.lineCap = "butt";
      ctx.lineJoin = "miter";
      for (const op of ops) {
        switch (op.op) {
          case "rect":
            ctx.globalAlpha = op.alpha ?? 1;
            ctx.fillRect(op.x, op.y, op.w, op.h);
            ctx.globalAlpha = 1;
            break;
          case "poly": {
            ctx.beginPath();
            op.points.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
            ctx.closePath();
            ctx.fill();
            break;
          }
          case "stroke": {
            ctx.beginPath();
            op.points.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
            ctx.lineWidth = op.width;
            ctx.stroke();
            break;
          }
        }
      }
      const img = ctx.getImageData(0, 0, w, h).data;
      return { data: extractAlpha(img, w, h), width: w, height: h, bearingX: 0, bearingY: 0, color: false };
    },
    dispose() {
      canvas.width = 0;
      canvas.height = 0;
    },
  };
}
