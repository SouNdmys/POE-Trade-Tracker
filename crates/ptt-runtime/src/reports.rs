//! Read-only reports built from the store, one per UI page.
//!
//! These live here rather than in the app so the numbers can be tested
//! without a window, and so the visual layer stays free to change without
//! touching anything that computes. Each returns display lines: the app
//! renders them and nothing else.

use std::collections::BTreeMap;

use chrono::Utc;
use ptt_market_book::{
    DataVisibility, EvaluatedQuoteEdge, QuoteSelectionPolicy, QuoteSelectionStrategy,
    build_coherent_current_book, select_quote_edges,
};
use ptt_strategy::{
    Actionability, BucketSize, MarkRateTable, MarketPolicy, ProfitTier, RiskThresholds,
    RouteAccountingRequest, ValuationMode, ValuationRequest, ValuationStatus, anomalies, candles,
    derive_route_accounting, price_points, recommend_liquidity_anchors, summarize,
    value_against_anchor,
};
use ptt_trade_domain::{MarketAssetId, MarketEdgeObservation};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ComparisonDirection, ConversionRequest, FeePolicy,
    MarketDepthIndex, SearchCancellation, find_best_conversion,
};
use ptt_workflows::{
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, derive_focus_probe_candidates,
};

/// The sizes the convert page prices, in whole orbs.
const CONVERT_SIZES: [u64; 3] = [1, 10, 100];

/// Everything the engine needs, assembled once from stored observations.
struct Market {
    index: MarketDepthIndex,
    units: AssetUnitCatalog,
    selected: Vec<EvaluatedQuoteEdge>,
    mark_rates: MarkRateTable,
}

fn build_market(
    observations: &[MarketEdgeObservation],
    context_key: &str,
) -> Result<Market, String> {
    let book = build_coherent_current_book(context_key, observations, DataVisibility::default())
        .map_err(|error| format!("book: {error}"))?;
    let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
        .map_err(|error| format!("policy: {error}"))?;
    let selection = select_quote_edges(&book, &policy, Utc::now())
        .map_err(|error| format!("select: {error}"))?;

    let mut units = BTreeMap::new();
    for observation in observations {
        units.insert(observation.edge.from_asset_id.clone(), AssetUnit::whole());
        units.insert(observation.edge.to_asset_id.clone(), AssetUnit::whole());
    }
    let units = AssetUnitCatalog::try_new(units).map_err(|error| format!("units: {error}"))?;
    let index = MarketDepthIndex::try_from_selection(&selection, units.clone())
        .map_err(|error| format!("index: {error}"))?;

    let mut selected = Vec::new();
    let mut mark_rates = MarkRateTable::new();
    for entry in &selection.selections {
        if let Some(edge) = &entry.selected_edge {
            mark_rates.insert(
                &edge.observation.edge.from_asset_id,
                &edge.observation.edge.to_asset_id,
                edge.observation.edge.rate.clone(),
            );
            selected.push(edge.clone());
        }
        selected.extend(entry.candidate_edges.iter().cloned());
    }

    Ok(Market {
        index,
        units,
        selected,
        mark_rates,
    })
}

fn tier_line(label: &str, tier: &ProfitTier) -> String {
    let profit = match (tier.direction, &tier.delta, tier.basis_points) {
        (Some(ComparisonDirection::Improved), Some(delta), Some(bps)) => {
            format!("+{} ({bps}bp vs direct)", delta.quanta)
        }
        (Some(ComparisonDirection::Worse), Some(delta), Some(bps)) => {
            format!("-{} ({bps}bp vs direct)", delta.quanta)
        }
        (Some(ComparisonDirection::Equal), _, _) => "level with direct".to_owned(),
        // No direct route observed: showing the route is useful, calling it
        // an improvement over nothing is not.
        _ => "no direct route to compare".to_owned(),
    };
    format!(
        "{label:<12} {} in -> {} out   {profit}",
        tier.input.quanta, tier.output.quanta
    )
}

/// "I hold X and want Y": the three profit tiers at a few sizes, plus what
/// gets stranded and what it would have to clear at to break even.
pub fn convert_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
) -> Result<Vec<String>, String> {
    let market = build_market(observations, context_key)?;
    let mut lines = Vec::new();

    for size in CONVERT_SIZES {
        let Ok(amount_in) = AssetAmount::from_whole_units(have.clone(), size, &market.units) else {
            continue;
        };
        let conversion = find_best_conversion(
            &market.index,
            &ConversionRequest {
                from_asset_id: have.clone(),
                to_asset_id: need.clone(),
                amount_in,
                max_hops: 3,
                max_paths: 64,
                max_expansions: 10_000,
                alternative_limit: 2,
                allowed_intermediate_asset_ids: None,
                // Gross by product decision: no monetary fee is modelled.
                fee_policy: FeePolicy::None,
            },
            &SearchCancellation::default(),
        )
        .map_err(|error| format!("convert: {error:?}"))?;

        let Some(best) = &conversion.best_path else {
            lines.push(format!("{size:>4} {have} -> {need}: no route yet"));
            continue;
        };
        let accounting = derive_route_accounting(RouteAccountingRequest {
            path: best,
            direct_path: conversion.direct_path.as_ref(),
            mark_rates: &market.mark_rates,
            thresholds: RiskThresholds::default(),
            needs_probe: false,
        })
        .map_err(|error| format!("accounting: {error}"))?;

        let route = accounting
            .route_asset_ids
            .iter()
            .map(MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(" -> ");
        lines.push(format!("{size:>4} {have}   via {route}"));
        lines.push(format!("     {}", tier_line("closed", &accounting.closed)));
        lines.push(format!(
            "     {}",
            tier_line("theoretical", &accounting.theoretical)
        ));
        lines.push(format!(
            "     {}",
            tier_line("mark-to-mkt", &accounting.mark_to_market)
        ));
        if accounting.recommended_input.quanta < accounting.requested_input.quanta {
            lines.push(format!(
                "     size down to {} {have}: past that, depth runs out",
                accounting.recommended_input.quanta
            ));
        }
        for residual in &accounting.residuals {
            let break_even = residual.break_even_unit_price.as_ref().map_or_else(
                || "no cost basis".to_owned(),
                |price| format!("break even at 1 : {price}", price = price.text),
            );
            lines.push(format!(
                "     stranded {} {}   {break_even}",
                residual.amount.quanta,
                residual.asset_id.as_str(),
            ));
        }
        let verdict = match accounting.assessment.actionability {
            Actionability::InstantExecutable => "executable now",
            Actionability::MakerTheoretical => "needs someone to take a listing",
            Actionability::ProbeRequired => "capture more before trusting",
            Actionability::SuspiciousOutlier => "looks wrong, not good",
        };
        lines.push(format!(
            "     {verdict}   risks {:?}",
            accounting.assessment.blocking()
        ));
    }

    if lines.is_empty() {
        lines.push("nothing to convert yet — capture a book first".to_owned());
    }
    Ok(lines)
}

/// "Is what I am watching healthy": coverage, valuations, and what to go
/// look at next.
pub fn watchlist_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
) -> Result<Vec<String>, String> {
    let market = build_market(observations, context_key)?;
    let policy = MarketPolicy::default_for(league);
    let mut lines = Vec::new();

    lines.push(format!(
        "core liquidity: {}",
        policy
            .core_liquidity
            .iter()
            .map(MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Value everything seen against the first core currency.
    let Some(anchor) = policy.core_liquidity.first() else {
        return Ok(lines);
    };
    let mut seen: Vec<MarketAssetId> = observations
        .iter()
        .flat_map(|observation| {
            [
                observation.edge.from_asset_id.clone(),
                observation.edge.to_asset_id.clone(),
            ]
        })
        .collect();
    seen.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    seen.dedup();

    for asset in seen.iter().filter(|asset| *asset != anchor) {
        let valuation = value_against_anchor(ValuationRequest {
            asset_id: asset,
            anchor_asset_id: anchor,
            mode: ValuationMode::Midpoint,
            edges: &market.selected,
            include_historical: false,
        });
        let value = match (&valuation.value, valuation.status) {
            (Some(value), ValuationStatus::TwoSided) => {
                format!("{} (both sides)", value.text)
            }
            (Some(value), _) => format!("{} (one side only)", value.text),
            (None, _) => "no price — capture this pair".to_owned(),
        };
        lines.push(format!("{:<20} {value}", asset.as_str(),));
    }

    // Typed coverage gaps for the pairs this focus group cares about, and
    // the probes that would close them.
    match focus_gaps(observations, context_key, &policy, &seen) {
        Ok(gap_lines) => lines.extend(gap_lines),
        Err(reason) => lines.push(format!("coverage unavailable: {reason}")),
    }

    for recommendation in recommend_liquidity_anchors(&market.selected, &policy) {
        lines.push(format!(
            "{:?}: {} (score {}.{}, {} pairs, {} two-way)",
            recommendation.action,
            recommendation.asset_id.as_str(),
            recommendation.score_tenths / 10,
            recommendation.score_tenths % 10,
            recommendation.pair_coverage_count,
            recommendation.bidirectional_pair_count,
        ));
    }
    Ok(lines)
}

/// "What has this pair been doing": candles, a summary, and what looks off.
pub fn history_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
) -> Result<Vec<String>, String> {
    let market = build_market(observations, context_key)?;
    let points = price_points(&market.selected, have, need);
    if points.is_empty() {
        return Ok(vec![format!("no history yet for {have} -> {need}")]);
    }

    let summary = summarize(&points, have, need);
    let mut lines = vec![format!(
        "{have} -> {need}: {} points over {} snapshots",
        summary.point_count, summary.snapshot_count
    )];
    if let Some(median) = &summary.median_rate {
        lines.push(format!(
            "median {}   low {}   high {}",
            median.text,
            summary
                .min_rate
                .as_ref()
                .map_or("—", |rate| rate.text.as_str()),
            summary
                .max_rate
                .as_ref()
                .map_or("—", |rate| rate.text.as_str()),
        ));
    }
    if let Some(spread) = summary.spread_basis_points {
        lines.push(format!("maker over taker: {spread}bp"));
    }
    if summary.historical_only {
        lines.push("nothing current — this is history, not a price".to_owned());
    }

    for candle in candles(&points, BucketSize::FiveMinutes)
        .iter()
        .rev()
        .take(8)
    {
        lines.push(format!(
            "{}  o {} h {} l {} c {}  n={}{}",
            candle.bucket_start.format("%H:%M"),
            candle.open.text,
            candle.high.text,
            candle.low.text,
            candle.close.text,
            candle.sample_count,
            if candle.maker_only {
                "  (listings)"
            } else {
                ""
            },
        ));
    }

    for anomaly in anomalies(&summary, &points) {
        lines.push(format!(
            "{:?} ({:?}){}",
            anomaly.kind,
            anomaly.severity,
            anomaly
                .basis_points
                .map_or_else(String::new, |bps| format!(" {bps}bp")),
        ));
    }
    Ok(lines)
}

/// Coverage and probe queue for the current focus group.
///
/// Coverage needs three views of the same book — what can be taken now, what
/// is only listed, and what shows up once old data is allowed — because
/// "missing" and "stale" are different problems with different fixes.
fn focus_gaps(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    policy: &MarketPolicy,
    seen: &[MarketAssetId],
) -> Result<Vec<String>, String> {
    let mut items: Vec<FocusGroupItem> = policy
        .core_liquidity
        .iter()
        .map(|asset_id| FocusGroupItem {
            asset_id: asset_id.clone(),
            role: FocusRole::Anchor,
        })
        .collect();
    for asset_id in seen {
        if !policy.is_core_liquidity(asset_id) {
            items.push(FocusGroupItem {
                asset_id: asset_id.clone(),
                role: FocusRole::Target,
            });
        }
    }
    let scope = FocusScope::try_new(&items, FocusScopePolicy::default())
        .map_err(|error| format!("{error}"))?;

    let book = build_coherent_current_book(context_key, observations, DataVisibility::default())
        .map_err(|error| format!("book: {error}"))?;
    let now = Utc::now();
    let mut selections = Vec::new();
    for strategy in [
        QuoteSelectionStrategy::Instant,
        QuoteSelectionStrategy::FastMaker,
        QuoteSelectionStrategy::Probe,
    ] {
        let policy = QuoteSelectionPolicy::personal_default(strategy)
            .map_err(|error| format!("policy: {error}"))?;
        selections.push(
            select_quote_edges(&book, &policy, now).map_err(|error| format!("select: {error}"))?,
        );
    }

    let (coverage, candidates) = derive_focus_probe_candidates(
        "live-focus",
        &scope,
        &selections[0],
        &selections[1],
        &selections[2],
    )
    .map_err(|error| format!("{error}"))?;

    let mut lines = Vec::new();
    let incomplete: Vec<_> = coverage
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .collect();
    lines.push(format!(
        "coverage: {} of {} pairs complete",
        coverage.len() - incomplete.len(),
        coverage.len()
    ));
    for entry in incomplete.iter().take(8) {
        lines.push(format!(
            "  {} -> {}  {:?}",
            entry.from_asset_id.as_str(),
            entry.to_asset_id.as_str(),
            entry.status,
        ));
    }
    for candidate in candidates.iter().take(8) {
        lines.push(format!(
            "  probe {:?}: {} -> {}  ({:?})",
            candidate.priority,
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            candidate.reason,
        ));
    }
    Ok(lines)
}
