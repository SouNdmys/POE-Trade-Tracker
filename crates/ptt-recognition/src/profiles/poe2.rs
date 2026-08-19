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
pub const TABLES_REGION: (i32, i32, u32, u32) = (1150, 220, 320, 560);
/// "I need" name text, icon excluded.
pub const NEED_NAME_REGION: (i32, i32, u32, u32) = (855, 296, 240, 52);
/// "I have" name text, icon and the right-edge favorite star excluded.
pub const HAVE_NAME_REGION: (i32, i32, u32, u32) = (1520, 296, 210, 52);

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
