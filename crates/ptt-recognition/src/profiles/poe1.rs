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
//! The route itself is [`super::poe2::Route`] driven by [`LAYOUT`]: the OCR
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

/// Both tables in one region: comparator column (x=1145) through the right
/// edge of stock (x=1287+117), and the first available row (y=349) through
/// the last competing row (y=770+32).
pub const TABLES_REGION: (i32, i32, u32, u32) = (1145, 349, 259, 453);

/// Where each table starts, relative to [`TABLES_REGION`].
pub const AVAILABLE_TABLE_TOP: u32 = 0;
pub const COMPETING_TABLE_TOP: u32 = 610 - 349;

/// Column offsets within [`TABLES_REGION`].
/// Where the chevron actually is, which is not where the annotation's cell
/// box is. The annotator marked the comparator cell as 1145..1177; the glyph
/// is drawn against that cell's right edge and overhangs into the ratio
/// column's left padding, spanning roughly 1173..1179. Reading the cell box
/// cuts the arrow in half and leaves a three-pixel sliver with no apex.
///
/// The window is kept tight on the other side too: widen it much past the
/// glyph and a short ratio's leading digit lands in the same bounding box,
/// which has the same effect as clipping — no apex to measure.
pub const COMPARATOR_COLUMN: (u32, u32) = (24, 12);
pub const RATIO_COLUMN: (u32, u32) = (1177 - 1145, 110);
pub const STOCK_COLUMN: (u32, u32) = (1287 - 1145, 117);

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
    rows: default_row_layout,
    row_source: super::RowSource::FixedGrid(super::FixedGrid {
        // Where each table starts if the client is English. Both are search
        // origins, not assertions: the zh-TW column header is taller and puts
        // the first row about 15px further down.
        available_top: AVAILABLE_TABLE_TOP,
        competing_top: COMPETING_TABLE_TOP,
        // Under one pitch, so an empty first row cannot anchor to the second.
        anchor_window: 31,
        // Rows measure 15-19px of ink in a 28px slice, so there is 9px of
        // slack to spend. Five above the first row leaves four below the
        // sixth once the pitch error has accumulated.
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
        comparator_column: COMPARATOR_COLUMN,
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

    #[test]
    fn the_two_tables_fit_inside_the_captured_region() {
        let (_, region_top, _, region_height) = TABLES_REGION;
        let layout = default_row_layout();
        let last_row_bottom =
            COMPETING_TABLE_TOP + u32::from(layout.max_rows_per_side) * layout.row_pitch;
        assert!(
            last_row_bottom <= region_height,
            "six competing rows end at {last_row_bottom}, past the {region_height}px region"
        );
        // The annotated competing table starts at absolute y=610.
        assert_eq!(
            region_top + i32::try_from(COMPETING_TABLE_TOP).unwrap(),
            610
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

    #[test]
    fn the_columns_stay_inside_the_region() {
        let (_, _, width, _) = TABLES_REGION;
        for (label, (offset, span)) in [
            ("comparator", COMPARATOR_COLUMN),
            ("ratio", RATIO_COLUMN),
            ("stock", STOCK_COLUMN),
        ] {
            assert!(
                offset + span <= width,
                "{label} column ends at {} past the {width}px region",
                offset + span
            );
        }
    }
}

#[cfg(test)]
mod comparator_window {
    use super::*;

    /// The window was calibrated against the corpus and both edges matter, so
    /// it is pinned here rather than left as a number someone could round.
    #[test]
    fn the_comparator_window_holds_the_glyph_and_excludes_the_ratio() {
        let (offset, width) = COMPARATOR_COLUMN;
        // Chevrons were observed spanning offsets 24 through 34.
        assert!(
            offset <= 24,
            "window starts after the leftmost chevron seen"
        );
        assert!(
            offset + width >= 35,
            "window ends before the rightmost chevron seen"
        );
        // The leading ratio digit was observed starting at offset 39 on the
        // tightest frame; including it gives the glyph a bounding box with no
        // apex, and the row is then rejected as unverified.
        assert!(
            offset + width < 39,
            "window reaches the ratio digit, which destroys the apex"
        );
    }
}
