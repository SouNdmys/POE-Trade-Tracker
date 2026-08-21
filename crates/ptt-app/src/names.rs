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

use ptt_runtime::domain::{Catalog, CatalogAsset};
use ptt_settings::UiLanguage;
use ptt_trade_domain::MarketAssetId;

/// The catalogue's own key for an id that has been through the domain.
///
/// The catalogue writes `divine_orb`; [`MarketAssetId`] permits only lowercase
/// letters, digits and hyphens, so the same currency is `divine-orb` by the
/// time it reaches a page. `ptt_runtime::live::domain_asset_id` is the
/// crossing in the other direction.
fn catalog_key(asset_id: &str) -> String {
    asset_id.replace('-', "_")
}

/// The catalogue entry behind an id in either spelling.
fn entry<'a>(catalog: &'a Catalog, asset_id: &str) -> Option<&'a CatalogAsset> {
    catalog
        .by_id(asset_id)
        .or_else(|| catalog.by_id(&catalog_key(asset_id)))
}

/// One asset's name in the interface's language.
///
/// Falls back to the id, deliberately. A blank would lose a real reading, and
/// an id on screen is the visible edge of a catalogue that needs an entry —
/// which is worth seeing rather than hiding.
#[must_use]
pub fn asset_name(catalog: &Catalog, language: UiLanguage, asset_id: &str) -> String {
    let Some(asset) = entry(catalog, asset_id) else {
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

/// A string reduced to what a search should compare.
///
/// Case folded, with everything that is not a letter or a digit dropped, so
/// `Divine Orb`, `divine-orb` and `divine_orb` are one key and a reader can
/// type whichever they have in mind — or just the part of it they remember.
#[must_use]
pub fn fold_query(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Everything one currency can be found by.
///
/// Both names, whatever the interface is currently speaking, plus the id and
/// any aliases. Someone running a Chinese interface still knows the English
/// name from the wiki, from a trade whisper, or from this program's own logs,
/// and a picker that refuses `div` because the list says 神聖石 is a picker
/// they have to scroll.
#[must_use]
pub fn search_keys(asset: &CatalogAsset) -> Vec<String> {
    let mut keys = vec![
        fold_query(&asset.name_en),
        fold_query(&asset.name_zh_tw),
        fold_query(&asset.id),
    ];
    keys.extend(asset.aliases.iter().map(|alias| fold_query(alias)));
    keys.retain(|key| !key.is_empty());
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poe2() -> &'static Catalog {
        ptt_runtime::domain::poe2_catalog()
    }

    /// The id that reaches a page has been through the domain, which spells it
    /// with hyphens. Looking that up in a catalogue keyed by underscores is
    /// the mistake this module exists to make impossible.
    #[test]
    fn a_domain_spelled_id_still_finds_its_name() {
        assert_eq!(
            asset_name(poe2(), UiLanguage::Chinese, "divine-orb"),
            "神聖石"
        );
        assert_eq!(
            asset_name(poe2(), UiLanguage::English, "divine-orb"),
            "Divine Orb"
        );
        // And the catalogue's own spelling, which the pickers hold.
        assert_eq!(
            asset_name(poe2(), UiLanguage::English, "divine_orb"),
            "Divine Orb"
        );
    }

    /// The fallback is the id, not a blank.
    #[test]
    fn an_unknown_id_reads_as_itself() {
        assert_eq!(
            asset_name(poe2(), UiLanguage::Chinese, "not-a-currency"),
            "not-a-currency"
        );
    }

    #[test]
    fn a_currency_is_searchable_by_either_name_a_prefix_or_its_id() {
        let asset = poe2()
            .by_id("divine_orb")
            .expect("the catalogue holds divine orb");
        let keys = search_keys(asset);
        let finds = |query: &str| {
            let folded = fold_query(query);
            keys.iter().any(|key| key.contains(&folded))
        };
        for query in [
            "Divine Orb",
            "divine orb",
            "div",
            "DIVINE",
            "divine-orb",
            "神聖石",
        ] {
            assert!(finds(query), "{query:?} should find divine orb");
        }
        assert!(!finds("chaos"));
    }
}
