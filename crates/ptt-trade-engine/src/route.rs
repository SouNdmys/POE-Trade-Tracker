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

/// Executability, liquidity risk, then the price the route got.
///
/// The order the ruling asks for (CORE-TRADING-MODEL, "兑换路线排序"), and
/// the same one section 5 already binds the radar to: liquidity outranks the
/// rate, and size is not a key at all. Size is not a key because a route is a
/// rate, not a parcel -- POE fills against listings at your rate or better,
/// so a route that eats 10 at 11.0 can simply be run at 4,000 and beats one
/// that eats 4,000 at 10.9. What a smaller order cannot dodge is a leg with
/// nobody on the other side, so that question is asked first.
///
/// Coverage (`is_fully_filled`) led this list until 2026-08-23 and was
/// defended as a coarse liquidity gate. It is not one: its ruler is the
/// number the user typed, not anything about the market. One book, two
/// clean routes, a fixed rate on each -- and the winner changes when the
/// request goes from 10 to 5,000, because the 11.0 route stops swallowing
/// the whole stake while the 10.0 route still does. Flipping on the size of
/// the ask is the definition of a size key, and it also contradicted the
/// stranding key below, which argues at length that coming up short on the
/// first leg traps nothing at all.
///
/// The price key used to be raw `amount_out`, which pays a route for burning
/// capital: on 2026-08-23 a detour that ate 2,526 divine for 21,786 chaos
/// outranked a direct fill that ate 1,855 for 20,030, even though the direct
/// fill priced 25% better and the detour was 3,451 chaos behind at mark.
/// Absolute output only means something when both routes spent the same
/// input, and two partial fills never do.
///
/// `execution_eligible` survives the cut, but only on a technicality worth
/// stating plainly: `make_path` builds it out of "swallowed the whole
/// request" too, so it is the same size gate wearing another name -- flip
/// `product_execution_allowed` on and `complete_route_wins_over_a_higher_
/// output_partial_quote` (tests/engine.rs) shows a rate ten times worse
/// winning again. What keeps it harmless is that no shipped policy ever
/// sets that flag, so the field is `false` on every path the app can
/// build and a constant key reorders nothing. That is a contingency, not
/// a proof, so the test below pins the constant rather than trusting it.
fn compare_paths(left: &ConversionPath, right: &ConversionPath) -> Ordering {
    right
        .execution_eligible
        .cmp(&left.execution_eligible)
        .then_with(|| compare_shares(worst_stranded_share(left), worst_stranded_share(right)))
        .then_with(|| compare_realized_price(right, left))
        .then_with(|| left.residuals.len().cmp(&right.residuals.len()))
        .then_with(|| left.steps.len().cmp(&right.steps.len()))
        .then_with(|| left.path_asset_ids.cmp(&right.path_asset_ids))
}

/// The worst share of a leg's input the market could not absorb, counted
/// only over the legs that spend an intermediate asset, as the exact
/// fraction `(stranded, handed to that leg)`.
///
/// **Only the legs after the first count, and that is the whole point.**
/// Coming up short on the first leg leaves the unspent part in the asset the
/// user was already holding: nothing is trapped, they simply traded less.
/// Coming up short later leaves it in a currency they neither started with
/// nor wanted, with the thin book that refused it as the only way out. That
/// is the "stuck in the middle" the ruling names as the one hazard a smaller
/// order cannot dodge, and it is why this key looks at the middle of a route
/// rather than at how much came out of the end.
///
/// A share and not an amount, so no scale creeps back into the ranking: how
/// many orbs got stranded says nothing about a route that can be run at any
/// size, while what fraction of them did says everything.
///
/// Only observed stranding counts. A leg that did fill might have had one
/// listing of headroom or a thousand, and the walk stops recording levels the
/// moment it is satisfied, so the engine cannot tell -- it reports the trap
/// it watched happen and does not guess at the ones it did not.
fn worst_stranded_share(path: &ConversionPath) -> (u64, u64) {
    path.steps
        .iter()
        .skip(1)
        .filter(|step| step.requested_input.quanta > 0)
        .map(|step| (step.unfilled_input.quanta, step.requested_input.quanta))
        .fold((0, 1), |worst, share| {
            if compare_shares(share, worst) == Ordering::Greater {
                share
            } else {
                worst
            }
        })
}

/// `left.0 / left.1` against `right.0 / right.1`, ascending.
///
/// Cross-multiplied for the same reason as [`compare_realized_price`]:
/// integer division would round two different shares into a tie the market
/// never had, and widening to `u128` makes the multiply total, since the
/// square of a `u64` always fits -- nothing to saturate and no overflow
/// branch to get wrong.
fn compare_shares(left: (u64, u64), right: (u64, u64)) -> Ordering {
    (u128::from(left.0) * u128::from(right.1)).cmp(&(u128::from(right.0) * u128::from(left.1)))
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

/// The product of every leg's front-row rate: the rate this whole route can
/// be listed at, in lowest terms.
///
/// **Size-free by construction, and that is the point.** The improvement the
/// radar ranks and filters on used to be `best.amount_out` against
/// `direct.amount_out` -- two blended averages of two multi-tier sweeps, each
/// averaging over however many levels the stake happened to reach. On a book
/// where direct degrades steeply and a detour degrades gently, the same pair
/// read +20% at a stake of ten and +141% at a stake of a hundred. It also put
/// the radar on a different basis from the convert page, which prices the
/// front rows alone (CORE-TRADING-MODEL, "兑换页围绕汇率算") -- so one pair
/// could be an opportunity on one page and not on the other.
///
/// POE fills a listing at the listed rate or better, so the front rows are
/// what the reader can actually ask for; the levels behind them are a
/// clearance price, not a rate anybody can list at.
///
/// Reduced after every multiply, which is what keeps four-hop products inside
/// `u128`. `None` when any leg recorded no level at all -- there is no rate
/// to compose, and guessing one would invent a number the book never said.
fn front_rate_product(path: &ConversionPath) -> Option<(u128, u128)> {
    let (mut numerator, mut denominator) = (1u128, 1u128);
    for step in &path.steps {
        let front = step.fills.first()?;
        numerator = numerator.checked_mul(u128::from(front.rate.numerator))?;
        denominator = denominator.checked_mul(u128::from(front.rate.denominator))?;
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        if a == 0 {
            return None;
        }
        numerator /= a;
        denominator /= a;
    }
    (numerator != 0 && denominator != 0).then_some((numerator, denominator))
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
    // A partial fill is deliberately *not* incomparable. The comparison is
    // one front rate over another, and neither rate knows how much of the
    // ask its book happened to swallow — the radar asks at a canonical size
    // nobody holds, so on a real book every fill is partial, and gating on
    // coverage here blanked the whole page. The only thing that genuinely
    // breaks the comparison is a leg with no priced front row at all.
    let (Some(best_rate), Some(direct_rate)) =
        (front_rate_product(best), front_rate_product(direct))
    else {
        return Ok(ConversionComparison {
            status: ConversionComparisonStatus::IncomparableCoverage,
            direction: None,
            delta: None,
            basis_points: None,
        });
    };
    // Cross-multiplied, never divided: integer division rounds two rates the
    // market never tied into a tie, and this tie decides whether the radar
    // calls a pair an opportunity at all.
    let (best_scaled, direct_scaled) = (
        best_rate.0.checked_mul(direct_rate.1),
        direct_rate.0.checked_mul(best_rate.1),
    );
    let (Some(best_scaled), Some(direct_scaled)) = (best_scaled, direct_scaled) else {
        return Err(EngineError::NumericOverflow);
    };
    let direction = match best_scaled.cmp(&direct_scaled) {
        Ordering::Greater => ComparisonDirection::Improved,
        Ordering::Equal => ComparisonDirection::Equal,
        Ordering::Less => ComparisonDirection::Worse,
    };
    // The percentage is rate against rate and carries no size. The delta
    // beside it is an amount, so it does scale with the ask -- both projected
    // at the front rates, so the two always agree with each other.
    let stake = u128::from(request.amount_in.quanta);
    let project = |rate: (u128, u128)| {
        stake
            .checked_mul(rate.0)
            .map(|scaled| scaled / rate.1)
            .ok_or(EngineError::NumericOverflow)
    };
    let magnitude = u64::try_from(project(best_rate)?.abs_diff(project(direct_rate)?))
        .map_err(|_| EngineError::NumericOverflow)?;
    let signed = i128::try_from(best_scaled).map_err(|_| EngineError::NumericOverflow)?
        - i128::try_from(direct_scaled).map_err(|_| EngineError::NumericOverflow)?;
    let basis_points = signed
        .checked_mul(10_000)
        .ok_or(EngineError::NumericOverflow)?
        / i128::try_from(direct_scaled).map_err(|_| EngineError::NumericOverflow)?;
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

    /// The ruling's first key: getting stuck beats getting a better rate.
    ///
    /// Both routes eat 1,000 of the 5,000 divine on their first leg and hand
    /// 10,000 of an intermediate currency to their second. The clean route's
    /// second leg swallows all of it; the other one's absorbs 2,000 and
    /// leaves 8,000 sitting in a currency the user neither started with nor
    /// asked for. It pays 5% better per divine for the privilege, and 5% is
    /// not worth being unable to get out.
    #[test]
    fn a_route_that_strands_capital_in_the_middle_loses_to_a_cheaper_clean_one() {
        let clean = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, 1_000, 10_000),
                leg("exalted", "chaos", 10_000, 10_000, 10_000),
            ],
            10_000,
        );
        let stranding = path(
            &["divine", "annul", "chaos"],
            vec![
                leg("divine", "annul", STAKE, 1_000, 10_000),
                leg("annul", "chaos", 10_000, 2_000, 10_500),
            ],
            10_500,
        );

        let mut paths = vec![stranding, clean];
        paths.sort_by(compare_paths);

        assert_eq!(
            paths[0].path_asset_ids,
            vec![asset("divine"), asset("exalted"), asset("chaos")],
            "a middle leg that cannot absorb what it is handed outranks 5% of rate"
        );
    }

    /// Guard on the key underneath: with nothing stranded either side, the
    /// better rate still decides, which is the whole of `70852e1`.
    #[test]
    fn two_partial_routes_that_strand_nothing_still_rank_by_rate() {
        let cheaper = path(
            &["divine", "annul", "chaos"],
            vec![
                leg("divine", "annul", STAKE, 1_000, 10_000),
                leg("annul", "chaos", 10_000, 10_000, 10_000),
            ],
            10_000,
        );
        let dearer = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, 1_000, 10_000),
                leg("exalted", "chaos", 10_000, 10_000, 10_500),
            ],
            10_500,
        );

        let mut paths = vec![cheaper, dearer];
        paths.sort_by(compare_paths);

        assert_eq!(
            paths[0].path_asset_ids,
            vec![asset("divine"), asset("exalted"), asset("chaos")],
            "the liquidity gate must stay silent when no leg came up short"
        );
    }

    /// Guard for the radar, which only ever reads a fully filled best path.
    ///
    /// A path is marked fully filled only when every leg was, so no leg can
    /// have left anything behind and the new key is arithmetically inert
    /// between two of them. That is why adding it cannot change which route
    /// the radar is handed for a pair.
    #[test]
    fn a_fully_filled_route_strands_nothing_so_the_radar_keeps_its_pick() {
        let short = path(
            &["divine", "chaos"],
            vec![leg("divine", "chaos", STAKE, STAKE, 20_030)],
            20_030,
        );
        let long = path(
            &["divine", "exalted", "annul", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, STAKE, 50_000),
                leg("exalted", "annul", 50_000, 50_000, 60_000),
                leg("annul", "chaos", 60_000, 60_000, 19_000),
            ],
            19_000,
        );

        assert_eq!(worst_stranded_share(&short), (0, 1));
        assert_eq!(worst_stranded_share(&long), (0, 1));

        let mut paths = vec![long, short];
        paths.sort_by(compare_paths);

        assert_eq!(
            paths[0].path_asset_ids,
            vec![asset("divine"), asset("chaos")],
            "between full fills the order is still the rate and nothing else"
        );
    }

    /// One market, two routes, and the only thing that changes is the
    /// number the user typed into the box.
    ///
    /// The direct book is deep and pays 10 chaos per divine; the detour
    /// pays 11 but its first leg has ten divine of room. Neither strands
    /// anything in the middle. Ask for ten and the detour is the better
    /// trade; ask for five thousand and it is *still* the better trade,
    /// because a route is a rate -- the user lists a smaller order at 11.0
    /// rather than a larger one at 10.0. Any key that asks "did this eat
    /// the whole request" flips the winner between the two runs, and that
    /// flip is exactly the ruling's "dropped the more profitable option
    /// because the computed amount was small".
    fn market_at(stake: u64) -> Vec<ConversionPath> {
        let direct_eaten = stake.min(5_000);
        let detour_eaten = stake.min(10);
        let mut direct = path(
            &["divine", "chaos"],
            vec![leg(
                "divine",
                "chaos",
                stake,
                direct_eaten,
                direct_eaten * 10,
            )],
            direct_eaten * 10,
        );
        direct.requested_input = amount("divine", stake);
        let mut detour = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", stake, detour_eaten, detour_eaten * 2),
                leg(
                    "exalted",
                    "chaos",
                    detour_eaten * 2,
                    detour_eaten * 2,
                    detour_eaten * 11,
                ),
            ],
            detour_eaten * 11,
        );
        detour.requested_input = amount("divine", stake);
        vec![direct, detour]
    }

    #[test]
    fn the_winner_does_not_change_when_only_the_requested_amount_does() {
        for stake in [10, 5_000] {
            let mut paths = market_at(stake);
            paths.sort_by(compare_paths);

            assert_eq!(
                paths[0].path_asset_ids,
                vec![asset("divine"), asset("exalted"), asset("chaos")],
                "asking for {stake} must not move the 11.0 route off the top"
            );
        }
    }

    /// Why the executability key may stay above the rate even though it
    /// asks a size question inside itself.
    ///
    /// `make_path` only sets `execution_eligible` on a path that swallowed
    /// the whole request, so as a sort key it would flip winners on size
    /// the same way coverage did -- except it also demands
    /// `product_execution_allowed`, which the one policy production ever
    /// builds hard-wires off. Every `ConversionPath` the app can produce
    /// therefore carries `false`, and a key that is constant cannot
    /// reorder anything. Turn this default on and the size flip comes
    /// back, so the day this assert goes red it is the key that needs
    /// revisiting, not the test.
    #[test]
    fn the_shipped_policy_never_allows_product_execution() {
        let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
            .expect("policy");

        assert!(!policy.product_execution_allowed);
    }

    /// The ruling nailed down: size is not a key and must never become one.
    ///
    /// Two routes of the same shape, the same rate and the same (absent)
    /// stranding, one of them run 400 times larger. A route is a rate and
    /// scales, so the small one is every bit as good and the tie has to fall
    /// through to the asset names -- which put `annul`, the 110-chaos route,
    /// first. Re-introduce `amount_out` anywhere in the chain and the
    /// 44,000-chaos route jumps the queue and this goes red.
    #[test]
    fn a_four_hundredfold_difference_in_output_does_not_move_the_order() {
        let small = path(
            &["divine", "annul", "chaos"],
            vec![
                leg("divine", "annul", STAKE, 10, 100),
                leg("annul", "chaos", 100, 100, 110),
            ],
            110,
        );
        let large = path(
            &["divine", "exalted", "chaos"],
            vec![
                leg("divine", "exalted", STAKE, 4_000, 40_000),
                leg("exalted", "chaos", 40_000, 40_000, 44_000),
            ],
            44_000,
        );

        for mut paths in [
            vec![small.clone(), large.clone()],
            vec![large.clone(), small.clone()],
        ] {
            paths.sort_by(compare_paths);
            assert_eq!(
                paths[0].path_asset_ids,
                vec![asset("divine"), asset("annul"), asset("chaos")],
                "400x the output at the same rate must not buy a better place"
            );
        }
    }
}
