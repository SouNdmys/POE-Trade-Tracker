//! Contract tests for price history and anchor valuation.

use ptt_market_book::{EvaluatedQuoteEdge, FreshnessStatus};
use ptt_strategy::{
    AnchorAction, BucketSize, ExecutionRisk, MarketPolicy, PriceAnomalyKind, ValuationMode,
    ValuationRequest, ValuationStatus, anomalies, candles, price_points,
    recommend_liquidity_anchors, summarize, value_against_anchor,
};
use ptt_trade_domain::Ratio;

mod support;
use support::{EdgeSpec, asset, at};

/// A rate written in hundredths, so 1033 reads as 10.33. Ratios are stored
/// reduced, so tests compare by value rather than by numerator.
fn rate(hundredths: u64) -> Ratio {
    Ratio::from_parts(hundredths, 100).expect("rate")
}

fn assert_rate(actual: &Ratio, hundredths: u64, label: &str) {
    assert_eq!(
        actual.compare_value(&rate(hundredths)),
        std::cmp::Ordering::Equal,
        "{label}: expected {hundredths} hundredths, got {actual:?}"
    );
}

fn series() -> Vec<EvaluatedQuoteEdge> {
    vec![
        EdgeSpec::new("a", 1_033, 0).build(),
        EdgeSpec::new("b", 1_043, 2).build(),
        EdgeSpec::new("c", 1_025, 4).build(),
        // Second bucket, six minutes in.
        EdgeSpec::new("d", 1_047, 6).build(),
        EdgeSpec::new("e", 1_021, 8).maker().build(),
    ]
}

#[test]
fn a_series_keeps_only_the_direction_it_was_asked_for() {
    let mut edges = series();
    edges.push(EdgeSpec::new("reverse", 10, 1).reversed().build());
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));

    assert_eq!(points.len(), 5, "the reverse quote is a different series");
    assert!(
        points
            .windows(2)
            .all(|pair| pair[0].captured_at <= pair[1].captured_at),
        "points come back in time order"
    );
}

#[test]
fn candles_prefer_traded_prices_and_say_when_they_had_none() {
    let edges = series();
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let candles = candles(&points, BucketSize::FiveMinutes);

    assert_eq!(candles.len(), 2);
    let first = &candles[0];
    assert_eq!(first.bucket_start, at(0));
    assert_rate(&first.open, 1_033, "first.open");
    assert_rate(&first.close, 1_025, "first.close");
    assert_rate(&first.high, 1_043, "first.high");
    assert_rate(&first.low, 1_025, "first.low");
    assert_eq!(first.sample_count, 3);
    assert!(!first.maker_only);

    let second = &candles[1];
    // 1047 is a taker price, 1021 is a listing: the candle prices the trade.
    assert_eq!(second.taker_count, 1);
    assert_eq!(second.maker_count, 1);
    assert_rate(&second.open, 1_047, "second.open");
    assert_rate(&second.close, 1_047, "second.close");
    assert!(!second.maker_only);
}

#[test]
fn a_bucket_of_listings_only_is_marked_as_asking_prices() {
    let edges = vec![
        EdgeSpec::new("m1", 1_021, 0).maker().build(),
        EdgeSpec::new("m2", 1_030, 1).maker().build(),
    ];
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let candles = candles(&points, BucketSize::FiveMinutes);

    assert_eq!(candles.len(), 1);
    assert!(
        candles[0].maker_only,
        "nothing traded here, so the candle must not pass as a trade price"
    );
}

#[test]
fn the_summary_selects_real_observations_rather_than_averaging_them() {
    let edges = series();
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let summary = summarize(&points, &asset("divine-orb"), &asset("chaos-orb"));

    assert_eq!(summary.point_count, 5);
    assert_rate(&summary.min_rate.clone().expect("min"), 1_021, "min");
    assert_rate(&summary.max_rate.clone().expect("max"), 1_047, "max");
    // Sorted: 1021, 1025, 1033, 1043, 1047 -> the middle one, exactly.
    assert_rate(
        &summary.median_rate.clone().expect("median"),
        1_033,
        "median",
    );
    assert_rate(
        &summary.latest_taker_rate.clone().expect("taker"),
        1_047,
        "latest taker",
    );
    assert_rate(
        &summary.latest_maker_rate.clone().expect("maker"),
        1_021,
        "latest maker",
    );
    assert!(!summary.historical_only);
}

#[test]
fn a_series_with_nothing_current_is_labelled_history() {
    let edges = vec![
        EdgeSpec::new("old", 1_033, 0)
            .freshness(FreshnessStatus::Stale)
            .build(),
        EdgeSpec::new("older", 1_040, 1)
            .freshness(FreshnessStatus::Archived)
            .build(),
    ];
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let summary = summarize(&points, &asset("divine-orb"), &asset("chaos-orb"));

    assert!(summary.historical_only);
    let found = anomalies(
        &summary,
        &points,
        &ptt_strategy::AnomalyThresholds::default(),
    );
    assert!(
        found
            .iter()
            .any(|item| item.kind == PriceAnomalyKind::StaleLatest)
    );
}

#[test]
fn a_future_capture_is_an_anomaly_not_the_freshest_price() {
    // B-15: the JS clamped a future timestamp to age zero, so a wrong clock
    // read as the newest possible data and stayed fresh.
    let edges = vec![
        EdgeSpec::new("now", 1_033, 0).build(),
        EdgeSpec::new("ahead", 1_035, 5)
            .ahead_of_the_clock()
            .build(),
    ];
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let summary = summarize(&points, &asset("divine-orb"), &asset("chaos-orb"));
    let found = anomalies(
        &summary,
        &points,
        &ptt_strategy::AnomalyThresholds::default(),
    );

    assert!(
        found
            .iter()
            .any(|item| item.kind == PriceAnomalyKind::ClockSkewFuture),
        "a capture ahead of the clock must be reported, not absorbed"
    );
}

#[test]
fn thin_stock_on_the_newest_point_is_flagged() {
    let edges = vec![
        EdgeSpec::new("deep", 1_033, 0).build(),
        EdgeSpec::new("thin", 1_034, 1).stock(3).build(),
    ];
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let summary = summarize(&points, &asset("divine-orb"), &asset("chaos-orb"));
    let found = anomalies(
        &summary,
        &points,
        &ptt_strategy::AnomalyThresholds::default(),
    );

    assert!(
        found
            .iter()
            .any(|item| item.kind == PriceAnomalyKind::ThinLiquidity)
    );
}

#[test]
fn anomaly_bars_come_from_the_caller_not_private_constants() {
    // A 15% jump: quiet under the shipped 20% bar, a spike once the caller
    // tightens the bar — the same knob every other risk site reads.
    let edges = vec![
        EdgeSpec::new("steady-1", 1_000, 0).build(),
        EdgeSpec::new("steady-2", 1_000, 1).build(),
        EdgeSpec::new("jump", 1_150, 2).build(),
    ];
    let points = price_points(&edges, &asset("divine-orb"), &asset("chaos-orb"));
    let summary = summarize(&points, &asset("divine-orb"), &asset("chaos-orb"));

    let default_bars = ptt_strategy::AnomalyThresholds::default();
    let quiet = anomalies(&summary, &points, &default_bars);
    assert!(
        quiet
            .iter()
            .all(|item| item.kind != PriceAnomalyKind::RateSpike),
        "15% is inside the shipped 20% band: {quiet:?}"
    );

    let tightened = ptt_strategy::AnomalyThresholds {
        spike_basis_points: 1_000,
        ..default_bars
    };
    let flagged = anomalies(&summary, &points, &tightened);
    assert!(
        flagged
            .iter()
            .any(|item| item.kind == PriceAnomalyKind::RateSpike),
        "the tightened bar must flag the same move: {flagged:?}"
    );
}

#[test]
fn a_valuation_with_only_one_side_says_so() {
    let edges = vec![EdgeSpec::new("sell", 1_033, 0).build()];
    let valuation = value_against_anchor(ValuationRequest {
        asset_id: &asset("divine-orb"),
        anchor_asset_id: &asset("chaos-orb"),
        mode: ValuationMode::SellValue,
        edges: &edges,
        include_historical: false,
    });

    assert_eq!(valuation.status, ValuationStatus::OneSided);
    assert_rate(&valuation.sell_value.clone().expect("sell"), 1_033, "sell");
    assert!(valuation.buy_cost.is_none());
    assert!(valuation.midpoint.is_none());
    assert!(valuation.spread_basis_points.is_none());
}

#[test]
fn a_midpoint_needs_both_sides_and_admits_it_is_approximate() {
    // Sell one divine for 10 chaos; buy one divine for 12.5 chaos.
    let edges = vec![
        EdgeSpec::new("sell", 1_000, 0).build(),
        EdgeSpec::new("buy", 8, 1).reversed().build(),
    ];
    let valuation = value_against_anchor(ValuationRequest {
        asset_id: &asset("divine-orb"),
        anchor_asset_id: &asset("chaos-orb"),
        mode: ValuationMode::Midpoint,
        edges: &edges,
        include_historical: false,
    });

    assert_eq!(valuation.status, ValuationStatus::TwoSided);
    assert_rate(&valuation.sell_value.clone().expect("sell"), 1_000, "sell");
    // 0.08 chaos per divine inverts to 12.5 chaos to buy one.
    let buy = valuation.buy_cost.expect("buy");
    assert_eq!((buy.numerator, buy.denominator), (25, 2));
    assert!(valuation.midpoint_is_approximate);
    let midpoint = valuation.midpoint.expect("midpoint");
    // sqrt(10 * 12.5) = 11.18...
    assert_eq!(midpoint.numerator * 100 / midpoint.denominator, 1_118);
    // Buying costs 25% more than selling returns.
    assert_eq!(valuation.spread_basis_points, Some(2_500));
}

#[test]
fn a_missing_pair_asks_for_a_probe_rather_than_reporting_a_price() {
    let valuation = value_against_anchor(ValuationRequest {
        asset_id: &asset("divine-orb"),
        anchor_asset_id: &asset("chaos-orb"),
        mode: ValuationMode::SellValue,
        edges: &[],
        include_historical: false,
    });

    assert_eq!(valuation.status, ValuationStatus::NoPath);
    assert!(valuation.value.is_none());
    assert!(valuation.risks.contains(&ExecutionRisk::NeedsProbe));
}

#[test]
fn a_well_connected_newcomer_is_recommended_and_an_isolated_one_is_not() {
    let policy = MarketPolicy::default_for("rise-of-the-abyssal");
    let mut edges = Vec::new();
    // annulment trades both ways against three different peers.
    for (index, peer) in ["divine-orb", "chaos-orb", "exalted-orb"]
        .into_iter()
        .enumerate()
    {
        let minute = u32::try_from(index).unwrap_or(0);
        let mut out = EdgeSpec::new("x", 100, minute).build();
        out.observation.edge.edge_id = format!("out-{peer}");
        out.observation.edge.from_asset_id = asset("annulment-orb");
        out.observation.edge.to_asset_id = asset(peer);
        let mut back = EdgeSpec::new("y", 100, minute).build();
        back.observation.edge.edge_id = format!("in-{peer}");
        back.observation.edge.from_asset_id = asset(peer);
        back.observation.edge.to_asset_id = asset("annulment-orb");
        edges.push(out);
        edges.push(back);
    }

    let recommendations = recommend_liquidity_anchors(&edges, &policy);
    let annulment = recommendations
        .iter()
        .find(|item| item.asset_id.as_str() == "annulment-orb")
        .expect("annulment recommended");
    assert_eq!(annulment.action, AnchorAction::PromoteToCore);
    assert_eq!(annulment.pair_coverage_count, 3);
    assert_eq!(annulment.bidirectional_pair_count, 3);
    assert!(
        recommendations
            .iter()
            .all(|item| !policy.is_core_liquidity(&item.asset_id)),
        "currencies already treated as money are not recommendations"
    );
}
