//! 交易所小时史的日折叠与保留清理（P11）。
//!
//! 形状照抄 `rollup.rs` 的纪律，但适用的 R4 边界不同：
//! `exchange_day_marks` 遵守 R4（一年后日折是唯一副本，任何删除路径不碰它）；
//! `exchange_hours` 的抓取 mark **不受** R4 保护——CDN 一年内可重抓，
//! 删 mark 重抓是合法修复路径。
//!
//! 折叠只折"覆盖完整"的过去天：一个有洞的天折出来的成交量是悄悄偏小的，
//! 而 day mark 一旦盖上就不会重折——宁可跳过并报告，等洞补齐。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ptt_storage::{ExchangeDayMarketRow, ExchangeHourMark, MarketStore};

#[derive(Debug, Default)]
pub struct ExchangeRollupOutcome {
    pub days_processed: Vec<String>,
    /// (day, 原因)。覆盖不完整的天留在这里，下一轮补齐后自然消失。
    pub days_skipped: Vec<(String, String)>,
    pub days_already_done: usize,
}

#[derive(Debug, Default)]
pub struct ExchangePruneOutcome {
    pub days_deleted: Vec<String>,
    /// (day, 原因)。ground-truth 核对不过的天拒绝删除。
    pub days_refused: Vec<(String, String)>,
    pub hours_deleted: u64,
    pub markets_deleted: u64,
}

/// 把没折的完整过去天折成日线。今天结构性排除（还没过完）。
pub fn ensure_exchange_day_rollups(
    store: &mut MarketStore,
    game: &str,
    league: &str,
    now: DateTime<Utc>,
    max_days_per_run: usize,
) -> Result<ExchangeRollupOutcome, String> {
    let mut outcome = ExchangeRollupOutcome::default();
    let marks = store
        .list_exchange_hour_marks(game, league)
        .map_err(|error| format!("hour marks: {error}"))?;
    if marks.is_empty() {
        return Ok(outcome);
    }
    let done: std::collections::BTreeSet<String> = store
        .list_exchange_day_marks(game, league)
        .map_err(|error| format!("day marks: {error}"))?
        .into_iter()
        .map(|mark| mark.utc_day)
        .collect();

    let today = now.format("%Y-%m-%d").to_string();
    let earliest_mark_ts = marks[0].hour_ts;
    let mut by_day: BTreeMap<String, Vec<&ExchangeHourMark>> = BTreeMap::new();
    for mark in &marks {
        by_day.entry(day_of(mark.hour_ts)).or_default().push(mark);
    }

    for (day, day_marks) in &by_day {
        if outcome.days_processed.len() >= max_days_per_run {
            break;
        }
        if *day >= today {
            continue;
        }
        if done.contains(day) {
            outcome.days_already_done += 1;
            continue;
        }
        // 完整性：这一天的 mark 必须逐小时连续排到当天最后一个整点。
        // 唯一允许的开头缺口是整个联赛的第一个 mark（赛季中午开服，
        // 之前的小时永远不会被抓，那个缺口是结构性的、诚实的）。
        let last_hour = day_start_ts(day)? + 23 * 3600;
        let contiguous = day_marks
            .windows(2)
            .all(|pair| pair[1].hour_ts == pair[0].hour_ts + 3600);
        let starts_at_midnight = day_marks[0].hour_ts == day_start_ts(day)?;
        let is_league_head = day_marks[0].hour_ts == earliest_mark_ts;
        let complete = contiguous
            && day_marks.last().map(|mark| mark.hour_ts) == Some(last_hour)
            && (starts_at_midnight || is_league_head);
        if !complete {
            outcome.days_skipped.push((
                day.clone(),
                format!("coverage incomplete: {} of 24 hours", day_marks.len()),
            ));
            continue;
        }

        let day_start = day_start_ts(day)?;
        let hours = store
            .load_exchange_hours(game, league, day_start, day_start + 24 * 3600)
            .map_err(|error| format!("load hours {day}: {error}"))?;
        // 按无向对合计两侧成交量；hours_covered = 这对实际出现的小时数。
        let mut folds: BTreeMap<(String, String), (u64, u64, u32)> = BTreeMap::new();
        for row in &hours {
            let fold = folds
                .entry((row.asset_a.clone(), row.asset_b.clone()))
                .or_insert((0, 0, 0));
            fold.0 = fold.0.saturating_add(row.volume_a);
            fold.1 = fold.1.saturating_add(row.volume_b);
            fold.2 += 1;
        }
        let rows: Vec<ExchangeDayMarketRow> = folds
            .into_iter()
            .map(
                |((asset_a, asset_b), (volume_a, volume_b, hours_covered))| ExchangeDayMarketRow {
                    utc_day: day.clone(),
                    asset_a,
                    asset_b,
                    volume_a,
                    volume_b,
                    hours_covered,
                },
            )
            .collect();
        let hour_count = u32::try_from(day_marks.len()).unwrap_or(u32::MAX);
        store
            .replace_exchange_day(game, league, day, &rows, hour_count, now)
            .map_err(|error| format!("fold {day}: {error}"))?;
        outcome.days_processed.push(day.clone());
    }
    Ok(outcome)
}

/// 删掉已折叠天的小时层。`retention_days == 0` 完全关闭。
///
/// ground-truth 纪律同 `prune_raw_days`：删除前核对该天的日折行**真实存在**
/// 且行数与 day mark 一致——不信任 mark 单独作证。
pub fn prune_exchange_hours(
    store: &mut MarketStore,
    game: &str,
    league: &str,
    now: DateTime<Utc>,
    retention_days: u64,
) -> Result<ExchangePruneOutcome, String> {
    let mut outcome = ExchangePruneOutcome::default();
    if retention_days == 0 {
        return Ok(outcome);
    }
    let cutoff = (now - chrono::Duration::days(retention_days as i64))
        .format("%Y-%m-%d")
        .to_string();
    let day_marks: BTreeMap<String, u32> = store
        .list_exchange_day_marks(game, league)
        .map_err(|error| format!("day marks: {error}"))?
        .into_iter()
        .map(|mark| (mark.utc_day, mark.market_count))
        .collect();
    let hour_days: Vec<String> = {
        let marks = store
            .list_exchange_hour_marks(game, league)
            .map_err(|error| format!("hour marks: {error}"))?;
        let mut days: Vec<String> = marks.iter().map(|mark| day_of(mark.hour_ts)).collect();
        days.dedup();
        days
    };

    for day in hour_days {
        if day >= cutoff {
            continue;
        }
        let Some(expected_rows) = day_marks.get(&day) else {
            outcome
                .days_refused
                .push((day, "no day mark: fold it first".to_owned()));
            continue;
        };
        let actual_rows = store
            .load_exchange_days(game, league, &day, &day)
            .map_err(|error| format!("ground truth {day}: {error}"))?
            .len();
        if actual_rows != *expected_rows as usize {
            outcome.days_refused.push((
                day,
                format!("day rows {actual_rows} != mark {expected_rows}"),
            ));
            continue;
        }
        let stats = store
            .delete_exchange_hours_of_day(game, league, &day)
            .map_err(|error| format!("delete {day}: {error}"))?;
        outcome.hours_deleted += stats.hours_deleted;
        outcome.markets_deleted += stats.markets_deleted;
        outcome.days_deleted.push(day);
    }
    Ok(outcome)
}

fn day_of(hour_ts: i64) -> String {
    chrono::DateTime::from_timestamp(hour_ts, 0)
        .map_or_else(|| "?".to_owned(), |ts| ts.format("%Y-%m-%d").to_string())
}

fn day_start_ts(utc_day: &str) -> Result<i64, String> {
    chrono::NaiveDate::parse_from_str(utc_day, "%Y-%m-%d")
        .map_err(|error| format!("bad day {utc_day}: {error}"))?
        .and_hms_opt(0, 0, 0)
        .map(|naive| naive.and_utc().timestamp())
        .ok_or_else(|| format!("bad day {utc_day}"))
}

#[cfg(test)]
mod exchange_rollup_tests {
    use super::*;
    use chrono::TimeZone;
    use ptt_storage::ExchangeHourMarketRow;

    const EXALTED: &str = "Metadata/Items/Currency/CurrencyAddModToRare";
    const DIVINE: &str = "Metadata/Items/Currency/CurrencyModValues";
    const LEAGUE: &str = "Runes of Aldur";
    /// 2026-08-30 00:00 UTC。
    const DAY_START: i64 = 1_788_048_000;

    fn now() -> DateTime<Utc> {
        // 2026-08-31 09:30 —— DAY_START 的次日，所以 08-30 是"完整的过去天"。
        Utc.with_ymd_and_hms(2026, 8, 31, 9, 30, 0).unwrap()
    }

    fn row(hour_ts: i64) -> ExchangeHourMarketRow {
        ExchangeHourMarketRow {
            hour_ts,
            asset_a: EXALTED.to_owned(),
            asset_b: DIVINE.to_owned(),
            volume_a: 400,
            volume_b: 1,
            lowest_stock_a: 10,
            lowest_stock_b: 10,
            highest_stock_a: 20,
            highest_stock_b: 20,
            lowest_ratio_a: "400".to_owned(),
            lowest_ratio_b: "1".to_owned(),
            highest_ratio_a: "390".to_owned(),
            highest_ratio_b: "1".to_owned(),
        }
    }

    fn write_hours(store: &mut MarketStore, from: i64, count: i64) {
        for index in 0..count {
            let hour_ts = from + index * 3600;
            store
                .replace_exchange_hour("poe2", LEAGUE, hour_ts, &[row(hour_ts)], now())
                .expect("write hour");
        }
    }

    #[test]
    fn folds_a_complete_past_day_and_sums_volumes() {
        let mut store = MarketStore::open_in_memory().expect("store");
        write_hours(&mut store, DAY_START, 24);
        let outcome =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        assert_eq!(outcome.days_processed, vec!["2026-08-30".to_owned()]);
        let days = store
            .load_exchange_days("poe2", LEAGUE, "2026-08-30", "2026-08-30")
            .expect("days");
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].volume_a, 24 * 400);
        assert_eq!(days[0].hours_covered, 24);
        // 重跑：已折的天跳过，不重算。
        let again =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("again");
        assert!(again.days_processed.is_empty());
        assert_eq!(again.days_already_done, 1);
    }

    #[test]
    fn a_day_with_a_hole_is_skipped_not_half_folded() {
        let mut store = MarketStore::open_in_memory().expect("store");
        write_hours(&mut store, DAY_START, 10);
        // 缺 10:00 这一格，之后继续。
        write_hours(&mut store, DAY_START + 11 * 3600, 13);
        let outcome =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        assert!(outcome.days_processed.is_empty());
        assert_eq!(outcome.days_skipped.len(), 1);
        // 洞补上之后，同一个入口自然折出来。
        write_hours(&mut store, DAY_START + 10 * 3600, 1);
        let healed =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("heal");
        assert_eq!(healed.days_processed.len(), 1);
    }

    #[test]
    fn league_first_day_may_start_mid_day() {
        // 赛季中午开服：第一天从 12:00 起有 mark，是结构性缺口，照折。
        let mut store = MarketStore::open_in_memory().expect("store");
        write_hours(&mut store, DAY_START + 12 * 3600, 12);
        let outcome =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        assert_eq!(outcome.days_processed.len(), 1);
    }

    #[test]
    fn today_is_structurally_excluded() {
        let mut store = MarketStore::open_in_memory().expect("store");
        // 只写"今天"（now 所在天）的小时。
        write_hours(&mut store, DAY_START + 24 * 3600, 8);
        let outcome =
            ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        assert!(outcome.days_processed.is_empty());
        assert!(outcome.days_skipped.is_empty());
    }

    #[test]
    fn prune_needs_ground_truth_and_spares_day_layer() {
        let mut store = MarketStore::open_in_memory().expect("store");
        write_hours(&mut store, DAY_START, 24);
        // retention=1 的语义是"昨天要留着"：清理时把现在拨到三天后，
        // 让 08-30 真正出保留窗。
        let later = now() + chrono::Duration::days(3);
        // 没折就想删：拒绝。
        let refused = prune_exchange_hours(&mut store, "poe2", LEAGUE, later, 1).expect("prune");
        assert!(refused.days_deleted.is_empty());
        assert_eq!(refused.days_refused.len(), 1);

        ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        // 还在保留窗内时，一根小时线都不能动。
        let kept = prune_exchange_hours(&mut store, "poe2", LEAGUE, now(), 1).expect("kept");
        assert!(kept.days_deleted.is_empty());

        let pruned = prune_exchange_hours(&mut store, "poe2", LEAGUE, later, 1).expect("prune");
        assert_eq!(pruned.days_deleted, vec!["2026-08-30".to_owned()]);
        assert_eq!(pruned.hours_deleted, 24);
        // 日折层原封不动（R4）。
        assert_eq!(
            store
                .load_exchange_days("poe2", LEAGUE, "2026-08-30", "2026-08-30")
                .expect("days")
                .len(),
            1
        );
        // 幂等：小时层已空，再跑一遍无事发生。
        let again = prune_exchange_hours(&mut store, "poe2", LEAGUE, later, 1).expect("again");
        assert!(again.days_deleted.is_empty());
    }

    #[test]
    fn retention_zero_is_fully_off() {
        let mut store = MarketStore::open_in_memory().expect("store");
        write_hours(&mut store, DAY_START, 24);
        ensure_exchange_day_rollups(&mut store, "poe2", LEAGUE, now(), 32).expect("fold");
        let outcome = prune_exchange_hours(&mut store, "poe2", LEAGUE, now(), 0).expect("prune");
        assert!(outcome.days_deleted.is_empty());
        assert!(outcome.days_refused.is_empty());
    }
}
