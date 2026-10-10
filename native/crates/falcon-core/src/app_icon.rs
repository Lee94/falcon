//! 应用图标（ADR 0018）：shared 的 `appIcon.ts` 里客户端用得到的部分，加上原生独有的
//! Dock 版式运算（像素级的纯函数；解码 / 缩放 / 编码在 falcon-app，那里有 image crate）。

/// 内置图标，顺序即设置里的排列顺序。与 native/xtask/src/icons.rs 一一对应
pub const APP_ICON_IDS: [&str; 8] = [
    "emberwing",
    "voltwing",
    "voltwing-spark",
    "voltwing-night",
    "voltwing-ivory",
    "glyph",
    "flash",
    "thunderbird",
];

pub const DEFAULT_APP_ICON: &str = "emberwing";
pub const CUSTOM: &str = "custom";

/// 自定义图标规整成这个边长的正方形 PNG 再上传（shared 的 `APP_ICON_CUSTOM_SIZE`）
pub const APP_ICON_CUSTOM_SIZE: u32 = 512;

pub fn is_builtin(id: &str) -> bool {
    APP_ICON_IDS.contains(&id)
}

/// 界面文案的 key：`appIcon.name_voltwing_spark`（web 的 `name_${id.replace(/-/g, "_")}`）
pub fn label_key(id: &str) -> String {
    format!("appIcon.name_{}", id.replace('-', "_"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// web `containRect`：等比缩放到长边贴边、居中，不裁切；宽高为 0 按正方形铺满。
/// 取整与 JS 的 `Math.round` 一致（正数的 .5 向上）。
pub fn contain_rect(w: u32, h: u32, size: u32) -> Rect {
    if w == 0 || h == 0 {
        return Rect { x: 0, y: 0, w: size, h: size };
    }
    let k = size as f64 / w.max(h) as f64;
    let dw = (w as f64 * k + 0.5).floor() as u32;
    let dh = (h as f64 * k + 0.5).floor() as u32;
    Rect {
        x: ((size - dw) as f64 / 2. + 0.5).floor() as u32,
        y: ((size - dh) as f64 / 2. + 0.5).floor() as u32,
        w: dw,
        h: dh,
    }
}

/// macOS 图标网格（Big Sur 起）：1024 画布上 824 见方的圆角块，四周 100 的透明边，圆角 185。
/// native/xtask/src/icons.rs 的 `macos()` 用的是同一组数
pub const MAC_INSET: f64 = 100. / 1024.;
pub const MAC_RADIUS: f64 = 185. / 1024.;

/// 图是不是自带形状：四个角（往里收 2%，躲开边上的抗锯齿）都几乎透明。
///
/// 自带形状的（按 macOS 网格画好的、圆形的 logo）原样放进 Dock；满版方块套一层 macOS 版式，
/// 否则在 Dock 里比别的应用大一圈、还是直角。`rgba` 是 w×h 的直通 RGBA。
pub fn has_own_shape(rgba: &[u8], w: u32, h: u32) -> bool {
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return false;
    }
    let dx = (w / 50).min(w - 1);
    let dy = (h / 50).min(h - 1);
    let corners = [(dx, dy), (w - 1 - dx, dy), (dx, h - 1 - dy), (w - 1 - dx, h - 1 - dy)];
    corners.iter().all(|&(x, y)| rgba[((y * w + x) * 4 + 3) as usize] < 16)
}

/// 圆角正方形（左上角 `(x0, y0)`、边长 `side`、圆角 `r`）在像素 `(px, py)` 的覆盖率，0–1。
/// 取像素中心到形状边界的有符号距离，边上 1 像素做线性过渡（抗锯齿）。
pub fn rounded_square_coverage(px: u32, py: u32, x0: f64, y0: f64, side: f64, r: f64) -> f64 {
    let cx = px as f64 + 0.5;
    let cy = py as f64 + 0.5;
    // 以形状中心为原点折到第一象限，按「内缩 r 的矩形 + 半径 r」算有符号距离
    let half = side / 2.;
    let qx = (cx - (x0 + half)).abs() - (half - r);
    let qy = (cy - (y0 + half)).abs() - (half - r);
    let outside = (qx.max(0.).powi(2) + qy.max(0.).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.);
    let dist = outside + inside - r;
    (0.5 - dist).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_rect_matches_web() {
        // 与 web lib/appIcon.test.ts 的 containRect 用例逐条对应
        assert_eq!(contain_rect(1000, 500, 512), Rect { x: 0, y: 128, w: 512, h: 256 });
        assert_eq!(contain_rect(300, 600, 512), Rect { x: 128, y: 0, w: 256, h: 512 });
        assert_eq!(contain_rect(64, 64, 512), Rect { x: 0, y: 0, w: 512, h: 512 });
        assert_eq!(contain_rect(0, 0, 512), Rect { x: 0, y: 0, w: 512, h: 512 });
    }

    #[test]
    fn label_keys_follow_web() {
        assert_eq!(label_key("voltwing-spark"), "appIcon.name_voltwing_spark");
        assert!(is_builtin("thunderbird"));
        assert!(!is_builtin(CUSTOM));
    }

    fn solid(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [10, 20, 30, alpha]).collect()
    }

    #[test]
    fn full_bleed_square_has_no_own_shape() {
        assert!(!has_own_shape(&solid(64, 64, 255), 64, 64));
    }

    #[test]
    fn transparent_corners_mean_own_shape() {
        let mut px = solid(100, 100, 255);
        for &(x, y) in &[(2u32, 2u32), (97, 2), (2, 97), (97, 97)] {
            px[((y * 100 + x) * 4 + 3) as usize] = 0;
        }
        assert!(has_own_shape(&px, 100, 100));
        // 只透明一个角（比如一张缺角的照片）不算
        px[((2 * 100 + 97) * 4 + 3) as usize] = 255;
        assert!(!has_own_shape(&px, 100, 100));
    }

    #[test]
    fn short_buffer_is_not_shaped() {
        assert!(!has_own_shape(&[0; 4], 2, 2));
    }

    #[test]
    fn coverage_inside_outside_and_corner() {
        let (x0, side, r) = (50., 412., 92.5);
        assert_eq!(rounded_square_coverage(256, 256, x0, x0, side, r), 1.);
        assert_eq!(rounded_square_coverage(10, 256, x0, x0, side, r), 0.);
        // 外接正方形的角点在圆角外面
        assert_eq!(rounded_square_coverage(51, 51, x0, x0, side, r), 0.);
        // 直边附近：x=50 这一列整个像素在边内，x=49 整个在边外（过渡带宽 1 像素，以边为中心）
        let edge = rounded_square_coverage(50, 256, x0, x0, side, r);
        assert!((edge - 1.).abs() < 1e-9, "x=50 这一列的像素中心在边内 0.5：{edge}");
        let out = rounded_square_coverage(49, 256, x0, x0, side, r);
        assert!(out.abs() < 1e-9, "x=49 的像素中心在边外 0.5：{out}");
    }
}
