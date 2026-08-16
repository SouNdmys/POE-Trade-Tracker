//! What a currency is worth, quoted against one anchor.
//!
//! Ported from `anchorValue.ts`. The valuation modes are the same three, but
//! the JS conflated two very different things under "value": the rate you can
//! sell at and the rate you must pay to buy, averaged together whenever both
//! existed and silently falling back to whichever one it had otherwise. A
//! one-sided valuation is a materially weaker claim than a two-sided one, so
//! [`Valuation::status`] states which it is instead of leaving the caller to
//! infer it from which fields happen to be populated.

use ptt_market_book::{EvaluatedQuoteEdge, FreshnessStatus};
use ptt_trade_domain::{ExecutionType, MarketAssetId, Ratio};
use serde::{Deserialize, Serialize};

use crate::execution_safety::ExecutionRisk;
use crate::units::rational_from_ratio;

/// Which side of the book to value against.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValuationMode {
    /// What you would receive selling the target for the anchor.
    #[default]
    SellValue,
    /// What you would pay buying the target with the anchor.
    BuyCost,
    /// The geometric mean of the two.
    Midpoint,
}

/// How well supported a valuation is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValuationStatus {
    /// Both sides of the book were observed.
    TwoSided,
    /// Only one side was observed, so there is no spread to reason about.
    OneSided,
    /// Neither side was observed.
    NoPath,
}

/// One currency's worth in anchor units.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Valuation {
    pub asset_id: MarketAssetId,
    pub anchor_asset_id: MarketAssetId,
    pub mode: ValuationMode,
    pub status: ValuationStatus,
    /// Anchor units received per unit sold.
    pub sell_value: Option<Ratio>,
    /// Anchor units paid per unit bought.
    pub buy_cost: Option<Ratio>,
    /// Geometric mean of the two. Approximate — see
    /// [`Valuation::midpoint_is_approximate`].
    pub midpoint: Option<Ratio>,
    /// The figure the requested mode selects.
    pub value: Option<Ratio>,
    /// True whenever `midpoint` is populated: the square root of a rational
    /// is generally irrational, so this one number is rounded to roughly
    /// `1e-6` relative and must never be used as an execution rate.
    pub midpoint_is_approximate: bool,
    /// The bid/ask spread implied by the two sides, in basis points.
    pub spread_basis_points: Option<i64>,
    pub risks: Vec<ExecutionRisk>,
}

/// Inputs for [`value_against_anchor`].
#[derive(Clone, Copy, Debug)]
pub struct ValuationRequest<'a> {
    pub asset_id: &'a MarketAssetId,
    pub anchor_asset_id: &'a MarketAssetId,
    pub mode: ValuationMode,
    /// Selected edges to draw from, both directions.
    pub edges: &'a [EvaluatedQuoteEdge],
    /// Whether stale and archived quotes may be used.
    pub include_historical: bool,
}

/// The best quote for one direction, or nothing.
fn best_rate(
    edges: &[EvaluatedQuoteEdge],
    from: &MarketAssetId,
    to: &MarketAssetId,
    include_historical: bool,
) -> Option<(Ratio, FreshnessStatus, bool)> {
    edges
        .iter()
        .filter(|evaluated| {
            let edge = &evaluated.observation.edge;
            edge.from_asset_id == *from && edge.to_asset_id == *to
        })
        .filter(|evaluated| {
            include_historical
                || matches!(
                    evaluated.freshness.status,
                    FreshnessStatus::Fresh | FreshnessStatus::Usable
                )
        })
        // Prefer what you could actually trade against, then the newest.
        .max_by(|left, right| {
            let taker = |edge: &EvaluatedQuoteEdge| {
                edge.observation.edge.execution_type == ExecutionType::Taker
            };
            taker(left).cmp(&taker(right)).then_with(|| {
                left.observation
                    .edge
                    .captured_at
                    .cmp(&right.observation.edge.captured_at)
            })
        })
        .map(|evaluated| {
            (
                evaluated.observation.edge.rate.clone(),
                evaluated.freshness.status,
                evaluated.observation.edge.execution_type == ExecutionType::MakerReference,
            )
        })
}

/// Value one currency against an anchor.
#[must_use]
pub fn value_against_anchor(request: ValuationRequest<'_>) -> Valuation {
    let mut risks = Vec::new();
    if request.asset_id == request.anchor_asset_id {
        // A currency is worth one of itself; saying so beats reporting no
        // path for the anchor's own row.
        return Valuation {
            asset_id: request.asset_id.clone(),
            anchor_asset_id: request.anchor_asset_id.clone(),
            mode: request.mode,
            status: ValuationStatus::TwoSided,
            sell_value: Ratio::from_parts(1, 1).ok(),
            buy_cost: Ratio::from_parts(1, 1).ok(),
            midpoint: Ratio::from_parts(1, 1).ok(),
            value: Ratio::from_parts(1, 1).ok(),
            midpoint_is_approximate: false,
            spread_basis_points: Some(0),
            risks,
        };
    }

    let sell = best_rate(
        request.edges,
        request.asset_id,
        request.anchor_asset_id,
        request.include_historical,
    );
    // Buying the target costs anchor per target, which is the inverse of the
    // anchor -> target rate.
    let buy = best_rate(
        request.edges,
        request.anchor_asset_id,
        request.asset_id,
        request.include_historical,
    );

    for observed in [sell.as_ref(), buy.as_ref()].into_iter().flatten() {
        match observed.1 {
            FreshnessStatus::Stale => risks.push(ExecutionRisk::StaleData),
            FreshnessStatus::Archived => risks.push(ExecutionRisk::ArchivedData),
            FreshnessStatus::Fresh | FreshnessStatus::Usable => {}
        }
        if observed.2 {
            risks.push(ExecutionRisk::MakerReference);
        }
    }
    risks.sort_unstable();
    risks.dedup();

    let sell_value = sell.as_ref().map(|observed| observed.0.clone());
    let buy_cost = buy.as_ref().map(|observed| observed.0.inverse());

    let midpoint =
        sell_value
            .as_ref()
            .zip(buy_cost.as_ref())
            .and_then(|(sell, buy)| -> Option<Ratio> {
                let product = rational_from_ratio(sell)?.checked_mul(rational_from_ratio(buy)?)?;
                let (numerator, denominator) = product.sqrt_approx()?.to_u64_pair()?;
                Ratio::from_parts(numerator, denominator).ok()
            });

    let spread_basis_points = sell_value
        .as_ref()
        .zip(buy_cost.as_ref())
        .and_then(|(sell, buy)| {
            crate::units::basis_points(rational_from_ratio(buy)?, rational_from_ratio(sell)?)
        });

    let status = match (&sell_value, &buy_cost) {
        (Some(_), Some(_)) => ValuationStatus::TwoSided,
        (None, None) => ValuationStatus::NoPath,
        _ => ValuationStatus::OneSided,
    };
    if status == ValuationStatus::NoPath {
        risks.push(ExecutionRisk::NeedsProbe);
    }

    // A mode that asks for a side the book does not have gets nothing rather
    // than the other side wearing the requested label.
    let value = match request.mode {
        ValuationMode::SellValue => sell_value.clone(),
        ValuationMode::BuyCost => buy_cost.clone(),
        ValuationMode::Midpoint => midpoint.clone(),
    };

    Valuation {
        asset_id: request.asset_id.clone(),
        anchor_asset_id: request.anchor_asset_id.clone(),
        mode: request.mode,
        status,
        sell_value,
        buy_cost,
        midpoint_is_approximate: midpoint.is_some(),
        midpoint,
        value,
        spread_basis_points,
        risks,
    }
}

#[cfg(test)]
mod tests {
    use crate::exact::Rational;

    #[test]
    fn the_geometric_midpoint_lands_between_the_two_sides() {
        // sell 4, buy 9 -> midpoint 6.
        let sell = Rational::new(4, 1).expect("sell");
        let buy = Rational::new(9, 1).expect("buy");
        let midpoint = sell
            .checked_mul(buy)
            .expect("product")
            .sqrt_approx()
            .expect("sqrt");
        assert_eq!(midpoint.floor_u64(), Some(6));
    }
}
