//! Versioned, crash-safe JSON settings store.
//!
//! Reading is lenient: a missing, unreadable, or malformed file yields safe
//! defaults, never a user-facing error. Writing is strict and atomic: pretty
//! JSON to a sibling temp file, fsync, then rename. A file written by a newer
//! schema puts the store into read-only mode instead of being clobbered.
//! (Same posture as POE Alarm's settings store, with a fresh v1 schema.)

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ptt_core::{ContentLanguage, Game, ProfileId};
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
const APP_DIR_NAME: &str = "PoeTradeTracker";
const SETTINGS_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiLanguage {
    #[default]
    English,
    Chinese,
}

/// A desktop-pixel rectangle (virtual-screen coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Per-profile calibration. Keyed by `ProfileId` display form (e.g.
/// `poe2-zh-TW`); regions are tied to the desktop resolution they were drawn
/// on. The user calibrates three text-only regions (icons deliberately
/// excluded — they degrade OCR): the "I need" name, the "I have" name, and
/// the order-tables area holding at most 12 ratio rows (6 available + 6
/// competing, sometimes fewer). The exchange panel shifts sideways when the
/// stash or character panels are open; calibration assumes the centered
/// default position, and a shifted panel simply fails the anchor/identity
/// gates and is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProfileSettings {
    #[serde(default)]
    pub need_name_region: Option<Region>,
    #[serde(default)]
    pub have_name_region: Option<Region>,
    #[serde(default)]
    pub tables_region: Option<Region>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    #[serde(default = "default_toggle_watch")]
    pub toggle_watch: String,
    #[serde(default = "default_toggle_hud")]
    pub toggle_hud: String,
    #[serde(default = "default_manual_capture")]
    pub manual_capture: String,
}

fn default_toggle_watch() -> String {
    "Ctrl+Alt+F10".to_string()
}
fn default_toggle_hud() -> String {
    "Alt+F11".to_string()
}
fn default_manual_capture() -> String {
    "Alt+F12".to_string()
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            toggle_watch: default_toggle_watch(),
            toggle_hud: default_toggle_hud(),
            manual_capture: default_manual_capture(),
        }
    }
}

/// Per-game algorithm tuning, keyed by [`Game::as_str`] in [`AppSettings`].
///
/// Settings hold plain strings and integers only — no domain types. The
/// conversion into `FreshnessPolicy`, `RiskThresholds`, asset ids and so on
/// happens where they are consumed (ptt-runtime), behind each type's own
/// validity gate, so a hand-edited bad value degrades to the shipped default
/// with a visible report line rather than poisoning a computation or making
/// the whole settings file unreadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketTuning {
    /// Settlement currencies (主要结算通货), in catalog slug form. These are
    /// the currencies every arbitrage cycle starts from — and therefore closes
    /// back into — and the first one is the anchor everything else is valued
    /// against. They must be the most liquid assets in the league.
    #[serde(default = "default_settlement_assets")]
    pub settlement_assets: Vec<String>,
    /// Focus targets: currencies the user actively trades in and out of.
    /// Empty means "every asset seen in the book", today's behavior.
    #[serde(default)]
    pub focus_assets: Vec<String>,
    /// Currencies routes may pass through but never end on.
    #[serde(default)]
    pub bridge_assets: Vec<String>,
    /// Currencies tracked for price only, excluded from every route.
    #[serde(default)]
    pub watch_only_assets: Vec<String>,
    #[serde(default)]
    pub freshness: FreshnessTuning,
    #[serde(default)]
    pub convert: ConvertTuning,
    #[serde(default)]
    pub radar: RadarTuning,
    #[serde(default)]
    pub risk: RiskTuning,
    /// How much history each report page loads, in hours. Must reach past the
    /// red freshness band: a window shorter than `usable_seconds` can never
    /// even load the data the yellow and red lights exist to warn about.
    #[serde(default = "default_report_window_hours")]
    pub report_window_hours: u64,
}

fn default_settlement_assets() -> Vec<String> {
    vec!["divine-orb".to_owned(), "chaos-orb".to_owned()]
}

fn default_report_window_hours() -> u64 {
    24
}

impl Default for MarketTuning {
    fn default() -> Self {
        Self {
            settlement_assets: default_settlement_assets(),
            focus_assets: Vec::new(),
            bridge_assets: Vec::new(),
            watch_only_assets: Vec::new(),
            freshness: FreshnessTuning::default(),
            convert: ConvertTuning::default(),
            radar: RadarTuning::default(),
            risk: RiskTuning::default(),
            report_window_hours: default_report_window_hours(),
        }
    }
}

/// The freshness traffic light, in seconds of data age. Green (fresh) is
/// trusted as-is; yellow (usable) means verify the rate in game before acting
/// on it; red (stale, and archived past `stale_seconds`) is excluded from
/// execution by default and asks for a recapture. Values follow the league's
/// capture rhythm, not wall-clock precision — the enforcement (strict
/// ordering, non-zero) lives in `FreshnessPolicy::try_new` at the consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessTuning {
    #[serde(default = "default_fresh_seconds")]
    pub fresh_seconds: u64,
    #[serde(default = "default_usable_seconds")]
    pub usable_seconds: u64,
    #[serde(default = "default_stale_seconds")]
    pub stale_seconds: u64,
    /// Maximum capture-time spread between the legs of one route before it is
    /// flagged as spanning different market moments. Half the green window:
    /// with green at two hours, the old 600s default would flag nearly every
    /// multi-leg route built from separately captured pairs.
    #[serde(default = "default_capture_skew_seconds")]
    pub capture_skew_seconds: u64,
}

fn default_fresh_seconds() -> u64 {
    2 * 60 * 60
}
fn default_usable_seconds() -> u64 {
    6 * 60 * 60
}
fn default_stale_seconds() -> u64 {
    24 * 60 * 60
}
fn default_capture_skew_seconds() -> u64 {
    60 * 60
}

impl Default for FreshnessTuning {
    fn default() -> Self {
        Self {
            fresh_seconds: default_fresh_seconds(),
            usable_seconds: default_usable_seconds(),
            stale_seconds: default_stale_seconds(),
            capture_skew_seconds: default_capture_skew_seconds(),
        }
    }
}

/// Knobs for the Convert page ("I hold X and want Y").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertTuning {
    /// The sizes priced when the user has not typed a holding, in whole units.
    #[serde(default = "default_convert_sizes")]
    pub sizes: Vec<u64>,
    /// Route search depth. The engine accepts 1..=4; out-of-range values fail
    /// its own validation and the report says so.
    #[serde(default = "default_max_hops")]
    pub max_hops: u64,
}

fn default_convert_sizes() -> Vec<u64> {
    vec![1, 10, 100]
}
fn default_max_hops() -> u64 {
    3
}

impl Default for ConvertTuning {
    fn default() -> Self {
        Self {
            sizes: default_convert_sizes(),
            max_hops: default_max_hops(),
        }
    }
}

/// Knobs for the opportunity radar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadarTuning {
    /// What the radar assumes you are willing to put in, in whole units of
    /// each settlement currency it scans from.
    #[serde(default = "default_radar_stake")]
    pub stake: u64,
    /// Total node-expansion budget shared across the whole scan.
    #[serde(default = "default_radar_expansions")]
    pub max_total_expansions: u64,
    /// How many ranked items the page shows.
    #[serde(default = "default_radar_results")]
    pub max_results: u64,
    /// Opportunities below this margin are reported as rejections, not items.
    #[serde(default = "default_minimum_profit_basis_points")]
    pub minimum_profit_basis_points: u64,
}

fn default_radar_stake() -> u64 {
    10
}
fn default_radar_expansions() -> u64 {
    60_000
}
fn default_radar_results() -> u64 {
    12
}
fn default_minimum_profit_basis_points() -> u64 {
    100
}

impl Default for RadarTuning {
    fn default() -> Self {
        Self {
            stake: default_radar_stake(),
            max_total_expansions: default_radar_expansions(),
            max_results: default_radar_results(),
            minimum_profit_basis_points: default_minimum_profit_basis_points(),
        }
    }
}

/// Liquidity and outlier thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskTuning {
    /// Visible depth below this stock count is flagged as thin liquidity.
    #[serde(default = "default_thin_liquidity_stock")]
    pub thin_liquidity_stock: u64,
    /// A listing whose rate differs from its side's baseline by more than this
    /// factor is a price outlier. The book enforces its own minimum of 2.
    #[serde(default = "default_outlier_factor")]
    pub top_book_outlier_factor: u64,
}

fn default_thin_liquidity_stock() -> u64 {
    100
}
fn default_outlier_factor() -> u64 {
    3
}

impl Default for RiskTuning {
    fn default() -> Self {
        Self {
            thin_liquidity_stock: default_thin_liquidity_stock(),
            top_book_outlier_factor: default_outlier_factor(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    pub schema_version: u32,
    #[serde(default)]
    pub ui_language: UiLanguage,
    #[serde(default = "default_active_profile")]
    pub active_profile: ProfileId,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileSettings>,
    #[serde(default)]
    pub hotkeys: Hotkeys,
    /// Per-game algorithm tuning, keyed by [`Game::as_str`] ("poe1"/"poe2").
    /// Absent games use [`MarketTuning::default`].
    #[serde(default)]
    pub market: BTreeMap<String, MarketTuning>,
}

fn default_active_profile() -> ProfileId {
    ProfileId::new(Game::Poe2, ContentLanguage::TraditionalChinese)
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            ui_language: UiLanguage::default(),
            active_profile: default_active_profile(),
            profiles: BTreeMap::new(),
            hotkeys: Hotkeys::default(),
            market: BTreeMap::new(),
        }
    }
}

impl AppSettings {
    pub fn profile_mut(&mut self, profile: ProfileId) -> &mut ProfileSettings {
        self.profiles.entry(profile.to_string()).or_default()
    }

    pub fn profile(&self, profile: ProfileId) -> Option<&ProfileSettings> {
        self.profiles.get(&profile.to_string())
    }

    /// The tuning for one game, defaults where the file has none. Returns a
    /// clone: tuning is read once per report build, not per frame, and the
    /// caller must not observe later edits mid-computation.
    pub fn market_tuning(&self, game: Game) -> MarketTuning {
        self.market.get(game.as_str()).cloned().unwrap_or_default()
    }

    pub fn market_tuning_mut(&mut self, game: Game) -> &mut MarketTuning {
        self.market.entry(game.as_str().to_owned()).or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// File parsed at a supported schema.
    Loaded,
    /// No file / unreadable / malformed — defaults returned.
    Defaults,
    /// File written by a newer schema — defaults returned, saving refused.
    FutureSchemaReadOnly { detected: u32 },
}

#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub settings: AppSettings,
    pub status: LoadStatus,
}

#[derive(Debug)]
pub enum SaveError {
    SchemaTooNew { detected: u32, supported: u32 },
    Io(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::SchemaTooNew {
                detected,
                supported,
            } => write!(
                f,
                "settings on disk use schema {detected}, newer than supported {supported}"
            ),
            SaveError::Io(error) => write!(f, "settings write failed: {error}"),
        }
    }
}

impl std::error::Error for SaveError {}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Production location: `<local_app_data>\PoeTradeTracker\settings.json`.
    /// Injectable root so tests never touch the real profile.
    pub fn release_default_from(local_app_data: &Path) -> Self {
        Self {
            path: local_app_data.join(APP_DIR_NAME).join(SETTINGS_FILE_NAME),
        }
    }

    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> LoadedSettings {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return LoadedSettings {
                settings: AppSettings::default(),
                status: LoadStatus::Defaults,
            };
        };
        match serde_json::from_str::<AppSettings>(&raw) {
            Ok(settings) if settings.schema_version <= CURRENT_SCHEMA_VERSION => LoadedSettings {
                settings,
                status: LoadStatus::Loaded,
            },
            Ok(settings) => LoadedSettings {
                status: LoadStatus::FutureSchemaReadOnly {
                    detected: settings.schema_version,
                },
                settings: AppSettings::default(),
            },
            Err(_) => {
                // Tolerate a partially-known file: retry just the version gate so a
                // future schema is still detected as such rather than "malformed".
                if let Some(detected) = detect_schema_version(&raw)
                    && detected > CURRENT_SCHEMA_VERSION
                {
                    return LoadedSettings {
                        settings: AppSettings::default(),
                        status: LoadStatus::FutureSchemaReadOnly { detected },
                    };
                }
                LoadedSettings {
                    settings: AppSettings::default(),
                    status: LoadStatus::Defaults,
                }
            }
        }
    }

    /// Atomic save. Re-checks the on-disk schema immediately before replacing so
    /// a newer process that wrote the file mid-flight still wins.
    pub fn save(&self, settings: &AppSettings) -> Result<(), SaveError> {
        let mut normalized = settings.clone();
        normalized.schema_version = CURRENT_SCHEMA_VERSION;
        let body =
            serde_json::to_vec_pretty(&normalized).expect("settings model always serializes");

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(SaveError::Io)?;
        }
        self.refuse_future_schema()?;

        let temp_path = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(SaveError::Io)?;
            file.write_all(&body).map_err(SaveError::Io)?;
            file.sync_all().map_err(SaveError::Io)?;
        }
        if let Err(error) = self.refuse_future_schema() {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        fs::rename(&temp_path, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temp_path);
            SaveError::Io(error)
        })
    }

    fn refuse_future_schema(&self) -> Result<(), SaveError> {
        if let Ok(raw) = fs::read_to_string(&self.path)
            && let Some(detected) = detect_schema_version(&raw)
            && detected > CURRENT_SCHEMA_VERSION
        {
            return Err(SaveError::SchemaTooNew {
                detected,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

fn detect_schema_version(raw: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct VersionOnly {
        schema_version: u32,
    }
    serde_json::from_str::<VersionOnly>(raw)
        .ok()
        .map(|value| value.schema_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> SettingsStore {
        let dir = std::env::temp_dir()
            .join("ptt-settings-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        SettingsStore::at_path(dir.join(SETTINGS_FILE_NAME))
    }

    #[test]
    fn missing_file_yields_defaults() {
        let store = temp_store("missing");
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Defaults);
        assert_eq!(loaded.settings, AppSettings::default());
    }

    #[test]
    fn round_trip_preserves_settings() {
        let store = temp_store("round-trip");
        let mut settings = AppSettings {
            ui_language: UiLanguage::Chinese,
            ..AppSettings::default()
        };
        settings.profile_mut(default_active_profile()).tables_region = Some(Region {
            x: 458,
            y: 60,
            width: 720,
            height: 620,
        });
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings, settings);
    }

    #[test]
    fn malformed_file_yields_defaults_and_stays_writable() {
        let store = temp_store("malformed");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"{not json").unwrap();
        assert_eq!(store.load().status, LoadStatus::Defaults);
        store.save(&AppSettings::default()).unwrap();
        assert_eq!(store.load().status, LoadStatus::Loaded);
    }

    /// A settings file from before P7 has no `market` key at all; it must
    /// load as `Loaded` (not fall back to defaults wholesale) and hand out
    /// the shipped tuning.
    #[test]
    fn a_pre_p7_file_loads_and_yields_default_tuning() {
        let store = temp_store("pre-p7");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            br#"{"schema_version": 1, "ui_language": "Chinese"}"#,
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings.ui_language, UiLanguage::Chinese);
        let tuning = loaded.settings.market_tuning(Game::Poe2);
        assert_eq!(tuning, MarketTuning::default());
        assert_eq!(tuning.settlement_assets, ["divine-orb", "chaos-orb"]);
        assert_eq!(tuning.freshness.fresh_seconds, 7200);
        assert_eq!(tuning.freshness.usable_seconds, 21600);
        assert_eq!(tuning.freshness.capture_skew_seconds, 3600);
        assert_eq!(tuning.report_window_hours, 24);
    }

    /// A partially specified tuning keeps its stated fields and defaults the
    /// rest — hand-editing one number must not require writing them all.
    #[test]
    fn a_partial_tuning_defaults_what_it_does_not_state() {
        let store = temp_store("partial-tuning");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            br#"{
                "schema_version": 1,
                "market": {
                    "poe2": {
                        "settlement_assets": ["chaos-orb"],
                        "freshness": { "fresh_seconds": 3600 }
                    }
                }
            }"#,
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        let tuning = loaded.settings.market_tuning(Game::Poe2);
        assert_eq!(tuning.settlement_assets, ["chaos-orb"]);
        assert_eq!(tuning.freshness.fresh_seconds, 3600);
        assert_eq!(tuning.freshness.usable_seconds, 21600);
        assert_eq!(tuning.convert.sizes, [1, 10, 100]);
        assert_eq!(tuning.radar.stake, 10);
        assert_eq!(tuning.risk.thin_liquidity_stock, 100);
        // The other game is untouched by poe2's entry.
        assert_eq!(
            loaded.settings.market_tuning(Game::Poe1),
            MarketTuning::default()
        );
    }

    /// Customized tuning survives a save/load round trip bit-for-bit.
    #[test]
    fn tuning_round_trips() {
        let store = temp_store("tuning-round-trip");
        let mut settings = AppSettings::default();
        {
            let tuning = settings.market_tuning_mut(Game::Poe2);
            tuning.settlement_assets = vec![
                "divine-orb".to_owned(),
                "chaos-orb".to_owned(),
                "exalted-orb".to_owned(),
            ];
            tuning.focus_assets = vec!["perfect-chaos-orb".to_owned()];
            tuning.freshness.fresh_seconds = 1800;
            tuning.radar.stake = 100;
            tuning.report_window_hours = 48;
        }
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings, settings);
    }

    #[test]
    fn future_schema_is_read_only() {
        let store = temp_store("future");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), br#"{"schema_version": 99}"#).unwrap();
        match store.load().status {
            LoadStatus::FutureSchemaReadOnly { detected } => assert_eq!(detected, 99),
            other => panic!("expected read-only, got {other:?}"),
        }
        assert!(matches!(
            store.save(&AppSettings::default()),
            Err(SaveError::SchemaTooNew { detected: 99, .. })
        ));
    }
}
