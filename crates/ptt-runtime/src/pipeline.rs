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

use crate::analysis::pair_analysis;
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

/// How far a top rate may sit from its recent daily median before the book's
/// identity is suspect.
///
/// Deliberately not `top_book_outlier_factor` (3): that one compares rows
/// inside a single frame, where three-fold is already extreme. Across days a
/// real currency moving three-fold is ordinary market news, so borrowing that
/// number here would warn every day and teach the user to ignore the warning.
pub const IDENTITY_SANITY_FACTOR: u64 = 10;

/// Whether this book's top taker rate is too far from what this pair has
/// recently been worth to be the pair it claims to be.
///
/// Two currencies whose names differ by one prefix word (Exalted Orb / Perfect
/// Exalted Orb) both match the catalog exactly, so the recognition layer has
/// nothing to tell them apart with — but their rates are orders of magnitude
/// apart, and history knows it. No history means no opinion: a missing
/// baseline always passes, because a guard that cannot read must never be the
/// reason a book is doubted.
#[must_use]
pub fn magnitude_suspect(
    top_rate: &ptt_trade_domain::Ratio,
    baseline: Option<&ptt_trade_domain::Ratio>,
    factor: u64,
) -> bool {
    baseline.is_some_and(|baseline| top_rate.differs_by_more_than(baseline, factor))
}

/// One order row of an accepted book.
///
/// The panel's own fields rather than a sentence about them, so the interface
/// can put each in its own column and colour the ones that matter. The
/// rendered lines beside it are what the overlay card paints, where a single
/// column of text is the whole point.
#[derive(Clone, Debug)]
pub struct BookRow {
    /// Which table the row came from, by the side's own stable key.
    pub side: &'static str,
    /// 0-based position within its table, top to bottom.
    pub row_index: u8,
    /// The rate exactly as the panel showed it.
    pub rate: String,
    /// True for the aggregate row, which restates the tier as "this and
    /// everything worse" rather than quoting a single listing.
    pub aggregate: bool,
    pub stock: u64,
}

/// A book that was recognised, confirmed, and durably stored.
#[derive(Clone, Debug)]
pub struct AcceptedBook {
    /// Position in this run, from one.
    pub sequence: u64,
    pub need_asset_id: String,
    pub have_asset_id: String,
    /// One display line per order row, in panel order. Painted by the
    /// overlay card, which has one column and no room for anything else.
    pub rows: Vec<String>,
    /// The same rows with their fields intact, for the monitor's table.
    pub order_rows: Vec<BookRow>,
    /// Time from the first capture to the confirmed result.
    pub elapsed: Duration,
    /// What this book says about its own pair, as typed facts. The monitor
    /// draws them as a table; probes print them via [`PairAnalysis::lines`].
    pub analysis: crate::analysis::PairAnalysis,
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
    /// The active season's start, read once at open so the per-accept
    /// analysis window can be clamped with zero work inside the loop. A
    /// season started mid-session reaches the next session.
    season_floor: Option<chrono::DateTime<chrono::Utc>>,
}

impl LivePipeline {
    /// Opens the recognition route, the store, and the market context, with
    /// the user's calibration applied.
    pub fn open(league: &str, database_path: Option<&Path>) -> Result<Self, PipelineError> {
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
        // No season configured -> no clamp -> today's behavior exactly.
        let season_floor = store
            .active_season(profile.game.as_str())
            .ok()
            .flatten()
            .map(|season| season.started_at);
        Ok(Self {
            route,
            store,
            context,
            context_key,
            sequence: 0,
            season_floor,
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
            season_floor,
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
                    let order_rows = book
                        .observation
                        .rows
                        .iter()
                        .map(|row| BookRow {
                            side: row.side.as_str(),
                            row_index: row.row_index,
                            rate: row.ratio.normalized.clone(),
                            aggregate: row.ratio.comparator != ptt_recognition::Comparator::Exact,
                            stock: row.stock,
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

                    let analysis = analyse(store, context_key, &need_id, &have_id, *season_floor)
                        .unwrap_or_else(|error| {
                            crate::analysis::PairAnalysis::failed(&have_id, &need_id, error)
                        });
                    on_event(PipelineEvent::Accepted(Box::new(AcceptedBook {
                        sequence: *sequence,
                        need_asset_id: need_id,
                        have_asset_id: have_id,
                        rows,
                        order_rows,
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
    season_floor: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<crate::analysis::PairAnalysis, String> {
    let since = crate::rollup::clamp_to_season(
        chrono::Utc::now() - chrono::Duration::hours(ANALYSIS_WINDOW_HOURS),
        season_floor,
    );
    let observations = store
        .load_observations(context_key, Some(since))
        .map_err(|error| format!("load: {error}"))?;
    let need = domain_asset_id(need_id).map_err(|error| format!("{error:?}"))?;
    let have = domain_asset_id(have_id).map_err(|error| format!("{error:?}"))?;
    pair_analysis(&observations, context_key, &need, &have)
        .map_err(|error| format!("analysis: {error}"))
}

#[cfg(test)]
mod identity_sanity_tests {
    use super::*;
    use ptt_trade_domain::Ratio;

    fn rate(numerator: u64, denominator: u64) -> Ratio {
        Ratio::from_parts(numerator, denominator).expect("ratio")
    }

    /// The baseline is a real pair's recent daily median: 100 chaos per exalted.
    #[test]
    fn a_top_rate_ten_times_off_its_recent_median_is_suspect() {
        let baseline = rate(100, 1);

        // Exactly ten-fold is still inside the band — the test is "differs by
        // more than", so the boundary itself passes.
        assert!(!magnitude_suspect(
            &rate(1_000, 1),
            Some(&baseline),
            IDENTITY_SANITY_FACTOR
        ));
        assert!(!magnitude_suspect(
            &rate(900, 1),
            Some(&baseline),
            IDENTITY_SANITY_FACTOR
        ));
        assert!(magnitude_suspect(
            &rate(1_100, 1),
            Some(&baseline),
            IDENTITY_SANITY_FACTOR
        ));
        // The other direction counts too: an eleventh of the median is just as
        // wrong an order of magnitude.
        assert!(magnitude_suspect(
            &rate(100, 11),
            Some(&baseline),
            IDENTITY_SANITY_FACTOR
        ));
        // No history, no opinion — never the reason a book is doubted.
        assert!(!magnitude_suspect(
            &rate(1_000_000, 1),
            None,
            IDENTITY_SANITY_FACTOR
        ));
    }
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
