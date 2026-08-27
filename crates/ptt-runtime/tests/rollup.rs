//! Contracts for the rollup builder, retention, and the season lifecycle.

use chrono::{DateTime, TimeZone, Utc};
use ptt_runtime::rollup::{
    MAX_ROLLUP_DAYS_PER_RUN, clamp_end_to_season, clamp_to_season, ensure_daily_rollups,
    prune_raw_days, purge_before_active_season, today_window,
};
use ptt_storage::MarketStore;
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, Game, MarketAssetId, MarketContext, ObservationIdentity, QuoteSide,
};

fn sha() -> String {
    "a".repeat(64)
}

/// A context whose key varies with the build string — the same market seen
/// across app releases.
fn context(game_ui_build: &str) -> MarketContext {
    let identity = ObservationIdentity::try_new(
        "ptt-windows-ocr",
        "1.0.0",
        sha(),
        sha(),
        sha(),
        "poe2-catalog-v1",
        sha(),
        "poe2-recognition-v1",
        sha(),
        "warm-mask-v1",
    )
    .expect("identity");
    MarketContext::try_new_for(
        Game::Poe2,
        ClientLanguage::TraditionalChinese,
        "rise-of-the-abyssal",
        game_ui_build,
        "poe2-zhtw-2560x1440",
        1,
        "poe2-zhtw-route-v1",
        identity,
    )
    .expect("context")
}

fn default_rows() -> Vec<ConfirmedOrderRow> {
    vec![
        ConfirmedOrderRow::try_new(
            QuoteSide::Available,
            0,
            Comparator::Exact,
            "1:9.80",
            "920",
            false,
            Some(990_000),
        )
        .expect("row"),
        ConfirmedOrderRow::try_new(
            QuoteSide::Competing,
            5,
            Comparator::Exact,
            "1:9.75",
            "611,620",
            false,
            Some(980_000),
        )
        .expect("row"),
    ]
}

fn capture(
    captured_at: DateTime<Utc>,
    game_ui_build: &str,
    rows: Vec<ConfirmedOrderRow>,
) -> ConfirmedCapture {
    let context = context(game_ui_build);
    let provenance = CaptureProvenance {
        draft_id: "draft-1".to_owned(),
        capture_job_id: "job-1".to_owned(),
        review_revision: 1,
        confirmation_mode: CaptureConfirmationMode::AutomaticConsensus,
        source: "live_watch_double_read".to_owned(),
        evidence_id: "evidence-1".to_owned(),
        evidence_removed: false,
        frame_hashes: vec![sha(), sha()],
        profile_sha256: sha(),
        provider_id: context.observation_identity.ocr_provider_id.clone(),
        provider_version: context.observation_identity.ocr_provider_version.clone(),
        model_sha256: context.observation_identity.ocr_model_sha256.clone(),
        provider_manifest_sha256: context
            .observation_identity
            .ocr_provider_manifest_sha256
            .clone(),
        parser_assets_sha256: context.observation_identity.parser_assets_sha256.clone(),
    };
    ConfirmedCapture::try_new(
        captured_at,
        context,
        MarketAssetId::try_new("divine-orb").expect("need"),
        MarketAssetId::try_new("chaos-orb").expect("have"),
        rows,
        provenance,
        "{}".to_owned(),
        "{}".to_owned(),
        Vec::new(),
    )
    .expect("capture")
}

fn at(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, hour, 0, 0)
        .single()
        .expect("time")
}

/// The fixed "now" the tests run against: 2026-08-22 12:00 UTC.
fn now() -> DateTime<Utc> {
    at(22, 12)
}

fn ensure(store: &mut MarketStore) -> ptt_runtime::rollup::RollupOutcome {
    ensure_daily_rollups(store, "poe2", now(), MAX_ROLLUP_DAYS_PER_RUN, 3).expect("ensure")
}

// ----- builder -----

#[test]
fn ensure_daily_rollups_never_processes_today() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(22, 9), "0.10.3", default_rows()))
        .expect("persist today");

    let outcome = ensure(&mut store);
    assert!(outcome.days_processed.is_empty(), "today is not a full day");
    assert!(store.list_rollup_marks("poe2").expect("marks").is_empty());
}

#[test]
fn ensure_daily_rollups_rerun_is_idempotent() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(20, 9), "0.10.3", default_rows()))
        .expect("persist");

    let first = ensure(&mut store);
    assert_eq!(first.days_processed, vec!["2026-08-20", "2026-08-21"]);
    let rows_after_first = store
        .load_rollups("poe2", "2026-08-20", "2026-08-21")
        .expect("rollups");
    assert_eq!(rows_after_first.len(), 1, "one book, one row");

    let second = ensure(&mut store);
    assert!(second.days_processed.is_empty(), "everything is marked");
    assert_eq!(second.days_already_done, 2);
    let rows_after_second = store
        .load_rollups("poe2", "2026-08-20", "2026-08-21")
        .expect("rollups");
    assert_eq!(rows_after_first, rows_after_second, "byte-identical");
}

#[test]
fn ensure_daily_rollups_marks_empty_days_once() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // Captures on the 19th only; the 20th and 21st elapsed with nothing.
    store
        .persist_capture(&capture(at(19, 9), "0.10.3", default_rows()))
        .expect("persist");

    let outcome = ensure(&mut store);
    assert_eq!(
        outcome.days_processed,
        vec!["2026-08-19", "2026-08-20", "2026-08-21"]
    );
    let marks = store.list_rollup_marks("poe2").expect("marks");
    assert_eq!(marks.len(), 3);
    assert_eq!(marks[1].utc_day, "2026-08-20");
    assert_eq!(marks[1].snapshot_count, 0, "a confirmed-empty day");
    assert_eq!(marks[1].pair_count, 0);

    let again = ensure(&mut store);
    assert!(again.days_processed.is_empty());
    assert_eq!(again.days_already_done, 3, "empty days are never revisited");
}

#[test]
fn ensure_daily_rollups_aggregates_two_contexts_of_same_game() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // The same book on the same day under two app builds: two context keys.
    store
        .persist_capture(&capture(at(20, 9), "0.10.3", default_rows()))
        .expect("persist v1");
    store
        .persist_capture(&capture(at(20, 15), "0.10.4", default_rows()))
        .expect("persist v2");

    let outcome = ensure(&mut store);
    assert_eq!(outcome.contexts, 2);
    assert!(outcome.days_processed.contains(&"2026-08-20".to_owned()));
    let rows = store
        .load_rollups("poe2", "2026-08-20", "2026-08-20")
        .expect("rollups");
    assert_eq!(rows.len(), 1, "one market, not one per release");
    assert_eq!(rows[0].snapshot_count, 2);
    assert_eq!(rows[0].contexts_merged, 2);
}

// ----- today's live fold -----

#[test]
fn today_window_reads_every_context_of_the_game() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // A release lands at midday: the morning's captures sit under the old
    // build's context key, the afternoon's under the new one.
    store
        .persist_capture(&capture(at(22, 9), "0.10.3", default_rows()))
        .expect("persist morning");
    store
        .persist_capture(&capture(at(22, 11), "0.10.4", default_rows()))
        .expect("persist afternoon");

    let window = today_window(&store, "poe2", now(), None).expect("window");
    assert!(
        window
            .iter()
            .any(|observation| observation.edge.captured_at == at(22, 9)),
        "a release today must not hide the morning"
    );
    assert_eq!(window.len(), 8, "both contexts' edges, four each");
}

#[test]
fn today_window_stops_at_midnight() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(21, 23), "0.10.3", default_rows()))
        .expect("persist yesterday");
    store
        .persist_capture(&capture(at(22, 9), "0.10.3", default_rows()))
        .expect("persist today");

    let window = today_window(&store, "poe2", now(), None).expect("window");
    assert_eq!(window.len(), 4, "yesterday belongs to the rollup builder");
}

#[test]
fn rollup_overflow_aborts_day_and_is_reported() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // 9e18 stock at 1:1000 converts to 9e21 have units: over i64, and the
    // day must be skipped and reported — never saturated, never marked.
    let huge = vec![
        ConfirmedOrderRow::try_new(
            QuoteSide::Available,
            0,
            Comparator::Exact,
            "1:1000",
            "9000000000000000000",
            false,
            Some(990_000),
        )
        .expect("row"),
    ];
    store
        .persist_capture(&capture(at(20, 9), "0.10.3", huge))
        .expect("persist");

    let outcome = ensure(&mut store);
    assert_eq!(outcome.days_skipped.len(), 1);
    assert_eq!(outcome.days_skipped[0].0, "2026-08-20");
    assert!(outcome.days_skipped[0].1.contains("overflow"));
    assert!(
        store
            .load_rollups("poe2", "2026-08-20", "2026-08-20")
            .expect("rollups")
            .is_empty(),
        "nothing partial was written"
    );
    let marks = store.list_rollup_marks("poe2").expect("marks");
    assert!(
        marks.iter().all(|mark| mark.utc_day != "2026-08-20"),
        "no mark, so a later run retries the day"
    );
}

// ----- season clamp -----

#[test]
fn season_clamp_absent_season_reads_full_window() {
    let start = at(1, 0);
    assert_eq!(clamp_to_season(start, None), start, "no season, no clamp");
}

#[test]
fn season_clamp_active_season_floors_since() {
    let window_start = at(1, 0);
    assert_eq!(
        clamp_to_season(window_start, Some(at(10, 0))),
        at(10, 0),
        "a season inside the window floors it"
    );
    assert_eq!(
        clamp_to_season(at(15, 0), Some(at(10, 0))),
        at(15, 0),
        "a season older than the window changes nothing"
    );
}

#[test]
fn season_end_caps_the_window_and_absent_end_does_not() {
    assert_eq!(
        clamp_end_to_season(at(20, 0), None),
        at(20, 0),
        "a running season reads to now"
    );
    assert_eq!(
        clamp_end_to_season(at(20, 0), Some(at(15, 0))),
        at(15, 0),
        "an ended season stops counting at its end"
    );
    assert_eq!(
        clamp_end_to_season(at(10, 0), Some(at(15, 0))),
        at(10, 0),
        "an end past the window changes nothing"
    );
}

#[test]
fn ending_a_season_is_recorded_and_starting_the_next_closes_the_last() {
    let mut store = MarketStore::open_in_memory().expect("store");
    assert!(
        store.end_season("poe2", at(5, 0)).is_err(),
        "no season to end is an explicit error"
    );
    store
        .start_season("poe2", "0.6", at(1, 0))
        .expect("first season");
    assert!(
        store.end_season("poe2", at(1, 0)).is_err(),
        "an end at or before the start is nonsense"
    );
    let ended = store.end_season("poe2", at(10, 0)).expect("end");
    assert_eq!(ended.ended_at, Some(at(10, 0)));
    assert_eq!(
        store
            .active_season("poe2")
            .expect("season")
            .expect("row")
            .ended_at,
        Some(at(10, 0)),
        "the end survives a re-read"
    );

    // 开下一季自动给上一季补结束点(若它还开着)。
    let mut second = MarketStore::open_in_memory().expect("store");
    second
        .start_season("poe2", "0.6", at(1, 0))
        .expect("first season");
    second
        .start_season("poe2", "0.7", at(12, 0))
        .expect("second season");
    let seasons = second.list_seasons("poe2").expect("list");
    assert_eq!(seasons.len(), 2);
    assert_eq!(
        seasons[1].ended_at,
        Some(at(12, 0)),
        "the previous season closes where the next one starts"
    );
    assert_eq!(seasons[0].ended_at, None, "the new season is running");
}

#[test]
fn today_window_respects_both_season_boundaries() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // Three captures today: early morning, midday, evening.
    for hour in [6, 12, 18] {
        store
            .persist_capture(&capture(at(22, hour), "0.10.3", default_rows()))
            .expect("persist");
    }
    let season = ptt_storage::SeasonRow {
        game: "poe2".to_owned(),
        season_id: "s".to_owned(),
        label: "0.7".to_owned(),
        // 开赛当天中午开的赛季:上午属于上一季。
        started_at: at(22, 10),
        // 傍晚前结束:晚上的抓取是赛季外的。
        ended_at: Some(at(22, 15)),
        created_at: at(22, 10),
    };
    let window = today_window(&store, "poe2", now(), Some(&season)).expect("window");
    assert_eq!(
        window.len(),
        4,
        "only the midday capture sits inside the season"
    );
    assert!(
        window
            .iter()
            .all(|observation| observation.edge.captured_at == at(22, 12))
    );
}

// ----- retention -----

#[test]
fn retention_refuses_day_with_unrolled_pair() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(10, 9), "0.10.3", default_rows()))
        .expect("persist");

    // No rollups exist: pruning must refuse, never destroy un-summarized data.
    let outcome = prune_raw_days(&mut store, "poe2", now(), 1).expect("prune");
    assert!(outcome.days_deleted.is_empty());
    assert_eq!(outcome.days_refused.len(), 1);
    assert!(outcome.days_refused[0].1.contains("no rollup"));
    let key = context("0.10.3").stable_key();
    assert_eq!(
        store.load_observations(&key, None).expect("load").len(),
        4,
        "nothing was deleted"
    );
}

#[test]
fn retention_ignores_lying_mark_without_rollup_rows() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(10, 9), "0.10.3", default_rows()))
        .expect("persist");
    // A mark that claims the day is done, with no rollup rows behind it.
    store
        .replace_day_rollups("poe2", "2026-08-10", &[], 1, now())
        .expect("lying mark");

    let outcome = prune_raw_days(&mut store, "poe2", now(), 1).expect("prune");
    assert!(outcome.days_deleted.is_empty(), "the mark is not trusted");
    assert_eq!(outcome.days_refused.len(), 1);
}

#[test]
fn retention_never_deletes_current_day() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(22, 9), "0.10.3", default_rows()))
        .expect("persist today");

    let outcome = prune_raw_days(&mut store, "poe2", now(), 1).expect("prune");
    assert!(outcome.days_deleted.is_empty());
    assert!(outcome.days_refused.is_empty());
    let key = context("0.10.3").stable_key();
    assert_eq!(store.load_observations(&key, None).expect("load").len(), 4);
}

#[test]
fn retention_deletes_fully_rolled_up_day_and_reports_stats() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(10, 9), "0.10.3", default_rows()))
        .expect("persist old");
    store
        .persist_capture(&capture(at(22, 9), "0.10.3", default_rows()))
        .expect("persist today");
    ensure(&mut store);

    let outcome = prune_raw_days(&mut store, "poe2", now(), 1).expect("prune");
    assert_eq!(outcome.days_deleted, vec!["2026-08-10"]);
    assert!(outcome.days_refused.is_empty());
    assert_eq!(outcome.stats.edges_deleted, 4);
    assert_eq!(outcome.stats.snapshots_deleted, 1);

    let key = context("0.10.3").stable_key();
    let remaining = store.load_observations(&key, None).expect("load");
    assert_eq!(remaining.len(), 4, "today's capture is untouched");
    assert!(
        !store
            .load_rollups("poe2", "2026-08-10", "2026-08-10")
            .expect("rollups")
            .is_empty(),
        "the summary of the deleted day survives"
    );
}

// ----- season purge -----

#[test]
fn purge_before_active_season_keeps_active_rows_and_all_marks() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture(at(10, 9), "0.10.3", default_rows()))
        .expect("persist old season");
    store
        .persist_capture(&capture(at(21, 9), "0.10.3", default_rows()))
        .expect("persist new season");
    ensure(&mut store);
    let marks_before = store.list_rollup_marks("poe2").expect("marks").len();

    assert!(
        purge_before_active_season(&mut store, "poe2").is_err(),
        "explicit error when no season is configured, never a silent no-op"
    );
    store
        .start_season("poe2", "0.6", at(20, 0))
        .expect("season");
    let (season, stats) = purge_before_active_season(&mut store, "poe2").expect("purge");
    assert_eq!(season.label, "0.6");
    assert_eq!(stats.edges_deleted, 4, "only the pre-season capture");

    let key = context("0.10.3").stable_key();
    let remaining = store.load_observations(&key, None).expect("load");
    assert_eq!(remaining.len(), 4);
    assert!(remaining.iter().all(|o| o.edge.captured_at >= at(20, 0)));
    assert_eq!(
        store.list_rollup_marks("poe2").expect("marks").len(),
        marks_before,
        "marks survive the purge (they gate the builder)"
    );
    assert!(
        !store
            .load_rollups("poe2", "2026-08-10", "2026-08-10")
            .expect("rollups")
            .is_empty(),
        "pre-season rollups are the season's durable history"
    );
}
