/**
 * 主题派生用的颜色数学。全是纯函数，输入输出都是 `#rrggbb`（可带 aa）字符串。
 *
 * 混色在 OKLab 里做而不是 sRGB：从底色往前景色掺 4% 这种"轻微提亮 / 压暗"，
 * sRGB 线性插值在深色端会一下子跳得太亮、在浅色端又几乎看不出来；OKLab 的
 * 亮度是感知均匀的，同一个比例在任何底色上看起来都是"差不多那么一点"。
 * 转换公式照 Björn Ottosson 的原文，系数一字不改。
 */

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

const HEX = /^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i;

/** #rgb / #rrggbb / #rrggbbaa → 0–255 分量与 0–1 的 alpha。非法返回 undefined。 */
export function parseHex(hex: string): (Rgb & { a: number }) | undefined {
  const m = HEX.exec(hex.trim());
  if (!m) return undefined;
  let h = m[1]!;
  if (h.length === 3) h = `${h[0]}${h[0]}${h[1]}${h[1]}${h[2]}${h[2]}`;
  const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
  return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
}

export function isHexColor(s: string): boolean {
  return HEX.test(s.trim());
}

function byte(n: number): string {
  return Math.round(Math.min(255, Math.max(0, n)))
    .toString(16)
    .padStart(2, "0");
}

export function toHex(c: Rgb): string {
  return `#${byte(c.r)}${byte(c.g)}${byte(c.b)}`;
}

/** 规范成小写 #rrggbb（丢掉 alpha）。非法输入原样返回，让调用方自己决定。 */
export function normalizeHex(hex: string): string {
  const c = parseHex(hex);
  return c ? toHex(c) : hex;
}

/** #rrggbb + alpha(0–1) → #rrggbbaa */
export function withAlpha(hex: string, alpha: number): string {
  const c = parseHex(hex);
  if (!c) return hex;
  return `${toHex(c)}${byte(alpha * 255)}`;
}

function srgbToLinear(v: number): number {
  const s = v / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function linearToSrgb(v: number): number {
  const c = Math.min(1, Math.max(0, v));
  return (c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055) * 255;
}

/** WCAG 相对亮度 0–1。解析失败当纯黑。 */
export function luminance(hex: string): number {
  const c = parseHex(hex);
  if (!c) return 0;
  return 0.2126 * srgbToLinear(c.r) + 0.7152 * srgbToLinear(c.g) + 0.0722 * srgbToLinear(c.b);
}

/** WCAG 对比度 1–21 */
export function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

export interface Oklab {
  L: number;
  a: number;
  b: number;
}

export function rgbToOklab(c: Rgb): Oklab {
  const r = srgbToLinear(c.r);
  const g = srgbToLinear(c.g);
  const b = srgbToLinear(c.b);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return {
    L: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    a: 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    b: 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  };
}

/** OKLab → 线性 sRGB，**不截断**：出了 0–1 就说明这个颜色不在 sRGB 色域里 */
function oklabToLinear(c: Oklab): Rgb {
  const l = (c.L + 0.3963377774 * c.a + 0.2158037573 * c.b) ** 3;
  const m = (c.L - 0.1055613458 * c.a - 0.0638541728 * c.b) ** 3;
  const s = (c.L - 0.0894841775 * c.a - 1.291485548 * c.b) ** 3;
  return {
    r: 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    g: -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    b: -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  };
}

function inGamut(c: Oklab): boolean {
  const { r, g, b } = oklabToLinear(c);
  const ok = (v: number) => v >= -1e-4 && v <= 1 + 1e-4;
  return ok(r) && ok(g) && ok(b);
}

export function oklabToRgb(c: Oklab): Rgb {
  const lin = oklabToLinear(c);
  return { r: linearToSrgb(lin.r), g: linearToSrgb(lin.g), b: linearToSrgb(lin.b) };
}

/** OKLab 感知亮度 0–1，比 WCAG 亮度更接近"看起来有多亮"，用来判主题深浅之外的排序 */
export function perceptualLightness(hex: string): number {
  const c = parseHex(hex);
  return c ? rgbToOklab(c).L : 0;
}

/**
 * 在 OKLab 里从 a 往 b 走 t（0–1）。t=0 回 a，t=1 回 b。
 * 结果超出 sRGB 色域时按分量截断——两端都在色域里时几乎不会发生。
 */
export function mix(a: string, b: string, t: number): string {
  const ca = parseHex(a);
  const cb = parseHex(b);
  if (!ca || !cb) return a;
  const k = Math.min(1, Math.max(0, t));
  if (k === 0) return toHex(ca);
  if (k === 1) return toHex(cb);
  const la = rgbToOklab(ca);
  const lb = rgbToOklab(cb);
  return toHex(
    oklabToRgb({
      L: la.L + (lb.L - la.L) * k,
      a: la.a + (lb.a - la.a) * k,
      b: la.b + (lb.b - la.b) * k,
    })
  );
}

/**
 * 只动 OKLab 的亮度、不碰色度：把一块底色压暗 / 提亮，又不把它的色调洗成灰。
 * 往黑掺会同时拉低 a/b，Solarized 的暖白、Catppuccin 的紫灰压两下就变中性灰了；
 * 窗口底是整屏最大的一块颜色，主题的性格必须留在上面。
 */
export function shiftLightness(hex: string, delta: number): string {
  const c = parseHex(hex);
  if (!c) return hex;
  const lab = rgbToOklab(c);
  const L = Math.min(1, Math.max(0, lab.L + delta));
  if (inGamut({ L, a: lab.a, b: lab.b })) return toHex(oklabToRgb({ L, a: lab.a, b: lab.b }));
  // 高饱和的底色（Borland 那种纯蓝）压暗会掉出 sRGB，直接截断分量就等于没压够亮度。
  // 二分把色度收到刚好回色域里：亮度优先保住——层次是靠亮度差看出来的，代价是
  // 一点饱和度。
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 12; i++) {
    const mid = (lo + hi) / 2;
    if (inGamut({ L, a: lab.a * mid, b: lab.b * mid })) lo = mid;
    else hi = mid;
  }
  return toHex(oklabToRgb({ L, a: lab.a * lo, b: lab.b * lo }));
}

/**
 * 保证 color 在 against 上至少有 min 的对比度：不够就往 toward 掺，掺到刚好够为止。
 * 掺色而不是单独拉亮度，是为了永远待在色域里、又不至于把色相洗掉太多。
 * toward 自己都不够对比度时返回 toward——已经是能做到的极限。
 */
export function ensureContrast(color: string, against: string, min: number, toward: string): string {
  if (contrast(color, against) >= min) return normalizeHex(color);
  if (contrast(toward, against) < min) return normalizeHex(toward);
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 14; i++) {
    const mid = (lo + hi) / 2;
    if (contrast(mix(color, toward, mid), against) >= min) hi = mid;
    else lo = mid;
  }
  return mix(color, toward, hi);
}

/**
 * 从候选里挑第一个在 against 上够 min 对比度的；都不够就挑对比度最高的。
 * 顺序有意义：调用方把"更想要的"（通常是普通色而不是亮色）排在前面。
 */
export function pickReadable(candidates: string[], against: string, min: number): string {
  let best = candidates[0]!;
  let bestRatio = -1;
  for (const c of candidates) {
    const ratio = contrast(c, against);
    if (ratio >= min) return normalizeHex(c);
    if (ratio > bestRatio) {
      best = c;
      bestRatio = ratio;
    }
  }
  return normalizeHex(best);
}

/** 两个候选里在 against 上对比度更高的那个（按钮字色在按钮底上选黑还是白这类） */
export function moreReadable(a: string, b: string, against: string): string {
  return contrast(a, against) >= contrast(b, against) ? normalizeHex(a) : normalizeHex(b);
}
