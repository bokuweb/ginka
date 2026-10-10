//! Image zoom and pan geometry, independent of rendering and image decoding.

/// Image rectangle in viewport pixels, including any cropped overflow.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImageRect {
    /// Horizontal offset from the viewport's left edge.
    pub x: f32,
    /// Vertical offset from the viewport's top edge.
    pub y: f32,
    /// Displayed width before clipping.
    pub width: f32,
    /// Displayed height before clipping.
    pub height: f32,
}

/// Bounded zoom relative to fit, with pan constrained to the image's edges.
pub struct ImageViewport {
    image: (f32, f32),
    viewport: (f32, f32),
    zoom: f32,
    rect: ImageRect,
}

impl ImageViewport {
    /// Start at fit scale. Invalid intrinsic dimensions use a one-pixel fallback.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            image: (
                positive(width).unwrap_or(1.),
                positive(height).unwrap_or(1.),
            ),
            viewport: (0., 0.),
            zoom: 1.,
            rect: ImageRect::default(),
        }
    }

    /// Current scale as a multiple of the fitted image size, from one to sixteen.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Current image placement in viewport pixels.
    pub fn rect(&self) -> ImageRect {
        self.rect
    }

    /// Resize while retaining the image coordinate under the viewport center.
    /// Zero or non-finite layout measurements are ignored.
    pub fn resize(&mut self, width: f32, height: f32) {
        if positive(width).is_none() || positive(height).is_none() {
            return;
        }
        let (old_width, old_height) = self.viewport;
        let anchor = if self.rect.width > 0. && self.rect.height > 0. {
            (
                (old_width / 2. - self.rect.x) / self.rect.width,
                (old_height / 2. - self.rect.y) / self.rect.height,
            )
        } else {
            (0.5, 0.5)
        };
        self.viewport = (width, height);
        self.place(anchor, (width / 2., height / 2.));
    }

    /// Multiply zoom while keeping the image coordinate under the cursor fixed.
    /// Edge clamping takes precedence when an anchor cannot remain in bounds.
    pub fn zoom_at(&mut self, factor: f32, x: f32, y: f32) {
        if positive(factor).is_none()
            || !x.is_finite()
            || !y.is_finite()
            || self.rect.width <= 0.
            || self.rect.height <= 0.
        {
            return;
        }
        let anchor = (
            (x - self.rect.x) / self.rect.width,
            (y - self.rect.y) / self.rect.height,
        );
        self.zoom = (self.zoom * factor).clamp(1., 16.);
        self.place(anchor, (x, y));
    }

    /// Pan in viewport pixels, keeping a smaller image centered on each axis.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.rect.x = offset(self.rect.x + dx, self.rect.width, self.viewport.0);
        self.rect.y = offset(self.rect.y + dy, self.rect.height, self.viewport.1);
    }

    /// Restore fit and center without changing intrinsic or viewport dimensions.
    pub fn reset(&mut self) {
        self.zoom = 1.;
        self.place((0.5, 0.5), (self.viewport.0 / 2., self.viewport.1 / 2.));
    }

    fn place(&mut self, anchor: (f32, f32), cursor: (f32, f32)) {
        let scale =
            (self.viewport.0 / self.image.0).min(self.viewport.1 / self.image.1) * self.zoom;
        self.rect.width = self.image.0 * scale;
        self.rect.height = self.image.1 * scale;
        self.rect.x = offset(
            cursor.0 - anchor.0 * self.rect.width,
            self.rect.width,
            self.viewport.0,
        );
        self.rect.y = offset(
            cursor.1 - anchor.1 * self.rect.height,
            self.rect.height,
            self.viewport.1,
        );
    }
}

fn positive(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.).then_some(value)
}

fn offset(value: f32, image: f32, viewport: f32) -> f32 {
    if image <= viewport {
        (viewport - image) / 2.
    } else {
        value.clamp(viewport - image, 0.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} != {b}");
    }

    #[test]
    fn cursor_anchor_survives_zoom_when_not_at_an_edge() {
        let mut view = ImageViewport::new(800., 600.);
        view.resize(400., 300.);
        view.zoom_at(2., 200., 150.);
        let before = view.rect();
        let x = (170. - before.x) / before.width;
        let y = (140. - before.y) / before.height;
        view.zoom_at(2., 170., 140.);
        let after = view.rect();
        close((170. - after.x) / after.width, x);
        close((140. - after.y) / after.height, y);
    }

    #[test]
    fn pan_stops_at_edges_and_fit_recenters_a_portrait() {
        let mut view = ImageViewport::new(100., 200.);
        view.resize(400., 300.);
        close(view.rect().x, 125.);
        view.pan(1000., -1000.);
        close(view.rect().x, 125.);
        close(view.rect().y, 0.);
        view.zoom_at(4., 200., 150.);
        view.pan(1000., -1000.);
        close(view.rect().x, 0.);
        close(view.rect().y, -900.);
        view.reset();
        close(view.zoom(), 1.);
        close(view.rect().x, 125.);
        close(view.rect().y, 0.);
    }

    #[test]
    fn zoom_is_bounded_and_non_finite_inputs_cannot_poison_geometry() {
        let mut view = ImageViewport::new(800., 600.);
        view.resize(400., 300.);
        view.zoom_at(f32::INFINITY, 0., 0.);
        close(view.zoom(), 1.);
        view.zoom_at(1000., 200., 150.);
        close(view.zoom(), 16.);
        view.zoom_at(0.001, 200., 150.);
        close(view.zoom(), 1.);
        view.pan(f32::NAN, 0.);
        view.resize(f32::NAN, 10.);
        assert!(view.rect().x.is_finite());
        close(view.rect().width, 400.);
    }

    #[test]
    fn resizing_retains_zoom_and_the_center_image_coordinate() {
        let mut view = ImageViewport::new(800., 600.);
        view.resize(400., 300.);
        view.zoom_at(4., 200., 150.);
        view.pan(-120., -90.);
        let before = view.rect();
        let x = (200. - before.x) / before.width;
        let y = (150. - before.y) / before.height;
        view.resize(800., 600.);
        let after = view.rect();
        close(view.zoom(), 4.);
        close((400. - after.x) / after.width, x);
        close((300. - after.y) / after.height, y);
    }
}
