//! Currency ids, as the reader knows them.
//!
//! Every layer under the interface speaks catalogue ids, and none of them
//! should stop: `chaos-orb` is stable, unambiguous, and the right key for a
//! map, a probe queue or a log line. It is also the wrong thing to put in
//! front of a person, who has to translate it back into the currency they saw
//! in game before they can act on it — and who, running a Chinese interface,
//! is being shown a database key in a language they did not choose.
//!
//! One module because there are two holders of this knowledge — the shell,
//! which has the settings, and the radar's table delegate, which does not —
//! and two copies would drift.

use ptt_runtime::domain::Catalog;
use ptt_settings::UiLanguage;
use ptt_trade_domain::MarketAssetId;

/// One asset's name in the interface's language.
///
/// Falls back to the id, deliberately. A blank would lose a real reading, and
/// an id on screen is the visible edge of a catalogue that needs an entry —
/// which is worth seeing rather than hiding.
#[must_use]
pub fn asset_name(catalog: &Catalog, language: UiLanguage, asset_id: &str) -> String {
    let Some(asset) = catalog.by_id(asset_id) else {
        return asset_id.to_owned();
    };
    // Empty means that language has not been authored for this entry, which
    // the catalogue documents as "reject rather than match against".
    let name = match language {
        UiLanguage::Chinese => asset.name_zh_tw.trim(),
        UiLanguage::English => asset.name_en.trim(),
    };
    if name.is_empty() {
        asset_id.to_owned()
    } else {
        name.to_owned()
    }
}

/// A pair, as an arrow between two names.
#[must_use]
pub fn pair_name(catalog: &Catalog, language: UiLanguage, from: &str, to: &str) -> String {
    format!(
        "{} → {}",
        asset_name(catalog, language, from),
        asset_name(catalog, language, to)
    )
}

/// A whole route, arrow-joined.
#[must_use]
pub fn route_name(catalog: &Catalog, language: UiLanguage, path: &[MarketAssetId]) -> String {
    path.iter()
        .map(|asset| asset_name(catalog, language, asset.as_str()))
        .collect::<Vec<_>>()
        .join(" → ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_id_reads_as_its_name_in_either_language() {
        let catalog = ptt_runtime::domain::poe2_catalog();
        let id = catalog.assets()[0].id.clone();
        for language in [UiLanguage::English, UiLanguage::Chinese] {
            let name = asset_name(catalog, language, &id);
            assert!(!name.is_empty());
            // The point of the module: what the reader sees is not the key.
            assert_ne!(name, id, "{language:?} name should not be the id");
        }
    }

    /// The fallback is the id, not a blank.
    #[test]
    fn an_unknown_id_reads_as_itself() {
        let catalog = ptt_runtime::domain::poe2_catalog();
        assert_eq!(
            asset_name(catalog, UiLanguage::Chinese, "not-a-currency"),
            "not-a-currency"
        );
    }
}
