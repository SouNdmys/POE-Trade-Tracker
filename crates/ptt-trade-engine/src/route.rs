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

/// Coverage first, then executability, then the price the route actually got.
///
/// The price key used to be raw `amount_out`, which pays a route for burning
/// capital: on 2026-08-23 a detour that ate 2,526 divine for 21,786 chaos
/// outranked a direct fill that ate 1,855 for 20,030, even though the direct
/// fill priced 25% better and the detour was 3,451 chaos behind at mark.
/// Absolute output only means something when both routes spent the same
/// input, and two partial fills never do.
///
/// Routes that swallow the whole stake all spent exactly the requested
/// amount, so for them the ratio is the old key over a shared denominator
/// and their order is unchanged -- there is a test below holding that half
/// of the ranking still.
fn compare_paths(left: &ConversionPath, right: &ConversionPath) -> Ordering {
    right
        .is_fully_filled
        .cmp(&left.is_fully_filled)
        .then_with(|| right.execution_eligible.cmp(&left.execution_eligible))
        .then_with(|| compare_realized_price(right, left))
        .then_with(|| left.residuals.len().cmp(&right.residuals.len()))
        .then_with(|| left.steps.len().cmp(&right.steps.len()))
        .then_with(|| left.path_asset_ids.cmp(&right.path_asset_ids))
}

/// `out_left / spent_left` against `out_right / spent_right`, ascending.
///
/// Cross-multiplied rather than divided: 8.3 keeps f64 at the drawing edge,
/// and integer division would round the two ratios into a tie the market
/// never had. Widening to `u128` makes the multiply total -- the square of a
/// `u64` always fits -- so there is no overflow branch to saturate here.
///
/// A route that spent nothing compares as infinitely good, which is the
/// right answer and also unreachable: a path is only recorded once a leg
/// produced output.
fn compare_realized_price(left: &ConversionPath, right: &ConversionPath) -> Ordering {
    let left_scaled = u128::from(left.amount_out.quanta) * u128::from(consumed_input_quanta(right));
    let right_scaled =
        u128::from(right.amount_out.quanta) * u128::from(consumed_input_quanta(left));
    left_scaled.cmp(&right_scaled)
}

/// How much of the starting asset the route actually spent.
///
/// The first leg answers it on its own: every later leg spends what the
/// first one bought, so the requested amount minus the first leg's leftover
/// is the whole bill in the asset the user is holding.
fn consumed_input_quanta(path: &ConversionPath) -> u64 {
    path.steps
        .first()
        .map_or(path.requested_input.quanta, |step| {
            step.consumed_input.quanta
        })
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

#[cfg(test)]
mod compare_paths_tests {
    use super::*;
    use crate::depth::QuoteLevelFill;
    use crate::quantity::AssetUnit;

    /// The stake from the 2026-08-23 field test: 5,000 divine offered, and
    /// neither route could eat all of it.
    const STAKE: u64 = 5_000;

    fn asset(name: &str) -> MarketAssetId {
        MarketAssetId::try_new(name).expect("asset id")
    }

    fn amount(name: &str, quanta: u64) -> AssetAmount {
        AssetAmount {
            asset_id: asset(name),
            quanta,
            unit: AssetUnit::whole(),
        }
    }

    fn leg(from: &str, to: &str, requested: u64, consumed: u64, produced: u64) -> PairFill {
        PairFill {
            from_asset_id: asset(from),
            to_asset_id: asset(to),
            kind: FillKind::TakerDepth,
            requested_input: amount(from, requested),
            consumed_input: amount(from, consumed),
            unfilled_input: amount(from, requested - consumed),
            gross_amount_out: amount(to, produced),
            net_amount_out: None,
            fills: Vec::<QuoteLevelFill>::new(),
            capture_time_evidence: None,
            is_fully_filled: consumed == requested,
            execution_eligible: false,
            bottleneck: None,
            risk_flags: Vec::new(),
        }
    }

    fn path(assets: &[&str], steps: Vec<PairFill>, amount_out: u64) -> ConversionPath {
        let is_fully_filled = steps.iter().all(|step| step.is_fully_filled);
        let residuals = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.unfilled_input.quanta > 0)
            .map(|(position, step)| ResidualAmount {
                after_step: u8::try_from(position + 1).expect("short fixture"),
                amount: step.unfilled_input.clone(),
            })
            .collect();
        ConversionPath {
            path_asset_ids: assets.iter().map(|name| asset(name)).collect(),
            requested_input: amount(assets[0], STAKE),
            amount_out: amount(assets[assets.len() - 1], amount_out),
            gross_only: true,
            steps,
            capture_time_evidence: None,
            residuals,
            is_fully_filled,
            execution_eligible: false,
            bottleneck: None,
            risk_flags: Vec::new(),
        }
    }

    /// The 2026-08-23 convert page, numbers straight off the live book.
    ///
    /// Direct eats 1,855 of the 5,000 divine and returns 20,030 chaos
    /// (10.80 each); the detour eats 2,526 and returns 21,786 (8.62 each).
    /// The detour is the worse trade in every sense that matters and wins on
    /// absolute output only because it burns more capital.
    #[test]
    fn a_partial_route_that_burns_more_capital_for_a_worse_price_does_not_win() {
        let direct = path(
            &["divine", "chaos"],
            vec![leg("divine", "chaos", STAKE, 1_855, 20_030)],
            20_030,
        );
        let detour = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, 2_526, 50_000),
                leg("exalted", "chaos", 50_000, 50_000, 21_786),
            ],
            21_786,
        );

        let mut paths = vec![detour, direct];
        paths.sort_by(compare_paths);

        assert_eq!(
            paths[0].path_asset_ids,
            vec![asset("divine"), asset("chaos")],
            "the cheaper realized price has to come first"
        );
    }

    /// Guard on the half of the order that must not move: when both routes
    /// swallow the whole stake they spent the same input, so the deeper
    /// payout still wins and nothing about today's ranking changes.
    #[test]
    fn two_fully_filled_routes_still_rank_by_absolute_output() {
        let direct = path(
            &["divine", "chaos"],
            vec![leg("divine", "chaos", STAKE, STAKE, 20_030)],
            20_030,
        );
        let detour = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, STAKE, 50_000),
                leg("exalted", "chaos", 50_000, 50_000, 21_786),
            ],
            21_786,
        );

        let mut paths = vec![direct, detour];
        paths.sort_by(compare_paths);

        assert_eq!(
            paths[0].path_asset_ids,
            vec![asset("divine"), asset("exalted"), asset("chaos")],
            "fully filled routes keep ranking on how much they return"
        );
    }
}
