use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{DateTime, Utc};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, SnapshotRecordStatus,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How far above every other row on its own side a listing's visible stock
/// may sit before the band names it.
///
/// Deliberately an order of magnitude, and deliberately not a user knob.
/// `top_book_outlier_factor` already lets the reader tune the *rate* band,
/// and a second dial pulling on the same rows would leave two bands to
/// reconcile every time one of them fired. Ten catches the failure this
/// exists for — an OCR reading that gained a digit — and clears a genuinely
/// deep listing, which is routinely a few times its neighbours and almost
/// never ten. Calibrate it against a full season's captures before promoting
/// it to a setting.
pub const STOCK_OUTLIER_FACTOR: u64 = 10;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPairKey {
    pub first_asset_id: MarketAssetId,
    pub second_asset_id: MarketAssetId,
}

impl MarketPairKey {
    pub fn try_new(left: MarketAssetId, right: MarketAssetId) -> Result<Self, MarketBookError> {
        if left == right {
            return Err(MarketBookError::SameAssetPair);
        }
        let (first_asset_id, second_asset_id) = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        Ok(Self {
            first_asset_id,
            second_asset_id,
        })
    }

    #[must_use]
    pub fn stable_key(&self) -> String {
        format!("{}<->{}", self.first_asset_id, self.second_asset_id)
    }
}

impl fmt::Display for MarketPairKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.stable_key().fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataVisibility {
    pub include_isolated: bool,
    pub include_deleted: bool,
}

impl Default for DataVisibility {
    fn default() -> Self {
        Self {
            include_isolated: true,
            include_deleted: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BookExclusionReason {
    ContextMismatch,
    IncompleteSnapshot,
    IsolatedHidden,
    DeletedHidden,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookExclusion {
    pub edge_id: String,
    pub snapshot_id: String,
    pub reason: BookExclusionReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoherentBookView {
    pub context_key: String,
    pub pair_key: MarketPairKey,
    pub snapshot_id: String,
    pub captured_at: DateTime<Utc>,
    pub observations: Vec<MarketEdgeObservation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoherentCurrentBook {
    pub context_key: String,
    pub views: Vec<CoherentBookView>,
    pub exclusions: Vec<BookExclusion>,
}

pub fn build_coherent_current_book(
    context_key: &str,
    observations: &[MarketEdgeObservation],
    visibility: DataVisibility,
) -> Result<CoherentCurrentBook, MarketBookError> {
    let context_key = context_key.trim();
    if context_key.is_empty() {
        return Err(MarketBookError::MissingContext);
    }

    let mut snapshots =
        BTreeMap::<MarketPairKey, BTreeMap<String, Vec<MarketEdgeObservation>>>::new();
    let mut exclusions = Vec::new();
    for observation in observations {
        let edge = &observation.edge;
        let exclusion = if edge.context_key != context_key {
            Some(BookExclusionReason::ContextMismatch)
        } else if !observation.snapshot_complete {
            Some(BookExclusionReason::IncompleteSnapshot)
        } else if observation.record_status == SnapshotRecordStatus::Isolated
            && !visibility.include_isolated
        {
            Some(BookExclusionReason::IsolatedHidden)
        } else if observation.record_status == SnapshotRecordStatus::Deleted
            && !visibility.include_deleted
        {
            Some(BookExclusionReason::DeletedHidden)
        } else {
            None
        };
        if let Some(reason) = exclusion {
            exclusions.push(BookExclusion {
                edge_id: edge.edge_id.clone(),
                snapshot_id: edge.snapshot_id.clone(),
                reason,
            });
            continue;
        }
        let pair_key = MarketPairKey::try_new(
            edge.original_need_asset_id.clone(),
            edge.original_have_asset_id.clone(),
        )?;
        snapshots
            .entry(pair_key)
            .or_default()
            .entry(edge.snapshot_id.clone())
            .or_default()
            .push(observation.clone());
    }

    let mut views = Vec::with_capacity(snapshots.len());
    for (pair_key, pair_snapshots) in snapshots {
        // Newest snapshot *per panel side*, not per pair. A capture that read
        // only the available table has observed nothing about the competing
        // one, and must not delete what an earlier capture knew about it.
        //
        // This is not hypothetical: 16% of one live session's accepted books
        // held a single row. Under newest-per-pair, each of those erased a
        // full twelve-row book taken seconds earlier — the competing side
        // vanished, and with it the pair's maker reference in one direction
        // and its instant price in the other. Coverage read "missing" for
        // pairs the user had just captured, and the radar starved, because a
        // cycle needs both directions priced and the reverse instant lives on
        // the competing side.
        //
        // Within a side the newest snapshot still wins whole: rows of one
        // side are one order book and must not be mixed across captures.
        let newest_for = |side: QuoteSide| {
            pair_snapshots
                .iter()
                .filter(|(_, observations)| {
                    observations
                        .iter()
                        .any(|observation| observation.edge.source_side == side)
                })
                .max_by(|left, right| {
                    snapshot_time(left.1)
                        .cmp(&snapshot_time(right.1))
                        .then_with(|| left.0.cmp(right.0))
                })
                .map(|(snapshot_id, _)| snapshot_id.clone())
        };
        let available_snapshot = newest_for(QuoteSide::Available);
        let competing_snapshot = newest_for(QuoteSide::Competing);

        let mut selected_observations: Vec<MarketEdgeObservation> = Vec::new();
        for (snapshot_id, observations) in &pair_snapshots {
            for observation in observations {
                let chosen = match observation.edge.source_side {
                    QuoteSide::Available => available_snapshot.as_ref(),
                    QuoteSide::Competing => competing_snapshot.as_ref(),
                };
                if chosen == Some(snapshot_id) {
                    selected_observations.push(observation.clone());
                }
            }
        }
        if selected_observations.is_empty() {
            continue;
        }
        // The newer contributing snapshot names the view; when one snapshot
        // supplies both sides this is exactly the old field.
        let snapshot_id = [&available_snapshot, &competing_snapshot]
            .into_iter()
            .flatten()
            .max_by_key(|id| {
                pair_snapshots
                    .get(*id)
                    .map(|observations| snapshot_time(observations))
            })
            .cloned()
            .unwrap_or_default();
        selected_observations.sort_by(compare_observation_identity);
        views.push(CoherentBookView {
            context_key: context_key.to_owned(),
            pair_key,
            snapshot_id,
            captured_at: snapshot_time(&selected_observations),
            observations: selected_observations,
        });
    }
    views.sort_by(|left, right| left.pair_key.cmp(&right.pair_key));
    exclusions.sort_by(|left, right| {
        left.snapshot_id
            .cmp(&right.snapshot_id)
            .then_with(|| left.edge_id.cmp(&right.edge_id))
            .then_with(|| (left.reason as u8).cmp(&(right.reason as u8)))
    });
    Ok(CoherentCurrentBook {
        context_key: context_key.to_owned(),
        views,
        exclusions,
    })
}

fn snapshot_time(observations: &[MarketEdgeObservation]) -> DateTime<Utc> {
    observations
        .iter()
        .map(|observation| observation.edge.captured_at)
        .max()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn compare_observation_identity(
    left: &MarketEdgeObservation,
    right: &MarketEdgeObservation,
) -> Ordering {
    left.edge
        .source_side
        .cmp(&right.edge.source_side)
        .then_with(|| {
            left.edge
                .original_row_index
                .cmp(&right.edge.original_row_index)
        })
        .then_with(|| left.edge.role.cmp(&right.edge.role))
        .then_with(|| left.edge.edge_id.cmp(&right.edge.edge_id))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessStatus {
    Fresh,
    Usable,
    Stale,
    Archived,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessPolicy {
    pub fresh_max_age_seconds: u64,
    pub usable_max_age_seconds: u64,
    pub stale_max_age_seconds: u64,
}

impl FreshnessPolicy {
    pub fn try_new(
        fresh_max_age_seconds: u64,
        usable_max_age_seconds: u64,
        stale_max_age_seconds: u64,
    ) -> Result<Self, MarketBookError> {
        // F1: strict ordering everywhere. The POE1/POE2 shipping policies set
        // fresh == usable, which made `FreshnessStatus::Usable` unreachable
        // (`classify` tests fresh first); equality is now rejected so the
        // middle band always exists.
        if fresh_max_age_seconds == 0
            || fresh_max_age_seconds >= usable_max_age_seconds
            || usable_max_age_seconds >= stale_max_age_seconds
        {
            return Err(MarketBookError::InvalidFreshnessPolicy);
        }
        Ok(Self {
            fresh_max_age_seconds,
            usable_max_age_seconds,
            stale_max_age_seconds,
        })
    }

    #[must_use]
    pub fn classify(self, captured_at: DateTime<Utc>, now: DateTime<Utc>) -> FreshnessAssessment {
        let signed_age = now.signed_duration_since(captured_at).num_seconds();
        let future_timestamp = signed_age < 0;
        let age_seconds = u64::try_from(signed_age.max(0)).unwrap_or(u64::MAX);
        let status = if age_seconds <= self.fresh_max_age_seconds {
            FreshnessStatus::Fresh
        } else if age_seconds <= self.usable_max_age_seconds {
            FreshnessStatus::Usable
        } else if age_seconds <= self.stale_max_age_seconds {
            FreshnessStatus::Stale
        } else {
            FreshnessStatus::Archived
        };
        FreshnessAssessment {
            status,
            age_seconds,
            future_timestamp,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessAssessment {
    pub status: FreshnessStatus,
    pub age_seconds: u64,
    pub future_timestamp: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessInclusion {
    pub include_fresh: bool,
    pub include_usable: bool,
    pub include_stale: bool,
    pub include_archived: bool,
}

impl FreshnessInclusion {
    #[must_use]
    pub const fn allows(self, status: FreshnessStatus) -> bool {
        match status {
            FreshnessStatus::Fresh => self.include_fresh,
            FreshnessStatus::Usable => self.include_usable,
            FreshnessStatus::Stale => self.include_stale,
            FreshnessStatus::Archived => self.include_archived,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteSelectionStrategy {
    Instant,
    FastMaker,
    BalancedMaker,
    GreedyMaker,
    Probe,
    Historical,
}

pub const PERSONAL_DEFAULT_POLICY_ID: &str = "personal_default_v1";
pub const PERSONAL_DEFAULT_POLICY_SOURCE: &str = "POE Trade Tracker personal default: selection thresholds are provisional; capture skew is measured directly by the auto-watch capture timestamps (F3); fees and minimum lots remain unverified";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCalibrationStatus {
    Unverified,
    Verified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteSelectionPolicyIdentity {
    pub policy_id: String,
    pub source: String,
    pub calibration_status: PolicyCalibrationStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Which trading costs the active policy claims to have verified.
///
/// The two below are the whole model. Profit is gross of anything the game
/// charges outside the exchange rate itself.
pub struct CostVerification {
    pub fee_verified: bool,
    pub minimum_lots_verified: bool,
}

impl CostVerification {
    #[must_use]
    pub const fn all_verified(self) -> bool {
        self.fee_verified && self.minimum_lots_verified
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSkewPolicy {
    pub max_capture_skew_seconds: Option<u64>,
    pub calibration_status: PolicyCalibrationStatus,
}

impl QuoteSelectionStrategy {
    const fn is_execution(self) -> bool {
        matches!(
            self,
            Self::Instant | Self::FastMaker | Self::BalancedMaker | Self::GreedyMaker
        )
    }

    const fn required_execution_type(self) -> Option<ExecutionType> {
        match self {
            Self::Instant => Some(ExecutionType::Taker),
            Self::FastMaker | Self::BalancedMaker | Self::GreedyMaker => {
                Some(ExecutionType::MakerReference)
            }
            Self::Probe | Self::Historical => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteSelectionPolicy {
    pub identity: QuoteSelectionPolicyIdentity,
    pub cost_verification: CostVerification,
    pub capture_skew: CaptureSkewPolicy,
    pub product_execution_allowed: bool,
    pub strategy: QuoteSelectionStrategy,
    pub freshness: FreshnessPolicy,
    pub inclusion: FreshnessInclusion,
    pub minimum_confidence_ppm: u32,
    pub minimum_stock: u64,
    pub top_book_outlier_factor: u64,
    pub allow_price_outliers: bool,
    pub allow_comparator_boundaries: bool,
}

impl QuoteSelectionPolicy {
    pub fn validate(&self) -> Result<(), MarketBookError> {
        FreshnessPolicy::try_new(
            self.freshness.fresh_max_age_seconds,
            self.freshness.usable_max_age_seconds,
            self.freshness.stale_max_age_seconds,
        )?;
        if self.identity.policy_id.trim().is_empty()
            || self.identity.source.trim().is_empty()
            || self.minimum_confidence_ppm > 1_000_000
            || self.top_book_outlier_factor < 2
            || self.capture_skew.max_capture_skew_seconds == Some(0)
            || (self.capture_skew.calibration_status == PolicyCalibrationStatus::Verified
                && self.capture_skew.max_capture_skew_seconds.is_none())
            || (self.product_execution_allowed
                && (self.identity.calibration_status != PolicyCalibrationStatus::Verified
                    || self.capture_skew.calibration_status != PolicyCalibrationStatus::Verified
                    || !self.cost_verification.all_verified()))
        {
            return Err(MarketBookError::InvalidSelectionPolicy);
        }
        Ok(())
    }

    pub fn personal_default(strategy: QuoteSelectionStrategy) -> Result<Self, MarketBookError> {
        let inclusion = match strategy {
            QuoteSelectionStrategy::Historical => FreshnessInclusion {
                include_fresh: true,
                include_usable: true,
                include_stale: true,
                include_archived: true,
            },
            QuoteSelectionStrategy::Probe => FreshnessInclusion {
                include_fresh: true,
                include_usable: true,
                include_stale: true,
                include_archived: false,
            },
            _ => FreshnessInclusion {
                include_fresh: true,
                include_usable: true,
                include_stale: false,
                include_archived: false,
            },
        };
        let policy = Self {
            identity: QuoteSelectionPolicyIdentity {
                policy_id: PERSONAL_DEFAULT_POLICY_ID.to_owned(),
                source: PERSONAL_DEFAULT_POLICY_SOURCE.to_owned(),
                calibration_status: PolicyCalibrationStatus::Unverified,
            },
            cost_verification: CostVerification {
                fee_verified: false,
                minimum_lots_verified: false,
            },
            // F3: the auto-watch loop stamps every book at capture, so the
            // cross-leg skew gate runs armed by default. The window matches
            // the freshness "fresh" band (10 min): the tracker reads one
            // panel at a time, so cross-pair legs are minutes apart by
            // construction — a 90s window would flag every multi-leg result
            // and stop discriminating genuinely stale legs.
            capture_skew: CaptureSkewPolicy {
                max_capture_skew_seconds: Some(600),
                calibration_status: PolicyCalibrationStatus::Verified,
            },
            product_execution_allowed: false,
            strategy,
            // F1: an auto-watching tracker refreshes books continuously, so
            // fresh is minutes, not hours — and strictly below usable.
            freshness: FreshnessPolicy::try_new(10 * 60, 60 * 60, 24 * 60 * 60)?,
            inclusion,
            minimum_confidence_ppm: 850_000,
            minimum_stock: 0,
            top_book_outlier_factor: 3,
            allow_price_outliers: false,
            allow_comparator_boundaries: false,
        };
        policy.validate()?;
        Ok(policy)
    }

    /// The personal default with the user-tuned knobs applied: freshness
    /// windows, cross-leg capture-skew tolerance, and the outlier factor.
    /// Everything else stays canonical, and the tuned values pass the same
    /// validation the defaults do — an invalid combination is the caller's
    /// typed error to degrade from, never a silently patched policy.
    pub fn personal_tuned(
        strategy: QuoteSelectionStrategy,
        freshness: FreshnessPolicy,
        max_capture_skew_seconds: u64,
        top_book_outlier_factor: u64,
    ) -> Result<Self, MarketBookError> {
        let mut policy = Self::personal_default(strategy)?;
        policy.freshness = freshness;
        policy.capture_skew.max_capture_skew_seconds = Some(max_capture_skew_seconds);
        policy.top_book_outlier_factor = top_book_outlier_factor;
        policy.validate()?;
        Ok(policy)
    }

    #[must_use]
    pub fn is_personal_default(&self) -> bool {
        Self::personal_default(self.strategy).is_ok_and(|canonical| *self == canonical)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteRiskFlag {
    ReverseFromAvailable,
    ReverseFromCompeting,
    ComparatorBoundary,
    StaleData,
    ArchivedData,
    LowConfidence,
    IsolatedRecord,
    DeletedRecord,
    FutureTimestamp,
    PriceOutlier,
    OutsideTopBookBand,
    /// Visible stock is an order of magnitude *above* the rest of its own
    /// side. Reported, never rejected: the same shape is either a misread
    /// digit or a genuinely huge listing, and only the reader can tell which.
    /// Only the high side is accused — see [`stock_outlier_quote_ids`].
    StockOutOfBand,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteRejectReason {
    WrongExecutionType,
    Stale,
    Archived,
    FutureTimestamp,
    LowConfidence,
    NoStock,
    ComparatorBoundary,
    IsolatedRecord,
    DeletedRecord,
    PriceOutlier,
    OutsideTopBookBand,
    Duplicate,
    LowerRank,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedQuoteEdge {
    pub observation: MarketEdgeObservation,
    pub freshness: FreshnessAssessment,
    pub effective_confidence_ppm: u32,
    pub risk_flags: Vec<QuoteRiskFlag>,
    pub selection_rejections: Vec<QuoteRejectReason>,
    pub execution_blockers: Vec<QuoteRejectReason>,
    pub accepted_for_selection: bool,
    pub eligible_for_depth_analysis: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRejection {
    pub edge_id: String,
    pub reasons: Vec<QuoteRejectReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedQuoteEdge {
    pub pair_key: String,
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub strategy: QuoteSelectionStrategy,
    pub selected_edge: Option<EvaluatedQuoteEdge>,
    pub candidate_edges: Vec<EvaluatedQuoteEdge>,
    pub rejections: Vec<EdgeRejection>,
    pub execution_eligible: bool,
    pub needs_probe: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteSelectionResult {
    pub context_key: String,
    pub policy: QuoteSelectionPolicy,
    pub selections: Vec<SelectedQuoteEdge>,
}

pub fn select_quote_edges(
    book: &CoherentCurrentBook,
    policy: &QuoteSelectionPolicy,
    now: DateTime<Utc>,
) -> Result<QuoteSelectionResult, MarketBookError> {
    policy.validate()?;
    let mut directional =
        BTreeMap::<(MarketAssetId, MarketAssetId), Vec<(MarketEdgeObservation, bool, bool)>>::new();
    for view in &book.views {
        if view.context_key != book.context_key {
            return Err(MarketBookError::ContextInvariantViolation);
        }
        let outlier_quotes =
            top_book_outlier_quote_ids(&view.observations, policy.top_book_outlier_factor);
        let deep_quotes = stock_outlier_quote_ids(&view.observations, STOCK_OUTLIER_FACTOR);
        for observation in &view.observations {
            directional
                .entry((
                    observation.edge.from_asset_id.clone(),
                    observation.edge.to_asset_id.clone(),
                ))
                .or_default()
                .push((
                    observation.clone(),
                    outlier_quotes.contains(&observation.edge.quote_id),
                    deep_quotes.contains(&observation.edge.quote_id),
                ));
        }
    }

    let mut selections = Vec::with_capacity(directional.len());
    for ((from_asset_id, to_asset_id), observations) in directional {
        let mut candidates = observations
            .into_iter()
            .map(|(observation, is_outlier, stock_out_of_band)| {
                evaluate_edge(observation, is_outlier, stock_out_of_band, policy, now)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| compare_candidates(left, right, policy.strategy));

        let selected_index = candidates
            .iter()
            .position(|candidate| candidate.accepted_for_selection);
        let selected_edge = selected_index.map(|index| candidates[index].clone());
        let selected_rate = selected_edge
            .as_ref()
            .map(|selected| selected.observation.edge.rate.clone());
        let mut rejections = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let reasons = if candidate.selection_rejections.is_empty() {
                if Some(index) == selected_index {
                    Vec::new()
                } else if selected_rate.as_ref().is_some_and(|rate| {
                    rate.compare_value(&candidate.observation.edge.rate) == Ordering::Equal
                }) {
                    vec![QuoteRejectReason::Duplicate]
                } else {
                    vec![QuoteRejectReason::LowerRank]
                }
            } else {
                candidate.selection_rejections.clone()
            };
            if !reasons.is_empty() {
                rejections.push(EdgeRejection {
                    edge_id: candidate.observation.edge.edge_id.clone(),
                    reasons,
                });
            }
        }
        let execution_eligible = selected_edge.as_ref().is_some_and(|selected| {
            selected.eligible_for_depth_analysis && policy.strategy.is_execution()
        }) && policy.product_execution_allowed;
        selections.push(SelectedQuoteEdge {
            pair_key: format!("{from_asset_id}->{to_asset_id}"),
            from_asset_id,
            to_asset_id,
            strategy: policy.strategy,
            selected_edge,
            candidate_edges: candidates,
            rejections,
            execution_eligible,
            needs_probe: !execution_eligible,
        });
    }
    Ok(QuoteSelectionResult {
        context_key: book.context_key.clone(),
        policy: policy.clone(),
        selections,
    })
}

fn evaluate_edge(
    observation: MarketEdgeObservation,
    is_outlier: bool,
    stock_out_of_band: bool,
    policy: &QuoteSelectionPolicy,
    now: DateTime<Utc>,
) -> EvaluatedQuoteEdge {
    let edge = &observation.edge;
    let freshness = policy.freshness.classify(edge.captured_at, now);
    let effective_confidence_ppm = if edge.user_edited {
        1_000_000
    } else {
        edge.machine_confidence_ppm.unwrap_or(0)
    };
    let mut risks = BTreeSet::new();
    let mut execution_blockers = BTreeSet::new();
    let mut selection_rejections = BTreeSet::new();

    match edge.role {
        QuoteEdgeRole::AvailableReverseMakerReference => {
            risks.insert(QuoteRiskFlag::ReverseFromAvailable);
        }
        QuoteEdgeRole::CompetingReverseTaker => {
            risks.insert(QuoteRiskFlag::ReverseFromCompeting);
        }
        QuoteEdgeRole::AvailableTaker | QuoteEdgeRole::CompetingMakerReference => {}
    }
    if edge.comparator != Comparator::Exact {
        risks.insert(QuoteRiskFlag::ComparatorBoundary);
        if !policy.allow_comparator_boundaries {
            execution_blockers.insert(QuoteRejectReason::ComparatorBoundary);
        }
    }
    match freshness.status {
        FreshnessStatus::Stale => {
            risks.insert(QuoteRiskFlag::StaleData);
            execution_blockers.insert(QuoteRejectReason::Stale);
        }
        FreshnessStatus::Archived => {
            risks.insert(QuoteRiskFlag::ArchivedData);
            execution_blockers.insert(QuoteRejectReason::Archived);
        }
        FreshnessStatus::Fresh | FreshnessStatus::Usable => {}
    }
    if !policy.inclusion.allows(freshness.status) {
        selection_rejections.insert(match freshness.status {
            FreshnessStatus::Archived => QuoteRejectReason::Archived,
            _ => QuoteRejectReason::Stale,
        });
    }
    if freshness.future_timestamp {
        risks.insert(QuoteRiskFlag::FutureTimestamp);
        execution_blockers.insert(QuoteRejectReason::FutureTimestamp);
    }
    if effective_confidence_ppm < policy.minimum_confidence_ppm {
        risks.insert(QuoteRiskFlag::LowConfidence);
        execution_blockers.insert(QuoteRejectReason::LowConfidence);
    }
    if edge.stock <= policy.minimum_stock {
        execution_blockers.insert(QuoteRejectReason::NoStock);
        selection_rejections.insert(QuoteRejectReason::NoStock);
    }
    // Risk only, on purpose. A rate an order of magnitude off its side is
    // wrong whichever way you read it, so it earns a blocker; a stock an
    // order of magnitude off is either a misread digit or the one listing
    // that can actually fill the whole order, and dropping the second to
    // catch the first would hide the best row on the panel.
    if stock_out_of_band {
        risks.insert(QuoteRiskFlag::StockOutOfBand);
    }
    match observation.record_status {
        SnapshotRecordStatus::Active => {}
        SnapshotRecordStatus::Isolated => {
            risks.insert(QuoteRiskFlag::IsolatedRecord);
            execution_blockers.insert(QuoteRejectReason::IsolatedRecord);
        }
        SnapshotRecordStatus::Deleted => {
            risks.insert(QuoteRiskFlag::DeletedRecord);
            execution_blockers.insert(QuoteRejectReason::DeletedRecord);
        }
    }
    if is_outlier {
        risks.insert(QuoteRiskFlag::PriceOutlier);
        risks.insert(QuoteRiskFlag::OutsideTopBookBand);
        execution_blockers.insert(QuoteRejectReason::PriceOutlier);
        execution_blockers.insert(QuoteRejectReason::OutsideTopBookBand);
        if !policy.allow_price_outliers {
            selection_rejections.insert(QuoteRejectReason::PriceOutlier);
            selection_rejections.insert(QuoteRejectReason::OutsideTopBookBand);
        }
    }
    if policy
        .strategy
        .required_execution_type()
        .is_some_and(|required| edge.execution_type != required)
    {
        selection_rejections.insert(QuoteRejectReason::WrongExecutionType);
    }
    if policy.strategy.is_execution() {
        selection_rejections.extend(execution_blockers.iter().copied());
    } else if policy.strategy == QuoteSelectionStrategy::Probe
        && observation.record_status == SnapshotRecordStatus::Deleted
    {
        selection_rejections.insert(QuoteRejectReason::DeletedRecord);
    }

    EvaluatedQuoteEdge {
        observation,
        freshness,
        effective_confidence_ppm,
        risk_flags: risks.into_iter().collect(),
        selection_rejections: selection_rejections.iter().copied().collect(),
        execution_blockers: execution_blockers.iter().copied().collect(),
        accepted_for_selection: selection_rejections.is_empty(),
        eligible_for_depth_analysis: execution_blockers.is_empty(),
    }
}

/// The quote ids whose rate sits outside their own side's band, by `factor`.
///
/// Public because there are two doors into the same rows and only one of them
/// used to have this on it. `select_quote_edges` walks the trading path; the
/// day rollup that feeds every valuation reads raw observations and never
/// comes through here — so an OCR row that lost its decimal point priced the
/// Convert page correctly and the market-analysis page a thousand times
/// wrong. Taking a plain slice rather than a `CoherentBookView` is what lets
/// the second caller reach it, and keeps one adjudicator of outlier-ness.
///
/// **The caller must hand in one panel side's worth of rows at most.** The
/// baseline is a median within `source_side`, and pooling two books or two
/// snapshots would compute it against rows that never sat on the same shelf —
/// quote ids are only unique within a snapshot besides.
#[must_use]
pub fn top_book_outlier_quote_ids(
    observations: &[MarketEdgeObservation],
    factor: u64,
) -> BTreeSet<String> {
    let mut current_edges = BTreeMap::<QuoteSide, Vec<&QuoteEdge>>::new();
    for observation in observations {
        let edge = &observation.edge;
        if matches!(
            edge.role,
            QuoteEdgeRole::AvailableTaker | QuoteEdgeRole::CompetingMakerReference
        ) {
            current_edges
                .entry(edge.source_side)
                .or_default()
                .push(edge);
        }
    }
    let mut outliers = BTreeSet::new();
    for edges in current_edges.values_mut() {
        edges.sort_by(|left, right| {
            left.original_row_index
                .cmp(&right.original_row_index)
                .then_with(|| left.quote_id.cmp(&right.quote_id))
        });
        // A malicious listing prices itself to the front of its side, so a
        // front-row baseline moves the whole band around exactly the row it
        // exists to catch. With three or more rows the baseline is the side's
        // median rate instead — element selection on the exact ordering, the
        // lower middle when even, no averaging and no division — which no
        // minority of listings can move, and which may flag the front row
        // itself. Two rows carry no majority either way, so they keep the
        // front row as baseline; one row has nothing to compare against.
        let baseline = if edges.len() >= 3 {
            let mut by_rate = edges.clone();
            by_rate.sort_by(|left, right| {
                left.rate
                    .compare_value(&right.rate)
                    .then_with(|| left.original_row_index.cmp(&right.original_row_index))
                    .then_with(|| left.quote_id.cmp(&right.quote_id))
            });
            by_rate[(by_rate.len() - 1) / 2]
        } else {
            match edges.first() {
                Some(front) => *front,
                None => continue,
            }
        };
        for edge in edges.iter() {
            if edge.rate.differs_by_more_than(&baseline.rate, factor) {
                outliers.insert(edge.quote_id.clone());
            }
        }
    }
    outliers
}

/// The quote ids whose visible stock is more than `factor` times *every other
/// row* on their own side.
///
/// A companion to [`top_book_outlier_quote_ids`] and not a clause inside it,
/// because the two catch opposite failures and deserve opposite verdicts. A
/// rate that lost its decimal point is wrong; a stock that gained a digit is
/// wrong *or* is the deep listing that fills the whole order, so this one
/// names rows and never removes them.
///
/// The band is one-sided on purpose. A row far *below* its side-mates is
/// arithmetically the same picture whether the OCR dropped a digit (1855 read
/// as 185) or someone simply bought most of the listing out — and the second
/// is what an order book does all day, so a badge there would fire constantly
/// and carry no information. A row far *above* them is the digit the reader
/// *gained* (1855 read as 18550), and real listings that far past their
/// neighbours are rare enough that the badge still means something.
///
/// The baseline is the biggest *other* row on the side rather than the side's
/// median, because a median is a number a sold-out majority drags down with
/// it: a side reading 2, 3, 5, 100, 120, 150 has a median of 5, and the three
/// healthy listings would each measure twenty times the band. The largest
/// rival cannot be dragged anywhere. A gained digit dwarfs the entire side by
/// construction, while no number of picked-over rows makes a healthy row look
/// oversized — so this baseline says yes to exactly the case worth saying it
/// for, and at most one row per side can be named, which is what one misread
/// digit looks like.
///
/// Fewer than three rows on a side get no verdict at all. The rate band falls
/// back to the front row there, but that fallback rests on a panel semantic
/// stock does not have — the first listing is the best price, never
/// necessarily the deepest — so on a short side there is simply nothing to
/// compare against.
///
/// **The caller must hand in one panel side's worth of rows at most**, for
/// the same reason the rate band does: the baseline is drawn from within
/// `source_side`, and quote ids are only unique within a snapshot.
#[must_use]
pub fn stock_outlier_quote_ids(
    observations: &[MarketEdgeObservation],
    factor: u64,
) -> BTreeSet<String> {
    let mut current_edges = BTreeMap::<QuoteSide, Vec<&QuoteEdge>>::new();
    for observation in observations {
        let edge = &observation.edge;
        if matches!(
            edge.role,
            QuoteEdgeRole::AvailableTaker | QuoteEdgeRole::CompetingMakerReference
        ) {
            current_edges
                .entry(edge.source_side)
                .or_default()
                .push(edge);
        }
    }
    let mut outliers = BTreeSet::new();
    for edges in current_edges.values_mut() {
        if edges.len() < 3 {
            continue;
        }
        edges.sort_by(|left, right| {
            left.stock
                .cmp(&right.stock)
                .then_with(|| left.original_row_index.cmp(&right.original_row_index))
                .then_with(|| left.quote_id.cmp(&right.quote_id))
        });
        // Sorted ascending, so only the deepest row can ever clear the bar:
        // every other row is measured against a rival at least its own size.
        // That is the rule, not a shortcut — at most one row per side can be
        // named, which is what one misread digit looks like.
        let deepest = edges[edges.len() - 1];
        let largest_rival = edges[edges.len() - 2].stock;
        // One live listing among sold-out neighbours is a thin side, not a
        // misread, and `factor * 0` would accuse it. Empty listings are
        // `NoStock`'s business.
        if largest_rival == 0 {
            continue;
        }
        if deepest.stock > factor.saturating_mul(largest_rival) {
            outliers.insert(deepest.quote_id.clone());
        }
    }
    outliers
}

fn compare_candidates(
    left: &EvaluatedQuoteEdge,
    right: &EvaluatedQuoteEdge,
    strategy: QuoteSelectionStrategy,
) -> Ordering {
    let common = left
        .freshness
        .status
        .cmp(&right.freshness.status)
        .then_with(|| match strategy {
            QuoteSelectionStrategy::GreedyMaker => right
                .observation
                .edge
                .rate
                .compare_value(&left.observation.edge.rate),
            QuoteSelectionStrategy::BalancedMaker => right
                .effective_confidence_ppm
                .cmp(&left.effective_confidence_ppm)
                .then_with(|| {
                    right
                        .observation
                        .edge
                        .rate
                        .compare_value(&left.observation.edge.rate)
                })
                .then_with(|| {
                    right
                        .observation
                        .edge
                        .stock
                        .cmp(&left.observation.edge.stock)
                }),
            QuoteSelectionStrategy::Instant
            | QuoteSelectionStrategy::FastMaker
            | QuoteSelectionStrategy::Probe
            | QuoteSelectionStrategy::Historical => right
                .effective_confidence_ppm
                .cmp(&left.effective_confidence_ppm)
                .then_with(|| {
                    right
                        .observation
                        .edge
                        .stock
                        .cmp(&left.observation.edge.stock)
                })
                .then_with(|| {
                    right
                        .observation
                        .edge
                        .rate
                        .compare_value(&left.observation.edge.rate)
                }),
        });
    common
        .then_with(|| {
            right
                .observation
                .edge
                .captured_at
                .cmp(&left.observation.edge.captured_at)
        })
        .then_with(|| {
            left.observation
                .edge
                .edge_id
                .cmp(&right.observation.edge.edge_id)
        })
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MarketBookError {
    #[error("market context is required")]
    MissingContext,
    #[error("an unordered market pair must contain two different assets")]
    SameAssetPair,
    #[error("freshness thresholds are invalid")]
    InvalidFreshnessPolicy,
    #[error("quote selection policy is invalid")]
    InvalidSelectionPolicy,
    #[error("coherent book context invariant was violated")]
    ContextInvariantViolation,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use ptt_trade_domain::{Ratio, SnapshotRecordStatus};

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 30, hour, 0, 0)
            .single()
            .expect("time")
    }

    #[allow(clippy::too_many_arguments)]
    fn observation(
        edge_id: &str,
        snapshot_id: &str,
        context_key: &str,
        captured_at: DateTime<Utc>,
        side: QuoteSide,
        role: QuoteEdgeRole,
        execution_type: ExecutionType,
        row_index: u8,
        rate: &str,
        status: SnapshotRecordStatus,
        complete: bool,
    ) -> MarketEdgeObservation {
        let need = MarketAssetId::try_new("divine-orb").expect("need");
        let have = MarketAssetId::try_new("chaos-orb").expect("have");
        let (from_asset_id, to_asset_id, rate) = match role {
            QuoteEdgeRole::AvailableTaker | QuoteEdgeRole::CompetingMakerReference => (
                have.clone(),
                need.clone(),
                Ratio::parse(rate).expect("rate"),
            ),
            QuoteEdgeRole::AvailableReverseMakerReference
            | QuoteEdgeRole::CompetingReverseTaker => (
                need.clone(),
                have.clone(),
                Ratio::parse(rate).expect("rate").inverse(),
            ),
        };
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                quote_id: format!("quote-{snapshot_id}-{side:?}-{row_index}"),
                context_key: context_key.to_owned(),
                from_asset_id,
                to_asset_id,
                rate,
                source_side: side,
                execution_type,
                role,
                stock: 100,
                original_need_asset_id: need,
                original_have_asset_id: have,
                original_row_index: row_index,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(980_000),
                captured_at,
                confirmed_at: captured_at,
            },
            snapshot_complete: complete,
            record_status: status,
            record_revision: 1,
            record_reason: None,
        }
    }

    fn snapshot_edges(
        snapshot_id: &str,
        context: &str,
        captured_at: DateTime<Utc>,
        status: SnapshotRecordStatus,
        complete: bool,
    ) -> Vec<MarketEdgeObservation> {
        vec![
            observation(
                &format!("{snapshot_id}-available-current"),
                snapshot_id,
                context,
                captured_at,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableTaker,
                ExecutionType::Taker,
                0,
                "1:180",
                status,
                complete,
            ),
            observation(
                &format!("{snapshot_id}-available-reverse"),
                snapshot_id,
                context,
                captured_at,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableReverseMakerReference,
                ExecutionType::MakerReference,
                0,
                "1:180",
                status,
                complete,
            ),
            observation(
                &format!("{snapshot_id}-competing-current"),
                snapshot_id,
                context,
                captured_at,
                QuoteSide::Competing,
                QuoteEdgeRole::CompetingMakerReference,
                ExecutionType::MakerReference,
                0,
                "1:175",
                status,
                complete,
            ),
            observation(
                &format!("{snapshot_id}-competing-reverse"),
                snapshot_id,
                context,
                captured_at,
                QuoteSide::Competing,
                QuoteEdgeRole::CompetingReverseTaker,
                ExecutionType::Taker,
                0,
                "1:175",
                status,
                complete,
            ),
        ]
    }

    /// A capture that read only one panel table observed nothing about the
    /// other, and must not delete what an earlier capture knew about it.
    ///
    /// Sixteen percent of one live session's accepted books held a single
    /// available row; under newest-per-pair each erased a full book taken
    /// seconds earlier, and the competing side — one direction's maker
    /// reference, the other direction's instant price — vanished from every
    /// report until the pair happened to be recaptured cleanly.
    #[test]
    fn a_one_sided_snapshot_does_not_erase_the_other_side() {
        let mut observations = snapshot_edges(
            "full",
            "context-a",
            at(9),
            SnapshotRecordStatus::Active,
            true,
        );
        // The newer capture: available side only, as a torn frame reads.
        let partial: Vec<MarketEdgeObservation> = snapshot_edges(
            "partial",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            true,
        )
        .into_iter()
        .filter(|observation| observation.edge.source_side == QuoteSide::Available)
        .collect();
        assert_eq!(partial.len(), 2, "the fixture halves cleanly");
        observations.extend(partial);

        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        assert_eq!(book.views.len(), 1);
        let view = &book.views[0];
        // Available rows come from the newer capture, competing rows survive
        // from the older one.
        let sides: Vec<(&str, QuoteSide)> = view
            .observations
            .iter()
            .map(|observation| {
                (
                    observation.edge.snapshot_id.as_str(),
                    observation.edge.source_side,
                )
            })
            .collect();
        assert!(
            sides.contains(&("partial", QuoteSide::Available)),
            "newest available wins: {sides:?}"
        );
        assert!(
            sides.contains(&("full", QuoteSide::Competing)),
            "competing survives: {sides:?}"
        );
        assert!(
            !sides.contains(&("full", QuoteSide::Available)),
            "sides are never mixed within available: {sides:?}"
        );
        // The view is named for the newer contributor.
        assert_eq!(view.snapshot_id, "partial");
    }

    #[test]
    fn coherent_book_uses_only_the_latest_complete_snapshot_in_exact_context() {
        let mut observations = snapshot_edges(
            "old",
            "context-a",
            at(9),
            SnapshotRecordStatus::Active,
            true,
        );
        observations.extend(snapshot_edges(
            "new",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            true,
        ));
        observations.extend(snapshot_edges(
            "foreign",
            "context-b",
            at(11),
            SnapshotRecordStatus::Active,
            true,
        ));
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        assert_eq!(book.views.len(), 1);
        assert_eq!(book.views[0].snapshot_id, "new");
        assert_eq!(book.views[0].observations.len(), 4);
        assert_eq!(book.exclusions.len(), 4);
        assert!(
            book.exclusions
                .iter()
                .all(|item| item.reason == BookExclusionReason::ContextMismatch)
        );
    }

    #[test]
    fn incomplete_newer_snapshot_never_replaces_complete_depth() {
        let mut observations = snapshot_edges(
            "complete",
            "context-a",
            at(9),
            SnapshotRecordStatus::Active,
            true,
        );
        observations.extend(snapshot_edges(
            "partial",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            false,
        ));
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        assert_eq!(book.views[0].snapshot_id, "complete");
        assert_eq!(book.exclusions.len(), 4);
    }

    #[test]
    fn isolation_remains_visible_but_is_never_execution_eligible() {
        let observations = snapshot_edges(
            "isolated",
            "context-a",
            at(10),
            SnapshotRecordStatus::Isolated,
            true,
        );
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        assert_eq!(book.views.len(), 1);
        let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
            .expect("policy");
        let result =
            select_quote_edges(&book, &policy, at(10) + Duration::minutes(5)).expect("selection");
        assert!(
            result
                .selections
                .iter()
                .all(|item| !item.execution_eligible)
        );
        assert!(result.selections.iter().any(|item| {
            item.rejections.iter().any(|rejection| {
                rejection
                    .reasons
                    .contains(&QuoteRejectReason::IsolatedRecord)
            })
        }));
    }

    #[test]
    fn instant_and_maker_selection_never_cross_execution_roles() {
        let observations = snapshot_edges(
            "snapshot",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            true,
        );
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let instant = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("instant policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("instant");
        assert!(instant.selections.iter().all(|selection| {
            selection
                .selected_edge
                .as_ref()
                .is_some_and(|edge| edge.observation.edge.execution_type == ExecutionType::Taker)
        }));
        let maker = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::BalancedMaker)
                .expect("maker policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("maker");
        assert!(maker.selections.iter().all(|selection| {
            selection.selected_edge.as_ref().is_some_and(|edge| {
                edge.observation.edge.execution_type == ExecutionType::MakerReference
            })
        }));
    }

    #[test]
    fn poe1_personal_beta_policy_is_canonical_unverified_and_analysis_only() {
        let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
            .expect("policy");
        assert_eq!(policy.identity.policy_id, PERSONAL_DEFAULT_POLICY_ID);
        assert_eq!(
            policy.identity.calibration_status,
            PolicyCalibrationStatus::Unverified
        );
        assert!(!policy.cost_verification.all_verified());
        assert_eq!(policy.capture_skew.max_capture_skew_seconds, Some(600));
        assert!(!policy.product_execution_allowed);
        assert!(policy.is_personal_default());

        let mut forged = policy.clone();
        forged.minimum_confidence_ppm = 0;
        assert!(!forged.is_personal_default());

        let observations = snapshot_edges(
            "snapshot",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            true,
        );
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result =
            select_quote_edges(&book, &policy, at(10) + Duration::minutes(5)).expect("selection");
        assert!(result.selections.iter().all(|item| {
            item.selected_edge
                .as_ref()
                .is_some_and(|edge| edge.accepted_for_selection && edge.eligible_for_depth_analysis)
                && !item.execution_eligible
                && item.needs_probe
        }));
    }

    #[test]
    fn top_book_outlier_is_visible_and_rejected_without_float_math() {
        let mut observations = snapshot_edges(
            "snapshot",
            "context-a",
            at(10),
            SnapshotRecordStatus::Active,
            true,
        );
        observations.push(observation(
            "outlier-current",
            "snapshot",
            "context-a",
            at(10),
            QuoteSide::Available,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::Taker,
            1,
            "1:1000",
            SnapshotRecordStatus::Active,
            true,
        ));
        observations.push(observation(
            "outlier-reverse",
            "snapshot",
            "context-a",
            at(10),
            QuoteSide::Available,
            QuoteEdgeRole::AvailableReverseMakerReference,
            ExecutionType::MakerReference,
            1,
            "1:1000",
            SnapshotRecordStatus::Active,
            true,
        ));
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let current_direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        assert_eq!(
            current_direction
                .selected_edge
                .as_ref()
                .expect("selected")
                .observation
                .edge
                .edge_id,
            "snapshot-available-current"
        );
        assert!(current_direction.rejections.iter().any(|rejection| {
            rejection.edge_id == "outlier-current"
                && rejection
                    .reasons
                    .contains(&QuoteRejectReason::OutsideTopBookBand)
        }));
    }

    /// One taker row on the named side of one snapshot, for building sides
    /// bigger than the shared fixture's.
    fn taker_row(edge_id: &str, row_index: u8, rate: &str) -> MarketEdgeObservation {
        observation(
            edge_id,
            "snapshot",
            "context-a",
            at(10),
            QuoteSide::Available,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::Taker,
            row_index,
            rate,
            SnapshotRecordStatus::Active,
            true,
        )
    }

    /// The attack the front-row baseline could never see: a too-good-to-be-
    /// true listing sorts itself to the front of its side, becomes the
    /// baseline, and every honest row lands "3× off" it instead — while
    /// selection, absent the band, would pick exactly that bait rate as the
    /// best available. With the median baseline the front row itself is
    /// flagged and selection falls to the best honest row.
    #[test]
    fn a_poisoned_front_row_is_the_outlier_not_the_baseline() {
        let observations = vec![
            taker_row("poisoned-front", 0, "1:50"),
            taker_row("honest-second", 1, "1:180"),
            taker_row("honest-third", 2, "1:175"),
            taker_row("honest-fourth", 3, "1:170"),
        ];
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        assert_eq!(
            direction
                .selected_edge
                .as_ref()
                .expect("selected")
                .observation
                .edge
                .edge_id,
            "honest-fourth",
            "selection must fall to the best honest rate, not the bait"
        );
        assert!(
            direction.rejections.iter().any(|rejection| {
                rejection.edge_id == "poisoned-front"
                    && rejection.reasons.contains(&QuoteRejectReason::PriceOutlier)
            }),
            "the poisoned front row must be the rejected one"
        );
        for honest in ["honest-second", "honest-third", "honest-fourth"] {
            assert!(
                !direction.rejections.iter().any(|rejection| {
                    rejection.edge_id == honest
                        && (rejection.reasons.contains(&QuoteRejectReason::PriceOutlier)
                            || rejection
                                .reasons
                                .contains(&QuoteRejectReason::OutsideTopBookBand))
                }),
                "{honest} sits within the band and must not be called an outlier"
            );
        }
    }

    /// An even-count side has two middles; the lower one (by exact rate
    /// ordering) is the baseline. The rates here are chosen so the two
    /// choices flag disjoint halves — if this test fails, the tie has been
    /// re-decided, not merely re-ordered.
    #[test]
    fn an_even_side_takes_the_lower_middle_as_baseline() {
        let observations = vec![
            taker_row("row-100", 0, "1:100"),
            taker_row("row-101", 1, "1:101"),
            taker_row("row-320", 2, "1:320"),
            taker_row("row-330", 3, "1:330"),
        ];
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        // Ascending by value the side reads 1:330, 1:320, 1:101, 1:100; the
        // lower middle is 1:320, so the 1:10x rows are outside the 3× band
        // and the 1:3xx rows are inside it.
        let rejected: Vec<&str> = direction
            .rejections
            .iter()
            .filter(|rejection| rejection.reasons.contains(&QuoteRejectReason::PriceOutlier))
            .map(|rejection| rejection.edge_id.as_str())
            .collect();
        assert!(
            rejected.contains(&"row-100") && rejected.contains(&"row-101"),
            "the far half must be flagged: {rejected:?}"
        );
        assert!(
            !rejected.contains(&"row-320") && !rejected.contains(&"row-330"),
            "the baseline half must not be flagged: {rejected:?}"
        );
    }

    /// Two rows carry no majority, so the band keeps the front row as
    /// baseline — the pre-median behavior, pinned so shrinking a side does
    /// not silently change which row gets accused.
    #[test]
    fn a_two_row_side_keeps_the_front_row_baseline() {
        let observations = vec![
            taker_row("front", 0, "1:180"),
            taker_row("second", 1, "1:1000"),
        ];
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        assert!(direction.rejections.iter().any(|rejection| {
            rejection.edge_id == "second"
                && rejection.reasons.contains(&QuoteRejectReason::PriceOutlier)
        }));
        assert!(
            !direction
                .rejections
                .iter()
                .any(|rejection| rejection.edge_id == "front"),
        );
    }

    /// One taker row with its own visible stock, for building a side whose
    /// rates agree and whose depths do not.
    fn taker_row_with_stock(edge_id: &str, row_index: u8, stock: u64) -> MarketEdgeObservation {
        let mut row = taker_row(edge_id, row_index, "1:180");
        row.edge.stock = stock;
        row
    }

    /// B-2: a stock that gained a digit passes every rate check — the price is
    /// right, only the depth is wrong — and then walks into coverage, the
    /// liquidity class and the radar's ordering, all of which sum stock. The
    /// band names it. It must not remove it: a real whale listing looks
    /// exactly the same from here, and the tracker reports, it does not
    /// adjudicate.
    #[test]
    fn a_stock_ten_times_its_whole_side_is_flagged_but_not_rejected() {
        let observations = vec![
            taker_row_with_stock("honest-a", 0, 100),
            taker_row_with_stock("honest-b", 1, 120),
            taker_row_with_stock("honest-c", 2, 90),
            taker_row_with_stock("honest-d", 3, 110),
            // The row that should have read 185 and came back 1855.
            taker_row_with_stock("misread", 4, 1855),
        ];
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        let misread = direction
            .candidate_edges
            .iter()
            .find(|candidate| candidate.observation.edge.edge_id == "misread")
            .expect("misread candidate");
        assert!(
            misread.risk_flags.contains(&QuoteRiskFlag::StockOutOfBand),
            "the misread depth must be named: {:?}",
            misread.risk_flags
        );
        assert!(
            misread.accepted_for_selection,
            "a flagged stock is still a usable quote"
        );
        assert!(
            misread.execution_blockers.is_empty(),
            "a flagged stock must not block execution: {:?}",
            misread.execution_blockers
        );
        for honest in ["honest-a", "honest-b", "honest-c", "honest-d"] {
            let candidate = direction
                .candidate_edges
                .iter()
                .find(|candidate| candidate.observation.edge.edge_id == honest)
                .expect("honest candidate");
            assert!(
                !candidate
                    .risk_flags
                    .contains(&QuoteRiskFlag::StockOutOfBand),
                "{honest} sits inside the band and must not be accused"
            );
        }
    }

    /// The band must stay one-sided. A front row down to 4 of a side whose
    /// median is 100 is the most ordinary thing an order book does — someone
    /// bought most of it — and a badge there would fire every day and mean
    /// nothing. The same side's 1855 still has to be named, so this pins both
    /// halves at once.
    #[test]
    fn a_nearly_exhausted_row_is_not_accused_but_an_oversized_one_still_is() {
        let observations = vec![
            // Bought down to its last few, still listed, still honest.
            taker_row_with_stock("exhausted", 0, 4),
            taker_row_with_stock("honest-a", 1, 90),
            taker_row_with_stock("honest-b", 2, 100),
            taker_row_with_stock("honest-c", 3, 110),
            taker_row_with_stock("honest-d", 4, 120),
            // The row that should have read 185 and came back 1855.
            taker_row_with_stock("misread", 5, 1855),
        ];
        let book =
            build_coherent_current_book("context-a", &observations, DataVisibility::default())
                .expect("book");
        let result = select_quote_edges(
            &book,
            &QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
                .expect("policy"),
            at(10) + Duration::minutes(5),
        )
        .expect("selection");
        let direction = result
            .selections
            .iter()
            .find(|item| item.pair_key == "chaos-orb->divine-orb")
            .expect("direction");
        let flagged = |edge_id: &str| {
            direction
                .candidate_edges
                .iter()
                .find(|candidate| candidate.observation.edge.edge_id == edge_id)
                .expect("candidate")
                .risk_flags
                .contains(&QuoteRiskFlag::StockOutOfBand)
        };
        assert!(
            !flagged("exhausted"),
            "a nearly sold-out row is normal trading, not a misread"
        );
        assert!(
            flagged("misread"),
            "a stock ten times its side's median must still be named"
        );
    }

    /// Late in a listing's life most of a side is picked over. A side reading
    /// 2, 3, 5, 100, 120, 150 has a median of 5, and against that baseline the
    /// three healthy listings all measure twenty times the band — three badges
    /// on the only rows worth trading with. The baseline has to be something a
    /// sold-out majority cannot drag down.
    #[test]
    fn a_sold_out_majority_does_not_make_the_healthy_rows_oversized() {
        let observations = vec![
            taker_row_with_stock("picked-a", 0, 2),
            taker_row_with_stock("picked-b", 1, 3),
            taker_row_with_stock("picked-c", 2, 5),
            taker_row_with_stock("healthy-a", 3, 100),
            taker_row_with_stock("healthy-b", 4, 120),
            taker_row_with_stock("healthy-c", 5, 150),
        ];
        let flagged = stock_outlier_quote_ids(&observations, STOCK_OUTLIER_FACTOR);
        assert!(
            flagged.is_empty(),
            "no row towers over the whole side, so none is a misread: {flagged:?}"
        );
    }

    /// A side of two rows is not a side to tower over — one listing being ten
    /// times the only other one is an ordinary deep-versus-shallow pair. And
    /// unlike the rate band there is no front-row fallback worth having: the
    /// first listing is the best price, never the deepest one.
    #[test]
    fn a_two_row_side_gets_no_stock_verdict() {
        assert!(
            stock_outlier_quote_ids(
                &[
                    taker_row_with_stock("front", 0, 100),
                    taker_row_with_stock("second", 1, 99_999),
                ],
                STOCK_OUTLIER_FACTOR,
            )
            .is_empty()
        );
    }

    /// F1: the POE1/POE2 shipping bug — fresh == usable made `Usable`
    /// unreachable. Equality must be rejected at construction, and the
    /// default policy must classify some age as `Usable`.
    #[test]
    fn f1_usable_freshness_band_is_reachable() {
        assert!(FreshnessPolicy::try_new(7200, 7200, 86_400).is_err());

        let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
            .expect("default policy")
            .freshness;
        assert!(policy.fresh_max_age_seconds < policy.usable_max_age_seconds);
        let now = at(20);
        let mid_age = i64::try_from(policy.fresh_max_age_seconds + 1).expect("fits");
        assert_eq!(
            policy
                .classify(now - Duration::seconds(mid_age), now)
                .status,
            FreshnessStatus::Usable
        );
    }

    #[test]
    fn freshness_boundaries_and_future_time_are_explicit() {
        let policy = FreshnessPolicy::try_new(10, 20, 30).expect("policy");
        let now = at(10);
        assert_eq!(
            policy.classify(now - Duration::seconds(10), now).status,
            FreshnessStatus::Fresh
        );
        assert_eq!(
            policy.classify(now - Duration::seconds(11), now).status,
            FreshnessStatus::Usable
        );
        assert_eq!(
            policy.classify(now - Duration::seconds(21), now).status,
            FreshnessStatus::Stale
        );
        assert_eq!(
            policy.classify(now - Duration::seconds(31), now).status,
            FreshnessStatus::Archived
        );
        assert!(
            policy
                .classify(now + Duration::seconds(1), now)
                .future_timestamp
        );
    }
}
