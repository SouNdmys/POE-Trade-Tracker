//! Arbitrage in rate space: exact rate products, no quantities at all.
//!
//! The question "is there an edge between these currencies" is a question
//! about rates, and answering it with quantities gets it backwards. A
//! quantity answer floors once per leg, so a small size loses the edge to
//! rounding and a large one loses it to depth — and either way the size, a
//! number that has nothing to do with the market, decides what the market is
//! said to contain. That is how a scan staked at ten chaos concluded there
//! were no opportunities: ten chaos cannot buy one of anything, so every
//! product it computed was zero.
//!
//! What a trader actually does makes the quantity question moot as well.
//! Nobody is bound to a listed rate: with the panel showing 11.33 bid against
//! 11.67 ask, an order at 11.4 goes to the front of the queue and fills, so
//! the trader picks their own rate and their own size. The listed rate's
//! denominator constrains nothing. The edge — the thing worth finding — is
//! the spread itself.
//!
//! So: multiply the legs' rates as exact rationals and compare the product
//! against one. Whole-unit rounding never enters, no stake is assumed, and
//! nothing here knows what the user owns. Sizing belongs downstream, where a
//! person decides how much of their own money to commit.

use ptt_trade_domain::{MarketAssetId, Ratio};
use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::depth::MarketDepthIndex;
use crate::quantity::FeePolicy;

/// One leg of a rate chain: the top-of-book rate for a directed pair, and
/// how much the market is showing at it.
///
/// `top_stock` is a fact about the panel, not about the reader's holdings —
/// it says how far this rate goes before the next level takes over.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLeg {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub rate: Ratio,
    /// What the best row alone is showing.
    pub top_stock: u64,
    /// What the whole side is showing, aggregate row included.
    pub listed_stock: u64,
}

/// A path's rates, multiplied out exactly.
///
/// `numerator / denominator` is how much of the end asset one unit of the
/// start asset becomes, as an exact fraction in lowest terms. For a cycle the
/// two assets are the same, so the fraction is the return and anything above
/// one is arbitrage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateChain {
    pub legs: Vec<RateLeg>,
    pub numerator: u128,
    pub denominator: u128,
}

impl RateChain {
    /// The chain's return over its input, in basis points.
    ///
    /// `(numerator - denominator) / denominator`, scaled — exact, and
    /// truncated toward zero only at the last step, where a whole number of
    /// basis points has to be produced. A cycle at exactly break-even reads
    /// zero rather than a rounding artefact either side of it.
    #[must_use]
    pub fn basis_points(&self) -> i64 {
        let numerator = i128::try_from(self.numerator).unwrap_or(i128::MAX);
        let denominator = i128::try_from(self.denominator).unwrap_or(i128::MAX);
        if denominator == 0 {
            return 0;
        }
        let Some(scaled) = numerator
            .checked_sub(denominator)
            .and_then(|difference| difference.checked_mul(10_000))
        else {
            return i64::MAX;
        };
        i64::try_from(scaled / denominator).unwrap_or(i64::MAX)
    }

    /// Whether the chain ends with more than it started.
    #[must_use]
    pub fn is_profitable(&self) -> bool {
        self.numerator > self.denominator
    }

    /// How liquid this route is, in whole units of its start asset.
    ///
    /// The thinnest leg decides, because a loop moves no more freely than its
    /// narrowest currency, and each leg is measured by everything its side
    /// has listed rather than by its best row alone. The best row is the
    /// thinnest part of any book — a seller at the front of the queue is
    /// giving up margin for speed and rarely has much to give up — so reading
    /// liquidity there measures impatience, not circulation. Capturing all
    /// six rows is what makes the difference visible: on one live panel the
    /// front row showed 72 against 2,831 listed behind it.
    ///
    /// Not a ceiling on the trade. Listings are replaced as they are taken,
    /// and other players list against the same rates, so this is an estimate
    /// of how much of the currency is going around — the thing that decides
    /// whether an edge is worth acting on — rather than a limit on size.
    ///
    /// `None` when the arithmetic would overflow, which is a market deeper
    /// than any trade rather than a failure to answer.
    #[must_use]
    pub fn liquidity(&self) -> Option<u64> {
        self.thinnest_leg(|leg| leg.listed_stock)
    }

    /// The largest input that stays on the rates this chain was priced from,
    /// in whole units of the start asset.
    ///
    /// A size, which [`Self::liquidity`] deliberately is not. Judging an
    /// opportunity and sizing a walk of it are different questions and the
    /// same number cannot answer both: circulation counts every listing on
    /// the side, including the aggregate row whose rate is unknown, so a walk
    /// sized from it runs off the end of the prices it was told about and
    /// comes back a partial fill. This counts only the front row, which is
    /// exactly as far as the quoted rate is good for.
    #[must_use]
    pub fn top_of_book_capacity(&self) -> Option<u64> {
        self.thinnest_leg(|leg| leg.top_stock)
    }

    /// The binding leg under one measure of a leg's stock, carried back to
    /// the start asset through the rates ahead of it.
    fn thinnest_leg(&self, stock_of: impl Fn(&RateLeg) -> u64) -> Option<u64> {
        let mut capacity: Option<u128> = None;
        // Rate product from the start up to (not including) this leg, so a
        // leg's stock can be converted back into start-asset units.
        let mut numerator: u128 = 1;
        let mut denominator: u128 = 1;
        for leg in &self.legs {
            // This leg's stock is in units of what it pays out, which is what
            // one unit of its input buys `rate` of -- so the input it can
            // absorb is `stock * rate.denominator / rate.numerator`, and the
            // start-asset amount that reaches it inverts the prefix.
            let leg_input = u128::from(stock_of(leg))
                .checked_mul(u128::from(leg.rate.denominator))?
                / u128::from(leg.rate.numerator).max(1);
            let at_start = leg_input.checked_mul(denominator)? / numerator.max(1);
            capacity = Some(capacity.map_or(at_start, |held: u128| held.min(at_start)));
            numerator = numerator.checked_mul(u128::from(leg.rate.numerator))?;
            denominator = denominator.checked_mul(u128::from(leg.rate.denominator))?;
            let divisor = greatest_common_divisor(numerator, denominator);
            numerator /= divisor;
            denominator /= divisor;
        }
        capacity.and_then(|value| u64::try_from(value).ok())
    }
}

/// The rates along `path`, multiplied out — or `None` if any leg is unpriced.
///
/// Every leg is the top of its book, because that is the rate a trade would
/// actually get: the exchange fills against the best listing available, not
/// against the one the trader named. How much of the currency is going around
/// is a separate question from what it costs, and it is reported
/// separately by [`RateChain::liquidity`].
pub fn chain_rates(
    index: &MarketDepthIndex,
    path: &[MarketAssetId],
    fee_policy: FeePolicy,
) -> Result<Option<RateChain>, EngineError> {
    fee_policy.validate()?;
    // A per-leg fee is part of the rate a trade actually gets, so it belongs
    // in the product rather than in a correction afterwards: a chain that
    // multiplies gross rates reports edges the fees have already eaten, which
    // is the same false positive in a new place.
    let (fee_retained, fee_total) = match fee_policy {
        FeePolicy::OutputFraction {
            numerator,
            denominator,
        } => (u128::from(denominator - numerator), u128::from(denominator)),
        FeePolicy::None | FeePolicy::Unknown => (1, 1),
    };
    if path.len() < 2 {
        return Ok(None);
    }
    let mut legs = Vec::with_capacity(path.len() - 1);
    let mut numerator: u128 = 1;
    let mut denominator: u128 = 1;
    for window in path.windows(2) {
        let (from, to) = (&window[0], &window[1]);
        if from == to {
            return Ok(None);
        }
        let Some((rate, top_stock)) = index.top_of_book(from, to) else {
            return Ok(None);
        };
        let listed_stock = index.listed_liquidity(from, to).unwrap_or(top_stock);
        numerator = numerator
            .checked_mul(u128::from(rate.numerator))
            .and_then(|value| value.checked_mul(fee_retained))
            .ok_or(EngineError::NumericOverflow)?;
        denominator = denominator
            .checked_mul(u128::from(rate.denominator))
            .and_then(|value| value.checked_mul(fee_total))
            .ok_or(EngineError::NumericOverflow)?;
        // Reduced every step, not just at the end: three panel rates with
        // two-decimal denominators reach 10^6 on their own, and a fourth leg
        // would carry that into the range where the product stops fitting.
        let divisor = greatest_common_divisor(numerator, denominator);
        numerator /= divisor;
        denominator /= divisor;
        legs.push(RateLeg {
            from_asset_id: from.clone(),
            to_asset_id: to.clone(),
            rate,
            top_stock,
            listed_stock,
        });
    }
    Ok(Some(RateChain {
        legs,
        numerator,
        denominator,
    }))
}

fn greatest_common_divisor(left: u128, right: u128) -> u128 {
    let (mut left, mut right) = (left, right);
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(rates: &[(u64, u64)], stocks: &[u64]) -> RateChain {
        let mut numerator: u128 = 1;
        let mut denominator: u128 = 1;
        let legs = rates
            .iter()
            .zip(stocks)
            .enumerate()
            .map(|(index, ((rate_numerator, rate_denominator), stock))| {
                numerator *= u128::from(*rate_numerator);
                denominator *= u128::from(*rate_denominator);
                RateLeg {
                    from_asset_id: MarketAssetId::try_new(format!("a{index}")).expect("asset"),
                    to_asset_id: MarketAssetId::try_new(format!("a{}", index + 1)).expect("asset"),
                    rate: Ratio::from_parts(*rate_numerator, *rate_denominator).expect("rate"),
                    top_stock: *stock,
                    listed_stock: *stock,
                }
            })
            .collect();
        let divisor = greatest_common_divisor(numerator, denominator);
        RateChain {
            legs,
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    /// A cycle that returns exactly what it started with is not an edge.
    ///
    /// The boundary case a quantity walk cannot state: with flooring, a
    /// break-even cycle reads as a loss at every size, and the size decides
    /// how big a loss. In rate space it is zero, exactly.
    #[test]
    fn a_break_even_cycle_reads_zero_and_is_not_profitable() {
        // 2 -> 3 -> 1/6 of the way back: 2 * 3 * 1/6 = 1.
        let cycle = chain(&[(2, 1), (3, 1), (1, 6)], &[100, 100, 100]);
        assert_eq!(cycle.numerator, cycle.denominator);
        assert_eq!(cycle.basis_points(), 0);
        assert!(!cycle.is_profitable());
    }

    /// The edge survives rates no whole number can express.
    ///
    /// Every rate here is a panel decimal — 11.15, 10.81 — whose whole-unit
    /// lot would be twenty or a hundred units. In rate space the lot never
    /// comes up: 1115/100 x 100/1081 x 1/1 is one exact fraction.
    #[test]
    fn two_decimal_panel_rates_multiply_exactly() {
        // 11.15 out, then 1/10.81 back, then 1: a real 3.14% round trip.
        let cycle = chain(&[(1115, 100), (100, 1081), (1, 1)], &[1_000, 1_000, 1_000]);
        assert!(cycle.is_profitable());
        // 1115/1081 = 1.031452...
        assert_eq!(cycle.basis_points(), 314);
    }

    /// A loss is reported as a negative, not clamped or dropped.
    #[test]
    fn a_losing_cycle_reports_a_negative_edge() {
        let cycle = chain(&[(1, 2), (1, 1), (1, 1)], &[10, 10, 10]);
        assert!(!cycle.is_profitable());
        assert_eq!(cycle.basis_points(), -5_000);
    }

    /// Judging a route and sizing a walk of it are different measurements.
    ///
    /// The regression this pins, in miniature: liquidity counts every listing
    /// on the side — most of a panel's currency sits in the aggregate row,
    /// whose rate is unknown — while the walk may only use the front row's
    /// stock, because that is as far as the quoted rate is good for. Sizing
    /// the walk from liquidity ran every cycle off the end of its own prices
    /// and turned nine opportunities into none.
    #[test]
    fn liquidity_counts_the_whole_side_and_the_walk_size_only_the_front_row() {
        let mut path = chain(&[(1, 1), (1, 1)], &[10, 10]);
        // The front row is thin and the rest of the side is deep, which is
        // the usual shape: a seller at the front of the queue is trading
        // margin for speed and has little to trade.
        for leg in &mut path.legs {
            leg.top_stock = 10;
            leg.listed_stock = 2_831;
        }
        assert_eq!(path.top_of_book_capacity(), Some(10));
        assert_eq!(path.liquidity(), Some(2_831));
    }

    /// Capacity is the thinnest leg, carried back to the start asset.
    ///
    /// A market fact — how far the listed rates go — and deliberately not a
    /// statement about the reader's holdings, which this layer never sees.
    #[test]
    fn liquidity_is_the_thinnest_leg_expressed_in_the_start_asset() {
        // Leg one doubles and shows 100 of its payout, so it absorbs 50 of
        // the start. Leg two shows 40 of its payout at 1:1, so it absorbs 40
        // of leg one's output -- which is 20 of the start, and the binding
        // constraint.
        let path = chain(&[(2, 1), (1, 1)], &[100, 40]);
        assert_eq!(path.liquidity(), Some(20));

        // Widen the second leg and the first one binds instead.
        let wider = chain(&[(2, 1), (1, 1)], &[100, 400]);
        assert_eq!(wider.liquidity(), Some(50));
    }
}
