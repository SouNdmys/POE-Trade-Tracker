//! Conversions between quoted rates, whole orbs, and engine quanta.
//!
//! Rates are quoted in whole orbs but amounts are held in quanta of an
//! asset's minimum unit. Every place that multiplies one by the other has to
//! fold both units in; doing it in one place keeps the two sides from
//! drifting apart, and keeps POE1's fractional minimum units from silently
//! working only because POE2's are all whole.

use ptt_trade_domain::Ratio;
use ptt_trade_engine::{AssetAmount, AssetUnit};

use crate::exact::Rational;

/// An amount of the same asset and unit as `reference`.
pub(crate) fn amount_like(reference: &AssetAmount, quanta: u64) -> AssetAmount {
    AssetAmount {
        asset_id: reference.asset_id.clone(),
        quanta,
        unit: reference.unit,
    }
}

/// Whole orbs per quantum.
pub(crate) fn unit_value(unit: AssetUnit) -> Option<Rational> {
    Rational::new(u128::from(unit.numerator), u128::from(unit.denominator))
}

/// The amount in whole orbs, exactly.
pub(crate) fn whole_units(amount: &AssetAmount) -> Option<Rational> {
    let (numerator, denominator) = amount.exact_display_fraction();
    Rational::new(numerator, denominator)
}

pub(crate) fn rational_from_ratio(rate: &Ratio) -> Option<Rational> {
    Rational::new(u128::from(rate.numerator), u128::from(rate.denominator))
}

/// A whole-orbs-per-whole-orb rate as quanta out per quantum in.
pub(crate) fn rate_to_quanta_scale(
    rate: &Ratio,
    from: AssetUnit,
    to: AssetUnit,
) -> Option<Rational> {
    rational_from_ratio(rate)?
        .checked_mul(unit_value(from)?)?
        .checked_div(unit_value(to)?)
}

/// The inverse of [`rate_to_quanta_scale`], for quoting a derived scale.
pub(crate) fn quanta_scale_to_rate(
    scale: Rational,
    from: AssetUnit,
    to: AssetUnit,
) -> Option<Ratio> {
    let (numerator, denominator) = scale
        .checked_mul(unit_value(to)?)?
        .checked_div(unit_value(from)?)?
        .to_u64_pair()?;
    Ratio::from_parts(numerator, denominator).ok()
}

/// Apply a quanta scale, flooring: the game has no fractional orbs.
pub(crate) fn apply_scale(quanta: u64, scale: Rational) -> Option<u64> {
    Rational::from_u128(u128::from(quanta))
        .checked_mul(scale)?
        .floor_u64()
}
