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
use ptt_recognition::profiles::poe2::Route;
use ptt_storage::MarketStore;
use ptt_trade_domain::MarketContext;

use crate::analysis::pair_analysis_lines;
use crate::live::{capture_from_book, domain_asset_id, poe2_live_context};

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
/// Returns the names of any stored regions that were rejected, so a caller
/// can say so rather than silently watching the preset rectangles.
pub fn apply_saved_calibration_for(layout: ptt_recognition::profiles::PanelLayout) -> Vec<String> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let store = ptt_settings::SettingsStore::release_default_from(Path::new(&local));
    let settings = store.load().settings;
    // Regions are drawn against one game's panel. Applying them to another
    // game's route would silently watch the wrong rectangles, so a profile
    // for a different game is reported rather than installed.
    if settings.active_profile.game != layout.game {
        return vec![format!(
            "saved calibration is for {:?}; this route reads {:?}",
            settings.active_profile.game, layout.game
        )];
    }
    let Some(profile) = settings.profile(settings.active_profile) else {
        return Vec::new();
    };
    let mut rejected = Vec::new();
    for (name, region) in [
        ("NEED", profile.need_name_region),
        ("HAVE", profile.have_name_region),
        ("TABLES", profile.tables_region),
    ] {
        if let Some(region) = region
            && !ptt_recognition::profiles::poe2::set_region_override(
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

/// The saved calibration for the profile the pipeline actually runs.
pub fn apply_saved_calibration() -> Vec<String> {
    apply_saved_calibration_for(ptt_recognition::profiles::poe2::LAYOUT)
}

/// The live pipeline, opened once and driven by [`LivePipeline::run`].
pub struct LivePipeline {
    route: Route,
    store: MarketStore,
    context: MarketContext,
    context_key: String,
    sequence: u64,
}

impl LivePipeline {
    /// Opens the recognition route, the store, and the market context, with
    /// the user's calibration applied.
    pub fn open(league: &str, database_path: Option<&Path>) -> Result<Self, PipelineError> {
        apply_saved_calibration();
        let route = Route::new().map_err(|reason| PipelineError::Route(format!("{reason:?}")))?;
        let path = database_path.map_or_else(default_database_path, Path::to_path_buf);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store =
            MarketStore::open(&path).map_err(|error| PipelineError::Storage(error.to_string()))?;
        let context = poe2_live_context(league)
            .map_err(|error| PipelineError::Context(format!("{error:?}")))?;
        let context_key = context.stable_key();
        Ok(Self {
            route,
            store,
            context,
            context_key,
            sequence: 0,
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

                    let analysis = analyse(store, context_key, &need_id, &have_id)
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
) -> Result<Vec<String>, String> {
    let observations = store
        .load_observations(
            context_key,
            Some(chrono::Utc::now() - chrono::Duration::hours(ANALYSIS_WINDOW_HOURS)),
        )
        .map_err(|error| format!("load: {error}"))?;
    let need = domain_asset_id(need_id).map_err(|error| format!("{error:?}"))?;
    let have = domain_asset_id(have_id).map_err(|error| format!("{error:?}"))?;
    pair_analysis_lines(&observations, context_key, &need, &have)
        .map_err(|error| format!("analysis: {error}"))
}
