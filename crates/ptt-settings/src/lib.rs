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
