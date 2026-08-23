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
        // Trust first, price second: what you could actually trade against,
        // then the newest, then the deepest, and only then the best rate.
        //
        // Depth is the key that decides in practice: a whole panel is read in
        // one go, so every level of a book carries the same `captured_at` and
        // the first two keys tie across all of them. Depth breaks that tie
        // because a two-item listing is the row most likely to be a
        // fat-fingered price and the first to disappear — letting it set the
        // valuation pegs the number to the least reliable line on the screen.
        //
        // Rate has the last word, and not merely to be deterministic. Two
        // listings that are equally takeable, equally recent and equally deep
        // are equally trustworthy, so the argument that put depth above price
        // has nothing left to say about them, and the instant tier is defined
        // as hitting the best available level: you would sell into the 44:1
        // order before the 41:1 one sitting next to it, and `rate` is always
        // `to` per `from`, so "largest wins" is the best bid on one side and
        // the cheapest ask on the other. It also keeps `spread_basis_points`
        // meaning what a spread means — best bid against best ask, not two
        // arbitrary levels. Without this key the winner of a tie at the
        // deepest level is whoever the candidate list happens to hand over
        // last, which valued one real book 7.3% apart depending on nothing.
        .max_by(|left, right| {
            let taker = |edge: &EvaluatedQuoteEdge| {
                edge.observation.edge.execution_type == ExecutionType::Taker
            };
            taker(left)
                .cmp(&taker(right))
                .then_with(|| {
                    left.observation
                        .edge
                        .captured_at
                        .cmp(&right.observation.edge.captured_at)
                })
                .then_with(|| {
                    left.observation
                        .edge
                        .stock
                        .cmp(&right.observation.edge.stock)
                })
                .then_with(|| {
                    left.observation
                        .edge
                        .rate
                        .compare_value(&right.observation.edge.rate)
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
    use std::cmp::Ordering;

    use chrono::{Duration, TimeZone, Utc};
    use ptt_market_book::{EvaluatedQuoteEdge, FreshnessAssessment, FreshnessStatus};
    use ptt_trade_domain::{
        Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
        QuoteSide, Ratio, SnapshotRecordStatus,
    };

    use super::{Valuation, ValuationMode, ValuationRequest, value_against_anchor};
    use crate::exact::Rational;
    use crate::execution_safety::ExecutionRisk;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// One row of a panel: same capture instant and same taker flag for every
    /// level, which is what a single screenshot of one book actually yields.
    fn book_edge(from: &str, to: &str, rate: &str, stock: u64) -> EvaluatedQuoteEdge {
        let captured_at = Utc
            .with_ymd_and_hms(2026, 8, 23, 2, 22, 20)
            .single()
            .expect("timestamp");
        EvaluatedQuoteEdge {
            observation: MarketEdgeObservation {
                edge: QuoteEdge {
                    edge_id: format!("{from}->{to}@{rate}x{stock}"),
                    snapshot_id: "snapshot".to_owned(),
                    quote_id: format!("quote-{rate}-{stock}"),
                    context_key: "test-context".to_owned(),
                    from_asset_id: asset(from),
                    to_asset_id: asset(to),
                    rate: Ratio::parse(rate).expect("rate"),
                    source_side: QuoteSide::Available,
                    execution_type: ExecutionType::Taker,
                    role: QuoteEdgeRole::AvailableTaker,
                    stock,
                    original_need_asset_id: asset(to),
                    original_have_asset_id: asset(from),
                    original_row_index: 0,
                    comparator: Comparator::Exact,
                    user_edited: false,
                    machine_confidence_ppm: Some(990_000),
                    captured_at,
                    confirmed_at: captured_at,
                },
                snapshot_complete: true,
                record_status: SnapshotRecordStatus::Active,
                record_revision: 1,
                record_reason: None,
            },
            freshness: FreshnessAssessment {
                status: FreshnessStatus::Fresh,
                age_seconds: 60,
                future_timestamp: false,
            },
            effective_confidence_ppm: 990_000,
            risk_flags: Vec::new(),
            selection_rejections: Vec::new(),
            execution_blockers: Vec::new(),
            accepted_for_selection: true,
            eligible_for_depth_analysis: true,
        }
    }

    /// A competing listing: the same row, but on the side of the book you can
    /// only read off the screen and never trade against.
    fn maker_edge(from: &str, to: &str, rate: &str, stock: u64) -> EvaluatedQuoteEdge {
        let mut evaluated = book_edge(from, to, rate, stock);
        evaluated.observation.edge.execution_type = ExecutionType::MakerReference;
        evaluated.observation.edge.role = QuoteEdgeRole::CompetingMakerReference;
        evaluated
    }

    /// The same row read `minutes` earlier — a second panel of the same book.
    /// Freshness is an input to this file rather than something it computes,
    /// so the status stays Fresh and only the age moves with the timestamp.
    fn aged_edge(from: &str, to: &str, rate: &str, stock: u64, minutes: i64) -> EvaluatedQuoteEdge {
        let mut evaluated = book_edge(from, to, rate, stock);
        let earlier = evaluated.observation.edge.captured_at - Duration::minutes(minutes);
        evaluated.observation.edge.captured_at = earlier;
        evaluated.observation.edge.confirmed_at = earlier;
        evaluated.freshness.age_seconds += u64::try_from(minutes.max(0)).unwrap_or(0) * 60;
        evaluated
    }

    /// The ancient-clavicle book as the 2026-08-23 run captured it, taker
    /// levels only and in the order the Instant strategy hands them over
    /// (stock descending). One screenshot, so every level shares a capture
    /// instant and every tie-break above depth is a dead heat.
    fn ancient_clavicle_book() -> Vec<EvaluatedQuoteEdge> {
        vec![
            book_edge("ancient-clavicle", "chaos-orb", "44:1", 3740),
            book_edge("ancient-clavicle", "chaos-orb", "41:1", 2705),
            book_edge("ancient-clavicle", "chaos-orb", "43:1", 344),
            book_edge("ancient-clavicle", "chaos-orb", "45:1", 180),
            book_edge("ancient-clavicle", "chaos-orb", "45.33:1", 136),
            book_edge("ancient-clavicle", "chaos-orb", "41:1", 41),
            book_edge("chaos-orb", "ancient-clavicle", "1:49", 99),
            book_edge("chaos-orb", "ancient-clavicle", "1:48", 8),
            book_edge("chaos-orb", "ancient-clavicle", "1:48.75", 8),
            book_edge("chaos-orb", "ancient-clavicle", "1:47.5", 4),
            book_edge("chaos-orb", "ancient-clavicle", "1:49", 3),
            book_edge("chaos-orb", "ancient-clavicle", "1:47", 2),
        ]
    }

    /// Every rotation of a candidate list, each one also reversed. A rotation
    /// walks every edge through every queue position without disturbing the
    /// list itself, which is exactly the dependency a valuation must not have;
    /// for a three-edge list the rotations and their reverses are all six
    /// permutations.
    fn every_queue_position(edges: &[EvaluatedQuoteEdge]) -> Vec<Vec<EvaluatedQuoteEdge>> {
        let mut orderings = Vec::new();
        for offset in 0..edges.len() {
            let mut rotated = edges[offset..].to_vec();
            rotated.extend_from_slice(&edges[..offset]);
            let mut reversed = rotated.clone();
            reversed.reverse();
            orderings.push(rotated);
            orderings.push(reversed);
        }
        orderings
    }

    fn clavicle_valuation(edges: &[EvaluatedQuoteEdge]) -> Valuation {
        value_against_anchor(ValuationRequest {
            asset_id: &asset("ancient-clavicle"),
            anchor_asset_id: &asset("chaos-orb"),
            mode: ValuationMode::Midpoint,
            edges,
            include_historical: false,
        })
    }

    fn is_rate(actual: Option<&Ratio>, expected: &str) -> bool {
        actual.is_some_and(|rate| {
            rate.compare_value(&Ratio::parse(expected).expect("expected rate")) == Ordering::Equal
        })
    }

    #[test]
    fn a_valuation_reads_the_deepest_level_when_the_whole_book_shares_one_capture() {
        let target = asset("ancient-clavicle");
        let anchor = asset("chaos-orb");
        let edges = ancient_clavicle_book();
        let valuation = value_against_anchor(ValuationRequest {
            asset_id: &target,
            anchor_asset_id: &anchor,
            mode: ValuationMode::Midpoint,
            edges: &edges,
            include_historical: false,
        });

        let sell = valuation.sell_value.expect("sell value");
        assert_eq!(
            sell.compare_value(&Ratio::parse("44:1").expect("deep sell rate")),
            Ordering::Equal,
            "sell value came from the thin stock=41 level instead of stock=3740"
        );

        let buy = valuation.buy_cost.expect("buy cost");
        assert_eq!(
            buy.compare_value(&Ratio::parse("49:1").expect("deep buy rate")),
            Ordering::Equal,
            "buy cost came from the thin stock=2 level instead of stock=99"
        );
    }

    #[test]
    fn the_same_book_values_the_same_however_the_candidate_list_is_ordered() {
        // A valuation has to be a property of the book, not of the order the
        // book happens to arrive in. This is what tells "compare depth out
        // loud" apart from "take whichever one is first", which agree on a
        // stock-descending list and so cannot be told apart by the fixture
        // above on its own.
        let book = ancient_clavicle_book();
        let expected = clavicle_valuation(&book);
        for (index, ordering) in every_queue_position(&book).into_iter().enumerate() {
            let valuation = clavicle_valuation(&ordering);
            assert_eq!(
                valuation, expected,
                "ordering {index} valued the same book differently"
            );
            assert!(
                is_rate(valuation.sell_value.as_ref(), "44:1"),
                "ordering {index} sold at {:?} instead of the deepest level",
                valuation.sell_value
            );
            assert!(
                is_rate(valuation.buy_cost.as_ref(), "49:1"),
                "ordering {index} bought at {:?} instead of the deepest level",
                valuation.buy_cost
            );
        }
    }

    #[test]
    fn a_takeable_listing_outranks_a_reference_price_of_the_same_depth() {
        // Same instant, same depth, and the one you cannot trade against
        // quotes the better rate — so every key below the first points at it.
        // Only "prefer what you could actually trade against" keeps the
        // valuation on the 41:1 order that is really there to be sold into.
        let edges = vec![
            maker_edge("ancient-clavicle", "chaos-orb", "44:1", 500),
            book_edge("ancient-clavicle", "chaos-orb", "41:1", 500),
        ];
        for (index, ordering) in every_queue_position(&edges).into_iter().enumerate() {
            let valuation = clavicle_valuation(&ordering);
            assert!(
                is_rate(valuation.sell_value.as_ref(), "41:1"),
                "ordering {index} valued at {:?}, a price nobody is offering to trade at",
                valuation.sell_value
            );
            assert!(
                !valuation.risks.contains(&ExecutionRisk::MakerReference),
                "ordering {index} reported a reference-price risk for a takeable quote"
            );
        }
    }

    #[test]
    fn the_newest_quote_wins_when_two_reads_of_a_book_disagree() {
        // Two panels of one book half an hour apart, equally deep, and the
        // older read quotes the better rate. Age has to decide here: a price
        // from thirty minutes ago is a price that may not exist any more,
        // however flattering it is.
        let edges = vec![
            aged_edge("ancient-clavicle", "chaos-orb", "44:1", 500, 30),
            book_edge("ancient-clavicle", "chaos-orb", "41:1", 500),
        ];
        for (index, ordering) in every_queue_position(&edges).into_iter().enumerate() {
            let valuation = clavicle_valuation(&ordering);
            assert!(
                is_rate(valuation.sell_value.as_ref(), "41:1"),
                "ordering {index} valued at {:?}, taken from the half-hour-old panel",
                valuation.sell_value
            );
        }
    }

    #[test]
    fn a_tie_at_the_deepest_level_is_settled_by_price_not_by_position() {
        // Two listings tied at the deepest stock this book has, and a thin one
        // under them. Depth cannot separate the top two, so without a key
        // below it the winner was whichever one the candidate list handed over
        // last: the same three rows valued the clavicle at 44 or at 41 — 7.3%
        // apart — depending on nothing at all.
        let edges = vec![
            book_edge("ancient-clavicle", "chaos-orb", "44:1", 500),
            book_edge("ancient-clavicle", "chaos-orb", "41:1", 500),
            book_edge("ancient-clavicle", "chaos-orb", "43:1", 12),
        ];
        for (index, ordering) in every_queue_position(&edges).into_iter().enumerate() {
            let valuation = clavicle_valuation(&ordering);
            assert!(
                is_rate(valuation.sell_value.as_ref(), "44:1"),
                "ordering {index} valued at {:?} instead of the better of the two deepest",
                valuation.sell_value
            );
        }
    }

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
