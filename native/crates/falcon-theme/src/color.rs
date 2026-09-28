//! 主题派生用的颜色数学（`lib/theme/color.ts`）。全是纯函数。
//!
//! 混色在 OKLab 里做而不是 sRGB：从底色往前景色掺 4% 这种"轻微提亮 / 压暗"，
//! sRGB 线性插值在深色端会一下子跳得太亮、在浅色端又几乎看不出来；OKLab 的
//! 亮度是感知均匀的，同一个比例在任何底色上看起来都是"差不多那么一点"。
//! 转换公式照 Björn Ottosson 的原文，系数一字不改——**逆矩阵 g 行的第三个系数是
//! −0.3413193965**，抄错一位灰色会泛绿（ADR 0006）。
//!
//! 与 TS 的对应关系：TS 的颜色全是 `#rrggbb` 字符串，每个函数的结果都经 `toHex`
//! 取整成 8 位分量再交给下一步；这里用 [`Rgb`]（u8 分量）表示同一件事，所以**每一步
//! 的取整点与 TS 完全相同**（`mix` 的结果是 `Rgb`，二分里每次试探都先取整再算对比度）。
//! 浮点运算顺序照抄（`a + k1 * x + k2 * y` 这类式子左结合，与 JS 一致），`**` 一律用
//! `powf`（不用 `powi`：`x ** 3` 在 JS 里走的是 pow，不是连乘，舍入不同）。
//! `tests/derive_golden.rs` 拿 web 的 deriveTheme 跑全部内置主题的结果逐字对拍。
//!
//! TS 里"非法输入原样返回"（`mix("nope", …)`）在这边由类型挡掉：拿不到非法的
//! [`Rgb`]。只有 [`normalize_hex`] 保留了字符串进、字符串出的原语义。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::js::js_trim;

/// 不透明颜色，8 位分量。文本形式是小写 `#rrggbb`（[`fmt::Display`]）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `0xrrggbb`，写常量用
    pub const fn from_u32(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// `#rgb` / `#rrggbb` / `#rrggbbaa`（首尾空白忽略、大小写不限），alpha 丢掉——
    /// 即 TS `normalizeHex` 成功的那一支。
    pub fn parse_hex(s: &str) -> Option<Self> {
        parse_hex(s).map(Rgba::rgb)
    }

    /// 小写 `#rrggbb`
    pub fn to_hex(self) -> String {
        self.to_string()
    }

    /// `withAlpha`：alpha 0–1，按 TS 的 `byte(alpha * 255)` 取整
    pub fn with_alpha(self, alpha: f64) -> Rgba {
        Rgba { r: self.r, g: self.g, b: self.b, a: byte(alpha * 255.0) }
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// 收 `#rgb` / `#rrggbb` / `#rrggbbaa`，与偏好清洗里的 `hexOrNull` 同一口径
impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("不是颜色：{s}")))
    }
}

/// 带 alpha 的颜色（`withAlpha` 的产物，边框 / 焦点环这类半透明 token）。
/// 文本形式是小写 `#rrggbbaa`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(self) -> Rgb {
        Rgb { r: self.r, g: self.g, b: self.b }
    }

    /// 0–1，即 TS `parseHex` 给的 `a`（`n / 255`）
    pub fn alpha(self) -> f64 {
        f64::from(self.a) / 255.0
    }

    /// 小写 `#rrggbbaa`
    pub fn to_hex(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// `/^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i` 匹配到的那串十六进制（不含 #）
fn hex_body(s: &str) -> Option<&str> {
    let body = js_trim(s).strip_prefix('#')?;
    // is_ascii_hexdigit 只认 ASCII；JS 的 i 标志在非 u 模式下也不会让非 ASCII 字符折叠进来
    (matches!(body.len(), 3 | 6 | 8) && body.bytes().all(|c| c.is_ascii_hexdigit())).then_some(body)
}

/// `parseHex`：#rgb / #rrggbb / #rrggbbaa → 分量；没写 alpha 时 `a = 255`（即 1）。
pub fn parse_hex(hex: &str) -> Option<Rgba> {
    let body = hex_body(hex)?;
    let expanded: String;
    let h = if body.len() == 3 {
        expanded = body.chars().flat_map(|c| [c, c]).collect();
        expanded.as_str()
    } else {
        body
    };
    let n = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgba { r: n(0)?, g: n(2)?, b: n(4)?, a: if h.len() == 8 { n(6)? } else { 255 } })
}

/// `isHexColor`
pub fn is_hex_color(s: &str) -> bool {
    hex_body(s).is_some()
}

/// `normalizeHex`：规范成小写 `#rrggbb`（丢掉 alpha）。非法输入原样返回，让调用方自己决定。
pub fn normalize_hex(hex: &str) -> String {
    match Rgb::parse_hex(hex) {
        Some(c) => c.to_hex(),
        None => hex.to_string(),
    }
}

/// TS 的 `byte()`：`Math.round(min(255, max(0, n)))`。
/// 夹紧后非负，`f64::round`（远离零）与 `Math.round`（朝 +∞）在非负数上相同。
///
/// 全文的夹紧都用 `clamp`：它与 `Math.min(hi, Math.max(lo, x))` 一样让 NaN 穿过去
/// （`max().min()` 会把 NaN 变成 lo）。合法输入算不出 NaN，这只是不引入新的分叉。
fn byte(n: f64) -> u8 {
    n.clamp(0.0, 255.0).round() as u8
}

/// 0–255 的浮点分量（OKLab 回来的、还没取整的 sRGB）。
/// TS 的 `oklabToRgb` 返回的就是这个，`toHex` 时才取整。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RgbF {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl RgbF {
    /// `toHex` 的取整：每个分量夹到 0–255 再四舍五入
    pub fn to_rgb(self) -> Rgb {
        Rgb { r: byte(self.r), g: byte(self.g), b: byte(self.b) }
    }
}

fn srgb_to_linear(v: f64) -> f64 {
    let s = v / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f64) -> f64 {
    let c = v.clamp(0.0, 1.0);
    (if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0
}

/// WCAG 相对亮度 0–1
pub fn luminance(c: Rgb) -> f64 {
    0.2126 * srgb_to_linear(f64::from(c.r))
        + 0.7152 * srgb_to_linear(f64::from(c.g))
        + 0.0722 * srgb_to_linear(f64::from(c.b))
}

/// WCAG 对比度 1–21
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let la = luminance(a);
    let lb = luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Oklab {
    /// 感知亮度 0–1（TS 里叫 `L`）
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

pub fn rgb_to_oklab(c: Rgb) -> Oklab {
    let r = srgb_to_linear(f64::from(c.r));
    let g = srgb_to_linear(f64::from(c.g));
    let b = srgb_to_linear(f64::from(c.b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    Oklab {
        l: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
        a: 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
        b: 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
    }
}

/// OKLab → 线性 sRGB，**不截断**：出了 0–1 就说明这个颜色不在 sRGB 色域里
fn oklab_to_linear(c: Oklab) -> RgbF {
    let l = (c.l + 0.3963377774 * c.a + 0.2158037573 * c.b).powf(3.0);
    let m = (c.l - 0.1055613458 * c.a - 0.0638541728 * c.b).powf(3.0);
    let s = (c.l - 0.0894841775 * c.a - 1.291485548 * c.b).powf(3.0);
    RgbF {
        r: 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        g: -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        b: -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
    }
}

fn in_gamut(c: Oklab) -> bool {
    let RgbF { r, g, b } = oklab_to_linear(c);
    let ok = |v: f64| (-1e-4..=1.0 + 1e-4).contains(&v);
    ok(r) && ok(g) && ok(b)
}

/// OKLab → 0–255 的浮点 sRGB（线性值截断到 0–1 后再做伽马）
pub fn oklab_to_rgb(c: Oklab) -> RgbF {
    let lin = oklab_to_linear(c);
    RgbF { r: linear_to_srgb(lin.r), g: linear_to_srgb(lin.g), b: linear_to_srgb(lin.b) }
}

/// OKLab 感知亮度 0–1，比 WCAG 亮度更接近"看起来有多亮"，用来判主题深浅之外的排序
pub fn perceptual_lightness(c: Rgb) -> f64 {
    rgb_to_oklab(c).l
}

/// 在 OKLab 里从 a 往 b 走 t（0–1，夹紧）。t=0 回 a，t=1 回 b。
/// 结果超出 sRGB 色域时按分量截断——两端都在色域里时几乎不会发生。
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let k = t.clamp(0.0, 1.0);
    if k == 0.0 {
        return a;
    }
    if k == 1.0 {
        return b;
    }
    let la = rgb_to_oklab(a);
    let lb = rgb_to_oklab(b);
    oklab_to_rgb(Oklab {
        l: la.l + (lb.l - la.l) * k,
        a: la.a + (lb.a - la.a) * k,
        b: la.b + (lb.b - la.b) * k,
    })
    .to_rgb()
}

/// 只动 OKLab 的亮度、不碰色度：把一块底色压暗 / 提亮，又不把它的色调洗成灰。
/// 往黑掺会同时拉低 a/b，Solarized 的暖白、Catppuccin 的紫灰压两下就变中性灰了；
/// 窗口底是整屏最大的一块颜色，主题的性格必须留在上面（ADR 0011）。
pub fn shift_lightness(c: Rgb, delta: f64) -> Rgb {
    let lab = rgb_to_oklab(c);
    let l = (lab.l + delta).clamp(0.0, 1.0);
    if in_gamut(Oklab { l, a: lab.a, b: lab.b }) {
        return oklab_to_rgb(Oklab { l, a: lab.a, b: lab.b }).to_rgb();
    }
    // 高饱和的底色（Borland 那种纯蓝）压暗会掉出 sRGB，直接截断分量就等于没压够亮度
    // （实测只降了 0.033，肉眼分不出——这个 bug 是测试先发现的）。二分把色度收到刚好
    // 回色域里：亮度优先保住——层次是靠亮度差看出来的，代价是一点饱和度。
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if in_gamut(Oklab { l, a: lab.a * mid, b: lab.b * mid }) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    oklab_to_rgb(Oklab { l, a: lab.a * lo, b: lab.b * lo }).to_rgb()
}

/// 保证 color 在 against 上至少有 min 的对比度：不够就往 toward 掺，掺到刚好够为止。
/// 掺色而不是单独拉亮度，是为了永远待在色域里、又不至于把色相洗掉太多。
/// toward 自己都不够对比度时返回 toward——已经是能做到的极限。
pub fn ensure_contrast(color: Rgb, against: Rgb, min: f64, toward: Rgb) -> Rgb {
    if contrast(color, against) >= min {
        return color;
    }
    if contrast(toward, against) < min {
        return toward;
    }
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if contrast(mix(color, toward, mid), against) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(color, toward, hi)
}

/// 从候选里挑第一个在 against 上够 min 对比度的；都不够就挑对比度最高的（并列取靠前的）。
/// 顺序有意义：调用方把"更想要的"（通常是普通色而不是亮色）排在前面。
///
/// # Panics
/// `candidates` 为空时 panic（TS 那边是 `candidates[0]!`，同样不允许空）。
pub fn pick_readable(candidates: &[Rgb], against: Rgb, min: f64) -> Rgb {
    let mut best = candidates[0];
    let mut best_ratio = -1.0;
    for &c in candidates {
        let ratio = contrast(c, against);
        if ratio >= min {
            return c;
        }
        if ratio > best_ratio {
            best = c;
            best_ratio = ratio;
        }
    }
    best
}

/// 两个候选里在 against 上对比度更高的那个（并列取 a）。按钮字色在按钮底上选黑还是白这类。
pub fn more_readable(a: Rgb, b: Rgb, against: Rgb) -> Rgb {
    if contrast(a, against) >= contrast(b, against) { a } else { b }
}

/// 移植自 `color.test.ts`，用例与断言值一一对应。
#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Rgb {
        Rgb::parse_hex(s).unwrap_or_else(|| panic!("测试里写错了颜色 {s}"))
    }

    // describe("hex")

    /// 3 / 6 / 8 位都认，非法 undefined
    #[test]
    fn hex_parses_3_6_8_digits() {
        assert_eq!(parse_hex("#fff"), Some(Rgba { r: 255, g: 255, b: 255, a: 255 }));
        assert_eq!(parse_hex("#fff").unwrap().alpha(), 1.0);
        assert_eq!(parse_hex(" #1e1e2e "), Some(Rgba { r: 30, g: 30, b: 46, a: 255 }));
        assert_eq!(parse_hex("#ffffff80").unwrap().alpha(), 128.0 / 255.0);
        assert_eq!(parse_hex("1e1e2e"), None);
        assert_eq!(parse_hex("#12345"), None);
    }

    /// normalizeHex 小写六位、丢 alpha，非法原样
    #[test]
    fn normalize_hex_lowercases_and_drops_alpha() {
        assert_eq!(normalize_hex("#ABC"), "#aabbcc");
        assert_eq!(normalize_hex("#ffffff80"), "#ffffff");
        assert_eq!(normalize_hex("red"), "red");
    }

    /// withAlpha 追加两位
    #[test]
    fn with_alpha_appends_two_digits() {
        assert_eq!(hex("#ffffff").with_alpha(0.1).to_hex(), "#ffffff1a");
        assert_eq!(hex("#000").with_alpha(1.0).to_hex(), "#000000ff");
    }

    // describe("luminance / contrast")

    /// 黑白 21:1，同色 1:1
    #[test]
    fn contrast_black_white_is_21() {
        assert_eq!(luminance(hex("#ffffff")), 1.0);
        assert_eq!(luminance(hex("#000000")), 0.0);
        assert_eq!(contrast(hex("#000000"), hex("#ffffff")), 21.0);
        assert_eq!(contrast(hex("#ffffff"), hex("#000000")), 21.0);
        assert_eq!(contrast(hex("#808080"), hex("#808080")), 1.0);
    }

    /// shadcn 浅色 muted-foreground 在白底上约 4.7:1
    #[test]
    fn contrast_shadcn_muted_foreground() {
        let c = contrast(hex("#737373"), hex("#ffffff"));
        assert!(c > 4.6 && c < 4.8, "{c}");
    }

    // describe("oklab")

    /// 往返恒等
    #[test]
    fn oklab_roundtrip() {
        for h in ["#000000", "#ffffff", "#1e1e2e", "#f38ba8", "#005661", "#f6edda"] {
            assert_eq!(oklab_to_rgb(rgb_to_oklab(hex(h))).to_rgb().to_hex(), h);
        }
    }

    /// 白 L≈1、黑 L≈0
    #[test]
    fn oklab_white_and_black_lightness() {
        assert!((perceptual_lightness(hex("#ffffff")) - 1.0).abs() < 0.001);
        assert!(perceptual_lightness(hex("#000000")).abs() < 0.001);
    }

    // describe("mix")

    /// 两端恒等，中间在两者之间
    #[test]
    fn mix_endpoints_and_middle() {
        assert_eq!(mix(hex("#0a0a0a"), hex("#fafafa"), 0.0).to_hex(), "#0a0a0a");
        assert_eq!(mix(hex("#0a0a0a"), hex("#fafafa"), 1.0).to_hex(), "#fafafa");
        let mid = perceptual_lightness(mix(hex("#0a0a0a"), hex("#fafafa"), 0.5));
        assert!(mid > 0.5 && mid < 0.6, "{mid}");
    }

    /// 复现 shadcn neutral：白底往近黑掺 3.5% ≈ oklch(0.97)
    #[test]
    fn mix_reproduces_shadcn_light_muted() {
        let l = perceptual_lightness(mix(hex("#ffffff"), hex("#171717"), 0.035));
        assert!((l - 0.97).abs() < 0.01, "{l}");
    }

    /// 复现 shadcn neutral：黑底往近白掺 7% ≈ oklch(0.205)
    #[test]
    fn mix_reproduces_shadcn_dark_card() {
        let l = perceptual_lightness(mix(hex("#0a0a0a"), hex("#fafafa"), 0.07));
        assert!((l - 0.205).abs() < 0.01, "{l}");
    }

    /// 非法输入原样返回 a——Rust 里非法颜色进不了 `mix`，场景落在解析这一步
    #[test]
    fn mix_invalid_input_is_unrepresentable() {
        assert_eq!(Rgb::parse_hex("nope"), None);
    }

    // describe("ensureContrast")

    /// 够就不动
    #[test]
    fn ensure_contrast_keeps_enough() {
        assert_eq!(ensure_contrast(hex("#cd3131"), hex("#ffffff"), 3.0, hex("#000000")).to_hex(), "#cd3131");
    }

    /// 不够就往 toward 掺到刚好够
    #[test]
    fn ensure_contrast_mixes_just_enough() {
        let out = ensure_contrast(hex("#b5ba00"), hex("#ffffff"), 3.0, hex("#171717"));
        assert!(contrast(out, hex("#ffffff")) >= 3.0, "{out}");
        // 只掺到刚好，不会直接变成 toward
        assert!(contrast(out, hex("#ffffff")) < 4.0, "{out}");
        assert_ne!(out.to_hex(), "#171717");
    }

    /// toward 自己都不够就返回 toward
    #[test]
    fn ensure_contrast_returns_toward_when_hopeless() {
        assert_eq!(ensure_contrast(hex("#eeeeee"), hex("#ffffff"), 4.5, hex("#cccccc")).to_hex(), "#cccccc");
    }

    // describe("pickReadable / moreReadable")

    /// 第一个够的优先
    #[test]
    fn pick_readable_first_enough_wins() {
        assert_eq!(pick_readable(&[hex("#cd3131"), hex("#ff0000")], hex("#ffffff"), 4.5).to_hex(), "#cd3131");
    }

    /// 第一个不够、第二个够就取第二个
    #[test]
    fn pick_readable_falls_to_second() {
        assert_eq!(pick_readable(&[hex("#c50f1f"), hex("#e74856")], hex("#0c0c0c"), 4.5).to_hex(), "#e74856");
    }

    /// 都不够取对比度最高的
    #[test]
    fn pick_readable_best_when_none_enough() {
        assert_eq!(pick_readable(&[hex("#eeeeee"), hex("#dddddd")], hex("#ffffff"), 4.5).to_hex(), "#dddddd");
    }

    /// moreReadable 选黑白
    #[test]
    fn more_readable_picks_black_or_white() {
        assert_eq!(more_readable(hex("#ffffff"), hex("#000000"), hex("#cd3131")).to_hex(), "#ffffff");
        assert_eq!(more_readable(hex("#1e1e2e"), hex("#cdd6f4"), hex("#f38ba8")).to_hex(), "#1e1e2e");
    }

    // describe("shiftLightness")

    /// 只动亮度，色度原样留着
    #[test]
    fn shift_lightness_keeps_chroma() {
        // Solarized Light 的暖米底压暗后还得是暖的——往黑掺会把 a/b 一起拉平
        let warm = hex("#fdf6e3");
        let shaded = shift_lightness(warm, -0.05);
        let a = rgb_to_oklab(warm);
        let b = rgb_to_oklab(shaded);
        assert!((b.l - (a.l - 0.05)).abs() < 0.004, "{shaded} L={}", b.l);
        assert!((b.a - a.a).abs() < 0.004 && (b.b - a.b).abs() < 0.004, "{shaded}");
    }

    /// 往黑掺会顺带洗掉色度，压同样多的亮度时差得出来
    #[test]
    fn shift_lightness_vs_mixing_black() {
        // Catppuccin Mocha 的紫灰底：压到同一个亮度，掺黑的那份蓝紫掉了三成
        let bg = hex("#1e1e2e");
        let before = rgb_to_oklab(bg);
        let shifted = rgb_to_oklab(shift_lightness(bg, -0.07));
        let mixed = rgb_to_oklab(mix(bg, hex("#000000"), 0.29));
        assert!((shifted.l - mixed.l).abs() < 0.01, "{} vs {}", shifted.l, mixed.l);
        assert!((shifted.b - before.b).abs() < 0.004, "{} vs {}", shifted.b, before.b);
        assert!(mixed.b.abs() < before.b.abs() * 0.8, "{} vs {}", mixed.b, before.b);
    }

    /// 两端截断，不出 0–1
    #[test]
    fn shift_lightness_clamps() {
        assert_eq!(shift_lightness(hex("#ffffff"), 0.5).to_hex(), "#ffffff");
        assert_eq!(shift_lightness(hex("#000000"), -0.5).to_hex(), "#000000");
        // TS 的 shiftLightness("not-a-color") 原样返回；这边非法颜色进不来
        assert_eq!(Rgb::parse_hex("not-a-color"), None);
    }
}
