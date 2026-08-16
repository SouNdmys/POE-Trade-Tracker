//! Currency-identity resolution: OCR name text → exact catalog hit or nothing.
//!
//! An open OCR string can never mint an asset id. The only normalization is
//! whitespace removal (Windows OCR sometimes spaces out Han glyphs); a miss
//! is a skip, and fuzzy/nearest-name matching is banned.

use ptt_catalog::{Catalog, CatalogAsset};

use crate::profiles::ProfileLanguage;

/// Resolves a name in whichever language the profile reads.
///
/// The two languages need different normalization — Traditional Chinese names
/// contain no spaces and Windows OCR sometimes spaces the glyphs out, while
/// English names do contain spaces and OCR varies the run lengths — so the
/// dispatch is here rather than at the call site, where getting it wrong
/// means every frame skips with a name that looks perfectly correct in the
/// skip reason.
pub fn resolve_name<'catalog>(
    text: &str,
    catalog: &'catalog Catalog,
    language: ProfileLanguage,
) -> Option<&'catalog CatalogAsset> {
    match language {
        ProfileLanguage::TraditionalChinese => resolve_zh_name(text, catalog),
        ProfileLanguage::English => resolve_en_name(text, catalog),
    }
}

/// Strips all whitespace (zh-TW names contain none) and resolves against the
/// closed Traditional Chinese lexicon, aliases included.
pub fn resolve_zh_name<'catalog>(
    text: &str,
    catalog: &'catalog Catalog,
) -> Option<&'catalog CatalogAsset> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    catalog.by_name_zh_tw(&compact)
}

/// English variant: collapses runs of whitespace to single spaces and resolves
/// case-insensitively.
pub fn resolve_en_name<'catalog>(
    text: &str,
    catalog: &'catalog Catalog,
) -> Option<&'catalog CatalogAsset> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    catalog.by_name_en(&collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaced_out_glyphs_still_hit_exactly() {
        let catalog = ptt_catalog::poe2();
        let asset = resolve_zh_name("神 聖 石", catalog).unwrap();
        assert_eq!(asset.name_en, "Divine Orb");
        assert!(
            resolve_zh_name("神聖右", catalog).is_none(),
            "no fuzzy repair"
        );
        assert!(resolve_zh_name("  ", catalog).is_none());
    }

    #[test]
    fn english_resolution_is_case_insensitive_exact() {
        let catalog = ptt_catalog::poe2();
        assert!(resolve_en_name("divine  orb", catalog).is_some());
        assert!(resolve_en_name("divine orbs", catalog).is_none());
    }
}
