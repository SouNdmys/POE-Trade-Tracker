use std::cmp::Ordering;
use std::collections::BTreeMap;

use ptt_market_book::{QuoteSelectionPolicy, QuoteSelectionResult, QuoteSelectionStrategy};
use ptt_strategy::{Actionability, ExecutionRisk, RiskThresholds, assess_path, assess_triangle};
use ptt_trade_domain::MarketAssetId;
use ptt_trade_engine::{
    AssetAmount, AssetUnitCatalog, ComparisonDirection, ConversionComparisonStatus, ConversionPath,
    ConversionRequest, EngineError, ExecutionRiskFlag, FeePolicy, MarketDepthIndex,
    SearchCancellation, TriangleEvaluation, TriangleRequest, canonical_cycle_key, chain_rates,
    find_best_conversion, find_triangle_opportunities,
};
use serde::{Deserialize, Serialize};

use crate::WorkflowError;
use crate::focus::{FocusScope, FocusScopeStatus, ProbePriority};
use crate::probe::{ProbeCandidate, ProbeReason, ProbeSource};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RadarStage {
    Preparing,
    ConversionScan,
    TriangleScan,
    Ranking,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarProgress {
    pub stage: RadarStage,
    pub completed_units: u32,
    pub total_units: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RadarItemKind {
    BestConversion,
    Triangle,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RadarReason {
    BetterThanDirect,
    NoDirectBaseline,
    TriangleReturn,
    GrossTheoryOnly,
    ResidualInventory,
    MakerReference,
    SearchTruncated,
    CaptureSkewUnverified,
    CaptureSkewExceeded,
    /// The configured stake could not buy one whole unit of this target, so
    /// the scan used the smallest size that can. The row's own `amount_in`
    /// says what that was.
    StakeRaisedToMinimum,
}

/// One settlement currency the radar scans from. Cycles rooted here close
/// back into it by construction, which is what makes it a settlement scan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarStart {
    pub start_asset_id: MarketAssetId,
    pub amount_in: AssetAmount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarRequest {
    pub context_key: String,
    /// The settlement currencies to scan from, in preference order. Every
    /// conversion and every cycle starts at one of these; the evaluation
    /// budgets are split equally across them.
    pub starts: Vec<RadarStart>,
    pub minimum_conversion_improvement_basis_points: u32,
    pub minimum_triangle_profit_basis_points: u32,
    pub max_hops: u8,
    pub max_paths_per_target: u32,
    /// Ceiling on the search each single target may consume.
    pub max_expansions_per_target: u32,
    /// Ceiling on the whole conversion scan.
    ///
    /// F8: without this, a scope with two hundred targets quietly does two
    /// hundred times the per-target work and no result says so. The budget is
    /// divided equally among targets rather than spent first-come — an
    /// early-exhausted budget would otherwise leave the tail of the scope
    /// unscanned while the report claimed full coverage, which is the audit's
    /// B-09 bias.
    pub budget: RadarBudget,
    pub max_triangle_evaluations: u32,
    pub max_results: u16,
    pub fee_policy: FeePolicy,
    /// Thresholds for the strategy risk ladder every item is assessed with.
    pub thresholds: RiskThresholds,
}

/// How much search one radar run may spend.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarBudget {
    /// Total state expansions across every target in the scope.
    pub max_total_expansions: u32,
    /// Most targets to scan. Zero means every target in scope.
    ///
    /// Enforced by the scan loop, which records anything it did not reach in
    /// `skipped_target_count` — a knob that is only stored would report a
    /// bound it never applied.
    pub max_targets: u32,
}

impl Default for RadarBudget {
    fn default() -> Self {
        // Enough to scan a few dozen targets to four hops without the scan
        // outlasting a user's patience.
        Self {
            max_total_expansions: 200_000,
            max_targets: 0,
        }
    }
}

impl RadarBudget {
    /// The share of the budget one target gets, given how many there are.
    ///
    /// Equal shares, floor-divided, never below one: the last target in the
    /// scope is scanned as hard as the first.
    #[must_use]
    fn per_target(self, target_count: usize) -> u32 {
        if target_count == 0 {
            return self.max_total_expansions;
        }
        let count = u32::try_from(target_count).unwrap_or(u32::MAX);
        (self.max_total_expansions / count).max(1)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarItem {
    pub item_id: String,
    pub kind: RadarItemKind,
    /// The strategy ladder's verdict — the same one the Convert page shows,
    /// so a route never reads as two different risks on two pages.
    pub category: Actionability,
    pub path_asset_ids: Vec<MarketAssetId>,
    pub amount_in: AssetAmount,
    pub amount_out: AssetAmount,
    pub value_basis_points: Option<i64>,
    /// How much of the settlement anchor this route's shown rates support.
    ///
    /// Expressed in the anchor rather than in each route's own start asset so
    /// that rows are comparable: ten divine and ten chaos are not the same
    /// depth, and a list sorted on the raw numbers puts every chaos-started
    /// row above every divine-started one for no reason but the unit. A fact
    /// about the panel; nothing here knows what the reader owns.
    pub liquidity_capacity: Option<u64>,
    pub reasons: Vec<RadarReason>,
    pub risk_flags: Vec<ExecutionRiskFlag>,
    /// The typed risks that keep this item off the instant rung.
    pub blocking_risks: Vec<ExecutionRisk>,
    pub conversion_path: Option<ConversionPath>,
    pub triangle: Option<TriangleEvaluation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarDiagnostics {
    pub target_count: u32,
    pub scanned_conversion_count: u32,
    pub complete_conversion_count: u32,
    pub missing_conversion_count: u32,
    /// Targets the book can route to but not at the stake asked for: whole
    /// units mean a stake below one unit of the target buys nothing at all.
    /// Counted apart from `missing_conversion_count` because the answer to
    /// one is "go capture that pair" and to the other is "trade bigger".
    pub unfillable_conversion_count: u32,
    pub triangle_evaluation_count: u32,
    pub item_count_before_limit: u32,
    /// State expansions the conversion scan consumed.
    pub expansions_used: u32,
    /// Targets in scope that the budget did not reach.
    pub skipped_target_count: u32,
    /// True when the scan stopped early rather than exhausting the space, so
    /// a missing opportunity may exist but was never looked for.
    pub budget_exhausted: bool,
    /// True when ranked results were cut to fit `max_results`. Distinct from
    /// `budget_exhausted`: this one means the answers exist and were dropped
    /// from the list, not that they were never computed.
    pub results_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarResult {
    pub context_key: String,
    pub analysis_policy: QuoteSelectionPolicy,
    pub starts: Vec<RadarStart>,
    pub items: Vec<RadarItem>,
    pub probe_candidates: Vec<ProbeCandidate>,
    pub diagnostics: RadarDiagnostics,
}

pub fn run_opportunity_radar(
    selection: &QuoteSelectionResult,
    units: &AssetUnitCatalog,
    scope: &FocusScope,
    request: &RadarRequest,
    cancellation: &SearchCancellation,
    mut progress: impl FnMut(RadarProgress),
) -> Result<RadarResult, WorkflowError> {
    validate_request(selection, units, scope, request)?;
    let mut request = request.clone();
    if !selection.policy.cost_verification.all_verified() {
        request.fee_policy = FeePolicy::Unknown;
    }
    let restricted = restrict_selection(selection, scope);
    // Only what the book can actually price. The scope names currencies the
    // user chose, and a chosen currency the window has never seen has no unit
    // yet — the engine rejects the whole search rather than the one asset, so
    // an unreachable name in the intermediate list would silence the radar
    // entirely. It stays a target, which is how it becomes a probe suggestion.
    let routable: Vec<MarketAssetId> = scope
        .intermediate_asset_ids
        .iter()
        .filter(|asset| units.contains(asset))
        .cloned()
        .collect();
    let index = MarketDepthIndex::try_from_selection(&restricted, units.clone())
        .map_err(|_| WorkflowError::InvalidMarketSelection)?;
    // One scan unit per (settlement start, target) pair, so the budget math
    // and the coverage claim describe the same set.
    let pairs = request
        .starts
        .iter()
        .flat_map(|start| {
            scope
                .endpoint_asset_ids
                .iter()
                .filter(|target| {
                    **target != start.start_asset_id
                        && scope.endpoint_pair_allowed(&start.start_asset_id, target)
                })
                .map(move |target| (start, target.clone()))
        })
        .collect::<Vec<_>>();
    let total_units = count(pairs.len())?
        .checked_add(1)
        .ok_or(WorkflowError::NumericOverflow)?;
    progress(RadarProgress {
        stage: RadarStage::Preparing,
        completed_units: 0,
        total_units,
    });

    // Every row's depth is restated in this one currency so the list can be
    // ordered by it. The first start is the settlement preference order's
    // primary, which is also what the valuations elsewhere anchor to.
    let anchor = request
        .starts
        .first()
        .map(|start| start.start_asset_id.clone());
    let mut items = Vec::new();
    let mut probe_candidates = Vec::new();
    let mut scanned_conversion_count = 0_u32;
    let mut complete_conversion_count = 0_u32;
    let mut missing_conversion_count = 0_u32;
    let mut unfillable_conversion_count = 0_u32;
    let mut expansions_used = 0_u32;
    let mut budget_exhausted = false;
    let per_target_budget = request
        .budget
        .per_target(pairs.len())
        .min(request.max_expansions_per_target);
    for (start, target) in &pairs {
        ensure_not_cancelled(cancellation)?;
        if expansions_used >= request.budget.max_total_expansions {
            budget_exhausted = true;
            break;
        }
        if request.budget.max_targets > 0 && scanned_conversion_count >= request.budget.max_targets
        {
            budget_exhausted = true;
            break;
        }
        progress(RadarProgress {
            stage: RadarStage::ConversionScan,
            completed_units: scanned_conversion_count,
            total_units,
        });
        // A target the book has never priced has no unit yet; the engine
        // rightly refuses to route to it. That is not an error — it is the
        // definition of a missing pair, so it becomes the probe suggestion
        // that sends the user to capture it.
        if !units.contains(target) {
            scanned_conversion_count = scanned_conversion_count
                .checked_add(1)
                .ok_or(WorkflowError::NumericOverflow)?;
            missing_conversion_count = missing_conversion_count
                .checked_add(1)
                .ok_or(WorkflowError::NumericOverflow)?;
            probe_candidates.push(missing_conversion_probe(start, target));
            continue;
        }
        let scan = |amount_in: AssetAmount| {
            find_best_conversion(
                &index,
                &ConversionRequest {
                    from_asset_id: start.start_asset_id.clone(),
                    to_asset_id: target.clone(),
                    amount_in,
                    max_hops: request.max_hops,
                    max_paths: request.max_paths_per_target,
                    max_expansions: per_target_budget,
                    alternative_limit: 0,
                    allowed_intermediate_asset_ids: Some(routable.clone()),
                    fee_policy: request.fee_policy,
                },
                cancellation,
            )
            .map_err(map_engine_error)
        };
        let mut result = scan(start.amount_in.clone())?;
        scanned_conversion_count = scanned_conversion_count
            .checked_add(1)
            .ok_or(WorkflowError::NumericOverflow)?;
        expansions_used = expansions_used.saturating_add(result.diagnostics.expanded_state_count);
        if result.diagnostics.truncated {
            budget_exhausted = true;
        }
        // A stake below the pair's own minimum lot buys nothing at all: units
        // are whole, and ten chaos against a divine at eleven chaos rounds to
        // zero. The search then reports no path, which is true of that size
        // and false of the market — and reporting it as a missing quote sent
        // the user to capture a pair whose quotes were already on file and
        // fresh. Ask what would be enough and scan that instead; the row's own
        // `amount_in` and `StakeRaisedToMinimum` say which size answered.
        let mut stake_raised = false;
        if result.best_path.is_none() {
            let minimum = index
                .minimum_input_for_one_unit(&start.start_asset_id, target, request.fee_policy)
                .map_err(map_engine_error)?;
            if let Some(minimum) = minimum
                && minimum.quanta > start.amount_in.quanta
            {
                let retried = scan(minimum)?;
                expansions_used =
                    expansions_used.saturating_add(retried.diagnostics.expanded_state_count);
                if retried.diagnostics.truncated {
                    budget_exhausted = true;
                }
                if retried.best_path.is_some() {
                    stake_raised = true;
                    result = retried;
                }
            }
        }
        let Some(best) = result.best_path else {
            // Still nothing. Whether that is a gap in the data or a stake the
            // market cannot trade at is not something the search says — it
            // returns no path for both — so ask the graph. Priced pairs that
            // chain from here to the target mean the book knows the way and
            // only the size was wrong; no chain means nobody has captured it,
            // which is the one case worth sending the user to the panel for.
            if index.is_priced_route(
                &start.start_asset_id,
                target,
                Some(&routable),
                request.max_hops,
            ) {
                unfillable_conversion_count = unfillable_conversion_count
                    .checked_add(1)
                    .ok_or(WorkflowError::NumericOverflow)?;
            } else {
                missing_conversion_count = missing_conversion_count
                    .checked_add(1)
                    .ok_or(WorkflowError::NumericOverflow)?;
                probe_candidates.push(missing_conversion_probe(start, target));
            }
            continue;
        };
        if !best.is_fully_filled {
            missing_conversion_count = missing_conversion_count
                .checked_add(1)
                .ok_or(WorkflowError::NumericOverflow)?;
            probe_candidates.push(confirm_conversion_probe(start, target, &best));
            continue;
        }
        complete_conversion_count = complete_conversion_count
            .checked_add(1)
            .ok_or(WorkflowError::NumericOverflow)?;
        let should_include = match result.comparison.status {
            ConversionComparisonStatus::ComparableGross
            | ConversionComparisonStatus::ComparableNet => {
                result.comparison.direction == Some(ComparisonDirection::Improved)
                    && result.comparison.basis_points.is_some_and(|basis_points| {
                        basis_points
                            >= i64::from(request.minimum_conversion_improvement_basis_points)
                    })
            }
            ConversionComparisonStatus::NoDirectPath => true,
            ConversionComparisonStatus::IncomparableCoverage
            | ConversionComparisonStatus::NoPath => false,
        };
        if should_include {
            let capacity = anchor.as_ref().and_then(|anchor| {
                anchor_capacity(&index, &best.path_asset_ids, anchor, request.fee_policy)
            });
            items.push(conversion_item(
                items.len(),
                best,
                result.comparison.status,
                result.comparison.basis_points,
                request.thresholds,
                stake_raised,
                capacity,
            ));
        }
    }

    ensure_not_cancelled(cancellation)?;
    progress(RadarProgress {
        stage: RadarStage::TriangleScan,
        completed_units: scanned_conversion_count,
        total_units,
    });
    // One cycle scan per settlement start, the evaluation budget split
    // equally. The same physical loop reached from two starts is one
    // opportunity entered at different points, so cycles deduplicate by
    // their rotation-invariant key and the better entry wins.
    let per_start_evaluations =
        (request.max_triangle_evaluations / count(request.starts.len())?.max(1)).max(1);
    let mut triangles_truncated = false;
    let mut triangle_evaluation_count = 0_u32;
    let mut best_by_cycle: BTreeMap<String, TriangleEvaluation> = BTreeMap::new();
    for start in &request.starts {
        ensure_not_cancelled(cancellation)?;
        let triangles = find_triangle_opportunities(
            &index,
            &TriangleRequest {
                start_asset_id: start.start_asset_id.clone(),
                // Each cycle takes its own size from its thinnest leg.
                amount_in: None,
                minimum_profit_basis_points: request.minimum_triangle_profit_basis_points,
                max_results: request.max_results,
                max_evaluations: per_start_evaluations,
                fee_policy: request.fee_policy,
            },
            cancellation,
        )
        .map_err(map_engine_error)?;
        triangles_truncated |= triangles.diagnostics.truncated;
        triangle_evaluation_count =
            triangle_evaluation_count.saturating_add(triangles.diagnostics.evaluated_cycle_count);
        for triangle in triangles.opportunities {
            let cycle: Vec<MarketAssetId> = triangle
                .steps
                .iter()
                .map(|step| step.from_asset_id.clone())
                .collect();
            let key = canonical_cycle_key(&cycle);
            let better = best_by_cycle
                .get(&key)
                .is_none_or(|existing| triangle.edge_basis_points() > existing.edge_basis_points());
            if better {
                best_by_cycle.insert(key, triangle);
            }
        }
    }
    for triangle in best_by_cycle.into_values() {
        if !triangle.execution_eligible {
            for step in &triangle.steps {
                probe_candidates.push(ProbeCandidate {
                    from_asset_id: step.from_asset_id.clone(),
                    to_asset_id: step.to_asset_id.clone(),
                    reason: ProbeReason::OpportunityConfirmation,
                    source: ProbeSource::OpportunityRadar,
                    priority: ProbePriority::High,
                    related_focus_group_id: None,
                    last_seen_at: None,
                    freshness_status: None,
                    expected_value_hint: triangle
                        .profit_basis_points
                        .map(|value| format!("triangle theory {value} bps")),
                    notes: Some("theoretical triangle requires current confirmation".to_owned()),
                });
            }
        }
        let capacity = anchor.as_ref().and_then(|anchor| {
            anchor_capacity(
                &index,
                &triangle.cycle_asset_ids,
                anchor,
                request.fee_policy,
            )
        });
        items.push(triangle_item(
            items.len(),
            triangle,
            request.thresholds,
            capacity,
        ));
    }
    progress(RadarProgress {
        stage: RadarStage::Ranking,
        completed_units: total_units,
        total_units,
    });
    items.sort_by(compare_items);
    let item_count_before_limit = count(items.len())?;
    let result_limit = usize::from(request.max_results);
    let results_truncated = items.len() > result_limit;
    if triangles_truncated {
        budget_exhausted = true;
    }
    let skipped_target_count = count(pairs.len())?.saturating_sub(scanned_conversion_count);
    items.truncate(result_limit);
    deduplicate_probe_candidates(&mut probe_candidates);
    progress(RadarProgress {
        stage: RadarStage::Complete,
        completed_units: total_units,
        total_units,
    });
    Ok(RadarResult {
        context_key: request.context_key.clone(),
        analysis_policy: selection.policy.clone(),
        starts: request.starts.clone(),
        items,
        probe_candidates,
        diagnostics: RadarDiagnostics {
            target_count: count(pairs.len())?,
            scanned_conversion_count,
            complete_conversion_count,
            missing_conversion_count,
            unfillable_conversion_count,
            triangle_evaluation_count,
            item_count_before_limit,
            expansions_used,
            skipped_target_count,
            budget_exhausted,
            results_truncated,
        },
    })
}

fn validate_request(
    selection: &QuoteSelectionResult,
    units: &AssetUnitCatalog,
    scope: &FocusScope,
    request: &RadarRequest,
) -> Result<(), WorkflowError> {
    request
        .fee_policy
        .validate()
        .map_err(|_| WorkflowError::InvalidRequest)?;
    if selection.policy.strategy != QuoteSelectionStrategy::Instant
        || selection.context_key != request.context_key
        || request.context_key.trim().is_empty()
        || scope.status != FocusScopeStatus::Ready
        || request.starts.is_empty()
        || request.minimum_conversion_improvement_basis_points > 1_000_000
        || request.minimum_triangle_profit_basis_points > 1_000_000
        || !(1..=4).contains(&request.max_hops)
        || request.max_paths_per_target == 0
        || request.max_paths_per_target > 10_000
        || request.max_expansions_per_target == 0
        || request.max_expansions_per_target > 1_000_000
        || request.max_triangle_evaluations == 0
        || request.max_triangle_evaluations > 1_000_000
        || request.max_results == 0
        || request.max_results > 500
    {
        return Err(WorkflowError::InvalidRequest);
    }
    let mut seen_starts = std::collections::BTreeSet::new();
    for start in &request.starts {
        if !scope.endpoint_asset_ids.contains(&start.start_asset_id)
            || start.amount_in.asset_id != start.start_asset_id
            || start.amount_in.quanta == 0
            || start.amount_in.unit
                != units
                    .unit(&start.start_asset_id)
                    .map_err(|_| WorkflowError::InvalidRequest)?
            || !seen_starts.insert(start.start_asset_id.clone())
        {
            return Err(WorkflowError::InvalidRequest);
        }
    }
    Ok(())
}

fn restrict_selection(
    selection: &QuoteSelectionResult,
    scope: &FocusScope,
) -> QuoteSelectionResult {
    let mut restricted = selection.clone();
    restricted
        .selections
        .retain(|candidate| scope.edge_allowed(&candidate.from_asset_id, &candidate.to_asset_id));
    restricted
}

/// A path's top-of-book depth, restated in the settlement anchor.
///
/// Depth is what decides between two profitable rows, so it has to be one
/// number in one currency. Measured in each path's own start asset it is not:
/// the same trade reads eleven times larger started from chaos than from
/// divine, and a list sorted on that ranks by unit rather than by market.
///
/// `None` when the chain or the conversion to the anchor cannot be formed,
/// which sorts last — an unknown depth is not a deep one.
fn anchor_capacity(
    index: &MarketDepthIndex,
    path: &[MarketAssetId],
    anchor: &MarketAssetId,
    fee_policy: FeePolicy,
) -> Option<u64> {
    let capacity = chain_rates(index, path, fee_policy)
        .ok()
        .flatten()?
        .top_of_book_capacity()?;
    let start = path.first()?;
    if start == anchor {
        return Some(capacity);
    }
    let (rate, _) = index.top_of_book(start, anchor)?;
    u64::try_from(
        u128::from(capacity).checked_mul(u128::from(rate.numerator))?
            / u128::from(rate.denominator).max(1),
    )
    .ok()
}

fn conversion_item(
    index: usize,
    path: ConversionPath,
    comparison_status: ConversionComparisonStatus,
    basis_points: Option<i64>,
    thresholds: RiskThresholds,
    stake_raised: bool,
    liquidity_capacity: Option<u64>,
) -> RadarItem {
    let assessment = assess_path(&path, thresholds, false);
    let mut reasons = Vec::new();
    if stake_raised {
        reasons.push(RadarReason::StakeRaisedToMinimum);
    }
    if matches!(
        comparison_status,
        ConversionComparisonStatus::ComparableGross | ConversionComparisonStatus::ComparableNet
    ) {
        reasons.push(RadarReason::BetterThanDirect);
    } else if comparison_status == ConversionComparisonStatus::NoDirectPath {
        reasons.push(RadarReason::NoDirectBaseline);
    }
    append_path_reasons(&path, &mut reasons);
    RadarItem {
        item_id: format!(
            "conversion-{index}-{}",
            path.path_asset_ids
                .iter()
                .map(MarketAssetId::as_str)
                .collect::<Vec<_>>()
                .join("-")
        ),
        kind: RadarItemKind::BestConversion,
        category: assessment.actionability,
        path_asset_ids: path.path_asset_ids.clone(),
        amount_in: path.requested_input.clone(),
        amount_out: path.amount_out.clone(),
        value_basis_points: basis_points,
        liquidity_capacity,
        reasons,
        risk_flags: path.risk_flags.clone(),
        blocking_risks: assessment.blocking(),
        conversion_path: Some(path),
        triangle: None,
    }
}

fn triangle_item(
    index: usize,
    triangle: TriangleEvaluation,
    thresholds: RiskThresholds,
    liquidity_capacity: Option<u64>,
) -> RadarItem {
    let assessment = assess_triangle(&triangle, thresholds, false);
    let mut reasons = vec![RadarReason::TriangleReturn];
    if !triangle.execution_eligible {
        reasons.push(RadarReason::GrossTheoryOnly);
    }
    if !triangle.residuals.is_empty() {
        reasons.push(RadarReason::ResidualInventory);
    }
    append_capture_skew_reasons(&triangle.risk_flags, &mut reasons);
    RadarItem {
        item_id: format!(
            "triangle-{index}-{}",
            triangle
                .cycle_asset_ids
                .iter()
                .map(MarketAssetId::as_str)
                .collect::<Vec<_>>()
                .join("-")
        ),
        kind: RadarItemKind::Triangle,
        category: assessment.actionability,
        path_asset_ids: triangle.cycle_asset_ids.clone(),
        amount_in: triangle.amount_in.clone(),
        amount_out: triangle.amount_out.clone(),
        // The exact rate-space edge, not the walked one: the walk floors
        // once per leg at whatever size the book allowed.
        value_basis_points: Some(triangle.edge_basis_points()),
        liquidity_capacity,
        reasons,
        risk_flags: triangle.risk_flags.clone(),
        blocking_risks: assessment.blocking(),
        conversion_path: None,
        triangle: Some(triangle),
    }
}

fn append_path_reasons(path: &ConversionPath, reasons: &mut Vec<RadarReason>) {
    if path.gross_only {
        reasons.push(RadarReason::GrossTheoryOnly);
    }
    if !path.residuals.is_empty() {
        reasons.push(RadarReason::ResidualInventory);
    }
    if path.risk_flags.contains(&ExecutionRiskFlag::MakerReference) {
        reasons.push(RadarReason::MakerReference);
    }
    if path
        .risk_flags
        .contains(&ExecutionRiskFlag::SearchTruncated)
    {
        reasons.push(RadarReason::SearchTruncated);
    }
    append_capture_skew_reasons(&path.risk_flags, reasons);
}

fn append_capture_skew_reasons(risks: &[ExecutionRiskFlag], reasons: &mut Vec<RadarReason>) {
    if risks.contains(&ExecutionRiskFlag::CaptureSkewUnverified) {
        reasons.push(RadarReason::CaptureSkewUnverified);
    }
    if risks.contains(&ExecutionRiskFlag::CaptureSkewExceeded) {
        reasons.push(RadarReason::CaptureSkewExceeded);
    }
}

fn missing_conversion_probe(start: &RadarStart, target: &MarketAssetId) -> ProbeCandidate {
    ProbeCandidate {
        from_asset_id: start.start_asset_id.clone(),
        to_asset_id: target.clone(),
        reason: ProbeReason::MissingForwardQuote,
        source: ProbeSource::OpportunityRadar,
        priority: ProbePriority::High,
        related_focus_group_id: None,
        last_seen_at: None,
        freshness_status: None,
        expected_value_hint: None,
        notes: Some("radar could not build a conversion path".to_owned()),
    }
}

fn confirm_conversion_probe(
    start: &RadarStart,
    target: &MarketAssetId,
    path: &ConversionPath,
) -> ProbeCandidate {
    ProbeCandidate {
        from_asset_id: start.start_asset_id.clone(),
        to_asset_id: target.clone(),
        reason: ProbeReason::OpportunityConfirmation,
        source: ProbeSource::OpportunityRadar,
        priority: ProbePriority::High,
        related_focus_group_id: None,
        last_seen_at: None,
        freshness_status: None,
        expected_value_hint: None,
        notes: Some(format!(
            "partial path with {} residual entries",
            path.residuals.len()
        )),
    }
}

fn deduplicate_probe_candidates(candidates: &mut Vec<ProbeCandidate>) {
    candidates.sort_by(|left, right| {
        left.from_asset_id
            .cmp(&right.from_asset_id)
            .then_with(|| left.to_asset_id.cmp(&right.to_asset_id))
            .then_with(|| left.reason.cmp(&right.reason))
            .then_with(|| left.priority.cmp(&right.priority))
    });
    candidates.dedup_by(|left, right| {
        left.from_asset_id == right.from_asset_id
            && left.to_asset_id == right.to_asset_id
            && left.reason == right.reason
            && left.source == right.source
    });
}

/// Ordering for the ladder in a list: cleanest verdicts first, suspicious
/// last. Local because `Actionability` deliberately does not implement `Ord`
/// — "worse risk" is a radar-sorting concern, not a property of the type.
const fn category_rank(value: Actionability) -> u8 {
    match value {
        Actionability::InstantExecutable => 0,
        Actionability::MakerTheoretical => 1,
        Actionability::ProbeRequired => 2,
        Actionability::SuspiciousOutlier => 3,
    }
}

/// Deepest first, then most profitable, within each actionability rung.
///
/// The order the user asked for, and the one that matches how the list gets
/// used: everything on it already clears the profit floor, so what decides
/// between two rows is how much of each can actually be done. A thin row with
/// a spectacular percentage is a rounding artefact waiting to happen; a deep
/// row with a modest one is the trade.
fn compare_items(left: &RadarItem, right: &RadarItem) -> Ordering {
    category_rank(left.category)
        .cmp(&category_rank(right.category))
        .then_with(|| {
            right
                .liquidity_capacity
                .unwrap_or(0)
                .cmp(&left.liquidity_capacity.unwrap_or(0))
        })
        .then_with(|| {
            right
                .value_basis_points
                .unwrap_or(i64::MIN)
                .cmp(&left.value_basis_points.unwrap_or(i64::MIN))
        })
        .then_with(|| right.amount_out.quanta.cmp(&left.amount_out.quanta))
        .then_with(|| left.path_asset_ids.cmp(&right.path_asset_ids))
        .then_with(|| left.item_id.cmp(&right.item_id))
}

fn ensure_not_cancelled(cancellation: &SearchCancellation) -> Result<(), WorkflowError> {
    if cancellation.is_cancelled() {
        Err(WorkflowError::Cancelled)
    } else {
        Ok(())
    }
}

const fn map_engine_error(error: EngineError) -> WorkflowError {
    match error {
        EngineError::Cancelled => WorkflowError::Cancelled,
        EngineError::NumericOverflow => WorkflowError::NumericOverflow,
        EngineError::InvalidAssetUnit
        | EngineError::InvalidAssetUnitCatalog
        | EngineError::AmountNotAligned
        | EngineError::NonPositiveAmount
        | EngineError::InvalidFeePolicy
        | EngineError::InvalidQuoteSelection
        | EngineError::InvalidAnalysisRequest
        | EngineError::InvalidSearchLimits => WorkflowError::InvalidMarketSelection,
    }
}

fn count(value: usize) -> Result<u32, WorkflowError> {
    u32::try_from(value).map_err(|_| WorkflowError::NumericOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_shared_equally_so_the_last_target_is_scanned_like_the_first() {
        // B-09: spending the budget first-come leaves the tail of the scope
        // unscanned while the report still claims it covered the scope.
        let budget = RadarBudget {
            max_total_expansions: 1_000,
            max_targets: 0,
        };
        assert_eq!(budget.per_target(4), 250);
        assert_eq!(
            budget.per_target(3),
            333,
            "floor, so the total is never exceeded"
        );
    }

    #[test]
    fn a_scope_larger_than_the_budget_still_gives_every_target_something() {
        let budget = RadarBudget {
            max_total_expansions: 10,
            max_targets: 0,
        };
        assert_eq!(
            budget.per_target(1_000),
            1,
            "a starved target gets a token scan rather than silently none"
        );
    }

    #[test]
    fn an_empty_scope_leaves_the_budget_whole() {
        let budget = RadarBudget::default();
        assert_eq!(
            budget.per_target(0),
            budget.max_total_expansions,
            "no targets means nothing to divide among"
        );
    }
}
