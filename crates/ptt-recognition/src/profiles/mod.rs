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

/// A pinned two-table grid, in pixels relative to the captured tables region.
#[derive(Clone, Copy, Debug)]
pub struct FixedGrid {
    pub available_top: u32,
    pub competing_top: u32,
    pub pitch: u32,
    /// How much of each pitch step holds glyphs; the rest is padding.
    pub row_height: u32,
    pub rows_per_side: u8,
    /// A slice with fewer lit mask pixels than this is an empty row, which is
    /// how a table showing four of six rows is read as four.
    pub min_lit_pixels: u32,
    /// (offset, width) of the comparator column within the region.
    ///
    /// A pinned panel knows where its chevron lives, so the glyph is read
    /// from that column alone. Deriving the zone from where the other rows'
    /// ink starts — which is what a floating layout must do — sweeps the
    /// first digit into the same bounding box, and a chevron beside a digit
    /// has no apex.
    pub comparator_column: (u32, u32),
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
        let assets = ptt_catalog::poe2().assets();
        assert!(!assets.is_empty(), "catalog is empty");
        for asset in assets {
            for (language, name) in [
                (ProfileLanguage::TraditionalChinese, &asset.name_zh_tw),
                (ProfileLanguage::English, &asset.name_en),
            ] {
                assert!(
                    !name.trim().is_empty(),
                    "{} has no name for {language:?}",
                    asset.id
                );
                assert!(
                    ptt_core::FullLineAffixMatcher::new(name).is_ok(),
                    "{} cannot be matched in {language:?}: {name:?}",
                    asset.id
                );
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
