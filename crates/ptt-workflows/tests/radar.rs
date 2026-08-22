//! The radar's account of why it found nothing, against a hand-built book.
//!
//! One claim is under test and it is the one a live session got wrong twice:
//! a pair the book prices must never be reported as a pair the book is
//! missing. The two look identical from inside the search — both come back
//! with no path — and only the reason separates "go capture this" from "you
//! asked to trade below one whole unit".

use chrono::{TimeZone, Utc};
use ptt_market_book::{
    CostVerification, EvaluatedQuoteEdge, FreshnessAssessment, FreshnessStatus,
    PolicyCalibrationStatus, QuoteSelectionPolicy, QuoteSelectionResult, QuoteSelectionStrategy,
    SelectedQuoteEdge,
};
use ptt_strategy::RiskThresholds;
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};
use ptt_trade_engine::{AssetAmount, AssetUnit, AssetUnitCatalog, FeePolicy, SearchCancellation};
use ptt_workflows::{
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, RadarBudget, RadarRequest, RadarStart,
    run_opportunity_radar,
};

fn asset(value: &str) -> MarketAssetId {
    MarketAssetId::try_new(value).expect("asset")
}

fn whole_catalog(asset_ids: &[&str]) -> AssetUnitCatalog {
    let units: std::collections::BTreeMap<MarketAssetId, AssetUnit> = asset_ids
        .iter()
        .map(|id| (asset(id), AssetUnit::whole()))
        .collect();
    AssetUnitCatalog::try_new(units).expect("catalog")
}

/// One taker level, priced `rate` with `stock` of the payout asset.
fn taker_edge(from: &str, to: &str, rate: &str, stock: u64) -> EvaluatedQuoteEdge {
    let captured_at = Utc
        .with_ymd_and_hms(2026, 8, 22, 10, 0, 0)
        .single()
        .expect("time");
    EvaluatedQuoteEdge {
        observation: MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: format!("edge-{from}-{to}"),
                snapshot_id: format!("snapshot-{from}-{to}"),
                quote_id: format!("quote-{from}-{to}"),
                context_key: "context".to_owned(),
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
                user_edited: true,
                machine_confidence_ppm: None,
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
        effective_confidence_ppm: 1_000_000,
        risk_flags: Vec::new(),
        selection_rejections: Vec::new(),
        execution_blockers: Vec::new(),
        accepted_for_selection: true,
        eligible_for_depth_analysis: true,
    }
}

fn selection(pairs: &[(&str, &str, &str, u64)]) -> QuoteSelectionResult {
    let strategy = QuoteSelectionStrategy::Instant;
    let mut policy = QuoteSelectionPolicy::personal_default(strategy).expect("policy");
    policy.identity.policy_id = "test_verified_policy".to_owned();
    policy.identity.source = "test-only calibrated fixture".to_owned();
    policy.identity.calibration_status = PolicyCalibrationStatus::Verified;
    policy.cost_verification = CostVerification {
        fee_verified: true,
        minimum_lots_verified: true,
    };
    policy.product_execution_allowed = true;
    policy.validate().expect("test policy");
    let selections = pairs
        .iter()
        .map(|(from, to, rate, stock)| {
            let candidate = taker_edge(from, to, rate, *stock);
            SelectedQuoteEdge {
                pair_key: format!("{from}->{to}"),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                strategy,
                selected_edge: Some(candidate.clone()),
                candidate_edges: vec![candidate],
                rejections: Vec::new(),
                execution_eligible: true,
                needs_probe: false,
            }
        })
        .collect();
    QuoteSelectionResult {
        context_key: "context".to_owned(),
        policy,
        selections,
    }
}

fn request(start: &str, stake: u64, units: &AssetUnitCatalog) -> RadarRequest {
    RadarRequest {
        context_key: "context".to_owned(),
        starts: vec![RadarStart {
            start_asset_id: asset(start),
            amount_in: AssetAmount::from_whole_units(asset(start), stake, units).expect("stake"),
        }],
        minimum_conversion_improvement_basis_points: 100,
        minimum_triangle_profit_basis_points: 100,
        max_hops: 3,
        max_cycle_length: 6,
        max_paths_per_target: 32,
        max_expansions_per_target: 4_000,
        budget: RadarBudget {
            max_total_expansions: 60_000,
            max_targets: 48,
        },
        max_triangle_evaluations: 4_000,
        max_results: 12,
        fee_policy: FeePolicy::None,
        thresholds: RiskThresholds {
            thin_liquidity_stock: 100,
        },
    }
}

/// A stake below one whole unit of the target is not a missing quote.
///
/// The live case, in miniature: a settlement currency worth a fraction of its
/// target, staked at ten. `floor(10 / 11)` is zero, so the search reports no
/// path — and the radar used to file that under "no forward quote" and send
/// the user to capture a pair whose quotes were on file and fresh. The
/// contrast target has no quotes at all, and must still be asked for: the fix
/// has to keep the distinction, not remove the suggestion.
#[test]
fn a_priced_pair_below_the_minimum_lot_is_not_reported_as_a_missing_quote() {
    let units = whole_catalog(&["chaos", "divine", "mirror"]);
    // Priced: 11 chaos buys one divine, with room for many.
    let selection = selection(&[("chaos", "divine", "1:11", 500)]);
    let scope = FocusScope::try_new(
        &[
            FocusGroupItem {
                asset_id: asset("chaos"),
                role: FocusRole::Anchor,
            },
            FocusGroupItem {
                asset_id: asset("divine"),
                role: FocusRole::Target,
            },
            // Never quoted, so genuinely missing.
            FocusGroupItem {
                asset_id: asset("mirror"),
                role: FocusRole::Target,
            },
        ],
        FocusScopePolicy::default(),
    )
    .expect("scope");

    let result = run_opportunity_radar(
        &selection,
        &units,
        &scope,
        &request("chaos", 10, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    let asked_for: Vec<&str> = result
        .probe_candidates
        .iter()
        .map(|candidate| candidate.to_asset_id.as_str())
        .collect();
    assert_eq!(
        asked_for,
        vec!["mirror"],
        "only the pair with no quotes should be asked for; divine is priced and fresh"
    );
    assert_eq!(
        result.diagnostics.missing_conversion_count, 1,
        "the unpriced target, and only it"
    );
    assert_eq!(
        result.diagnostics.complete_conversion_count, 1,
        "the priced target was scanned to a complete fill once the stake was \
         raised to what one unit costs"
    );
}

/// The same book, staked large enough to need no help, reads the same.
///
/// Guards the retry against changing an answer it was not meant to touch: a
/// stake that already clears the minimum lot must produce the same verdicts
/// as before the retry existed.
#[test]
fn a_stake_that_already_clears_the_minimum_lot_is_unaffected() {
    let units = whole_catalog(&["chaos", "divine"]);
    let selection = selection(&[("chaos", "divine", "1:11", 500)]);
    let scope = FocusScope::try_new(
        &[
            FocusGroupItem {
                asset_id: asset("chaos"),
                role: FocusRole::Anchor,
            },
            FocusGroupItem {
                asset_id: asset("divine"),
                role: FocusRole::Target,
            },
        ],
        FocusScopePolicy::default(),
    )
    .expect("scope");

    let result = run_opportunity_radar(
        &selection,
        &units,
        &scope,
        &request("chaos", 110, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    assert!(result.probe_candidates.is_empty());
    assert_eq!(result.diagnostics.complete_conversion_count, 1);
    assert_eq!(result.diagnostics.missing_conversion_count, 0);
}
