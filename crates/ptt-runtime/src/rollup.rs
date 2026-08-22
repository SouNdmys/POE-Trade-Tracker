//! Daily-rollup orchestration, raw-data retention, and the season lifecycle.
//!
//! Everything here runs outside the capture loop — from a background report
//! build or a probe — and reads across every stored context of the same game:
//! the context key rotates on every release (the version string is hashed
//! into it), so a season of history only exists as the union of contexts.
//!
//! The sharp edge (R4): once raw days are pruned, `rollup_marks` is the only
//! barrier stopping a rebuild from replacing real rollups with emptiness.
//! Deleters never touch marks, and the pruner verifies actual rollup rows —
//! never the marks — before deleting a day.

use chrono::{DateTime, Days, NaiveDate, Utc};
use ptt_storage::{MarketStore, PairDayRollupRow, PurgeStats, SeasonRow};
use ptt_strategy::{DailyPairStat, PairDayRollup, build_pair_day_rollups};
use ptt_trade_domain::{Game, MarketContext, MarketEdgeObservation, Ratio};

/// How many missing days one builder run will roll up. A bound, not a goal:
/// the next run continues where this one stopped.
pub const MAX_ROLLUP_DAYS_PER_RUN: usize = 32;

/// What one builder run did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RollupOutcome {
    pub contexts: usize,
    pub days_processed: Vec<String>,
    /// Days that could not be rolled up, with the reason. No mark is written
    /// for them, so a later run (or a fixed build) retries.
    pub days_skipped: Vec<(String, String)>,
    /// Days already marked and therefore not revisited.
    pub days_already_done: usize,
}

/// What one retention pass did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PruneOutcome {
    pub days_deleted: Vec<String>,
    /// Days refused with the reason — a pair without a persisted rollup row
    /// blocks its whole day (never destroy un-summarized data).
    pub days_refused: Vec<(String, String)>,
    pub stats: PurgeStats,
}

/// The stable storage key for a game ("poe1" / "poe2"), matching both the
/// settings map key and the CHECKed game column.
#[must_use]
pub const fn game_key(game: Game) -> &'static str {
    match game {
        Game::Poe1 => "poe1",
        Game::Poe2 => "poe2",
    }
}

/// Every stored context key belonging to the game, parsed out of the context
/// JSON. This union is what makes analytics survive context-key rotation.
pub fn game_context_keys(store: &MarketStore, game: &str) -> Result<Vec<String>, String> {
    let contexts = store
        .list_contexts()
        .map_err(|error| format!("contexts: {error}"))?;
    Ok(contexts
        .into_iter()
        .filter(|row| {
            serde_json::from_str::<MarketContext>(&row.context_json)
                .is_ok_and(|context| game_key(context.game) == game)
        })
        .map(|row| row.context_key)
        .collect())
}

/// Today's observations, read across every context of the game.
///
/// `ensure_daily_rollups` excludes today by construction, so the current day
/// only ever exists as a live fold — and folding a single context key hides
/// the morning of any day the client updated. Same union the builder reads
/// for elapsed days, for the day still in progress. The hour of lookahead
/// keeps a capture clock slightly ahead of ours inside today.
pub fn today_window(
    store: &MarketStore,
    game: &str,
    now: DateTime<Utc>,
) -> Result<Vec<MarketEdgeObservation>, String> {
    let (from, to) = (day_start(now.date_naive()), now + chrono::Duration::hours(1));
    let mut window = Vec::new();
    for key in game_context_keys(store, game)? {
        window.extend(
            store
                .load_observations_between(&key, from, to)
                .map_err(|error| format!("today: {error}"))?,
        );
    }
    Ok(window)
}

/// Rolls up every fully-elapsed, not-yet-marked UTC day for the game, across
/// all of its contexts. Today is structurally excluded (the candidate range
/// ends yesterday); empty days get a zero mark and are never revisited;
/// rerunning a day through the explicit replace path is byte-identical.
pub fn ensure_daily_rollups(
    store: &mut MarketStore,
    game: &str,
    now: DateTime<Utc>,
    max_days_per_run: usize,
) -> Result<RollupOutcome, String> {
    let keys = game_context_keys(store, game)?;
    let mut outcome = RollupOutcome {
        contexts: keys.len(),
        ..RollupOutcome::default()
    };
    if keys.is_empty() {
        return Ok(outcome);
    }

    let earliest = keys
        .iter()
        .filter_map(|key| store.earliest_capture_day(key).transpose())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("earliest day: {error}"))?
        .into_iter()
        .min();
    let Some(earliest) = earliest else {
        return Ok(outcome);
    };
    let Some(mut day) = parse_day(&earliest) else {
        return Err(format!("stored day key unreadable: {earliest}"));
    };

    let done: std::collections::BTreeSet<String> = store
        .list_rollup_marks(game)
        .map_err(|error| format!("marks: {error}"))?
        .into_iter()
        .map(|mark| mark.utc_day)
        .collect();

    let today = now.date_naive();
    while day < today && outcome.days_processed.len() < max_days_per_run {
        let day_key = day.format("%Y-%m-%d").to_string();
        let next = day + Days::new(1);
        if done.contains(&day_key) {
            outcome.days_already_done += 1;
            day = next;
            continue;
        }

        let (from, to) = (day_start(day), day_start(next));
        let mut edges: Vec<MarketEdgeObservation> = Vec::new();
        for key in &keys {
            edges.extend(
                store
                    .load_observations_between(key, from, to)
                    .map_err(|error| format!("load {day_key}: {error}"))?,
            );
        }
        let snapshots: std::collections::BTreeSet<&str> = edges
            .iter()
            .map(|observation| observation.edge.snapshot_id.as_str())
            .collect();
        let day_snapshots = u32::try_from(snapshots.len())
            .map_err(|_| format!("snapshot count on {day_key} exceeds u32"))?;

        let rollups = build_pair_day_rollups(&edges);
        let mut rows = Vec::with_capacity(rollups.len());
        let mut overflow: Option<String> = None;
        for rollup in rollups {
            match rollup_to_row(game, &day_key, &rollup) {
                Ok(row) => rows.push(row),
                Err(reason) => {
                    overflow = Some(reason);
                    break;
                }
            }
        }
        if let Some(reason) = overflow {
            // Hard skip, no mark: never saturate, let a fixed build retry.
            outcome.days_skipped.push((day_key, reason));
            day = next;
            continue;
        }
        store
            .replace_day_rollups(game, &day_key, &rows, day_snapshots, now)
            .map_err(|error| format!("replace {day_key}: {error}"))?;
        outcome.days_processed.push(day_key);
        day = next;
    }
    Ok(outcome)
}

/// Deletes raw captures older than `retention_days`, one day per
/// transaction, and only for days whose every raw pair has a persisted
/// rollup row. `retention_days == 0` means retention is off entirely.
pub fn prune_raw_days(
    store: &mut MarketStore,
    game: &str,
    now: DateTime<Utc>,
    retention_days: u64,
) -> Result<PruneOutcome, String> {
    let mut outcome = PruneOutcome::default();
    if retention_days == 0 {
        return Ok(outcome);
    }
    let keys = game_context_keys(store, game)?;
    if keys.is_empty() {
        return Ok(outcome);
    }
    let earliest = keys
        .iter()
        .filter_map(|key| store.earliest_capture_day(key).transpose())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("earliest day: {error}"))?
        .into_iter()
        .min();
    let Some(earliest) = earliest else {
        return Ok(outcome);
    };
    let Some(mut day) = parse_day(&earliest) else {
        return Err(format!("stored day key unreadable: {earliest}"));
    };
    // Days strictly before the cutoff are prunable; the current day can
    // never qualify (retention_days >= 1 keeps cutoff <= today).
    let cutoff = now.date_naive() - Days::new(retention_days);

    while day < cutoff {
        let day_key = day.format("%Y-%m-%d").to_string();
        day = day + Days::new(1);
        let raw_pairs = store
            .raw_pairs_on_day(&keys, &day_key)
            .map_err(|error| format!("pairs {day_key}: {error}"))?;
        if raw_pairs.is_empty() {
            continue;
        }
        // Ground truth check against actual rollup rows — marks are not
        // trusted here, a mark without rows refuses the day.
        let rolled: std::collections::BTreeSet<(String, String)> = store
            .load_rollups(game, &day_key, &day_key)
            .map_err(|error| format!("rollups {day_key}: {error}"))?
            .into_iter()
            .map(|row| (row.need_asset_id, row.have_asset_id))
            .collect();
        if let Some(missing) = raw_pairs
            .iter()
            .find(|pair| !rolled.contains(&(pair.0.clone(), pair.1.clone())))
        {
            outcome.days_refused.push((
                day_key,
                format!("pair {} -> {} has no rollup", missing.0, missing.1),
            ));
            continue;
        }
        let stats = store
            .delete_raw_day(&keys, &day_key)
            .map_err(|error| format!("delete {day_key}: {error}"))?;
        outcome.stats.edges_deleted += stats.edges_deleted;
        outcome.stats.snapshots_deleted += stats.snapshots_deleted;
        outcome.stats.freed_bytes_estimate += stats.freed_bytes_estimate;
        outcome.days_deleted.push(day_key);
    }
    Ok(outcome)
}

/// The explicit "clear last season's raw data" Settings action. Refuses when
/// no season is configured — an explicit error, never a silent no-op.
/// Rollups, marks, and contexts survive; the active season is untouched by
/// construction (strictly-before delete).
pub fn purge_before_active_season(
    store: &mut MarketStore,
    game: &str,
) -> Result<(SeasonRow, PurgeStats), String> {
    let season = store
        .active_season(game)
        .map_err(|error| format!("season: {error}"))?
        .ok_or_else(|| "no season configured for this game".to_owned())?;
    let keys = game_context_keys(store, game)?;
    let stats = store
        .purge_before(&keys, season.started_at)
        .map_err(|error| format!("purge: {error}"))?;
    Ok((season, stats))
}

/// Season clamp shared by the report window and the per-accept analysis
/// window: no season floor means no clamp — existing behavior exactly.
#[must_use]
pub fn clamp_to_season(
    window_start: DateTime<Utc>,
    season_floor: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    season_floor.map_or(window_start, |floor| window_start.max(floor))
}

/// Stored rollup rows into strategy-facing stats. A row that fails to parse
/// is reported by day+pair rather than silently dropped.
pub fn stats_from_rollup_rows(rows: &[PairDayRollupRow]) -> (Vec<DailyPairStat>, Vec<String>) {
    let mut stats = Vec::with_capacity(rows.len());
    let mut notes = Vec::new();
    for row in rows {
        match stat_from_row(row) {
            Ok(stat) => stats.push(stat),
            Err(reason) => notes.push(format!(
                "rollup {} {} -> {} unreadable: {reason}",
                row.utc_day, row.need_asset_id, row.have_asset_id
            )),
        }
    }
    (stats, notes)
}

/// The current day folded live from window observations, since rollups only
/// cover fully-elapsed days. Only edges captured today (UTC) are folded —
/// the window also spans yesterday, which the rollups already cover.
#[must_use]
pub fn today_stats(
    observations: &[MarketEdgeObservation],
    now: DateTime<Utc>,
) -> Vec<DailyPairStat> {
    let today = now.date_naive();
    let start = day_start(today);
    let todays: Vec<MarketEdgeObservation> = observations
        .iter()
        .filter(|observation| observation.edge.captured_at >= start)
        .cloned()
        .collect();
    fold_window_stats(&todays, &today.format("%Y-%m-%d").to_string())
}

/// A whole observation window folded as one pseudo-day: median across every
/// snapshot in it, orientation dedup left to the pulse. The right grain for
/// window-scoped evidence (focus suggestions) where day boundaries add
/// nothing.
#[must_use]
pub fn fold_window_stats(
    observations: &[MarketEdgeObservation],
    day_key: &str,
) -> Vec<DailyPairStat> {
    build_pair_day_rollups(observations)
        .into_iter()
        .map(|rollup| stat_from_rollup(day_key, &rollup))
        .collect()
}

fn stat_from_rollup(day_key: &str, rollup: &PairDayRollup) -> DailyPairStat {
    DailyPairStat {
        utc_day: day_key.to_owned(),
        need_asset_id: rollup.need_asset_id.clone(),
        have_asset_id: rollup.have_asset_id.clone(),
        snapshot_count: rollup.snapshot_count,
        last_captured_at: rollup.last_captured_at,
        available_rows: rollup.median_available_rows,
        competing_rows: rollup.median_competing_rows,
        available_sum_need_units: rollup.median_available_sum_need_units,
        available_sum_have_units: rollup.median_available_sum_have_units,
        competing_sum_have_units: rollup.median_competing_sum_have_units,
        competing_sum_need_units: rollup.median_competing_sum_need_units,
        top_taker_rate: rollup.median_top_taker_rate.clone(),
    }
}

fn stat_from_row(row: &PairDayRollupRow) -> Result<DailyPairStat, String> {
    let need = ptt_trade_domain::MarketAssetId::try_new(&row.need_asset_id)
        .map_err(|error| format!("need: {error:?}"))?;
    let have = ptt_trade_domain::MarketAssetId::try_new(&row.have_asset_id)
        .map_err(|error| format!("have: {error:?}"))?;
    let top_taker_rate = row
        .median_top_taker_rate
        .map(|(numerator, denominator)| {
            Ratio::from_parts(numerator, denominator).map_err(|error| format!("rate: {error:?}"))
        })
        .transpose()?;
    let to_u128 = |value: i64, label: &str| -> Result<u128, String> {
        u128::try_from(value).map_err(|_| format!("{label} negative"))
    };
    Ok(DailyPairStat {
        utc_day: row.utc_day.clone(),
        need_asset_id: need,
        have_asset_id: have,
        snapshot_count: row.snapshot_count,
        last_captured_at: row.last_captured_at,
        available_rows: row.median_available_rows,
        competing_rows: row.median_competing_rows,
        available_sum_need_units: to_u128(row.median_available_sum_need_units, "available need")?,
        available_sum_have_units: to_u128(row.median_available_sum_have_units, "available have")?,
        competing_sum_have_units: to_u128(row.median_competing_sum_have_units, "competing have")?,
        competing_sum_need_units: to_u128(row.median_competing_sum_need_units, "competing need")?,
        top_taker_rate,
    })
}

/// Strategy rollup into a storage row. The u128 -> i64 boundary is the
/// overflow gate: failure skips the whole day upstream, never saturates.
fn rollup_to_row(
    game: &str,
    day_key: &str,
    rollup: &PairDayRollup,
) -> Result<PairDayRollupRow, String> {
    let to_i64 = |value: u128, label: &str| -> Result<i64, String> {
        i64::try_from(value).map_err(|_| {
            format!(
                "{label} overflows the rollup column ({value}) for {} -> {}",
                rollup.need_asset_id, rollup.have_asset_id
            )
        })
    };
    Ok(PairDayRollupRow {
        game: game.to_owned(),
        utc_day: day_key.to_owned(),
        need_asset_id: rollup.need_asset_id.as_str().to_owned(),
        have_asset_id: rollup.have_asset_id.as_str().to_owned(),
        snapshot_count: rollup.snapshot_count,
        contexts_merged: rollup.contexts_merged,
        first_captured_at: rollup.first_captured_at,
        last_captured_at: rollup.last_captured_at,
        median_available_rows: rollup.median_available_rows,
        median_competing_rows: rollup.median_competing_rows,
        median_available_sum_need_units: to_i64(
            rollup.median_available_sum_need_units,
            "available need units",
        )?,
        median_available_sum_have_units: to_i64(
            rollup.median_available_sum_have_units,
            "available have units",
        )?,
        median_competing_sum_have_units: to_i64(
            rollup.median_competing_sum_have_units,
            "competing have units",
        )?,
        median_competing_sum_need_units: to_i64(
            rollup.median_competing_sum_need_units,
            "competing need units",
        )?,
        median_top_taker_rate: rollup
            .median_top_taker_rate
            .as_ref()
            .map(|rate| (rate.numerator, rate.denominator)),
        computed_at: Utc::now(),
    })
}

fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0)
        .expect("midnight exists for every date")
        .and_utc()
}

fn parse_day(day_key: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day_key, "%Y-%m-%d").ok()
}
