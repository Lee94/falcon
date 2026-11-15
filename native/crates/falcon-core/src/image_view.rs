//! 图片预览几何（ADR 0007）：fit / 1:1、指针锚点缩放与有界平移。
//! 与视图系统无关；调用方提供按界面缩放换算过的 padding。

pub const MIN_ZOOM: f64 = 0.05;
pub const MAX_ZOOM: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageGeometry {
    pub viewport: (f64, f64),
    pub natural: (f64, f64),
    pub padding: f64,
    /// None 为 fit，小图不放大。
    pub zoom: Option<f64>,
    pub scroll: (f64, f64),
}

impl ImageGeometry {
    pub fn fit(self) -> f64 {
        let (w, h) = self.natural;
        if w <= 0.0 || h <= 0.0 || self.viewport.0 <= 0.0 || self.viewport.1 <= 0.0 {
            return 1.0;
        }
        ((self.viewport.0 - self.padding * 2.0).max(1.0) / w)
            .min((self.viewport.1 - self.padding * 2.0).max(1.0) / h)
            .min(1.0)
    }

    pub fn scale(self) -> f64 {
        self.zoom.unwrap_or_else(|| self.fit())
    }

    pub fn content(self) -> (f64, f64) {
        (
            self.viewport
                .0
                .max(self.natural.0 * self.scale() + self.padding * 2.0),
            self.viewport
                .1
                .max(self.natural.1 * self.scale() + self.padding * 2.0),
        )
    }

    pub fn origin(self) -> (f64, f64) {
        let content = self.content();
        (
            (content.0 - self.natural.0 * self.scale()) / 2.0 - self.scroll.0,
            (content.1 - self.natural.1 * self.scale()) / 2.0 - self.scroll.1,
        )
    }

    pub fn pan(&mut self, scroll: (f64, f64)) {
        let content = self.content();
        self.scroll = (
            scroll.0.clamp(0.0, (content.0 - self.viewport.0).max(0.0)),
            scroll.1.clamp(0.0, (content.1 - self.viewport.1).max(0.0)),
        );
    }

    pub fn zoom_to(&mut self, zoom: Option<f64>, at: (f64, f64)) {
        if zoom.is_some_and(|s| !s.is_finite()) {
            return;
        }
        let origin = self.origin();
        let old = self.scale().max(f64::EPSILON);
        let pixel = ((at.0 - origin.0) / old, (at.1 - origin.1) / old);
        self.zoom = zoom.map(|s| s.clamp(MIN_ZOOM, MAX_ZOOM));
        let content = self.content();
        // 先求保持原图像素不动的滚动量，再按新内容盒夹边界。
        self.pan((
            (content.0 - self.natural.0 * self.scale()) / 2.0 - at.0 + pixel.0 * self.scale(),
            (content.1 - self.natural.1 * self.scale()) / 2.0 - at.1 + pixel.1 * self.scale(),
        ));
    }

    pub fn toggle_actual(&mut self, at: (f64, f64)) {
        self.zoom_to(
            if self.zoom.is_none() && self.fit() < 1.0 {
                Some(1.0)
            } else {
                None
            },
            at,
        );
    }

    pub fn center(self) -> (f64, f64) {
        (self.viewport.0 / 2.0, self.viewport.1 / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> ImageGeometry {
        ImageGeometry {
            viewport: (848.0, 648.0),
            natural: (1600.0, 1200.0),
            padding: 24.0,
            zoom: None,
            scroll: (0.0, 0.0),
        }
    }

    #[test]
    fn fit_centers_without_enlarging_small_images() {
        let mut g = geometry();
        assert_eq!(g.fit(), 0.5);
        assert_eq!(g.origin(), (24.0, 24.0));
        g.natural = (16.0, 16.0);
        assert_eq!(g.fit(), 1.0);
        assert_eq!(g.origin(), (416.0, 316.0));
        g.viewport = (0.0, 0.0);
        assert_eq!(g.fit(), 1.0);
    }

    #[test]
    fn zoom_keeps_the_image_pixel_under_the_pointer() {
        let mut g = geometry();
        let at = (230.0, 170.0);
        let before = (
            (at.0 - g.origin().0) / g.scale(),
            (at.1 - g.origin().1) / g.scale(),
        );
        g.zoom_to(Some(2.0), at);
        let after = (
            (at.0 - g.origin().0) / g.scale(),
            (at.1 - g.origin().1) / g.scale(),
        );
        assert!((before.0 - after.0).abs() < 1e-8);
        assert!((before.1 - after.1).abs() < 1e-8);
        g.pan((f64::MAX, -100.0));
        assert_eq!(g.scroll, (g.content().0 - g.viewport.0, 0.0));
    }

    #[test]
    fn double_click_toggles_fit_actual_and_resets_scroll() {
        let mut g = geometry();
        g.toggle_actual(g.center());
        assert_eq!(g.zoom, Some(1.0));
        assert!(g.scroll.0 > 0.0);
        g.toggle_actual(g.center());
        assert_eq!(g.zoom, None);
        assert_eq!(g.scroll, (0.0, 0.0));
        g.zoom_to(Some(2.0), g.center());
        g.toggle_actual(g.center());
        assert_eq!(g.zoom, None);
    }

    #[test]
    fn explicit_zoom_is_clamped_and_invalid_input_is_ignored() {
        let mut g = geometry();
        g.zoom_to(Some(1000.0), g.center());
        assert_eq!(g.scale(), MAX_ZOOM);
        g.zoom_to(Some(-1.0), g.center());
        assert_eq!(g.scale(), MIN_ZOOM);
        let before = g;
        g.zoom_to(Some(f64::NAN), g.center());
        assert_eq!(g, before);
        g.zoom_to(Some(f64::INFINITY), g.center());
        assert_eq!(g, before);
    }

    #[test]
    fn resize_preserves_zoom_and_clamps_obsolete_scroll() {
        let mut g = geometry();
        g.zoom_to(Some(1.0), g.center());
        g.pan((500.0, 400.0));
        g.viewport = (3000.0, 2000.0);
        g.pan(g.scroll);
        assert_eq!(g.scale(), 1.0);
        assert_eq!(g.scroll, (0.0, 0.0));
    }
}
