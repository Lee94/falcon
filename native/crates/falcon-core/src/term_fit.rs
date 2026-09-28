//! 终端格子能不能发给 PTY。对应 web 的 `lib/termFit.ts`。
//!
//! web 那边 FitAddon 在 renderer 还没量出 cell 时给 undefined，容器高度还是 0 时会算出
//! 2×1（addon 下限），xterm 自身默认 80×24——刷新时这几种都会在字体 / 布局就绪前冒出来，
//! 发给 PTY 就会把会话挤扁。原生这边字体度量没就绪、元素还没排版时同样会算出这些
//! 残骸，用同一道闸拦。

/// 容器的像素尺寸
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermBox {
    pub width: f64,
    pub height: f64,
}

/// 按格子度量算出来的行列（可能是 NaN：度量还没出来时除出来的）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermDims {
    pub cols: f64,
    pub rows: f64,
}

/// 容器已经撑开，却只够 1 行：格子高度还没量出来（FitAddon 下限就是 1）
const TALL_ENOUGH_FOR_TWO_ROWS: f64 = 48.0;

pub fn is_usable_term_size(proposed: Option<TermDims>, bx: TermBox) -> bool {
    if bx.width <= 0.0 || bx.height <= 0.0 {
        return false;
    }
    let Some(TermDims { cols, rows }) = proposed else { return false };
    if !cols.is_finite() || !rows.is_finite() {
        return false;
    }
    if cols < 2.0 || rows < 1.0 {
        return false;
    }
    if bx.height >= TALL_ENOUGH_FOR_TWO_ROWS && rows < 2.0 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANE: TermBox = TermBox { width: 960.0, height: 640.0 };

    fn dims(cols: f64, rows: f64) -> Option<TermDims> {
        Some(TermDims { cols, rows })
    }

    #[test]
    fn rejects_missing_dimensions() {
        assert!(!is_usable_term_size(None, PANE));
    }

    #[test]
    fn rejects_0_size_container() {
        assert!(!is_usable_term_size(dims(80.0, 24.0), TermBox { width: 960.0, height: 0.0 }));
        assert!(!is_usable_term_size(dims(80.0, 24.0), TermBox { width: 0.0, height: 640.0 }));
    }

    #[test]
    fn rejects_nan_and_lower_bound_leftovers() {
        assert!(!is_usable_term_size(dims(f64::NAN, 24.0), PANE));
        assert!(!is_usable_term_size(dims(150.0, 1.0), PANE));
        assert!(!is_usable_term_size(dims(1.0, 24.0), PANE));
    }

    #[test]
    fn accepts_a_real_fit() {
        assert!(is_usable_term_size(dims(128.0, 40.0), PANE));
        assert!(is_usable_term_size(dims(80.0, 24.0), PANE));
    }
}
