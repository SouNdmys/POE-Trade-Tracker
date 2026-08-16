//! Three-tier profit accounting for a conversion route.
//!
//! Ported from `routeExecutionAccounting.ts`. The tiers answer three
//! different questions that the Electron UI kept conflating:
//!
//! * **closed** — what you get if you size the trade so nothing is left
//!   stranded. This is the number to act on.
//! * **theoretical** — what the arithmetic says at the full requested size,
//!   ignoring the depth ceiling. Always the largest and always the least
//!   trustworthy.
//! * **mark to market** — what you actually hold afterwards at the full size:
//!   the realised output plus stranded inventory valued at current rates.
//!   Unrealised, and labelled as such.
//!
//! Two things the JS did that this does not. It re-simulated the capped run
//! itself, duplicating (and disagreeing with) the engine's own fill; here the
//! engine's `ConversionPath` *is* the capped run, so the realised numbers and
//! the residual list come straight from it. And it treated every step's
//! blended rate as valid at any size; here extrapolating past observed depth
//! raises [`ModelCaveat::ExtrapolatedBeyondObservedDepth`], because deeper
//! levels price worse and the theoretical tier is an upper bound.

use std::collections::BTreeMap;

use ptt_trade_domain::{MarketAssetId, Ratio};
use ptt_trade_engine::{
    AssetAmount, ComparisonDirection, ConversionPath, PairBottleneck, PairFill,
};
use serde::{Deserialize, Serialize};

use crate::StrategyError;
use crate::exact::Rational;
use crate::execution_safety::{
    ExecutionRisk, ModelCaveat, RiskAssessment, RiskThresholds, assess_path,
};
use crate::units::{
    amount_like, apply_scale, quanta_scale_to_rate, rate_to_quanta_scale, whole_units,
};

/// Current rates used to value inventory the route could not convert.
///
/// The caller fills this from the selected book; the accounting never reaches
/// back into the depth index itself, which keeps it testable with a handful
/// of rates instead of a whole market.
#[derive(Clone, Debug, Default)]
pub struct MarkRateTable {
    rates: BTreeMap<(String, String), Ratio>,
}

impl MarkRateTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, from: &MarketAssetId, to: &MarketAssetId, rate: Ratio) {
        self.rates
            .insert((from.as_str().to_owned(), to.as_str().to_owned()), rate);
    }

    /// The rate to value one whole unit of `from` in units of `to`.
    ///
    /// A reverse quote is inverted only as a mark, never as an execution
    /// rate: marking inventory is an estimate, whereas F6 forbids synthesising
    /// a tradable inverse edge.
    #[must_use]
    pub fn rate(&self, from: &MarketAssetId, to: &MarketAssetId) -> Option<Ratio> {
        if from == to {
            return Ratio::from_parts(1, 1).ok();
        }
        let key = (from.as_str().to_owned(), to.as_str().to_owned());
        if let Some(rate) = self.rates.get(&key) {
            return Some(rate.clone());
        }
        let reverse = (to.as_str().to_owned(), from.as_str().to_owned());
        self.rates.get(&reverse).map(Ratio::inverse)
    }
}

/// Where a residual's valuation rate came from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkRateSource {
    /// A quote for the residual asset against the account asset.
    DirectQuote,
    /// The remaining legs of this route, which end in the account asset.
    RemainingRoute,
}

/// One profit tier: an output, the baseline it is measured against, and the
/// signed difference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfitTier {
    pub input: AssetAmount,
    pub output: AssetAmount,
    /// What the same input would have produced going direct. `None` when no
    /// direct route was observed, in which case no improvement is claimed —
    /// the audit's B-08 case.
    pub baseline_output: Option<AssetAmount>,
    pub direction: Option<ComparisonDirection>,
    /// Absolute size of the difference from the baseline.
    pub delta: Option<AssetAmount>,
    pub basis_points: Option<i64>,
}

/// Currency the route could not convert, and what it is worth now.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualPosition {
    pub asset_id: MarketAssetId,
    pub amount: AssetAmount,
    pub after_step: u8,
    pub mark_rate_source: Option<MarkRateSource>,
    /// The rate used to value it, in whole units of the account asset per
    /// whole unit of the residual asset.
    pub mark_rate: Option<Ratio>,
    /// Value of this inventory in the account asset, at current rates.
    pub mark_value: Option<AssetAmount>,
    /// What the input spent acquiring it would have been worth going direct.
    pub cost_basis: Option<AssetAmount>,
    pub clear_direction: Option<ComparisonDirection>,
    pub clear_delta: Option<AssetAmount>,
    /// The unit price at which clearing this inventory breaks even, expressed
    /// as `residual_asset : account_asset`.
    pub break_even_unit_price: Option<Ratio>,
}

/// The full accounting for one route at one requested size.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteAccounting {
    pub route_asset_ids: Vec<MarketAssetId>,
    pub requested_input: AssetAmount,
    /// The asset every profit figure is denominated in: the input asset for a
    /// closed loop, the output asset otherwise.
    pub account_asset_id: MarketAssetId,
    pub is_closed_loop: bool,
    /// The largest input that flows through with nothing stranded. May
    /// exceed [`Self::requested_input`] when the book has headroom.
    pub closed_input_capacity: AssetAmount,
    /// True when no leg ran out of depth, so the real ceiling is unknown and
    /// the capacity above is only "at least this much".
    pub capacity_is_lower_bound: bool,
    pub recommended_input: AssetAmount,
    pub closed: ProfitTier,
    pub theoretical: ProfitTier,
    pub mark_to_market: ProfitTier,
    pub residuals: Vec<ResidualPosition>,
    /// Total residual value in the account asset; `None` when nothing could
    /// be valued.
    pub residual_mark_value: Option<AssetAmount>,
    /// False when at least one residual had no rate to value it with, so the
    /// mark-to-market tier understates what is held.
    pub residual_mark_complete: bool,
    pub bottleneck: Option<PairBottleneck>,
    pub assessment: RiskAssessment,
}

/// Inputs for [`derive_route_accounting`].
#[derive(Clone, Copy, Debug)]
pub struct RouteAccountingRequest<'a> {
    pub path: &'a ConversionPath,
    /// The one-hop alternative, when the book has one.
    pub direct_path: Option<&'a ConversionPath>,
    pub mark_rates: &'a MarkRateTable,
    pub thresholds: RiskThresholds,
    pub needs_probe: bool,
}

/// The blended output-per-input scale a leg actually achieved, in quanta.
fn step_scale(step: &PairFill) -> Option<Rational> {
    let realized = step
        .net_amount_out
        .as_ref()
        .unwrap_or(&step.gross_amount_out);
    if step.consumed_input.quanta == 0 {
        return None;
    }
    Rational::new(
        u128::from(realized.quanta),
        u128::from(step.consumed_input.quanta),
    )
}

/// Run `quanta` through the legs at their observed scales, flooring at each
/// leg the way the game does — you cannot hold a fraction of an orb.
fn simulate(steps: &[PairFill], quanta: u64) -> Option<u64> {
    let mut amount = quanta;
    for step in steps {
        let scale = step_scale(step)?;
        amount = apply_scale(amount, scale)?;
        if amount == 0 {
            return Some(0);
        }
    }
    Some(amount)
}

fn tier(
    input: AssetAmount,
    output: AssetAmount,
    baseline_output: Option<AssetAmount>,
) -> Result<ProfitTier, StrategyError> {
    let Some(baseline) = baseline_output else {
        return Ok(ProfitTier {
            input,
            output,
            baseline_output: None,
            direction: None,
            delta: None,
            basis_points: None,
        });
    };
    if baseline.asset_id != output.asset_id {
        return Err(StrategyError::InvalidRequest);
    }
    let (direction, magnitude) = match output.quanta.cmp(&baseline.quanta) {
        std::cmp::Ordering::Greater => (
            ComparisonDirection::Improved,
            output.quanta - baseline.quanta,
        ),
        std::cmp::Ordering::Equal => (ComparisonDirection::Equal, 0),
        std::cmp::Ordering::Less => (ComparisonDirection::Worse, baseline.quanta - output.quanta),
    };
    // Same convention as the engine's own comparison: signed basis points
    // against the baseline, truncated toward zero.
    let signed = match direction {
        ComparisonDirection::Improved => i128::from(magnitude),
        ComparisonDirection::Equal => 0,
        ComparisonDirection::Worse => -i128::from(magnitude),
    };
    let basis_points = if baseline.quanta == 0 {
        None
    } else {
        signed
            .checked_mul(10_000)
            .map(|scaled| scaled / i128::from(baseline.quanta))
            .and_then(|value| i64::try_from(value).ok())
    };
    let delta = amount_like(&output, magnitude);
    Ok(ProfitTier {
        input,
        output,
        baseline_output: Some(baseline),
        direction: Some(direction),
        delta: Some(delta),
        basis_points,
    })
}

/// Largest input, in input quanta, that leaves nothing stranded.
///
/// Only legs that actually ran out of depth constrain the answer. A leg that
/// filled completely proves only that its ceiling is at least what flowed
/// through it, so treating that as its capacity would invent a bottleneck
/// where none was observed.
fn closed_capacity(path: &ConversionPath) -> Option<(u64, bool)> {
    let mut prefix = Rational::one();
    let mut capacity: Option<u64> = None;
    for step in &path.steps {
        if let Some(bottleneck) = &step.bottleneck {
            let pulled_back =
                Rational::from_u128(u128::from(bottleneck.max_consumable_input.quanta))
                    .checked_div(prefix)?
                    .floor_u64()?;
            capacity = Some(capacity.map_or(pulled_back, |current| current.min(pulled_back)));
        }
        prefix = prefix.checked_mul(step_scale(step)?)?;
    }
    // Deliberately unclamped: this is what the book absorbs, which may be
    // more than was asked for. Clamping it to the request made the field a
    // copy of `recommended_input` and hid genuine headroom, which reads as
    // "split your order" when nothing needs splitting.
    match capacity {
        Some(value) => Some((value, false)),
        None => Some((path.requested_input.quanta, true)),
    }
}

fn remaining_route_scale(
    steps: &[PairFill],
    from_step: usize,
    account_asset_id: &MarketAssetId,
) -> Option<Rational> {
    let remaining = steps.get(from_step..)?;
    if remaining.is_empty() || remaining.last()?.to_asset_id != *account_asset_id {
        return None;
    }
    let mut scale = Rational::one();
    for step in remaining {
        scale = scale.checked_mul(step_scale(step)?)?;
    }
    Some(scale)
}

/// Derive the three-tier accounting for one route.
pub fn derive_route_accounting(
    request: RouteAccountingRequest<'_>,
) -> Result<RouteAccounting, StrategyError> {
    let path = request.path;
    if path.steps.is_empty() || path.path_asset_ids.len() < 2 {
        return Err(StrategyError::InvalidRequest);
    }
    let input = path.requested_input.clone();
    let output_reference = path.amount_out.clone();
    let is_closed_loop = input.asset_id == output_reference.asset_id;
    let account_reference = if is_closed_loop {
        input.clone()
    } else {
        output_reference.clone()
    };

    let mut assessment = assess_path(path, request.thresholds, request.needs_probe);

    let (capacity_quanta, capacity_is_lower_bound) =
        closed_capacity(path).ok_or(StrategyError::UnusableRoute)?;
    let closed_input_capacity = amount_like(&input, capacity_quanta);
    let recommended_quanta = capacity_quanta.min(input.quanta);
    let recommended_input = amount_like(&input, recommended_quanta);

    // Closed tier: size down to what flows cleanly.
    let closed_output_quanta =
        simulate(&path.steps, recommended_quanta).ok_or(StrategyError::UnusableRoute)?;
    let closed_output = amount_like(&output_reference, closed_output_quanta);
    let closed_baseline = direct_output(request.direct_path, recommended_quanta, &input)?;
    let closed = tier(recommended_input.clone(), closed_output, closed_baseline)?;

    // Theoretical tier: full size, depth ceiling ignored.
    let theoretical_output_quanta =
        simulate(&path.steps, input.quanta).ok_or(StrategyError::UnusableRoute)?;
    let theoretical_output = amount_like(&output_reference, theoretical_output_quanta);
    let theoretical_baseline = direct_output(request.direct_path, input.quanta, &input)?;
    let theoretical = tier(
        input.clone(),
        theoretical_output,
        theoretical_baseline.clone(),
    )?;
    if recommended_quanta < input.quanta {
        assessment
            .caveats
            .insert(ModelCaveat::ExtrapolatedBeyondObservedDepth);
    }

    // Mark to market: what is actually held afterwards, at full size.
    let mut residuals = Vec::with_capacity(path.residuals.len());
    let mut residual_total: Option<u64> = None;
    let mut residual_mark_complete = true;
    let mut prefix_by_step = Vec::with_capacity(path.steps.len() + 1);
    let mut prefix = Rational::one();
    prefix_by_step.push(prefix);
    for step in &path.steps {
        prefix = prefix
            .checked_mul(step_scale(step).ok_or(StrategyError::UnusableRoute)?)
            .ok_or(StrategyError::UnusableRoute)?;
        prefix_by_step.push(prefix);
    }

    for residual in &path.residuals {
        // `after_step` counts steps taken, so the leg that could not consume
        // this inventory sits one index earlier — and that same leg is where
        // the rest of the route would resume from.
        let step_index = usize::from(residual.after_step).saturating_sub(1);
        let (scale, rate, source) = match request
            .mark_rates
            .rate(&residual.amount.asset_id, &account_reference.asset_id)
        {
            Some(rate) => (
                rate_to_quanta_scale(&rate, residual.amount.unit, account_reference.unit),
                Some(rate),
                Some(MarkRateSource::DirectQuote),
            ),
            None => {
                match remaining_route_scale(&path.steps, step_index, &account_reference.asset_id) {
                    Some(scale) => (
                        Some(scale),
                        quanta_scale_to_rate(scale, residual.amount.unit, account_reference.unit),
                        Some(MarkRateSource::RemainingRoute),
                    ),
                    None => (None, None, None),
                }
            }
        };
        let marked = scale.and_then(|scale| {
            apply_scale(residual.amount.quanta, scale)
                .map(|quanta| amount_like(&account_reference, quanta))
        });
        if let Some(value) = &marked {
            let total = residual_total.unwrap_or(0);
            residual_total = Some(
                total
                    .checked_add(value.quanta)
                    .ok_or(StrategyError::UnusableRoute)?,
            );
        } else {
            residual_mark_complete = false;
        }

        // What the input that bought this inventory would have been worth had
        // it gone direct instead: the honest basis for "was stranding it a
        // loss".
        let basis_input = prefix_by_step
            .get(step_index)
            .and_then(|prefix| {
                Rational::from_u128(u128::from(residual.amount.quanta)).checked_div(*prefix)
            })
            .and_then(Rational::floor_u64);
        let cost_basis = match basis_input {
            Some(basis) if is_closed_loop => Some(amount_like(&account_reference, basis)),
            Some(basis) => direct_output(request.direct_path, basis, &input)?,
            None => None,
        };
        let (clear_direction, clear_delta) = match (&marked, &cost_basis) {
            (Some(value), Some(basis)) => match value.quanta.cmp(&basis.quanta) {
                std::cmp::Ordering::Greater => (
                    Some(ComparisonDirection::Improved),
                    Some(amount_like(&account_reference, value.quanta - basis.quanta)),
                ),
                std::cmp::Ordering::Equal => (
                    Some(ComparisonDirection::Equal),
                    Some(amount_like(&account_reference, 0)),
                ),
                std::cmp::Ordering::Less => (
                    Some(ComparisonDirection::Worse),
                    Some(amount_like(&account_reference, basis.quanta - value.quanta)),
                ),
            },
            _ => (None, None),
        };
        let break_even_unit_price = cost_basis.as_ref().and_then(|basis| {
            let basis_whole = whole_units(basis)?;
            let residual_whole = whole_units(&residual.amount)?;
            if residual_whole.is_zero() {
                return None;
            }
            let (num, den) = basis_whole.checked_div(residual_whole)?.to_u64_pair()?;
            Ratio::from_parts(num, den).ok()
        });

        residuals.push(ResidualPosition {
            asset_id: residual.amount.asset_id.clone(),
            amount: residual.amount.clone(),
            after_step: residual.after_step,
            mark_rate_source: source,
            mark_rate: rate,
            mark_value: marked,
            cost_basis,
            clear_direction,
            clear_delta,
            break_even_unit_price,
        });
    }

    let residual_mark_value = residual_total.map(|quanta| amount_like(&account_reference, quanta));

    // A closed loop ends in the account asset by definition, and an open
    // route's account asset is its output asset, so the realised output is
    // already denominated correctly in both cases.
    let held_quanta = path
        .amount_out
        .quanta
        .saturating_add(residual_mark_value.as_ref().map_or(0, |value| value.quanta));
    let mark_to_market = tier(
        input.clone(),
        amount_like(&account_reference, held_quanta),
        theoretical_baseline,
    )?;

    if !residuals.is_empty() {
        assessment.risks.insert(ExecutionRisk::ResidualInventory);
    }

    Ok(RouteAccounting {
        route_asset_ids: path.path_asset_ids.clone(),
        requested_input: input,
        account_asset_id: account_reference.asset_id.clone(),
        is_closed_loop,
        closed_input_capacity,
        capacity_is_lower_bound,
        recommended_input,
        closed,
        theoretical,
        mark_to_market,
        residuals,
        residual_mark_value,
        residual_mark_complete,
        bottleneck: path.bottleneck.clone(),
        assessment,
    })
}

/// What the direct route yields for a given input, when one was observed.
fn direct_output(
    direct: Option<&ConversionPath>,
    input_quanta: u64,
    input_reference: &AssetAmount,
) -> Result<Option<AssetAmount>, StrategyError> {
    let Some(direct) = direct else {
        return Ok(None);
    };
    if direct.path_asset_ids.first() != Some(&input_reference.asset_id) {
        return Err(StrategyError::InvalidRequest);
    }
    let Some(quanta) = simulate(&direct.steps, input_quanta) else {
        return Ok(None);
    };
    Ok(Some(amount_like(&direct.amount_out, quanta)))
}
