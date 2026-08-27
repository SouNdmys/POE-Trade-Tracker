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
/// (Traditional Chinese primary, English secondary), carried over from
/// POE2-Trade-Tracker-Electron `data/currencies/currency_master.zh_tw.json`.
///
/// Carried over is not the same as carried over whole: the file arrived with
/// thirty fields per entry and [`CatalogAsset`] reads eleven. The other
/// nineteen were dropped rather than embedded and ignored, because a field
/// sitting in a data file is an invitation to wire it up, and because they are
/// weight in every build.
pub const POE2_CATALOG_JSON: &str = include_str!("../data/poe2/currency_master.zh_tw.json");
pub const POE2_CATALOG_SHA256: &str =
    "8094644293bbd9c6aaeeae45eeae35a6ba92c4180ec9359d76498538735dbe28";
pub const POE2_CATALOG_ENTRIES: usize = 660;

/// POE1 catalog: 1,047 assets across 16 categories, transcribed from in-game
/// selector screenshots in both languages.
///
/// Converted from `POE1-Trade-Tracker/resources/catalogs/poe1-market-assets.json`
/// (upstream SHA-256 `2a778e7af6e7d2fa7b9250271ca01bc87e75df4170ebe13d87d6da2d934a06dc`),
/// which also carries the 60×51 icon templates this crate does not yet use.
///
/// Traditional Chinese names were transcribed from zh-TW selector screenshots
/// of the same panel (2026-08-16). The join is positional: within a category
/// the selector reads left-to-right, top-to-bottom, and the English catalogue
/// came from that same grid, so rank within `trade_list_order` binds the pair.
/// Four entries were added at the same time — the screenshots showed items the
/// English source predates — and their English names come from external
/// lookups rather than from a screenshot.
///
/// Every asset carries a 1/1 minimum unit upstream, because selector
/// screenshots do not show trade increments. That is inherited rather than
/// verified.
pub const POE1_CATALOG_JSON: &str = include_str!("../data/poe1/market_assets.en.json");
pub const POE1_CATALOG_SHA256: &str =
    "45bb54a2d48fe11c6972ce2615a73bff3ee4906786368b979b49ed81894a5d1f";
pub const POE1_CATALOG_ENTRIES: usize = 1_047;

/// How the two catalogues were transcribed, kept beside them and deliberately
/// *not* embedded: which in-game category each asset sits in and where it sits
/// in the selector's order.
///
/// That pairing is what bound 1,043 Traditional Chinese names to their English
/// counterparts — the selector reads left-to-right, top-to-bottom and both
/// transcriptions came from the same grid — so it is worth keeping for the next
/// time the game adds items. It is worth nothing at runtime, where a name maps
/// to an id and the id is all anything downstream uses, so it stays out of the
/// binary.
pub const POE1_TRANSCRIPTION_ORDER_PATH: &str = "data/poe1/transcription-order.json";
pub const POE2_TRANSCRIPTION_ORDER_PATH: &str = "data/poe2/transcription-order.json";

/// One tradeable asset in a game's currency exchange.
///
/// Four fields, because the catalogue answers one question: which asset is
/// this name. Everything the program does after that — listings, depth,
/// routes, advice — keys on the id and never looks back here. Anything
/// describing what a currency *does* in game would be weight in every build
/// and an invitation to make decisions from it.
// deny_unknown_fields:数据文件手录,`name_zhtw` 这种拼错的字段名以前会
// 被静默丢掉,落成一个中文名为空、永远识别不到的资产。拒收比默吞好。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogAsset {
    pub id: String,
    /// Empty when that language has not been authored for this game yet.
    /// Callers must reject an empty name rather than match against it.
    #[serde(default)]
    pub name_zh_tw: String,
    pub name_en: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Immutable, indexed catalog for one game.
/// A Traditional Chinese name with every space removed.
///
/// Applied to the index and to the query, which is the entire point: the two
/// used to disagree. Lookups stripped whitespace — Windows OCR returns these
/// names spaced glyph by glyph, so they have to — while the index stored the
/// catalogue's own spelling verbatim. Any catalogue name holding a space was
/// therefore unreachable, which was 42 of POE2's 660 entries: every uncut gem,
/// whose names all read `（等級 20）`. POE1 hid the bug entirely, its
/// Traditional Chinese names having no spaces at all.
///
/// Written Chinese does not use spaces between words, so removing them cannot
/// merge two distinct names — a duplicate key here would mean two entries that
/// differ only in spacing, which the constructor still rejects.
#[must_use]
pub fn compact_zh(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace()).collect()
}

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
                    .insert(compact_zh(&asset.name_zh_tw), index)
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
                by_name_zh_tw.entry(compact_zh(alias)).or_insert(index);
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
            .get(&compact_zh(name))
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

/// The POE1 English catalog, verified against its pin on first access.
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
    fn embedded_poe1_catalog_matches_pin_and_is_fully_bilingual() {
        let catalog = poe1();
        assert_eq!(catalog.game(), Game::Poe1);
        assert_eq!(catalog.len(), POE1_CATALOG_ENTRIES);
        for asset in catalog.assets() {
            assert!(
                !asset.name_en.trim().is_empty(),
                "{} has no English name",
                asset.id
            );
            // The zh-TW route matches on exact catalogue hits, so one blank
            // name is one asset the Chinese client can never identify — and
            // it fails as a skipped frame, not as an error.
            assert!(
                !asset.name_zh_tw.trim().is_empty(),
                "{} has no Traditional Chinese name",
                asset.id
            );
            assert_ne!(
                asset.name_zh_tw, asset.name_en,
                "{} mirrors its English name instead of being translated",
                asset.id
            );
        }
    }

    #[test]
    fn poe1_names_bind_to_the_asset_the_screenshots_showed() {
        // Spot checks across the transcription's seams: the first category
        // read, one whose section headers use different words than its items
        // (Essence of Woe is 悲痛, under a 災禍 header), and the two ends of
        // the run of cards inserted after the English source was written.
        let catalog = poe1();
        for (id, name_zh_tw) in [
            ("chaos-orb", "混沌石"),
            ("divine-orb", "神聖石"),
            ("whispering-essence-of-woe", "悲痛之低語精髓"),
            ("the-undisputed", "不許你胡說"),
            ("the-visionary", "空想家"),
            ("ukatoa-s-ducat", "烏卡托瓦的達克特"),
        ] {
            let asset = catalog
                .by_id(id)
                .unwrap_or_else(|| panic!("{id} is missing from the catalogue"));
            assert_eq!(asset.name_zh_tw, name_zh_tw, "{id}");
            assert_eq!(
                catalog.by_name_zh_tw(name_zh_tw).map(|a| a.id.as_str()),
                Some(id)
            );
        }
    }

    #[test]
    fn a_blank_name_is_not_indexed_as_a_lookup_key() {
        // Both shipped catalogues are now fully bilingual, so this is tested
        // against a synthetic one — otherwise the branch that skips blanks
        // has no coverage and would break unnoticed the next time a catalogue
        // ships with a language only partly authored.
        let json = r#"[
            {"id":"a","name_zh_tw":"","name_en":"First"},
            {"id":"b","name_zh_tw":"","name_en":"Second"}
        ]"#;
        let catalog = Catalog::from_json(Game::Poe1, json).expect("blanks must not collide");
        assert!(catalog.by_name_zh_tw("").is_none());
        assert!(poe1().by_name_zh_tw("").is_none());
    }

    /// 和 POE1 一样查双语齐全:`name_zh_tw` 带 `#[serde(default)]`,漏填
    /// 不报错、只是那个通货在中文客户端永远识别不到。新赛季批量录入最
    /// 容易犯的正是"英文先填、中文待补"。
    #[test]
    fn embedded_poe2_catalog_matches_pin_and_is_fully_bilingual() {
        let catalog = poe2();
        assert_eq!(catalog.game(), Game::Poe2);
        assert_eq!(catalog.len(), POE2_CATALOG_ENTRIES);
        for asset in catalog.assets() {
            assert!(
                !asset.name_en.trim().is_empty(),
                "{} has no English name",
                asset.id
            );
            assert!(
                !asset.name_zh_tw.trim().is_empty(),
                "{} has no Traditional Chinese name",
                asset.id
            );
        }
    }

    #[test]
    fn a_misspelled_field_is_refused_not_silently_dropped() {
        let json = r#"[{"id":"a","name_zhtw":"某","name_en":"First"}]"#;
        assert!(
            Catalog::from_json(Game::Poe2, json).is_err(),
            "a typoed field name must be a parse error, not a silent blank"
        );
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

    /// The provenance file still describes the catalogue beside it.
    ///
    /// It is not embedded, so nothing at runtime would notice it drifting —
    /// but the next transcription joins against it, and a stale order file
    /// would bind Chinese names to the wrong English ones exactly as silently
    /// as it once bound them correctly.
    #[test]
    fn the_transcription_order_still_matches_the_catalogue() {
        #[derive(serde::Deserialize)]
        struct OrderEntry {
            id: String,
            in_game_category: Option<String>,
        }
        for (path, catalog, categories) in [
            (POE1_TRANSCRIPTION_ORDER_PATH, poe1(), Some(16)),
            (POE2_TRANSCRIPTION_ORDER_PATH, poe2(), None),
        ] {
            let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
            let text = std::fs::read_to_string(&full)
                .unwrap_or_else(|error| panic!("{}: {error}", full.display()));
            let entries: Vec<OrderEntry> = serde_json::from_str(&text).expect("order json");
            assert_eq!(
                entries.len(),
                catalog.len(),
                "{path} describes {} assets, the catalogue has {}",
                entries.len(),
                catalog.len()
            );
            for entry in &entries {
                assert!(
                    catalog.by_id(&entry.id).is_some(),
                    "{path} names {}, which is not in the catalogue",
                    entry.id
                );
            }
            if let Some(expected) = categories {
                let seen: std::collections::BTreeSet<_> = entries
                    .iter()
                    .filter_map(|entry| entry.in_game_category.as_deref())
                    .collect();
                assert_eq!(seen.len(), expected, "categories in {path}");
            }
        }
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

#[cfg(test)]
mod cost_tests {
    use super::*;
    use std::time::Instant;

    /// Where the catalogue's cost actually falls.
    ///
    /// Recorded because the intuition is that carrying more per-asset data
    /// slows recognition down, and it does not: identity resolution is a hash
    /// lookup keyed on the name, so it is independent of both how many fields
    /// an entry has and how many entries there are. What the data costs is
    /// parsing, once, at first access. This test prints all three so the
    /// trade-off is a measurement rather than an argument.
    #[test]
    fn parsing_is_where_the_catalogue_costs_anything() {
        let started = Instant::now();
        let poe1 = Catalog::from_json(Game::Poe1, POE1_CATALOG_JSON).expect("poe1");
        let poe1_parse = started.elapsed();
        let started = Instant::now();
        let poe2 = Catalog::from_json(Game::Poe2, POE2_CATALOG_JSON).expect("poe2");
        let poe2_parse = started.elapsed();

        // A name lookup, which is what a frame actually does.
        let started = Instant::now();
        let rounds = 100_000;
        let mut hits = 0usize;
        for _ in 0..rounds {
            if poe1.by_name_zh_tw("混沌石").is_some() {
                hits += 1;
            }
        }
        let per_lookup = started.elapsed() / rounds;
        assert_eq!(hits, rounds as usize);

        println!(
            "poe1 parse {poe1_parse:?} ({} entries)  poe2 parse {poe2_parse:?} ({} entries)  \
             lookup {per_lookup:?}",
            poe1.len(),
            poe2.len()
        );
        // Parsing happens once per process, behind a OnceLock. A tenth of a
        // second would still be invisible; anything past that is a real
        // startup cost worth trimming.
        assert!(
            poe1_parse + poe2_parse < std::time::Duration::from_millis(100),
            "parsing both catalogues costs {:?}",
            poe1_parse + poe2_parse
        );
        // The per-frame path. If this ever stops being sub-microsecond,
        // matching has quietly become something other than a hash lookup.
        assert!(
            per_lookup < std::time::Duration::from_micros(1),
            "a name lookup costs {per_lookup:?}"
        );
    }
}
