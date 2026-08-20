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

/// The panel's rows are drawn at fixed offsets, so they are sliced rather
/// than detected.
///
/// This used to detect bands from the mask and infer the table split from the
/// gap, on the belief that POE2's two tables float relative to each other.
/// They do not. Across thirty field frames the band tops read 63, 94, 124,
/// 155, 186, 216 and 324, 355, 385, 416, 447, 477 — a constant pitch and a
/// constant 108px gap between the tables. Every departure from that was a
/// detection artefact: a row the panel wrapped reported its band 17px high,
/// a torn repaint split one row into two, and in each case the *inference*
/// moved, never the rows.
///
/// Detection made all twelve rows share a fate. One band the detector could
/// not classify took the whole frame with it, because the split, the row
/// count and every row's rank were all read off the same band list. Slicing a
/// known grid makes each row independent: a wrapped rate costs its own slice
/// and the other eleven are untouched, which is what the panel's own geometry
/// was always offering.
pub const ROW_PITCH: u32 = 31;
/// Of each pitch step, how much holds glyphs. The rest is padding.
///
/// Measured, not tuned. A row lays down 18px of ink and the next starts 31px
/// on: `77-94`, `108-125`, `138-155`, `169-186`, `200-217`. From five pixels
/// above the ink, 28 covers all of it and still stops eight clear of the row
/// below. The same numbers POE1 arrived at, on the same panel geometry.
pub const ROW_HEIGHT: u32 = 28;
/// Where each table's first row is looked for, relative to the frame.
///
/// Below the column headings, not level with them. Each table lays out a title
/// bar, then a `Ratio / Stock` heading, then its rows: 55-73 and 76 on the
/// available side, 304-333 and 337 on the competing one. Searching from 62 and
/// 323 started the window *inside* those headings, and it found the rows
/// anyway only because the headings are usually drawn too dim to reach the
/// warm mask. On a frame where one was not, the anchor caught the heading and
/// every slice under it landed in the gaps between rows -- four empty crops in
/// a row. A window that cannot see the heading cannot latch onto it.
pub const AVAILABLE_TABLE_TOP: u32 = 74;
pub const COMPETING_TABLE_TOP: u32 = 334;
/// Held under one pitch, so an empty first row cannot anchor to the second.
pub const ANCHOR_WINDOW: u32 = 30;

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
    row_source: super::RowSource::FixedGrid(super::FixedGrid {
        available_top: AVAILABLE_TABLE_TOP,
        competing_top: COMPETING_TABLE_TOP,
        anchor_window: ANCHOR_WINDOW,
        anchor_lead_in: 5,
        pitch: ROW_PITCH,
        row_height: ROW_HEIGHT,
        rows_per_side: 6,
        min_lit_pixels: 40,
    }),
    catalog: ptt_catalog::poe2,
    comparator_mask: None,
};
