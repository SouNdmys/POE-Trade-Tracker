//! POE2 panel geometry.
//!
//! Defaults come from the calibrated corpus (docs/P1-CALIBRATION-NOTES.md).
//! Users on other resolutions calibrate their own regions; these constants are
//! the factory preset, not a requirement.
//!
//! Only the Traditional Chinese path is corpus-verified. The English path is
//! built from the same catalog's English names and has no screenshots behind
//! it yet, so its geometry presets are inherited rather than calibrated.

use crate::rows::RowLayout;

/// (x, y, width, height) desktop-pixel presets for 2560×1440 windowed
/// fullscreen with the exchange panel in its centered default position.
///
/// Drawn by hand on a live client and kept as measured. The first set was
/// taken from the corpus screenshots and missed twice over: the tables frame
/// started above the market-ratio strip, and the "I have" frame was cut short
/// on both axes.
pub const TABLES_REGION: (i32, i32, u32, u32) = (1163, 217, 269, 523);
/// "I need" name text, icon excluded.
///
/// Tall enough for a name that wraps to two lines. The first frame was sized
/// to the one-line case, which is most of them, and clipped the second line
/// of every gem — the name then resolved to whatever the first line alone
/// happened to look like.
pub const NEED_NAME_REGION: (i32, i32, u32, u32) = (858, 288, 240, 66);
/// "I have" name text, icon excluded — but the favourite star kept.
///
/// The star sits at the slot's right edge and lights up for a favourited
/// currency, so excluding it looked like the careful choice. It is not: the
/// slot is taller than the "I need" one and the old frame clipped the name
/// itself, which is a real loss against a decoration that measurably costs
/// nothing. Tested both ways on a live client, lit and unlit, with no
/// difference to what the name resolves to.
pub const HAVE_NAME_REGION: (i32, i32, u32, u32) = (1518, 286, 246, 74);

pub fn default_row_layout() -> RowLayout {
    RowLayout::default()
}

/// The POE2 panel, in either client language.
pub const LAYOUT: super::PanelLayout = super::PanelLayout {
    key_prefix: "POE2",
    game: ptt_core::Game::Poe2,
    need_name: NEED_NAME_REGION,
    have_name: HAVE_NAME_REGION,
    tables: TABLES_REGION,
    // Zero, and unmeasured rather than measured as zero: this
    // region was calibrated on the Traditional Chinese client and
    // there are no English POE2 screenshots to compare it against.
    // POE2 detects its rows from the mask instead of slicing a
    // grid, so it absorbs a shift of this size anyway.
    tables_zh_tw_offset: 0,
    rows: default_row_layout,
    row_source: super::RowSource::DetectedBands,
    catalog: ptt_catalog::poe2,
    comparator_mask: None,
};
