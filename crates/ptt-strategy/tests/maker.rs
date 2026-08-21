//! Contract tests for maker listing advice, including the B-11 direction fix.

use chrono::{DateTime, TimeZone, Utc};
use ptt_market_book::{EvaluatedQuoteEdge, FreshnessAssessment, FreshnessStatus, QuoteRiskFlag};
use ptt_strategy::{
    ExecutionRisk, MakerMode, MakerQueueExclusion, MakerRequest, RiskThresholds,
    calculate_maker_strategy,
};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};
use ptt_trade_engine::{AssetAmount, AssetUnit};

fn asset(id: &str) -> MarketAssetId {
    MarketAssetId::try_new(id).expect("asset id")
}

fn amount(id: &str, quanta: u64) -> AssetAmount {
    AssetAmount {
        asset_id: asset(id),
        quanta,
        unit: AssetUnit::whole(),
    }
}

fn captured() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0)
        .single()
        .expect("timestamp")
}

/// A competing (maker-reference) listing on divine -> chaos.
fn listing(id: &str, rate_numerator: u64, stock: u64) -> EvaluatedQuoteEdge {
    let rate = Ratio::from_parts(rate_numerator, 1).expect("rate");
    EvaluatedQuoteEdge {
        observation: MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: id.to_owned(),
                snapshot_id: "snapshot-1".to_owned(),
                quote_id: format!("quote-{id}"),
                context_key: "test-context".to_owned(),
                from_asset_id: asset("divine-orb"),
                to_asset_id: asset("chaos-orb"),
                rate,
                source_side: QuoteSide::Competing,
                execution_type: ExecutionType::MakerReference,
                role: QuoteEdgeRole::CompetingMakerReference,
                stock,
                original_need_asset_id: asset("divine-orb"),
                original_have_asset_id: asset("chaos-orb"),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured(),
                confirmed_at: captured(),
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        },
        freshness: FreshnessAssessment {
            status: FreshnessStatus::Fresh,
            age_seconds: 30,
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

/// The audit's B-11 fixture: rates 1..4 holding 10, 20, 30 and 400.
fn audit_book() -> Vec<EvaluatedQuoteEdge> {
    vec![
        listing("wall", 4, 400),
        listing("third", 3, 30),
        listing("second", 2, 20),
        listing("front", 1, 10),
    ]
}

fn request<'a>(
    competing: &'a [EvaluatedQuoteEdge],
    amount_in: &'a AssetAmount,
) -> MakerRequest<'a> {
    MakerRequest {
        from_asset_id: &amount_in.asset_id,
        to_asset_id: &TO_ASSET,
        amount_in,
        competing,
        instant: None,
        match_front: false,
        thresholds: RiskThresholds::default(),
    }
}

// `MarketAssetId` has no const constructor, so the shared destination lives
// in a lazy static rather than being threaded through every call site.
static TO_ASSET: std::sync::LazyLock<MarketAssetId> =
    std::sync::LazyLock::new(|| MarketAssetId::try_new("chaos-orb").expect("asset id"));

#[test]
fn the_queue_front_is_the_lowest_rate_not_the_greediest() {
    // B-11: the JS read front depth off a descending array and reported 450
    // for this book. The front three listings hold 60.
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    let rates: Vec<u64> = strategy
        .queue
        .iter()
        .map(|level| level.rate.numerator)
        .collect();
    assert_eq!(rates, vec![1, 2, 3, 4], "front of queue fills first");

    assert_eq!(
        strategy.front_depth_from.as_ref().expect("front").quanta,
        60,
        "the three listings ahead of you hold 60, not the 450 the greediest three hold"
    );
    assert_eq!(
        strategy
            .visible_depth_from
            .as_ref()
            .expect("visible")
            .quanta,
        460
    );
    assert_eq!(strategy.front_level_count, 3);
}

#[test]
fn depth_ahead_accumulates_from_the_front() {
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    let ahead: Vec<u64> = strategy
        .queue
        .iter()
        .map(|level| level.depth_ahead_from.as_ref().expect("ahead").quanta)
        .collect();
    assert_eq!(
        ahead,
        vec![10, 30, 60, 460],
        "listing behind rate 3 waits for 60 orbs to clear"
    );
}

#[test]
fn opportunity_undercuts_the_front_rather_than_matching_it() {
    // Matching the front puts you behind it — that order was there first.
    // The book's front asks 1; the opportunity listing must ask less.
    let book = vec![
        listing("front", 784, 10),
        listing("second", 785, 20),
        listing("back", 795, 40),
    ];
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    let opportunity = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity)
        .expect("opportunity");
    assert_eq!(
        opportunity
            .rate
            .compare_value(&Ratio::from_parts(784, 1).expect("front")),
        std::cmp::Ordering::Less,
        "listing at 784 queues behind the existing 784; 783 takes the front"
    );
    assert_eq!(opportunity.rate.numerator, 783);
    assert!(!opportunity.is_speculative);
    assert!(!opportunity.queued_behind_front);

    // Liquid pairs may not be worth the tick; matching is offered, and says
    // plainly that it queues behind the order already there.
    let matched = calculate_maker_strategy(MakerRequest {
        match_front: true,
        ..request(&book, &amount_in)
    })
    .expect("ok");
    let matched = matched
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity)
        .expect("opportunity");
    assert_eq!(matched.rate.numerator, 784);
    assert!(matched.queued_behind_front);

    let greedy = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Greedy)
        .expect("greedy");
    assert_eq!(
        greedy.rate.numerator, 795,
        "greedy joins the top of the book"
    );
    assert!(
        greedy.is_speculative,
        "the greedy payoff depends on the market moving, not on today's book"
    );
}

#[test]
fn a_listing_no_better_than_the_instant_price_is_not_worth_making() {
    // Undercutting a front that already sits below what you could take now
    // produces a listing that is strictly worse than just trading.
    let book = vec![listing("front", 1, 10), listing("back", 2, 20)];
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");
    let opportunity = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity)
        .expect("opportunity");
    // Front is 1:1, so there is no tick below it to take.
    assert!(opportunity.rate.numerator <= 1);
}

#[test]
fn a_wall_is_found_by_the_same_ordering_as_the_queue() {
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    let wall = strategy.wall.as_ref().expect("wall");
    assert_eq!(wall.stock, 400);
    assert_eq!(
        wall.queue_index, 3,
        "the wall sits at the back of the queue"
    );

    let greedy = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Greedy)
        .expect("greedy");
    assert!(
        greedy.behind_wall,
        "listing at the wall's own rate queues behind it"
    );
}

#[test]
fn maker_stock_is_read_as_what_the_lister_pays_out() {
    // Settled by the Divine/Mirror panel: competing rows showed 2, 2, 1, 8
    // — mirror counts, the asset those listers are giving up — while the
    // available rows carried exact multiples of the ratio numerator. A
    // maker-reference edge's stock is therefore in its `from` asset and needs
    // no conversion.
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    let depths: Vec<u64> = strategy
        .queue
        .iter()
        .map(|level| level.depth_from.as_ref().expect("depth").quanta)
        .collect();
    assert_eq!(depths, vec![10, 20, 30, 400]);
}

#[test]
fn an_empty_competing_book_asks_for_a_probe_instead_of_inventing_a_rate() {
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&[], &amount_in)).expect("ok");

    assert!(strategy.queue.is_empty());
    assert!(
        strategy.recommendations.is_empty(),
        "no listings and no instant price means no advice"
    );
    assert!(
        strategy
            .assessment
            .contains(ptt_strategy::ExecutionRisk::NeedsProbe)
    );
}

#[test]
fn an_order_larger_than_the_visible_book_is_flagged_for_splitting() {
    let book = audit_book();
    let amount_in = amount("divine-orb", 1_000);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    assert!(strategy.split_order_recommended);
    assert!(
        strategy
            .assessment
            .contains(ptt_strategy::ExecutionRisk::MakerDepthExceeded)
    );
    // Capped by the front of the queue (60), which is tighter than a third
    // of everything visible (153).
    assert_eq!(
        strategy
            .suggested_max_single_order
            .as_ref()
            .expect("suggested")
            .quanta,
        60
    );
}

/// The listing as the book would hand it over after the median band fired:
/// outlier-flagged and rejected from selection.
fn flagged_outlier(mut listing: EvaluatedQuoteEdge) -> EvaluatedQuoteEdge {
    listing.risk_flags = vec![
        QuoteRiskFlag::PriceOutlier,
        QuoteRiskFlag::OutsideTopBookBand,
    ];
    listing.accepted_for_selection = false;
    listing
}

#[test]
fn an_outlier_at_the_queue_front_is_excluded_from_the_math() {
    // A malicious listing prices itself to the front of the queue — the
    // exact row Opportunity would undercut — and its stock inflates every
    // depth figure. It must be excluded before any arithmetic, and stay
    // visible with its reason.
    let book = vec![
        flagged_outlier(listing("bait", 100, 500)),
        listing("front", 784, 10),
        listing("second", 785, 20),
        listing("back", 795, 40),
    ];
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");

    assert_eq!(
        strategy.queue.first().expect("front").edge_id,
        "front",
        "the bait must not be the queue front"
    );
    assert_eq!(strategy.excluded.len(), 1);
    assert_eq!(strategy.excluded[0].edge_id, "bait");
    assert_eq!(
        strategy.excluded[0].reason,
        MakerQueueExclusion::PriceOutlier
    );
    assert_eq!(
        strategy
            .visible_depth_from
            .as_ref()
            .expect("visible")
            .quanta,
        70,
        "the bait's 500 stock must not inflate visible depth"
    );

    let opportunity = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity)
        .expect("opportunity");
    assert_eq!(
        opportunity.rate.numerator, 783,
        "opportunity undercuts the honest front, not the bait"
    );
    assert!(
        !opportunity
            .assessment
            .risks
            .contains(&ExecutionRisk::PriceOutlier),
        "the recommendation's math never saw the bait, so it is not tainted"
    );
    assert!(
        strategy
            .assessment
            .risks
            .contains(&ExecutionRisk::PriceOutlier),
        "the pair-level picture still names the suspicious row"
    );
}

#[test]
fn a_single_listing_book_is_flagged_as_uncorroborated() {
    // One listing prices the whole queue: nothing corroborates the rate,
    // and the median band cannot judge a side of one.
    let book = vec![listing("only", 784, 50)];
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(request(&book, &amount_in)).expect("ok");
    assert!(
        strategy
            .assessment
            .risks
            .contains(&ExecutionRisk::SingleListingBook)
    );
    for recommendation in &strategy.recommendations {
        assert!(
            recommendation
                .assessment
                .risks
                .contains(&ExecutionRisk::SingleListingBook),
            "{:?} prices against the uncorroborated row and must say so",
            recommendation.mode
        );
    }
}

#[test]
fn the_spread_reads_the_front_of_the_queue_not_the_back() {
    // Instant taker rate 700:1, competing front 784, back 795. The spread
    // the trading model defines is front over instant: (784-700)/700 =
    // exactly 1200 basis points. The old reading took the back row and
    // would report (795-700)/700 = 1357.
    use ptt_trade_engine::{FillKind, PairFill};
    let instant = PairFill {
        from_asset_id: asset("divine-orb"),
        to_asset_id: asset("chaos-orb"),
        kind: FillKind::TakerDepth,
        requested_input: amount("divine-orb", 10),
        consumed_input: amount("divine-orb", 10),
        unfilled_input: amount("divine-orb", 0),
        gross_amount_out: amount("chaos-orb", 7_000),
        net_amount_out: None,
        fills: Vec::new(),
        capture_time_evidence: None,
        is_fully_filled: true,
        execution_eligible: true,
        bottleneck: None,
        risk_flags: Vec::new(),
    };
    let book = vec![
        listing("front", 784, 10),
        listing("second", 785, 20),
        listing("back", 795, 40),
    ];
    let amount_in = amount("divine-orb", 10);
    let strategy = calculate_maker_strategy(MakerRequest {
        instant: Some(&instant),
        ..request(&book, &amount_in)
    })
    .expect("ok");
    assert_eq!(
        strategy
            .best_competing_rate
            .as_ref()
            .expect("front")
            .numerator,
        784,
        "best competing is the front of the queue"
    );
    assert_eq!(
        strategy.spread_basis_points.expect("spread"),
        1_200,
        "spread is front over instant, exactly"
    );
}
