//! The big radar: the official exchange's hourly traded volumes, run through
//! the **same** loop search as the capture radar.
//!
//! The user's ruling (P11): one algorithm, two data preparations. The capture
//! radar reads one to twelve real order-book levels per pair; this one reads
//! one synthetic level per pair — the hour's volume-weighted average rate —
//! and never looks at listed stock. Its answer is a hint about *which group
//! of currencies is worth capturing*, after which the capture radar rules on
//! the real book. That is the three-layer loop: API sync → this → capture.
//!
//! The impersonation of an order book happens in exactly one place,
//! [`synthetic_book`]. Nothing here leaks into the capture radar's item
//! list, and the policy it runs under is stamped unverified on every axis the
//! engine consults, so no loop it finds can ever read as instantly executable.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use ptt_market_book::{
    EvaluatedQuoteEdge, FreshnessPolicy, PolicyCalibrationStatus, QuoteSelectionPolicy,
    QuoteSelectionResult, QuoteSelectionStrategy, SelectedQuoteEdge,
};
use ptt_strategy::{ExecutionRisk, RiskThresholds};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ExecutionRiskFlag, FeePolicy, SearchCancellation,
};

use crate::WorkflowError;
use crate::focus::FocusScope;
use crate::radar::{
    RadarBudget, RadarItemKind, RadarRequest, RadarResult, RadarStart, run_opportunity_radar,
};

/// Identity of the synthetic policy — surfaces in the result so a report can
/// never confuse a VWAP loop with a captured one.
pub const EXCHANGE_RADAR_POLICY_ID: &str = "exchange-vwap-hourly";

/// Seconds in one exchange-history bucket.
const HOUR_SECONDS: i64 = 3_600;

/// The stake the search walks with. Same size and same reason as the capture
/// radar's: big enough that no bridge rounds away, far below the overflow
/// guards. A loop takes its own size from its thinnest leg regardless.
const WALK_SIZE: u64 = 1_000_000;

/// One market's traded volumes in one hour, already mapped to domain ids.
///
/// `volume_a` of `asset_a` changed hands against `volume_b` of `asset_b`, so
/// the hour's average rate a→b is `volume_b / volume_a` and b→a its inverse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeMarketHour {
    /// Start of the hour, unix seconds.
    pub hour_ts: i64,
    pub asset_a: MarketAssetId,
    pub asset_b: MarketAssetId,
    pub volume_a: u64,
    pub volume_b: u64,
}

#[derive(Clone, Debug)]
pub struct ExchangeRadarRequest {
    pub context_key: String,
    /// Settlement currencies every loop starts from. Each must appear in the
    /// data; the caller filters, this rejects.
    pub starts: Vec<MarketAssetId>,
    pub minimum_profit_basis_points: u32,
    /// Three or four. The graph is dense — every pair the exchange traded is
    /// an edge — and five-asset loops multiply the evaluations past any
    /// budget without saying anything a four-asset loop did not.
    pub max_cycle_length: u8,
    pub max_triangle_evaluations: u32,
    pub max_results: u16,
    pub thresholds: RiskThresholds,
    /// Hourly buckets age in hours, not minutes: this policy, not the capture
    /// one, decides what "fresh" means for a bucket.
    pub freshness: FreshnessPolicy,
    pub now: DateTime<Utc>,
}

/// One row per unordered pair — its newest hour — dropping hours older than
/// `max_age_hours` and any side that traded nothing.
///
/// Newest rather than busiest: the radar is asked "what does the market look
/// like now", and a busy bucket from yesterday is an answer to a different
/// question. A pair with a silent side has no rate, only a volume.
#[must_use]
pub fn latest_market_hours(
    hours: &[ExchangeMarketHour],
    now: DateTime<Utc>,
    max_age_hours: i64,
) -> Vec<ExchangeMarketHour> {
    let oldest_allowed = now.timestamp() - max_age_hours.max(0) * HOUR_SECONDS;
    let recent: Vec<ExchangeMarketHour> = hours
        .iter()
        .filter(|hour| hour.hour_ts >= oldest_allowed)
        .cloned()
        .collect();
    newest_per_pair(&recent)
}

/// The newest usable hour of every unordered pair. Silent sides and
/// self-pairs are dropped here too, so the synthetic book can never see two
/// rows for one pair — the depth index keeps whichever came last in input
/// order, which is not "newest", and two rows of one hour collide on ids.
fn newest_per_pair(hours: &[ExchangeMarketHour]) -> Vec<ExchangeMarketHour> {
    let mut newest: BTreeMap<(MarketAssetId, MarketAssetId), ExchangeMarketHour> = BTreeMap::new();
    for hour in hours {
        if hour.volume_a == 0 || hour.volume_b == 0 || hour.asset_a == hour.asset_b {
            continue;
        }
        let key = if hour.asset_a <= hour.asset_b {
            (hour.asset_a.clone(), hour.asset_b.clone())
        } else {
            (hour.asset_b.clone(), hour.asset_a.clone())
        };
        let replace = newest
            .get(&key)
            .is_none_or(|existing| hour.hour_ts > existing.hour_ts);
        if replace {
            newest.insert(key, hour.clone());
        }
    }
    newest.into_values().collect()
}

/// The synthetic book the radar walks: two taker levels per market hour.
pub(crate) struct SyntheticBook {
    pub(crate) selection: QuoteSelectionResult,
    pub(crate) units: AssetUnitCatalog,
    pub(crate) assets: Vec<MarketAssetId>,
}

/// Run the capture radar's search over the exchange's hourly averages.
///
/// `probe_candidates` comes back empty on purpose: "go capture this" is an
/// explicit action in the detail panel, chosen by the reader, not something
/// a synthetic book gets to suggest on its own.
pub fn run_exchange_radar(
    hours: &[ExchangeMarketHour],
    request: &ExchangeRadarRequest,
) -> Result<RadarResult, WorkflowError> {
    let book = synthetic_book(hours, request)?;
    let scope = FocusScope::whole_market(&request.starts, &book.assets)?;
    let mut starts = Vec::new();
    for asset in &request.starts {
        let amount_in = AssetAmount::from_whole_units(asset.clone(), WALK_SIZE, &book.units)
            .map_err(|_| WorkflowError::InvalidRequest)?;
        starts.push(RadarStart {
            start_asset_id: asset.clone(),
            amount_in,
        });
    }
    let radar_request = RadarRequest {
        context_key: request.context_key.clone(),
        starts,
        minimum_conversion_improvement_basis_points: request.minimum_profit_basis_points,
        minimum_triangle_profit_basis_points: request.minimum_profit_basis_points,
        max_hops: 3,
        max_cycle_length: request.max_cycle_length,
        // The whole-market scope has no directed pairs, so the conversion
        // scan has nothing to do and these knobs never bite; the budget that
        // matters is the triangle one.
        max_paths_per_target: 8,
        max_expansions_per_target: 2_000,
        budget: RadarBudget {
            max_total_expansions: 20_000,
            max_targets: 0,
        },
        max_triangle_evaluations: request.max_triangle_evaluations,
        max_results: request.max_results,
        // Gross by product decision, same as the capture radar.
        fee_policy: FeePolicy::None,
        thresholds: request.thresholds.clone(),
    };
    let mut result = run_opportunity_radar(
        &book.selection,
        &book.units,
        &scope,
        &radar_request,
        &SearchCancellation::default(),
        |_| {},
    )?;
    result.probe_candidates.clear();
    // Loops only. A conversion is "better than direct *at this stake*", and
    // a synthetic book has no honest stake: walked at the search size the
    // anchor↔anchor conversions came back as +500% through two thin legs —
    // partial fills wearing the shape of an opportunity. A loop sizes itself
    // from its thinnest leg and needs no stake to mean something.
    result.items.retain(|item| item.kind == RadarItemKind::Loop);
    // One level per pair is how this book is built, not a fact about the
    // market — the hour's average is backed by every trade in it, the very
    // opposite of an uncorroborated single listing.
    for item in &mut result.items {
        item.risk_flags
            .retain(|flag| *flag != ExecutionRiskFlag::SingleListingBook);
        item.blocking_risks
            .retain(|risk| *risk != ExecutionRisk::SingleListingBook);
    }
    Ok(result)
}

/// Where the impersonation lives. Each market hour becomes two `Taker`
/// levels, one per direction, priced at the hour's average and stocked with
/// the hour's traded volume of the payout asset — never "unlimited", because
/// the thinnest-leg sizing multiplies stock through the loop and an
/// unbounded stock overflows straight into "zero output".
pub(crate) fn synthetic_book(
    hours: &[ExchangeMarketHour],
    request: &ExchangeRadarRequest,
) -> Result<SyntheticBook, WorkflowError> {
    if request.starts.is_empty() || request.context_key.trim().is_empty() {
        return Err(WorkflowError::InvalidRequest);
    }
    // The dedup is enforced here, not trusted to the caller: a book with two
    // rows for one pair is silently wrong (see `newest_per_pair`).
    let hours = newest_per_pair(hours);
    let mut assets: BTreeSet<MarketAssetId> = BTreeSet::new();
    for hour in &hours {
        assets.insert(hour.asset_a.clone());
        assets.insert(hour.asset_b.clone());
    }
    if request.starts.iter().any(|start| !assets.contains(start)) {
        return Err(WorkflowError::InvalidRequest);
    }
    let units = AssetUnitCatalog::try_new(
        assets
            .iter()
            .map(|asset| (asset.clone(), AssetUnit::whole()))
            .collect::<BTreeMap<_, _>>(),
    )
    .map_err(|_| WorkflowError::InvalidMarketSelection)?;

    let strategy = QuoteSelectionStrategy::Instant;
    let mut policy = QuoteSelectionPolicy::personal_default(strategy)
        .map_err(|_| WorkflowError::InvalidRequest)?;
    policy.identity.policy_id = EXCHANGE_RADAR_POLICY_ID.to_owned();
    policy.identity.source = "official exchange hourly VWAP".to_owned();
    policy.identity.calibration_status = PolicyCalibrationStatus::Unverified;
    // Three legs of one loop were traded by different people at different
    // minutes of the hour. No capture-skew window can be claimed for that,
    // so the gate stays unverified and every multi-leg route stays off the
    // instant rung.
    policy.capture_skew.max_capture_skew_seconds = None;
    policy.capture_skew.calibration_status = PolicyCalibrationStatus::Unverified;
    policy.product_execution_allowed = false;
    policy.freshness = request.freshness;
    policy.minimum_stock = 0;
    policy
        .validate()
        .map_err(|_| WorkflowError::InvalidRequest)?;

    let mut selections = Vec::new();
    for hour in hours {
        for (from, to, volume_from, volume_to) in [
            (&hour.asset_a, &hour.asset_b, hour.volume_a, hour.volume_b),
            (&hour.asset_b, &hour.asset_a, hour.volume_b, hour.volume_a),
        ] {
            let edge = synthetic_edge(hour.hour_ts, from, to, volume_from, volume_to, request)?;
            selections.push(SelectedQuoteEdge {
                pair_key: format!("{}->{}", from.as_str(), to.as_str()),
                from_asset_id: from.clone(),
                to_asset_id: to.clone(),
                strategy,
                selected_edge: Some(edge.clone()),
                candidate_edges: vec![edge],
                rejections: Vec::new(),
                execution_eligible: false,
                needs_probe: false,
            });
        }
    }
    Ok(SyntheticBook {
        selection: QuoteSelectionResult {
            context_key: request.context_key.clone(),
            policy,
            selections,
        },
        units,
        assets: assets.into_iter().collect(),
    })
}

fn synthetic_edge(
    hour_ts: i64,
    from: &MarketAssetId,
    to: &MarketAssetId,
    volume_from: u64,
    volume_to: u64,
    request: &ExchangeRadarRequest,
) -> Result<EvaluatedQuoteEdge, WorkflowError> {
    // "to per from": giving `volume_from` bought `volume_to` over the hour.
    let rate = Ratio::parse(&format!("{volume_to}:{volume_from}"))
        .map_err(|_| WorkflowError::NumericOverflow)?;
    // Stamped at the hour's end: the bucket is not known before it closes.
    let captured_at = Utc
        .timestamp_opt(hour_ts.saturating_add(HOUR_SECONDS), 0)
        .single()
        .ok_or(WorkflowError::InvalidRequest)?;
    let (low, high) = if from <= to { (from, to) } else { (to, from) };
    let edge_id = format!("exchange:{hour_ts}:{}->{}", from.as_str(), to.as_str());
    Ok(EvaluatedQuoteEdge {
        observation: MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.clone(),
                snapshot_id: format!("exchange:{hour_ts}:{}|{}", low.as_str(), high.as_str()),
                quote_id: edge_id,
                context_key: request.context_key.clone(),
                from_asset_id: from.clone(),
                to_asset_id: to.clone(),
                rate,
                source_side: QuoteSide::Available,
                execution_type: ExecutionType::Taker,
                role: QuoteEdgeRole::AvailableTaker,
                stock: volume_to,
                original_need_asset_id: to.clone(),
                original_have_asset_id: from.clone(),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(1_000_000),
                captured_at,
                confirmed_at: captured_at,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        },
        freshness: request.freshness.classify(captured_at, request.now),
        effective_confidence_ppm: 1_000_000,
        risk_flags: Vec::new(),
        selection_rejections: Vec::new(),
        execution_blockers: Vec::new(),
        accepted_for_selection: true,
        eligible_for_depth_analysis: true,
    })
}

#[cfg(test)]
mod exchange_radar_tests {
    use super::*;
    use ptt_strategy::Actionability;

    fn asset(value: &str) -> MarketAssetId {
        MarketAssetId::try_new(value).expect("asset")
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 2, hour, 0, 0)
            .single()
            .expect("time")
    }

    fn hour(
        hour_ts: DateTime<Utc>,
        a: &str,
        b: &str,
        volume_a: u64,
        volume_b: u64,
    ) -> ExchangeMarketHour {
        ExchangeMarketHour {
            hour_ts: hour_ts.timestamp(),
            asset_a: asset(a),
            asset_b: asset(b),
            volume_a,
            volume_b,
        }
    }

    fn request(starts: &[&str]) -> ExchangeRadarRequest {
        ExchangeRadarRequest {
            context_key: "exchange:test".to_owned(),
            starts: starts.iter().map(|start| asset(start)).collect(),
            minimum_profit_basis_points: 100,
            max_cycle_length: 4,
            max_triangle_evaluations: 10_000,
            max_results: 20,
            thresholds: RiskThresholds::default(),
            freshness: FreshnessPolicy::try_new(3 * 3_600, 6 * 3_600, 24 * 3_600)
                .expect("freshness"),
            now: at(12),
        }
    }

    #[test]
    fn an_inconsistent_triangle_is_one_loop_priced_by_its_averages() {
        // a→b = 2, b→c = 3, c→a = 1/5: around the loop 2 × 3 / 5 = 1.2.
        let hours = [
            hour(at(10), "a", "b", 100, 200),
            hour(at(10), "b", "c", 100, 300),
            hour(at(10), "a", "c", 100, 500),
        ];
        let result = run_exchange_radar(&hours, &request(&["a"])).expect("radar");

        assert_eq!(result.items.len(), 1, "{:?}", result.items);
        let item = &result.items[0];
        assert_eq!(item.kind, RadarItemKind::Loop);
        assert_eq!(item.round_trip_basis_points, Some(2_000));
        assert_eq!(
            item.path_asset_ids,
            vec![asset("a"), asset("b"), asset("c"), asset("a")]
        );
        assert!(item.liquidity_capacity.is_some());
        assert_ne!(item.category, Actionability::InstantExecutable);
        assert_eq!(
            result.analysis_policy.identity.policy_id,
            EXCHANGE_RADAR_POLICY_ID
        );
        assert!(result.probe_candidates.is_empty());
        assert!(!result.diagnostics.budget_exhausted);
    }

    #[test]
    fn a_consistent_triangle_has_no_loop() {
        // 2 × 3 / 6 = 1 exactly: the averages agree with each other.
        let hours = [
            hour(at(10), "a", "b", 100, 200),
            hour(at(10), "b", "c", 100, 300),
            hour(at(10), "a", "c", 100, 600),
        ];
        let result = run_exchange_radar(&hours, &request(&["a"])).expect("radar");

        assert!(result.items.is_empty(), "{:?}", result.items);
    }

    #[test]
    fn only_the_newest_hour_of_a_pair_survives_and_silent_sides_do_not() {
        let hours = [
            hour(at(9), "a", "b", 100, 900),
            hour(at(10), "b", "a", 200, 100),
            hour(at(10), "a", "c", 100, 0),
            hour(at(1), "b", "c", 100, 300),
        ];
        let kept = latest_market_hours(&hours, at(12), 6);

        assert_eq!(kept, vec![hour(at(10), "b", "a", 200, 100)]);
    }

    #[test]
    fn every_synthetic_edge_has_its_own_id_and_the_payout_volume_as_stock() {
        let hours = [
            hour(at(10), "a", "b", 100, 200),
            hour(at(10), "b", "c", 100, 300),
        ];
        let book = synthetic_book(&hours, &request(&["a"])).expect("book");

        let ids: BTreeSet<String> = book
            .selection
            .selections
            .iter()
            .map(|selected| {
                selected
                    .selected_edge
                    .as_ref()
                    .expect("edge")
                    .observation
                    .edge
                    .edge_id
                    .clone()
            })
            .collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(book.assets, vec![asset("a"), asset("b"), asset("c")]);
        let a_to_b = book
            .selection
            .selections
            .iter()
            .find(|selected| selected.pair_key == "a->b")
            .and_then(|selected| selected.selected_edge.as_ref())
            .expect("a->b");
        assert_eq!(a_to_b.observation.edge.rate.numerator, 2);
        assert_eq!(a_to_b.observation.edge.rate.denominator, 1);
        assert_eq!(a_to_b.observation.edge.stock, 200);
        assert_eq!(a_to_b.observation.edge.captured_at, at(11));
        assert!(!book.selection.policy.product_execution_allowed);
    }

    #[test]
    fn a_start_the_data_never_traded_is_rejected() {
        let hours = [hour(at(10), "a", "b", 100, 200)];
        let error = run_exchange_radar(&hours, &request(&["divine"])).expect_err("no start");
        assert_eq!(error, WorkflowError::InvalidRequest);
    }

    #[test]
    fn the_same_pair_in_two_hours_reaches_the_book_once_at_its_newest_hour() {
        // 调用方忘了先去重也不能炸:同一对两个小时,只有最新那小时进书。
        let hours = [
            hour(at(9), "a", "b", 100, 900),
            hour(at(10), "a", "b", 100, 200),
            hour(at(10), "b", "c", 100, 300),
        ];
        let book = synthetic_book(&hours, &request(&["a"])).expect("book");

        assert_eq!(book.selection.selections.len(), 4);
        let a_to_b = book
            .selection
            .selections
            .iter()
            .find(|selected| selected.pair_key == "a->b")
            .and_then(|selected| selected.selected_edge.as_ref())
            .expect("a->b");
        assert_eq!(a_to_b.observation.edge.stock, 200);
        assert_eq!(a_to_b.observation.edge.captured_at, at(11));
    }
}
