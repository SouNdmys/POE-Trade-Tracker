//! Closed per-game asset catalogs and the OCR lexicons derived from them.
//!
//! Recognition never mints an asset identity from an open OCR string: a name
//! must hit one of these catalogs exactly (after canonicalization) or the
//! frame is skipped. Catalog data files are embedded and SHA-256 pinned, and
//! the pin is re-verified on first access — a corrupted build fails closed
//! instead of recognizing against a silently different catalog.

use std::collections::HashMap;
use std::sync::OnceLock;

use ptt_core::Game;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Embedded POE2 catalog: 660 currency-exchange assets transcribed from poe2db
/// (Traditional Chinese primary, English secondary), carried over verbatim from
/// POE2-Trade-Tracker-Electron `data/currencies/currency_master.zh_tw.json`.
pub const POE2_CATALOG_JSON: &str = include_str!("../data/poe2/currency_master.zh_tw.json");
pub const POE2_CATALOG_SHA256: &str =
    "d238ba276402eca7cb426f3384ab30b2fb69fd31d9a8aeb9c3ea92843b244b59";
pub const POE2_CATALOG_ENTRIES: usize = 660;

/// POE1 seed catalog: the currencies the annotated fixture corpus actually
/// contains, with English names only.
///
/// This is a seed, not the game's currency list. POE1 has an order of
/// magnitude more tradeable items; they land here as the data is authored.
/// A partial catalog is safe because recognition is closed-lexicon and
/// fail-skip: an absent currency makes the frame skip, never a wrong id.
///
/// Traditional Chinese names are deliberately absent rather than mirrored
/// from English, so the zh-TW route refuses to start instead of matching
/// Chinese OCR output against English strings.
pub const POE1_CATALOG_JSON: &str = include_str!("../data/poe1/currency_seed.en.json");
pub const POE1_CATALOG_SHA256: &str =
    "2c3849d7ed9613d2eb16936db92d4ad6a04a56046cce8d575b866a6195abd8de";
pub const POE1_CATALOG_ENTRIES: usize = 9;

/// One tradeable asset in a game's currency exchange.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogAsset {
    pub id: String,
    /// Empty when that language has not been authored for this game yet.
    /// Callers must reject an empty name rather than match against it.
    #[serde(default)]
    pub name_zh_tw: String,
    pub name_en: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub in_game_category: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub base_family: Option<String>,
    #[serde(default)]
    pub is_tradeable: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub trade_list_order: Option<u32>,
}

/// Immutable, indexed catalog for one game.
#[derive(Debug)]
pub struct Catalog {
    game: Game,
    assets: Vec<CatalogAsset>,
    by_id: HashMap<String, usize>,
    by_name_zh_tw: HashMap<String, usize>,
    by_name_en_lower: HashMap<String, usize>,
}

#[derive(Debug)]
pub enum CatalogError {
    Parse(serde_json::Error),
    DuplicateId(String),
    DuplicateName(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Parse(error) => write!(f, "catalog JSON is invalid: {error}"),
            CatalogError::DuplicateId(id) => write!(f, "duplicate asset id: {id}"),
            CatalogError::DuplicateName(name) => write!(f, "duplicate asset name: {name}"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl Catalog {
    /// Parses and indexes a catalog. Primary names must be unique per language;
    /// aliases are indexed only where they do not collide with a primary name
    /// (primary names always win, deterministically).
    pub fn from_json(game: Game, json: &str) -> Result<Self, CatalogError> {
        let assets: Vec<CatalogAsset> = serde_json::from_str(json).map_err(CatalogError::Parse)?;

        let mut by_id = HashMap::with_capacity(assets.len());
        let mut by_name_zh_tw = HashMap::with_capacity(assets.len());
        let mut by_name_en_lower = HashMap::with_capacity(assets.len());

        for (index, asset) in assets.iter().enumerate() {
            if by_id.insert(asset.id.clone(), index).is_some() {
                return Err(CatalogError::DuplicateId(asset.id.clone()));
            }
            // A language that has not been authored for this game leaves the
            // name blank. Blanks are skipped rather than indexed: indexing
            // them would make every such asset a duplicate of every other,
            // and would make the empty string a lookup key that matches an
            // arbitrary currency.
            if !asset.name_zh_tw.trim().is_empty()
                && by_name_zh_tw
                    .insert(asset.name_zh_tw.clone(), index)
                    .is_some()
            {
                return Err(CatalogError::DuplicateName(asset.name_zh_tw.clone()));
            }
            if !asset.name_en.trim().is_empty()
                && by_name_en_lower
                    .insert(asset.name_en.to_lowercase(), index)
                    .is_some()
            {
                return Err(CatalogError::DuplicateName(asset.name_en.clone()));
            }
        }
        for (index, asset) in assets.iter().enumerate() {
            for alias in &asset.aliases {
                by_name_zh_tw.entry(alias.clone()).or_insert(index);
                by_name_en_lower
                    .entry(alias.to_lowercase())
                    .or_insert(index);
            }
        }

        Ok(Self {
            game,
            assets,
            by_id,
            by_name_zh_tw,
            by_name_en_lower,
        })
    }

    pub fn game(&self) -> Game {
        self.game
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    pub fn assets(&self) -> &[CatalogAsset] {
        &self.assets
    }

    pub fn by_id(&self, id: &str) -> Option<&CatalogAsset> {
        self.by_id.get(id).map(|&index| &self.assets[index])
    }

    /// Exact Traditional Chinese name (or alias) lookup.
    pub fn by_name_zh_tw(&self, name: &str) -> Option<&CatalogAsset> {
        self.by_name_zh_tw
            .get(name)
            .map(|&index| &self.assets[index])
    }

    /// Case-insensitive exact English name (or alias) lookup.
    pub fn by_name_en(&self, name: &str) -> Option<&CatalogAsset> {
        self.by_name_en_lower
            .get(&name.to_lowercase())
            .map(|&index| &self.assets[index])
    }

    /// Closed lexicon of Traditional Chinese primary names, in catalog order.
    pub fn zh_tw_lexicon(&self) -> impl Iterator<Item = &str> {
        self.assets.iter().map(|asset| asset.name_zh_tw.as_str())
    }

    /// Closed lexicon of English primary names, in catalog order.
    pub fn en_lexicon(&self) -> impl Iterator<Item = &str> {
        self.assets.iter().map(|asset| asset.name_en.as_str())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

/// The POE2 catalog. First access verifies the embedded data against its
/// pinned SHA-256 and panics on mismatch (fail-closed startup, same posture as
/// the pinned OCR model assets).
pub fn poe2() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let actual = sha256_hex(POE2_CATALOG_JSON.as_bytes());
        assert_eq!(
            actual, POE2_CATALOG_SHA256,
            "embedded POE2 catalog does not match its pinned SHA-256; \
             the build is corrupt and recognition must not proceed"
        );
        Catalog::from_json(Game::Poe2, POE2_CATALOG_JSON)
            .expect("pinned POE2 catalog data failed to parse")
    })
}

/// The POE1 seed catalog, verified against its pin on first access.
pub fn poe1() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let actual = sha256_hex(POE1_CATALOG_JSON.as_bytes());
        assert_eq!(
            actual, POE1_CATALOG_SHA256,
            "embedded POE1 catalog does not match its pinned SHA-256;              the build is corrupt and recognition must not proceed"
        );
        Catalog::from_json(Game::Poe1, POE1_CATALOG_JSON)
            .expect("pinned POE1 catalog data failed to parse")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_poe1_seed_matches_pin_and_is_english_only() {
        let catalog = poe1();
        assert_eq!(catalog.game(), Game::Poe1);
        assert_eq!(catalog.len(), POE1_CATALOG_ENTRIES);
        for asset in catalog.assets() {
            assert!(
                !asset.name_en.trim().is_empty(),
                "{} has no English name",
                asset.id
            );
        }
        // Pinned deliberately: when the Traditional Chinese names are
        // authored this test fails, which is the signal to enable the zh-TW
        // route for POE1 rather than something to quietly relax.
        assert!(
            catalog
                .assets()
                .iter()
                .all(|asset| asset.name_zh_tw.trim().is_empty()),
            "POE1 Traditional Chinese names now exist; enable the zh-TW route              and update this test"
        );
    }

    #[test]
    fn a_blank_name_is_not_indexed_as_a_lookup_key() {
        // Nine assets all with an empty zh-TW name must not collide as
        // duplicates, and none of them may be findable by the empty string.
        assert!(poe1().by_name_zh_tw("").is_none());
    }

    #[test]
    fn embedded_poe2_catalog_matches_pin_and_shape() {
        let catalog = poe2();
        assert_eq!(catalog.game(), Game::Poe2);
        assert_eq!(catalog.len(), POE2_CATALOG_ENTRIES);
    }

    #[test]
    fn known_assets_resolve_in_both_languages() {
        let catalog = poe2();
        let divine = catalog
            .by_name_zh_tw("神聖石")
            .expect("Divine Orb must exist under its zh-TW name");
        assert_eq!(divine.name_en, "Divine Orb");
        let exalted = catalog
            .by_name_en("exalted orb")
            .expect("Exalted Orb must resolve case-insensitively");
        assert_eq!(exalted.name_zh_tw, "崇高石");
        assert_eq!(catalog.by_id(&divine.id).unwrap().name_zh_tw, "神聖石");
    }

    #[test]
    fn lexicons_are_closed_and_unique() {
        let catalog = poe2();
        let zh: Vec<&str> = catalog.zh_tw_lexicon().collect();
        let en: Vec<&str> = catalog.en_lexicon().collect();
        assert_eq!(zh.len(), POE2_CATALOG_ENTRIES);
        assert_eq!(en.len(), POE2_CATALOG_ENTRIES);
        let zh_unique: std::collections::HashSet<&str> = zh.iter().copied().collect();
        let en_unique: std::collections::HashSet<String> =
            en.iter().map(|name| name.to_lowercase()).collect();
        assert_eq!(zh_unique.len(), zh.len(), "zh-TW names must be unique");
        assert_eq!(en_unique.len(), en.len(), "EN names must be unique");
    }

    #[test]
    fn all_assets_are_tradeable_and_active() {
        let catalog = poe2();
        assert!(catalog.assets().iter().all(|asset| asset.is_tradeable));
        assert!(catalog.assets().iter().all(|asset| asset.is_active));
    }

    #[test]
    fn duplicate_primary_names_are_rejected() {
        let json = r#"[
            {"id":"a","name_zh_tw":"同名","name_en":"Same"},
            {"id":"b","name_zh_tw":"同名","name_en":"Other"}
        ]"#;
        assert!(matches!(
            Catalog::from_json(Game::Poe2, json),
            Err(CatalogError::DuplicateName(_))
        ));
    }
}
