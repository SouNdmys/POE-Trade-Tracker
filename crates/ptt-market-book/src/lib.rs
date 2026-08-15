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
        let selected = pair_snapshots.into_iter().max_by(|left, right| {
            snapshot_time(&left.1)
                .cmp(&snapshot_time(&right.1))
                .then_with(|| left.0.cmp(&right.0))
        });
        let Some((snapshot_id, mut selected_observations)) = selected else {
            continue;
        };
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
pub struct CostVerification {
    pub fee_verified: bool,
    pub gold_cost_verified: bool,
    pub minimum_lots_verified: bool,
}

impl CostVerification {
    #[must_use]
    pub const fn all_verified(self) -> bool {
        self.fee_verified && self.gold_cost_verified && self.minimum_lots_verified
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
                gold_cost_verified: false,
                minimum_lots_verified: false,
            },
            // F3: the auto-watch loop stamps every book at capture, so the
            // cross-leg skew gate runs armed by default — multi-pair routes
            // whose legs were captured more than 90s apart are flagged and
            // lose execution eligibility instead of passing silently.
            capture_skew: CaptureSkewPolicy {
                max_capture_skew_seconds: Some(90),
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
        BTreeMap::<(MarketAssetId, MarketAssetId), Vec<(MarketEdgeObservation, bool)>>::new();
    for view in &book.views {
        if view.context_key != book.context_key {
            return Err(MarketBookError::ContextInvariantViolation);
        }
        let outlier_quotes = top_book_outlier_quote_ids(view, policy.top_book_outlier_factor);
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
                ));
        }
    }

    let mut selections = Vec::with_capacity(directional.len());
    for ((from_asset_id, to_asset_id), observations) in directional {
        let mut candidates = observations
            .into_iter()
            .map(|(observation, is_outlier)| evaluate_edge(observation, is_outlier, policy, now))
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

fn top_book_outlier_quote_ids(view: &CoherentBookView, factor: u64) -> BTreeSet<String> {
    let mut current_edges = BTreeMap::<QuoteSide, Vec<&QuoteEdge>>::new();
    for observation in &view.observations {
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
        let Some(baseline) = edges.first() else {
            continue;
        };
        for edge in edges.iter().skip(1) {
            if edge.rate.differs_by_more_than(&baseline.rate, factor) {
                outliers.insert(edge.quote_id.clone());
            }
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
        assert_eq!(policy.capture_skew.max_capture_skew_seconds, Some(90));
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
