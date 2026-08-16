//! Which currencies the league treats as money, and which of them earn it.
//!
//! Ported from `shared/marketPolicy.ts`. Two changes. Roles were free strings
//! whose capabilities were looked up in four near-identical object literals;
//! they are an enum with the capabilities derived from it, so a typo cannot
//! quietly produce a currency that can do nothing. And the anchor
//! recommendation score used `Math.log10` inside a value that results are
//! then sorted by, which makes the ordering depend on floating point; the
//! score here is an integer in tenths, so the same book always ranks the same
//! way.

use std::collections::{BTreeMap, BTreeSet};

use ptt_market_book::{EvaluatedQuoteEdge, FreshnessStatus};
use ptt_trade_domain::{MarketAssetId, QuoteSide};
use serde::{Deserialize, Serialize};

/// The currencies a fresh install treats as money.
pub const DEFAULT_CORE_LIQUIDITY: [&str; 3] = ["divine-orb", "chaos-orb", "exalted-orb"];

/// What a currency is allowed to do in route search and valuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRole {
    /// Core liquidity: money. May do anything.
    Anchor,
    /// Something you want to buy or sell, but not route through.
    Target,
    /// Useful only in the middle of a route.
    Bridge,
    /// Tracked, but never used in a calculation.
    WatchOnly,
}

/// What a role may do.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleCapabilities {
    pub can_start_route: bool,
    pub can_end_route: bool,
    pub can_bridge_route: bool,
    pub can_price_anchor: bool,
    pub can_probe_market: bool,
}

impl MarketRole {
    #[must_use]
    pub const fn capabilities(self) -> RoleCapabilities {
        match self {
            Self::Anchor => RoleCapabilities {
                can_start_route: true,
                can_end_route: true,
                can_bridge_route: true,
                can_price_anchor: true,
                can_probe_market: true,
            },
            Self::Target => RoleCapabilities {
                can_start_route: true,
                can_end_route: true,
                can_bridge_route: false,
                can_price_anchor: false,
                can_probe_market: true,
            },
            Self::Bridge => RoleCapabilities {
                can_start_route: false,
                can_end_route: false,
                can_bridge_route: true,
                can_price_anchor: false,
                can_probe_market: false,
            },
            Self::WatchOnly => RoleCapabilities {
                can_start_route: false,
                can_end_route: false,
                can_bridge_route: false,
                can_price_anchor: false,
                can_probe_market: false,
            },
        }
    }
}

/// The active policy for one league.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPolicy {
    pub league: String,
    /// Currencies treated as money, in preference order.
    pub core_liquidity: Vec<MarketAssetId>,
    pub roles: BTreeMap<String, MarketRole>,
    /// Whether route search may explore links between two targets rather
    /// than only target-to-core.
    pub explore_target_interconnect: bool,
    /// False while the core set is still the shipped default.
    pub user_configured: bool,
}

impl MarketPolicy {
    /// The shipped default for a league.
    #[must_use]
    pub fn default_for(league: &str) -> Self {
        let core_liquidity = DEFAULT_CORE_LIQUIDITY
            .iter()
            .filter_map(|id| MarketAssetId::try_new(*id).ok())
            .collect::<Vec<_>>();
        let roles = core_liquidity
            .iter()
            .map(|id| (id.as_str().to_owned(), MarketRole::Anchor))
            .collect();
        Self {
            league: league.trim().to_owned(),
            core_liquidity,
            roles,
            explore_target_interconnect: false,
            user_configured: false,
        }
    }

    /// The role of a currency; anything unlisted is a target, since the user
    /// is looking at it for a reason.
    #[must_use]
    pub fn role(&self, asset_id: &MarketAssetId) -> MarketRole {
        self.roles
            .get(asset_id.as_str())
            .copied()
            .unwrap_or(MarketRole::Target)
    }

    #[must_use]
    pub fn is_core_liquidity(&self, asset_id: &MarketAssetId) -> bool {
        self.core_liquidity.contains(asset_id)
    }

    /// Replace the core set, dropping duplicates and keeping order. An empty
    /// list is rejected rather than silently leaving the market with no
    /// money.
    pub fn set_core_liquidity(&mut self, assets: impl IntoIterator<Item = MarketAssetId>) -> bool {
        let mut seen = BTreeSet::new();
        let core: Vec<MarketAssetId> = assets
            .into_iter()
            .filter(|asset| seen.insert(asset.as_str().to_owned()))
            .collect();
        if core.is_empty() {
            return false;
        }
        for asset in &self.core_liquidity {
            self.roles.remove(asset.as_str());
        }
        for asset in &core {
            self.roles
                .insert(asset.as_str().to_owned(), MarketRole::Anchor);
        }
        self.core_liquidity = core;
        self.user_configured = true;
        true
    }
}

/// What the data suggests doing about a currency's liquidity role.
/// Ordered so that a promotion outranks a watch when results are sorted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorAction {
    /// Well enough connected to serve as money.
    PromoteToCore,
    /// Promising, but not yet enough evidence.
    Watch,
}

/// Why a currency scored the way it did.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorEvidence {
    RecentQuotes,
    LimitedQuotes,
    PairCoverage,
    LimitedPairCoverage,
    BidirectionalCoverage,
    LimitedBidirectionalCoverage,
    AvailableDepth,
    NoAvailableDepth,
}

/// A suggestion about one currency's liquidity role.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRecommendation {
    pub asset_id: MarketAssetId,
    pub action: AnchorAction,
    /// Score in tenths of a point, out of 1000. Integer so that the sort is
    /// reproducible.
    pub score_tenths: u32,
    pub recent_quote_count: u32,
    pub fresh_quote_count: u32,
    pub pair_coverage_count: u32,
    pub bidirectional_pair_count: u32,
    pub available_stock_depth: u64,
    pub evidence: Vec<AnchorEvidence>,
}

#[derive(Default)]
struct AnchorStats {
    recent: u32,
    fresh: u32,
    peers: BTreeSet<String>,
    outgoing: BTreeSet<String>,
    incoming: BTreeSet<String>,
    depth: u64,
}

/// Thresholds a currency must clear to be recommended as core liquidity.
const PROMOTE_MIN_QUOTES: u32 = 6;
const PROMOTE_MIN_PAIRS: u32 = 3;
const PROMOTE_MIN_BIDIRECTIONAL: u32 = 2;
const PROMOTE_MIN_FRESH: u32 = 3;
const WATCH_MIN_QUOTES: u32 = 3;
const WATCH_MIN_PAIRS: u32 = 2;
const MAX_RECOMMENDATIONS: usize = 12;

/// Suggest currencies that behave like money but are not yet treated as such.
///
/// Only current, corroborated quotes count: a currency that was liquid last
/// week is not liquid now, and promoting it would put stale rates at the
/// centre of every valuation.
#[must_use]
pub fn recommend_liquidity_anchors(
    edges: &[EvaluatedQuoteEdge],
    policy: &MarketPolicy,
) -> Vec<AnchorRecommendation> {
    let mut stats: BTreeMap<String, AnchorStats> = BTreeMap::new();
    for evaluated in edges {
        let edge = &evaluated.observation.edge;
        if edge.from_asset_id == edge.to_asset_id {
            continue;
        }
        if !matches!(
            evaluated.freshness.status,
            FreshnessStatus::Fresh | FreshnessStatus::Usable
        ) {
            continue;
        }
        let from = edge.from_asset_id.as_str().to_owned();
        let to = edge.to_asset_id.as_str().to_owned();
        let is_fresh = evaluated.freshness.status == FreshnessStatus::Fresh;
        // Available-side stock is the only depth you can actually take.
        let depth = if edge.source_side == QuoteSide::Available {
            edge.stock
        } else {
            0
        };

        for (owner, peer, outgoing) in [(&from, &to, true), (&to, &from, false)] {
            let entry = stats.entry(owner.clone()).or_default();
            entry.recent = entry.recent.saturating_add(1);
            if is_fresh {
                entry.fresh = entry.fresh.saturating_add(1);
            }
            entry.peers.insert(peer.clone());
            if outgoing {
                entry.outgoing.insert(peer.clone());
            } else {
                entry.incoming.insert(peer.clone());
            }
            entry.depth = entry.depth.saturating_add(depth);
        }
    }

    let mut recommendations: Vec<AnchorRecommendation> = stats
        .into_iter()
        .filter_map(|(asset_id, item)| {
            let asset_id = MarketAssetId::try_new(asset_id).ok()?;
            if policy.is_core_liquidity(&asset_id) {
                return None;
            }
            let pairs = u32::try_from(item.peers.len()).unwrap_or(u32::MAX);
            let bidirectional = u32::try_from(
                item.peers
                    .iter()
                    .filter(|peer| item.outgoing.contains(*peer) && item.incoming.contains(*peer))
                    .count(),
            )
            .unwrap_or(u32::MAX);

            let promote = item.recent >= PROMOTE_MIN_QUOTES
                && pairs >= PROMOTE_MIN_PAIRS
                && bidirectional >= PROMOTE_MIN_BIDIRECTIONAL
                && item.fresh >= PROMOTE_MIN_FRESH
                && item.depth > 0;
            let watch = item.recent >= WATCH_MIN_QUOTES && pairs >= WATCH_MIN_PAIRS;
            let action = if promote {
                AnchorAction::PromoteToCore
            } else if watch {
                AnchorAction::Watch
            } else {
                return None;
            };

            Some(AnchorRecommendation {
                asset_id,
                action,
                score_tenths: score_tenths(&item, pairs, bidirectional),
                recent_quote_count: item.recent,
                fresh_quote_count: item.fresh,
                pair_coverage_count: pairs,
                bidirectional_pair_count: bidirectional,
                available_stock_depth: item.depth,
                evidence: evidence(&item, pairs, bidirectional),
            })
        })
        .collect();

    recommendations.sort_by(|left, right| {
        left.action
            .cmp(&right.action)
            .then_with(|| right.score_tenths.cmp(&left.score_tenths))
            .then_with(|| left.asset_id.as_str().cmp(right.asset_id.as_str()))
    });
    recommendations.truncate(MAX_RECOMMENDATIONS);
    recommendations
}

fn score_tenths(item: &AnchorStats, pairs: u32, bidirectional: u32) -> u32 {
    // Same five components as the JS, in tenths of a point: quote volume,
    // breadth, two-way coverage, depth, and how much of it is fresh.
    let quotes = (item.recent.saturating_mul(30)).min(300);
    let coverage = (pairs.saturating_mul(50)).min(250);
    let two_way = (bidirectional.saturating_mul(80)).min(250);
    let depth = item
        .depth
        .saturating_add(1)
        .ilog10()
        .saturating_mul(40)
        .min(100);
    let freshness = if item.recent == 0 {
        0
    } else {
        (item.fresh.saturating_mul(100) / item.recent).min(100)
    };
    quotes + coverage + two_way + depth + freshness
}

fn evidence(item: &AnchorStats, pairs: u32, bidirectional: u32) -> Vec<AnchorEvidence> {
    vec![
        if item.recent >= PROMOTE_MIN_QUOTES {
            AnchorEvidence::RecentQuotes
        } else {
            AnchorEvidence::LimitedQuotes
        },
        if pairs >= PROMOTE_MIN_PAIRS {
            AnchorEvidence::PairCoverage
        } else {
            AnchorEvidence::LimitedPairCoverage
        },
        if bidirectional >= PROMOTE_MIN_BIDIRECTIONAL {
            AnchorEvidence::BidirectionalCoverage
        } else {
            AnchorEvidence::LimitedBidirectionalCoverage
        },
        if item.depth > 0 {
            AnchorEvidence::AvailableDepth
        } else {
            AnchorEvidence::NoAvailableDepth
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_a_currency_is_not_listed_under_is_a_target_not_a_dead_end() {
        let policy = MarketPolicy::default_for("rise-of-the-abyssal");
        let unknown = MarketAssetId::try_new("annulment-orb").expect("asset");
        assert_eq!(policy.role(&unknown), MarketRole::Target);
        assert!(policy.role(&unknown).capabilities().can_start_route);
    }

    #[test]
    fn core_liquidity_can_do_everything_and_watch_only_can_do_nothing() {
        let anchor = MarketRole::Anchor.capabilities();
        assert!(
            anchor.can_start_route
                && anchor.can_end_route
                && anchor.can_bridge_route
                && anchor.can_price_anchor
                && anchor.can_probe_market
        );
        let watch = MarketRole::WatchOnly.capabilities();
        assert!(
            !watch.can_start_route
                && !watch.can_end_route
                && !watch.can_bridge_route
                && !watch.can_price_anchor
                && !watch.can_probe_market
        );
    }

    #[test]
    fn an_empty_core_set_is_refused() {
        let mut policy = MarketPolicy::default_for("standard");
        assert!(!policy.set_core_liquidity(Vec::new()));
        assert_eq!(policy.core_liquidity.len(), 3);
        assert!(!policy.user_configured);
    }

    #[test]
    fn replacing_the_core_set_drops_duplicates_and_moves_the_anchor_roles() {
        let mut policy = MarketPolicy::default_for("standard");
        let annul = MarketAssetId::try_new("annulment-orb").expect("asset");
        assert!(policy.set_core_liquidity(vec![annul.clone(), annul.clone()]));
        assert_eq!(policy.core_liquidity, vec![annul.clone()]);
        assert_eq!(policy.role(&annul), MarketRole::Anchor);
        let divine = MarketAssetId::try_new("divine-orb").expect("asset");
        assert_eq!(
            policy.role(&divine),
            MarketRole::Target,
            "a demoted anchor becomes an ordinary target, not a leftover anchor"
        );
        assert!(policy.user_configured);
    }
}
