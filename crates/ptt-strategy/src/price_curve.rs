//! Price history for one pair: candles, a summary, and what looks wrong.
//!
//! Ported from `priceCurve.ts`. Three departures.
//!
//! The audit's B-15: a capture timestamp in the future was clamped to age
//! zero, so a wrong clock read as freshly captured and stayed "fresh" until
//! reality caught up. Points here carry the freshness the book assessed,
//! including its future-timestamp finding, and a future point raises an
//! anomaly rather than passing as the newest price.
//!
//! The mean rate is gone. Averaging quoted ratios has no exact rational form
//! at any useful scale, and every consumer — the anomaly detector included —
//! wanted the median anyway, which is exact because it selects a value rather
//! than computing one.
//!
//! Candles are built from taker points when the bucket has any. A bucket
//! holding only maker listings describes what people are asking, not what
//! anything traded at, and the two are not the same series.

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use ptt_market_book::{EvaluatedQuoteEdge, FreshnessStatus};
use ptt_trade_domain::{ExecutionType, MarketAssetId, Ratio};
use serde::{Deserialize, Serialize};

use crate::units::{basis_points, rational_from_ratio};

/// Candle width.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketSize {
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneHour,
}

impl BucketSize {
    #[must_use]
    pub const fn seconds(self) -> i64 {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 5 * 60,
            Self::FifteenMinutes => 15 * 60,
            Self::OneHour => 60 * 60,
        }
    }

    fn delta(self) -> TimeDelta {
        TimeDelta::seconds(self.seconds())
    }
}

/// One observed rate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricePoint {
    pub captured_at: DateTime<Utc>,
    pub snapshot_id: String,
    pub quote_edge_id: String,
    pub rate: Ratio,
    pub execution_type: ExecutionType,
    pub stock: u64,
    pub freshness: FreshnessStatus,
    /// The book found this timestamp ahead of the clock.
    pub future_timestamp: bool,
}

/// One candle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceCandle {
    pub bucket_start: DateTime<Utc>,
    pub bucket_size: BucketSize,
    pub open: Ratio,
    pub high: Ratio,
    pub low: Ratio,
    pub close: Ratio,
    pub sample_count: u32,
    pub taker_count: u32,
    pub maker_count: u32,
    pub total_visible_stock: u64,
    /// The worst freshness in the bucket: a candle is only as current as its
    /// stalest input.
    pub freshness: FreshnessStatus,
    /// True when the bucket held no taker prices, so the candle is built from
    /// listings rather than trades.
    pub maker_only: bool,
}

/// Summary statistics over a series.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceSummary {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub point_count: u32,
    pub snapshot_count: u32,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub latest_rate: Option<Ratio>,
    pub latest_taker_rate: Option<Ratio>,
    pub latest_maker_rate: Option<Ratio>,
    pub median_rate: Option<Ratio>,
    pub min_rate: Option<Ratio>,
    pub max_rate: Option<Ratio>,
    /// The maker/taker spread on the latest points, in basis points.
    pub spread_basis_points: Option<i64>,
    /// Full range as a fraction of the median, in basis points.
    pub range_basis_points: Option<i64>,
    pub fresh_point_count: u32,
    pub usable_point_count: u32,
    pub stale_point_count: u32,
    pub archived_point_count: u32,
    /// True when nothing in the series is current: history, not a price.
    pub historical_only: bool,
}

/// Something about the series that deserves a second look.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceAnomalyKind {
    RateSpike,
    RateDrop,
    SpreadWidened,
    ThinLiquidity,
    StaleLatest,
    /// B-15: a capture timestamp ahead of the clock.
    ClockSkewFuture,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalySeverity {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceAnomaly {
    pub kind: PriceAnomalyKind,
    pub severity: AnomalySeverity,
    /// The deviation that triggered it, in basis points, when it has one.
    pub basis_points: Option<i64>,
}

/// The bars anomaly detection judges against. Previously five hardcoded
/// constants while every other thin-liquidity site read settings — the same
/// number should mean the same thing everywhere, so these now arrive from
/// the caller with the shipped values as the default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnomalyThresholds {
    /// A move of this size against the median reads as a spike or a drop.
    pub spike_basis_points: i64,
    /// ...and this much is high severity.
    pub severe_spike_basis_points: i64,
    /// A maker/taker spread wider than this is worth explaining.
    pub wide_spread_basis_points: i64,
    pub severe_spread_basis_points: i64,
    /// Stock below this on the newest point is thin.
    pub thin_stock: u64,
}

impl Default for AnomalyThresholds {
    fn default() -> Self {
        Self {
            spike_basis_points: 2_000,
            severe_spike_basis_points: 5_000,
            wide_spread_basis_points: 1_000,
            severe_spread_basis_points: 2_000,
            // Unified with RiskThresholds' global bar (was a divergent 10).
            thin_stock: 100,
        }
    }
}

/// Turn selected edges for one direction into a time-ordered series.
///
/// Edges for the other direction are dropped rather than inverted: an
/// inverted rate is a different observation with different depth behind it,
/// and mixing the two produces a series that is not a price of anything.
#[must_use]
pub fn price_points(
    edges: &[EvaluatedQuoteEdge],
    from_asset_id: &MarketAssetId,
    to_asset_id: &MarketAssetId,
) -> Vec<PricePoint> {
    let mut points: Vec<PricePoint> = edges
        .iter()
        .filter(|evaluated| {
            let edge = &evaluated.observation.edge;
            edge.from_asset_id == *from_asset_id && edge.to_asset_id == *to_asset_id
        })
        .map(|evaluated| {
            let edge = &evaluated.observation.edge;
            PricePoint {
                captured_at: edge.captured_at,
                snapshot_id: edge.snapshot_id.clone(),
                quote_edge_id: edge.edge_id.clone(),
                rate: edge.rate.clone(),
                execution_type: edge.execution_type,
                stock: edge.stock,
                freshness: evaluated.freshness.status,
                future_timestamp: evaluated.freshness.future_timestamp,
            }
        })
        .collect();
    points.sort_by(|left, right| {
        left.captured_at
            .cmp(&right.captured_at)
            .then_with(|| left.quote_edge_id.cmp(&right.quote_edge_id))
    });
    points
}

/// The staler of two statuses.
///
/// `FreshnessStatus` derives `Ord` in declaration order — Fresh, Usable,
/// Stale, Archived — so this is the book's ordering rather than a second copy
/// of it that a newly inserted status could silently contradict.
fn worst_freshness(left: FreshnessStatus, right: FreshnessStatus) -> FreshnessStatus {
    left.max(right)
}

/// Group a series into candles.
#[must_use]
pub fn candles(points: &[PricePoint], size: BucketSize) -> Vec<PriceCandle> {
    let mut buckets: Vec<(DateTime<Utc>, Vec<&PricePoint>)> = Vec::new();
    for point in points {
        let Ok(start) = point.captured_at.duration_trunc(size.delta()) else {
            continue;
        };
        match buckets.last_mut() {
            Some((existing, group)) if *existing == start => group.push(point),
            _ => buckets.push((start, vec![point])),
        }
    }

    buckets
        .into_iter()
        .filter_map(|(bucket_start, group)| {
            let taker: Vec<&PricePoint> = group
                .iter()
                .copied()
                .filter(|point| point.execution_type == ExecutionType::Taker)
                .collect();
            let maker_only = taker.is_empty();
            // Prices come from trades when the bucket has them, listings only
            // as a fallback — and the candle says which.
            let priced: Vec<&PricePoint> = if maker_only { group.to_vec() } else { taker };
            let open = priced.first()?.rate.clone();
            let close = priced.last()?.rate.clone();
            let mut high = open.clone();
            let mut low = open.clone();
            for point in &priced {
                if point.rate.compare_value(&high) == std::cmp::Ordering::Greater {
                    high = point.rate.clone();
                }
                if point.rate.compare_value(&low) == std::cmp::Ordering::Less {
                    low = point.rate.clone();
                }
            }
            let mut freshness = FreshnessStatus::Fresh;
            let mut taker_count = 0_u32;
            let mut maker_count = 0_u32;
            let mut stock = 0_u64;
            for point in &group {
                freshness = worst_freshness(freshness, point.freshness);
                stock = stock.saturating_add(point.stock);
                match point.execution_type {
                    ExecutionType::Taker => taker_count = taker_count.saturating_add(1),
                    ExecutionType::MakerReference => maker_count = maker_count.saturating_add(1),
                }
            }
            Some(PriceCandle {
                bucket_start,
                bucket_size: size,
                open,
                high,
                low,
                close,
                sample_count: u32::try_from(group.len()).unwrap_or(u32::MAX),
                taker_count,
                maker_count,
                total_visible_stock: stock,
                freshness,
                maker_only,
            })
        })
        .collect()
}

fn median_rate(points: &[PricePoint]) -> Option<Ratio> {
    let mut rates: Vec<&Ratio> = points.iter().map(|point| &point.rate).collect();
    if rates.is_empty() {
        return None;
    }
    rates.sort_by(|left, right| left.compare_value(right));
    // The lower of the two middles for an even count: selecting a real
    // observation keeps the value exact, where averaging the pair would not.
    Some(rates[(rates.len() - 1) / 2].clone())
}

/// Summarise a series.
#[must_use]
pub fn summarize(
    points: &[PricePoint],
    from_asset_id: &MarketAssetId,
    to_asset_id: &MarketAssetId,
) -> PriceSummary {
    let mut snapshots: Vec<&str> = points
        .iter()
        .map(|point| point.snapshot_id.as_str())
        .collect();
    snapshots.sort_unstable();
    snapshots.dedup();

    let latest_taker_rate = points
        .iter()
        .rev()
        .find(|point| point.execution_type == ExecutionType::Taker)
        .map(|point| point.rate.clone());
    let latest_maker_rate = points
        .iter()
        .rev()
        .find(|point| point.execution_type == ExecutionType::MakerReference)
        .map(|point| point.rate.clone());
    let spread_basis_points = latest_taker_rate
        .as_ref()
        .zip(latest_maker_rate.as_ref())
        .and_then(|(taker, maker)| {
            basis_points(rational_from_ratio(maker)?, rational_from_ratio(taker)?)
        });

    let median = median_rate(points);
    let min = points
        .iter()
        .map(|point| &point.rate)
        .min_by(|left, right| left.compare_value(right))
        .cloned();
    let max = points
        .iter()
        .map(|point| &point.rate)
        .max_by(|left, right| left.compare_value(right))
        .cloned();
    // Range as a share of the median, via two comparisons against the same
    // baseline rather than a subtraction of rationals.
    let range_basis_points = match (&min, &max, &median) {
        (Some(min), Some(max), Some(median)) => {
            let baseline = rational_from_ratio(median);
            let high =
                baseline.and_then(|baseline| basis_points(rational_from_ratio(max)?, baseline));
            let low =
                baseline.and_then(|baseline| basis_points(rational_from_ratio(min)?, baseline));
            high.zip(low).map(|(high, low)| high - low)
        }
        _ => None,
    };

    let count_of = |status: FreshnessStatus| {
        u32::try_from(
            points
                .iter()
                .filter(|point| point.freshness == status)
                .count(),
        )
        .unwrap_or(u32::MAX)
    };
    let fresh = count_of(FreshnessStatus::Fresh);
    let usable = count_of(FreshnessStatus::Usable);

    PriceSummary {
        from_asset_id: from_asset_id.clone(),
        to_asset_id: to_asset_id.clone(),
        point_count: u32::try_from(points.len()).unwrap_or(u32::MAX),
        snapshot_count: u32::try_from(snapshots.len()).unwrap_or(u32::MAX),
        first_seen_at: points.first().map(|point| point.captured_at),
        last_seen_at: points.last().map(|point| point.captured_at),
        latest_rate: points.last().map(|point| point.rate.clone()),
        latest_taker_rate,
        latest_maker_rate,
        median_rate: median,
        min_rate: min,
        max_rate: max,
        spread_basis_points,
        range_basis_points,
        fresh_point_count: fresh,
        usable_point_count: usable,
        stale_point_count: count_of(FreshnessStatus::Stale),
        archived_point_count: count_of(FreshnessStatus::Archived),
        historical_only: !points.is_empty() && fresh == 0 && usable == 0,
    }
}

/// Find what looks wrong with the newest end of the series.
#[must_use]
pub fn anomalies(
    summary: &PriceSummary,
    points: &[PricePoint],
    thresholds: &AnomalyThresholds,
) -> Vec<PriceAnomaly> {
    let mut found = Vec::new();
    let Some(latest) = points.last() else {
        return found;
    };

    if points.iter().any(|point| point.future_timestamp) {
        // B-15: the JS treated a future capture as age zero, so a wrong clock
        // looked like the freshest possible data.
        found.push(PriceAnomaly {
            kind: PriceAnomalyKind::ClockSkewFuture,
            severity: AnomalySeverity::High,
            basis_points: None,
        });
    }

    if let Some(median) = &summary.median_rate
        && let (Some(latest_rate), Some(median_rate)) = (
            rational_from_ratio(&latest.rate),
            rational_from_ratio(median),
        )
        && let Some(deviation) = basis_points(latest_rate, median_rate)
    {
        if deviation >= thresholds.spike_basis_points {
            found.push(PriceAnomaly {
                kind: PriceAnomalyKind::RateSpike,
                severity: if deviation >= thresholds.severe_spike_basis_points {
                    AnomalySeverity::High
                } else {
                    AnomalySeverity::Medium
                },
                basis_points: Some(deviation),
            });
        } else if deviation <= -thresholds.spike_basis_points {
            found.push(PriceAnomaly {
                kind: PriceAnomalyKind::RateDrop,
                severity: if deviation <= -thresholds.severe_spike_basis_points {
                    AnomalySeverity::High
                } else {
                    AnomalySeverity::Medium
                },
                basis_points: Some(deviation),
            });
        }
    }

    if let Some(spread) = summary.spread_basis_points
        && spread > thresholds.wide_spread_basis_points
    {
        found.push(PriceAnomaly {
            kind: PriceAnomalyKind::SpreadWidened,
            severity: if spread > thresholds.severe_spread_basis_points {
                AnomalySeverity::High
            } else {
                AnomalySeverity::Medium
            },
            basis_points: Some(spread),
        });
    }

    if latest.stock > 0 && latest.stock < thresholds.thin_stock {
        found.push(PriceAnomaly {
            kind: PriceAnomalyKind::ThinLiquidity,
            severity: AnomalySeverity::Low,
            basis_points: None,
        });
    }

    if matches!(
        latest.freshness,
        FreshnessStatus::Stale | FreshnessStatus::Archived
    ) {
        found.push(PriceAnomaly {
            kind: PriceAnomalyKind::StaleLatest,
            severity: if latest.freshness == FreshnessStatus::Archived {
                AnomalySeverity::High
            } else {
                AnomalySeverity::Medium
            },
            basis_points: None,
        });
    }

    found
}
