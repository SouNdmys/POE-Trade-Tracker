//! Contract tests for maker listing advice, including the B-11 direction fix.

use chrono::{DateTime, TimeZone, Utc};
use ptt_market_book::{EvaluatedQuoteEdge, FreshnessAssessment, FreshnessStatus};
use ptt_strategy::{MakerMode, MakerRequest, RiskThresholds, StockBasis, calculate_maker_strategy};
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
    basis: StockBasis,
) -> MakerRequest<'a> {
    MakerRequest {
        from_asset_id: &amount_in.asset_id,
        to_asset_id: &TO_ASSET,
        amount_in,
        competing,
        instant: None,
        stock_basis: basis,
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
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::FromAsset)).expect("ok");

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
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::FromAsset)).expect("ok");

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
fn modes_only_move_backwards_along_one_queue() {
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::FromAsset)).expect("ok");

    let rate_of = |mode: MakerMode| {
        strategy
            .recommendations
            .iter()
            .find(|item| item.mode == mode)
            .map(|item| item.rate.clone())
            .expect("recommendation")
    };
    let fast = rate_of(MakerMode::Fast);
    let balanced = rate_of(MakerMode::Balanced);
    let greedy = rate_of(MakerMode::Greedy);

    assert!(fast.compare_value(&balanced) != std::cmp::Ordering::Greater);
    assert!(balanced.compare_value(&greedy) != std::cmp::Ordering::Greater);
    assert_eq!(greedy.numerator, 4, "greedy asks the best rate in the book");
    assert_eq!(fast.numerator, 1, "fast sits at the head of the queue");
}

#[test]
fn a_wall_is_found_by_the_same_ordering_as_the_queue() {
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::FromAsset)).expect("ok");

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
fn stock_counted_in_the_destination_asset_is_divided_by_the_rate() {
    // Same book, opposite stock convention: 400 chaos at 4 chaos per divine
    // is 100 divine of depth, not 400.
    let book = audit_book();
    let amount_in = amount("divine-orb", 10);
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::ToAsset)).expect("ok");

    let depths: Vec<u64> = strategy
        .queue
        .iter()
        .map(|level| level.depth_from.as_ref().expect("depth").quanta)
        .collect();
    assert_eq!(depths, vec![10, 10, 10, 100]);
    assert_eq!(strategy.front_depth_from.expect("front").quanta, 30);
}

#[test]
fn an_empty_competing_book_asks_for_a_probe_instead_of_inventing_a_rate() {
    let amount_in = amount("divine-orb", 10);
    let strategy =
        calculate_maker_strategy(request(&[], &amount_in, StockBasis::FromAsset)).expect("ok");

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
    let strategy =
        calculate_maker_strategy(request(&book, &amount_in, StockBasis::FromAsset)).expect("ok");

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
