//! Read-only reports built from the store, one per UI page.
//!
//! These live here rather than in the app so the numbers can be tested
//! without a window, and so the visual layer stays free to change without
//! touching anything that computes. Each returns display lines: the app
//! renders them and nothing else.

use std::collections::BTreeMap;

use chrono::Utc;
use ptt_market_book::{
    DataVisibility, EvaluatedQuoteEdge, FreshnessPolicy, FreshnessStatus, QuoteSelectionPolicy,
    QuoteSelectionStrategy, build_coherent_current_book, select_quote_edges,
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

// ---------------------------------------------------------------------------
// Page models
//
// Every page is computed once into one of these and rendered twice: as the
// text lines below, and as the real interface in `ptt-app`. The split exists
// because the lines were the only form the answers had, so anything the
// renderer wanted that a line did not already say -- a per-leg breakdown, the
// seconds behind a freshness light, the rows a queue excluded -- had to be
// recomputed or parsed back out of prose.
//
// The models therefore hold the typed values, and the flattening functions
// hold every formatting decision. A page that needs a new number adds it here;
// it never re-derives one from a rendered string.
// ---------------------------------------------------------------------------

/// Prose the pages print before anything else: a configuration that could not
/// be used degrades loudly, never silently. Rendered at build time because
/// these are sentences, not values.
type Notes = Vec<String>;

/// "I hold X and want Y", answered at one or more sizes.
#[derive(Clone, Debug)]
pub struct ConvertModel {
    pub notes: Notes,
    pub have: MarketAssetId,
    pub need: MarketAssetId,
    /// One entry per size that could be priced at all. A size the unit
    /// catalogue cannot express is absent rather than present-and-empty.
    pub sizes: Vec<SizeRoute>,
    /// The listing advice for the middle size, when there was a book to give
    /// it. Advisory: its absence never invalidates the routes above.
    pub maker: Option<MakerModel>,
    /// Market-pulse context for the asset being acquired — the greedy card's
    /// evidence line (scarce? drifting up?). Absent without season history.
    pub need_structural: Option<StructuralNote>,
}

#[derive(Clone, Debug)]
pub struct SizeRoute {
    pub size: u64,
    /// `None` when the search found no path at this size.
    pub accounting: Option<Box<ptt_strategy::RouteAccounting>>,
}

/// The trader's three ways to act on a pair as a maker, priced against taking
/// the instant fill now.
#[derive(Clone, Debug)]
pub struct MakerModel {
    pub size: u64,
    pub strategy: ptt_strategy::MakerStrategy,
    /// The same Opportunity mode priced at the front instead of below it — a
    /// second evaluation, so the trade-off stays visible without a third mode
    /// existing anywhere.
    pub match_front: Option<ptt_strategy::MakerRecommendation>,
}

/// "Is what I am watching healthy": coverage, valuations, and what to promote.
#[derive(Clone, Debug)]
pub struct WatchlistModel {
    pub notes: Notes,
    pub core_liquidity: Vec<MarketAssetId>,
    pub valuations: Vec<AssetValuation>,
    pub coverage: CoverageOutcome,
    pub suggestions: Vec<FocusSuggestion>,
    pub anchors: Vec<ptt_strategy::AnchorRecommendation>,
}

#[derive(Clone, Debug)]
pub struct AssetValuation {
    pub asset_id: MarketAssetId,
    pub valuation: ptt_strategy::Valuation,
}

/// A currency with real buy pressure that is not in the focus list.
///
/// The evidence is the listed quantities on the two book sides, valued in
/// the primary settlement currency — capture frequency proves only that the
/// user looked, not that anyone wants the thing (user-adjudicated 2026-08-22).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusSuggestion {
    pub asset_id: MarketAssetId,
    /// Anchor-valued demand-side listings in the window: the buy pressure.
    pub demand_anchor: u64,
    /// Anchor-valued supply-side listings, for the same window.
    pub supply_anchor: u64,
}

/// Coverage is a third view of the book and can fail on its own; the page
/// still has valuations to show when it does.
#[derive(Clone, Debug)]
pub enum CoverageOutcome {
    /// No anchor currency, so there was nothing to measure coverage against.
    NotComputed,
    Failed(String),
    Ready(CoverageModel),
}

#[derive(Clone, Debug)]
pub struct CoverageModel {
    /// Whether the scope was answerable at all.
    ///
    /// Carried rather than inferred from an entry count: a list that names
    /// only currencies the settlement set already covers produces a scope with
    /// no targets, and the two pairs left over — the settlement currencies
    /// against each other — look exactly like a market nobody has captured.
    pub status: ptt_workflows::FocusScopeStatus,
    pub entries: Vec<ptt_workflows::FocusCoverage>,
    pub candidates: Vec<ptt_workflows::ProbeCandidate>,
}

/// "Where is the money right now": the unified radar.
#[derive(Clone, Debug)]
pub struct OpportunitiesModel {
    pub notes: Notes,
    pub scan: RadarScan,
}

#[derive(Clone, Debug)]
pub enum RadarScan {
    /// The radar could not run, and why. Distinct from running and finding
    /// nothing: one is a gap in the data, the other is an answer.
    Unavailable(RadarUnavailable),
    Ran(Box<RadarScanResult>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadarUnavailable {
    /// No settlement currency, so there is nowhere to start or return to.
    NoCoreCurrency,
    NotEnoughMarket,
    /// Every settlement currency is missing a unit, so no stake can be built.
    CannotStake {
        stake: u64,
        anchor: Option<MarketAssetId>,
    },
}

#[derive(Clone, Debug)]
pub struct RadarScanResult {
    pub stake: u64,
    pub starts: Vec<MarketAssetId>,
    pub items: Vec<OpportunityRow>,
    pub probe_candidates: Vec<ptt_workflows::ProbeCandidate>,
    pub diagnostics: ptt_workflows::RadarDiagnostics,
}

#[derive(Clone, Debug)]
pub struct OpportunityRow {
    pub item: ptt_workflows::RadarItem,
    /// Read from the oldest leg: a route is only as current as the capture it
    /// leans on hardest.
    pub light: Option<FreshnessStatus>,
    /// Season-scale liquidity context for each leg asset the market pulse
    /// knows. Advisory only: it never reorders items or changes a category —
    /// the current snapshot depth is real even when a leg's market is
    /// structurally thin (user ruling: sort stays liquidity > profit > hops).
    pub structural: Vec<StructuralNote>,
}

/// One asset's market-pulse context, attached to a route or a pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralNote {
    pub asset_id: MarketAssetId,
    pub class: ptt_strategy::LiquidityClass,
    pub verdict: Option<ptt_strategy::TrendVerdict>,
    /// Scarce and not depreciating relative to the market: the greedy-mode
    /// precondition holds for this asset.
    pub greedy_candidate: bool,
}

impl StructuralNote {
    /// A structurally thin leg: nothing wrong with the printed numbers, but
    /// the asset's market is oversupplied or quiet season-wide, so refills
    /// behind the visible book are less likely.
    #[must_use]
    pub const fn structurally_illiquid(&self) -> bool {
        matches!(
            self.class,
            ptt_strategy::LiquidityClass::Oversupplied | ptt_strategy::LiquidityClass::Quiet
        )
    }
}

/// The pulse context for every path asset the pulse knows, in path order,
/// deduplicated. Annotation only — the caller must not resort on it.
fn structural_notes_for(
    path: &[MarketAssetId],
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Vec<StructuralNote> {
    let Some(pulse) = pulse else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut notes = Vec::new();
    for asset_id in path {
        if !seen.insert(asset_id.clone()) {
            continue;
        }
        if let Some(asset) = pulse
            .assets
            .iter()
            .find(|asset| asset.asset_id == *asset_id)
        {
            notes.push(StructuralNote {
                asset_id: asset.asset_id.clone(),
                class: asset.class,
                verdict: asset.verdict,
                greedy_candidate: asset.greedy_candidate,
            });
        }
    }
    notes
}

/// "What should I go look at next": the probe queue on its own.
#[derive(Clone, Debug)]
pub struct ProbeQueueModel {
    /// Nothing seen in the window at all, which is a different message from
    /// having nothing left to probe.
    pub nothing_captured: bool,
    pub complete_pairs: usize,
    pub total_pairs: usize,
    pub candidates: Vec<ptt_workflows::ProbeCandidate>,
}

/// "What has this pair been doing": candles, a summary, and what looks off.
#[derive(Clone, Debug)]
pub struct HistoryModel {
    pub notes: Notes,
    pub have: MarketAssetId,
    pub need: MarketAssetId,
    /// `None` when this direction has no priced points yet.
    pub summary: Option<ptt_strategy::PriceSummary>,
    /// Reads the newest capture of this direction: older points are history,
    /// but the light describes what acting now would lean on.
    pub light: Option<FreshnessStatus>,
    /// Every candle, oldest first. The text below shows the last few; a chart
    /// wants all of them.
    pub candles: Vec<ptt_strategy::PriceCandle>,
    pub anomalies: Vec<ptt_strategy::PriceAnomaly>,
}

/// "What is the market doing": season-scale value, supply/demand pressure,
/// and anchor health, built from persisted day rollups plus a live fold of
/// today. The only page whose data reaches past the report window.
#[derive(Clone, Debug)]
pub struct AnalyticsModel {
    pub notes: Notes,
    /// The active season banner, when one is configured.
    pub season: Option<SeasonBanner>,
    /// Distinct UTC days feeding the pulse (rollups + today).
    pub data_days: u32,
    pub pulse: ptt_strategy::MarketPulse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeasonBanner {
    pub label: String,
    pub started_day: String,
}

/// Everything the engine needs, assembled once from stored observations.
struct Market {
    index: MarketDepthIndex,
    /// Lines the pages print before anything else — a tuned policy that
    /// could not be used degrades loudly, never silently.
    notes: Vec<String>,
    units: AssetUnitCatalog,
    selected: Vec<EvaluatedQuoteEdge>,
    mark_rates: MarkRateTable,
    /// Kept so coverage can reuse them. Building the book clones every
    /// observation in the window, so doing it twice for one page doubled the
    /// most expensive step on the UI thread.
    book: ptt_market_book::CoherentCurrentBook,
    instant_selection: ptt_market_book::QuoteSelectionResult,
}

/// The tuned selection policy for one strategy, or the shipped default and
/// a visible note when the tuning does not validate.
fn selection_policy_from(
    tuning: &MarketTuning,
    strategy: QuoteSelectionStrategy,
    language: UiLanguage,
) -> Result<(QuoteSelectionPolicy, Option<String>), String> {
    let text = crate::report_text::report(language);
    let tuned = FreshnessPolicy::try_new(
        tuning.freshness.fresh_seconds,
        tuning.freshness.usable_seconds,
        tuning.freshness.stale_seconds,
    )
    .ok()
    .and_then(|freshness| {
        QuoteSelectionPolicy::personal_tuned(
            strategy,
            freshness,
            tuning.freshness.capture_skew_seconds,
            tuning.risk.top_book_outlier_factor,
        )
        .ok()
    });
    if let Some(policy) = tuned {
        return Ok((policy, None));
    }
    let policy = QuoteSelectionPolicy::personal_default(strategy)
        .map_err(|error| format!("policy: {error}"))?;
    Ok((policy, Some(text.freshness_config_invalid.to_owned())))
}

fn build_market(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Market, String> {
    let book = build_coherent_current_book(context_key, observations, DataVisibility::default())
        .map_err(|error| format!("book: {error}"))?;
    let (policy, policy_note) =
        selection_policy_from(tuning, QuoteSelectionStrategy::Instant, language)?;
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
        notes: policy_note.into_iter().collect(),
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
    holdings: Option<u64>,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = convert_model(
        observations,
        context_key,
        have,
        need,
        holdings,
        tuning,
        language,
        None,
    )?;
    Ok(render_convert(&model, language))
}

/// The sizes the page prices.
///
/// A stated holding prices exactly that size — "I have 100 divine" is a
/// question about 100, not about a ladder. Otherwise the configured ladder,
/// with the shipped one behind an empty or zeroed setting.
fn convert_sizes(holdings: Option<u64>, tuning: &MarketTuning) -> Vec<u64> {
    match holdings {
        Some(count) => vec![count.max(1)],
        None => {
            let configured: Vec<u64> = tuning
                .convert
                .sizes
                .iter()
                .copied()
                .filter(|size| *size > 0)
                .collect();
            if configured.is_empty() {
                CONVERT_SIZES.to_vec()
            } else {
                configured
            }
        }
    }
}

/// Everything the Convert page knows, before anything decides how to draw it.
pub fn convert_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    holdings: Option<u64>,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<ConvertModel, String> {
    // "X into X" is not a route question. The engine says so with
    // `InvalidAnalysisRequest`, which is correct of it — that is a caller
    // that should have known better — but propagating it turns a reader
    // setting both pickers to the same currency into a page-wide fault with
    // no way back out. The one useful sentence, said here, instead.
    if have == need {
        return Ok(ConvertModel {
            notes: vec![
                crate::report_text::report(language)
                    .same_currency
                    .to_owned(),
            ],
            have: have.clone(),
            need: need.clone(),
            sizes: Vec::new(),
            maker: None,
            need_structural: None,
        });
    }
    let market = build_market(observations, context_key, tuning, language)?;
    let sizes = convert_sizes(holdings, tuning);
    let max_hops = u8::try_from(tuning.convert.max_hops.clamp(1, 4)).unwrap_or(3);

    let mut routes: Vec<SizeRoute> = Vec::new();
    for size in sizes.iter().copied() {
        let Ok(amount_in) = AssetAmount::from_whole_units(have.clone(), size, &market.units) else {
            continue;
        };
        let conversion = find_best_conversion(
            &market.index,
            &ConversionRequest {
                from_asset_id: have.clone(),
                to_asset_id: need.clone(),
                amount_in,
                max_hops,
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
            routes.push(SizeRoute {
                size,
                accounting: None,
            });
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
        routes.push(SizeRoute {
            size,
            accounting: Some(Box::new(accounting)),
        });
    }

    // Listing advice belongs under a page that said something. When there is
    // neither a note nor a priced size, the page reads "nothing to convert",
    // and maker advice under that heading would be advice about a pair the
    // page just said it could not price.
    let maker = (!market.notes.is_empty() || !routes.is_empty())
        .then(|| maker_model(&market, have, need, sizes[sizes.len() / 2]))
        .flatten();

    Ok(ConvertModel {
        notes: market.notes,
        have: have.clone(),
        need: need.clone(),
        sizes: routes,
        maker,
        need_structural: structural_notes_for(std::slice::from_ref(need), pulse)
            .into_iter()
            .next(),
    })
}

/// The Convert page as display lines.
fn render_convert(model: &ConvertModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();
    let (have, need) = (&model.have, &model.need);

    for route in &model.sizes {
        let size = route.size;
        let Some(accounting) = &route.accounting else {
            lines.push(format!(
                "{size:>4} {}",
                fill(text.no_route_for_pair, &[have.as_str(), need.as_str()])
            ));
            continue;
        };
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
        return lines;
    }
    if let Some(maker) = &model.maker {
        lines.extend(render_maker(maker, have, need, language));
    }
    lines
}

/// The listing-strategy section of the Convert page: the trader's three ways
/// to act on this pair as a maker — undercut the competing front, match it,
/// or list greedily — each priced against taking the instant fill now.
///
/// Returns nothing rather than erroring: the section is advisory, and a pair
/// with no maker picture still has a working convert report above it.
fn maker_model(
    market: &Market,
    have: &MarketAssetId,
    need: &MarketAssetId,
    size: u64,
) -> Option<MakerModel> {
    use ptt_strategy::{MakerMode, MakerRequest, calculate_maker_strategy};

    let amount_in = AssetAmount::from_whole_units(have.clone(), size, &market.units).ok()?;
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
    let strategy = calculate_maker_strategy(base).ok()?;
    // The match-front variant is the same Opportunity mode priced at the
    // front instead of below it; a second evaluation keeps that trade-off
    // visible without a third mode existing anywhere. Skipped with no queue,
    // where there is no front to match.
    let match_front = if strategy.queue.is_empty() {
        None
    } else {
        calculate_maker_strategy(MakerRequest {
            match_front: true,
            ..base
        })
        .ok()
        .and_then(|matched| {
            matched
                .recommendations
                .into_iter()
                .find(|item| item.mode == MakerMode::Opportunity)
        })
    };
    Some(MakerModel {
        size,
        strategy,
        match_front,
    })
}

/// The listing-strategy section as display lines.
fn render_maker(
    model: &MakerModel,
    have: &MarketAssetId,
    need: &MarketAssetId,
    language: UiLanguage,
) -> Vec<String> {
    use crate::report_text::fill;
    use ptt_strategy::{MakerMode, MakerRecommendation};

    let text = crate::report_text::report(language);
    let strategy = &model.strategy;
    let size = model.size;

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
    if let Some(recommendation) = &model.match_front {
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

/// Currencies with real buy pressure the user has not put in the focus list.
///
/// The window's books are folded as one pseudo-day and run through the
/// market pulse, so the evidence is anchor-valued listed quantities on both
/// sides — never how often a book was captured. An asset the user flips all
/// day with nothing on its demand side is not suggested; an asset seen once
/// with heavy buy pressure is. The pulse's quiet floor is the offer bar: no
/// direction can be read below it.
///
/// Offered from the first session rather than only once a list exists. The
/// list is meant to be built out of what the market turns out to want, and a
/// feature that suggests nothing until you have already used it cannot start
/// that.
fn focus_suggestions(
    observations: &[MarketEdgeObservation],
    policy: &MarketPolicy,
    tuning: &MarketTuning,
) -> Vec<FocusSuggestion> {
    let configured: std::collections::BTreeSet<MarketAssetId> = tuning
        .focus_assets
        .iter()
        .chain(&tuning.bridge_assets)
        .chain(&tuning.watch_only_assets)
        .filter_map(|id| MarketAssetId::try_new(id).ok())
        .collect();
    let day_key = Utc::now().format("%Y-%m-%d").to_string();
    let stats = crate::rollup::fold_window_stats(observations, &day_key);
    let thresholds = analytics_thresholds_from(tuning).unwrap_or_default();
    let pulse = ptt_strategy::market_pulse(&stats, &policy.core_liquidity, &thresholds);

    let mut suggestions = Vec::new();
    // pulse.assets arrive sorted by demand pressure descending.
    for asset in &pulse.assets {
        if policy.is_core_liquidity(&asset.asset_id) || configured.contains(&asset.asset_id) {
            continue;
        }
        // Unvalued demand cannot be compared across assets; below the quiet
        // floor there is no direction to read at all.
        let Some(demand) = asset.demand_anchor else {
            continue;
        };
        if demand < thresholds.quiet_floor_anchor_units {
            continue;
        }
        let demand = u64::try_from(demand).unwrap_or(u64::MAX);
        // Dismissals are held until the pressure doubles what it was.
        let due = tuning
            .ignored_suggestions
            .iter()
            .find(|ignored| ignored.asset_id == asset.asset_id.as_str())
            .is_none_or(|ignored| ignored.is_due_again(demand));
        if !due {
            continue;
        }
        suggestions.push(FocusSuggestion {
            asset_id: asset.asset_id.clone(),
            demand_anchor: demand,
            supply_anchor: u64::try_from(asset.supply_anchor.unwrap_or(0)).unwrap_or(u64::MAX),
        });
    }
    suggestions.truncate(4);
    suggestions
}

/// What a scope is being built to answer.
///
/// The two answers differ, and that difference is the point of having a focus
/// list at all. Arbitrage is arithmetic: a currency the window can price is
/// worth checking whether or not anyone declared an interest in it, and
/// refusing to price it because it is off a list throws away a real
/// opportunity the data already contains. Attention is a choice: coverage and
/// probe suggestions measure what the user says they trade, and measuring
/// them against everything the watcher happened to see buries that answer
/// under currencies nobody asked about.
///
/// Conflating them meant the moment a user curated a list — the feature
/// working as intended — everything they had captured but not listed silently
/// stopped being scanned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusPurpose {
    /// The list, and everything else the window can price.
    Arbitrage,
    /// The list. Or, with no list configured, everything seen: a scope of
    /// nothing would report perfect coverage of no pairs.
    Attention,
}

/// The focus items the reports scope over: the settlement currencies as
/// anchors, then the user's configured lists, then — for arbitrage — every
/// other asset seen in the window.
///
/// Watch-only and bridge are pushed first and win over target, so a currency
/// the user has explicitly demoted cannot come back in through "seen". That
/// is the one exclusion a focus list still performs, and it is deliberate:
/// watch-only means "price it, never route through it".
///
/// Listed targets come before seen ones because the radar divides its budget
/// among targets in order, so the list decides who gets scanned first when
/// there is not enough budget for everyone.
fn focus_items_from(
    policy: &MarketPolicy,
    tuning: &MarketTuning,
    seen: &[MarketAssetId],
    purpose: FocusPurpose,
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
    for id in &tuning.focus_assets {
        if let Ok(asset) = MarketAssetId::try_new(id) {
            push(asset, FocusRole::Target, &mut taken, &mut items);
        }
    }
    let include_seen = purpose == FocusPurpose::Arbitrage || tuning.focus_assets.is_empty();
    if include_seen {
        for asset in seen {
            push(asset.clone(), FocusRole::Target, &mut taken, &mut items);
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
    let model = watchlist_model(observations, context_key, league, tuning, language)?;
    Ok(render_watchlist(&model, language))
}

/// Everything the Watchlist page knows, before anything decides how to draw
/// it.
pub fn watchlist_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<WatchlistModel, String> {
    let market = build_market(observations, context_key, tuning, language)?;
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    let mut notes = market.notes.clone();
    notes.extend(policy_warning);

    // Value everything seen against the first core currency.
    let Some(anchor) = policy.core_liquidity.first().cloned() else {
        return Ok(WatchlistModel {
            notes,
            core_liquidity: policy.core_liquidity,
            valuations: Vec::new(),
            coverage: CoverageOutcome::NotComputed,
            suggestions: Vec::new(),
            anchors: Vec::new(),
        });
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

    let valuations = seen
        .iter()
        .filter(|asset| **asset != anchor)
        .map(|asset| AssetValuation {
            asset_id: asset.clone(),
            valuation: value_against_anchor(ValuationRequest {
                asset_id: asset,
                anchor_asset_id: &anchor,
                mode: ValuationMode::Midpoint,
                edges: &market.selected,
                include_historical: false,
            }),
        })
        .collect();

    // Typed coverage gaps for the pairs this focus group cares about, and
    // the probes that would close them.
    let coverage = match focus_coverage(
        observations,
        context_key,
        &policy,
        tuning,
        &seen,
        Some(&market),
    ) {
        Ok((status, entries, candidates)) => CoverageOutcome::Ready(CoverageModel {
            status,
            entries,
            candidates,
        }),
        Err(reason) => CoverageOutcome::Failed(reason),
    };

    let suggestions = focus_suggestions(observations, &policy, tuning);
    let anchors = recommend_liquidity_anchors(&market.selected, &policy);

    Ok(WatchlistModel {
        notes,
        core_liquidity: policy.core_liquidity,
        valuations,
        coverage,
        suggestions,
        anchors,
    })
}

/// The Watchlist page as display lines.
fn render_watchlist(model: &WatchlistModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();

    lines.push(fill(
        text.core_liquidity,
        &[&model
            .core_liquidity
            .iter()
            .map(MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(", ")],
    ));

    for entry in &model.valuations {
        let value = match (&entry.valuation.value, entry.valuation.status) {
            (Some(value), ValuationStatus::TwoSided) => {
                fill(text.valuation_two_sided, &[&value.text])
            }
            (Some(value), _) => fill(text.valuation_one_sided, &[&value.text]),
            (None, _) => text.no_price_capture.to_owned(),
        };
        lines.push(format!("{:<20} {value}", entry.asset_id.as_str(),));
    }

    match &model.coverage {
        CoverageOutcome::NotComputed => return lines,
        CoverageOutcome::Failed(reason) => {
            lines.push(fill(text.coverage_unavailable, &[reason]));
        }
        CoverageOutcome::Ready(coverage) => lines.extend(render_coverage(coverage, language)),
    }

    for suggestion in &model.suggestions {
        lines.push(fill(
            text.focus_suggestion,
            &[
                suggestion.asset_id.as_str(),
                &suggestion.demand_anchor.to_string(),
                &suggestion.supply_anchor.to_string(),
            ],
        ));
    }

    for recommendation in &model.anchors {
        lines.push(fill(
            text.anchor_recommendation,
            &[
                crate::report_text::anchor_action(language, recommendation.action),
                recommendation.asset_id.as_str(),
                &format!(
                    "{}.{}",
                    recommendation.score_tenths / 10,
                    recommendation.score_tenths % 10
                ),
                &recommendation.pair_coverage_count.to_string(),
                &recommendation.bidirectional_pair_count.to_string(),
            ],
        ));
    }
    lines
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
    let model = opportunities_model(observations, context_key, league, tuning, language, None)?;
    Ok(render_opportunities(&model, language))
}

/// Everything the Opportunities page knows, before anything decides how to
/// draw it.
pub fn opportunities_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<OpportunitiesModel, String> {
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    // The unavailable answers carry no notes: each is a single sentence about
    // why there is no page, and a degradation notice above it would be
    // commentary on a scan that never ran.
    if policy.core_liquidity.is_empty() {
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::NoCoreCurrency),
        });
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
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::NotEnoughMarket),
        });
    }
    let market = build_market(observations, context_key, tuning, language)?;

    let items = focus_items_from(&policy, tuning, &seen, FocusPurpose::Arbitrage);
    // Routing is the one place the setting applies. Coverage keeps the plain
    // policy below: target-to-target pairs would square the list the reader
    // is meant to work through, and that list is about attention.
    let scope = FocusScope::try_new(
        &items,
        FocusScopePolicy {
            allow_target_interconnect: tuning.route_through_targets,
            ..FocusScopePolicy::default()
        },
    )
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
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::CannotStake {
                stake,
                anchor: policy.core_liquidity.first().cloned(),
            }),
        });
    }
    let start_names: Vec<MarketAssetId> = starts
        .iter()
        .map(|start| start.start_asset_id.clone())
        .collect();
    // Settings values pass through the engine's own validation bounds; a
    // hand-edited extreme is clamped to the largest honest value rather than
    // failing the page.
    let minimum_bps =
        u32::try_from(tuning.radar.minimum_profit_basis_points.min(1_000_000)).unwrap_or(100);
    let budget_expansions =
        u32::try_from(tuning.radar.max_total_expansions.clamp(1, 1_000_000)).unwrap_or(60_000);
    let max_results = u16::try_from(tuning.radar.max_results.clamp(1, 500)).unwrap_or(12);
    // Three is the shortest loop that exists; past that the graph limits the
    // walk long before the bound does.
    let max_cycle_length = u8::try_from(tuning.radar.max_cycle_length.clamp(3, 12)).unwrap_or(6);
    let request = RadarRequest {
        context_key: context_key.to_owned(),
        starts,
        minimum_conversion_improvement_basis_points: minimum_bps,
        minimum_triangle_profit_basis_points: minimum_bps,
        max_hops: 3,
        max_cycle_length,
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

    let mut notes = market.notes.clone();
    notes.extend(policy_warning);

    let freshness = market.instant_selection.policy.freshness;
    let now = Utc::now();
    let items = result
        .items
        .into_iter()
        .map(|item| {
            // The light reads the oldest leg: a route is only as current as
            // the capture it leans on hardest.
            let light = item
                .conversion_path
                .as_ref()
                .and_then(|path| path.capture_time_evidence.as_ref())
                .or_else(|| {
                    item.triangle
                        .as_ref()
                        .and_then(|triangle| triangle.capture_time_evidence.as_ref())
                })
                .map(|evidence| {
                    freshness
                        .classify(evidence.earliest_captured_at, now)
                        .status
                });
            let structural = structural_notes_for(&item.path_asset_ids, pulse);
            OpportunityRow {
                item,
                light,
                structural,
            }
        })
        .collect();

    Ok(OpportunitiesModel {
        notes,
        scan: RadarScan::Ran(Box::new(RadarScanResult {
            stake,
            starts: start_names,
            items,
            probe_candidates: result.probe_candidates,
            diagnostics: result.diagnostics,
        })),
    })
}

/// The Opportunities page as display lines.
fn render_opportunities(model: &OpportunitiesModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);

    let scan = match &model.scan {
        RadarScan::Unavailable(RadarUnavailable::NoCoreCurrency) => {
            return vec![text.no_core_currency.to_owned()];
        }
        RadarScan::Unavailable(RadarUnavailable::NotEnoughMarket) => {
            return vec![text.not_enough_market.to_owned()];
        }
        RadarScan::Unavailable(RadarUnavailable::CannotStake { stake, anchor }) => {
            return vec![fill(
                text.cannot_stake,
                &[
                    &stake.to_string(),
                    anchor.as_ref().map_or("?", MarketAssetId::as_str),
                ],
            )];
        }
        RadarScan::Ran(scan) => scan,
    };

    let mut lines = model.notes.clone();
    lines.push(fill(
        text.staking,
        &[
            &scan.stake.to_string(),
            &scan
                .starts
                .iter()
                .map(MarketAssetId::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            &scan.diagnostics.target_count.to_string(),
        ],
    ));
    // Said before the results, not after: a truncated search that looks like a
    // complete one is how "there is nothing better" gets believed.
    if scan.diagnostics.budget_exhausted || scan.diagnostics.results_truncated {
        lines.push(fill(
            text.partial_scan,
            &[
                &scan.diagnostics.skipped_target_count.to_string(),
                &scan.diagnostics.expansions_used.to_string(),
                if scan.diagnostics.results_truncated {
                    text.results_cut
                } else {
                    ""
                },
            ],
        ));
    }
    // The radar's own probe suggestions — the pairs whose absence or
    // staleness limited what it could claim. Shown whether or not any item
    // survived, because an empty page is exactly when "go flip X" matters
    // most.
    let mut probe_lines = Vec::new();
    if !scan.probe_candidates.is_empty() {
        probe_lines.push(text.radar_probe_header.to_owned());
        for candidate in scan.probe_candidates.iter().take(4) {
            probe_lines.push(format!(
                "  {}  {} {} -> {}   ({})",
                crate::report_text::probe_priority(language, candidate.priority),
                text.flip,
                candidate.from_asset_id.as_str(),
                candidate.to_asset_id.as_str(),
                crate::report_text::probe_reason(language, candidate.reason),
            ));
        }
    }
    if scan.items.is_empty() {
        lines.push(text.nothing_beats_holding.to_owned());
        if scan.diagnostics.missing_conversion_count > 0 {
            lines.push(fill(
                text.targets_without_route,
                &[&scan.diagnostics.missing_conversion_count.to_string()],
            ));
        }
        lines.extend(probe_lines);
        return lines;
    }

    for row in &scan.items {
        lines.extend(radar_item_lines(&row.item, row.light, language));
    }
    lines.extend(probe_lines);
    lines
}

/// One radar item, as the page prints it.
///
/// Split out so it can be tested against a hand-built item. Reaching this code
/// through the search needs a market that actually contains an arbitrage, and
/// the captured corpus does not have one — leaving the only branch a user sees
/// when the radar succeeds as the only branch never executed.
/// The word introducing a list of risk flags.
const fn risks_label(language: UiLanguage) -> &'static str {
    crate::report_text::report(language).risks
}

fn radar_item_lines(
    item: &ptt_workflows::RadarItem,
    freshness: Option<FreshnessStatus>,
    language: UiLanguage,
) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let route = item
        .path_asset_ids
        .iter()
        .map(MarketAssetId::as_str)
        .collect::<Vec<_>>()
        .join(" -> ");
    let edge = item.value_basis_points.map_or_else(
        || text.unpriced.to_owned(),
        |points| format!("{}.{:02}%", points / 100, (points % 100).abs()),
    );
    let category = crate::report_text::actionability(language, item.category);
    let light = freshness.map_or(String::new(), |status| {
        format!(
            "   {}",
            crate::report_text::freshness_light(language, status)
        )
    });
    let mut lines = vec![
        format!(
            "{edge:>8}  {}  {route}",
            crate::report_text::radar_item_kind(language, item.kind)
        ),
        format!(
            "          {category}   {}{light}",
            fill(
                text.out_amount,
                &[
                    item.amount_out.quanta.to_string().as_str(),
                    item.path_asset_ids
                        .last()
                        .map_or("?", MarketAssetId::as_str),
                ],
            )
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
    let model = probe_queue_model(observations, context_key, league, tuning, language)?;
    Ok(render_probe_queue(&model, language))
}

/// The probe queue, before anything decides how to draw it.
pub fn probe_queue_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<ProbeQueueModel, String> {
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
        return Ok(ProbeQueueModel {
            nothing_captured: true,
            complete_pairs: 0,
            total_pairs: 0,
            candidates: Vec::new(),
        });
    }

    let (_status, coverage, candidates) =
        focus_coverage(observations, context_key, &policy, tuning, &seen, None)?;
    let missing = coverage
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .count();
    Ok(ProbeQueueModel {
        nothing_captured: false,
        complete_pairs: coverage.len() - missing,
        total_pairs: coverage.len(),
        candidates,
    })
}

/// The probe queue as display lines.
fn render_probe_queue(model: &ProbeQueueModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    if model.nothing_captured {
        return vec![text.no_pairs_captured.to_owned()];
    }
    let mut lines = vec![fill(
        text.pairs_complete,
        &[
            &model.complete_pairs.to_string(),
            &model.total_pairs.to_string(),
        ],
    )];
    if model.candidates.is_empty() {
        lines.push(text.nothing_to_probe.to_owned());
        return lines;
    }
    for candidate in model.candidates.iter().take(6) {
        lines.push(format!(
            "{}  flip {} -> {}   ({})",
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
        ));
    }
    lines
}

/// "What has this pair been doing": candles, a summary, and what looks off.
pub fn history_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = history_model(observations, context_key, have, need, tuning, language)?;
    Ok(render_history(&model, language))
}

/// Everything the History page knows, before anything decides how to draw it.
pub fn history_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<HistoryModel, String> {
    let market = build_market(observations, context_key, tuning, language)?;
    let points = price_points(&market.selected, have, need);
    if points.is_empty() {
        return Ok(HistoryModel {
            notes: market.notes,
            have: have.clone(),
            need: need.clone(),
            summary: None,
            light: None,
            candles: Vec::new(),
            anomalies: Vec::new(),
        });
    }

    let summary = summarize(&points, have, need);
    // The pair's traffic light reads the newest capture of this direction:
    // older points are history, but the light describes what acting now
    // would lean on.
    let light = observations
        .iter()
        .filter(|observation| {
            observation.edge.from_asset_id == *have && observation.edge.to_asset_id == *need
        })
        .map(|observation| observation.edge.captured_at)
        .max()
        .map(|newest| {
            market
                .instant_selection
                .policy
                .freshness
                .classify(newest, Utc::now())
                .status
        });
    let anomalies = anomalies(&summary, &points);
    Ok(HistoryModel {
        notes: market.notes,
        have: have.clone(),
        need: need.clone(),
        candles: candles(&points, BucketSize::FiveMinutes),
        summary: Some(summary),
        light,
        anomalies,
    })
}

/// The History page as display lines.
fn render_history(model: &HistoryModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let (have, need) = (&model.have, &model.need);
    let mut lines = model.notes.clone();

    let Some(summary) = &model.summary else {
        lines.push(fill(text.no_history_yet, &[have.as_str(), need.as_str()]));
        return lines;
    };
    lines.push(fill(
        text.history_header,
        &[
            have.as_str(),
            need.as_str(),
            &summary.point_count.to_string(),
            &summary.snapshot_count.to_string(),
        ],
    ));
    if let Some(status) = model.light {
        lines.push(fill(
            text.freshness_light_line,
            &[crate::report_text::freshness_light(language, status)],
        ));
    }
    if let Some(median) = &summary.median_rate {
        lines.push(fill(
            text.median_low_high,
            &[
                &median.text,
                summary
                    .min_rate
                    .as_ref()
                    .map_or("—", |rate| rate.text.as_str()),
                summary
                    .max_rate
                    .as_ref()
                    .map_or("—", |rate| rate.text.as_str()),
            ],
        ));
    }
    if let Some(spread) = summary.spread_basis_points {
        lines.push(fill(text.maker_over_taker, &[&spread.to_string()]));
    }
    if summary.historical_only {
        lines.push(text.nothing_current.to_owned());
    }

    // Newest first, and only the most recent few: the model keeps every
    // candle so a chart can draw the whole window.
    for candle in model.candles.iter().rev().take(8) {
        lines.push(fill(
            text.candle_line,
            &[
                &candle.bucket_start.format("%H:%M").to_string(),
                &candle.open.text,
                &candle.high.text,
                &candle.low.text,
                &candle.close.text,
                &candle.sample_count.to_string(),
                if candle.maker_only {
                    text.listings_note
                } else {
                    ""
                },
            ],
        ));
    }

    for anomaly in &model.anomalies {
        lines.push(format!(
            "{} ({}){}",
            crate::report_text::price_anomaly_kind(language, anomaly.kind),
            crate::report_text::anomaly_severity(language, anomaly.severity),
            anomaly
                .basis_points
                .map_or_else(String::new, |bps| format!(" {bps}bp")),
        ));
    }
    lines
}

/// Coverage and probe queue for the current focus group.
///
/// Coverage needs three views of the same book — what can be taken now, what
/// is only listed, and what shows up once old data is allowed — because
/// "missing" and "stale" are different problems with different fixes.
/// Prints every candidate edge for one pair in all three coverage views.
///
/// Diagnosis, not product: "I captured this pair and coverage still calls it
/// missing" cannot be answered from the screen, because the screen shows the
/// verdict and the reasons live on the edges. This walks the same three
/// selections coverage walks and says what each saw.
pub fn debug_pair(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    tuning: &MarketTuning,
    from: &str,
    to: &str,
) -> Result<(), String> {
    let market = build_market(observations, context_key, tuning, UiLanguage::English)?;
    let now = Utc::now();
    let mut views = vec![("instant", market.instant_selection.clone())];
    for (name, strategy) in [
        ("maker", QuoteSelectionStrategy::FastMaker),
        ("probe", QuoteSelectionStrategy::Probe),
    ] {
        let (policy, _) = selection_policy_from(tuning, strategy, UiLanguage::English)?;
        views.push((
            name,
            select_quote_edges(&market.book, &policy, now)
                .map_err(|error| format!("select: {error}"))?,
        ));
    }
    for (name, view) in &views {
        let selection = view.selections.iter().find(|selection| {
            selection.from_asset_id.as_str() == from && selection.to_asset_id.as_str() == to
        });
        let Some(selection) = selection else {
            println!("[{name}] no selection entry for {from} -> {to}");
            continue;
        };
        println!(
            "[{name}] {from} -> {to}: {} candidate edge(s), selected={}",
            selection.candidate_edges.len(),
            selection.selected_edge.is_some(),
        );
        for edge in &selection.candidate_edges {
            println!(
                "  role={:?} exec={:?} rate={} stock={} captured={} fresh={:?}",
                edge.observation.edge.role,
                edge.observation.edge.execution_type,
                edge.observation.edge.rate.text,
                edge.observation.edge.stock,
                edge.observation.edge.captured_at.format("%H:%M:%S"),
                edge.freshness.status,
            );
            println!(
                "    accepted={} depth_eligible={} rejections={:?} blockers={:?} risks={:?}",
                edge.accepted_for_selection,
                edge.eligible_for_depth_analysis,
                edge.selection_rejections,
                edge.execution_blockers,
                edge.risk_flags,
            );
        }
    }
    Ok(())
}

/// Why the radar says a target is unreachable, against the live database.
///
/// Diagnosis for "the radar wants a quote I have already captured": walks the
/// same scope and starts `opportunities_model` builds, and for each target
/// prints which step refuses it — no unit, no scope edge, no selected direct
/// edge, or no path. The one number that matters is how many of these the
/// selection could have answered directly.
pub fn debug_radar(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
) -> Result<(), String> {
    let (policy, _) = market_policy_from(tuning, league, UiLanguage::English);
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
    let market = build_market(observations, context_key, tuning, UiLanguage::English)?;
    let items = focus_items_from(&policy, tuning, &seen, FocusPurpose::Arbitrage);
    let scope = FocusScope::try_new(
        &items,
        FocusScopePolicy {
            allow_target_interconnect: tuning.route_through_targets,
            ..FocusScopePolicy::default()
        },
    )
    .map_err(|error| format!("scope: {error}"))?;
    println!(
        "scope: status={:?} anchors={} targets={} bridges={} watch_only={} intermediates={} \
         endpoints={} route_through_targets={}",
        scope.status,
        scope.anchors.len(),
        scope.targets.len(),
        scope.bridges.len(),
        scope.watch_only.len(),
        scope.intermediate_asset_ids.len(),
        scope.endpoint_asset_ids.len(),
        tuning.route_through_targets,
    );
    let selection = &market.instant_selection;
    println!(
        "instant selection: {} pair entries, {} with a selected edge",
        selection.selections.len(),
        selection
            .selections
            .iter()
            .filter(|entry| entry.selected_edge.is_some())
            .count(),
    );
    let kept = selection
        .selections
        .iter()
        .filter(|entry| scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id))
        .count();
    println!(
        "after scope restriction: {kept} of {} pair entries survive",
        selection.selections.len()
    );

    for start in &policy.core_liquidity {
        for target in &scope.endpoint_asset_ids {
            if target == start || !scope.endpoint_pair_allowed(start, target) {
                continue;
            }
            let direct = selection
                .selections
                .iter()
                .find(|entry| entry.from_asset_id == *start && entry.to_asset_id == *target);
            let direct_state = match direct {
                None => "no-entry".to_owned(),
                Some(entry) => format!(
                    "selected={} candidates={} scope_allows={}",
                    entry.selected_edge.is_some(),
                    entry.candidate_edges.len(),
                    scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id),
                ),
            };
            // What the radar actually asks the engine, at several stakes: a
            // path that appears only at a larger stake was never a data gap.
            let mut routed = String::new();
            for stake in [tuning.radar.stake.max(1), 100, 10_000] {
                let Ok(amount_in) =
                    AssetAmount::from_whole_units(start.clone(), stake, &market.units)
                else {
                    continue;
                };
                let restricted = {
                    let mut clone = selection.clone();
                    clone.selections.retain(|entry| {
                        scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id)
                    });
                    clone
                };
                let Ok(index) =
                    MarketDepthIndex::try_from_selection(&restricted, market.units.clone())
                else {
                    continue;
                };
                let routable: Vec<MarketAssetId> = scope
                    .intermediate_asset_ids
                    .iter()
                    .filter(|asset| market.units.contains(asset))
                    .cloned()
                    .collect();
                let outcome = find_best_conversion(
                    &index,
                    &ConversionRequest {
                        from_asset_id: start.clone(),
                        to_asset_id: target.clone(),
                        amount_in,
                        max_hops: 3,
                        max_paths: 32,
                        max_expansions: 4_000,
                        alternative_limit: 0,
                        allowed_intermediate_asset_ids: Some(routable),
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                );
                let verdict = match &outcome {
                    Ok(result) => match &result.best_path {
                        Some(path) => format!(
                            "out={} filled={}",
                            path.amount_out.quanta, path.is_fully_filled
                        ),
                        None => "NO-PATH".to_owned(),
                    },
                    Err(error) => format!("{error:?}"),
                };
                routed.push_str(&format!(" [{stake}: {verdict}]"));
            }
            println!(
                "{:<26} -> {:<26} unit={} routable_target={} direct[{direct_state}]{routed}",
                start.as_str(),
                target.as_str(),
                market.units.contains(target),
                scope.intermediate_asset_ids.contains(target),
            );
        }
    }

    // How the cycle walk actually grows with its length bound, on this book
    // rather than in the abstract. The captured graph is hub-and-spoke --
    // most currencies are only ever priced against the settlement pair -- so
    // the exponent that a complete market would carry is not the one here,
    // and the only way to know where the knee is is to walk it.
    {
        let index =
            MarketDepthIndex::try_from_selection(&market.instant_selection, market.units.clone())
                .map_err(|error| format!("index: {error:?}"))?;
        for length in 3..=7u8 {
            let started = std::time::Instant::now();
            let mut found = 0usize;
            let mut profitable = 0usize;
            for start in &policy.core_liquidity {
                let result = ptt_trade_engine::find_triangle_opportunities(
                    &index,
                    &ptt_trade_engine::TriangleRequest {
                        start_asset_id: start.clone(),
                        amount_in: None,
                        minimum_profit_basis_points: 100,
                        max_cycle_length: length,
                        max_results: 500,
                        max_evaluations: 1_000_000,
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                );
                match result {
                    Ok(result) => {
                        found += result.diagnostics.evaluated_cycle_count as usize;
                        profitable += result.opportunities.len();
                    }
                    Err(error) => {
                        println!("  length {length}: {error:?}");
                        break;
                    }
                }
            }
            println!(
                "cycle length <= {length}: {found} cycles walked, {profitable} profitable, {:.0}ms",
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    // What the page itself would print, so the diagnosis and the product
    // cannot disagree about the same database.
    println!("\n--- the Opportunities page, verbatim ---");
    let model = opportunities_model(
        observations,
        context_key,
        league,
        tuning,
        UiLanguage::English,
        None,
    )?;
    if let RadarScan::Ran(scan) = &model.scan {
        println!("diagnostics: {:?}", scan.diagnostics);
    }
    for line in opportunities_report(
        observations,
        context_key,
        league,
        tuning,
        UiLanguage::Chinese,
    )? {
        println!("{line}");
    }
    Ok(())
}

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
        ptt_workflows::FocusScopeStatus,
        Vec<ptt_workflows::FocusCoverage>,
        Vec<ptt_workflows::ProbeCandidate>,
    ),
    String,
> {
    let items = focus_items_from(policy, tuning, seen, FocusPurpose::Attention);
    let scope = FocusScope::try_new(&items, FocusScopePolicy::default())
        .map_err(|error| format!("{error}"))?;

    // Coverage needs three views of one book. The Instant view is the one the
    // caller already built, so it is borrowed rather than recomputed.
    let owned;
    let market = match prebuilt {
        Some(market) => market,
        None => {
            owned = build_market(observations, context_key, tuning, UiLanguage::English)?;
            &owned
        }
    };
    let now = Utc::now();
    let mut selections = Vec::new();
    for strategy in [
        QuoteSelectionStrategy::FastMaker,
        QuoteSelectionStrategy::Probe,
    ] {
        let (policy, _note) = selection_policy_from(tuning, strategy, UiLanguage::English)?;
        selections.push(
            select_quote_edges(&market.book, &policy, now)
                .map_err(|error| format!("select: {error}"))?,
        );
    }

    let (entries, candidates) = derive_focus_probe_candidates(
        "live-focus",
        &scope,
        &market.instant_selection,
        &selections[0],
        &selections[1],
    )
    .map_err(|error| format!("{error}"))?;
    Ok((scope.status, entries, candidates))
}

/// The same coverage, rendered for the watchlist page.
fn render_coverage(coverage: &CoverageModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = Vec::new();
    if coverage.status == ptt_workflows::FocusScopeStatus::MissingTarget {
        lines.push(text.focus_has_no_targets.to_owned());
    }
    let incomplete: Vec<_> = coverage
        .entries
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .collect();
    lines.push(fill(
        text.coverage_progress,
        &[
            &(coverage.entries.len() - incomplete.len()).to_string(),
            &coverage.entries.len().to_string(),
        ],
    ));
    for entry in incomplete.iter().take(8) {
        lines.push(format!(
            "  {} -> {}  {}",
            entry.from_asset_id.as_str(),
            entry.to_asset_id.as_str(),
            crate::report_text::focus_coverage_status(language, entry.status),
        ));
    }
    for candidate in coverage.candidates.iter().take(8) {
        lines.push(format!(
            "  {} {}: {} -> {}  ({})",
            text.probe,
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// Analytics: season-scale value, supply/demand pressure, anchor health
// ---------------------------------------------------------------------------

/// Builds the analytics page model from persisted day rollups plus a live
/// fold of today's window observations. Pure over its inputs: the caller
/// fetches rollup rows, the window, and the season (see `rollup`).
#[must_use]
pub fn analytics_model(
    rollup_rows: &[ptt_storage::PairDayRollupRow],
    window_observations: &[MarketEdgeObservation],
    season: Option<&ptt_storage::SeasonRow>,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> AnalyticsModel {
    let text = crate::report_text::report(language);
    let mut notes = Vec::new();

    let (mut stats, row_notes) = crate::rollup::stats_from_rollup_rows(rollup_rows);
    notes.extend(row_notes);
    stats.extend(crate::rollup::today_stats(window_observations, Utc::now()));

    let (policy, policy_note) = market_policy_from(tuning, league, language);
    notes.extend(policy_note);

    let thresholds = match analytics_thresholds_from(tuning) {
        Some(thresholds) => thresholds,
        None => {
            notes.push(text.analytics_config_invalid.to_owned());
            ptt_strategy::AnalyticsThresholds::default()
        }
    };

    let data_days: std::collections::BTreeSet<&str> =
        stats.iter().map(|stat| stat.utc_day.as_str()).collect();
    let data_days = u32::try_from(data_days.len()).unwrap_or(u32::MAX);
    let pulse = ptt_strategy::market_pulse(&stats, &policy.core_liquidity, &thresholds);

    AnalyticsModel {
        notes,
        season: season.map(|row| SeasonBanner {
            label: row.label.clone(),
            started_day: row.started_at.format("%Y-%m-%d").to_string(),
        }),
        data_days,
        pulse,
    }
}

/// The tuned analytics thresholds, or `None` when the settings values do not
/// validate (the caller notes the degradation and uses the defaults).
fn analytics_thresholds_from(tuning: &MarketTuning) -> Option<ptt_strategy::AnalyticsThresholds> {
    let analytics = &tuning.analytics;
    ptt_strategy::AnalyticsThresholds::try_new(
        u32::try_from(analytics.trend_recent_days).ok()?,
        u32::try_from(analytics.trend_window_days).ok()?,
        u32::try_from(analytics.breadth_threshold_percent).ok()?,
        i64::try_from(analytics.verdict_threshold_bps).ok()?,
        u32::try_from(analytics.scarce_ratio_percent).ok()?,
        u128::from(analytics.quiet_floor_anchor_units),
    )
    .ok()
}

/// The analytics model as text lines — the probe's verification surface and
/// the parity reference for the page.
#[must_use]
pub fn analytics_report_lines(model: &AnalyticsModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::{anchor_drift, fill, liquidity_class, trend_verdict};
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();
    if let Some(season) = &model.season {
        lines.push(fill(
            text.analytics_season_line,
            &[&season.label, &season.started_day],
        ));
    }
    let Some(as_of) = &model.pulse.as_of_day else {
        lines.push(text.analytics_no_data.to_owned());
        return lines;
    };
    lines.push(fill(
        text.analytics_as_of,
        &[as_of, &model.data_days.to_string()],
    ));
    if let Some(health) = &model.pulse.anchor_health {
        lines.push(fill(
            text.analytics_anchor_line,
            &[
                health.anchor_asset_id.as_str(),
                anchor_drift(language, health.drift),
            ],
        ));
        let median = health
            .market_median_move_bps
            .map_or_else(|| "-".to_owned(), |bps| bps.to_string());
        lines.push(fill(
            text.analytics_breadth_line,
            &[
                &health.risers.to_string(),
                &health.fallers.to_string(),
                &health.flat.to_string(),
                &median,
            ],
        ));
        for cross in &health.crosses {
            let drift = cross
                .drift_bps
                .map_or_else(|| "-".to_owned(), |bps| bps.to_string());
            lines.push(fill(
                text.analytics_cross_line,
                &[cross.asset_id.as_str(), &cross.latest_rate.text, &drift],
            ));
        }
    }
    lines.push(text.analytics_table_header.to_owned());
    for asset in &model.pulse.assets {
        let value = asset
            .value_in_anchor
            .as_ref()
            .map_or_else(|| "-".to_owned(), |rate| rate.text.clone());
        let supply = asset.supply_anchor.map_or_else(
            || format!("{}?", asset.supply_units),
            |units| units.to_string(),
        );
        let demand = asset.demand_anchor.map_or_else(
            || format!("{}?", asset.demand_units),
            |units| units.to_string(),
        );
        let verdict = asset
            .verdict
            .map_or("-", |verdict| trend_verdict(language, verdict));
        let mut markers = String::new();
        if asset.high_turnover {
            markers.push_str("  [");
            markers.push_str(text.analytics_marker_high_turnover);
            markers.push(']');
        }
        if asset.greedy_candidate {
            markers.push_str("  [");
            markers.push_str(text.analytics_marker_greedy);
            markers.push(']');
        }
        lines.push(format!(
            "{:<28} {:>12} {:>14} {:>14}  {}  {}{}",
            asset.asset_id.as_str(),
            value,
            supply,
            demand,
            liquidity_class(language, asset.class),
            verdict,
            markers,
        ));
    }
    lines
}

#[cfg(test)]
mod focus_scope_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn tuning(focus: &[&str], watch_only: &[&str]) -> MarketTuning {
        MarketTuning {
            focus_assets: focus.iter().map(|id| (*id).to_owned()).collect(),
            watch_only_assets: watch_only.iter().map(|id| (*id).to_owned()).collect(),
            ..MarketTuning::default()
        }
    }

    fn role_of(items: &[FocusGroupItem], id: &str) -> Option<FocusRole> {
        items
            .iter()
            .find(|item| item.asset_id.as_str() == id)
            .map(|item| item.role)
    }

    /// A currency the user has not listed is still worth arbitrage.
    ///
    /// Curating a focus list used to silence every other captured currency:
    /// the moment the feature was used as intended, opportunities the data
    /// already contained stopped being looked for. Being on the list buys
    /// priority and attention, not permission to be priced.
    #[test]
    fn an_unlisted_but_captured_currency_is_still_an_arbitrage_target() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &[]);
        let seen = vec![asset("omen-of-light"), asset("fracturing-orb")];

        let arbitrage = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Arbitrage);
        assert_eq!(
            role_of(&arbitrage, "fracturing-orb"),
            Some(FocusRole::Target),
            "a captured currency is arithmetic, not a preference"
        );

        // The other half of the split: attention stays where the user put it.
        let attention = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Attention);
        assert_eq!(role_of(&attention, "fracturing-orb"), None);
        assert_eq!(
            role_of(&attention, "omen-of-light"),
            Some(FocusRole::Target)
        );
    }

    /// The list decides the order, and the order decides the budget.
    #[test]
    fn listed_targets_come_before_captured_ones() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &[]);
        let seen = vec![asset("fracturing-orb"), asset("omen-of-light")];
        let items = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Arbitrage);
        let targets: Vec<&str> = items
            .iter()
            .filter(|item| item.role == FocusRole::Target)
            .map(|item| item.asset_id.as_str())
            .collect();
        assert_eq!(targets.first(), Some(&"omen-of-light"));
    }

    /// Watch-only is the one exclusion a list still performs.
    #[test]
    fn watch_only_survives_being_captured() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &["fracturing-orb"]);
        let seen = vec![asset("fracturing-orb")];
        for purpose in [FocusPurpose::Arbitrage, FocusPurpose::Attention] {
            let items = focus_items_from(&policy, &tuning, &seen, purpose);
            assert_eq!(
                role_of(&items, "fracturing-orb"),
                Some(FocusRole::WatchOnly),
                "{purpose:?} must not promote a demoted currency"
            );
        }
    }

    /// With nothing configured, both answers are "everything seen" -- the
    /// behaviour before the split, unchanged.
    #[test]
    fn an_empty_list_scopes_over_everything_seen() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&[], &[]);
        let seen = vec![asset("fracturing-orb")];
        for purpose in [FocusPurpose::Arbitrage, FocusPurpose::Attention] {
            let items = focus_items_from(&policy, &tuning, &seen, purpose);
            assert_eq!(role_of(&items, "fracturing-orb"), Some(FocusRole::Target));
        }
    }
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
            liquidity_capacity: None,
            reasons: vec![ptt_workflows::RadarReason::BetterThanDirect],
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let lines = radar_item_lines(&item, None, UiLanguage::English);
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
        let chinese = radar_item_lines(&item, None, UiLanguage::Chinese).join(
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
            kind: ptt_workflows::RadarItemKind::Loop,
            category: ptt_strategy::Actionability::ProbeRequired,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 11, &units)
                .expect("out"),
            value_basis_points: None,
            liquidity_capacity: None,
            reasons: Vec::new(),
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let joined = radar_item_lines(&item, None, UiLanguage::English).join(
            "
",
        );
        assert!(joined.contains("unpriced"), "{joined}");
        assert!(joined.contains("capture more before trusting"), "{joined}");
        let chinese = radar_item_lines(&item, None, UiLanguage::Chinese).join(
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

    /// A taker row between any two assets, captured `age_seconds` ago.
    fn aged_taker(
        from: &str,
        to: &str,
        rate: (u64, u64),
        stock: u64,
        age_seconds: i64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::seconds(age_seconds);
        taker_at(from, to, rate, stock, captured)
    }

    /// A fresh taker row between any two assets.
    fn taker(from: &str, to: &str, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        taker_at(from, to, rate, stock, captured)
    }

    fn taker_at(
        from: &str,
        to: &str,
        rate: (u64, u64),
        stock: u64,
        captured: chrono::DateTime<Utc>,
    ) -> MarketEdgeObservation {
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
        // The kind column, not the word: reason lines also say "loop".
        let loop_lines = lines
            .iter()
            .filter(|line| line.contains("  loop  "))
            .count();
        assert_eq!(
            loop_lines, 1,
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

    /// A focus asset the book has never priced becomes a probe suggestion
    /// on the radar page — the loop that sends the user into the game to
    /// close the gap — instead of an engine error or a silent omission.
    #[test]
    fn a_never_captured_focus_asset_becomes_a_probe_suggestion() {
        let observations = vec![taker("divine-orb", "chaos-orb", (100, 1), 1_000)];
        let tuning = MarketTuning {
            focus_assets: vec!["perfect-chaos-orb".to_owned()],
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
            joined.contains("flip divine-orb -> perfect-chaos-orb"),
            "the missing pair must be a suggestion, not an error: {joined}"
        );
        assert!(
            joined.contains("no forward quote"),
            "with its typed reason: {joined}"
        );
    }

    /// A currency captured repeatedly but absent from the focus list gets a
    /// promotion suggestion; focus members and settlement currencies do not.
    #[test]
    fn a_frequently_captured_outsider_is_suggested_for_focus() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 1_000),
            taker("vaal-orb", "divine-orb", (1, 50), 1_000),
            taker("vaal-orb", "chaos-orb", (2, 1), 1_000),
            taker("chaos-orb", "vaal-orb", (1, 2), 1_000),
            taker("exalted-orb", "chaos-orb", (1, 3), 1_000),
        ];
        let tuning = MarketTuning {
            focus_assets: vec!["exalted-orb".to_owned()],
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
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("consider adding vaal-orb to focus - 3 snapshots"),
            "three distinct snapshots must earn the suggestion: {joined}"
        );
        assert!(
            !joined.contains("consider adding exalted-orb"),
            "a focus member needs no promotion: {joined}"
        );
        assert!(
            !joined.contains("consider adding divine-orb"),
            "settlement currencies need no promotion: {joined}"
        );
    }

    /// The traffic light against the default 2h/6h bands: one hour is
    /// green, three is yellow, seven is red — and exactly 7200 seconds is
    /// still green, pinning the classifier's inclusive boundary.
    #[test]
    fn the_pair_light_follows_the_configured_bands() {
        for (age, expected) in [
            (3_600_i64, "green - fresh"),
            (7_200, "green - fresh"),
            (3 * 3_600, "yellow - verify in game before acting"),
            (7 * 3_600, "red - stale, recapture"),
        ] {
            let observations = vec![aged_taker("divine-orb", "chaos-orb", (100, 1), 1_000, age)];
            let lines = history_report(
                &observations,
                CONTEXT,
                &asset("divine-orb"),
                &asset("chaos-orb"),
                &MarketTuning::default(),
                UiLanguage::English,
            )
            .expect("report");
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("data freshness: {expected}"))),
                "age {age}s must read {expected}: {lines:?}"
            );
        }
    }

    /// Thresholds that do not validate (fresh >= usable) degrade to the
    /// shipped default with a visible line — the page still renders.
    #[test]
    fn invalid_freshness_thresholds_degrade_loudly() {
        let observations = vec![aged_taker("divine-orb", "chaos-orb", (100, 1), 1_000, 60)];
        let tuning = MarketTuning {
            freshness: ptt_settings::FreshnessTuning {
                fresh_seconds: 21_600,
                usable_seconds: 7_200,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = history_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("freshness thresholds in settings are invalid")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("data freshness: green - fresh")),
            "the default bands must still classify: {lines:?}"
        );
    }
}

#[cfg(test)]
mod structural_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn pulse_asset(id: &str, class: ptt_strategy::LiquidityClass) -> ptt_strategy::AssetPulse {
        ptt_strategy::AssetPulse {
            asset_id: asset(id),
            value_in_anchor: None,
            value_is_composed: false,
            supply_units: 0,
            demand_units: 0,
            supply_anchor: None,
            demand_anchor: None,
            listing_rows: 0,
            days_observed: 1,
            circulation_norm_units: None,
            trend_bps_raw: None,
            trend_bps_relative: None,
            verdict: None,
            class,
            high_turnover: false,
            greedy_candidate: class == ptt_strategy::LiquidityClass::Scarce,
        }
    }

    /// The annotation is context, never a verdict: legs keep path order, a
    /// repeated asset notes once, an unknown asset notes nothing, and no
    /// pulse means no notes — the page renders identically to before.
    #[test]
    fn structural_notes_annotate_without_reordering() {
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![
                pulse_asset(
                    "scroll-of-wisdom",
                    ptt_strategy::LiquidityClass::Oversupplied,
                ),
                pulse_asset("mirror-of-kalandra", ptt_strategy::LiquidityClass::Scarce),
            ],
        };
        let path = [
            asset("divine-orb"),
            asset("scroll-of-wisdom"),
            asset("mirror-of-kalandra"),
            asset("divine-orb"),
        ];

        let notes = structural_notes_for(&path, Some(&pulse));
        assert_eq!(notes.len(), 2, "unknown assets and repeats add nothing");
        assert_eq!(notes[0].asset_id.as_str(), "scroll-of-wisdom");
        assert!(
            notes[0].structurally_illiquid(),
            "oversupplied is the junk shape"
        );
        assert_eq!(notes[1].asset_id.as_str(), "mirror-of-kalandra");
        assert!(!notes[1].structurally_illiquid(), "scarce is not illiquid");
        assert!(notes[1].greedy_candidate);

        assert!(
            structural_notes_for(&path, None).is_empty(),
            "no pulse, no notes, no page change"
        );
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// One panel-orientation edge of a (need, have) book.
    fn book_edge(
        snapshot: &str,
        need: &str,
        have: &str,
        side: QuoteSide,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        let (role, execution) = match side {
            QuoteSide::Available => (QuoteEdgeRole::AvailableTaker, ExecutionType::Taker),
            QuoteSide::Competing => (
                QuoteEdgeRole::CompetingMakerReference,
                ExecutionType::MakerReference,
            ),
        };
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: format!("{snapshot}-{need}-{have}-{side:?}-{stock}"),
                snapshot_id: snapshot.to_owned(),
                quote_id: format!("quote-{snapshot}-{stock}"),
                context_key: "suggestion-test-context".to_owned(),
                from_asset_id: asset(have),
                to_asset_id: asset(need),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset(need),
                original_have_asset_id: asset(have),
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

    /// The user-adjudicated evidence rule: capture frequency proves only
    /// attention. An asset captured five times with nothing on its demand
    /// side is never suggested; an asset seen once with real buy pressure is.
    #[test]
    fn suggestions_follow_buy_pressure_not_capture_frequency() {
        let mut observations = Vec::new();
        // Flipped five times, supply only: nobody is buying it.
        for snapshot in 0..5 {
            observations.push(book_edge(
                &format!("shiny-{snapshot}"),
                "shiny-bauble",
                "divine-orb",
                QuoteSide::Available,
                (10, 1),
                500,
            ));
        }
        // Seen once, with 400 divine of standing buy pressure: the book
        // (need=wanted, have=divine) prices it at 5 divine each and its
        // competing side holds 400 divine seeking it.
        observations.push(book_edge(
            "wanted-1",
            "wanted-orb",
            "divine-orb",
            QuoteSide::Available,
            (1, 5),
            20,
        ));
        observations.push(book_edge(
            "wanted-1",
            "wanted-orb",
            "divine-orb",
            QuoteSide::Competing,
            (1, 5),
            400,
        ));

        let tuning = MarketTuning::default();
        let (policy, _) = market_policy_from(&tuning, "test-league", UiLanguage::English);
        let suggestions = focus_suggestions(&observations, &policy, &tuning);

        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.asset_id.as_str() == "wanted-orb"),
            "real buy pressure must be suggested: {suggestions:?}"
        );
        assert!(
            suggestions
                .iter()
                .all(|suggestion| suggestion.asset_id.as_str() != "shiny-bauble"),
            "capture frequency alone must never be evidence: {suggestions:?}"
        );
        let wanted = suggestions
            .iter()
            .find(|suggestion| suggestion.asset_id.as_str() == "wanted-orb")
            .expect("wanted-orb");
        assert_eq!(
            wanted.demand_anchor, 400,
            "400 divine paid out by the competing side, anchor-valued"
        );

        // A dismissal at this prominence holds until the pressure doubles.
        let mut dismissed = tuning.clone();
        dismissed
            .ignored_suggestions
            .push(ptt_settings::IgnoredSuggestion {
                asset_id: "wanted-orb".to_owned(),
                snapshots_when_ignored: wanted.demand_anchor,
            });
        let after = focus_suggestions(&observations, &policy, &dismissed);
        assert!(
            after
                .iter()
                .all(|suggestion| suggestion.asset_id.as_str() != "wanted-orb"),
            "a dismissed suggestion stays down until it doubles: {after:?}"
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

    /// "I hold 100" prices one size — the holding — and the ladder stays
    /// home. The maker section follows the same size.
    #[test]
    fn a_stated_holding_prices_exactly_that_size() {
        let observations = vec![
            taker("take-700", 0, (700, 1), 100_000),
            competing("front", 1, (784, 1), 40),
        ];
        let lines = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            Some(100),
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join("\n");
        assert!(
            joined.contains(" 100 divine-orb"),
            "the stated holding must be priced: {joined}"
        );
        assert!(
            !joined.contains("  10 divine-orb   via"),
            "the default ladder must not run alongside a stated holding: {joined}"
        );
        assert!(
            joined.contains("at size 100"),
            "the maker section follows the holding: {joined}"
        );
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
            None,
            &MarketTuning::default(),
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
            None,
            &MarketTuning::default(),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join("\n");
        assert!(chinese.contains("挂单策略"), "{chinese}");
        assert!(chinese.contains("价格离群"), "{chinese}");
    }
}
