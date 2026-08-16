use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

use crate::WorkflowError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusRole {
    Anchor,
    Target,
    Bridge,
    WatchOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbePriority {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusPairRelation {
    AnchorTarget,
    AnchorAnchor,
    TargetTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusGroupItem {
    pub asset_id: MarketAssetId,
    pub role: FocusRole,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusGroup {
    pub focus_group_id: String,
    pub context_key: String,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub include_in_algorithms: bool,
    pub show_refresh_alerts: bool,
    pub stale_after_minutes: u32,
    pub allow_target_interconnect: bool,
    pub revision: u32,
    pub items: Vec<FocusGroupItem>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusGroupDraft {
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub include_in_algorithms: bool,
    pub show_refresh_alerts: bool,
    pub stale_after_minutes: u32,
    pub allow_target_interconnect: bool,
    pub items: Vec<FocusGroupItem>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusScopePolicy {
    pub include_bridges: bool,
    pub allow_target_interconnect: bool,
}

impl Default for FocusScopePolicy {
    fn default() -> Self {
        Self {
            include_bridges: true,
            allow_target_interconnect: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusScopeStatus {
    Ready,
    MissingAnchor,
    MissingTarget,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusRoleCounts {
    pub anchor_count: u32,
    pub target_count: u32,
    pub bridge_count: u32,
    pub watch_only_count: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectedFocusPair {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub from_role: FocusRole,
    pub to_role: FocusRole,
    pub priority: ProbePriority,
    pub relation: FocusPairRelation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusScope {
    pub status: FocusScopeStatus,
    pub counts: FocusRoleCounts,
    pub anchors: Vec<MarketAssetId>,
    pub targets: Vec<MarketAssetId>,
    pub bridges: Vec<MarketAssetId>,
    pub watch_only: Vec<MarketAssetId>,
    pub endpoint_asset_ids: Vec<MarketAssetId>,
    pub intermediate_asset_ids: Vec<MarketAssetId>,
    pub directed_pairs: Vec<DirectedFocusPair>,
    pub allow_target_interconnect: bool,
}

impl FocusScope {
    pub fn try_new(
        items: &[FocusGroupItem],
        policy: FocusScopePolicy,
    ) -> Result<Self, WorkflowError> {
        if items.is_empty() {
            return Err(WorkflowError::InvalidFocusScope);
        }
        let mut roles = BTreeMap::new();
        for item in items {
            if roles.insert(item.asset_id.clone(), item.role).is_some() {
                return Err(WorkflowError::InvalidFocusScope);
            }
        }
        let by_role = |role| {
            roles
                .iter()
                .filter(|(_, candidate)| **candidate == role)
                .map(|(asset_id, _)| asset_id.clone())
                .collect::<Vec<_>>()
        };
        let anchors = by_role(FocusRole::Anchor);
        let targets = by_role(FocusRole::Target);
        let bridges = by_role(FocusRole::Bridge);
        let watch_only = by_role(FocusRole::WatchOnly);
        let status = if anchors.is_empty() {
            FocusScopeStatus::MissingAnchor
        } else if targets.is_empty() {
            FocusScopeStatus::MissingTarget
        } else {
            FocusScopeStatus::Ready
        };
        let counts = FocusRoleCounts {
            anchor_count: count(&anchors)?,
            target_count: count(&targets)?,
            bridge_count: count(&bridges)?,
            watch_only_count: count(&watch_only)?,
        };
        let mut endpoint_asset_ids = anchors
            .iter()
            .chain(targets.iter())
            .cloned()
            .collect::<Vec<_>>();
        endpoint_asset_ids.sort();
        endpoint_asset_ids.dedup();
        let mut intermediate_asset_ids = anchors.clone();
        if policy.include_bridges {
            intermediate_asset_ids.extend(bridges.iter().cloned());
        }
        if policy.allow_target_interconnect {
            intermediate_asset_ids.extend(targets.iter().cloned());
        }
        intermediate_asset_ids.sort();
        intermediate_asset_ids.dedup();

        let mut directed_pairs = Vec::new();
        let mut seen = BTreeSet::new();
        for anchor in &anchors {
            for target in &targets {
                push_pair(
                    &mut directed_pairs,
                    &mut seen,
                    anchor,
                    target,
                    FocusRole::Anchor,
                    FocusRole::Target,
                    ProbePriority::High,
                    FocusPairRelation::AnchorTarget,
                );
                push_pair(
                    &mut directed_pairs,
                    &mut seen,
                    target,
                    anchor,
                    FocusRole::Target,
                    FocusRole::Anchor,
                    ProbePriority::High,
                    FocusPairRelation::AnchorTarget,
                );
            }
        }
        for from in &anchors {
            for to in &anchors {
                push_pair(
                    &mut directed_pairs,
                    &mut seen,
                    from,
                    to,
                    FocusRole::Anchor,
                    FocusRole::Anchor,
                    ProbePriority::Medium,
                    FocusPairRelation::AnchorAnchor,
                );
            }
        }
        if policy.allow_target_interconnect {
            for from in &targets {
                for to in &targets {
                    push_pair(
                        &mut directed_pairs,
                        &mut seen,
                        from,
                        to,
                        FocusRole::Target,
                        FocusRole::Target,
                        ProbePriority::Low,
                        FocusPairRelation::TargetTarget,
                    );
                }
            }
        }
        directed_pairs.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.from_asset_id.cmp(&right.from_asset_id))
                .then_with(|| left.to_asset_id.cmp(&right.to_asset_id))
        });
        Ok(Self {
            status,
            counts,
            anchors,
            targets,
            bridges,
            watch_only,
            endpoint_asset_ids,
            intermediate_asset_ids,
            directed_pairs,
            allow_target_interconnect: policy.allow_target_interconnect,
        })
    }

    #[must_use]
    pub fn endpoint_pair_allowed(&self, from: &MarketAssetId, to: &MarketAssetId) -> bool {
        self.directed_pairs
            .iter()
            .any(|pair| pair.from_asset_id == *from && pair.to_asset_id == *to)
    }

    #[must_use]
    pub fn intermediate_allowed(&self, asset_id: &MarketAssetId) -> bool {
        self.intermediate_asset_ids.contains(asset_id)
    }

    #[must_use]
    pub fn edge_allowed(&self, from: &MarketAssetId, to: &MarketAssetId) -> bool {
        if from == to
            || self.watch_only.contains(from)
            || self.watch_only.contains(to)
            || !self.member_allowed(from)
            || !self.member_allowed(to)
        {
            return false;
        }
        self.allow_target_interconnect
            || !(self.targets.contains(from) && self.targets.contains(to))
    }

    fn member_allowed(&self, asset_id: &MarketAssetId) -> bool {
        self.anchors.contains(asset_id)
            || self.targets.contains(asset_id)
            || self.bridges.contains(asset_id)
    }
}

#[allow(clippy::too_many_arguments)]
fn push_pair(
    pairs: &mut Vec<DirectedFocusPair>,
    seen: &mut BTreeSet<(MarketAssetId, MarketAssetId)>,
    from: &MarketAssetId,
    to: &MarketAssetId,
    from_role: FocusRole,
    to_role: FocusRole,
    priority: ProbePriority,
    relation: FocusPairRelation,
) {
    if from == to || !seen.insert((from.clone(), to.clone())) {
        return;
    }
    pairs.push(DirectedFocusPair {
        from_asset_id: from.clone(),
        to_asset_id: to.clone(),
        from_role,
        to_role,
        priority,
        relation,
    });
}

fn count<T>(values: &[T]) -> Result<u32, WorkflowError> {
    u32::try_from(values.len()).map_err(|_| WorkflowError::NumericOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(value: &str) -> MarketAssetId {
        MarketAssetId::try_new(value).expect("asset")
    }

    #[test]
    fn roles_define_endpoints_intermediates_and_watch_exclusion() {
        let scope = FocusScope::try_new(
            &[
                FocusGroupItem {
                    asset_id: asset("anchor"),
                    role: FocusRole::Anchor,
                },
                FocusGroupItem {
                    asset_id: asset("target-a"),
                    role: FocusRole::Target,
                },
                FocusGroupItem {
                    asset_id: asset("target-b"),
                    role: FocusRole::Target,
                },
                FocusGroupItem {
                    asset_id: asset("bridge"),
                    role: FocusRole::Bridge,
                },
                FocusGroupItem {
                    asset_id: asset("watch"),
                    role: FocusRole::WatchOnly,
                },
            ],
            FocusScopePolicy::default(),
        )
        .expect("scope");
        assert_eq!(scope.status, FocusScopeStatus::Ready);
        assert_eq!(scope.directed_pairs.len(), 4);
        assert!(scope.intermediate_asset_ids.contains(&asset("bridge")));
        assert!(!scope.endpoint_asset_ids.contains(&asset("bridge")));
        assert!(!scope.endpoint_asset_ids.contains(&asset("watch")));
        assert!(!scope.endpoint_pair_allowed(&asset("target-a"), &asset("target-b")));
    }

    #[test]
    fn target_interconnect_is_an_explicit_low_priority_option() {
        let scope = FocusScope::try_new(
            &[
                FocusGroupItem {
                    asset_id: asset("anchor"),
                    role: FocusRole::Anchor,
                },
                FocusGroupItem {
                    asset_id: asset("target-a"),
                    role: FocusRole::Target,
                },
                FocusGroupItem {
                    asset_id: asset("target-b"),
                    role: FocusRole::Target,
                },
            ],
            FocusScopePolicy {
                include_bridges: true,
                allow_target_interconnect: true,
            },
        )
        .expect("scope");
        let pair = scope
            .directed_pairs
            .iter()
            .find(|pair| {
                pair.from_asset_id == asset("target-a") && pair.to_asset_id == asset("target-b")
            })
            .expect("target pair");
        assert_eq!(pair.priority, ProbePriority::Low);
        assert_eq!(pair.relation, FocusPairRelation::TargetTarget);
    }
}
