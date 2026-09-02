//! Contracts for the exchange-history persistence added in P11: hourly marks
//! and market rows, day folds, the retention delete, the watermark, and the
//! footprint report.

use chrono::{TimeZone, Utc};
use ptt_storage::{
    ExchangeDayMarketRow, ExchangeHourMarketRow, ExchangeHourVolumeRow, MarketStore, StorageError,
};

const EXALTED: &str = "Metadata/Items/Currency/CurrencyAddModToRare";
const DIVINE: &str = "Metadata/Items/Currency/CurrencyModValues";
const LEAGUE: &str = "Runes of Aldur";
/// 2026-08-31 07:00 UTC —— spike 实拉过的那个小时，数字未改。
const HOUR: i64 = 1_788_159_600;

fn hour_row(hour_ts: i64) -> ExchangeHourMarketRow {
    ExchangeHourMarketRow {
        hour_ts,
        asset_a: EXALTED.to_owned(),
        asset_b: DIVINE.to_owned(),
        volume_a: 1_004_431,
        volume_b: 2_416,
        lowest_stock_a: 4_758_920,
        lowest_stock_b: 6_291,
        highest_stock_a: 4_884_606,
        highest_stock_b: 6_803,
        lowest_ratio_a: "434".to_owned(),
        lowest_ratio_b: "1".to_owned(),
        highest_ratio_a: "373".to_owned(),
        highest_ratio_b: "1".to_owned(),
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 31, 9, 0, 0).unwrap()
}

#[test]
fn hour_roundtrip_watermark_and_marks() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR, &[hour_row(HOUR)], now())
        .expect("write hour");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR + 3600, &[hour_row(HOUR + 3600)], now())
        .expect("write next hour");

    assert_eq!(
        store.exchange_watermark("poe2", LEAGUE).expect("watermark"),
        Some(HOUR + 3600)
    );
    // 另一个联赛/游戏是另一条水位，互不串。
    assert_eq!(
        store
            .exchange_watermark("poe2", "Standard")
            .expect("watermark"),
        None
    );
    assert_eq!(
        store.exchange_watermark("poe1", LEAGUE).expect("watermark"),
        None
    );

    let loaded = store
        .load_exchange_hours("poe2", LEAGUE, HOUR, HOUR + 3600)
        .expect("load");
    assert_eq!(loaded, vec![hour_row(HOUR)]);

    let marks = store
        .list_exchange_hour_marks("poe2", LEAGUE)
        .expect("marks");
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0].hour_ts, HOUR);
    assert_eq!(marks[0].market_count, 1);
}

/// 精简读只是全读的投影：同一段、同一序、同样的五列，少搬的只是字符串。
#[test]
fn hour_volumes_read_back_the_same_rows_as_the_full_read() {
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR, &[hour_row(HOUR)], now())
        .expect("write hour");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR + 3600, &[hour_row(HOUR + 3600)], now())
        .expect("write next hour");

    let lean = store
        .load_exchange_hour_volumes("poe2", LEAGUE, HOUR, HOUR + 7200)
        .expect("lean load");
    let full: Vec<ExchangeHourVolumeRow> = store
        .load_exchange_hours("poe2", LEAGUE, HOUR, HOUR + 7200)
        .expect("full load")
        .into_iter()
        .map(|row| ExchangeHourVolumeRow {
            hour_ts: row.hour_ts,
            asset_a: row.asset_a,
            asset_b: row.asset_b,
            volume_a: row.volume_a,
            volume_b: row.volume_b,
        })
        .collect();
    assert_eq!(lean.len(), 2);
    assert_eq!(lean, full);
    // 半开区间：终点那个小时不在里面。
    assert_eq!(
        store
            .load_exchange_hour_volumes("poe2", LEAGUE, HOUR, HOUR + 3600)
            .expect("lean load")
            .len(),
        1
    );
}

#[test]
fn confirmed_empty_hour_is_a_zero_mark() {
    // 赛季前的小时：没有行，但 mark 必须存在——它就是"抓过了"的证据，
    // 水位靠它前进，永不重访靠它成立。
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR, &[], now())
        .expect("write empty hour");
    assert_eq!(
        store.exchange_watermark("poe2", LEAGUE).expect("watermark"),
        Some(HOUR)
    );
    let marks = store
        .list_exchange_hour_marks("poe2", LEAGUE)
        .expect("marks");
    assert_eq!(marks[0].market_count, 0);
    assert!(
        store
            .load_exchange_hours("poe2", LEAGUE, HOUR, HOUR + 3600)
            .expect("load")
            .is_empty()
    );
}

#[test]
fn one_market_of_one_hour_is_a_point_lookup() {
    // 面板核对要的是"这一小时、这一对"——按抓取时刻逐点查，
    // 不把两周的小时表整个搬进内存。
    let mut store = MarketStore::open_in_memory().expect("store");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR, &[hour_row(HOUR)], now())
        .expect("write hour");
    assert_eq!(
        store
            .load_exchange_hour_market("poe2", LEAGUE, HOUR, EXALTED, DIVINE)
            .expect("lookup"),
        Some(hour_row(HOUR))
    );
    // 另一个小时、另一对、另一个联赛：都不是同一条账。
    assert_eq!(
        store
            .load_exchange_hour_market("poe2", LEAGUE, HOUR + 3600, EXALTED, DIVINE)
            .expect("lookup"),
        None
    );
    assert_eq!(
        store
            .load_exchange_hour_market("poe2", LEAGUE, HOUR, DIVINE, EXALTED)
            .expect("lookup"),
        None
    );
    assert_eq!(
        store
            .load_exchange_hour_market("poe2", "Standard", HOUR, EXALTED, DIVINE)
            .expect("lookup"),
        None
    );
}

#[test]
fn replace_rejects_rows_from_another_hour() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let result = store.replace_exchange_hour("poe2", LEAGUE, HOUR, &[hour_row(HOUR + 3600)], now());
    assert!(matches!(result, Err(StorageError::Rejected(_))));
}

#[test]
fn replace_rejects_misaligned_hour() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let result = store.replace_exchange_hour("poe2", LEAGUE, HOUR + 1, &[], now());
    assert!(matches!(result, Err(StorageError::Rejected(_))));
}

#[test]
fn unsorted_pair_is_refused_by_schema() {
    // asset_a < asset_b 是无向对身份的根基，schema 层再守一道。
    let mut store = MarketStore::open_in_memory().expect("store");
    let mut row = hour_row(HOUR);
    std::mem::swap(&mut row.asset_a, &mut row.asset_b);
    std::mem::swap(&mut row.volume_a, &mut row.volume_b);
    let result = store.replace_exchange_hour("poe2", LEAGUE, HOUR, &[row], now());
    assert!(result.is_err());
}

#[test]
fn volume_overflow_is_a_hard_error_and_nothing_lands() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let mut row = hour_row(HOUR);
    row.volume_a = u64::MAX;
    let result = store.replace_exchange_hour("poe2", LEAGUE, HOUR, &[row], now());
    assert!(matches!(result, Err(StorageError::RollupOverflow(_))));
    // 事务整体回滚：连 mark 都不能留下，否则这个小时永远不会被重抓。
    assert_eq!(
        store.exchange_watermark("poe2", LEAGUE).expect("watermark"),
        None
    );
}

#[test]
fn day_fold_roundtrip_and_hour_prune() {
    let mut store = MarketStore::open_in_memory().expect("store");
    // 2026-08-31 当天的两个小时。
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR, &[hour_row(HOUR)], now())
        .expect("hour 1");
    store
        .replace_exchange_hour("poe2", LEAGUE, HOUR + 3600, &[hour_row(HOUR + 3600)], now())
        .expect("hour 2");

    let day = ExchangeDayMarketRow {
        utc_day: "2026-08-31".to_owned(),
        asset_a: EXALTED.to_owned(),
        asset_b: DIVINE.to_owned(),
        volume_a: 2 * 1_004_431,
        volume_b: 2 * 2_416,
        hours_covered: 2,
    };
    store
        .replace_exchange_day("poe2", LEAGUE, "2026-08-31", &[day.clone()], 2, now())
        .expect("fold day");

    let days = store
        .load_exchange_days("poe2", LEAGUE, "2026-08-31", "2026-08-31")
        .expect("load days");
    assert_eq!(days, vec![day]);
    let day_marks = store
        .list_exchange_day_marks("poe2", LEAGUE)
        .expect("day marks");
    assert_eq!(day_marks.len(), 1);
    assert_eq!(day_marks[0].hour_count, 2);

    // ground-truth（日折行存在）已核对，允许清理小时层。
    let pruned = store
        .delete_exchange_hours_of_day("poe2", LEAGUE, "2026-08-31")
        .expect("prune");
    assert_eq!(pruned.hours_deleted, 2);
    assert_eq!(pruned.markets_deleted, 2);
    assert!(
        store
            .load_exchange_hours("poe2", LEAGUE, HOUR, HOUR + 2 * 3600)
            .expect("load")
            .is_empty()
    );
    // 日折行与 day mark 必须原封不动（R4）。
    assert_eq!(
        store
            .load_exchange_days("poe2", LEAGUE, "2026-08-31", "2026-08-31")
            .expect("load days")
            .len(),
        1
    );
    assert_eq!(
        store
            .list_exchange_day_marks("poe2", LEAGUE)
            .expect("marks")
            .len(),
        1
    );
}

#[test]
fn day_replace_rejects_rows_from_another_day() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let row = ExchangeDayMarketRow {
        utc_day: "2026-08-30".to_owned(),
        asset_a: EXALTED.to_owned(),
        asset_b: DIVINE.to_owned(),
        volume_a: 1,
        volume_b: 1,
        hours_covered: 1,
    };
    let result = store.replace_exchange_day("poe2", LEAGUE, "2026-08-31", &[row], 1, now());
    assert!(matches!(result, Err(StorageError::Rejected(_))));
}

#[test]
fn footprint_reports_the_exchange_tables() {
    let store = MarketStore::open_in_memory().expect("store");
    let footprint = store.database_footprint().expect("footprint");
    let names: Vec<&str> = footprint
        .table_rows
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    for table in [
        "exchange_hours",
        "exchange_hour_markets",
        "exchange_day_markets",
        "exchange_day_marks",
    ] {
        assert!(names.contains(&table), "footprint missing {table}");
    }
}

#[test]
fn a_wrong_season_start_can_be_amended_backward() {
    // 三测的实况：0.5 被记成 08-22，真实开服 05-30。单调约束挡住了
    // "重开一个更早的赛季"，所以必须有显式的修正通道。
    let mut store = MarketStore::open_in_memory().expect("store");
    let wrong = Utc.with_ymd_and_hms(2026, 8, 22, 13, 55, 0).unwrap();
    store.start_season("poe2", "0.5", wrong).expect("start");
    let truth = Utc.with_ymd_and_hms(2026, 5, 30, 0, 0, 0).unwrap();
    let amended = store.amend_season_start("poe2", truth).expect("amend");
    assert_eq!(amended.started_at, truth);
    let active = store.active_season("poe2").expect("read").expect("some");
    assert_eq!(active.started_at, truth);
    assert_eq!(active.label, "0.5");
}

#[test]
fn amendment_drags_an_auto_end_but_may_not_eat_the_previous_season() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let old_start = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
    store
        .start_season("poe2", "0.4", old_start)
        .expect("start 0.4");
    let wrong = Utc.with_ymd_and_hms(2026, 8, 22, 0, 0, 0).unwrap();
    store.start_season("poe2", "0.5", wrong).expect("start 0.5");
    // 0.4 的结束是 0.5 开启时自动盖的章（=错误的 08-22）——修正 0.5 到
    // 05-30 时它必须跟着挪，而不是反过来挡住纠错。
    let truth = Utc.with_ymd_and_hms(2026, 5, 30, 0, 0, 0).unwrap();
    store.amend_season_start("poe2", truth).expect("amend");
    let seasons = store.list_seasons("poe2").expect("list");
    assert_eq!(seasons[0].started_at, truth);
    assert_eq!(seasons[1].ended_at, Some(truth));
    // 但吃进上一季的地盘（早于 0.4 的开始）永远非法。
    let overlap = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
    assert!(matches!(
        store.amend_season_start("poe2", overlap),
        Err(StorageError::Rejected(_))
    ));
}
