//! B-3: a book whose rate is orders of magnitude off its own pair's recent
//! history is flagged before it is stored — and stored anyway.
//!
//! The guard runs on the live capture path, so the rule it must never break is
//! "a guard that cannot read is not a reason to lose a book". Both tests below
//! exist to pin that: one that the flag fires, one that a pair with no history
//! is never doubted.

use chrono::{DateTime, TimeZone, Utc};
use ptt_runtime::pipeline::{PipelineEvent, warn_then_persist};
use ptt_settings::UiLanguage;
use ptt_storage::{MarketStore, PairDayRollupRow};
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, Game, MarketAssetId, MarketContext, ObservationIdentity, QuoteSide,
};

fn sha() -> String {
    "a".repeat(64)
}

fn context() -> MarketContext {
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
        "0.5.5",
        "poe2-zhtw-2560x1440",
        1,
        "poe2-zhtw-route-v1",
        identity,
    )
    .expect("context")
}

/// A book of exalted-for-chaos whose only taker row quotes `rate`.
fn capture(captured_at: DateTime<Utc>, rate: &str) -> ConfirmedCapture {
    capture_of(captured_at, &[rate])
}

/// A book of exalted-for-chaos whose taker side quotes `rates`, top row first.
fn capture_of(captured_at: DateTime<Utc>, rates: &[&str]) -> ConfirmedCapture {
    let context = context();
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
    let rows = rates
        .iter()
        .enumerate()
        .map(|(row_index, rate)| {
            ConfirmedOrderRow::try_new(
                QuoteSide::Available,
                u8::try_from(row_index).expect("row index"),
                Comparator::Exact,
                rate,
                "920",
                false,
                Some(990_000),
            )
            .expect("row")
        })
        .collect();
    ConfirmedCapture::try_new(
        captured_at,
        context,
        MarketAssetId::try_new("exalted-orb").expect("need"),
        MarketAssetId::try_new("chaos-orb").expect("have"),
        rows,
        provenance,
        "{}".to_owned(),
        "{}".to_owned(),
        Vec::new(),
    )
    .expect("capture")
}

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0)
        .single()
        .expect("time")
}

fn store(name: &str) -> MarketStore {
    let directory = std::env::temp_dir().join("ptt-identity-sanity");
    std::fs::create_dir_all(&directory).expect("temp dir");
    let path = directory.join(format!("{name}.sqlite"));
    let _ = std::fs::remove_file(&path);
    MarketStore::open(&path).expect("store")
}

/// One day's fold for exalted-for-chaos, with `rate` as the day's median top
/// taker rate.
fn rollup(utc_day: &str, rate: (u64, u64)) -> PairDayRollupRow {
    PairDayRollupRow {
        game: "poe2".to_owned(),
        utc_day: utc_day.to_owned(),
        need_asset_id: "exalted-orb".to_owned(),
        have_asset_id: "chaos-orb".to_owned(),
        snapshot_count: 12,
        contexts_merged: 1,
        first_captured_at: at(9),
        last_captured_at: at(9),
        median_available_rows: 5,
        median_competing_rows: 5,
        median_available_sum_need_units: 100,
        median_available_sum_have_units: 10_000,
        median_competing_sum_have_units: 10_000,
        median_competing_sum_need_units: 100,
        median_top_taker_rate: Some(rate),
        computed_at: at(10),
    }
}

/// Runs the guard and collects what it said. The outlier band is the shipped
/// `risk.top_book_outlier_factor` default, which is the band the daily fold
/// these tests compare against runs with.
fn drain(
    store: &mut MarketStore,
    capture: &ConfirmedCapture,
    season_floor: Option<DateTime<Utc>>,
) -> (Vec<String>, Result<(), String>) {
    let mut warnings = Vec::new();
    let outcome = warn_then_persist(
        store,
        capture,
        season_floor,
        3,
        UiLanguage::English,
        &mut |event| {
            if let PipelineEvent::Warning(message) = event {
                warnings.push(message);
            }
        },
    )
    .map_err(|error| error.to_string());
    (warnings, outcome)
}

#[test]
fn a_book_a_hundred_times_off_yesterdays_median_is_warned_about_and_still_stored() {
    let mut store = store("suspect");
    store
        .replace_day_rollups(
            "poe2",
            "2026-08-09",
            &[rollup("2026-08-09", (1, 100))],
            12,
            at(10),
        )
        .expect("rollup");

    let capture = capture(at(10), "1:10000");
    let (warnings, outcome) = drain(&mut store, &capture, None);

    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("exalted-orb") && warnings[0].contains("chaos-orb"),
        "{warnings:?}"
    );
    assert!(outcome.is_ok(), "{outcome:?}");
    // The flag reports, it never rejects: the book has to be in the database.
    let stored = store
        .load_observations(&capture.context.stable_key(), None)
        .expect("load");
    assert!(!stored.is_empty(), "the flagged book was not stored");
}

#[test]
fn a_pair_with_no_history_is_never_doubted() {
    let mut store = store("no-history");

    let capture = capture(at(10), "1:10000");
    let (warnings, outcome) = drain(&mut store, &capture, None);

    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// One misread row must not be able to speak for the whole book.
///
/// The daily fold on the other side of the comparison drops the rows that sit
/// outside their own side's band before it takes the day's top rate. A check
/// that keeps them is measuring two different things against each other: a
/// single row that lost a decimal point becomes this book's "best rate", and
/// the pair gets doubted on the strength of a row the baseline never saw.
#[test]
fn one_outlier_row_does_not_speak_for_the_whole_book() {
    let mut store = store("outlier-row");
    store
        .replace_day_rollups(
            "poe2",
            "2026-08-09",
            &[rollup("2026-08-09", (1, 100))],
            12,
            at(10),
        )
        .expect("rollup");

    // Four rows agree on 1:100 and match yesterday's median exactly; the fifth
    // lost the zeroes off its denominator and reads a hundred times better
    // than its own side, which is what an OCR misread looks like.
    let capture = capture_of(at(10), &["1:100", "1:100", "1:100", "1:100", "1:1"]);
    let (warnings, outcome) = drain(&mut store, &capture, None);

    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// The baseline must not reach back past the league's own start.
///
/// Two leagues price the same currency an order of magnitude apart as a
/// matter of course, and the daily fold has no league column to tell them
/// apart — so without this clamp every book captured in the first days of a
/// new league is measured against the old league's prices, and every book
/// looks wrong.
#[test]
fn a_previous_leagues_rollup_is_never_the_baseline() {
    // The league started midway through 08-10: 08-09 is entirely the old
    // league, and 08-10's own fold is half of each with no way to separate
    // them. The book is captured on 08-11, so both days are in the window.
    let season_floor = Utc
        .with_ymd_and_hms(2026, 8, 10, 15, 0, 0)
        .single()
        .expect("time");
    let capture = capture(at(11), "1:10000");

    let mut fresh_league = store("previous-league");
    for day in ["2026-08-09", "2026-08-10"] {
        fresh_league
            .replace_day_rollups("poe2", day, &[rollup(day, (1, 100))], 12, at(11))
            .expect("rollup");
    }
    let (warnings, outcome) = drain(&mut fresh_league, &capture, Some(season_floor));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(outcome.is_ok(), "{outcome:?}");

    // The clamp is not a blanket mute: a league that started long ago leaves
    // the same two days inside the window, and the book is flagged as before.
    let mut old_league = store("same-league");
    for day in ["2026-08-09", "2026-08-10"] {
        old_league
            .replace_day_rollups("poe2", day, &[rollup(day, (1, 100))], 12, at(11))
            .expect("rollup");
    }
    let (warnings, outcome) = drain(&mut old_league, &capture, Some(at(1)));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(outcome.is_ok(), "{outcome:?}");
}
