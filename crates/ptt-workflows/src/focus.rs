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

impl ProbePriority {
    /// One step more urgent. Used when a gap touches a currency the market
    /// pulse marks as scarce or high-turnover: the pairs that cost the most
    /// to leave unprobed.
    #[must_use]
    pub const fn raised(self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium | Self::High => Self::High,
        }
    }
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

    /// 整个市场当一张图：锚是结算通货，其余全部当中转，没有 target。
    /// 大雷达要的是"任意两两成边、环路扫全图"，而 `try_new` 没有 target 就不 Ready——
    /// 这里另开一条路：直兑扫描只扫锚↔锚（两三对，便宜且有意义），环路扫整个索引。
    pub fn whole_market(
        anchors: &[MarketAssetId],
        members: &[MarketAssetId],
    ) -> Result<Self, WorkflowError> {
        let mut anchors = anchors.to_vec();
        anchors.sort();
        anchors.dedup();
        if anchors.is_empty() {
            return Err(WorkflowError::InvalidFocusScope);
        }
        let mut bridges = members
            .iter()
            .filter(|asset_id| !anchors.contains(asset_id))
            .cloned()
            .collect::<Vec<_>>();
        bridges.sort();
        bridges.dedup();
        let counts = FocusRoleCounts {
            anchor_count: count(&anchors)?,
            target_count: 0,
            bridge_count: count(&bridges)?,
            watch_only_count: 0,
        };
        let mut intermediate_asset_ids = anchors.clone();
        intermediate_asset_ids.extend(bridges.iter().cloned());
        intermediate_asset_ids.sort();
        let mut directed_pairs = Vec::new();
        let mut seen = BTreeSet::new();
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
        Ok(Self {
            status: FocusScopeStatus::Ready,
            counts,
            endpoint_asset_ids: anchors.clone(),
            anchors,
            targets: Vec::new(),
            bridges,
            watch_only: Vec::new(),
            intermediate_asset_ids,
            directed_pairs,
            allow_target_interconnect: false,
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

    #[test]
    fn whole_market_lets_any_two_members_form_an_edge() {
        let scope = FocusScope::whole_market(
            &[asset("exalted")],
            &[asset("exalted"), asset("divine"), asset("chaos")],
        )
        .expect("scope");

        assert_eq!(scope.status, FocusScopeStatus::Ready);
        assert!(scope.targets.is_empty());
        assert_eq!(scope.bridges, vec![asset("chaos"), asset("divine")]);
        // bridge↔bridge 也成边：环路扫描要走整张图。
        assert!(scope.edge_allowed(&asset("divine"), &asset("chaos")));
        assert!(scope.edge_allowed(&asset("exalted"), &asset("chaos")));
        assert!(!scope.edge_allowed(&asset("chaos"), &asset("chaos")));
        assert!(!scope.edge_allowed(&asset("chaos"), &asset("unknown")));
        assert!(scope.intermediate_allowed(&asset("divine")));
        assert!(scope.intermediate_allowed(&asset("exalted")));
        // 只有一个锚：没有锚↔锚的直兑对。
        assert!(scope.directed_pairs.is_empty());
    }

    #[test]
    fn whole_market_endpoints_are_only_the_anchors() {
        let scope = FocusScope::whole_market(
            &[asset("exalted"), asset("divine"), asset("exalted")],
            &[asset("divine"), asset("chaos"), asset("chaos")],
        )
        .expect("scope");

        assert_eq!(scope.anchors, vec![asset("divine"), asset("exalted")]);
        assert_eq!(
            scope.endpoint_asset_ids,
            vec![asset("divine"), asset("exalted")]
        );
        assert_eq!(scope.bridges, vec![asset("chaos")]);
        assert_eq!(scope.counts.anchor_count, 2);
        assert_eq!(scope.counts.bridge_count, 1);
        assert_eq!(scope.counts.target_count, 0);
        // 两个锚：正反两条锚↔锚直兑对，都是 Medium。
        assert_eq!(scope.directed_pairs.len(), 2);
        assert!(scope.endpoint_pair_allowed(&asset("exalted"), &asset("divine")));
        assert!(scope.endpoint_pair_allowed(&asset("divine"), &asset("exalted")));
        assert!(!scope.endpoint_pair_allowed(&asset("exalted"), &asset("chaos")));
    }

    #[test]
    fn whole_market_needs_at_least_one_anchor() {
        let error = FocusScope::whole_market(&[], &[asset("divine")]).expect_err("no anchor");
        assert_eq!(error, WorkflowError::InvalidFocusScope);
    }
}
