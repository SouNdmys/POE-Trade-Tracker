use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ptt_market_book::{
    FreshnessStatus, QuoteRejectReason, QuoteRiskFlag, QuoteSelectionResult, SelectedQuoteEdge,
};
use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

use crate::WorkflowError;
use crate::focus::{FocusScope, ProbePriority};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeReason {
    MissingForwardQuote,
    MissingInstantQuote,
    MissingMakerReferenceQuote,
    OnlyOldData,
    LowConfidence,
    ComparatorBoundary,
    ThinLiquidity,
    MissingBridgeQuote,
    OpportunityConfirmation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeSource {
    FocusGroup,
    QuoteSelection,
    BestConversion,
    Triangle,
    MakerStrategy,
    OpportunityRadar,
    /// A leg the reader queued from the exchange radar, kept until a fresh
    /// taker capture answers it.
    ExchangeRadar,
    Manual,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Open,
    InProgress,
    Done,
    Ignored,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusCoverageStatus {
    Complete,
    MissingInstant,
    MissingMakerReference,
    MissingBoth,
    OnlyOldData,
    NeedsReview,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusCoverage {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub status: FocusCoverageStatus,
    pub instant_available: bool,
    pub maker_reference_available: bool,
    pub latest_observation_at: Option<DateTime<Utc>>,
    pub freshness_status: Option<FreshnessStatus>,
    pub priority: ProbePriority,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeCandidate {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub reason: ProbeReason,
    pub source: ProbeSource,
    pub priority: ProbePriority,
    pub related_focus_group_id: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub freshness_status: Option<FreshnessStatus>,
    pub expected_value_hint: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSuggestion {
    pub probe_suggestion_id: String,
    pub context_key: String,
    pub candidate: ProbeCandidate,
    pub status: ProbeStatus,
    pub revision: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub fn derive_focus_probe_candidates(
    focus_group_id: &str,
    scope: &FocusScope,
    instant: &QuoteSelectionResult,
    maker: &QuoteSelectionResult,
    probe: &QuoteSelectionResult,
) -> Result<(Vec<FocusCoverage>, Vec<ProbeCandidate>), WorkflowError> {
    if focus_group_id.trim().is_empty()
        || instant.context_key != maker.context_key
        || instant.context_key != probe.context_key
    {
        return Err(WorkflowError::InvalidRequest);
    }
    let instant_pairs = selection_map(instant);
    let maker_pairs = selection_map(maker);
    let probe_pairs = selection_map(probe);
    let mut coverage = Vec::with_capacity(scope.directed_pairs.len());
    let mut candidates = Vec::new();

    for pair in &scope.directed_pairs {
        let key = (pair.from_asset_id.clone(), pair.to_asset_id.clone());
        let instant_selection = instant_pairs.get(&key).copied();
        let maker_selection = maker_pairs.get(&key).copied();
        let probe_selection = probe_pairs.get(&key).copied();
        let instant_available = instant_selection.is_some_and(selection_has_current_edge);
        let maker_reference_available = maker_selection.is_some_and(selection_has_current_edge);
        let evidence = probe_selection.map(probe_evidence).unwrap_or_default();
        let status = if instant_available && maker_reference_available {
            FocusCoverageStatus::Complete
        } else if evidence.needs_review {
            FocusCoverageStatus::NeedsReview
        } else if evidence.only_old_data {
            FocusCoverageStatus::OnlyOldData
        } else if instant_available {
            FocusCoverageStatus::MissingMakerReference
        } else if maker_reference_available {
            FocusCoverageStatus::MissingInstant
        } else {
            FocusCoverageStatus::MissingBoth
        };
        coverage.push(FocusCoverage {
            from_asset_id: pair.from_asset_id.clone(),
            to_asset_id: pair.to_asset_id.clone(),
            status,
            instant_available,
            maker_reference_available,
            latest_observation_at: evidence.latest_observation_at,
            freshness_status: evidence.best_freshness,
            priority: pair.priority,
        });
        if status != FocusCoverageStatus::Complete {
            candidates.push(ProbeCandidate {
                from_asset_id: pair.from_asset_id.clone(),
                to_asset_id: pair.to_asset_id.clone(),
                reason: reason_for_status(status, &evidence),
                source: ProbeSource::FocusGroup,
                priority: priority_for_status(status, pair.priority),
                related_focus_group_id: Some(focus_group_id.to_owned()),
                last_seen_at: evidence.latest_observation_at,
                freshness_status: evidence.best_freshness,
                expected_value_hint: None,
                notes: Some(format!("focus:{focus_group_id}")),
            });
        }
    }
    Ok((coverage, candidates))
}

fn selection_map(
    result: &QuoteSelectionResult,
) -> BTreeMap<(MarketAssetId, MarketAssetId), &SelectedQuoteEdge> {
    result
        .selections
        .iter()
        .map(|selection| {
            (
                (
                    selection.from_asset_id.clone(),
                    selection.to_asset_id.clone(),
                ),
                selection,
            )
        })
        .collect()
}

fn selection_has_current_edge(selection: &SelectedQuoteEdge) -> bool {
    selection.selected_edge.as_ref().is_some_and(|edge| {
        edge.accepted_for_selection
            && edge.eligible_for_depth_analysis
            && matches!(
                edge.freshness.status,
                FreshnessStatus::Fresh | FreshnessStatus::Usable
            )
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct ProbeEvidence {
    only_old_data: bool,
    needs_review: bool,
    low_confidence: bool,
    comparator_boundary: bool,
    latest_observation_at: Option<DateTime<Utc>>,
    best_freshness: Option<FreshnessStatus>,
}

fn probe_evidence(selection: &SelectedQuoteEdge) -> ProbeEvidence {
    let mut evidence = ProbeEvidence::default();
    for edge in &selection.candidate_edges {
        evidence.latest_observation_at = Some(
            evidence
                .latest_observation_at
                .map_or(edge.observation.edge.captured_at, |current| {
                    current.max(edge.observation.edge.captured_at)
                }),
        );
        evidence.best_freshness = Some(match evidence.best_freshness {
            Some(current) => current.min(edge.freshness.status),
            None => edge.freshness.status,
        });
        evidence.only_old_data |= matches!(
            edge.freshness.status,
            FreshnessStatus::Stale | FreshnessStatus::Archived
        );
        evidence.low_confidence |= edge.risk_flags.contains(&QuoteRiskFlag::LowConfidence)
            || edge
                .execution_blockers
                .contains(&QuoteRejectReason::LowConfidence);
        evidence.comparator_boundary |=
            edge.risk_flags.contains(&QuoteRiskFlag::ComparatorBoundary)
                || edge
                    .execution_blockers
                    .contains(&QuoteRejectReason::ComparatorBoundary);
    }
    evidence.needs_review = evidence.low_confidence || evidence.comparator_boundary;
    evidence
}

const fn reason_for_status(status: FocusCoverageStatus, evidence: &ProbeEvidence) -> ProbeReason {
    match status {
        FocusCoverageStatus::MissingInstant => ProbeReason::MissingInstantQuote,
        FocusCoverageStatus::MissingMakerReference => ProbeReason::MissingMakerReferenceQuote,
        FocusCoverageStatus::MissingBoth => ProbeReason::MissingForwardQuote,
        FocusCoverageStatus::OnlyOldData => ProbeReason::OnlyOldData,
        FocusCoverageStatus::NeedsReview if evidence.comparator_boundary => {
            ProbeReason::ComparatorBoundary
        }
        FocusCoverageStatus::NeedsReview => ProbeReason::LowConfidence,
        FocusCoverageStatus::Complete => ProbeReason::OpportunityConfirmation,
    }
}

const fn priority_for_status(
    status: FocusCoverageStatus,
    pair_priority: ProbePriority,
) -> ProbePriority {
    match status {
        FocusCoverageStatus::MissingInstant | FocusCoverageStatus::MissingBoth => {
            ProbePriority::High
        }
        FocusCoverageStatus::MissingMakerReference
        | FocusCoverageStatus::OnlyOldData
        | FocusCoverageStatus::NeedsReview => match pair_priority {
            ProbePriority::High => ProbePriority::Medium,
            other => other,
        },
        FocusCoverageStatus::Complete => ProbePriority::Low,
    }
}
