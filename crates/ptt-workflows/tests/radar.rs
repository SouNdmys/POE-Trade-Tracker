//! The radar's account of why it found nothing, against a hand-built book.
//!
//! One claim is under test and it is the one a live session got wrong twice:
//! a pair the book prices must never be reported as a pair the book is
//! missing. The two look identical from inside the search — both come back
//! with no path — and only the reason separates "go capture this" from "you
//! asked to trade below one whole unit".

use chrono::{Duration, TimeZone, Utc};
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
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, ProbeCandidate, ProbePriority,
    ProbeReason,
    RadarBudget, RadarItemKind, RadarRequest, RadarStart, run_opportunity_radar,
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

/// Like [`selection`], but with every capture stamped `age_seconds` ago —
/// for the tests that vary how old the book is *now*.
fn aged_selection(
    pairs: &[(&str, &str, &str, u64)],
    age_seconds: i64,
    status: FreshnessStatus,
) -> QuoteSelectionResult {
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
            let candidate = aged_taker_edge(from, to, rate, *stock, age_seconds, status);
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

/// The bridge book the de-stake tests share: chaos reaches divine only
/// through exalt, and the exalt book is three deep — far under any large
/// ask, so the walk always comes back partial.
fn bridge_pairs() -> [(&'static str, &'static str, &'static str, u64); 2] {
    [
        ("chaos", "exalt", "1:2", 3),
        ("exalt", "divine", "2:1", 500),
    ]
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
            asset_thin_thresholds: std::collections::BTreeMap::new(),
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

/// The user's ruling, verbatim: the radar has no business knowing what they
/// hold. A book that cannot swallow the scan's ask in one pass still names a
/// rate, so the route stays on the page — before 2026-08-24 this item was
/// gated out on its fill and refiled as a probe, which is how a stale-but-
/// real book produced "scanned 40, shown 0".
#[test]
fn a_route_the_book_cannot_swallow_whole_is_still_an_opportunity() {
    let units = whole_catalog(&["chaos", "divine", "exalt"]);
    let selection = aged_selection(&bridge_pairs(), 30 * 60, FreshnessStatus::Usable);

    let result = run_opportunity_radar(
        &selection,
        &units,
        &loop_scope(),
        &request("chaos", 1_000, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    let item = result
        .items
        .iter()
        .find(|item| item.kind == RadarItemKind::BestConversion)
        .expect("the partially filled route is still shown");
    assert_eq!(
        item.path_asset_ids,
        vec![asset("chaos"), asset("exalt"), asset("divine")]
    );
    assert!(
        !item
            .conversion_path
            .as_ref()
            .expect("conversion item carries its path")
            .is_fully_filled,
        "the premise: the exalt book is three deep against a thousand-chaos ask"
    );
    assert_eq!(
        result.diagnostics.complete_conversion_count, 1,
        "a found route counts as an answered pair, whatever its fill"
    );
    assert_eq!(result.diagnostics.missing_conversion_count, 0);
}

/// Two suggestions, two urgencies.
///
/// The page shows four probes. When the radar files more than four, the ones
/// that survive the cut should be the ones a single capture can turn into a
/// trade: a shown opportunity leaning on aged captures is one capture from a
/// verdict, a pair with no quotes at all is a guess. Neither starts at High —
/// that would leave the scarce-currency boost nothing to raise, which is what
/// made the whole field meaningless on this path. (Until 2026-08-24 the
/// confirmation half keyed on partial fills; with the radar size-blind, a
/// partial fill says nothing and the trigger is age, same as the loops.)
#[test]
fn radar_probes_rank_confirmation_above_missing_data() {
    let units = whole_catalog(&["chaos", "divine", "exalt", "mirror"]);
    // Half an hour old: past the fresh window, so the shown route's legs are
    // worth one capture each. Mirror has no quotes at all.
    let selection = aged_selection(&bridge_pairs(), 30 * 60, FreshnessStatus::Usable);
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
            FocusGroupItem {
                asset_id: asset("exalt"),
                role: FocusRole::Bridge,
            },
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
        &request("chaos", 1_000, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    let graded: std::collections::BTreeMap<String, (ProbeReason, ProbePriority)> = result
        .probe_candidates
        .iter()
        .map(|candidate| {
            (
                format!(
                    "{}->{}",
                    candidate.from_asset_id.as_str(),
                    candidate.to_asset_id.as_str()
                ),
                (candidate.reason, candidate.priority),
            )
        })
        .collect();
    assert_eq!(
        graded.get("chaos->exalt"),
        Some(&(ProbeReason::OpportunityConfirmation, ProbePriority::Medium)),
        "each aged leg of a shown route is one capture from a verdict"
    );
    assert_eq!(
        graded.get("exalt->divine"),
        Some(&(ProbeReason::OpportunityConfirmation, ProbePriority::Medium))
    );
    assert_eq!(
        graded.get("chaos->mirror"),
        Some(&(ProbeReason::MissingForwardQuote, ProbePriority::Low)),
        "an unpriced pair is exploratory, and must leave the boost room to raise it"
    );
}

/// **A conversion's margin over direct is not what closing it earns.**
///
/// On the owner's book a route reading +17.53% against its own direct trade
/// came home at +2.09%, and one reading +1.80% came home at **−0.66%** — a
/// loss wearing the shape of the fourth-best opportunity on the page. Both
/// numbers are real; only one of them is comparable with a cycle's, and only
/// one of them answers "what does this earn".
///
/// Here: chaos buys exalt at 1:2 and exalt buys divine at 2:1, so a chaos
/// buys one divine. The way home prices a divine at 3 chaos, so closing the
/// trip triples the stake — and that is the number the row must carry,
/// whatever the outbound leg looks like against a direct chaos→divine quote.
#[test]
fn a_closed_route_is_priced_on_the_round_trip_not_on_the_margin_over_direct() {
    let units = whole_catalog(&["chaos", "divine", "exalt"]);
    let mut pairs = bridge_pairs().to_vec();
    // The way home: one divine fetches three chaos.
    pairs.push(("divine", "chaos", "3:1", 10_000));
    let selection = aged_selection(&pairs, 60, FreshnessStatus::Fresh);

    let result = run_opportunity_radar(
        &selection,
        &units,
        &loop_scope(),
        &request("chaos", 1_000, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    let item = result
        .items
        .iter()
        .find(|item| {
            item.kind == RadarItemKind::BestConversion
                && item.path_asset_ids.last().map(MarketAssetId::as_str) == Some("divine")
        })
        .expect("the conversion to divine is shown");
    assert_eq!(
        item.round_trip_basis_points,
        Some(20_000),
        "out at 1 divine per chaos, home at 3 chaos per divine: {item:?}"
    );
}

/// A shown route whose every leg is fresh has nothing left to confirm — the
/// same silence the loops earned, for the same reason: a suggestion that
/// fires on every scan regardless of the data is noise.
#[test]
fn a_fresh_route_is_not_filed_for_confirmation() {
    let units = whole_catalog(&["chaos", "divine", "exalt"]);
    let selection = aged_selection(&bridge_pairs(), 60, FreshnessStatus::Fresh);

    let result = run_opportunity_radar(
        &selection,
        &units,
        &loop_scope(),
        &request("chaos", 1_000, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == RadarItemKind::BestConversion),
        "the route itself is shown either way"
    );
    let confirmations: Vec<&ProbeCandidate> = result
        .probe_candidates
        .iter()
        .filter(|candidate| candidate.reason == ProbeReason::OpportunityConfirmation)
        .collect();
    assert!(
        confirmations.is_empty(),
        "every leg was captured a minute ago; asked for {confirmations:?}"
    );
}

/// One taker level of the loop fixture, stamped `age_seconds` ago.
///
/// Separate from `taker_edge` because that one pins an absolute date, and
/// what these tests vary is how old the capture is *now* — the radar reads
/// the leg's age against the policy's fresh window, not against a calendar.
fn aged_taker_edge(
    from: &str,
    to: &str,
    rate: &str,
    stock: u64,
    age_seconds: i64,
    status: FreshnessStatus,
) -> EvaluatedQuoteEdge {
    let captured_at = Utc::now() - Duration::seconds(age_seconds);
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
            status,
            age_seconds: u64::try_from(age_seconds).expect("age"),
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

/// A book holding one profitable closed loop, every quote the same age.
///
/// `product_execution_allowed` is left at the shipped default (`false`), so
/// every triangle this book produces comes back `execution_eligible == false`
/// — the live condition, not a contrived one.
fn loop_selection(age_seconds: i64, status: FreshnessStatus) -> QuoteSelectionResult {
    let strategy = QuoteSelectionStrategy::Instant;
    let mut policy = QuoteSelectionPolicy::personal_default(strategy).expect("policy");
    policy.identity.policy_id = "test_loop_policy".to_owned();
    policy.identity.source = "test-only calibrated fixture".to_owned();
    policy.identity.calibration_status = PolicyCalibrationStatus::Verified;
    policy.cost_verification = CostVerification {
        fee_verified: true,
        minimum_lots_verified: true,
    };
    policy.validate().expect("test policy");
    // chaos -> divine -> exalt -> chaos returns 1.2x: 100 chaos buys 10
    // divine, which buys 80 exalt, which buys 120 chaos.
    let legs = [
        ("chaos", "divine", "1:10", 200_u64),
        ("divine", "exalt", "8:1", 4_000_u64),
        ("exalt", "chaos", "3:2", 9_000_u64),
    ];
    let selections = legs
        .iter()
        .map(|(from, to, rate, stock)| {
            let candidate = aged_taker_edge(from, to, rate, *stock, age_seconds, status);
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

/// chaos anchors, divine is the target, exalt is the bridge that closes the
/// loop.
fn loop_scope() -> FocusScope {
    FocusScope::try_new(
        &[
            FocusGroupItem {
                asset_id: asset("chaos"),
                role: FocusRole::Anchor,
            },
            FocusGroupItem {
                asset_id: asset("divine"),
                role: FocusRole::Target,
            },
            FocusGroupItem {
                asset_id: asset("exalt"),
                role: FocusRole::Bridge,
            },
        ],
        FocusScopePolicy::default(),
    )
    .expect("scope")
}

/// A loop whose every leg is fresh has nothing left to confirm.
///
/// `execution_eligible` is hard-wired off for products, so it is false for
/// every triangle ever found — asking to re-capture each leg of every
/// profitable loop is a constant, and a constant is noise. It filled the four
/// probe slots on the page and pushed the pairs the user has genuinely never
/// captured off the bottom.
#[test]
fn a_fully_fresh_profitable_loop_is_not_filed_for_confirmation() {
    let units = whole_catalog(&["chaos", "divine", "exalt"]);
    let selection = loop_selection(60, FreshnessStatus::Fresh);

    let result = run_opportunity_radar(
        &selection,
        &units,
        &loop_scope(),
        &request("chaos", 100, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == RadarItemKind::Loop),
        "the fixture has to actually produce the profitable loop, or the \
         assertion below passes for the wrong reason"
    );
    let confirmations: Vec<String> = result
        .probe_candidates
        .iter()
        .filter(|candidate| candidate.reason == ProbeReason::OpportunityConfirmation)
        .map(|candidate| {
            format!(
                "{}->{}",
                candidate.from_asset_id.as_str(),
                candidate.to_asset_id.as_str()
            )
        })
        .collect();
    assert!(
        confirmations.is_empty(),
        "every leg was captured a minute ago; there is nothing to re-confirm, \
         but the radar asked for {confirmations:?}"
    );
}

/// The same loop on half-hour-old quotes still asks.
///
/// The guard above must not turn the suggestion off wholesale: when a leg has
/// aged out of the fresh window the loop really is theory, and one capture is
/// what turns it back into a price.
#[test]
fn a_stale_legged_profitable_loop_is_still_filed_for_confirmation() {
    let units = whole_catalog(&["chaos", "divine", "exalt"]);
    // Thirty minutes: past the ten-minute fresh window, inside the hour-long
    // usable one, so the legs still price the walk.
    let selection = loop_selection(30 * 60, FreshnessStatus::Usable);

    let result = run_opportunity_radar(
        &selection,
        &units,
        &loop_scope(),
        &request("chaos", 100, &units),
        &SearchCancellation::default(),
        |_| {},
    )
    .expect("radar");

    let confirmations: Vec<String> = result
        .probe_candidates
        .iter()
        .filter(|candidate| candidate.reason == ProbeReason::OpportunityConfirmation)
        .map(|candidate| {
            format!(
                "{}->{}",
                candidate.from_asset_id.as_str(),
                candidate.to_asset_id.as_str()
            )
        })
        .collect();
    assert_eq!(
        confirmations,
        vec!["chaos->divine", "divine->exalt", "exalt->chaos"],
        "each aged leg of the loop is worth one capture"
    );
}
