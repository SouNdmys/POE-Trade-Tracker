use std::cmp::Ordering;
use std::collections::BTreeSet;

use ptt_market_book::{QuoteSelectionPolicy, QuoteSelectionStrategy};
use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::depth::{
    CaptureTimeEvidence, ExecutionRiskFlag, FillKind, MarketDepthIndex, PairFill,
    apply_capture_skew_safety,
};
use crate::quantity::{AssetAmount, FeePolicy};
use crate::rate_space::RateChain;
use crate::route::{ComparisonDirection, ResidualAmount, SearchCancellation};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfitKind {
    GrossTheory,
    Net,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriangleEvaluationStatus {
    Profitable,
    BelowThreshold,
    PartialResidual,
    ZeroOutput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriangleEvaluation {
    pub cycle_asset_ids: Vec<MarketAssetId>,
    pub amount_in: AssetAmount,
    pub amount_out: AssetAmount,
    pub steps: Vec<PairFill>,
    pub capture_time_evidence: Option<CaptureTimeEvidence>,
    pub residuals: Vec<ResidualAmount>,
    pub is_fully_filled: bool,
    pub execution_eligible: bool,
    pub profit_kind: Option<ProfitKind>,
    pub profit_direction: Option<ComparisonDirection>,
    pub profit_amount: Option<AssetAmount>,
    pub profit_basis_points: Option<i64>,
    /// The cycle's rates multiplied out exactly, with no quantities involved.
    ///
    /// This, not `profit_basis_points`, is whether the edge exists: the
    /// quantity figure floors once per leg, so it reports a different answer
    /// at every size and none of them is the market's. See
    /// [`crate::rate_space`].
    pub rate_chain: Option<RateChain>,
    /// How much of the start asset the shown rates support, in whole units.
    /// A fact about the panel, never about the reader's holdings.
    pub liquidity_capacity: Option<u64>,
    pub status: TriangleEvaluationStatus,
    pub risk_flags: Vec<ExecutionRiskFlag>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriangleDiagnostics {
    pub indexed_pair_count: u32,
    pub evaluated_cycle_count: u32,
    pub profitable_cycle_count: u32,
    pub below_threshold_count: u32,
    pub partial_cycle_count: u32,
    pub zero_output_count: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriangleRequest {
    pub start_asset_id: MarketAssetId,
    /// How much to walk each cycle with, or `None` to let each cycle take
    /// its own size from the market.
    ///
    /// `None` is what a scanner wants. A shared size cannot fit currencies
    /// whose values differ by three orders of magnitude: pick it small and
    /// every cycle floors to nothing, pick it large and every cycle runs past
    /// the top of its book, and either way a number unrelated to the market
    /// decides what the market is said to contain. Sized from the book, each
    /// cycle is walked exactly as far as its own thinnest leg allows.
    pub amount_in: Option<AssetAmount>,
    pub minimum_profit_basis_points: u32,
    pub max_results: u16,
    pub max_evaluations: u32,
    pub fee_policy: FeePolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriangleResult {
    pub context_key: String,
    pub strategy: QuoteSelectionStrategy,
    pub analysis_policy: QuoteSelectionPolicy,
    pub opportunities: Vec<TriangleEvaluation>,
    pub rejections: Vec<TriangleEvaluation>,
    pub diagnostics: TriangleDiagnostics,
}

pub fn find_triangle_opportunities(
    index: &MarketDepthIndex,
    request: &TriangleRequest,
    cancellation: &SearchCancellation,
) -> Result<TriangleResult, EngineError> {
    validate_request(index, request)?;
    let mut request = request.clone();
    request.fee_policy = index.effective_fee_policy(request.fee_policy);
    let mut evaluations = Vec::new();
    let mut truncated = false;
    'outer: for middle_a in index.neighbors(&request.start_asset_id) {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if middle_a == request.start_asset_id {
            continue;
        }
        for middle_b in index.neighbors(&middle_a) {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if middle_b == request.start_asset_id || middle_b == middle_a {
                continue;
            }
            if !index.contains_pair(&middle_b, &request.start_asset_id) {
                continue;
            }
            if evaluations.len()
                >= usize::try_from(request.max_evaluations)
                    .map_err(|_| EngineError::NumericOverflow)?
            {
                truncated = true;
                break 'outer;
            }
            evaluations.push(evaluate_cycle(
                index,
                &request,
                [&request.start_asset_id, &middle_a, &middle_b],
            )?);
        }
    }

    let mut opportunities = evaluations
        .iter()
        .filter(|evaluation| evaluation.status == TriangleEvaluationStatus::Profitable)
        .cloned()
        .collect::<Vec<_>>();
    opportunities.sort_by(compare_evaluations);
    opportunities.truncate(usize::from(request.max_results));
    let mut rejections = evaluations
        .iter()
        .filter(|evaluation| evaluation.status != TriangleEvaluationStatus::Profitable)
        .cloned()
        .collect::<Vec<_>>();
    rejections.sort_by(compare_evaluations);
    rejections.truncate(usize::from(request.max_results));
    if truncated {
        for evaluation in opportunities.iter_mut().chain(rejections.iter_mut()) {
            if !evaluation
                .risk_flags
                .contains(&ExecutionRiskFlag::SearchTruncated)
            {
                evaluation
                    .risk_flags
                    .push(ExecutionRiskFlag::SearchTruncated);
                evaluation.risk_flags.sort();
            }
        }
    }
    Ok(TriangleResult {
        context_key: index.context_key().to_owned(),
        strategy: index.strategy(),
        analysis_policy: index.analysis_policy().clone(),
        opportunities,
        rejections,
        diagnostics: TriangleDiagnostics {
            indexed_pair_count: u32::try_from(index.pair_count())
                .map_err(|_| EngineError::NumericOverflow)?,
            evaluated_cycle_count: u32::try_from(evaluations.len())
                .map_err(|_| EngineError::NumericOverflow)?,
            profitable_cycle_count: count_status(
                &evaluations,
                TriangleEvaluationStatus::Profitable,
            )?,
            below_threshold_count: count_status(
                &evaluations,
                TriangleEvaluationStatus::BelowThreshold,
            )?,
            partial_cycle_count: count_status(
                &evaluations,
                TriangleEvaluationStatus::PartialResidual,
            )?,
            zero_output_count: count_status(&evaluations, TriangleEvaluationStatus::ZeroOutput)?,
            truncated,
        },
    })
}

fn validate_request(
    index: &MarketDepthIndex,
    request: &TriangleRequest,
) -> Result<(), EngineError> {
    request.fee_policy.validate()?;
    if let Some(amount_in) = &request.amount_in
        && (amount_in.asset_id != request.start_asset_id
            || amount_in.quanta == 0
            || amount_in.unit != index.units().unit(&request.start_asset_id)?)
    {
        return Err(EngineError::InvalidAnalysisRequest);
    }
    if request.minimum_profit_basis_points > 1_000_000
        || request.max_results == 0
        || request.max_results > 500
        || request.max_evaluations == 0
        || request.max_evaluations > 1_000_000
    {
        return Err(EngineError::InvalidSearchLimits);
    }
    Ok(())
}

impl TriangleEvaluation {
    /// The cycle's edge in basis points: the exact rate-space figure when the
    /// rates could be chained, and the walked figure otherwise.
    ///
    /// Rate space first because it is the answer to the question actually
    /// being asked. The walked number describes one particular size, and the
    /// flooring it carries is an artefact of that size rather than a property
    /// of the market.
    #[must_use]
    pub fn edge_basis_points(&self) -> i64 {
        self.rate_chain.as_ref().map_or_else(
            || self.profit_basis_points.unwrap_or(i64::MIN),
            |chain| chain.basis_points(),
        )
    }
}

fn evaluate_cycle(
    index: &MarketDepthIndex,
    request: &TriangleRequest,
    nodes: [&MarketAssetId; 3],
) -> Result<TriangleEvaluation, EngineError> {
    let cycle_asset_ids = vec![
        nodes[0].clone(),
        nodes[1].clone(),
        nodes[2].clone(),
        nodes[0].clone(),
    ];
    // The rates first, exactly, with nothing to do with size — this is the
    // verdict. The walk below only puts numbers and risks on it.
    let rate_chain = crate::rate_space::chain_rates(index, &cycle_asset_ids, request.fee_policy)?;
    let liquidity_capacity = rate_chain
        .as_ref()
        .and_then(crate::rate_space::RateChain::top_of_book_capacity);
    // Sized from the book when the caller named no size: as far as the
    // thinnest leg goes and no further, so the walk stays on the rates the
    // verdict was formed from.
    let amount_in = match &request.amount_in {
        Some(amount_in) => amount_in.clone(),
        None => AssetAmount::from_whole_units(
            request.start_asset_id.clone(),
            liquidity_capacity.unwrap_or(0).max(1),
            index.units(),
        )?,
    };
    let mut current = amount_in.clone();
    let mut steps = Vec::with_capacity(3);
    let mut residuals = Vec::new();
    for (index_of_step, target) in [nodes[1], nodes[2], nodes[0]].into_iter().enumerate() {
        let fill = index
            .fill_pair(&current, target, request.fee_policy)?
            .ok_or(EngineError::InvalidQuoteSelection)?;
        if fill.unfilled_input.quanta > 0 {
            residuals.push(ResidualAmount {
                after_step: u8::try_from(index_of_step + 1)
                    .map_err(|_| EngineError::NumericOverflow)?,
                amount: fill.unfilled_input.clone(),
            });
        }
        current = fill
            .net_amount_out
            .clone()
            .unwrap_or_else(|| fill.gross_amount_out.clone());
        steps.push(fill);
        if current.quanta == 0 {
            break;
        }
    }
    let is_fully_filled = steps.len() == 3 && steps.iter().all(|step| step.is_fully_filled);
    let mut risks = steps
        .iter()
        .flat_map(|step| step.risk_flags.iter().copied())
        .collect::<BTreeSet<_>>();
    if !is_fully_filled {
        risks.insert(ExecutionRiskFlag::PartialRoute);
    }
    if !residuals.is_empty() {
        risks.insert(ExecutionRiskFlag::ResidualInventory);
    }
    if steps
        .iter()
        .filter(|step| step.kind == FillKind::MakerTheory)
        .count()
        > 1
    {
        risks.insert(ExecutionRiskFlag::MultiHopMaker);
    }
    let capture_time_evidence = CaptureTimeEvidence::from_pair_fills(
        &steps,
        index
            .analysis_policy()
            .capture_skew
            .max_capture_skew_seconds,
        index.analysis_policy().capture_skew.calibration_status,
    );
    let capture_skew_allows_execution =
        apply_capture_skew_safety(&mut risks, capture_time_evidence, steps.len());
    let execution_eligible = index.strategy() == QuoteSelectionStrategy::Instant
        && index.analysis_policy().product_execution_allowed
        && !request.fee_policy.is_unknown()
        && is_fully_filled
        && capture_skew_allows_execution
        && steps.iter().all(|step| step.execution_eligible);

    let (status, profit_kind, profit_direction, profit_amount, profit_basis_points) =
        if current.quanta == 0 {
            (TriangleEvaluationStatus::ZeroOutput, None, None, None, None)
        } else if !is_fully_filled {
            (
                TriangleEvaluationStatus::PartialResidual,
                None,
                None,
                None,
                None,
            )
        } else {
            let (direction, magnitude) = match current.quanta.cmp(&amount_in.quanta) {
                Ordering::Greater => (
                    ComparisonDirection::Improved,
                    current.quanta - amount_in.quanta,
                ),
                Ordering::Equal => (ComparisonDirection::Equal, 0),
                Ordering::Less => (
                    ComparisonDirection::Worse,
                    amount_in.quanta - current.quanta,
                ),
            };
            let signed = match direction {
                ComparisonDirection::Improved => i128::from(magnitude),
                ComparisonDirection::Equal => 0,
                ComparisonDirection::Worse => -i128::from(magnitude),
            };
            let basis_points = signed
                .checked_mul(10_000)
                .ok_or(EngineError::NumericOverflow)?
                / i128::from(amount_in.quanta);
            let basis_points =
                i64::try_from(basis_points).map_err(|_| EngineError::NumericOverflow)?;
            // Judged on the rates when they could be chained, because that is
            // the market's own answer: the walked figure below carries one
            // floor per leg, and at a size the caller chose rather than the
            // book, so it says as much about the size as about the edge.
            let edge = rate_chain
                .as_ref()
                .map_or(basis_points, RateChain::basis_points);
            let improved = rate_chain.as_ref().map_or(
                direction == ComparisonDirection::Improved,
                RateChain::is_profitable,
            );
            let status = if improved && edge >= i64::from(request.minimum_profit_basis_points) {
                TriangleEvaluationStatus::Profitable
            } else {
                TriangleEvaluationStatus::BelowThreshold
            };
            (
                status,
                Some(if request.fee_policy.is_unknown() {
                    ProfitKind::GrossTheory
                } else {
                    ProfitKind::Net
                }),
                Some(direction),
                Some(AssetAmount::from_quanta(
                    request.start_asset_id.clone(),
                    magnitude,
                    index.units(),
                )?),
                Some(basis_points),
            )
        };

    Ok(TriangleEvaluation {
        cycle_asset_ids,
        amount_in: amount_in.clone(),
        amount_out: current,
        steps,
        capture_time_evidence,
        residuals,
        is_fully_filled,
        execution_eligible,
        profit_kind,
        profit_direction,
        profit_amount,
        profit_basis_points,
        rate_chain,
        liquidity_capacity,
        status,
        risk_flags: risks.into_iter().collect(),
    })
}

/// Deepest first, then most profitable.
///
/// Liquidity outranks profit deliberately: every cycle on this list already
/// clears the profit floor, so the one worth doing is the one that can be
/// done at size. A tenth of a percent across a thousand units beats fifty
/// percent across two, and ordering by profit alone buries the first under
/// the second.
fn compare_evaluations(left: &TriangleEvaluation, right: &TriangleEvaluation) -> Ordering {
    right
        .execution_eligible
        .cmp(&left.execution_eligible)
        .then_with(|| {
            right
                .liquidity_capacity
                .unwrap_or(0)
                .cmp(&left.liquidity_capacity.unwrap_or(0))
        })
        .then_with(|| right.edge_basis_points().cmp(&left.edge_basis_points()))
        .then_with(|| right.amount_out.quanta.cmp(&left.amount_out.quanta))
        .then_with(|| left.cycle_asset_ids.cmp(&right.cycle_asset_ids))
}

fn count_status(
    evaluations: &[TriangleEvaluation],
    status: TriangleEvaluationStatus,
) -> Result<u32, EngineError> {
    u32::try_from(
        evaluations
            .iter()
            .filter(|evaluation| evaluation.status == status)
            .count(),
    )
    .map_err(|_| EngineError::NumericOverflow)
}

/// F4: canonical identity of a cycle regardless of which asset the search
/// started from — rotate so the lexicographically smallest asset id leads.
/// Radar-level enumeration over many starts dedupes on this key, eliminating
/// the rotation-duplicate class (POE2 audit B-06) by construction.
#[must_use]
pub fn canonical_cycle_key(cycle: &[MarketAssetId]) -> String {
    if cycle.is_empty() {
        return String::new();
    }
    let pivot = cycle
        .iter()
        .enumerate()
        .min_by(|left, right| left.1.cmp(right.1))
        .map(|(index, _)| index)
        .unwrap_or(0);
    let mut parts = Vec::with_capacity(cycle.len());
    for offset in 0..cycle.len() {
        parts.push(cycle[(pivot + offset) % cycle.len()].to_string());
    }
    parts.join(">")
}

#[cfg(test)]
mod canonical_cycle_tests {
    use super::*;

    fn id(text: &str) -> MarketAssetId {
        MarketAssetId::try_new(text).expect("asset id")
    }

    #[test]
    fn f4_rotations_share_one_canonical_key() {
        let rotation_a = [id("divine"), id("exalted"), id("chaos")];
        let rotation_b = [id("exalted"), id("chaos"), id("divine")];
        let rotation_c = [id("chaos"), id("divine"), id("exalted")];
        let key = canonical_cycle_key(&rotation_a);
        assert_eq!(key, canonical_cycle_key(&rotation_b));
        assert_eq!(key, canonical_cycle_key(&rotation_c));
        assert!(key.starts_with("chaos>"));

        let different = [id("chaos"), id("exalted"), id("divine")];
        assert_ne!(key, canonical_cycle_key(&different), "direction matters");
    }
}
