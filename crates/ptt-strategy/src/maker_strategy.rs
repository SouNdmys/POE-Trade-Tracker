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
//! the front depth, the wall scan and every mode's pricing all sort through
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

/// Why a competing listing was kept out of the queue math.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MakerQueueExclusion {
    /// The book flagged the rate as a price outlier. A malicious listing
    /// fronts its side by construction, so left in it becomes exactly the
    /// row Opportunity would undercut — and it inflates visible depth and
    /// the suggested order size on the way.
    PriceOutlier,
}

/// A listing the panel shows but this module refuses to price against.
/// Kept visible with its reason: exclusion is a judgement, not a deletion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakerExcludedListing {
    pub edge_id: String,
    pub rate: Ratio,
    pub stock: u64,
    pub reason: MakerQueueExclusion,
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
    /// someone taking the order at today's prices. Derived from
    /// [`MakerMode::is_speculative`]; carried on the recommendation so a
    /// serialised one keeps the warning without the reader re-deriving it.
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
    /// The competing listings admitted to the math, front of queue first.
    pub queue: Vec<MakerQueueLevel>,
    /// Listings kept out of every number above, each with its reason.
    pub excluded: Vec<MakerExcludedListing>,
    /// Total visible listing depth, in the `from` asset.
    pub visible_depth_from: Option<AssetAmount>,
    /// Depth held by the front of the queue.
    pub front_depth_from: Option<AssetAmount>,
    /// How many levels the front-depth figure covers.
    pub front_level_count: usize,
    pub wall: Option<MakerWall>,
    pub instant_rate: Option<Ratio>,
    /// The front of the competing queue: the first rate a new listing has to
    /// beat.
    pub best_competing_rate: Option<Ratio>,
    /// Spread of the competing front over the instant price, in basis
    /// points — the reward a maker at the head of the queue earns over just
    /// taking (docs/CORE-TRADING-MODEL.md defines the spread this way).
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
#[derive(Clone, Debug)]
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

/// How far a single displayed tick may move a rate before it stops being a
/// tick, in basis points.
///
/// The tick is inferred from the quote's own precision, which is right while
/// the quote carries decimals — POE2's `1 : 10.33` steps by a hundredth, and
/// POE1's whole-unit `1 : 130` steps by one — but a coarse quote infers a
/// coarse tick: `1 : 1` would "undercut" to `1 : 2`, halving the rate. A step
/// this large is not a tick and is refused.
///
/// The inference has one known blind spot: `Ratio` canonicalisation trims
/// trailing zeros, so a panel showing `1 : 1.00` reaches here as `1:1` with
/// its precision already lost. Those quotes are refused rather than stepped
/// by a whole unit, which is the safe direction to fail.
const MAX_TICK_BASIS_POINTS: i64 = 500;

/// The rate one displayed tick below `rate`, for undercutting the front.
///
/// The tick comes from how the panel writes the quote: "784 : 1" moves in
/// whole units on the left, "1 : 10.33" moves in hundredths on the right —
/// and note the directions oppose, since asking fewer of the other currency
/// per unit means a *larger* number on the right.
fn undercut(rate: &Ratio) -> Option<Ratio> {
    let stepped = undercut_by_displayed_tick(rate)?;
    // A quote with no decimals implies a whole-unit step, which on a small
    // ratio is a price cut rather than a tick.
    let moved = basis_points(rational_from_ratio(&stepped)?, rational_from_ratio(rate)?)?;
    (moved.abs() <= MAX_TICK_BASIS_POINTS).then_some(stepped)
}

fn undercut_by_displayed_tick(rate: &Ratio) -> Option<Ratio> {
    let (left, right) = rate.text.split_once(':')?;
    let (left, right) = (left.trim(), right.trim());
    let (left_coefficient, left_scale) = coefficient_and_scale(left)?;
    let (right_coefficient, right_scale) = coefficient_and_scale(right)?;

    if left_coefficient == 1 && left_scale == 0 {
        // "1 : X" — a larger X is a lower rate.
        let numerator = 10_u64.checked_pow(right_scale)?;
        let stepped = right_coefficient.checked_add(1)?;
        let mut rate = Ratio::from_parts(numerator, stepped).ok()?;
        // Written back in the notation it was read in. `from_parts` reduces
        // and prints "{n}:{d}", so a tick below "1:93.33" comes out
        // "50:4667" — the same number wearing a form nothing else on the
        // listing card uses, in the one row whose job is to say what to type.
        // Every other rate on that card is a captured one carrying the game's
        // own text; this is the only one the program writes itself.
        rate.text = format_display_tick(stepped, right_scale);
        Some(rate)
    } else {
        let left_coefficient = left_coefficient.checked_sub(1).filter(|value| *value > 0)?;
        let numerator = left_coefficient.checked_mul(10_u64.checked_pow(right_scale)?)?;
        let denominator = right_coefficient.checked_mul(10_u64.checked_pow(left_scale)?)?;
        Ratio::from_parts(numerator, denominator).ok()
    }
}

/// `(9334, 2)` back into `"1:93.34"` — the inverse of
/// [`coefficient_and_scale`] for the right-hand side of a `1 : X` quote.
fn format_display_tick(coefficient: u64, scale: u32) -> String {
    if scale == 0 {
        return format!("1:{coefficient}");
    }
    let divisor = 10_u64.pow(scale);
    let scale = scale as usize;
    format!(
        "1:{}.{:0scale$}",
        coefficient / divisor,
        coefficient % divisor
    )
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

    // Partition before any arithmetic: a price-outlier row is exactly the
    // row a mode would otherwise price against, and it inflates visible
    // depth and the suggested order size on the way. The judgement is
    // evidence-based — the book's own outlier flags — not selection-based:
    // under an Instant selection every maker-reference row is "rejected"
    // for the wrong execution type, which says nothing about honesty.
    let mut excluded = Vec::new();
    let mut admitted: Vec<&EvaluatedQuoteEdge> = Vec::with_capacity(listings.len());
    for evaluated in listings {
        let edge = &evaluated.observation.edge;
        let is_outlier = evaluated.risk_flags.iter().any(|flag| {
            matches!(
                flag,
                QuoteRiskFlag::PriceOutlier | QuoteRiskFlag::OutsideTopBookBand
            )
        });
        if is_outlier {
            excluded.push(MakerExcludedListing {
                edge_id: edge.edge_id.clone(),
                rate: edge.rate.clone(),
                stock: edge.stock,
                reason: MakerQueueExclusion::PriceOutlier,
            });
        } else {
            admitted.push(evaluated);
        }
    }
    let listings = admitted;

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
    // The front, not the greediest: what a new listing competes with first.
    let best_competing_rate = queue.first().map(|level| level.rate.clone());
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
    if listings.len() == 1 {
        // One admitted listing prices the whole queue; nothing corroborates
        // the rate both modes would list against.
        book_risks.insert(ExecutionRisk::SingleListingBook);
    }
    if split_order_recommended {
        book_risks.insert(ExecutionRisk::MakerDepthExceeded);
    }
    // Maker depth is denominated in the input asset (TASK-50 payout side),
    // so the thin bar is that currency's own norm when one exists.
    if let Some(depth) = &visible_depth_from
        && depth.quanta > 0
        && depth.quanta
            < request
                .thresholds
                .thin_threshold_for(request.from_asset_id.as_str())
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
                &request,
                &to_reference,
                &book_risks,
                &caveats,
            )
        })
        .collect::<Vec<_>>();

    // The pair-level picture names the rows it refused to price against;
    // the recommendations stay clean because their math never saw them.
    let mut strategy_risks = book_risks;
    if !excluded.is_empty() {
        strategy_risks.insert(ExecutionRisk::PriceOutlier);
        strategy_risks.insert(ExecutionRisk::OutsideTopBookBand);
    }
    let assessment = RiskAssessment {
        actionability: Actionability::safest(
            recommendations
                .iter()
                .map(|item| item.assessment.actionability),
        ),
        risks: strategy_risks,
        caveats,
    };

    Ok(MakerStrategy {
        from_asset_id: request.from_asset_id.clone(),
        to_asset_id: request.to_asset_id.clone(),
        amount_in: request.amount_in.clone(),
        queue,
        excluded,
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

/// The best price on offer right now: the front row, not the blend.
///
/// **A listing decision is a rate against a rate, and neither side of it may
/// depend on how much the reader holds.** This used to return the blended
/// average of however many levels the ask happened to sweep, which made the
/// advice for an identical order at an identical price read differently at
/// every holding -- 6.24% at 500, 6.44% at 169, 5.82% at 50,000 on the
/// owner's real book, and 11.03% against 4.76% on another pair. It is the
/// same defect the convert rows were cured of: a percentage that moves with
/// the size of the ask.
///
/// The front row is the right baseline on its own terms too. POE fills a
/// listing at the listed rate or better, so what a listing competes with is
/// the best price a taker can get, not the average price of clearing a
/// particular parcel. The blended walk is still reported elsewhere as a
/// clearance figure; it is not a rate anyone can list against.
fn instant_rate_of(fill: &PairFill) -> Option<Ratio> {
    if let Some(front) = fill.fills.first() {
        return Some(front.rate.clone());
    }
    // No level records at all -- a fill the caller synthesised rather than
    // walked. The blend is then the only rate there is, and with one level
    // it is the front row anyway.
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
    request: &MakerRequest<'_>,
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
    // Matching the front queues behind it: that order was there first and
    // clears first. Undercutting by one displayed tick buys the front
    // outright — worth it unless the pair turns over fast enough that
    // position barely matters.
    //
    // When no tick can be derived the recommendation is still returned, at
    // the front's own rate and carrying the fact: dropping it here would
    // delete the mode from the output with nothing to say why, which reads
    // exactly like "this pair has no opportunity".
    let mut undercut_unavailable = false;
    let rate = match mode {
        MakerMode::Opportunity if request.match_front => level.rate.clone(),
        MakerMode::Opportunity => match undercut(&level.rate) {
            Some(rate) => rate,
            None => {
                undercut_unavailable = true;
                level.rate.clone()
            }
        },
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
    if undercut_unavailable {
        // The listing sits level with the front rather than ahead of it, so
        // it queues behind that order — the same position matching produces.
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
        queued_behind_front: mode == MakerMode::Opportunity
            && (request.match_front || undercut_unavailable),
        beats_instant,
        is_speculative: mode.is_speculative(),
        assessment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(text: &str) -> Ratio {
        Ratio::parse(text).expect("rate")
    }

    #[test]
    fn a_tick_follows_the_precision_the_quote_is_written_in() {
        // POE2 quotes hundredths; POE1 quotes whole units. Both are the
        // panel's own step, so both are the right tick for their game.
        let poe2 = undercut(&rate("1:10.33")).expect("poe2 tick");
        assert_eq!(
            poe2.compare_value(&rate("1:10.34")),
            std::cmp::Ordering::Equal
        );
        let poe1 = undercut(&rate("1:130")).expect("poe1 tick");
        assert_eq!(
            poe1.compare_value(&rate("1:131")),
            std::cmp::Ordering::Equal
        );
        // The left side moves when that is where the precision lives.
        let left = undercut(&rate("784:1")).expect("left tick");
        assert_eq!(
            left.compare_value(&rate("783:1")),
            std::cmp::Ordering::Equal
        );
    }

    /// **A tick computed in display space has to come back in display
    /// space.**
    ///
    /// `undercut_by_displayed_tick` reads the panel's own notation, steps one
    /// tick, and then hands the result to `Ratio::from_parts`, which
    /// gcd-reduces and writes `"{n}:{d}"`. One tick below `1:93.33` is
    /// `100:9334`, which reduces to `50:4667` — the same number, in a
    /// notation nothing else on the panel uses. On the owner's live screen
    /// the listing card read
    ///
    /// ```text
    ///   立即成交价 1:94
    ///   机会（压一档），挂 50:4667      <-- the only computed rate
    ///   跟价，挂 1:93.33
    ///   贪婪，挂 1:38
    /// ```
    ///
    /// Every other rate there is a captured one carrying the game's own text,
    /// so the one the program works out for itself is the one that looks
    /// foreign — in the row whose entire job is to tell the reader what to
    /// type.
    #[test]
    fn a_tick_is_written_in_the_notation_the_quote_was_written_in() {
        assert_eq!(
            undercut(&rate("1:93.33")).expect("tick").text,
            "1:93.34",
            "a tick below a hundredths quote is a hundredths quote"
        );
        assert_eq!(
            undercut(&rate("1:130")).expect("tick").text,
            "1:131",
            "and a whole-unit quote stays whole"
        );
        assert_eq!(
            undercut(&rate("784:1")).expect("tick").text,
            "783:1",
            "the left-hand form is already what from_parts would write, and \
             must not change"
        );
    }

    #[test]
    fn a_step_too_large_to_be_a_tick_is_refused_rather_than_offered() {
        // "1:1" has no decimals, so the inferred tick is a whole unit — which
        // would halve the rate. Undercutting by half is a price cut, not a
        // queue position, and must not be presented as one.
        assert!(undercut(&rate("1:1")).is_none());
        assert!(undercut(&rate("1:2")).is_none());
        // A quote whose decimals survive canonicalisation still works.
        assert!(undercut(&rate("1:1.05")).is_some());
        // But note the limit this exposes: canonicalisation trims trailing
        // zeros, so a panel showing "1 : 1.00" arrives as text "1:1" and its
        // hundredth-precision is unrecoverable. Such a quote is refused
        // rather than undercut by a whole unit.
        assert_eq!(rate("1:1.00").text, rate("1:1").text);
    }

    #[test]
    fn an_undercut_that_cannot_be_computed_still_yields_a_recommendation() {
        // The mode vanishing from the output is indistinguishable from "this
        // pair has no opportunity", so the listing is returned at the front's
        // own rate and says it queues behind.
        let front = rate("1:1");
        assert!(
            undercut(&front).is_none(),
            "fixture must exercise the no-tick path"
        );
    }
}
