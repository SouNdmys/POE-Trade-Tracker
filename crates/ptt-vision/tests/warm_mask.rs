//! Contract tests for the warm currency-exchange text mask.

use ptt_vision::{
    CaptureRegion, CapturedFrame, WarmMaskSettings, build_warm_mask, build_warm_mask_into,
};

const WIDTH: usize = 32;
const HEIGHT: usize = 8;

/// Table-body background gray (L≈78) from the 1440p corpus.
const BACKGROUND: [u8; 4] = [70, 79, 82, 255];
/// Warm ratio-digit ink ≈ (r 204, g 185, b 143) as BGRA.
const WARM_INK: [u8; 4] = [143, 185, 204, 255];
/// Near-white slot-name ink.
const WHITE_INK: [u8; 4] = [240, 243, 245, 255];
/// Bright blue UI accent — bright enough to pass luminance, must be rejected.
const BLUE_ACCENT: [u8; 4] = [255, 190, 120, 255];

fn frame_with(pixels: &[(usize, usize, [u8; 4])], origin_x: i32, origin_y: i32) -> CapturedFrame {
    let mut bgra = Vec::with_capacity(WIDTH * HEIGHT * 4);
    for _ in 0..WIDTH * HEIGHT {
        bgra.extend_from_slice(&BACKGROUND);
    }
    for &(x, y, ink) in pixels {
        let offset = (y * WIDTH + x) * 4;
        bgra[offset..offset + 4].copy_from_slice(&ink);
    }
    CapturedFrame::from_bgra(
        CaptureRegion::new(origin_x, origin_y, WIDTH as u32, HEIGHT as u32).unwrap(),
        WIDTH * 4,
        bgra,
        std::time::SystemTime::UNIX_EPOCH,
    )
    .unwrap()
}

#[test]
fn warm_and_white_ink_are_kept_background_and_blue_rejected() {
    let frame = frame_with(
        &[(4, 2, WARM_INK), (10, 2, WHITE_INK), (16, 2, BLUE_ACCENT)],
        0,
        0,
    );
    let mask = build_warm_mask(&frame, WarmMaskSettings::default());

    assert!(mask.intensity_at(4, 2).unwrap() > 0, "warm digit ink kept");
    assert!(mask.intensity_at(10, 2).unwrap() > 0, "white name ink kept");
    assert_eq!(
        mask.intensity_at(16, 2).unwrap(),
        0,
        "bright blue accent must not become ink"
    );
    assert_eq!(mask.intensity_at(5, 5).unwrap(), 0, "gray background empty");
}

#[test]
fn fingerprint_ignores_roi_origin_but_not_glyph_positions() {
    let glyphs = [(4usize, 2usize, WARM_INK), (10, 3, WARM_INK)];
    let at_origin = build_warm_mask(&frame_with(&glyphs, 0, 0), WarmMaskSettings::default());
    let moved = build_warm_mask(&frame_with(&glyphs, 640, 360), WarmMaskSettings::default());
    assert_eq!(
        at_origin.fingerprint(),
        moved.fingerprint(),
        "moving the ROI must not invalidate caches"
    );

    let shifted_glyph = build_warm_mask(
        &frame_with(&[(5, 2, WARM_INK), (10, 3, WARM_INK)], 0, 0),
        WarmMaskSettings::default(),
    );
    assert_ne!(
        at_origin.fingerprint(),
        shifted_glyph.fingerprint(),
        "a moved glyph is a different book state"
    );
}

#[test]
fn reused_allocation_produces_identical_results() {
    let frame = frame_with(&[(4, 2, WARM_INK)], 0, 0);
    let fresh = build_warm_mask(&frame, WarmMaskSettings::default());

    let mut reused = build_warm_mask(
        &frame_with(&[(20, 6, WHITE_INK), (21, 6, WHITE_INK)], 0, 0),
        WarmMaskSettings::default(),
    );
    build_warm_mask_into(&frame, WarmMaskSettings::default(), &mut reused);
    assert_eq!(fresh.fingerprint(), reused.fingerprint());
    assert_eq!(fresh.intensities(), reused.intensities());
}
