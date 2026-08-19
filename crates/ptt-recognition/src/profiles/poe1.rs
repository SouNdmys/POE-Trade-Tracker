//! POE1 English profile geometry.
//!
//! Unlike POE2, whose two order tables sit adjacent and float relative to
//! each other (so the split has to be inferred from the gap), POE1 pins both
//! tables to fixed rows. Every one of the ten annotated fixtures puts the
//! available table's first row at y=349 and the competing table's at y=610,
//! regardless of how many rows either side holds. That makes POE1 the easier
//! of the two layouts and lets the split be a constant instead of a search.
//!
//! These numbers are not guesses: they are read straight from the annotated
//! crop rectangles in the POE1 fixture store, where a human marked every
//! field on ten real screenshots.
//!
//! The route itself is [`crate::route::Route`] driven by [`LAYOUT`]: the OCR
//! ladder, field parsers, comparator classifier and book assembly are all
//! layout-agnostic, so a second game is a second layout value rather than a
//! second copy of the pipeline.

use crate::rows::RowLayout;

/// (x, y, width, height) desktop-pixel presets for 2560×1440.
///
/// The name slots exclude the icon boxes to their left (need at x=801, have
/// at x=1466): the icons are separate evidence, and feeding them to a text
/// recogniser only adds noise.
pub const NEED_NAME_REGION: (i32, i32, u32, u32) = (856, 294, 238, 61);
pub const HAVE_NAME_REGION: (i32, i32, u32, u32) = (1522, 294, 238, 61);

/// Both tables in one region: from the panel's outer top edge, above the
/// "Available Trades" title bar, down to just under the last competing row.
///
/// These are the numbers a person drew by hand on a screenshot of their own
/// client, kept because they hug the panel — which is what makes a preset
/// checkable by looking at it. The earlier constants read every corpus frame
/// too, but sat 11px narrower and 24px lower than the panel, so the box on
/// screen did not obviously correspond to the thing it was framing.
///
/// The top is the panel's border, not the first data row. The first row's ink
/// has no visible edge to aim at, so asking for it was asking for a landmark
/// that is not on the screen -- a frame one pixel low clips the row's first
/// scanline -- while the border is exactly where a hand lands when the eye is
/// told to frame the two tables.
///
/// The mask is what makes that safe: the title bar, the "Rate / Stock" column
/// headings and the panel's own ornament are all drawn too dim to survive the
/// warm threshold, so nothing between the frame's top edge and the first row
/// can be mistaken for it. Seventy pixels of furniture sit inside this frame
/// and the anchor still lands on the first row.
pub const TABLES_REGION: (i32, i32, u32, u32) = (1146, 281, 270, 520);

/// How far the Traditional Chinese client pushes the tables down.
///
/// Measured, not assumed, and not a rounding of someone's hand: with this set
/// to zero — one rectangle for both clients, which is what the panel looks
/// like it should need — the two Traditional Chinese corpora fail 23 frames
/// out of 24 while English still passes all ten. The client draws taller table
/// headings inside a panel that has not moved; the name slots are identical in
/// both, which is the same fact seen from the other side.
///
/// Fourteen is the row ink's own offset (English first row at y=358 against
/// 372), which is a pixel more than the panel furniture above it moves. The
/// rows are what the grid has to find, so the rows are what this tracks: at 14
/// both clients anchor at the same +75..+77, and at 13 the Traditional Chinese
/// one drifts a pixel low.
pub const TABLES_ZH_TW_OFFSET: i32 = 14;

/// Where each table's first row is looked for, relative to [`TABLES_REGION`].
///
/// Search origins, not assertions, placed half a window above where the row
/// actually sits so the slack is symmetric. Against the factory preset all
/// three corpora anchor at +75..+77 and +336..+338 -- the same numbers in
/// either client, because [`TABLES_ZH_TW_OFFSET`] has already taken the
/// language difference out of them. Centring a 30px window on those leaves
/// about 13px for a hand-drawn frame either way, which is the whole budget:
/// the window cannot reach a pitch without letting an empty first row anchor
/// to the second.
pub const AVAILABLE_TABLE_TOP: u32 = 62;
pub const COMPETING_TABLE_TOP: u32 = 323;

/// How far below each nominal top to look for that table's first row.
///
/// Held under one pitch so a table whose first row is empty cannot anchor to
/// its second and shift every slot by one.
pub const ANCHOR_WINDOW: u32 = 30;

/// POE1 rows are a uniform 32px with no wrapped-decimal case observed in the
/// corpus, so the band search is tighter than POE2's.
#[must_use]
pub fn default_row_layout() -> RowLayout {
    RowLayout {
        single_height: (24, 36),
        merged_height_max: 72,
        row_pitch: 32,
        // The region starts exactly at the first row.
        first_row_top: (0, 12),
        // Six available rows end at 541 and competing starts at 610, so the
        // narrowest observed gap is 69px.
        table_gap_min: 50,
        max_rows_per_side: 6,
    }
}

/// The POE1 panel. The layout carries no language of its own — geometry is
/// identical between clients because ratios and stock are Arabic numerals
/// either way — so `Route::new_with(LAYOUT, language)` builds either one now
/// that the catalog supplies both. The zh-TW combination is untested against
/// real frames: the Traditional Chinese screenshots on hand are of the item
/// selector, not of the exchange panel.
pub const LAYOUT: super::PanelLayout = super::PanelLayout {
    key_prefix: "POE1",
    game: ptt_core::Game::Poe1,
    need_name: NEED_NAME_REGION,
    have_name: HAVE_NAME_REGION,
    tables: TABLES_REGION,
    tables_zh_tw_offset: TABLES_ZH_TW_OFFSET,
    rows: default_row_layout,
    row_source: super::RowSource::FixedGrid(super::FixedGrid {
        available_top: AVAILABLE_TABLE_TOP,
        competing_top: COMPETING_TABLE_TOP,
        anchor_window: ANCHOR_WINDOW,
        // Rows measure 15-19px of ink in a 28px slice, so there is 9px of
        // slack; the pitch error spends 2 of it by the sixth row. Five leaves
        // the band centred at the top of the table and still clear at the
        // bottom. Tightening it to three was tried and costs rows: the lower
        // slices then sit high enough to clip their own band.
        anchor_lead_in: 5,
        // Measured, not assumed: six row tops span 153px in both clients, in
        // every one of the 21 frames on hand. The 32 this used to carry was
        // over by 1.4px per row, and only fitted because the old origin
        // happened to leave exactly enough headroom to absorb the drift.
        pitch: 31,
        row_height: 28,
        rows_per_side: 6,
        // A digit or two of ink; below this the slice is an unused row.
        min_lit_pixels: 40,
    }),
    catalog: ptt_catalog::poe1,
    // The chevron's core clears 140 while the cell background sits near 70,
    // so a slightly lower threshold than the shared 150 captures the whole
    // arrow without letting the brighter available-table cell flood.
    comparator_mask: Some(ptt_vision::WarmMaskSettings {
        minimum_luminance: 120,
        maximum_blue_over_red: 12,
    }),
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the route reports each table's first ink against the preset,
    /// across all three corpora (`PTT_DEBUG_GRID=1`). Identical in both
    /// clients, because [`TABLES_ZH_TW_OFFSET`] has already taken the language
    /// difference out.
    const OBSERVED_AVAILABLE: std::ops::RangeInclusive<u32> = 75..=77;
    const OBSERVED_COMPETING: std::ops::RangeInclusive<u32> = 336..=338;
    /// The lowest the competing table has been seen to start.
    const LOWEST_COMPETING_ANCHOR: u32 = *OBSERVED_COMPETING.end();

    /// The frame must hold the whole of its last row at the preset.
    ///
    /// At the preset, not at the far end of the anchor's travel: an anchor
    /// pushed to the bottom of its window is a *top* edge drawn too high, and
    /// a hand that misjudges the panel's top border does not raise its bottom
    /// border to match. How much of the last slice may be cut off after that
    /// is not something arithmetic can answer -- the slice is 28px but its ink
    /// is 15 to 19, so some of the loss is padding -- so the corpus answers it
    /// instead, in `book-probe --tolerance`.
    ///
    /// Cutting it short does not fail loudly. Windows OCR returns nothing for
    /// a crop that short and the table reads one row fewer than it has, which
    /// is how the zh-TW client's extra height was found the first time.
    #[test]
    fn the_frame_holds_its_last_row() {
        let crate::profiles::RowSource::FixedGrid(grid) = LAYOUT.row_source else {
            panic!("POE1 slices a fixed grid");
        };
        let (_, _, _, region_height) = TABLES_REGION;
        // The grid's own pitch, not `RowLayout`'s: that one describes the band
        // detector, which this panel does not use.
        let last_row_bottom = LOWEST_COMPETING_ANCHOR - grid.anchor_lead_in
            + u32::from(grid.rows_per_side - 1) * grid.pitch
            + grid.row_height;
        assert!(
            last_row_bottom <= region_height,
            "the last row ends at {last_row_bottom}, past the {region_height}px frame"
        );
    }

    /// Both tables must be reachable from where their search begins.
    ///
    /// The tops are search origins, not row positions — a distinction the
    /// earlier version of this test did not make, because back then the frame
    /// was defined to start exactly on the first row and the two were the same
    /// number. They are not any more, and asserting the old equality would
    /// pin the window to one edge of its own travel.
    ///
    /// The offsets below are what the route reports across all three corpora
    /// (`PTT_DEBUG_GRID=1`), identical in both clients because
    /// [`TABLES_ZH_TW_OFFSET`] has already removed the language difference.
    #[test]
    fn each_table_sits_within_its_own_search_window() {
        for (label, nominal, observed) in [
            ("available", AVAILABLE_TABLE_TOP, OBSERVED_AVAILABLE),
            ("competing", COMPETING_TABLE_TOP, OBSERVED_COMPETING),
        ] {
            // Two scanlines short of the limit, because the anchor needs three
            // consecutive lit rows before it will call one.
            let reach = nominal + ANCHOR_WINDOW - 2;
            assert!(
                nominal <= *observed.start() && *observed.end() <= reach,
                "{label} rows at {observed:?} fall outside the {nominal}..{reach} search"
            );
        }
    }

    /// The window may not reach a whole row pitch.
    ///
    /// Past that, a table whose first row is empty anchors to its second and
    /// every slot shifts by one — the rates still parse, so nothing fails
    /// loudly; they are simply filed under the wrong rank.
    #[test]
    fn the_anchor_window_stays_under_one_pitch() {
        let crate::profiles::RowSource::FixedGrid(grid) = LAYOUT.row_source else {
            panic!("POE1 slices a fixed grid");
        };
        assert!(
            ANCHOR_WINDOW < grid.pitch,
            "a {ANCHOR_WINDOW}px window reaches past the {}px pitch",
            grid.pitch
        );
    }

    #[test]
    fn the_gap_between_tables_is_wider_than_the_split_threshold() {
        let layout = default_row_layout();
        let full_available_bottom =
            AVAILABLE_TABLE_TOP + u32::from(layout.max_rows_per_side) * layout.row_pitch;
        let gap = COMPETING_TABLE_TOP - full_available_bottom;
        assert!(
            i32::try_from(gap).unwrap() >= layout.table_gap_min,
            "a full available table leaves {gap}px, under the {}px split threshold",
            layout.table_gap_min
        );
    }
}
