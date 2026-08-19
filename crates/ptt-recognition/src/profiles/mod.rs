//! Per-profile bindings of the pure recognition layers to real OCR backends.

pub mod poe1;
pub mod poe2;

use crate::rows::RowLayout;

/// Everything about a panel that varies between games.
///
/// The recognition stack below this — the OCR ladder, the field parsers, the
/// comparator classifier, the book assembly — is layout-agnostic, so a second
/// game is a second value here rather than a second copy of the route.
#[derive(Clone, Copy, Debug)]
pub struct PanelLayout {
    /// Namespaces calibration overrides and environment variables, so a
    /// calibrated POE2 panel cannot be applied to a POE1 one.
    pub key_prefix: &'static str,
    /// Which game this panel belongs to, so a caller holding saved settings
    /// can tell whether they were drawn for this panel at all.
    pub game: ptt_core::Game,
    pub need_name: (i32, i32, u32, u32),
    pub have_name: (i32, i32, u32, u32),
    pub tables: (i32, i32, u32, u32),
    /// How much lower [`PanelLayout::tables`] sits in the Traditional Chinese
    /// client, in pixels.
    ///
    /// The panel itself does not move -- the two name slots are identical in
    /// both clients -- but the headings *inside* it are taller, which pushes
    /// both tables down together. Correcting that here rather than by widening
    /// the row anchor's search window is what buys a person room to draw the
    /// frame by hand: the window has to stay under one row pitch or an empty
    /// first row anchors to the second, so every pixel of that budget the
    /// language spends is a pixel the hand cannot. Folded in, both clients
    /// present the same offsets from the frame to the first row, and the whole
    /// window is left over for aim.
    ///
    /// Applies to the factory preset only. A region the user calibrated was
    /// drawn on their own client and is already where it belongs.
    pub tables_zh_tw_offset: i32,
    /// Function rather than value because `RowLayout` is not const.
    pub rows: fn() -> RowLayout,
    pub row_source: RowSource,
    pub catalog: fn() -> &'static ptt_catalog::Catalog,
    /// A second, more permissive mask used only to read the comparator glyph.
    ///
    /// `None` reuses the main mask, which is what a panel whose chevron is as
    /// bright as its digits wants. POE1 draws its chevron far dimmer than its
    /// text — at the shared threshold only the apex tip survives, three pixels
    /// wide, which has no shape to classify. Lowering the shared threshold
    /// instead would flood the mask, because POE1's inter-table strip is
    /// *brighter* than that chevron; the comparator cell's own background is
    /// dark, so a lower threshold is safe there and only there.
    pub comparator_mask: Option<ptt_vision::WarmMaskSettings>,
}

impl PanelLayout {
    /// The tables preset for one client language.
    ///
    /// One definition, shared by the route that captures the frame, the app
    /// that offers the preset, and the probe that measures its tolerance.
    /// Three copies of a `+ 14` would be three chances to disagree about
    /// where the tables are.
    #[must_use]
    pub const fn tables_for(&self, language: ProfileLanguage) -> (i32, i32, u32, u32) {
        let (x, y, width, height) = self.tables;
        match language {
            ProfileLanguage::TraditionalChinese => (x, y + self.tables_zh_tw_offset, width, height),
            ProfileLanguage::English => (x, y, width, height),
        }
    }
}

/// How a panel's rows are located.
///
/// POE2's two tables float relative to each other, so its rows have to be
/// detected from the warm mask and the table split inferred from the gap.
/// POE1 pins both tables to fixed offsets, which makes detection not just
/// unnecessary but actively worse: the header between the tables reads as an
/// extra band, and rows 32px apart merge into their neighbours. Slicing a
/// known grid skips the header by construction and cannot merge rows.
#[derive(Clone, Copy, Debug)]
pub enum RowSource {
    DetectedBands,
    FixedGrid(FixedGrid),
}

/// A two-table grid, in pixels relative to the captured tables region.
///
/// The tops are where each table is *expected* to start, not where it is
/// asserted to start. The distance from the panel's header to its first row is
/// a function of the client's font: the Traditional Chinese column header is
/// about 15px taller than the English one, which slides both tables down by
/// that much and leaves a pinned grid straddling row boundaries — every slice
/// holding the tail of one row and the head of the next. That does not fail
/// loudly; it clips leading digits, so `1 : 9.67` is read as `1 : 0.67` and
/// accepted. Each table's origin is therefore recovered from its own first row
/// of ink, with the constants below bounding the search.
#[derive(Clone, Copy, Debug)]
pub struct FixedGrid {
    pub available_top: u32,
    pub competing_top: u32,
    /// How far below a nominal top to look for that table's first row. Held to
    /// less than one pitch so a table whose first row is empty — which in this
    /// panel means an empty table, since rows fill downward — cannot anchor to
    /// its second row and shift every slot by one.
    pub anchor_window: u32,
    /// How far above the first row's ink the first slice starts. Absorbs the
    /// residual pitch error over the rows below it, so it has to leave room
    /// both above the first row and below the last.
    pub anchor_lead_in: u32,
    pub pitch: u32,
    /// How much of each pitch step holds glyphs; the rest is padding.
    pub row_height: u32,
    pub rows_per_side: u8,
    /// A slice with fewer lit mask pixels than this is an empty row, which is
    /// how a table showing four of six rows is read as four.
    pub min_lit_pixels: u32,
}

/// Which client language a profile reads names in.
///
/// Only names are affected. Ratios and stock are Arabic numerals in every
/// client, so those lanes are English regardless — a fact worth stating
/// because it is the reason a language switch is a data change rather than a
/// second recognition path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProfileLanguage {
    #[default]
    TraditionalChinese,
    English,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both languages must be fully populated and matchable.
    ///
    /// A blank name silently costs one currency in that language: the matcher
    /// either fails to build or matches nothing, and the frame skips forever
    /// with no indication that the catalog, not the screenshot, is at fault.
    #[test]
    fn every_catalog_asset_is_matchable_in_both_languages() {
        for (game, catalog) in [
            (ptt_core::Game::Poe2, ptt_catalog::poe2()),
            (ptt_core::Game::Poe1, ptt_catalog::poe1()),
        ] {
            let assets = catalog.assets();
            assert!(!assets.is_empty(), "{game:?} catalog is empty");
            for asset in assets {
                for (language, name) in [
                    (ProfileLanguage::TraditionalChinese, &asset.name_zh_tw),
                    (ProfileLanguage::English, &asset.name_en),
                ] {
                    assert!(
                        !name.trim().is_empty(),
                        "{game:?} {} has no name for {language:?}",
                        asset.id
                    );
                    assert!(
                        ptt_core::FullLineAffixMatcher::new(name).is_ok(),
                        "{game:?} {} cannot be matched in {language:?}: {name:?}",
                        asset.id
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// Each layout must namespace itself and name its own game.
    ///
    /// The prefix keys calibration storage and the game decides whether saved
    /// regions belong to this panel at all; two layouts sharing either would
    /// let one game's rectangles be applied to the other's route.
    #[test]
    fn layouts_are_distinguishable_by_prefix_and_game() {
        let layouts = [poe1::LAYOUT, poe2::LAYOUT];
        assert_ne!(layouts[0].key_prefix, layouts[1].key_prefix);
        assert_ne!(layouts[0].game, layouts[1].game);
    }
}
