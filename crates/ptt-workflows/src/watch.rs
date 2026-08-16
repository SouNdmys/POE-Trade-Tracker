use chrono::{DateTime, Utc};
use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchPriority {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchHealth {
    Disabled,
    Missing,
    Fresh,
    DueSoon,
    Stale,
    FutureTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WatchTarget {
    Asset {
        asset_id: MarketAssetId,
    },
    Pair {
        from_asset_id: MarketAssetId,
        to_asset_id: MarketAssetId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchItemDraft {
    pub target: WatchTarget,
    pub priority: WatchPriority,
    pub enabled: bool,
    pub note: Option<String>,
    pub stale_after_minutes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchItem {
    pub watch_item_id: String,
    pub context_key: String,
    pub target: WatchTarget,
    pub priority: WatchPriority,
    pub enabled: bool,
    pub note: Option<String>,
    pub stale_after_minutes: u32,
    pub revision: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchItemStatus {
    pub item: WatchItem,
    pub health: WatchHealth,
    pub last_observed_at: Option<DateTime<Utc>>,
}

#[must_use]
pub fn evaluate_watch_health(
    enabled: bool,
    last_observed_at: Option<DateTime<Utc>>,
    stale_after_minutes: u32,
    now: DateTime<Utc>,
) -> WatchHealth {
    if !enabled {
        return WatchHealth::Disabled;
    }
    let Some(last_observed_at) = last_observed_at else {
        return WatchHealth::Missing;
    };
    let age_seconds = now.signed_duration_since(last_observed_at).num_seconds();
    if age_seconds < 0 {
        return WatchHealth::FutureTimestamp;
    }
    let stale_seconds = i64::from(stale_after_minutes.max(1)) * 60;
    if age_seconds > stale_seconds {
        WatchHealth::Stale
    } else if age_seconds * 4 > stale_seconds * 3 {
        WatchHealth::DueSoon
    } else {
        WatchHealth::Fresh
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    #[test]
    fn watch_health_has_explicit_missing_due_stale_and_future_states() {
        let now = Utc
            .with_ymd_and_hms(2026, 7, 30, 12, 0, 0)
            .single()
            .expect("time");
        assert_eq!(
            evaluate_watch_health(true, None, 60, now),
            WatchHealth::Missing
        );
        assert_eq!(
            evaluate_watch_health(true, Some(now - Duration::minutes(50)), 60, now),
            WatchHealth::DueSoon
        );
        assert_eq!(
            evaluate_watch_health(true, Some(now - Duration::minutes(61)), 60, now),
            WatchHealth::Stale
        );
        assert_eq!(
            evaluate_watch_health(true, Some(now + Duration::seconds(1)), 60, now),
            WatchHealth::FutureTimestamp
        );
    }
}
