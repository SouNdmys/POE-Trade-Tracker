//! The one live pipeline: capture, confirm, persist, analyse.
//!
//! The app, `watch-probe` and `session-probe` each used to assemble these
//! steps themselves. Three copies meant three places to fix a pipeline bug and
//! three chances to fix only two of them — and the probes, which build their
//! own [`Route`], never saw the user's saved ROI calibration, so a probe run
//! on a calibrated machine watched the shipped preset rectangles and
//! disagreed with the app for reasons that had nothing to do with what was
//! being tested.
//!
//! [`LivePipeline::open`] applies the saved calibration, so every consumer
//! watches the same rectangles, and [`LivePipeline::run`] is the only
//! implementation of the loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ptt_monitoring::{SessionConfig, SessionEvent, SessionStats, run_session, skip_key};
use ptt_recognition::route::Route;
use ptt_storage::MarketStore;
use ptt_trade_domain::MarketContext;

use crate::analysis::pair_analysis_lines;
use crate::live::{capture_from_book, domain_asset_id, live_context};

/// The league every live component agrees on.
///
/// The writer stamps captures with it and the reader filters on it, so a
/// second literal anywhere makes the pages read an empty book with no error.
pub const LIVE_LEAGUE: &str = "live-league";

/// How far back the per-accept analysis reads.
///
/// Bounded because this runs inside the capture loop: an unbounded read grows
/// with the season and eventually stalls recognition.
const ANALYSIS_WINDOW_HOURS: i64 = 2;

/// A book that was recognised, confirmed, and durably stored.
#[derive(Clone, Debug)]
pub struct AcceptedBook {
    /// Position in this run, from one.
    pub sequence: u64,
    pub need_asset_id: String,
    pub have_asset_id: String,
    /// One display line per order row, in panel order.
    pub rows: Vec<String>,
    /// Time from the first capture to the confirmed result.
    pub elapsed: Duration,
    /// Conversion and cycle lines for this pair, or the reason there are none.
    pub analysis: Vec<String>,
}

/// What the pipeline produced for one tick.
#[derive(Clone, Debug)]
pub enum PipelineEvent {
    Accepted(Box<AcceptedBook>),
    /// A frame was not used. The reason is the session's own typed key, not a
    /// parsed debug string.
    Skipped(String),
    /// Something the run cannot recover from.
    Fault(String),
}

#[derive(Debug)]
pub enum PipelineError {
    Route(String),
    Storage(String),
    Context(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Route(reason) => write!(formatter, "route init failed: {reason}"),
            Self::Storage(reason) => write!(formatter, "storage open failed: {reason}"),
            Self::Context(reason) => write!(formatter, "market context failed: {reason}"),
        }
    }
}

impl std::error::Error for PipelineError {}

/// The default database location.
#[must_use]
pub fn default_database_path() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    Path::new(&local)
        .join("PoeTradeTracker")
        .join("market.sqlite")
}

/// Applies the user's saved ROI calibration to the recognition route.
///
/// Regions are installed under the prefix of the layout they were drawn for,
/// which is why the layout is a parameter rather than assumed: installing a
/// POE1 rectangle under the POE2 prefix would apply one game's calibration to
/// the other with nothing to indicate it.
///
/// Installs a profile's saved regions, returning the names of any that were
/// rejected so a caller can say so rather than silently watching the preset
/// rectangles.
fn apply_saved_calibration_for(profile_id: ptt_core::ProfileId) -> Vec<String> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let store = ptt_settings::SettingsStore::release_default_from(Path::new(&local));
    let settings = store.load().settings;
    // Regions are drawn against one game's panel, so applying them to another
    // game's route would silently watch the wrong rectangles. This used to be
    // a runtime check that reported the mismatch; it is now impossible instead,
    // because `route_for` derives the layout from this same profile. The
    // invariant is held by `a_profile_selects_its_own_game_s_panel` below.
    let Some(profile) = settings.profile(profile_id) else {
        return Vec::new();
    };
    let layout = route_for(profile_id).0;
    let mut rejected = Vec::new();
    for (name, region) in [
        ("NEED", profile.need_name_region),
        ("HAVE", profile.have_name_region),
        ("TABLES", profile.tables_region),
    ] {
        if let Some(region) = region
            && !ptt_recognition::route::set_region_override(
                layout.key_prefix,
                name,
                (region.x, region.y, region.width, region.height),
            )
        {
            rejected.push(name.to_owned());
        }
    }
    rejected
}

/// The panel and OCR language a profile selects.
///
/// The geometry differs per game and the language only picks which catalog
/// names the identity slots are matched against, which is why one route serves
/// both — see `ptt_recognition::route`.
#[must_use]
pub fn route_for(
    profile: ptt_core::ProfileId,
) -> (
    ptt_recognition::profiles::PanelLayout,
    ptt_recognition::profiles::ProfileLanguage,
) {
    let layout = match profile.game {
        ptt_core::Game::Poe1 => ptt_recognition::profiles::poe1::LAYOUT,
        ptt_core::Game::Poe2 => ptt_recognition::profiles::poe2::LAYOUT,
    };
    let language = match profile.language {
        ptt_core::ContentLanguage::English => ptt_recognition::profiles::ProfileLanguage::English,
        ptt_core::ContentLanguage::TraditionalChinese => {
            ptt_recognition::profiles::ProfileLanguage::TraditionalChinese
        }
    };
    (layout, language)
}

/// The profile the user has selected, or the default if settings are unreadable.
#[must_use]
pub fn active_profile() -> ptt_core::ProfileId {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    ptt_settings::SettingsStore::release_default_from(Path::new(&local))
        .load()
        .settings
        .active_profile
}

/// The saved calibration for the profile the pipeline actually runs.
pub fn apply_saved_calibration() -> Vec<String> {
    apply_saved_calibration_for(active_profile())
}

/// The live pipeline, opened once and driven by [`LivePipeline::run`].
pub struct LivePipeline {
    route: Route,
    store: MarketStore,
    context: MarketContext,
    context_key: String,
    sequence: u64,
    /// The reader's language, for the analysis lines each accepted book
    /// carries. Distinct from the route's language, which is the game
    /// client's. Captured at open: a switch mid-session reaches the next book
    /// rather than rewriting the one already on screen.
    ui_language: ptt_settings::UiLanguage,
}

impl LivePipeline {
    /// Opens the recognition route, the store, and the market context, with
    /// the user's calibration applied.
    pub fn open(
        league: &str,
        database_path: Option<&Path>,
        ui_language: ptt_settings::UiLanguage,
    ) -> Result<Self, PipelineError> {
        let profile = active_profile();
        apply_saved_calibration();
        let (layout, language) = route_for(profile);
        let route = Route::new_with(layout, language)
            .map_err(|reason| PipelineError::Route(format!("{reason:?}")))?;
        let path = database_path.map_or_else(default_database_path, Path::to_path_buf);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store =
            MarketStore::open(&path).map_err(|error| PipelineError::Storage(error.to_string()))?;
        let context = live_context(profile, league)
            .map_err(|error| PipelineError::Context(format!("{error:?}")))?;
        let context_key = context.stable_key();
        Ok(Self {
            route,
            store,
            context,
            context_key,
            sequence: 0,
            ui_language,
        })
    }

    /// Where books are being stored, for display.
    #[must_use]
    pub fn context_key(&self) -> &str {
        &self.context_key
    }

    /// Watches until `budget` elapses or `cancel` is set, reporting every
    /// outcome through `on_event`.
    pub fn run(
        &mut self,
        budget: Duration,
        cancel: &AtomicBool,
        mut on_event: impl FnMut(PipelineEvent),
    ) -> SessionStats {
        // Split the borrows: the session holds the route for the whole run
        // while the callback needs the store and the counter mutably.
        let Self {
            route,
            store,
            context,
            context_key,
            sequence,
            ui_language,
        } = self;
        run_session(
            route,
            &SessionConfig::default(),
            budget,
            cancel,
            |event| match event {
                SessionEvent::Accepted {
                    book,
                    elapsed,
                    captured_at,
                    frame_hashes,
                } => {
                    *sequence += 1;
                    let need_id = book.observation.identity.need_asset_id.clone();
                    let have_id = book.observation.identity.have_asset_id.clone();
                    let rows = book
                        .observation
                        .rows
                        .iter()
                        .map(|row| {
                            format!(
                                "{} #{} {} stock {}",
                                row.side.as_str(),
                                row.row_index,
                                row.ratio.normalized,
                                row.stock,
                            )
                        })
                        .collect::<Vec<_>>();

                    // A book counts as accepted only once it is durably
                    // stored; a write failure is a fault, not a quiet loss.
                    let capture = match capture_from_book(
                        &book,
                        context,
                        chrono::DateTime::<chrono::Utc>::from(captured_at),
                        frame_hashes,
                        *sequence,
                    ) {
                        Ok(capture) => capture,
                        Err(error) => {
                            on_event(PipelineEvent::Skipped("mapping-failed".to_owned()));
                            on_event(PipelineEvent::Fault(format!(
                                "book NOT stored ({need_id} -> {have_id}): mapping: {error:?}"
                            )));
                            return;
                        }
                    };
                    if let Err(error) = store.persist_capture(&capture) {
                        on_event(PipelineEvent::Skipped("persist-failed".to_owned()));
                        on_event(PipelineEvent::Fault(format!(
                            "book NOT stored ({need_id} -> {have_id}): persist: {error}"
                        )));
                        return;
                    }

                    let analysis = analyse(store, context_key, &need_id, &have_id, *ui_language)
                        .unwrap_or_else(|error| vec![format!("analysis error: {error}")]);
                    on_event(PipelineEvent::Accepted(Box::new(AcceptedBook {
                        sequence: *sequence,
                        need_asset_id: need_id,
                        have_asset_id: have_id,
                        rows,
                        elapsed,
                        analysis,
                    })));
                }
                SessionEvent::FrameSkipped { reason } => {
                    on_event(PipelineEvent::Skipped(skip_key(&reason)));
                }
                SessionEvent::ConfirmationMismatch => {
                    on_event(PipelineEvent::Skipped("confirmation-mismatch".to_owned()));
                }
                SessionEvent::Duplicate => {
                    on_event(PipelineEvent::Skipped("duplicate".to_owned()));
                }
                SessionEvent::CaptureError(_) => {
                    on_event(PipelineEvent::Skipped("capture-error".to_owned()));
                }
            },
        )
    }
}

fn analyse(
    store: &MarketStore,
    context_key: &str,
    need_id: &str,
    have_id: &str,
    language: ptt_settings::UiLanguage,
) -> Result<Vec<String>, String> {
    let observations = store
        .load_observations(
            context_key,
            Some(chrono::Utc::now() - chrono::Duration::hours(ANALYSIS_WINDOW_HOURS)),
        )
        .map_err(|error| format!("load: {error}"))?;
    let need = domain_asset_id(need_id).map_err(|error| format!("{error:?}"))?;
    let have = domain_asset_id(have_id).map_err(|error| format!("{error:?}"))?;
    pair_analysis_lines(&observations, context_key, &need, &have, language)
        .map_err(|error| format!("analysis: {error}"))
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use ptt_core::{ContentLanguage, Game, ProfileId};

    /// Every profile must select the panel belonging to its own game.
    ///
    /// This is what makes the calibration lookup safe without a runtime
    /// guard: the regions a profile stores are drawn against the panel this
    /// function returns for it. Break the pairing and the watcher reads
    /// another game's rectangles, which looks like a recognition problem
    /// rather than a wiring one.
    #[test]
    fn a_profile_selects_its_own_game_s_panel() {
        for game in [Game::Poe1, Game::Poe2] {
            for language in [
                ContentLanguage::English,
                ContentLanguage::TraditionalChinese,
            ] {
                let (layout, _) = route_for(ProfileId::new(game, language));
                assert_eq!(
                    layout.game, game,
                    "{game:?}/{language:?} reads another panel"
                );
            }
        }
    }

    /// The client language must reach the route, or a Chinese client is read
    /// against English names and every frame skips with nothing to explain it.
    #[test]
    fn the_client_language_reaches_the_route() {
        use ptt_recognition::profiles::ProfileLanguage;
        for game in [Game::Poe1, Game::Poe2] {
            assert_eq!(
                route_for(ProfileId::new(game, ContentLanguage::English)).1,
                ProfileLanguage::English
            );
            assert_eq!(
                route_for(ProfileId::new(game, ContentLanguage::TraditionalChinese)).1,
                ProfileLanguage::TraditionalChinese
            );
        }
    }

    /// The context key must separate games, and the reader must be able to
    /// reproduce the writer's.
    ///
    /// The app's report pages rebuild the context from the same profile the
    /// pipeline stored under. If those two ever disagree they do not error —
    /// the reader simply finds nothing under its key and every page shows an
    /// empty book, which reads as "the watcher captured nothing".
    #[test]
    fn a_profile_reproduces_its_own_context_key_and_no_other() {
        let key = |game, language| {
            crate::live::live_context(ProfileId::new(game, language), LIVE_LEAGUE)
                .expect("context")
                .stable_key()
        };
        let poe1 = key(Game::Poe1, ContentLanguage::TraditionalChinese);
        let poe2 = key(Game::Poe2, ContentLanguage::TraditionalChinese);
        assert_eq!(
            poe1,
            key(Game::Poe1, ContentLanguage::TraditionalChinese),
            "the same profile must reproduce its key"
        );
        assert_ne!(poe1, poe2, "two games must not share a context key");
        assert_ne!(
            poe1,
            key(Game::Poe1, ContentLanguage::English),
            "two client languages must not share a context key"
        );
    }

    /// Opening the store is cheap enough to do on the UI thread.
    ///
    /// The report pages reopen it per refresh. Measured rather than assumed,
    /// because the fix for a slow open is a cached connection whose lifetime
    /// then has to be reasoned about against the writer's.
    #[test]
    fn opening_the_store_is_cheap() {
        let directory = std::env::temp_dir().join("ptt-open-cost");
        std::fs::create_dir_all(&directory).expect("temp dir");
        let path = directory.join("cost.sqlite");
        let _ = std::fs::remove_file(&path);
        ptt_storage::MarketStore::open(&path).expect("first open");
        let started = std::time::Instant::now();
        for _ in 0..20 {
            ptt_storage::MarketStore::open(&path).expect("open");
        }
        let each = started.elapsed() / 20;
        assert!(
            each < std::time::Duration::from_millis(20),
            "reopening the store costs {each:?}, too much for the UI thread"
        );
        println!("store open: {each:?} each");
    }

    /// Provenance must carry the catalog the session actually matched against.
    #[test]
    fn the_context_pins_its_own_catalog() {
        for (game, expected) in [
            (Game::Poe1, ptt_catalog::POE1_CATALOG_SHA256),
            (Game::Poe2, ptt_catalog::POE2_CATALOG_SHA256),
        ] {
            let context = crate::live::live_context(
                ProfileId::new(game, ContentLanguage::TraditionalChinese),
                "live-league",
            )
            .expect("context");
            assert!(
                format!("{context:?}").contains(expected),
                "{game:?} context does not pin its own catalog"
            );
        }
    }
}
