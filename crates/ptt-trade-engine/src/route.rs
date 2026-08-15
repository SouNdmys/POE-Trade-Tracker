use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use ptt_market_book::{QuoteSelectionPolicy, QuoteSelectionStrategy};
use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::depth::{
    CaptureTimeEvidence, ExecutionRiskFlag, FillKind, MarketDepthIndex, PairBottleneck, PairFill,
    apply_capture_skew_safety,
};
use crate::quantity::{AssetAmount, FeePolicy};

#[derive(Clone, Debug, Default)]
pub struct SearchCancellation(Arc<AtomicBool>);

impl SearchCancellation {
    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualAmount {
    pub after_step: u8,
    pub amount: AssetAmount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionPath {
    pub path_asset_ids: Vec<MarketAssetId>,
    pub requested_input: AssetAmount,
    pub amount_out: AssetAmount,
    pub gross_only: bool,
    pub steps: Vec<PairFill>,
    pub capture_time_evidence: Option<CaptureTimeEvidence>,
    pub residuals: Vec<ResidualAmount>,
    pub is_fully_filled: bool,
    pub execution_eligible: bool,
    pub bottleneck: Option<PairBottleneck>,
    pub risk_flags: Vec<ExecutionRiskFlag>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionComparisonStatus {
    ComparableGross,
    ComparableNet,
    NoDirectPath,
    IncomparableCoverage,
    NoPath,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonDirection {
    Improved,
    Equal,
    Worse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionComparison {
    pub status: ConversionComparisonStatus,
    pub direction: Option<ComparisonDirection>,
    pub delta: Option<AssetAmount>,
    pub basis_points: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionDiagnostics {
    pub indexed_pair_count: u32,
    pub expanded_state_count: u32,
    pub complete_path_count: u32,
    pub partial_path_count: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionRequest {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub amount_in: AssetAmount,
    pub max_hops: u8,
    pub max_paths: u32,
    pub max_expansions: u32,
    pub alternative_limit: u8,
    pub allowed_intermediate_asset_ids: Option<Vec<MarketAssetId>>,
    pub fee_policy: FeePolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionResult {
    pub context_key: String,
    pub strategy: QuoteSelectionStrategy,
    pub analysis_policy: QuoteSelectionPolicy,
    pub best_path: Option<ConversionPath>,
    pub direct_path: Option<ConversionPath>,
    pub alternatives: Vec<ConversionPath>,
    pub comparison: ConversionComparison,
    pub diagnostics: ConversionDiagnostics,
}

pub fn find_best_conversion(
    index: &MarketDepthIndex,
    request: &ConversionRequest,
    cancellation: &SearchCancellation,
) -> Result<ConversionResult, EngineError> {
    validate_request(index, request)?;
    let mut request = request.clone();
    request.fee_policy = index.effective_fee_policy(request.fee_policy);
    let mut search = PathSearch {
        index,
        request: &request,
        cancellation,
        paths: Vec::new(),
        expanded_states: 0,
        truncated: false,
    };
    search.visit(
        request.amount_in.clone(),
        vec![request.from_asset_id.clone()],
        BTreeSet::from([request.from_asset_id.clone()]),
        Vec::new(),
        Vec::new(),
        None,
    )?;
    if search.truncated {
        for path in &mut search.paths {
            if !path
                .risk_flags
                .contains(&ExecutionRiskFlag::SearchTruncated)
            {
                path.risk_flags.push(ExecutionRiskFlag::SearchTruncated);
                path.risk_flags.sort();
            }
        }
    }
    search.paths.sort_by(compare_paths);
    let direct_path = search
        .paths
        .iter()
        .find(|path| path.steps.len() == 1)
        .cloned();
    let best_path = search.paths.first().cloned();
    let alternatives = search
        .paths
        .iter()
        .skip(1)
        .take(usize::from(request.alternative_limit))
        .cloned()
        .collect();
    let comparison =
        compare_best_to_direct(best_path.as_ref(), direct_path.as_ref(), &request, index)?;
    let complete_path_count = u32::try_from(
        search
            .paths
            .iter()
            .filter(|path| path.is_fully_filled)
            .count(),
    )
    .map_err(|_| EngineError::NumericOverflow)?;
    let partial_path_count = u32::try_from(
        search
            .paths
            .iter()
            .filter(|path| !path.is_fully_filled)
            .count(),
    )
    .map_err(|_| EngineError::NumericOverflow)?;
    Ok(ConversionResult {
        context_key: index.context_key().to_owned(),
        strategy: index.strategy(),
        analysis_policy: index.analysis_policy().clone(),
        best_path,
        direct_path,
        alternatives,
        comparison,
        diagnostics: ConversionDiagnostics {
            indexed_pair_count: u32::try_from(index.pair_count())
                .map_err(|_| EngineError::NumericOverflow)?,
            expanded_state_count: search.expanded_states,
            complete_path_count,
            partial_path_count,
            truncated: search.truncated,
        },
    })
}

fn validate_request(
    index: &MarketDepthIndex,
    request: &ConversionRequest,
) -> Result<(), EngineError> {
    request.fee_policy.validate()?;
    if request.from_asset_id == request.to_asset_id
        || request.amount_in.asset_id != request.from_asset_id
        || request.amount_in.quanta == 0
        || request.amount_in.unit != index.units().unit(&request.from_asset_id)?
        || !index.units().contains(&request.to_asset_id)
    {
        return Err(EngineError::InvalidAnalysisRequest);
    }
    if !(1..=4).contains(&request.max_hops)
        || request.max_paths == 0
        || request.max_paths > 10_000
        || request.max_expansions == 0
        || request.max_expansions > 1_000_000
        || request.alternative_limit > 50
        || request
            .allowed_intermediate_asset_ids
            .as_ref()
            .is_some_and(|assets| {
                let unique = assets.iter().collect::<BTreeSet<_>>();
                unique.len() != assets.len()
                    || assets
                        .iter()
                        .any(|asset_id| !index.units().contains(asset_id))
            })
    {
        return Err(EngineError::InvalidSearchLimits);
    }
    Ok(())
}

struct PathSearch<'a> {
    index: &'a MarketDepthIndex,
    request: &'a ConversionRequest,
    cancellation: &'a SearchCancellation,
    paths: Vec<ConversionPath>,
    expanded_states: u32,
    truncated: bool,
}

impl PathSearch<'_> {
    #[allow(clippy::too_many_arguments)]
    fn visit(
        &mut self,
        current_amount: AssetAmount,
        path_assets: Vec<MarketAssetId>,
        visited: BTreeSet<MarketAssetId>,
        steps: Vec<PairFill>,
        residuals: Vec<ResidualAmount>,
        bottleneck: Option<PairBottleneck>,
    ) -> Result<(), EngineError> {
        if self.cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if steps.len() >= usize::from(self.request.max_hops) {
            return Ok(());
        }
        if self.expanded_states >= self.request.max_expansions
            || self.paths.len()
                >= usize::try_from(self.request.max_paths)
                    .map_err(|_| EngineError::NumericOverflow)?
        {
            self.truncated = true;
            return Ok(());
        }
        self.expanded_states = self
            .expanded_states
            .checked_add(1)
            .ok_or(EngineError::NumericOverflow)?;

        for neighbor in self.index.neighbors(&current_amount.asset_id) {
            if self.cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if self.truncated || visited.contains(&neighbor) {
                continue;
            }
            if neighbor != self.request.to_asset_id
                && self
                    .request
                    .allowed_intermediate_asset_ids
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&neighbor))
            {
                continue;
            }
            let Some(fill) =
                self.index
                    .fill_pair(&current_amount, &neighbor, self.request.fee_policy)?
            else {
                continue;
            };
            let propagated = fill
                .net_amount_out
                .clone()
                .unwrap_or_else(|| fill.gross_amount_out.clone());
            if propagated.quanta == 0 {
                continue;
            }
            let mut next_steps = steps.clone();
            next_steps.push(fill.clone());
            let mut next_assets = path_assets.clone();
            next_assets.push(neighbor.clone());
            let mut next_residuals = residuals.clone();
            if fill.unfilled_input.quanta > 0 {
                next_residuals.push(ResidualAmount {
                    after_step: u8::try_from(next_steps.len())
                        .map_err(|_| EngineError::NumericOverflow)?,
                    amount: fill.unfilled_input.clone(),
                });
            }
            let next_bottleneck = bottleneck.clone().or(fill.bottleneck.clone());
            if neighbor == self.request.to_asset_id {
                self.paths.push(make_path(
                    self.index,
                    self.request,
                    next_assets,
                    propagated,
                    next_steps,
                    next_residuals,
                    next_bottleneck,
                ));
                if self.paths.len()
                    >= usize::try_from(self.request.max_paths)
                        .map_err(|_| EngineError::NumericOverflow)?
                {
                    self.truncated = true;
                    break;
                }
                continue;
            }
            let mut next_visited = visited.clone();
            next_visited.insert(neighbor);
            self.visit(
                propagated,
                next_assets,
                next_visited,
                next_steps,
                next_residuals,
                next_bottleneck,
            )?;
        }
        Ok(())
    }
}

fn make_path(
    index: &MarketDepthIndex,
    request: &ConversionRequest,
    path_asset_ids: Vec<MarketAssetId>,
    amount_out: AssetAmount,
    steps: Vec<PairFill>,
    residuals: Vec<ResidualAmount>,
    bottleneck: Option<PairBottleneck>,
) -> ConversionPath {
    let is_fully_filled = steps.iter().all(|step| step.is_fully_filled);
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
    ConversionPath {
        path_asset_ids,
        requested_input: request.amount_in.clone(),
        amount_out,
        gross_only: request.fee_policy.is_unknown(),
        steps,
        capture_time_evidence,
        residuals,
        is_fully_filled,
        execution_eligible,
        bottleneck,
        risk_flags: risks.into_iter().collect(),
    }
}

fn compare_paths(left: &ConversionPath, right: &ConversionPath) -> Ordering {
    right
        .is_fully_filled
        .cmp(&left.is_fully_filled)
        .then_with(|| right.execution_eligible.cmp(&left.execution_eligible))
        .then_with(|| right.amount_out.quanta.cmp(&left.amount_out.quanta))
        .then_with(|| left.residuals.len().cmp(&right.residuals.len()))
        .then_with(|| left.steps.len().cmp(&right.steps.len()))
        .then_with(|| left.path_asset_ids.cmp(&right.path_asset_ids))
}

fn compare_best_to_direct(
    best: Option<&ConversionPath>,
    direct: Option<&ConversionPath>,
    request: &ConversionRequest,
    index: &MarketDepthIndex,
) -> Result<ConversionComparison, EngineError> {
    let Some(best) = best else {
        return Ok(ConversionComparison {
            status: ConversionComparisonStatus::NoPath,
            direction: None,
            delta: None,
            basis_points: None,
        });
    };
    let Some(direct) = direct else {
        return Ok(ConversionComparison {
            status: ConversionComparisonStatus::NoDirectPath,
            direction: None,
            delta: None,
            basis_points: None,
        });
    };
    if !best.is_fully_filled || !direct.is_fully_filled || direct.amount_out.quanta == 0 {
        return Ok(ConversionComparison {
            status: ConversionComparisonStatus::IncomparableCoverage,
            direction: None,
            delta: None,
            basis_points: None,
        });
    }
    let (direction, magnitude) = match best.amount_out.quanta.cmp(&direct.amount_out.quanta) {
        Ordering::Greater => (
            ComparisonDirection::Improved,
            best.amount_out.quanta - direct.amount_out.quanta,
        ),
        Ordering::Equal => (ComparisonDirection::Equal, 0),
        Ordering::Less => (
            ComparisonDirection::Worse,
            direct.amount_out.quanta - best.amount_out.quanta,
        ),
    };
    let signed_delta = match direction {
        ComparisonDirection::Improved => i128::from(magnitude),
        ComparisonDirection::Equal => 0,
        ComparisonDirection::Worse => -i128::from(magnitude),
    };
    let basis_points = signed_delta
        .checked_mul(10_000)
        .ok_or(EngineError::NumericOverflow)?
        / i128::from(direct.amount_out.quanta);
    Ok(ConversionComparison {
        status: if request.fee_policy.is_unknown() {
            ConversionComparisonStatus::ComparableGross
        } else {
            ConversionComparisonStatus::ComparableNet
        },
        direction: Some(direction),
        delta: Some(AssetAmount::from_quanta(
            request.to_asset_id.clone(),
            magnitude,
            index.units(),
        )?),
        basis_points: Some(i64::try_from(basis_points).map_err(|_| EngineError::NumericOverflow)?),
    })
}
