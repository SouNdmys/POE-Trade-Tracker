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
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, RadarBudget, RadarCategory,
    RadarRequest, derive_focus_probe_candidates, run_opportunity_radar,
};

/// The sizes the convert page prices, in whole orbs.
const CONVERT_SIZES: [u64; 3] = [1, 10, 100];

/// What the radar assumes you are willing to put in, in whole anchor units.
///
/// A radar has to stake something, because depth makes profit size-dependent:
/// the best route for one orb is often not the best route for a hundred. This
/// is the middle of the sizes the Convert page prices, so the two pages agree
/// about the market they are describing.
const RADAR_STAKE: u64 = 10;

/// Everything the engine needs, assembled once from stored observations.
struct Market {
    index: MarketDepthIndex,
    units: AssetUnitCatalog,
    selected: Vec<EvaluatedQuoteEdge>,
    mark_rates: MarkRateTable,
    /// Kept so coverage can reuse them. Building the book clones every
    /// observation in the window, so doing it twice for one page doubled the
    /// most expensive step on the UI thread.
    book: ptt_market_book::CoherentCurrentBook,
    instant_selection: ptt_market_book::QuoteSelectionResult,
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
        book,
        instant_selection: selection,
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
    match focus_gaps(observations, context_key, &policy, &seen, Some(&market)) {
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

/// "Where is the money right now": the unified radar.
///
/// This is the page the whole loop points at. Everything else answers a
/// question the user had to think of first — this one ranks what the book
/// already knows, so the answer arrives before the question.
///
/// Anchored on the league's first core currency: a radar has to start
/// somewhere, and the currency everything else is quoted in is the one holding
/// that is not itself a position. Items keep their execution category, because
/// "executable now" and "someone would have to take this listing" are
/// different products and collapsing them is how a theoretical number gets
/// traded.
pub fn opportunities_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
) -> Result<Vec<String>, String> {
    let policy = MarketPolicy::default_for(league);
    let Some(anchor) = policy.core_liquidity.first().cloned() else {
        return Ok(vec![
            "no core currency configured for this league".to_owned(),
        ]);
    };
    // Counted before the book is built: an empty window has no assets, and
    // `build_market` cannot make a unit catalogue out of none.
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
    if seen.len() < 2 {
        return Ok(vec![
            "not enough of the market captured yet — flip a few pairs first".to_owned(),
        ]);
    }
    let market = build_market(observations, context_key)?;

    let mut items: Vec<FocusGroupItem> = vec![FocusGroupItem {
        asset_id: anchor.clone(),
        role: FocusRole::Anchor,
    }];
    for asset_id in seen.iter().filter(|asset| **asset != anchor) {
        items.push(FocusGroupItem {
            asset_id: asset_id.clone(),
            // Core liquidity is money, not a position to end up holding, so it
            // routes through rather than being a destination — the same split
            // the Watchlist uses.
            role: if policy.is_core_liquidity(asset_id) {
                FocusRole::Anchor
            } else {
                FocusRole::Target
            },
        });
    }
    let scope = FocusScope::try_new(&items, FocusScopePolicy::default())
        .map_err(|error| format!("scope: {error}"))?;

    let Ok(amount_in) = AssetAmount::from_whole_units(anchor.clone(), RADAR_STAKE, &market.units)
    else {
        return Ok(vec![format!("cannot stake {RADAR_STAKE} {anchor}")]);
    };
    let request = RadarRequest {
        context_key: context_key.to_owned(),
        start_asset_id: anchor.clone(),
        amount_in,
        minimum_conversion_improvement_basis_points: 100,
        minimum_triangle_profit_basis_points: 100,
        max_hops: 3,
        max_paths_per_target: 32,
        max_expansions_per_target: 4_000,
        budget: RadarBudget {
            max_total_expansions: 60_000,
            max_targets: 48,
        },
        max_triangle_evaluations: 4_000,
        max_results: 12,
        // Gross by product decision: no monetary fee is modelled.
        fee_policy: FeePolicy::None,
    };
    let result = run_opportunity_radar(
        &market.instant_selection,
        &market.units,
        &scope,
        &request,
        &SearchCancellation::default(),
        |_| {},
    )
    .map_err(|error| format!("radar: {error:?}"))?;

    let mut lines = vec![format!(
        "staking {RADAR_STAKE} {anchor} across {} targets",
        result.diagnostics.target_count
    )];
    // Said before the results, not after: a truncated search that looks like a
    // complete one is how "there is nothing better" gets believed.
    if result.diagnostics.budget_exhausted || result.diagnostics.results_truncated {
        lines.push(format!(
            "partial scan — {} targets skipped, {} expansions used{}",
            result.diagnostics.skipped_target_count,
            result.diagnostics.expansions_used,
            if result.diagnostics.results_truncated {
                ", results cut to the top few"
            } else {
                ""
            }
        ));
    }
    if result.items.is_empty() {
        lines.push("nothing beats holding right now".to_owned());
        if result.diagnostics.missing_conversion_count > 0 {
            lines.push(format!(
                "{} targets have no route yet — the Watchlist says which to flip",
                result.diagnostics.missing_conversion_count
            ));
        }
        return Ok(lines);
    }

    for item in &result.items {
        lines.extend(radar_item_lines(item));
    }
    Ok(lines)
}

/// One radar item, as the page prints it.
///
/// Split out so it can be tested against a hand-built item. Reaching this code
/// through the search needs a market that actually contains an arbitrage, and
/// the captured corpus does not have one — leaving the only branch a user sees
/// when the radar succeeds as the only branch never executed.
fn radar_item_lines(item: &ptt_workflows::RadarItem) -> Vec<String> {
    let route = item
        .path_asset_ids
        .iter()
        .map(MarketAssetId::as_str)
        .collect::<Vec<_>>()
        .join(" -> ");
    let edge = item.value_basis_points.map_or_else(
        || "unpriced".to_owned(),
        |points| format!("{}.{:02}%", points / 100, (points % 100).abs()),
    );
    let category = match item.category {
        RadarCategory::Executable => "executable now",
        RadarCategory::Theoretical => "needs a taker",
        RadarCategory::ProbeRequired => "capture more first",
    };
    let mut lines = vec![
        format!("{edge:>8}  {:?}  {route}", item.kind),
        format!(
            "          {category}   out {} {}",
            item.amount_out.quanta,
            item.path_asset_ids
                .last()
                .map_or("?", MarketAssetId::as_str),
        ),
    ];
    if !item.risk_flags.is_empty() {
        lines.push(format!("          risks {:?}", item.risk_flags));
    }
    if !item.reasons.is_empty() {
        lines.push(format!("          {:?}", item.reasons));
    }
    lines
}

/// "What should I go look at next": the probe queue on its own.
///
/// This is the loop the product is built around — a gap in the book becomes a
/// suggestion, the suggestion sends the user to that pair in game, the watcher
/// ingests it, and the suggestion disappears. Monitor shows it permanently so
/// the next move is always on screen.
pub fn probe_queue(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
) -> Result<Vec<String>, String> {
    let policy = MarketPolicy::default_for(league);
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
    if seen.is_empty() {
        return Ok(vec!["no pairs captured yet".to_owned()]);
    }

    let (coverage, candidates) = focus_coverage(observations, context_key, &policy, &seen, None)?;
    let missing = coverage
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .count();
    let mut lines = vec![format!(
        "{} of {} pairs complete",
        coverage.len() - missing,
        coverage.len()
    )];
    if candidates.is_empty() {
        lines.push("nothing to probe — the book is current".to_owned());
        return Ok(lines);
    }
    for candidate in candidates.iter().take(6) {
        lines.push(format!(
            "{:?}  flip {} -> {}   ({:?})",
            candidate.priority,
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            candidate.reason,
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
fn focus_coverage(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    policy: &MarketPolicy,
    seen: &[MarketAssetId],
    // The caller's already-built Instant selection, when it has one. Coverage
    // needs three views of the book and one of them is the Instant view the
    // watchlist just computed; rebuilding it doubled the book construction on
    // the UI thread.
    prebuilt: Option<&Market>,
) -> Result<
    (
        Vec<ptt_workflows::FocusCoverage>,
        Vec<ptt_workflows::ProbeCandidate>,
    ),
    String,
> {
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

    // Coverage needs three views of one book. The Instant view is the one the
    // caller already built, so it is borrowed rather than recomputed.
    let owned;
    let market = match prebuilt {
        Some(market) => market,
        None => {
            owned = build_market(observations, context_key)?;
            &owned
        }
    };
    let now = Utc::now();
    let mut selections = Vec::new();
    for strategy in [
        QuoteSelectionStrategy::FastMaker,
        QuoteSelectionStrategy::Probe,
    ] {
        let policy = QuoteSelectionPolicy::personal_default(strategy)
            .map_err(|error| format!("policy: {error}"))?;
        selections.push(
            select_quote_edges(&market.book, &policy, now)
                .map_err(|error| format!("select: {error}"))?,
        );
    }

    derive_focus_probe_candidates(
        "live-focus",
        &scope,
        &market.instant_selection,
        &selections[0],
        &selections[1],
    )
    .map_err(|error| format!("{error}"))
}

/// The same coverage, rendered for the watchlist page.
fn focus_gaps(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    policy: &MarketPolicy,
    seen: &[MarketAssetId],
    prebuilt: Option<&Market>,
) -> Result<Vec<String>, String> {
    let (coverage, candidates) = focus_coverage(observations, context_key, policy, seen, prebuilt)?;
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

#[cfg(test)]
mod radar_tests {
    use super::*;

    const CONTEXT: &str = "radar-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// The line a user reads when the radar succeeds must actually render.
    ///
    /// Built by hand rather than searched for: the captured corpus contains no
    /// arbitrage, so driving this branch through the search would need a
    /// synthetic market whose depth index cooperates, and that tests the
    /// engine rather than these four lines of formatting.
    #[test]
    fn a_found_opportunity_renders() {
        let path: Vec<MarketAssetId> = ["divine-orb", "chaos-orb", "exalted-orb"]
            .into_iter()
            .map(asset)
            .collect();
        let units = AssetUnitCatalog::try_new(
            path.iter()
                .map(|id| (id.clone(), AssetUnit::whole()))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("units");
        let item = ptt_workflows::RadarItem {
            item_id: "test-item".to_owned(),
            kind: ptt_workflows::RadarItemKind::BestConversion,
            category: RadarCategory::Executable,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 4000, &units)
                .expect("out"),
            value_basis_points: Some(30_012),
            reasons: vec![ptt_workflows::RadarReason::BetterThanDirect],
            risk_flags: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let lines = radar_item_lines(&item);
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("divine-orb -> chaos-orb -> exalted-orb"),
            "the route is not shown: {joined}"
        );
        assert!(
            joined.contains("300.12%"),
            "basis points are not rendered as a percentage: {joined}"
        );
        assert!(
            joined.contains("executable now"),
            "the execution category is missing: {joined}"
        );
        assert!(
            joined.contains("out 4000 exalted-orb"),
            "the payout is missing: {joined}"
        );
        assert!(
            joined.contains("BetterThanDirect"),
            "the reason is missing: {joined}"
        );
    }

    /// An unpriced item must not print a bogus percentage.
    #[test]
    fn an_unpriced_item_says_unpriced() {
        let path: Vec<MarketAssetId> = ["divine-orb", "exalted-orb"]
            .into_iter()
            .map(asset)
            .collect();
        let units = AssetUnitCatalog::try_new(
            path.iter()
                .map(|id| (id.clone(), AssetUnit::whole()))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("units");
        let item = ptt_workflows::RadarItem {
            item_id: "unpriced".to_owned(),
            kind: ptt_workflows::RadarItemKind::Triangle,
            category: RadarCategory::ProbeRequired,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 11, &units)
                .expect("out"),
            value_basis_points: None,
            reasons: Vec::new(),
            risk_flags: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let joined = radar_item_lines(&item).join(
            "
",
        );
        assert!(joined.contains("unpriced"), "{joined}");
        assert!(joined.contains("capture more first"), "{joined}");
    }

    /// A book with nothing in it must say so rather than error.
    #[test]
    fn an_empty_book_says_so() {
        let lines = opportunities_report(&[], CONTEXT, "test-league").expect("report");
        assert!(!lines.is_empty(), "the radar must always say something");
    }
}
