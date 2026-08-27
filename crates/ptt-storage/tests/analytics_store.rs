//! Contracts for the analytics persistence added in P10: seasons, daily pair
//! rollups, rollup marks, retention deletes, and the footprint report.

use chrono::{DateTime, TimeZone, Utc};
use ptt_storage::{MarketStore, PairDayRollupRow, StorageError};
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
        "0.10.3",
        "poe2-zhtw-2560x1440",
        1,
        "poe2-zhtw-route-v1",
        identity,
    )
    .expect("context")
}

/// One accepted book (2 rows -> 4 edges) captured at the given instant.
fn capture_at(captured_at: DateTime<Utc>) -> ConfirmedCapture {
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
    let rows = vec![
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
            Comparator::GreaterThan,
            "1:9.75",
            "611,620",
            false,
            Some(980_000),
        )
        .expect("row"),
    ];
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

fn at(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, 0, 0)
        .single()
        .expect("time")
}

fn rollup_row(
    game: &str,
    utc_day: &str,
    need: &str,
    have: &str,
    rate: Option<(u64, u64)>,
) -> PairDayRollupRow {
    PairDayRollupRow {
        game: game.to_owned(),
        utc_day: utc_day.to_owned(),
        need_asset_id: need.to_owned(),
        have_asset_id: have.to_owned(),
        snapshot_count: 3,
        contexts_merged: 1,
        first_captured_at: at(2026, 8, 20, 1),
        last_captured_at: at(2026, 8, 20, 23),
        median_available_rows: 6,
        median_competing_rows: 5,
        median_available_sum_need_units: 920,
        median_available_sum_have_units: 9_016,
        median_competing_sum_have_units: 611_620,
        median_competing_sum_need_units: 62_730,
        median_top_taker_rate: rate,
        computed_at: at(2026, 8, 21, 0),
    }
}

// ----- seasons -----

#[test]
fn season_round_trip_and_active_is_latest_started_at() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let mut first = store
        .start_season("poe2", "0.5 Abyss", at(2026, 6, 1, 0))
        .expect("first season");
    let second = store
        .start_season("poe2", "0.6", at(2026, 8, 1, 0))
        .expect("second season");
    // Starting the next season closes the previous one at the handover.
    first.ended_at = Some(second.started_at);

    let active = store.active_season("poe2").expect("active").expect("some");
    assert_eq!(active, second, "active season is the latest started_at");

    let listed = store.list_seasons("poe2").expect("list");
    assert_eq!(listed, vec![second, first], "newest first");

    assert!(
        store.active_season("poe1").expect("poe1").is_none(),
        "seasons are per game"
    );
}

/// A database created before `ended_at` existed opens and answers: the
/// upgrade adds the column in place, and the old row reads back as a season
/// that never ended.
#[test]
fn an_old_database_without_ended_at_upgrades_in_place() {
    let path = std::env::temp_dir().join(format!(
        "ptt-season-upgrade-{}-{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    {
        let connection = rusqlite::Connection::open(&path).expect("raw open");
        connection
            .execute_batch(
                "CREATE TABLE seasons (
                    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
                    season_id TEXT NOT NULL,
                    label TEXT NOT NULL CHECK (length(label) > 0),
                    started_at TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    PRIMARY KEY (game, season_id),
                    UNIQUE (game, started_at)
                ) STRICT;
                INSERT INTO seasons VALUES (
                    'poe2', '2026-06-01T00:00:00+00:00', '0.5 Abyss',
                    '2026-06-01T00:00:00+00:00', '2026-06-01T00:00:00+00:00'
                );",
            )
            .expect("old schema");
    }
    let store = MarketStore::open(&path).expect("upgraded open");
    let active = store.active_season("poe2").expect("season").expect("row");
    assert_eq!(active.label, "0.5 Abyss");
    assert_eq!(active.ended_at, None, "the old row never ended");
    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn start_season_rejects_started_at_not_after_active() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .start_season("poe2", "0.6", at(2026, 8, 1, 0))
        .expect("season");
    for earlier in [at(2026, 8, 1, 0), at(2026, 7, 31, 0)] {
        match store.start_season("poe2", "bad", earlier) {
            Err(StorageError::Rejected(_)) => {}
            other => panic!("expected Rejected, got {other:?}"),
        }
    }
}

#[test]
fn active_season_returns_none_when_table_empty() {
    let store = MarketStore::open_in_memory().expect("store");
    assert!(store.active_season("poe2").expect("query").is_none());
}

// ----- rollups -----

#[test]
fn rollup_row_round_trip_including_null_rate_pair() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let priced = rollup_row(
        "poe2",
        "2026-08-20",
        "divine-orb",
        "chaos-orb",
        Some((49, 5)),
    );
    let maker_only = rollup_row("poe2", "2026-08-20", "divine-orb", "exalted-orb", None);
    store
        .replace_day_rollups(
            "poe2",
            "2026-08-20",
            &[priced.clone(), maker_only.clone()],
            3,
            at(2026, 8, 21, 0),
        )
        .expect("replace");

    let loaded = store
        .load_rollups("poe2", "2026-08-20", "2026-08-20")
        .expect("load");
    assert_eq!(loaded, vec![priced, maker_only], "byte-equal round trip");

    let marks = store.list_rollup_marks("poe2").expect("marks");
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].utc_day, "2026-08-20");
    assert_eq!(marks[0].snapshot_count, 3);
    assert_eq!(marks[0].pair_count, 2);
}

#[test]
fn replace_day_rollups_is_idempotent_and_replaces_stale_rows() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let stale = rollup_row(
        "poe2",
        "2026-08-20",
        "divine-orb",
        "chaos-orb",
        Some((49, 5)),
    );
    store
        .replace_day_rollups("poe2", "2026-08-20", &[stale], 1, at(2026, 8, 21, 0))
        .expect("first write");

    let mut fresh = rollup_row(
        "poe2",
        "2026-08-20",
        "divine-orb",
        "chaos-orb",
        Some((50, 5)),
    );
    fresh.snapshot_count = 7;
    store
        .replace_day_rollups(
            "poe2",
            "2026-08-20",
            &[fresh.clone()],
            7,
            at(2026, 8, 21, 1),
        )
        .expect("replace");
    store
        .replace_day_rollups(
            "poe2",
            "2026-08-20",
            &[fresh.clone()],
            7,
            at(2026, 8, 21, 1),
        )
        .expect("idempotent rerun");

    let loaded = store
        .load_rollups("poe2", "2026-08-20", "2026-08-20")
        .expect("load");
    assert_eq!(
        loaded,
        vec![fresh],
        "stale row fully replaced, no duplicates"
    );
    let marks = store.list_rollup_marks("poe2").expect("marks");
    assert_eq!(marks.len(), 1, "mark upserted, not duplicated");
    assert_eq!(marks[0].snapshot_count, 7);
}

#[test]
fn load_rollups_filters_by_game_and_day_range() {
    let mut store = MarketStore::open_in_memory().expect("store");
    for day in ["2026-08-19", "2026-08-20", "2026-08-21"] {
        let row = rollup_row("poe2", day, "divine-orb", "chaos-orb", Some((49, 5)));
        store
            .replace_day_rollups("poe2", day, &[row], 1, at(2026, 8, 22, 0))
            .expect("poe2 write");
    }
    let other = rollup_row("poe1", "2026-08-20", "divine-orb", "chaos-orb", None);
    store
        .replace_day_rollups("poe1", "2026-08-20", &[other], 1, at(2026, 8, 22, 0))
        .expect("poe1 write");

    let loaded = store
        .load_rollups("poe2", "2026-08-19", "2026-08-20")
        .expect("load");
    assert_eq!(loaded.len(), 2, "range is inclusive and game-scoped");
    assert!(loaded.iter().all(|row| row.game == "poe2"));
    assert_eq!(loaded[0].utc_day, "2026-08-19");
    assert_eq!(loaded[1].utc_day, "2026-08-20");
}

// ----- contexts and bounded reads -----

#[test]
fn list_contexts_returns_key_and_json() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let capture = capture_at(at(2026, 8, 20, 12));
    store.persist_capture(&capture).expect("persist");

    let contexts = store.list_contexts().expect("contexts");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].context_key, capture.context.stable_key());
    let parsed: MarketContext =
        serde_json::from_str(&contexts[0].context_json).expect("context json parses");
    assert_eq!(parsed.game, Game::Poe2);
}

#[test]
fn load_observations_between_excludes_upper_bound() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let early = capture_at(at(2026, 8, 20, 12));
    let boundary = capture_at(at(2026, 8, 21, 0));
    store.persist_capture(&early).expect("persist early");
    store.persist_capture(&boundary).expect("persist boundary");
    let key = early.context.stable_key();

    let day = store
        .load_observations_between(&key, at(2026, 8, 20, 0), at(2026, 8, 21, 0))
        .expect("bounded load");
    assert_eq!(day.len(), 4, "only the in-day capture");
    assert!(
        day.iter().all(|o| o.edge.captured_at == early.captured_at),
        "the capture at exactly the upper bound is excluded"
    );

    let all = store.load_observations(&key, None).expect("full load");
    assert_eq!(all.len(), 8, "unbounded read still sees both");
}

#[test]
fn earliest_capture_day_matches_min_edge() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let key = context().stable_key();
    assert!(
        store
            .earliest_capture_day(&key)
            .expect("empty query")
            .is_none(),
        "empty context has no earliest day"
    );
    store
        .persist_capture(&capture_at(at(2026, 8, 21, 3)))
        .expect("persist later");
    store
        .persist_capture(&capture_at(at(2026, 8, 19, 23)))
        .expect("persist earlier");
    assert_eq!(
        store.earliest_capture_day(&key).expect("query").as_deref(),
        Some("2026-08-19")
    );
}

// ----- retention deletes -----

#[test]
fn delete_raw_day_removes_edges_then_snapshots_in_one_txn() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let key = context().stable_key();
    store
        .persist_capture(&capture_at(at(2026, 8, 19, 12)))
        .expect("persist day one");
    store
        .persist_capture(&capture_at(at(2026, 8, 20, 12)))
        .expect("persist day two");

    let pairs = store
        .raw_pairs_on_day(&[key.clone()], "2026-08-19")
        .expect("pairs");
    assert_eq!(
        pairs,
        vec![("divine-orb".to_owned(), "chaos-orb".to_owned())]
    );

    let stats = store
        .delete_raw_day(&[key.clone()], "2026-08-19")
        .expect("delete");
    assert_eq!(stats.edges_deleted, 4);
    assert_eq!(stats.snapshots_deleted, 1);

    assert!(
        store
            .raw_pairs_on_day(&[key.clone()], "2026-08-19")
            .expect("pairs after")
            .is_empty(),
        "deleted day has no raw pairs left"
    );
    let remaining = store.load_observations(&key, None).expect("load");
    assert_eq!(remaining.len(), 4, "the other day is intact");
    let footprint = store.database_footprint().expect("footprint");
    let count = |table: &str| {
        footprint
            .table_rows
            .iter()
            .find(|(name, _)| name == table)
            .expect("table listed")
            .1
    };
    assert_eq!(count("quote_edges"), 4);
    assert_eq!(
        count("market_snapshots"),
        1,
        "snapshot deleted with its edges"
    );
}

#[test]
fn delete_raw_day_leaves_marks_and_rollups_intact() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let key = context().stable_key();
    store
        .persist_capture(&capture_at(at(2026, 8, 19, 12)))
        .expect("persist");
    let row = rollup_row(
        "poe2",
        "2026-08-19",
        "divine-orb",
        "chaos-orb",
        Some((49, 5)),
    );
    store
        .replace_day_rollups("poe2", "2026-08-19", &[row.clone()], 1, at(2026, 8, 20, 0))
        .expect("rollup");

    store
        .delete_raw_day(&[key], "2026-08-19")
        .expect("delete raw");

    assert_eq!(
        store
            .load_rollups("poe2", "2026-08-19", "2026-08-19")
            .expect("rollups"),
        vec![row],
        "rollups survive raw deletion"
    );
    assert_eq!(
        store.list_rollup_marks("poe2").expect("marks").len(),
        1,
        "marks survive raw deletion (they gate the builder)"
    );
}

#[test]
fn purge_before_leaves_rows_at_or_after_cutoff() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let key = context().stable_key();
    store
        .persist_capture(&capture_at(at(2026, 7, 30, 12)))
        .expect("persist old");
    let boundary = capture_at(at(2026, 8, 1, 0));
    store.persist_capture(&boundary).expect("persist boundary");
    store
        .persist_capture(&capture_at(at(2026, 8, 2, 12)))
        .expect("persist new");

    let stats = store
        .purge_before(&[key.clone()], at(2026, 8, 1, 0))
        .expect("purge");
    assert_eq!(stats.edges_deleted, 4, "only the strictly-older capture");
    assert_eq!(stats.snapshots_deleted, 1);

    let remaining = store.load_observations(&key, None).expect("load");
    assert_eq!(remaining.len(), 8);
    assert!(
        remaining
            .iter()
            .all(|o| o.edge.captured_at >= at(2026, 8, 1, 0)),
        "the capture at exactly the cutoff survives"
    );
    assert_eq!(
        store.list_contexts().expect("contexts").len(),
        1,
        "contexts are provenance, never purged"
    );
}

// ----- footprint and vacuum -----

#[test]
fn database_footprint_row_counts_match_inserts() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .persist_capture(&capture_at(at(2026, 8, 20, 12)))
        .expect("persist");
    store
        .start_season("poe2", "0.6", at(2026, 8, 1, 0))
        .expect("season");
    let row = rollup_row("poe2", "2026-08-19", "divine-orb", "chaos-orb", None);
    store
        .replace_day_rollups("poe2", "2026-08-19", &[row], 1, at(2026, 8, 20, 0))
        .expect("rollup");

    let footprint = store.database_footprint().expect("footprint");
    assert!(footprint.total_bytes > 0);
    let expected = [
        ("market_contexts", 1),
        ("market_snapshots", 1),
        ("quote_edges", 4),
        ("seasons", 1),
        ("pair_day_rollups", 1),
        ("rollup_marks", 1),
    ];
    for (table, count) in expected {
        assert_eq!(
            footprint
                .table_rows
                .iter()
                .find(|(name, _)| name == table)
                .map(|(_, rows)| *rows),
            Some(count),
            "row count for {table}"
        );
    }
}

#[test]
fn vacuum_reports_reclaimed_pages_after_purge() {
    let path = std::env::temp_dir().join(format!(
        "ptt-analytics-vacuum-{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    {
        let mut store = MarketStore::open(&path).expect("store");
        let key = context().stable_key();
        // Enough rows to span multiple pages so the purge actually frees some.
        for hour in 0..24 {
            store
                .persist_capture(&capture_at(at(2026, 7, 1, hour)))
                .expect("persist");
        }
        let stats = store
            .purge_before(&[key], at(2026, 8, 1, 0))
            .expect("purge");
        assert_eq!(stats.edges_deleted, 96);
        let reclaimed = store.vacuum().expect("vacuum");
        assert!(
            reclaimed > 0,
            "vacuum after a bulk purge must shrink the file (got {reclaimed})"
        );
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
}
