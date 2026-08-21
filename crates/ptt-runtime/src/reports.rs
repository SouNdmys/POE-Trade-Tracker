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
use ptt_settings::{MarketTuning, UiLanguage};
use ptt_strategy::{
    BucketSize, MarkRateTable, MarketPolicy, ProfitTier, RiskThresholds, RouteAccountingRequest,
    ValuationMode, ValuationRequest, ValuationStatus, anomalies, candles, derive_route_accounting,
    price_points, recommend_liquidity_anchors, summarize, value_against_anchor,
};
use ptt_trade_domain::{MarketAssetId, MarketEdgeObservation};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ComparisonDirection, ConversionRequest, FeePolicy,
    MarketDepthIndex, SearchCancellation, find_best_conversion,
};
use ptt_workflows::{
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, RadarBudget, RadarRequest, RadarStart,
    derive_focus_probe_candidates, run_opportunity_radar,
};

/// The sizes the convert page prices, in whole orbs.
const CONVERT_SIZES: [u64; 3] = [1, 10, 100];

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
        }
        // The selected edge is one of the candidates, so the candidates alone
        // carry it — pushing it separately double-counted every selected edge
        // in valuations and price histories.
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

fn tier_line(label: &str, tier: &ProfitTier, language: UiLanguage) -> String {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let profit = match (tier.direction, &tier.delta, tier.basis_points) {
        (Some(ComparisonDirection::Improved), Some(delta), Some(bps)) => fill(
            text.better_than_direct,
            &[&delta.quanta.to_string(), &bps.to_string()],
        ),
        (Some(ComparisonDirection::Worse), Some(delta), Some(bps)) => fill(
            text.worse_than_direct,
            &[&delta.quanta.to_string(), &bps.to_string()],
        ),
        (Some(ComparisonDirection::Equal), _, _) => text.level_with_direct.to_owned(),
        // No direct route observed: showing the route is useful, calling it
        // an improvement over nothing is not.
        _ => text.no_direct_route.to_owned(),
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
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let market = build_market(observations, context_key)?;
    let mut lines = Vec::new();
    use crate::report_text::fill;
    let text = crate::report_text::report(language);

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
        for (label, tier) in [
            (text.tier_closed, &accounting.closed),
            (text.tier_theoretical, &accounting.theoretical),
            (text.tier_mark_to_market, &accounting.mark_to_market),
        ] {
            lines.push(format!("     {}", tier_line(label, tier, language)));
        }
        if accounting.recommended_input.quanta < accounting.requested_input.quanta {
            lines.push(format!(
                "     {}",
                fill(
                    text.size_down_to,
                    &[
                        &accounting.recommended_input.quanta.to_string(),
                        have.as_str(),
                    ],
                )
            ));
        }
        for residual in &accounting.residuals {
            let break_even = residual.break_even_unit_price.as_ref().map_or_else(
                || text.no_cost_basis.to_owned(),
                |price| fill(text.break_even_at, &[&price.text]),
            );
            lines.push(format!(
                "     {}",
                fill(
                    text.stranded,
                    &[
                        &residual.amount.quanta.to_string(),
                        residual.asset_id.as_str(),
                        &break_even,
                    ],
                )
            ));
        }
        let verdict =
            crate::report_text::actionability(language, accounting.assessment.actionability);
        lines.push(format!(
            "     {verdict}   {} {}",
            risks_label(language),
            crate::report_text::join(
                language,
                &accounting.assessment.blocking(),
                crate::report_text::execution_risk
            )
        ));
    }

    if lines.is_empty() {
        lines.push(text.nothing_to_convert.to_owned());
        return Ok(lines);
    }
    lines.extend(maker_section(&market, have, need, language));
    Ok(lines)
}

/// The listing-strategy section of the Convert page: the trader's three ways
/// to act on this pair as a maker — undercut the competing front, match it,
/// or list greedily — each priced against taking the instant fill now.
///
/// Returns nothing rather than erroring: the section is advisory, and a pair
/// with no maker picture still has a working convert report above it.
fn maker_section(
    market: &Market,
    have: &MarketAssetId,
    need: &MarketAssetId,
    language: UiLanguage,
) -> Vec<String> {
    use crate::report_text::fill;
    use ptt_strategy::{MakerMode, MakerRecommendation, MakerRequest, calculate_maker_strategy};

    let text = crate::report_text::report(language);
    // The middle configured size, so this section and the radar (which also
    // stakes the middle) describe the same market.
    let size = CONVERT_SIZES[CONVERT_SIZES.len() / 2];
    let Ok(amount_in) = AssetAmount::from_whole_units(have.clone(), size, &market.units) else {
        return Vec::new();
    };
    let instant = market
        .index
        .fill_pair(&amount_in, need, FeePolicy::None)
        .ok()
        .flatten()
        .filter(|fill| fill.consumed_input.quanta > 0);
    let base = MakerRequest {
        from_asset_id: have,
        to_asset_id: need,
        amount_in: &amount_in,
        competing: &market.selected,
        instant: instant.as_ref(),
        match_front: false,
        thresholds: ptt_strategy::RiskThresholds::default(),
    };
    let Ok(strategy) = calculate_maker_strategy(base) else {
        return Vec::new();
    };

    let mut lines = vec![fill(
        text.maker_header,
        &[have.as_str(), need.as_str(), &size.to_string()],
    )];
    match &strategy.instant_rate {
        Some(rate) => lines.push(format!("     {}", fill(text.maker_instant, &[&rate.text]))),
        None => lines.push(format!("     {}", text.maker_no_instant)),
    }
    if strategy.queue.is_empty() {
        lines.push(format!("     {}", text.maker_no_book));
        return lines;
    }

    let mode_line = |template: &str, recommendation: &MakerRecommendation| -> Vec<String> {
        let mut block = vec![format!(
            "     {}",
            fill(template, &[&recommendation.rate.text])
        )];
        let verdict = if recommendation.beats_instant {
            match (
                &recommendation.improvement_over_instant,
                recommendation.improvement_basis_points,
            ) {
                (Some(delta), Some(points)) => Some(fill(
                    text.maker_improvement,
                    &[
                        &delta.quanta.to_string(),
                        need.as_str(),
                        &points.to_string(),
                    ],
                )),
                _ => None,
            }
        } else {
            Some(text.maker_not_worth.to_owned())
        };
        if let Some(verdict) = verdict {
            block.push(format!("        {verdict}"));
        }
        let blocking = recommendation.assessment.blocking();
        if !blocking.is_empty() {
            block.push(format!(
                "        {} {}",
                risks_label(language),
                crate::report_text::join(language, &blocking, crate::report_text::execution_risk)
            ));
        }
        block
    };

    let undercut = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity);
    if let Some(recommendation) = undercut {
        lines.extend(mode_line(text.maker_undercut, recommendation));
    }
    // The match-front variant is the same Opportunity mode priced at the
    // front instead of below it; a second call keeps that trade-off visible
    // without a third mode existing anywhere.
    if let Ok(matched) = calculate_maker_strategy(MakerRequest {
        match_front: true,
        ..base
    }) && let Some(recommendation) = matched
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity)
    {
        lines.extend(mode_line(text.maker_match, recommendation));
    }
    if let Some(recommendation) = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Greedy)
    {
        lines.extend(mode_line(text.maker_greedy, recommendation));
    }

    if let Some(spread) = strategy.spread_basis_points {
        lines.push(format!(
            "     {}",
            fill(text.maker_spread, &[&spread.to_string()])
        ));
    }
    if let (Some(depth), Some(cap)) = (
        &strategy.visible_depth_from,
        &strategy.suggested_max_single_order,
    ) {
        lines.push(format!(
            "     {}",
            fill(
                text.maker_depth,
                &[
                    &depth.quanta.to_string(),
                    have.as_str(),
                    &cap.quanta.to_string(),
                    have.as_str(),
                ],
            )
        ));
    }
    for excluded in &strategy.excluded {
        lines.push(format!(
            "     {}",
            fill(
                text.maker_excluded,
                &[
                    &excluded.rate.text,
                    &excluded.stock.to_string(),
                    crate::report_text::maker_exclusion(language, excluded.reason),
                ],
            )
        ));
    }
    lines
}

/// The market policy the settings ask for: the configured settlement
/// currencies become the core-liquidity set, with the shipped default as the
/// fallback and a visible line — never a silent one — when the configuration
/// could not be used.
fn market_policy_from(
    tuning: &MarketTuning,
    league: &str,
    language: UiLanguage,
) -> (MarketPolicy, Option<String>) {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut policy = MarketPolicy::default_for(league);
    if tuning.settlement_assets.is_empty() {
        return (policy, None);
    }
    let parsed: Vec<MarketAssetId> = tuning
        .settlement_assets
        .iter()
        .filter_map(|id| MarketAssetId::try_new(id).ok())
        .collect();
    let dropped = tuning.settlement_assets.len() - parsed.len();
    if parsed.is_empty() || !policy.set_core_liquidity(parsed) {
        return (policy, Some(text.settlement_config_invalid.to_owned()));
    }
    let warning =
        (dropped > 0).then(|| fill(text.settlement_config_partial, &[&dropped.to_string()]));
    (policy, warning)
}

/// The focus items the reports scope over: the settlement currencies as
/// anchors, then the user's configured lists — or, when no focus list is
/// configured, every asset seen in the window, which is the pre-P7 behavior.
/// Watch-only and bridge lists apply in both modes, and watch-only wins over
/// target so a demoted asset cannot sneak back in through "seen".
fn focus_items_from(
    policy: &MarketPolicy,
    tuning: &MarketTuning,
    seen: &[MarketAssetId],
) -> Vec<FocusGroupItem> {
    let mut taken = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    let push = |asset: MarketAssetId,
                role: FocusRole,
                taken: &mut std::collections::BTreeSet<MarketAssetId>,
                items: &mut Vec<FocusGroupItem>| {
        if taken.insert(asset.clone()) {
            items.push(FocusGroupItem {
                asset_id: asset,
                role,
            });
        }
    };
    for asset in &policy.core_liquidity {
        push(asset.clone(), FocusRole::Anchor, &mut taken, &mut items);
    }
    for (list, role) in [
        (&tuning.watch_only_assets, FocusRole::WatchOnly),
        (&tuning.bridge_assets, FocusRole::Bridge),
    ] {
        for id in list {
            if let Ok(asset) = MarketAssetId::try_new(id) {
                push(asset, role, &mut taken, &mut items);
            }
        }
    }
    if tuning.focus_assets.is_empty() {
        for asset in seen {
            push(asset.clone(), FocusRole::Target, &mut taken, &mut items);
        }
    } else {
        for id in &tuning.focus_assets {
            if let Ok(asset) = MarketAssetId::try_new(id) {
                push(asset, FocusRole::Target, &mut taken, &mut items);
            }
        }
    }
    items
}

/// "Is what I am watching healthy": coverage, valuations, and what to go
/// look at next.
pub fn watchlist_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let market = build_market(observations, context_key)?;
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    let mut lines = Vec::new();
    use crate::report_text::fill;
    let text = crate::report_text::report(language);

    if let Some(warning) = policy_warning {
        lines.push(warning);
    }
    lines.push(fill(
        text.core_liquidity,
        &[&policy
            .core_liquidity
            .iter()
            .map(MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(", ")],
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
            (None, _) => text.no_price_capture.to_owned(),
        };
        lines.push(format!("{:<20} {value}", asset.as_str(),));
    }

    // Typed coverage gaps for the pairs this focus group cares about, and
    // the probes that would close them.
    match focus_gaps(
        observations,
        context_key,
        &policy,
        tuning,
        &seen,
        Some(&market),
        language,
    ) {
        Ok(gap_lines) => lines.extend(gap_lines),
        Err(reason) => lines.push(fill(text.coverage_unavailable, &[&reason])),
    }

    for recommendation in recommend_liquidity_anchors(&market.selected, &policy) {
        lines.push(format!(
            "{}: {} (score {}.{}, {} pairs, {} two-way)",
            crate::report_text::anchor_action(language, recommendation.action),
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
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    if policy.core_liquidity.is_empty() {
        return Ok(vec![text.no_core_currency.to_owned()]);
    }
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
        return Ok(vec![text.not_enough_market.to_owned()]);
    }
    let market = build_market(observations, context_key)?;

    let items = focus_items_from(&policy, tuning, &seen);
    let scope = FocusScope::try_new(&items, FocusScopePolicy::default())
        .map_err(|error| format!("scope: {error}"))?;

    // One start per settlement currency the book can actually stake — a
    // configured settlement asset the window has never seen has no unit yet
    // and is skipped rather than failing the whole scan.
    let stake = tuning.radar.stake.max(1);
    let mut starts = Vec::new();
    for asset in &policy.core_liquidity {
        if let Ok(amount_in) = AssetAmount::from_whole_units(asset.clone(), stake, &market.units) {
            starts.push(RadarStart {
                start_asset_id: asset.clone(),
                amount_in,
            });
        }
    }
    if starts.is_empty() {
        let anchor_name = policy
            .core_liquidity
            .first()
            .map_or("?", MarketAssetId::as_str);
        return Ok(vec![fill(
            text.cannot_stake,
            &[&stake.to_string(), anchor_name],
        )]);
    }
    let start_names = starts
        .iter()
        .map(|start| start.start_asset_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // Settings values pass through the engine's own validation bounds; a
    // hand-edited extreme is clamped to the largest honest value rather than
    // failing the page.
    let minimum_bps =
        u32::try_from(tuning.radar.minimum_profit_basis_points.min(1_000_000)).unwrap_or(100);
    let budget_expansions =
        u32::try_from(tuning.radar.max_total_expansions.clamp(1, 1_000_000)).unwrap_or(60_000);
    let max_results = u16::try_from(tuning.radar.max_results.clamp(1, 500)).unwrap_or(12);
    let request = RadarRequest {
        context_key: context_key.to_owned(),
        starts,
        minimum_conversion_improvement_basis_points: minimum_bps,
        minimum_triangle_profit_basis_points: minimum_bps,
        max_hops: 3,
        max_paths_per_target: 32,
        max_expansions_per_target: 4_000,
        budget: RadarBudget {
            max_total_expansions: budget_expansions,
            max_targets: 48,
        },
        max_triangle_evaluations: 4_000,
        max_results,
        // Gross by product decision: no monetary fee is modelled.
        fee_policy: FeePolicy::None,
        thresholds: ptt_strategy::RiskThresholds {
            thin_liquidity_stock: tuning.risk.thin_liquidity_stock,
        },
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

    let mut lines = Vec::new();
    if let Some(warning) = policy_warning {
        lines.push(warning);
    }
    lines.push(fill(
        text.staking,
        &[
            &stake.to_string(),
            &start_names,
            &result.diagnostics.target_count.to_string(),
        ],
    ));
    // Said before the results, not after: a truncated search that looks like a
    // complete one is how "there is nothing better" gets believed.
    if result.diagnostics.budget_exhausted || result.diagnostics.results_truncated {
        lines.push(fill(
            text.partial_scan,
            &[
                &result.diagnostics.skipped_target_count.to_string(),
                &result.diagnostics.expansions_used.to_string(),
                if result.diagnostics.results_truncated {
                    text.results_cut
                } else {
                    ""
                },
            ],
        ));
    }
    if result.items.is_empty() {
        lines.push(text.nothing_beats_holding.to_owned());
        if result.diagnostics.missing_conversion_count > 0 {
            lines.push(format!(
                "{} targets have no route yet — the Watchlist says which to flip",
                result.diagnostics.missing_conversion_count
            ));
        }
        return Ok(lines);
    }

    for item in &result.items {
        lines.extend(radar_item_lines(item, language));
    }
    Ok(lines)
}

/// One radar item, as the page prints it.
///
/// Split out so it can be tested against a hand-built item. Reaching this code
/// through the search needs a market that actually contains an arbitrage, and
/// the captured corpus does not have one — leaving the only branch a user sees
/// when the radar succeeds as the only branch never executed.
/// The word introducing a list of risk flags.
///
/// Here rather than in `report_text` because it names nothing typed -- it is
/// the one word of prose these two lines share, and the rest of the reports'
/// prose has not moved yet.
const fn risks_label(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "risks",
        UiLanguage::Chinese => "风险",
    }
}

fn radar_item_lines(item: &ptt_workflows::RadarItem, language: UiLanguage) -> Vec<String> {
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
    let category = crate::report_text::actionability(language, item.category);
    let mut lines = vec![
        format!(
            "{edge:>8}  {}  {route}",
            crate::report_text::radar_item_kind(language, item.kind)
        ),
        format!(
            "          {category}   out {} {}",
            item.amount_out.quanta,
            item.path_asset_ids
                .last()
                .map_or("?", MarketAssetId::as_str),
        ),
    ];
    if !item.blocking_risks.is_empty() {
        lines.push(format!(
            "          {} {}",
            risks_label(language),
            crate::report_text::join(
                language,
                &item.blocking_risks,
                crate::report_text::execution_risk
            )
        ));
    }
    if !item.reasons.is_empty() {
        lines.push(format!(
            "          {}",
            crate::report_text::join(language, &item.reasons, crate::report_text::radar_reason)
        ));
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
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let text = crate::report_text::report(language);
    let (policy, _policy_warning) = market_policy_from(tuning, league, language);
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
        return Ok(vec![text.no_pairs_captured.to_owned()]);
    }

    let (coverage, candidates) =
        focus_coverage(observations, context_key, &policy, tuning, &seen, None)?;
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
        lines.push(text.nothing_to_probe.to_owned());
        return Ok(lines);
    }
    for candidate in candidates.iter().take(6) {
        lines.push(format!(
            "{}  flip {} -> {}   ({})",
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
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
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let market = build_market(observations, context_key)?;
    let points = price_points(&market.selected, have, need);
    if points.is_empty() {
        return Ok(vec![fill(
            text.no_history_yet,
            &[have.as_str(), need.as_str()],
        )]);
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
        lines.push(fill(text.maker_over_taker, &[&spread.to_string()]));
    }
    if summary.historical_only {
        lines.push(text.nothing_current.to_owned());
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
            "{} ({}){}",
            crate::report_text::price_anomaly_kind(language, anomaly.kind),
            crate::report_text::anomaly_severity(language, anomaly.severity),
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
    tuning: &MarketTuning,
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
    let items = focus_items_from(policy, tuning, seen);
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
    tuning: &MarketTuning,
    seen: &[MarketAssetId],
    prebuilt: Option<&Market>,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let (coverage, candidates) =
        focus_coverage(observations, context_key, policy, tuning, seen, prebuilt)?;
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
            "  {} -> {}  {}",
            entry.from_asset_id.as_str(),
            entry.to_asset_id.as_str(),
            crate::report_text::focus_coverage_status(language, entry.status),
        ));
    }
    for candidate in candidates.iter().take(8) {
        lines.push(format!(
            "  probe {}: {} -> {}  ({})",
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
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
            category: ptt_strategy::Actionability::InstantExecutable,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 4000, &units)
                .expect("out"),
            value_basis_points: Some(30_012),
            reasons: vec![ptt_workflows::RadarReason::BetterThanDirect],
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let lines = radar_item_lines(&item, UiLanguage::English);
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
            joined.contains("better than direct"),
            "the reason is missing: {joined}"
        );

        // The same row for a Chinese reader. Nothing about a radar row is
        // language-specific except its words, so a row that renders in one
        // language and not the other means a value reached the screen as a
        // bare Rust identifier -- which is what this whole path replaced.
        let chinese = radar_item_lines(&item, UiLanguage::Chinese).join(
            "
",
        );
        assert!(
            chinese.contains("divine-orb -> chaos-orb -> exalted-orb"),
            "asset ids are the game's, not the interface's: {chinese}"
        );
        assert!(chinese.contains("300.12%"), "{chinese}");
        assert!(
            chinese.contains("现在就能成交"),
            "the execution category is still English: {chinese}"
        );
        assert!(
            chinese.contains("优于直兑"),
            "the reason is still English: {chinese}"
        );
        assert!(
            !chinese.contains("better than direct"),
            "both languages came out at once: {chinese}"
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
            category: ptt_strategy::Actionability::ProbeRequired,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 11, &units)
                .expect("out"),
            value_basis_points: None,
            reasons: Vec::new(),
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let joined = radar_item_lines(&item, UiLanguage::English).join(
            "
",
        );
        assert!(joined.contains("unpriced"), "{joined}");
        assert!(joined.contains("capture more before trusting"), "{joined}");
        let chinese = radar_item_lines(&item, UiLanguage::Chinese).join(
            "
",
        );
        assert!(chinese.contains("数据不够，先多抓几次"), "{chinese}");
    }

    /// A book with nothing in it must say so rather than error.
    #[test]
    fn an_empty_book_says_so() {
        let lines = opportunities_report(
            &[],
            CONTEXT,
            "test-league",
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        assert!(!lines.is_empty(), "the radar must always say something");
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "settlement-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// A fresh taker row between any two assets.
    fn taker(from: &str, to: &str, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: format!("{from}->{to}"),
                snapshot_id: format!("snapshot-{from}-{to}"),
                quote_id: format!("quote-{from}-{to}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: QuoteSide::Available,
                execution_type: ExecutionType::Taker,
                role: QuoteEdgeRole::AvailableTaker,
                stock,
                original_need_asset_id: asset(to),
                original_have_asset_id: asset(from),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    /// One physical arbitrage loop, visible from both settlement starts.
    /// divine -> chaos x100, chaos -> exalted x2, exalted -> divine /100:
    /// the cycle multiplies holdings by 2 from either entry, exactly, so
    /// both scans find it — and the report must show it once, not twice.
    #[test]
    fn the_same_cycle_from_two_settlement_starts_appears_once() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 10_000_000),
            taker("chaos-orb", "exalted-orb", (2, 1), 10_000_000),
            taker("exalted-orb", "divine-orb", (1, 100), 10_000_000),
        ];
        let tuning = MarketTuning {
            radar: ptt_settings::RadarTuning {
                stake: 1_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = opportunities_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );

        assert!(
            joined.contains("staking 1000 divine-orb, chaos-orb"),
            "both settlement currencies must be scanned from: {joined}"
        );
        // The kind column, not the word: reason lines also say "triangle".
        let triangle_lines = lines
            .iter()
            .filter(|line| line.contains("  triangle  "))
            .count();
        assert_eq!(
            triangle_lines, 1,
            "one physical loop must appear exactly once: {joined}"
        );
        assert!(
            joined.contains("100.00%"),
            "the loop doubles holdings — 10000bp: {joined}"
        );
    }

    /// The risk ladder the radar shows is the strategy ladder, thresholds
    /// included: raising the thin-liquidity bar above every stock in the
    /// book must flag every leg — with the typed reason, not a category
    /// downgrade alone.
    #[test]
    fn the_thin_liquidity_threshold_comes_from_settings() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 10_000_000),
            taker("chaos-orb", "exalted-orb", (2, 1), 10_000_000),
            taker("exalted-orb", "divine-orb", (1, 100), 10_000_000),
        ];
        let tuning = MarketTuning {
            radar: ptt_settings::RadarTuning {
                stake: 1_000,
                ..Default::default()
            },
            risk: ptt_settings::RiskTuning {
                thin_liquidity_stock: 20_000_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = opportunities_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("thin liquidity"),
            "every stock sits below the configured bar: {joined}"
        );
        assert!(
            joined.contains("capture more before trusting"),
            "a blocked item carries the ladder's verdict: {joined}"
        );
    }

    /// The configured settlement list drives the core-liquidity set; a
    /// wholly invalid configuration falls back to the shipped default with a
    /// visible line, never silently.
    #[test]
    fn the_settlement_setting_drives_the_core_list() {
        let observations = vec![taker("divine-orb", "chaos-orb", (100, 1), 1_000)];
        let tuning = MarketTuning {
            settlement_assets: vec!["chaos-orb".to_owned()],
            ..Default::default()
        };
        let lines = watchlist_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("core liquidity: chaos-orb")),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("divine-orb, chaos-orb")),
            "the default list must be replaced, not appended to: {lines:?}"
        );

        let broken = MarketTuning {
            settlement_assets: vec!["NOT AN ID".to_owned()],
            ..Default::default()
        };
        let lines = watchlist_report(
            &observations,
            CONTEXT,
            "test-league",
            &broken,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("settlement currencies in settings are invalid")),
            "an unusable configuration must say so: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("core liquidity: divine-orb")),
            "and the shipped default must carry the page: {lines:?}"
        );
    }
}

#[cfg(test)]
mod convert_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "convert-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn observation(
        edge_id: &str,
        side: QuoteSide,
        role: QuoteEdgeRole,
        execution: ExecutionType,
        row: u8,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.to_owned(),
                snapshot_id: "snapshot-1".to_owned(),
                quote_id: format!("quote-{edge_id}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset("divine-orb"),
                to_asset_id: asset("chaos-orb"),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset("chaos-orb"),
                original_have_asset_id: asset("divine-orb"),
                original_row_index: row,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    fn taker(edge_id: &str, row: u8, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        observation(
            edge_id,
            QuoteSide::Available,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::Taker,
            row,
            rate,
            stock,
        )
    }

    fn competing(edge_id: &str, row: u8, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        observation(
            edge_id,
            QuoteSide::Competing,
            QuoteEdgeRole::CompetingMakerReference,
            ExecutionType::MakerReference,
            row,
            rate,
            stock,
        )
    }

    /// The Convert page's maker section, end to end from raw observations:
    /// the bait listing on the competing side is flagged by the book's
    /// median band, excluded from the queue math, and rendered with its
    /// reason — while the undercut prices against the honest front.
    #[test]
    fn the_maker_section_excludes_the_bait_and_prices_the_honest_front() {
        let observations = vec![
            taker("take-700", 0, (700, 1), 100_000),
            competing("bait", 0, (100, 1), 500),
            competing("front", 1, (784, 1), 40),
            competing("second", 2, (785, 1), 60),
            competing("back", 3, (795, 1), 80),
        ];
        let lines = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join("\n");

        assert!(
            joined.contains("listing strategy divine-orb -> chaos-orb"),
            "the maker section is missing: {joined}"
        );
        assert!(
            joined.contains("take now at 700:1"),
            "the instant reference is missing: {joined}"
        );
        assert!(
            joined.contains("undercut, list at 783:1"),
            "the undercut must price one tick below the honest front: {joined}"
        );
        assert!(
            joined.contains("excluded listing at 100:1 (stock 500): price outlier"),
            "the bait must be visible with its reason: {joined}"
        );
        assert!(
            !joined.contains("list at 99:1"),
            "nothing may price against the bait: {joined}"
        );

        // The same section for a Chinese reader, and the exclusion reason
        // with it.
        let chinese = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join("\n");
        assert!(chinese.contains("挂单策略"), "{chinese}");
        assert!(chinese.contains("价格离群"), "{chinese}");
    }
}
