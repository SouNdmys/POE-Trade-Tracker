//! Contract tests for the P10 analytics core: the day-rollup fold and the
//! market-pulse analysis (attribution, anchor breadth, classification).

use chrono::{DateTime, TimeZone, Utc};
use ptt_strategy::{
    AnalyticsThresholds, AnchorDrift, DailyPairStat, LiquidityClass, TrendVerdict,
    build_pair_day_rollups, market_pulse,
};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};

fn asset(id: &str) -> MarketAssetId {
    MarketAssetId::try_new(id).expect("asset id")
}

fn at(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, hour, 0, 0)
        .single()
        .expect("timestamp")
}

fn ratio(numerator: u64, denominator: u64) -> Ratio {
    Ratio::from_parts(numerator, denominator).expect("ratio")
}

/// One panel row exactly as the domain stores it: a forward edge in panel
/// orientation (from=have, to=need, rate as displayed) plus the inverse
/// restatement. Mirrors `build_quote_edges` so the fold sees real shapes.
#[allow(clippy::too_many_arguments)]
fn panel_row(
    snapshot_id: &str,
    context_key: &str,
    need: &str,
    have: &str,
    side: QuoteSide,
    row_index: u8,
    rate: Ratio,
    stock: u64,
    comparator: Comparator,
    captured_at: DateTime<Utc>,
) -> Vec<MarketEdgeObservation> {
    let (forward_execution, forward_role, reverse_execution, reverse_role) = match side {
        QuoteSide::Available => (
            ExecutionType::Taker,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::MakerReference,
            QuoteEdgeRole::AvailableReverseMakerReference,
        ),
        QuoteSide::Competing => (
            ExecutionType::MakerReference,
            QuoteEdgeRole::CompetingMakerReference,
            ExecutionType::Taker,
            QuoteEdgeRole::CompetingReverseTaker,
        ),
    };
    let base = |suffix: &str,
                from: &str,
                to: &str,
                rate: Ratio,
                execution: ExecutionType,
                role: QuoteEdgeRole| MarketEdgeObservation {
        edge: QuoteEdge {
            edge_id: format!("{snapshot_id}-{row_index}-{suffix}"),
            snapshot_id: snapshot_id.to_owned(),
            quote_id: format!("{snapshot_id}-{row_index}"),
            context_key: context_key.to_owned(),
            from_asset_id: asset(from),
            to_asset_id: asset(to),
            rate,
            source_side: side,
            execution_type: execution,
            role,
            stock,
            original_need_asset_id: asset(need),
            original_have_asset_id: asset(have),
            original_row_index: row_index,
            comparator,
            user_edited: false,
            machine_confidence_ppm: Some(990_000),
            captured_at,
            confirmed_at: captured_at,
        },
        snapshot_complete: true,
        record_status: SnapshotRecordStatus::Active,
        record_revision: 1,
        record_reason: None,
    };
    vec![
        base(
            "f",
            have,
            need,
            rate.clone(),
            forward_execution,
            forward_role,
        ),
        base(
            "r",
            need,
            have,
            rate.inverse(),
            reverse_execution,
            reverse_role,
        ),
    ]
}

// ----- day_rollup: the fold -----

#[test]
fn fold_uses_floor_division_per_row_before_summing() {
    // Rate 3:7 need-per-have. Available stock is in need units; each row
    // converts to have units as floor(stock * 7 / 3):
    //   10 -> 23 (23.33), 11 -> 25 (25.67), sum 48.
    // Summing first would give floor(21 * 7 / 3) = 49 — the wrong discipline.
    let mut edges = Vec::new();
    edges.extend(panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Available,
        0,
        ratio(3, 7),
        10,
        Comparator::Exact,
        at(20, 10),
    ));
    edges.extend(panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Available,
        1,
        ratio(3, 7),
        11,
        Comparator::Exact,
        at(20, 10),
    ));
    // Competing stock is in have units; to need units: floor(10 * 3 / 7) = 4.
    edges.extend(panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Competing,
        2,
        ratio(3, 7),
        10,
        Comparator::Exact,
        at(20, 10),
    ));

    let rollups = build_pair_day_rollups(&edges, 3);
    assert_eq!(rollups.len(), 1);
    let rollup = &rollups[0];
    assert_eq!(rollup.median_available_sum_need_units, 21);
    assert_eq!(
        rollup.median_available_sum_have_units, 48,
        "floor per row, then sum"
    );
    assert_eq!(rollup.median_competing_sum_have_units, 10);
    assert_eq!(rollup.median_competing_sum_need_units, 4);
}

#[test]
fn fold_ignores_reverse_role_edges() {
    // panel_row emits forward + reverse for each row, exactly like storage.
    // One available row of stock 920 must count once, not twice.
    let edges = panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Available,
        0,
        ratio(49, 5),
        920,
        Comparator::Exact,
        at(20, 10),
    );
    let rollups = build_pair_day_rollups(&edges, 3);
    assert_eq!(rollups.len(), 1);
    assert_eq!(rollups[0].median_available_rows, 1);
    assert_eq!(rollups[0].median_available_sum_need_units, 920);
    assert_eq!(rollups[0].median_competing_rows, 0);
}

#[test]
fn top_taker_rate_is_max_by_exact_cross_multiply() {
    let mut edges = Vec::new();
    // 49:5 = 9.8 and 59:6 = 9.83…: cross-multiply picks 59:6.
    for (index, rate) in [(0u8, ratio(49, 5)), (1u8, ratio(59, 6))] {
        edges.extend(panel_row(
            "s1",
            "ctx",
            "divine-orb",
            "chaos-orb",
            QuoteSide::Available,
            index,
            rate,
            100,
            Comparator::Exact,
            at(20, 10),
        ));
    }
    // An aggregate boundary row with a better-looking rate must not win: it
    // prices the whole tail, not the front.
    edges.extend(panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Available,
        2,
        ratio(100, 1),
        5_000,
        Comparator::LessThan,
        at(20, 10),
    ));

    let rollups = build_pair_day_rollups(&edges, 3);
    let top = rollups[0]
        .median_top_taker_rate
        .as_ref()
        .expect("priced day");
    assert_eq!(
        top.compare_value(&ratio(59, 6)),
        std::cmp::Ordering::Equal,
        "exact max, aggregate row excluded"
    );
    // The aggregate row's stock still counts toward circulation.
    assert_eq!(rollups[0].median_available_sum_need_units, 5_200);
}

/// The day's rate is a *maximum*, which is the single most outlier-fragile
/// statistic there is — and OCR drops decimal points.
///
/// The live book on 2026-08-24: `divine-orb -> orb-of-annulment` carried
/// `2131 : 1` beside the true `23 : 10` and `9 : 4`, because 2.131 was read
/// without its point. A maximum hands the whole day to that row, and every
/// valuation downstream then divides by it — 188,175 annulments came out
/// worth 88 divine. The trading path never saw this: the Convert page runs
/// through `select_quote_edges`, which applies the median-baseline outlier
/// band. The valuation path reads raw observations and had no gate at all.
///
/// The bad row's *quantity* still counts. An outlier price does not make the
/// currency behind it imaginary, and `leg_book` already reasons this way for
/// the aggregate row it likewise refuses to price from.
#[test]
fn an_ocr_row_that_lost_its_decimal_point_does_not_set_the_day_rate() {
    let mut edges = Vec::new();
    for (index, rate, stock) in [
        // 2.131 read without its decimal point: a thousand times its side.
        (0u8, ratio(2131, 1), 74u64),
        (1u8, ratio(23, 10), 500),
        (2u8, ratio(9, 4), 300),
    ] {
        edges.extend(panel_row(
            "s1",
            "ctx",
            "orb-of-annulment",
            "divine-orb",
            QuoteSide::Available,
            index,
            rate,
            stock,
            Comparator::Exact,
            at(24, 10),
        ));
    }

    let rollups = build_pair_day_rollups(&edges, 3);
    let top = rollups[0]
        .median_top_taker_rate
        .as_ref()
        .expect("priced day");
    assert_eq!(
        top.compare_value(&ratio(23, 10)),
        std::cmp::Ordering::Equal,
        "a row a thousand times off its side's median must not set the day's \
         rate, got {top:?}"
    );
    assert_eq!(
        rollups[0].median_available_sum_need_units, 874,
        "the outlier row's stock is still currency on the market"
    );
}

#[test]
fn maker_only_snapshot_has_no_top_rate() {
    let edges = panel_row(
        "s1",
        "ctx",
        "divine-orb",
        "chaos-orb",
        QuoteSide::Competing,
        0,
        ratio(49, 5),
        100,
        Comparator::Exact,
        at(20, 10),
    );
    let rollups = build_pair_day_rollups(&edges, 3);
    assert_eq!(rollups[0].median_top_taker_rate, None);
    assert_eq!(rollups[0].median_competing_rows, 1);
}

#[test]
fn median_takes_lower_middle_for_even_counts() {
    let mut edges = Vec::new();
    for (snapshot, stock) in [("s1", 10u64), ("s2", 20), ("s3", 30), ("s4", 40)] {
        edges.extend(panel_row(
            snapshot,
            "ctx",
            "divine-orb",
            "chaos-orb",
            QuoteSide::Available,
            0,
            ratio(49, 5),
            stock,
            Comparator::Exact,
            at(20, 10),
        ));
    }
    let rollups = build_pair_day_rollups(&edges, 3);
    assert_eq!(rollups[0].snapshot_count, 4);
    assert_eq!(
        rollups[0].median_available_sum_need_units, 20,
        "lower middle of 10/20/30/40"
    );
}

#[test]
fn median_rate_is_an_observed_ratio_never_averaged() {
    let mut edges = Vec::new();
    for (snapshot, rate) in [("s1", ratio(49, 5)), ("s2", ratio(50, 5))] {
        edges.extend(panel_row(
            snapshot,
            "ctx",
            "divine-orb",
            "chaos-orb",
            QuoteSide::Available,
            0,
            rate,
            100,
            Comparator::Exact,
            at(20, 10),
        ));
    }
    let rollups = build_pair_day_rollups(&edges, 3);
    let median = rollups[0]
        .median_top_taker_rate
        .as_ref()
        .expect("priced day");
    assert_eq!(
        median.compare_value(&ratio(49, 5)),
        std::cmp::Ordering::Equal,
        "the lower-middle observed rate, not an average"
    );
}

#[test]
fn rollup_merges_snapshots_across_two_context_keys() {
    // The same book captured under two context keys (an app release rotated
    // the key mid-day) is one market, one rollup row.
    let mut edges = Vec::new();
    for (snapshot, context) in [("s1", "ctx-v1"), ("s2", "ctx-v2")] {
        edges.extend(panel_row(
            snapshot,
            context,
            "divine-orb",
            "chaos-orb",
            QuoteSide::Available,
            0,
            ratio(49, 5),
            100,
            Comparator::Exact,
            at(20, 10),
        ));
    }
    let rollups = build_pair_day_rollups(&edges, 3);
    assert_eq!(rollups.len(), 1, "one book, not one per context");
    assert_eq!(rollups[0].snapshot_count, 2);
    assert_eq!(rollups[0].contexts_merged, 2);
}

// ----- market_analytics: attribution, breadth, classification -----

fn stat(day: &str, need: &str, have: &str) -> DailyPairStat {
    let captured = format!("{day}T12:00:00+00:00");
    DailyPairStat {
        utc_day: day.to_owned(),
        need_asset_id: asset(need),
        have_asset_id: asset(have),
        snapshot_count: 1,
        last_captured_at: DateTime::parse_from_rfc3339(&captured)
            .expect("time")
            .with_timezone(&Utc),
        available_rows: 0,
        competing_rows: 0,
        available_sum_need_units: 0,
        available_sum_have_units: 0,
        competing_sum_have_units: 0,
        competing_sum_need_units: 0,
        top_taker_rate: None,
    }
}

fn thresholds() -> AnalyticsThresholds {
    AnalyticsThresholds::default()
}

#[test]
fn supply_demand_attribution_matches_the_payout_side_rule() {
    // Book (need=divine, have=chaos): the available side pays out divine
    // (divine supply, chaos demand); the competing side pays out chaos
    // (chaos supply, divine demand). TASK-50.
    let mut book = stat("2026-08-22", "divine-orb", "chaos-orb");
    book.available_rows = 6;
    book.competing_rows = 5;
    book.available_sum_need_units = 920;
    book.available_sum_have_units = 9_016;
    book.competing_sum_have_units = 611_620;
    book.competing_sum_need_units = 62_730;
    book.top_taker_rate = Some(ratio(1, 10)); // 1 divine per 10 chaos

    let pulse = market_pulse(&[book], &[asset("divine-orb")], &thresholds());
    let divine = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("divine-orb"))
        .expect("divine pulse");
    let chaos = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("chaos-orb"))
        .expect("chaos pulse");

    assert_eq!(divine.supply_units, 920);
    assert_eq!(divine.demand_units, 62_730);
    assert_eq!(chaos.supply_units, 611_620);
    assert_eq!(chaos.demand_units, 9_016);
    // Anchor valuation: divine 1:1; chaos at 1 divine per 10 chaos.
    assert_eq!(divine.supply_anchor, Some(920));
    assert_eq!(divine.demand_anchor, Some(62_730));
    assert_eq!(chaos.supply_anchor, Some(61_162));
    assert_eq!(chaos.demand_anchor, Some(901));
    // The unit trap the whole feature exists to avoid: comparing the two
    // sides raw would inflate the imbalance by the exchange rate.
    assert!(chaos.supply_anchor < Some(chaos.supply_units));
}

#[test]
fn mirrored_books_are_one_market_not_two() {
    // The same standing orders seen from both panel orientations on the same
    // day: the denser capture wins, quantities are not doubled.
    let mut dense = stat("2026-08-22", "divine-orb", "chaos-orb");
    dense.snapshot_count = 3;
    dense.available_sum_need_units = 920;
    dense.competing_sum_need_units = 62_730;
    dense.competing_sum_have_units = 611_620;
    dense.available_sum_have_units = 9_016;
    dense.top_taker_rate = Some(ratio(1, 10));

    let mut sparse_mirror = stat("2026-08-22", "chaos-orb", "divine-orb");
    sparse_mirror.snapshot_count = 1;
    sparse_mirror.available_sum_need_units = 999_999;
    sparse_mirror.competing_sum_need_units = 999_999;
    sparse_mirror.competing_sum_have_units = 999_999;
    sparse_mirror.available_sum_have_units = 999_999;

    let pulse = market_pulse(
        &[dense, sparse_mirror],
        &[asset("divine-orb")],
        &thresholds(),
    );
    let divine = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("divine-orb"))
        .expect("divine pulse");
    assert_eq!(
        divine.supply_units, 920,
        "the sparse mirror orientation is discarded, not added"
    );
}

/// Seven baseline days at `base` hundredths, two recent days at `recent`
/// hundredths, direct pair (need=anchor, have=asset).
fn trending_asset(name: &str, base: u64, recent: u64) -> Vec<DailyPairStat> {
    let mut stats = Vec::new();
    for day in 13..=20 {
        let mut entry = stat(&format!("2026-08-{day:02}"), "divine-orb", name);
        entry.top_taker_rate = Some(ratio(base, 100));
        stats.push(entry);
    }
    for day in 21..=22 {
        let mut entry = stat(&format!("2026-08-{day:02}"), "divine-orb", name);
        entry.top_taker_rate = Some(ratio(recent, 100));
        stats.push(entry);
    }
    stats
}

#[test]
fn a_rising_market_is_the_anchor_falling() {
    // Four assets all up ~10% against divine; the mirror up 11%. Breadth
    // reads that as divine inflating, and the mirror's verdict comes from
    // its move RELATIVE to the market: holding, not appreciating.
    let mut stats = Vec::new();
    stats.extend(trending_asset("chaos-orb", 100, 110));
    stats.extend(trending_asset("exalted-orb", 200, 220));
    stats.extend(trending_asset("annul-orb", 300, 330));
    stats.extend(trending_asset("mirror-of-kalandra", 1_000, 1_110));

    let pulse = market_pulse(&stats, &[asset("divine-orb")], &thresholds());
    let health = pulse.anchor_health.expect("anchor health");
    assert_eq!(health.drift, AnchorDrift::Inflating, "everything rose");
    assert_eq!(health.risers, 4);
    assert_eq!(health.fallers, 0);
    assert_eq!(health.market_median_move_bps, Some(1_000));

    let mirror = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("mirror-of-kalandra"))
        .expect("mirror pulse");
    assert_eq!(mirror.trend_bps_raw, Some(1_100));
    assert_eq!(mirror.trend_bps_relative, Some(100));
    assert_eq!(
        mirror.verdict,
        Some(TrendVerdict::Holding),
        "holding value while the anchor falls, not appreciating"
    );
}

#[test]
fn scarce_and_oversupplied_are_distinguished_by_imbalance_direction() {
    // Both books are thin. The mirror is demanded and barely supplied; the
    // junk is supplied and barely demanded. Thinness is the same, the
    // imbalance direction is the signal.
    let mut mirror = stat("2026-08-22", "mirror-of-kalandra", "divine-orb");
    mirror.available_sum_need_units = 2; // 2 mirrors listed
    mirror.competing_sum_need_units = 9; // 9 mirrors' worth of buy pressure
    mirror.top_taker_rate = Some(ratio(1, 700)); // 700 divine per mirror
    // value_in_anchor needs need=anchor orientation; invert the book:
    let mut mirror_priced = stat("2026-08-21", "divine-orb", "mirror-of-kalandra");
    mirror_priced.top_taker_rate = Some(ratio(700, 1));

    let mut junk = stat("2026-08-22", "scroll-of-wisdom", "divine-orb");
    junk.available_sum_need_units = 90_000;
    junk.competing_sum_need_units = 10_000;
    let mut junk_priced = stat("2026-08-21", "divine-orb", "scroll-of-wisdom");
    junk_priced.top_taker_rate = Some(ratio(1, 500)); // 500 scrolls per divine

    let pulse = market_pulse(
        &[mirror, mirror_priced, junk, junk_priced],
        &[asset("divine-orb")],
        &thresholds(),
    );
    let mirror_pulse = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("mirror-of-kalandra"))
        .expect("mirror");
    let junk_pulse = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("scroll-of-wisdom"))
        .expect("junk");

    assert_eq!(mirror_pulse.class, LiquidityClass::Scarce);
    assert_eq!(junk_pulse.class, LiquidityClass::Oversupplied);
}

#[test]
fn too_little_on_both_sides_is_quiet_not_a_verdict() {
    // A 3x imbalance on amounts worth less than the quiet floor reads as
    // "no market", never as scarce.
    let mut tiny = stat("2026-08-22", "shard-of-nothing", "divine-orb");
    tiny.available_sum_need_units = 1;
    tiny.competing_sum_need_units = 3;
    let mut priced = stat("2026-08-21", "divine-orb", "shard-of-nothing");
    priced.top_taker_rate = Some(ratio(1, 1)); // 1 divine each: 1 vs 3 anchor units

    let pulse = market_pulse(&[tiny, priced], &[asset("divine-orb")], &thresholds());
    let tiny_pulse = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("shard-of-nothing"))
        .expect("tiny");
    assert_eq!(tiny_pulse.class, LiquidityClass::Quiet);
}

#[test]
fn a_sparse_series_reports_no_trend_instead_of_guessing() {
    let mut lone = stat("2026-08-22", "divine-orb", "chaos-orb");
    lone.top_taker_rate = Some(ratio(1, 10));
    let pulse = market_pulse(&[lone], &[asset("divine-orb")], &thresholds());
    let chaos = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("chaos-orb"))
        .expect("chaos");
    assert_eq!(chaos.trend_bps_raw, None, "one day is not a trend");
    assert_eq!(chaos.verdict, None);
}

#[test]
fn scarce_plus_upward_relative_drift_is_the_greedy_candidate() {
    // The greedy-mode precondition: undersupplied AND not depreciating
    // relative to the market.
    let mut stats = Vec::new();
    // Market context: three assets flat, so the median move is ~0.
    stats.extend(trending_asset("chaos-orb", 100, 100));
    stats.extend(trending_asset("exalted-orb", 200, 200));
    stats.extend(trending_asset("annul-orb", 300, 300));
    // The mirror trends up and is scarce. Its day-22 book carries the stock
    // in the same orientation (need=divine, have=mirror): the competing side
    // pays out mirrors (supply 2), the available side seeks them (demand 9).
    stats.extend(trending_asset("mirror-of-kalandra", 70_000, 77_000));
    {
        let day22 = stats.last_mut().expect("mirror day 22");
        day22.competing_sum_have_units = 2;
        day22.available_sum_have_units = 9;
    }
    // Junk is oversupplied: never a greedy candidate even if it drifts up.
    let mut junk_priced = stat("2026-08-21", "divine-orb", "scroll-of-wisdom");
    junk_priced.top_taker_rate = Some(ratio(1, 500));
    stats.push(junk_priced);
    let mut junk_book = stat("2026-08-22", "divine-orb", "scroll-of-wisdom");
    junk_book.competing_sum_have_units = 90_000;
    junk_book.available_sum_have_units = 10_000;
    stats.push(junk_book);

    let pulse = market_pulse(&stats, &[asset("divine-orb")], &thresholds());
    let mirror = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("mirror-of-kalandra"))
        .expect("mirror");
    let junk = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("scroll-of-wisdom"))
        .expect("junk");

    assert_eq!(mirror.class, LiquidityClass::Scarce);
    assert!(mirror.trend_bps_relative.expect("trended") >= 0);
    assert!(mirror.greedy_candidate);
    assert!(!junk.greedy_candidate);
}

#[test]
fn a_value_without_a_direct_pair_composes_through_the_second_anchor() {
    // No divine pair for the gem, but chaos prices it and divine prices
    // chaos: one exact composition step, flagged as composed.
    let mut gem_in_chaos = stat("2026-08-22", "chaos-orb", "gemcutters-prism");
    gem_in_chaos.top_taker_rate = Some(ratio(5, 1)); // 5 chaos per prism
    let mut chaos_in_divine = stat("2026-08-22", "divine-orb", "chaos-orb");
    chaos_in_divine.top_taker_rate = Some(ratio(1, 10)); // 0.1 divine per chaos

    let pulse = market_pulse(
        &[gem_in_chaos, chaos_in_divine],
        &[asset("divine-orb"), asset("chaos-orb")],
        &thresholds(),
    );
    let prism = pulse
        .assets
        .iter()
        .find(|pulse| pulse.asset_id == asset("gemcutters-prism"))
        .expect("prism");
    let value = prism.value_in_anchor.as_ref().expect("composed value");
    assert_eq!(
        value.compare_value(&ratio(1, 2)),
        std::cmp::Ordering::Equal,
        "5 chaos x 0.1 divine = 0.5 divine, exactly"
    );
    assert!(prism.value_is_composed);
}
