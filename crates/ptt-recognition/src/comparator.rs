//! Pixel classifier for the `<` / `>` comparator glyph on boundary rows.
//!
//! Windows OCR frequently drops these glyphs, so they are read from the ink
//! mask directly: a chevron's apex points left (`<`) or right (`>`), which
//! shows up as the middle third of the glyph extending further left (or
//! right) than the outer thirds. Ambiguous shapes return `None` and the row
//! is skipped — the classifier never guesses. (Same closed-domain posture as
//! POE1's comparator template fusion, without shipping template assets.)

use ptt_vision::TextInkMask;

use crate::fields::Comparator;

/// Minimum ink pixels for a credible glyph (a 17px chevron has ~30).
const MINIMUM_INK: usize = 10;
/// How much further the mid third must extend than the outer thirds, px.
const MINIMUM_APEX_MARGIN: i32 = 3;

/// Classifies the comparator glyph inside `zone` (mask coordinates).
pub fn classify_comparator(
    mask: &TextInkMask,
    zone_x: usize,
    zone_y: usize,
    zone_width: usize,
    zone_height: usize,
) -> Option<Comparator> {
    let right = (zone_x + zone_width).min(mask.width());
    let bottom = (zone_y + zone_height).min(mask.height());
    if zone_x >= right || zone_y >= bottom {
        return None;
    }

    // Tight bbox of ink inside the zone.
    let mut min_x = usize::MAX;
    let mut max_x = 0usize;
    let mut min_y = usize::MAX;
    let mut max_y = 0usize;
    let mut ink = 0usize;
    for y in zone_y..bottom {
        for x in zone_x..right {
            if mask.intensity_at(x, y).unwrap_or(0) == 0 {
                continue;
            }
            ink += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if std::env::var_os("PTT_DEBUG_COMPARATOR").is_some() {
        eprintln!(
            "comparator zone x={zone_x} y={zone_y} w={zone_width} h={zone_height}:              ink={ink} bbox=({min_x},{min_y})..({max_x},{max_y})"
        );
    }
    if ink < MINIMUM_INK || min_y >= max_y || min_x >= max_x {
        return None;
    }

    // Per-row horizontal extents across the glyph bbox, split into thirds.
    let height = max_y - min_y + 1;
    let third = (height / 3).max(1);
    let mut mid_leftmost = i32::MAX;
    let mut outer_leftmost = i32::MAX;
    let mut mid_rightmost = i32::MIN;
    let mut outer_rightmost = i32::MIN;
    for y in min_y..=max_y {
        let mut row_left = None;
        let mut row_right = None;
        for x in min_x..=max_x {
            if mask.intensity_at(x, y).unwrap_or(0) != 0 {
                row_left.get_or_insert(x);
                row_right = Some(x);
            }
        }
        let (Some(left), Some(right_edge)) = (row_left, row_right) else {
            continue;
        };
        let band = y - min_y;
        let is_mid = band >= third && band < height.saturating_sub(third);
        if is_mid {
            mid_leftmost = mid_leftmost.min(left as i32);
            mid_rightmost = mid_rightmost.max(right_edge as i32);
        } else {
            outer_leftmost = outer_leftmost.min(left as i32);
            outer_rightmost = outer_rightmost.max(right_edge as i32);
        }
    }
    if mid_leftmost == i32::MAX || outer_leftmost == i32::MAX {
        return None;
    }

    // Combine both sides' evidence so an anti-aliased apex tip eroded by the
    // mask threshold cannot flip the call: a `>` indents the middle rows on
    // the left AND extends them on the right; `<` is the mirror image. The
    // score sums both effects; a vertical bar or noise scores ~0.
    let left_indent = mid_leftmost - outer_leftmost;
    let right_indent = outer_rightmost - mid_rightmost;
    let score = left_indent - right_indent;
    if std::env::var_os("PTT_DEBUG_OCR").is_some() {
        eprintln!(
            "debug comparator: zone x={zone_x} w={zone_width} ink={ink} \
             bbox x{min_x}..{max_x} y{min_y}..{max_y} \
             left mid/outer {mid_leftmost}/{outer_leftmost} \
             right mid/outer {mid_rightmost}/{outer_rightmost} score={score}"
        );
    }
    if score >= MINIMUM_APEX_MARGIN {
        Some(Comparator::GreaterThan)
    } else if score <= -MINIMUM_APEX_MARGIN {
        Some(Comparator::LessThan)
    } else {
        None
    }
}

/// Narrows a zone to its first connected column-run of ink.
///
/// The chevron is one token and the ratio beside it is another, separated by a
/// real gap — measured at 3 to 13px across both corpora. Without this the zone
/// borrows a pixel of the neighbouring digit, because the comparator mask is
/// permissive enough to keep an anti-aliased edge, and one stray pixel on the
/// right of the box is enough to flatten a `>`'s apex (a `<` survives it,
/// which is why this only ever showed on the competing side).
///
/// Returns `(x, width)` of the run, or `None` for an empty zone.
#[must_use]
pub fn leading_ink_run(
    mask: &TextInkMask,
    zone_x: usize,
    zone_y: usize,
    zone_width: usize,
    zone_height: usize,
) -> Option<(usize, usize)> {
    let right = (zone_x + zone_width).min(mask.width());
    let bottom = (zone_y + zone_height).min(mask.height());
    let inked =
        |x: usize| (zone_y..bottom).any(|y| mask.intensity_at(x, y).is_some_and(|value| value > 0));
    let start = (zone_x..right).find(|&x| inked(x))?;
    let end = (start..right).take_while(|&x| inked(x)).last()?;
    Some((start, end - start + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptt_vision::{CaptureRegion, CapturedFrame, WarmMaskSettings, build_warm_mask};

    const W: usize = 24;
    const H: usize = 20;

    /// Renders a chevron into a synthetic frame and masks it.
    fn chevron_mask(less_than: bool) -> TextInkMask {
        let mut bgra = vec![0u8; W * H * 4];
        // Dark background.
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[70, 79, 82, 255]);
        }
        // Chevron strokes: rows y=4..=14, apex at y=9.
        for y in 4..=14usize {
            let offset = (y as i32 - 9).unsigned_abs() as usize;
            let x = if less_than { 4 + offset } else { 12 - offset };
            for dx in 0..2 {
                let index = (y * W + x + dx) * 4;
                bgra[index..index + 4].copy_from_slice(&[143, 185, 204, 255]);
            }
        }
        let frame = CapturedFrame::from_bgra(
            CaptureRegion::new(0, 0, W as u32, H as u32).unwrap(),
            W * 4,
            bgra,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .unwrap();
        build_warm_mask(&frame, WarmMaskSettings::default())
    }

    #[test]
    fn chevrons_classify_by_apex_direction() {
        let less = chevron_mask(true);
        assert_eq!(
            classify_comparator(&less, 0, 0, W, H),
            Some(Comparator::LessThan)
        );
        let greater = chevron_mask(false);
        assert_eq!(
            classify_comparator(&greater, 0, 0, W, H),
            Some(Comparator::GreaterThan)
        );
    }

    #[test]
    fn empty_or_ambiguous_zones_return_none() {
        let mut bgra = vec![0u8; W * H * 4];
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[70, 79, 82, 255]);
        }
        // A vertical bar: extends equally in all thirds.
        for y in 4..=14usize {
            let index = (y * W + 8) * 4;
            bgra[index..index + 4].copy_from_slice(&[143, 185, 204, 255]);
            let index = (y * W + 9) * 4;
            bgra[index..index + 4].copy_from_slice(&[143, 185, 204, 255]);
        }
        let frame = CapturedFrame::from_bgra(
            CaptureRegion::new(0, 0, W as u32, H as u32).unwrap(),
            W * 4,
            bgra,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .unwrap();
        let mask = build_warm_mask(&frame, WarmMaskSettings::default());
        assert_eq!(classify_comparator(&mask, 0, 0, W, H), None);
        assert_eq!(classify_comparator(&mask, 0, 0, 2, 2), None);
    }
}
