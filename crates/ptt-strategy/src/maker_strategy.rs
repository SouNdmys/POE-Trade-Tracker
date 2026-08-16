//! Where to list an order, and what listing there costs you in waiting.
//!
//! Ported from `makerStrategy.ts` with the audit's B-11 defect fixed. That
//! module defined the competing *queue* one way and the competing *front*
//! another: queue depth summed every listing at or below a rate (correct —
//! a maker asking for less gets taken first), while `front_depth` was read
//! off an array sorted by descending rate, so it reported the three greediest
//! listings as if they were the ones at the head of the line. On the audit's
//! own fixture the front three should hold 60 orbs and it reported 450. That
//! number drove the suggested order size and the queue-pressure explanation,
//! so it was wrong in the direction that encourages oversized orders.
//!
//! Here [`queue_order`] is the single definition of "front", and the queue,
//! the front depth, the wall scan and the fast-mode search all sort through
//! it. The direction is pinned by a test.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use ptt_market_book::{EvaluatedQuoteEdge, FreshnessStatus, QuoteRiskFlag};
use ptt_trade_domain::{Comparator, ExecutionType, MarketAssetId, Ratio};
use ptt_trade_engine::{AssetAmount, PairFill};
use serde::{Deserialize, Serialize};

use crate::StrategyError;
use crate::exact::Rational;
use crate::execution_safety::{
    Actionability, ExecutionRisk, ModelCaveat, RiskAssessment, RiskThresholds, actionability_for,
};
use crate::units::{
    amount_like, apply_scale, basis_points, quanta_scale_to_rate, rate_to_quanta_scale,
    rational_from_ratio, unit_value,
};

/// How aggressively to price a listing.
///
/// There is deliberately no "middle" mode. Sitting halfway down the queue
/// means waiting for everything ahead of you to clear at a rate nobody is
/// competing for — it is strictly worse than either end.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MakerMode {
    /// Price against the front of the competing queue: one displayed tick
    /// below it, so the next buyer takes your order before anyone else's.
    /// In a liquid pair this fills; the reward is the gap between the
    /// instant price and the front of the competing book.
    ///
    /// See [`MakerRequest::match_front`] for listing *at* the front instead,
    /// which is the right call when the pair is liquid enough that queue
    /// position hardly matters — a judgement only the trader can make.
    Opportunity,
    /// Join the top of the observed book and wait for the market to move.
    /// This is a bet on drift, not a better price for the same trade.
    Greedy,
}

impl MakerMode {
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::Opportunity, Self::Greedy]
    }

    /// Whether this mode's payoff depends on the market moving rather than
    /// on someone taking the order at today's prices.
    #[must_use]
    pub const fn is_speculative(self) -> bool {
        matches!(self, Self::Greedy)
    }
}

/// One listing in the competing queue, ordered front first.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakerQueueLevel {
    pub edge_id: String,
    pub rate: Ratio,
    pub comparator: Comparator,
    pub stock: u64,
    /// Depth in the `from` asset, when the stock basis allows converting it.
    pub depth_from: Option<AssetAmount>,
    /// Everything at or ahead of this level, in the `from` asset: what has to
    /// clear before a listing at this rate starts filling.
    pub depth_ahead_from: Option<AssetAmount>,
    pub freshness: FreshnessStatus,
    pub risk_flags: Vec<QuoteRiskFlag>,
}

/// A listing wall: one level holding far more than its neighbours.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakerWall {
    pub edge_id: String,
    pub rate: Ratio,
    pub stock: u64,
    /// Position in the front-first queue.
    pub queue_index: usize,
}

/// What to list, at what rate, and what it costs in risk.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakerRecommendation {
    pub mode: MakerMode,
    pub rate: Ratio,
    pub expected_amount_out: AssetAmount,
    /// Extra output versus taking the instant price now. `None` when there is
    /// no instant reference to compare against.
    pub improvement_over_instant: Option<AssetAmount>,
    pub improvement_basis_points: Option<i64>,
    /// Depth ahead of this rate in the queue: what fills before you do.
    pub depth_ahead_from: Option<AssetAmount>,
    /// True when this rate is at or beyond a wall.
    pub behind_wall: bool,
    /// True when the listing sits at the competing front rather than below
    /// it, so it queues behind the order already there.
    pub queued_behind_front: bool,
    /// False when the listing price is no better than simply taking the
    /// instant price, in which case listing is strictly worse than trading.
    pub beats_instant: bool,
    /// True when the payoff depends on the market moving rather than on
    /// someone taking the order at today's prices.
    pub is_speculative: bool,
    pub assessment: RiskAssessment,
}

/// The full maker picture for one pair at one size.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakerStrategy {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub amount_in: AssetAmount,
    /// The competing listings, front of queue first.
    pub queue: Vec<MakerQueueLevel>,
    /// Total visible listing depth, in the `from` asset.
    pub visible_depth_from: Option<AssetAmount>,
    /// Depth held by the front of the queue.
    pub front_depth_from: Option<AssetAmount>,
    /// How many levels the front-depth figure covers.
    pub front_level_count: usize,
    pub wall: Option<MakerWall>,
    pub instant_rate: Option<Ratio>,
    pub best_competing_rate: Option<Ratio>,
    /// Spread of the best listing over the instant price, in basis points.
    pub spread_basis_points: Option<i64>,
    /// Requested size as a fraction of visible depth, in basis points.
    pub size_to_depth_basis_points: Option<i64>,
    /// The largest single order the visible depth supports comfortably.
    pub suggested_max_single_order: Option<AssetAmount>,
    pub split_order_recommended: bool,
    pub recommendations: Vec<MakerRecommendation>,
    pub assessment: RiskAssessment,
}

/// Inputs for [`calculate_maker_strategy`].
#[derive(Clone, Copy, Debug)]
pub struct MakerRequest<'a> {
    pub from_asset_id: &'a MarketAssetId,
    pub to_asset_id: &'a MarketAssetId,
    pub amount_in: &'a AssetAmount,
    /// Competing listings for this pair. Anything that is not a maker
    /// reference on this exact pair is ignored rather than silently reversed.
    pub competing: &'a [EvaluatedQuoteEdge],
    /// The taker fill that would happen right now, when the available side
    /// has depth.
    pub instant: Option<&'a PairFill>,
    /// Price the opportunity listing *at* the competing front instead of one
    /// tick below it. Matching queues behind the order already there, which
    /// costs nothing when the pair turns over fast and costs a fill when it
    /// does not; the trade-off is the trader's to judge, not this crate's.
    pub match_front: bool,
    pub thresholds: RiskThresholds,
}

/// How many levels count as "the front" for sizing advice.
const FRONT_LEVEL_COUNT: usize = 3;

/// A wall holds at least this multiple of its neighbours' depth.
const WALL_NEIGHBOUR_FACTOR: u64 = 3;

/// ...and at least this much stock, so a 3-versus-1 blip is not a wall.
const WALL_MINIMUM_STOCK: u64 = 10;

/// The competing queue, front first.
///
/// The front is the *lowest* rate: a maker asking for less of the other
/// currency gets taken first, so that listing fills before yours. Deeper
/// stock breaks ties (it clears more slowly, so it is more of an obstacle),
/// then the edge id so the order is total and reproducible.
fn queue_order(left: &EvaluatedQuoteEdge, right: &EvaluatedQuoteEdge) -> Ordering {
    let left_edge = &left.observation.edge;
    let right_edge = &right.observation.edge;
    left_edge
        .rate
        .compare_value(&right_edge.rate)
        .then_with(|| right_edge.stock.cmp(&left_edge.stock))
        .then_with(|| left_edge.edge_id.cmp(&right_edge.edge_id))
}

/// Depth of one listing, in the asset its owner is giving away.
///
/// Settled by the panel rather than assumed: on a Divine/Mirror book the
/// available rows carry stock in exact multiples of the ratio numerator
/// (776 for 776:1, 2325 for 775:1 — one and three mirrors' worth of divine),
/// while the competing rows carry 2, 2, 1, 8 — mirror counts. Stock is always
/// denominated in what the lister pays out, which for a maker-reference edge
/// is the `from` asset. The engine's own maker fill reads it the same way.
fn depth_from(edge_stock: u64, from_reference: &AssetAmount) -> Option<AssetAmount> {
    let quanta = Rational::from_u128(u128::from(edge_stock))
        .checked_div(unit_value(from_reference.unit)?)?
        .floor_u64()?;
    Some(amount_like(from_reference, quanta))
}

/// The rate one displayed tick below `rate`, for undercutting the front.
///
/// The tick comes from how the panel writes the quote: "784 : 1" moves in
/// whole units on the left, "1 : 10.33" moves in hundredths on the right —
/// and note the directions oppose, since asking fewer of the other currency
/// per unit means a *larger* number on the right.
fn undercut(rate: &Ratio) -> Option<Ratio> {
    let (left, right) = rate.text.split_once(':')?;
    let (left, right) = (left.trim(), right.trim());
    let (left_coefficient, left_scale) = coefficient_and_scale(left)?;
    let (right_coefficient, right_scale) = coefficient_and_scale(right)?;

    if left_coefficient == 1 && left_scale == 0 {
        // "1 : X" — a larger X is a lower rate.
        let numerator = 10_u64.checked_pow(right_scale)?;
        Ratio::from_parts(numerator, right_coefficient.checked_add(1)?).ok()
    } else {
        let left_coefficient = left_coefficient.checked_sub(1).filter(|value| *value > 0)?;
        let numerator = left_coefficient.checked_mul(10_u64.checked_pow(right_scale)?)?;
        let denominator = right_coefficient.checked_mul(10_u64.checked_pow(left_scale)?)?;
        Ratio::from_parts(numerator, denominator).ok()
    }
}

/// Splits "10.33" into (1033, 2).
fn coefficient_and_scale(value: &str) -> Option<(u64, u32)> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let scale = u32::try_from(fraction.len()).ok()?;
    let digits = format!("{whole}{fraction}");
    digits.parse::<u64>().ok().map(|value| (value, scale))
}

fn edge_risks(evaluated: &EvaluatedQuoteEdge, risks: &mut BTreeSet<ExecutionRisk>) {
    for flag in &evaluated.risk_flags {
        match flag {
            QuoteRiskFlag::ComparatorBoundary => {
                risks.insert(ExecutionRisk::ComparatorBoundary);
            }
            QuoteRiskFlag::StaleData => {
                risks.insert(ExecutionRisk::StaleData);
            }
            QuoteRiskFlag::ArchivedData => {
                risks.insert(ExecutionRisk::ArchivedData);
            }
            QuoteRiskFlag::LowConfidence => {
                risks.insert(ExecutionRisk::LowConfidence);
            }
            QuoteRiskFlag::FutureTimestamp => {
                risks.insert(ExecutionRisk::ClockSkewFuture);
            }
            QuoteRiskFlag::PriceOutlier => {
                risks.insert(ExecutionRisk::PriceOutlier);
            }
            QuoteRiskFlag::OutsideTopBookBand => {
                risks.insert(ExecutionRisk::OutsideTopBookBand);
            }
            QuoteRiskFlag::IsolatedRecord | QuoteRiskFlag::DeletedRecord => {
                risks.insert(ExecutionRisk::UnsupportedRecord);
            }
            QuoteRiskFlag::ReverseFromAvailable | QuoteRiskFlag::ReverseFromCompeting => {
                risks.insert(ExecutionRisk::CompetingReference);
            }
        }
    }
}

/// Build the maker picture for one pair.
pub fn calculate_maker_strategy(request: MakerRequest<'_>) -> Result<MakerStrategy, StrategyError> {
    if request.from_asset_id == request.to_asset_id || request.amount_in.quanta == 0 {
        return Err(StrategyError::InvalidRequest);
    }

    let mut listings: Vec<&EvaluatedQuoteEdge> = request
        .competing
        .iter()
        .filter(|evaluated| {
            let edge = &evaluated.observation.edge;
            edge.from_asset_id == *request.from_asset_id
                && edge.to_asset_id == *request.to_asset_id
                && edge.execution_type == ExecutionType::MakerReference
                && edge.stock > 0
        })
        .collect();
    listings.sort_by(|left, right| queue_order(left, right));

    let to_unit = request
        .instant
        .map_or(request.amount_in.unit, |fill| fill.gross_amount_out.unit);
    let to_reference = AssetAmount {
        asset_id: request.to_asset_id.clone(),
        quanta: 0,
        unit: to_unit,
    };

    // One pass down the queue, accumulating what sits ahead of each level.
    let mut queue = Vec::with_capacity(listings.len());
    let mut cumulative_from: Option<u64> = None;
    let mut front_depth_quanta: Option<u64> = None;
    for (index, evaluated) in listings.iter().enumerate() {
        let edge = &evaluated.observation.edge;
        let level_depth = depth_from(edge.stock, request.amount_in);
        if let Some(depth) = &level_depth {
            cumulative_from = Some(cumulative_from.unwrap_or(0).saturating_add(depth.quanta));
            if index < FRONT_LEVEL_COUNT {
                front_depth_quanta =
                    Some(front_depth_quanta.unwrap_or(0).saturating_add(depth.quanta));
            }
        }
        queue.push(MakerQueueLevel {
            edge_id: edge.edge_id.clone(),
            rate: edge.rate.clone(),
            comparator: edge.comparator,
            stock: edge.stock,
            depth_from: level_depth,
            depth_ahead_from: cumulative_from.map(|quanta| amount_like(request.amount_in, quanta)),
            freshness: evaluated.freshness.status,
            risk_flags: evaluated.risk_flags.clone(),
        });
    }

    let visible_depth_from = cumulative_from.map(|quanta| amount_like(request.amount_in, quanta));
    let front_depth_from = front_depth_quanta.map(|quanta| amount_like(request.amount_in, quanta));
    let wall = detect_wall(&queue);

    let instant_rate = request.instant.and_then(instant_rate_of);
    let best_competing_rate = queue.last().map(|level| level.rate.clone());
    let spread_basis_points = match (&instant_rate, &best_competing_rate) {
        (Some(instant), Some(best)) => {
            match (rational_from_ratio(best), rational_from_ratio(instant)) {
                (Some(best), Some(instant)) => basis_points(best, instant),
                _ => None,
            }
        }
        _ => None,
    };

    let size_to_depth_basis_points = visible_depth_from.as_ref().and_then(|depth| {
        basis_points(
            Rational::from_u128(u128::from(request.amount_in.quanta)),
            Rational::from_u128(u128::from(depth.quanta)),
        )
        .map(|value| value + 10_000)
    });
    // Never advise a single order larger than the front of the queue can
    // absorb, nor more than a third of everything visible.
    let suggested_max_single_order = match (&front_depth_from, &visible_depth_from) {
        (Some(front), Some(visible)) => Some(amount_like(
            request.amount_in,
            front.quanta.min(visible.quanta / 3).max(1),
        )),
        _ => None,
    };
    let split_order_recommended = visible_depth_from
        .as_ref()
        .is_some_and(|depth| request.amount_in.quanta > depth.quanta);

    let mut book_risks = BTreeSet::new();
    let mut caveats = BTreeSet::new();
    caveats.insert(ModelCaveat::GrossProfitOnly);
    caveats.insert(ModelCaveat::AdvisoryOnly);
    for evaluated in &listings {
        edge_risks(evaluated, &mut book_risks);
    }
    if listings.is_empty() {
        book_risks.insert(ExecutionRisk::NeedsProbe);
    } else {
        // Listing is always a maker act, whatever the book looks like.
        book_risks.insert(ExecutionRisk::MakerReference);
    }
    if split_order_recommended {
        book_risks.insert(ExecutionRisk::MakerDepthExceeded);
    }
    if let Some(depth) = &visible_depth_from
        && depth.quanta > 0
        && depth.quanta < request.thresholds.thin_liquidity_stock
    {
        book_risks.insert(ExecutionRisk::ThinLiquidity);
    }

    let recommendations = MakerMode::all()
        .into_iter()
        .filter_map(|mode| {
            recommendation(
                mode,
                &queue,
                instant_rate.as_ref(),
                wall.as_ref(),
                request,
                &to_reference,
                &book_risks,
                &caveats,
            )
        })
        .collect::<Vec<_>>();

    let assessment = RiskAssessment {
        actionability: Actionability::safest(
            recommendations
                .iter()
                .map(|item| item.assessment.actionability),
        ),
        risks: book_risks,
        caveats,
    };

    Ok(MakerStrategy {
        from_asset_id: request.from_asset_id.clone(),
        to_asset_id: request.to_asset_id.clone(),
        amount_in: request.amount_in.clone(),
        queue,
        visible_depth_from,
        front_depth_from,
        front_level_count: listings.len().min(FRONT_LEVEL_COUNT),
        wall,
        instant_rate,
        best_competing_rate,
        spread_basis_points,
        size_to_depth_basis_points,
        suggested_max_single_order,
        split_order_recommended,
        recommendations,
        assessment,
    })
}

/// The blended rate the instant fill actually achieved.
fn instant_rate_of(fill: &PairFill) -> Option<Ratio> {
    let realized = fill
        .net_amount_out
        .as_ref()
        .unwrap_or(&fill.gross_amount_out);
    if fill.consumed_input.quanta == 0 {
        return None;
    }
    let scale = Rational::new(
        u128::from(realized.quanta),
        u128::from(fill.consumed_input.quanta),
    )?;
    quanta_scale_to_rate(scale, fill.consumed_input.unit, realized.unit)
}

/// A level holding far more than the levels either side of it.
fn detect_wall(queue: &[MakerQueueLevel]) -> Option<MakerWall> {
    if queue.len() < 2 {
        return None;
    }
    for (index, level) in queue.iter().enumerate() {
        if level.stock < WALL_MINIMUM_STOCK {
            continue;
        }
        let neighbours: Vec<u64> = [index.checked_sub(1), Some(index + 1)]
            .into_iter()
            .flatten()
            .filter_map(|neighbour| queue.get(neighbour))
            .map(|neighbour| neighbour.stock)
            .collect();
        if neighbours.is_empty() {
            continue;
        }
        let total: u64 = neighbours.iter().sum();
        let mean = total / u64::try_from(neighbours.len()).unwrap_or(1);
        if mean > 0 && level.stock >= mean.saturating_mul(WALL_NEIGHBOUR_FACTOR) {
            return Some(MakerWall {
                edge_id: level.edge_id.clone(),
                rate: level.rate.clone(),
                stock: level.stock,
                queue_index: index,
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn recommendation(
    mode: MakerMode,
    queue: &[MakerQueueLevel],
    instant_rate: Option<&Ratio>,
    wall: Option<&MakerWall>,
    request: MakerRequest<'_>,
    to_reference: &AssetAmount,
    book_risks: &BTreeSet<ExecutionRisk>,
    caveats: &BTreeSet<ModelCaveat>,
) -> Option<MakerRecommendation> {
    // Opportunity reads the front of the queue and prices below it; greedy
    // reads the back and joins it. An empty book supports neither — the only
    // honest number left is the taker price, and that is not a listing.
    let level = match mode {
        MakerMode::Opportunity => queue.first()?,
        MakerMode::Greedy => queue.last()?,
    };
    let rate = match mode {
        // Matching the front queues behind it: that order was there first
        // and clears first. Undercutting by one displayed tick buys the
        // front outright — worth it unless the pair turns over fast enough
        // that position barely matters.
        MakerMode::Opportunity if request.match_front => level.rate.clone(),
        MakerMode::Opportunity => undercut(&level.rate)?,
        MakerMode::Greedy => level.rate.clone(),
    };
    let level = Some(level);

    let scale = rate_to_quanta_scale(&rate, request.amount_in.unit, to_reference.unit)?;
    let expected_quanta = apply_scale(request.amount_in.quanta, scale)?;
    let expected_amount_out = amount_like(to_reference, expected_quanta);

    let (improvement_over_instant, improvement_basis_points) = match instant_rate {
        Some(instant) => {
            let instant_scale =
                rate_to_quanta_scale(instant, request.amount_in.unit, to_reference.unit)?;
            let instant_quanta = apply_scale(request.amount_in.quanta, instant_scale)?;
            let delta = expected_quanta.saturating_sub(instant_quanta);
            let bps = basis_points(rational_from_ratio(&rate)?, rational_from_ratio(instant)?);
            (Some(amount_like(to_reference, delta)), bps)
        }
        None => (None, None),
    };

    // Listing below the price you could take right now is strictly worse
    // than taking it, however good it looks against the competing book.
    let beats_instant =
        instant_rate.is_none_or(|instant| rate.compare_value(instant) == Ordering::Greater);

    let mut risks = book_risks.clone();
    let caveats = caveats.clone();
    if !beats_instant {
        risks.insert(ExecutionRisk::NeedsProbe);
    }
    if let Some(level) = level {
        for flag in &level.risk_flags {
            if matches!(flag, QuoteRiskFlag::ComparatorBoundary) {
                risks.insert(ExecutionRisk::ComparatorBoundary);
            }
        }
    }
    let behind_wall = wall.is_some_and(|wall| {
        level.is_some_and(|level| level.rate.compare_value(&wall.rate) != Ordering::Less)
    });
    if behind_wall {
        risks.insert(ExecutionRisk::MakerDepthExceeded);
    }
    let assessment = RiskAssessment {
        actionability: actionability_for(&risks),
        risks,
        caveats,
    };

    Some(MakerRecommendation {
        mode,
        rate,
        expected_amount_out,
        improvement_over_instant,
        improvement_basis_points,
        depth_ahead_from: level.and_then(|level| level.depth_ahead_from.clone()),
        behind_wall,
        queued_behind_front: mode == MakerMode::Opportunity && request.match_front,
        beats_instant,
        is_speculative: mode.is_speculative(),
        assessment,
    })
}
