//! Drawing the three capture regions on a screenshot.
//!
//! The exchange panel only exists while the game holds focus, so it cannot be
//! drawn on live — the previous flow put a fullscreen overlay on top and asked
//! the user to find the panel from memory. What they could actually do was
//! draw a rectangle that looked right and was not: a tables region starting
//! above the tables swallows the panel's title and market ratio, the row grid
//! anchors on that furniture, and twelve rows read as two with no error.
//!
//! So calibration happens against a still. The user has the screenshot already
//! — the panel is what they were looking at — and a still can be zoomed,
//! panned, and magnified until the rectangle is exact.
//!
//! Everything here is in *source pixels*: the screenshot's own coordinates,
//! which are the desktop's, which is what the capture layer wants. The view
//! transform exists only to put those pixels on screen.

use std::path::PathBuf;

/// Which of the three regions is being drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Need,
    Have,
    Tables,
}

impl Target {
    pub const ALL: [Self; 3] = [Self::Need, Self::Have, Self::Tables];

    pub const fn element_id(self) -> &'static str {
        match self {
            Self::Need => "cal-target-need",
            Self::Have => "cal-target-have",
            Self::Tables => "cal-target-tables",
        }
    }
}

/// A rectangle in the screenshot's own pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl SourceRect {
    /// Builds a rectangle from two corners in either order.
    ///
    /// Dragging up-left is as natural as down-right and produces negative
    /// extents; normalising here means no caller has to remember that.
    #[must_use]
    pub fn from_corners(a: (f32, f32), b: (f32, f32)) -> Option<Self> {
        let x = a.0.min(b.0).round();
        let y = a.1.min(b.1).round();
        let width = (a.0 - b.0).abs().round();
        let height = (a.1 - b.1).abs().round();
        // A click without a drag is not a region. Two pixels is below any
        // legible glyph, so anything smaller is a stray click rather than an
        // intent to select nothing.
        if width < 2.0 || height < 2.0 {
            return None;
        }
        Some(Self {
            x: x as i32,
            y: y as i32,
            width: width as u32,
            height: height as u32,
        })
    }
}

/// How the screenshot is placed on screen.
///
/// One scale and one offset, both applied to source pixels. Kept as a value
/// rather than folded into the view so the mapping can be tested without a
/// window.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

/// Zoom is clamped rather than free: below this a 2560px screenshot is
/// unreadable, above it a single pixel fills the canvas and panning becomes
/// the only control that matters.
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 8.0;
/// The loupe's own magnification, in screen pixels per source pixel.
///
/// Absolute rather than a multiple of the view. A multiple compounds: fitted
/// to the window a 2560px screenshot sits near 0.3, where four times that is
/// barely magnified at all, while at the 8x view limit it would ask the
/// renderer for a 32x transform. Six is where a single pixel becomes a square
/// the eye can aim at, which is the whole job.
pub const MAGNIFIER_ZOOM: f32 = 6.0;

impl View {
    /// Source pixel to position within the canvas.
    #[must_use]
    pub fn to_canvas(self, x: f32, y: f32) -> (f32, f32) {
        (self.pan_x + x * self.zoom, self.pan_y + y * self.zoom)
    }

    /// Position within the canvas to source pixel.
    #[must_use]
    pub fn to_source(self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.pan_x) / self.zoom, (y - self.pan_y) / self.zoom)
    }

    /// Zooms about a fixed point on the canvas.
    ///
    /// The point under the cursor must not move, or zooming walks the image
    /// off screen and the user spends their time re-finding the panel instead
    /// of drawing on it.
    #[must_use]
    pub fn zoomed_about(self, factor: f32, anchor_x: f32, anchor_y: f32) -> Self {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let (source_x, source_y) = self.to_source(anchor_x, anchor_y);
        Self {
            zoom,
            pan_x: anchor_x - source_x * zoom,
            pan_y: anchor_y - source_y * zoom,
        }
    }

    /// Fits an image of `size` inside a canvas of `canvas`, centred.
    #[must_use]
    pub fn fitted(size: (u32, u32), canvas: (f32, f32)) -> Self {
        if size.0 == 0 || size.1 == 0 {
            return Self::default();
        }
        let zoom = (canvas.0 / size.0 as f32)
            .min(canvas.1 / size.1 as f32)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        Self {
            zoom,
            pan_x: (canvas.0 - size.0 as f32 * zoom) / 2.0,
            pan_y: (canvas.1 - size.1 as f32 * zoom) / 2.0,
        }
    }
}

/// The calibration screen's whole state.
#[derive(Debug, Default)]
pub struct Calibration {
    pub image: Option<PathBuf>,
    /// The screenshot's pixel size, needed to fit and to clamp.
    pub image_size: Option<(u32, u32)>,
    pub view: Option<View>,
    pub target: Option<Target>,
    /// Where the current drag began, in source pixels.
    pub drag_from: Option<(f32, f32)>,
    /// Where a pan began, in canvas pixels.
    ///
    /// Canvas rather than source, because panning changes the very transform
    /// that would convert them — measuring the drag in source pixels would
    /// have the picture chase the cursor.
    pub pan_from: Option<(f32, f32)>,
    /// Where the cursor is, in source pixels, for the magnifier.
    pub cursor: Option<(f32, f32)>,
    pub need: Option<SourceRect>,
    pub have: Option<SourceRect>,
    pub tables: Option<SourceRect>,
    pub message: Option<String>,
}

impl Calibration {
    #[must_use]
    pub fn view(&self) -> View {
        self.view.unwrap_or_default()
    }

    #[must_use]
    pub fn target(&self) -> Target {
        self.target.unwrap_or(Target::Need)
    }

    #[must_use]
    pub const fn rect(&self, target: Target) -> Option<SourceRect> {
        match target {
            Target::Need => self.need,
            Target::Have => self.have,
            Target::Tables => self.tables,
        }
    }

    pub const fn set_rect(&mut self, target: Target, rect: SourceRect) {
        match target {
            Target::Need => self.need = Some(rect),
            Target::Have => self.have = Some(rect),
            Target::Tables => self.tables = Some(rect),
        }
    }

    /// Every region drawn, in the order they are applied.
    #[must_use]
    pub fn completed(&self) -> Vec<(Target, SourceRect)> {
        Target::ALL
            .into_iter()
            .filter_map(|target| self.rect(target).map(|rect| (target, rect)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_is_the_same_whichever_corner_started_it() {
        let down_right = SourceRect::from_corners((10.0, 20.0), (60.0, 90.0));
        let up_left = SourceRect::from_corners((60.0, 90.0), (10.0, 20.0));
        assert_eq!(down_right, up_left);
        assert_eq!(
            down_right,
            Some(SourceRect {
                x: 10,
                y: 20,
                width: 50,
                height: 70
            })
        );
    }

    #[test]
    fn a_click_without_a_drag_is_not_a_region() {
        assert_eq!(SourceRect::from_corners((10.0, 20.0), (10.0, 20.0)), None);
        assert_eq!(SourceRect::from_corners((10.0, 20.0), (11.0, 21.0)), None);
    }

    /// The mapping must survive a round trip, or a drawn rectangle lands
    /// somewhere other than where it was drawn — silently, because the
    /// rectangle still looks right on screen.
    #[test]
    fn canvas_and_source_are_inverses_at_every_zoom() {
        for zoom in [0.25_f32, 0.5, 1.0, 2.5, 8.0] {
            let view = View {
                zoom,
                pan_x: -137.0,
                pan_y: 48.5,
            };
            for point in [(0.0_f32, 0.0_f32), (1145.0, 349.0), (2559.0, 1439.0)] {
                let (cx, cy) = view.to_canvas(point.0, point.1);
                let (sx, sy) = view.to_source(cx, cy);
                assert!(
                    (sx - point.0).abs() < 0.01 && (sy - point.1).abs() < 0.01,
                    "zoom {zoom}: {point:?} became ({cx},{cy}) became ({sx},{sy})"
                );
            }
        }
    }

    /// Zooming must not move the pixel under the cursor.
    #[test]
    fn zooming_holds_the_point_under_the_cursor_still() {
        let view = View {
            zoom: 0.5,
            pan_x: 20.0,
            pan_y: 10.0,
        };
        let (anchor_x, anchor_y) = (400.0, 300.0);
        let before = view.to_source(anchor_x, anchor_y);
        for factor in [1.25_f32, 0.8, 4.0] {
            let after = view.zoomed_about(factor, anchor_x, anchor_y);
            let moved = after.to_source(anchor_x, anchor_y);
            assert!(
                (moved.0 - before.0).abs() < 0.01 && (moved.1 - before.1).abs() < 0.01,
                "factor {factor}: {before:?} became {moved:?}"
            );
        }
    }

    #[test]
    fn zoom_stays_inside_its_bounds() {
        let view = View::default();
        assert!((view.zoomed_about(1000.0, 0.0, 0.0).zoom - MAX_ZOOM).abs() < f32::EPSILON);
        assert!((view.zoomed_about(0.0001, 0.0, 0.0).zoom - MIN_ZOOM).abs() < f32::EPSILON);
    }

    /// Fitting must show the whole screenshot, centred.
    #[test]
    fn fitting_shows_all_of_it() {
        let view = View::fitted((2560, 1440), (1280.0, 500.0));
        assert!((view.zoom - 500.0 / 1440.0).abs() < 0.001, "{view:?}");
        let (right, bottom) = view.to_canvas(2560.0, 1440.0);
        assert!(right <= 1280.5 && bottom <= 500.5, "{right},{bottom}");
        let (left, top) = view.to_canvas(0.0, 0.0);
        assert!(left >= -0.5 && top >= -0.5, "{left},{top}");
    }
}

/// The pixel size of a PNG or JPEG, read from its header.
///
/// The whole transform depends on this: without the true size the image cannot
/// be fitted, and a rectangle drawn on a wrongly scaled picture lands
/// somewhere else on the desktop. Decoding the file just to measure it would
/// mean pulling image decoding into the interface crate for two integers.
#[must_use]
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(PNG_MAGIC) {
        // IHDR is required to be the first chunk, so width and height sit at a
        // fixed offset in every valid PNG.
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return (width > 0 && height > 0).then_some((width, height));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        // JPEG carries its size in whichever start-of-frame marker the encoder
        // chose, so the segment chain has to be walked to find one.
        let mut index = 2usize;
        while index + 9 < bytes.len() {
            if bytes[index] != 0xFF {
                index += 1;
                continue;
            }
            let marker = bytes[index + 1];
            let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
            // C0..CF are all start-of-frame except C4 (Huffman tables), C8
            // (reserved) and CC (arithmetic conditioning).
            let is_frame = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if is_frame {
                let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]);
                let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]);
                return (width > 0 && height > 0).then_some((u32::from(width), u32::from(height)));
            }
            if length < 2 {
                return None;
            }
            index += 2 + length;
        }
    }
    None
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn a_png_reports_its_ihdr_size() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend([0, 0, 0, 13]);
        bytes.extend(b"IHDR");
        bytes.extend(2560u32.to_be_bytes());
        bytes.extend(1440u32.to_be_bytes());
        assert_eq!(image_size(&bytes), Some((2560, 1440)));
    }

    #[test]
    fn a_jpeg_reports_the_size_in_its_frame_marker() {
        let mut bytes = vec![0xFF, 0xD8];
        // A comment segment first, so the walk has to skip something.
        bytes.extend([0xFF, 0xFE, 0x00, 0x04, 0x00, 0x00]);
        bytes.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend(1440u16.to_be_bytes());
        bytes.extend(2560u16.to_be_bytes());
        bytes.extend([0u8; 8]);
        assert_eq!(image_size(&bytes), Some((2560, 1440)));
    }

    /// Anything unrecognised must say so rather than guess a size, because a
    /// guessed size scales every rectangle drawn on it.
    #[test]
    fn an_unknown_file_has_no_size() {
        assert_eq!(image_size(b"not an image at all"), None);
        assert_eq!(image_size(&[]), None);
        assert_eq!(image_size(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[test]
    fn a_zero_sized_png_is_rejected() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend([0, 0, 0, 13]);
        bytes.extend(b"IHDR");
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(1440u32.to_be_bytes());
        assert_eq!(image_size(&bytes), None);
    }
}

#[cfg(test)]
mod round_trip_tests {
    use super::*;

    /// The whole point of the screen, checked without a window.
    ///
    /// A 2560x1440 screenshot fitted into a canvas, a drag across the tables,
    /// and the rectangle that comes back has to be the one the capture layer
    /// would have been given. Everything between — the fit, the transform, the
    /// corner normalisation — is where a rectangle can silently land somewhere
    /// other than where it was drawn, and it still looks right on screen when
    /// it does.
    #[test]
    fn a_drag_over_the_tables_lands_on_the_tables() {
        const SHOT: (u32, u32) = (2560, 1440);
        // POE1's factory tables rectangle, which the corpus is calibrated on.
        const WANT: SourceRect = SourceRect {
            x: 1145,
            y: 349,
            width: 259,
            height: 470,
        };

        for canvas in [(1280.0_f32, 620.0_f32), (900.0, 500.0), (1920.0, 980.0)] {
            let view = View::fitted(SHOT, canvas);
            // Where the user would press and release, in canvas coordinates.
            let press = view.to_canvas(WANT.x as f32, WANT.y as f32);
            let release = view.to_canvas(
                (WANT.x + WANT.width as i32) as f32,
                (WANT.y + WANT.height as i32) as f32,
            );
            // Back through the same transform the mouse handler uses.
            let from = view.to_source(press.0, press.1);
            let to = view.to_source(release.0, release.1);
            let drawn = SourceRect::from_corners(from, to).expect("a drag this size is a region");
            assert_eq!(drawn, WANT, "canvas {canvas:?} gave {drawn:?}");
        }
    }

    /// Dragging past the edge stops at the edge rather than losing the region.
    #[test]
    fn a_drag_off_the_edge_clamps() {
        let size = (2560.0_f32, 1440.0_f32);
        let from = (2400.0_f32, 1300.0_f32);
        let overshoot = (3000.0_f32, 1800.0_f32);
        let clamped = (
            overshoot.0.clamp(0.0, size.0),
            overshoot.1.clamp(0.0, size.1),
        );
        let rect = SourceRect::from_corners(from, clamped).expect("region");
        assert_eq!(
            rect,
            SourceRect {
                x: 2400,
                y: 1300,
                width: 160,
                height: 140
            }
        );
    }
}
