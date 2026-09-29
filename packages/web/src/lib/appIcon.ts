/**
 * 应用图标的浏览器侧（ADR 0018）：把选择写回页面的 <link>，把用户挑的图片规整成上传用的 PNG。
 *
 * 首次加载不靠这里：index.html 的 <link rel="icon"> 指向 /api/app-icon/favicon，
 * 服务端按当前选择跳转。这里只管「设置里刚换了」之后立刻生效。
 */
import { APP_ICON_CUSTOM_SIZE, appIconLinks, type AppIconState } from "@falcon/shared";

/** Chrome 看到 icon 的 href 变了就立刻换标签页图标，不用刷新 */
export function applyAppIconLinks(state: AppIconState, doc: Document = document): void {
  const links = appIconLinks(state);
  doc.querySelector<HTMLLinkElement>('link[rel="icon"]')?.setAttribute("href", links.favicon);
  doc
    .querySelector<HTMLLinkElement>('link[rel="apple-touch-icon"]')
    ?.setAttribute("href", links.appleTouch);
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * 把 w×h 的图放进 size 见方的画布：等比缩放到长边贴边、居中，不裁切。
 * 图标多半是 logo，裁掉一截比留两道透明边难看得多。小图也放大——统一出 512。
 */
export function containRect(w: number, h: number, size: number): Rect {
  // SVG 只写了 viewBox 没写宽高时浏览器报 0，按正方形处理
  if (!(w > 0) || !(h > 0)) return { x: 0, y: 0, w: size, h: size };
  const k = size / Math.max(w, h);
  const dw = Math.round(w * k);
  const dh = Math.round(h * k);
  return { x: Math.round((size - dw) / 2), y: Math.round((size - dh) / 2), w: dw, h: dh };
}

/**
 * 浏览器能解码的图（PNG / JPEG / WebP / GIF 首帧 / SVG…）→ 512×512 PNG。
 * 服务端没有图像库，只收规整好的正方形 PNG。解不出来抛错。
 */
export async function normalizeIconImage(file: Blob): Promise<Blob> {
  const size = APP_ICON_CUSTOM_SIZE;
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const canvas = document.createElement("canvas");
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canvas 2d unavailable");
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    const r = containRect(img.naturalWidth, img.naturalHeight, size);
    ctx.drawImage(img, r.x, r.y, r.w, r.h);
    return await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("toBlob failed"))), "image/png")
    );
  } finally {
    URL.revokeObjectURL(url);
  }
}
