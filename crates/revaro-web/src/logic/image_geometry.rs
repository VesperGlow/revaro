//! Image preview pan and zoom maths.
//!
//! Port of `web/src/imageGeometry.ts`. These functions are tiny but the e2e
//! suite asserts their results pixel-exactly (fit scale, pointer-anchored zoom,
//! pinch anchor tracking, letterbox centring and edge clamping), so the
//! arithmetic below is a literal transcription rather than a tidied-up version.
//!
//! Keeping them target-independent matters: the same numbers are needed
//! natively by the unit tests and in the wasm build by `MediaPreview`.

/// A point in stage coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position.
    pub y: f64,
}

/// An image or stage size in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    /// Width in CSS pixels.
    pub width: f64,
    /// Height in CSS pixels.
    pub height: f64,
}

/// Shrink an image to fit a stage with a 16 px gutter on each axis.
///
/// The stage is inset by 32 px before scaling, and `max(1, …)` on the usable
/// extent stops a degenerate (smaller than the gutter) stage from producing a
/// zero or negative ratio. `min(1, …)` is what keeps small originals at 100 %:
/// the preview never upscales to fill the stage.
#[must_use]
pub fn fit_image(image: Size, stage: Size) -> Size {
    if image.width <= 0.0 || image.height <= 0.0 {
        return Size {
            width: 0.0,
            height: 0.0,
        };
    }
    let ratio = 1.0_f64
        .min((stage.width - 32.0).max(1.0) / image.width)
        .min((stage.height - 32.0).max(1.0) / image.height);
    Size {
        width: image.width * ratio,
        height: image.height * ratio,
    }
}

/// Clamp a pan offset so the image cannot be dragged off the stage.
///
/// The allowed range is the space each axis has outside the stage once scaled
/// (never negative, which keeps a letterboxed axis pinned at centre). The
/// comparison is written as `min`/`max` rather than `f64::clamp` on purpose:
/// stage measurements taken before layout can be `NaN`, and `clamp` panics on a
/// `NaN` bound while the original `Math.min`/`Math.max` silently propagates one.
#[must_use]
pub fn clamp_image_pan(pan: Point, image: Size, stage: Size, zoom: f64) -> Point {
    let x = ((image.width * zoom - stage.width) / 2.0).max(0.0);
    let y = ((image.height * zoom - stage.height) / 2.0).max(0.0);
    Point {
        x: pan.x.min(x).max(-x),
        y: pan.y.min(y).max(-y),
    }
}

/// Pan so the image point under the pointer (or pinch midpoint) stays put when
/// the zoom factor changes from `from` to `to`.
///
/// `ratio` is the *new* scale over the old one; `from` and `to` are the pointer
/// position before and after the gesture.
#[must_use]
pub fn zoom_image_pan(pan: Point, from: Point, to: Point, ratio: f64) -> Point {
    Point {
        x: to.x - (from.x - pan.x) * ratio,
        y: to.y - (from.y - pan.y) * ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f64, height: f64) -> Size {
        Size { width, height }
    }

    fn point(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    #[test]
    fn fits_portrait_and_landscape_images_without_enlarging_small_originals() {
        // Ported from `web/src/imageGeometry.test.ts`.
        assert_eq!(
            fit_image(size(200.0, 100.0), size(800.0, 600.0)),
            size(200.0, 100.0)
        );
        assert_eq!(
            fit_image(size(1600.0, 800.0), size(832.0, 632.0)),
            size(800.0, 400.0)
        );
        assert_eq!(
            fit_image(size(400.0, 1600.0), size(832.0, 632.0)),
            size(150.0, 600.0)
        );
    }

    #[test]
    fn degenerate_images_fit_to_nothing() {
        assert_eq!(
            fit_image(size(0.0, 100.0), size(800.0, 600.0)),
            size(0.0, 0.0)
        );
    }

    #[test]
    fn anchors_zoom_to_the_same_image_pixel_under_the_pointer() {
        // Ported from `web/src/imageGeometry.test.ts`.
        let before = point(30.0, -10.0);
        let pointer = point(100.0, 50.0);
        let after = zoom_image_pan(before, pointer, pointer, 2.0);
        assert_eq!((pointer.x - after.x) / 2.0, pointer.x - before.x);
        assert_eq!((pointer.y - after.y) / 2.0, pointer.y - before.y);
    }

    #[test]
    fn moves_the_pinch_anchor_with_the_fingers() {
        // Ported from `web/src/imageGeometry.test.ts`.
        assert_eq!(
            zoom_image_pan(point(0.0, 0.0), point(50.0, 40.0), point(70.0, 50.0), 2.0),
            point(-30.0, -30.0)
        );
    }

    #[test]
    fn keeps_letterboxed_axes_centered_and_clamps_against_the_image_edges() {
        // Ported from `web/src/imageGeometry.test.ts`.
        assert_eq!(
            clamp_image_pan(
                point(900.0, -900.0),
                size(200.0, 500.0),
                size(800.0, 600.0),
                2.0
            ),
            point(0.0, -200.0)
        );
        assert_eq!(
            clamp_image_pan(
                point(80.0, 100.0),
                size(400.0, 300.0),
                size(800.0, 600.0),
                1.0
            ),
            point(0.0, 0.0)
        );
    }
}
