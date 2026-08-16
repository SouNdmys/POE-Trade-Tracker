//! Per-profile bindings of the pure recognition layers to real OCR backends.

pub mod poe2;

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
