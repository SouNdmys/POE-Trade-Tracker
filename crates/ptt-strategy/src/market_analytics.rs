//! Market analytics: currency value via settlement cross-rates, supply and
//! demand pressure from the two listing sides, anchor-drift decomposition,
//! and the scarce-vs-oversupplied discrimination that tells a mirror from
//! junk.
//!
//! Three market facts drive the design (all user-adjudicated):
//! - Listing quantities are the only valid evidence of circulation and buy
//!   pressure; capture frequency proves nothing but attention.
//! - The settlement currency itself drifts. When most of the market "rises"
//!   against the anchor, that is the anchor falling — an asset's verdict must
//!   be its move relative to the market median, not relative to the anchor.
//! - Thin books split two ways: scarce (demand ≫ supply — a mirror) and
//!   oversupplied (supply ≫ demand — junk nobody wants). The imbalance
//!   direction, not the thinness, is the signal.
//!
//! Everything here is exact integer/rational arithmetic; basis points come
//! from u128 cross-multiplication with documented floor division.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ptt_trade_domain::{MarketAssetId, Ratio};

use crate::StrategyError;
use crate::day_rollup::lower_middle;

/// Moves within ±this band count as flat when reading market breadth.
const FLAT_BAND_BPS: i64 = 200;
/// Fewer trending assets than this cannot carry an anchor-drift verdict.
const MIN_BREADTH_SAMPLE: u32 = 3;
/// High-turnover marking needs at least this many valued assets for the
/// upper quartile to mean anything.
const MIN_QUARTILE_SAMPLE: usize = 4;

/// One (day, book orientation) statistic — the strategy-facing mirror of a
/// stored day rollup, also produced live for the current day. Rates are
/// panel-orientation: need units per one have unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyPairStat {
    pub utc_day: String,
    pub need_asset_id: MarketAssetId,
    pub have_asset_id: MarketAssetId,
    pub snapshot_count: u32,
    pub last_captured_at: DateTime<Utc>,
    pub available_rows: u32,
    pub competing_rows: u32,
    pub available_sum_need_units: u128,
    pub available_sum_have_units: u128,
    pub competing_sum_have_units: u128,
    pub competing_sum_need_units: u128,
    pub top_taker_rate: Option<Ratio>,
}

/// Validated analytics thresholds. First-pass defaults live in settings;
/// consumers build this through [`AnalyticsThresholds::try_new`] so a
/// hand-edited bad value degrades loudly instead of skewing every verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyticsThresholds {
    pub trend_recent_days: u32,
    pub trend_window_days: u32,
    pub breadth_threshold_percent: u32,
    pub verdict_threshold_bps: i64,
    pub scarce_ratio_percent: u32,
    pub quiet_floor_anchor_units: u128,
}

impl AnalyticsThresholds {
    pub fn try_new(
        trend_recent_days: u32,
        trend_window_days: u32,
        breadth_threshold_percent: u32,
        verdict_threshold_bps: i64,
        scarce_ratio_percent: u32,
        quiet_floor_anchor_units: u128,
    ) -> Result<Self, StrategyError> {
        let valid = trend_recent_days >= 1
            && trend_window_days >= 1
            && (1..=100).contains(&breadth_threshold_percent)
            && verdict_threshold_bps > 0
            && scarce_ratio_percent >= 100;
        if !valid {
            return Err(StrategyError::InvalidRequest);
        }
        Ok(Self {
            trend_recent_days,
            trend_window_days,
            breadth_threshold_percent,
            verdict_threshold_bps,
            scarce_ratio_percent,
            quiet_floor_anchor_units,
        })
    }
}

impl Default for AnalyticsThresholds {
    fn default() -> Self {
        Self {
            trend_recent_days: 2,
            trend_window_days: 7,
            breadth_threshold_percent: 70,
            verdict_threshold_bps: 500,
            scarce_ratio_percent: 300,
            quiet_floor_anchor_units: 10,
        }
    }
}

/// An asset's move relative to the market median move — never relative to
/// the raw anchor, which may itself be drifting.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrendVerdict {
    Appreciating,
    Holding,
    Depreciating,
}

/// The mirror-vs-junk discrimination, read from the imbalance direction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LiquidityClass {
    /// Demand ≥ scarce_ratio × supply: undersupplied, speculators queue for
    /// it (the mirror shape).
    Scarce,
    /// Supply ≥ scarce_ratio × demand: nobody wants it (the junk shape).
    Oversupplied,
    Balanced,
    /// Too little on both sides to read a direction at all.
    Quiet,
}

/// Whether the settlement anchor itself is drifting, read from market
/// breadth: `Inflating` means the anchor is losing purchasing power (most
/// asset prices in the anchor are rising) — the "D 贬值" case.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnchorDrift {
    Inflating,
    Deflating,
    Steady,
}

/// One secondary settlement currency measured against the primary anchor —
/// the direct evidence lane beside the breadth decomposition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorCross {
    pub asset_id: MarketAssetId,
    /// Primary-anchor units per one unit of this currency, latest observed.
    pub latest_rate: Ratio,
    pub drift_bps: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorHealth {
    pub anchor_asset_id: MarketAssetId,
    pub drift: AnchorDrift,
    /// Lower-middle median of every trending asset's raw move — the market
    /// baseline that relative verdicts subtract.
    pub market_median_move_bps: Option<i64>,
    pub risers: u32,
    pub fallers: u32,
    pub flat: u32,
    pub crosses: Vec<AnchorCross>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetPulse {
    pub asset_id: MarketAssetId,
    /// Anchor units per one asset unit, latest observed (1:1 for the anchor).
    pub value_in_anchor: Option<Ratio>,
    /// True when the value went through one secondary-anchor composition
    /// step instead of a directly captured pair.
    pub value_is_composed: bool,
    /// Latest-day listed quantities in the asset's own units, attributed by
    /// the TASK-50 payout-side rule (see `day_rollup`).
    pub supply_units: u128,
    pub demand_units: u128,
    /// The same quantities valued in the primary anchor (floor division);
    /// `None` when the asset has no anchor value yet.
    pub supply_anchor: Option<u128>,
    pub demand_anchor: Option<u128>,
    /// Listing rows referencing the asset on its latest day.
    pub listing_rows: u32,
    pub days_observed: u32,
    /// Median daily supply across the observed days — the asset's own norm,
    /// which relative thin-liquidity thresholds are measured against.
    pub circulation_norm_units: Option<u128>,
    /// Move vs the raw anchor over the trend windows.
    pub trend_bps_raw: Option<i64>,
    /// Move vs the market median move — the number verdicts are made of.
    pub trend_bps_relative: Option<i64>,
    pub verdict: Option<TrendVerdict>,
    /// The day-by-day value series behind the trend (anchor per unit),
    /// oldest first — what a detail view draws.
    pub value_by_day: Vec<(String, Ratio)>,
    /// The day-by-day supply attribution behind the circulation norm, in the
    /// asset's own units, oldest first.
    pub supply_by_day: Vec<(String, u128)>,
    pub class: LiquidityClass,
    /// Supply+demand anchor value in the market's upper quartile.
    pub high_turnover: bool,
    /// Scarce and not depreciating relative to the market: the greedy-mode
    /// precondition (undersupplied + drifting up).
    pub greedy_candidate: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarketPulse {
    /// The newest day any statistic covers; every window hangs off it.
    pub as_of_day: Option<String>,
    pub anchor_asset_id: Option<MarketAssetId>,
    pub anchor_health: Option<AnchorHealth>,
    /// Sorted by demand pressure (anchor-valued) descending.
    pub assets: Vec<AssetPulse>,
}

/// The whole analysis: orientation dedup, value series, trends, breadth,
/// attribution, classification. Pure and deterministic over its inputs.
#[must_use]
pub fn market_pulse(
    stats: &[DailyPairStat],
    anchors: &[MarketAssetId],
    thresholds: &AnalyticsThresholds,
) -> MarketPulse {
    let Some(anchor) = anchors.first() else {
        return MarketPulse::default();
    };
    let deduped = dedup_orientations(stats);
    let Some(as_of_day) = deduped
        .iter()
        .filter_map(|stat| parse_day(&stat.utc_day))
        .max()
    else {
        return MarketPulse::default();
    };

    // --- value series: anchor units per asset unit, per day ---
    let direct = value_series_against(&deduped, anchor);
    let mut series: BTreeMap<MarketAssetId, BTreeMap<NaiveDate, Ratio>> = direct;
    let mut composed_assets: BTreeSet<MarketAssetId> = BTreeSet::new();
    for secondary in anchors.iter().skip(1) {
        let via = value_series_against(&deduped, secondary);
        let Some(secondary_in_anchor) = series.get(secondary).cloned() else {
            continue;
        };
        for (asset, days) in via {
            if asset == *anchor || series.contains_key(&asset) {
                continue;
            }
            // One composition step: (anchor per secondary) x (secondary per
            // asset), same-day rates only, exact u128 multiply + reduce.
            let mut composed: BTreeMap<NaiveDate, Ratio> = BTreeMap::new();
            for (day, via_rate) in days {
                if let Some(anchor_rate) = secondary_in_anchor.get(&day)
                    && let Some(rate) = compose(anchor_rate, &via_rate)
                {
                    composed.insert(day, rate);
                }
            }
            if !composed.is_empty() {
                composed_assets.insert(asset.clone());
                series.insert(asset, composed);
            }
        }
    }

    // --- trends over the two calendar windows hanging off as_of_day ---
    let mut raw_trend: BTreeMap<MarketAssetId, i64> = BTreeMap::new();
    for (asset, days) in &series {
        if asset == anchor {
            continue;
        }
        if let Some(bps) = window_trend_bps(days, as_of_day, thresholds) {
            raw_trend.insert(asset.clone(), bps);
        }
    }

    // --- breadth: is the anchor itself drifting? ---
    let market_median_move_bps = lower_middle(raw_trend.values().copied().collect());
    let mut risers = 0u32;
    let mut fallers = 0u32;
    let mut flat = 0u32;
    for bps in raw_trend.values() {
        if *bps > FLAT_BAND_BPS {
            risers += 1;
        } else if *bps < -FLAT_BAND_BPS {
            fallers += 1;
        } else {
            flat += 1;
        }
    }
    let counted = risers + fallers + flat;
    let threshold = u64::from(thresholds.breadth_threshold_percent);
    let drift = if counted >= MIN_BREADTH_SAMPLE
        && u64::from(risers) * 100 >= threshold * u64::from(counted)
    {
        AnchorDrift::Inflating
    } else if counted >= MIN_BREADTH_SAMPLE
        && u64::from(fallers) * 100 >= threshold * u64::from(counted)
    {
        AnchorDrift::Deflating
    } else {
        AnchorDrift::Steady
    };
    let crosses = anchors
        .iter()
        .skip(1)
        .filter_map(|secondary| {
            let days = series.get(secondary)?;
            let (_, latest_rate) = days.iter().next_back()?;
            Some(AnchorCross {
                asset_id: secondary.clone(),
                latest_rate: latest_rate.clone(),
                drift_bps: raw_trend.get(secondary).copied(),
            })
        })
        .collect();
    let anchor_health = AnchorHealth {
        anchor_asset_id: anchor.clone(),
        drift,
        market_median_move_bps,
        risers,
        fallers,
        flat,
        crosses,
    };

    // --- supply/demand attribution (TASK-50 payout-side rule) ---
    // Latest stat per unordered pair, no older than the trend window: stock
    // estimates from a book nobody has opened in weeks would mislead.
    let horizon = as_of_day - chrono::Days::new(u64::from(thresholds.trend_window_days));
    let mut latest_per_pair: BTreeMap<(MarketAssetId, MarketAssetId), &DailyPairStat> =
        BTreeMap::new();
    for stat in &deduped {
        let Some(day) = parse_day(&stat.utc_day) else {
            continue;
        };
        if day < horizon {
            continue;
        }
        let key = unordered(&stat.need_asset_id, &stat.have_asset_id);
        let replace = match latest_per_pair.get(&key) {
            None => true,
            Some(current) => stat.utc_day > current.utc_day,
        };
        if replace {
            latest_per_pair.insert(key, stat);
        }
    }
    let mut supply_units: BTreeMap<MarketAssetId, u128> = BTreeMap::new();
    let mut demand_units: BTreeMap<MarketAssetId, u128> = BTreeMap::new();
    let mut listing_rows: BTreeMap<MarketAssetId, u32> = BTreeMap::new();
    for stat in latest_per_pair.values() {
        attribute(
            stat,
            &mut supply_units,
            &mut demand_units,
            &mut listing_rows,
        );
    }

    // --- per-day supply series for each asset's own circulation norm ---
    let mut daily_supply: BTreeMap<MarketAssetId, BTreeMap<NaiveDate, u128>> = BTreeMap::new();
    for stat in &deduped {
        let Some(day) = parse_day(&stat.utc_day) else {
            continue;
        };
        let mut day_supply: BTreeMap<MarketAssetId, u128> = BTreeMap::new();
        let mut day_demand: BTreeMap<MarketAssetId, u128> = BTreeMap::new();
        let mut day_rows: BTreeMap<MarketAssetId, u32> = BTreeMap::new();
        attribute(stat, &mut day_supply, &mut day_demand, &mut day_rows);
        for (asset, units) in day_supply {
            *daily_supply
                .entry(asset)
                .or_default()
                .entry(day)
                .or_default() += units;
        }
        // Demand-side-only assets still count as observed that day.
        for (asset, _) in day_demand {
            daily_supply
                .entry(asset)
                .or_default()
                .entry(day)
                .or_default();
        }
    }

    // --- assemble per-asset pulses ---
    let mut all_assets: BTreeSet<MarketAssetId> = BTreeSet::new();
    for stat in &deduped {
        all_assets.insert(stat.need_asset_id.clone());
        all_assets.insert(stat.have_asset_id.clone());
    }
    let mut pulses: Vec<AssetPulse> = all_assets
        .into_iter()
        .map(|asset| {
            let value_in_anchor = if asset == *anchor {
                Ratio::from_parts(1, 1).ok()
            } else {
                series
                    .get(&asset)
                    .and_then(|days| days.iter().next_back())
                    .map(|(_, rate)| rate.clone())
            };
            let supply = supply_units.get(&asset).copied().unwrap_or(0);
            let demand = demand_units.get(&asset).copied().unwrap_or(0);
            let supply_anchor = value_in_anchor
                .as_ref()
                .map(|value| anchor_value(supply, value));
            let demand_anchor = value_in_anchor
                .as_ref()
                .map(|value| anchor_value(demand, value));
            let day_series = daily_supply.get(&asset);
            let days_observed = day_series.map_or(0, |days| {
                u32::try_from(days.len()).expect("day count fits u32")
            });
            let circulation_norm_units =
                day_series.and_then(|days| lower_middle(days.values().copied().collect()));
            let trend_bps_raw = raw_trend.get(&asset).copied();
            let trend_bps_relative = match (trend_bps_raw, market_median_move_bps) {
                (Some(raw), Some(median)) => Some(raw.saturating_sub(median)),
                _ => None,
            };
            let verdict = trend_bps_relative.map(|relative| {
                if relative > thresholds.verdict_threshold_bps {
                    TrendVerdict::Appreciating
                } else if relative < -thresholds.verdict_threshold_bps {
                    TrendVerdict::Depreciating
                } else {
                    TrendVerdict::Holding
                }
            });
            let class = classify(supply, demand, supply_anchor, demand_anchor, thresholds);
            let greedy_candidate = class == LiquidityClass::Scarce
                && trend_bps_relative.is_some_and(|relative| relative >= 0);
            let rows = listing_rows.get(&asset).copied().unwrap_or(0);
            let value_by_day = series
                .get(&asset)
                .map(|days| {
                    days.iter()
                        .map(|(day, rate)| (day.format("%Y-%m-%d").to_string(), rate.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let supply_by_day = day_series
                .map(|days| {
                    days.iter()
                        .map(|(day, units)| (day.format("%Y-%m-%d").to_string(), *units))
                        .collect()
                })
                .unwrap_or_default();
            AssetPulse {
                value_is_composed: composed_assets.contains(&asset),
                asset_id: asset,
                value_in_anchor,
                supply_units: supply,
                demand_units: demand,
                supply_anchor,
                demand_anchor,
                listing_rows: rows,
                days_observed,
                circulation_norm_units,
                trend_bps_raw,
                trend_bps_relative,
                verdict,
                value_by_day,
                supply_by_day,
                class,
                high_turnover: false, // filled below, needs the full set
                greedy_candidate,
            }
        })
        .collect();

    // --- high-turnover marking: upper quartile of anchor-valued totals ---
    let mut totals: Vec<u128> = pulses
        .iter()
        .filter_map(|pulse| Some(pulse.supply_anchor? + pulse.demand_anchor?))
        .filter(|total| *total > 0)
        .collect();
    if totals.len() >= MIN_QUARTILE_SAMPLE {
        totals.sort_unstable();
        // Upper quartile by index selection — data-relative, no constant.
        let quartile = totals[(3 * (totals.len() - 1)) / 4];
        for pulse in &mut pulses {
            if let (Some(supply), Some(demand)) = (pulse.supply_anchor, pulse.demand_anchor) {
                pulse.high_turnover = supply + demand >= quartile && supply + demand > 0;
            }
        }
    }

    // Demand pressure first; the page's default reading order.
    pulses.sort_by(|left, right| {
        right
            .demand_anchor
            .unwrap_or(0)
            .cmp(&left.demand_anchor.unwrap_or(0))
            .then_with(|| left.asset_id.cmp(&right.asset_id))
    });

    MarketPulse {
        as_of_day: Some(as_of_day.format("%Y-%m-%d").to_string()),
        anchor_asset_id: Some(anchor.clone()),
        anchor_health: Some(anchor_health),
        assets: pulses,
    }
}

/// Keeps one orientation per (day, unordered pair): the two book directions
/// are the same standing orders seen from both sides, and counting both
/// doubles every quantity. The denser capture wins (more snapshots, then the
/// later capture, then the lexically smaller need id for determinism).
fn dedup_orientations(stats: &[DailyPairStat]) -> Vec<&DailyPairStat> {
    let mut chosen: BTreeMap<(String, MarketAssetId, MarketAssetId), &DailyPairStat> =
        BTreeMap::new();
    for stat in stats {
        let (low, high) = unordered(&stat.need_asset_id, &stat.have_asset_id);
        let key = (stat.utc_day.clone(), low, high);
        let replace = match chosen.get(&key) {
            None => true,
            Some(current) => {
                (
                    stat.snapshot_count,
                    stat.last_captured_at,
                    &current.need_asset_id,
                ) > (
                    current.snapshot_count,
                    current.last_captured_at,
                    &stat.need_asset_id,
                )
            }
        };
        if replace {
            chosen.insert(key, stat);
        }
    }
    chosen.into_values().collect()
}

fn unordered(left: &MarketAssetId, right: &MarketAssetId) -> (MarketAssetId, MarketAssetId) {
    if left <= right {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    }
}

/// Per-asset daily value series against one anchor: anchor units per one
/// asset unit, from directly captured pairs only. The panel-orientation rate
/// is need-per-have, so a book needing the anchor prices the have asset
/// directly and the mirrored book needs one exact inversion.
fn value_series_against(
    deduped: &[&DailyPairStat],
    anchor: &MarketAssetId,
) -> BTreeMap<MarketAssetId, BTreeMap<NaiveDate, Ratio>> {
    let mut series: BTreeMap<MarketAssetId, BTreeMap<NaiveDate, Ratio>> = BTreeMap::new();
    for stat in deduped {
        let Some(rate) = &stat.top_taker_rate else {
            continue;
        };
        let Some(day) = parse_day(&stat.utc_day) else {
            continue;
        };
        let (asset, value) = if stat.need_asset_id == *anchor {
            // rate = anchor per have: the have asset's value directly.
            (stat.have_asset_id.clone(), rate.clone())
        } else if stat.have_asset_id == *anchor {
            // rate = need per anchor: invert exactly.
            (stat.need_asset_id.clone(), rate.inverse())
        } else {
            continue;
        };
        series.entry(asset).or_default().insert(day, value);
    }
    series
}

/// TASK-50 attribution: on a book (need=X, have=Y) the available side pays
/// out X (supplies X, seeks Y) and the competing side pays out Y (supplies
/// Y, seeks X). One book therefore evidences supply AND demand for both
/// assets; the converted sums were built per-row in `day_rollup`.
fn attribute(
    stat: &DailyPairStat,
    supply: &mut BTreeMap<MarketAssetId, u128>,
    demand: &mut BTreeMap<MarketAssetId, u128>,
    rows: &mut BTreeMap<MarketAssetId, u32>,
) {
    let need = &stat.need_asset_id;
    let have = &stat.have_asset_id;
    *supply.entry(need.clone()).or_default() += stat.available_sum_need_units;
    *demand.entry(need.clone()).or_default() += stat.competing_sum_need_units;
    *supply.entry(have.clone()).or_default() += stat.competing_sum_have_units;
    *demand.entry(have.clone()).or_default() += stat.available_sum_have_units;
    let row_count = stat.available_rows + stat.competing_rows;
    *rows.entry(need.clone()).or_default() += row_count;
    *rows.entry(have.clone()).or_default() += row_count;
}

/// (anchor per secondary) × (secondary per asset) = anchor per asset.
/// Exact u128 multiply, gcd-reduced; `None` when the reduced ratio cannot
/// fit u64 (the value is then simply not composed rather than approximated).
pub(crate) fn compose(anchor_per_secondary: &Ratio, secondary_per_asset: &Ratio) -> Option<Ratio> {
    let numerator = u128::from(anchor_per_secondary.numerator)
        .checked_mul(u128::from(secondary_per_asset.numerator))?;
    let denominator = u128::from(anchor_per_secondary.denominator)
        .checked_mul(u128::from(secondary_per_asset.denominator))?;
    let divisor = gcd(numerator, denominator);
    let numerator = u64::try_from(numerator / divisor).ok()?;
    let denominator = u64::try_from(denominator / divisor).ok()?;
    Ratio::from_parts(numerator, denominator).ok()
}

const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left == 0 { 1 } else { left }
}

/// Recent-window median vs baseline-window median, in basis points. `None`
/// when the recent window has fewer than two observed days or the baseline
/// none — "insufficient data" is an answer, never a guess.
pub(crate) fn window_trend_bps(
    days: &BTreeMap<NaiveDate, Ratio>,
    as_of_day: NaiveDate,
    thresholds: &AnalyticsThresholds,
) -> Option<i64> {
    let recent_floor = as_of_day - chrono::Days::new(u64::from(thresholds.trend_recent_days) - 1);
    let baseline_floor = recent_floor - chrono::Days::new(u64::from(thresholds.trend_window_days));
    let recent: Vec<Ratio> = days
        .range(recent_floor..)
        .map(|(_, rate)| rate.clone())
        .collect();
    let baseline: Vec<Ratio> = days
        .range(baseline_floor..recent_floor)
        .map(|(_, rate)| rate.clone())
        .collect();
    if recent.len() < 2 || baseline.is_empty() {
        return None;
    }
    let recent_median = median_rate_value(recent)?;
    let baseline_median = median_rate_value(baseline)?;
    Some(bps_between(&baseline_median, &recent_median))
}

fn median_rate_value(mut rates: Vec<Ratio>) -> Option<Ratio> {
    if rates.is_empty() {
        return None;
    }
    rates.sort_by(|left, right| {
        left.compare_value(right)
            .then(left.numerator.cmp(&right.numerator))
            .then(left.denominator.cmp(&right.denominator))
    });
    let index = (rates.len() - 1) / 2;
    Some(rates.swap_remove(index))
}

/// ((to / from) − 1) × 10000, floored via u128 cross-multiplication and
/// saturated at the i64 rails (display/verdict use only; the rails are
/// unreachable for real panel rates).
pub(crate) fn bps_between(from: &Ratio, to: &Ratio) -> i64 {
    let numerator = u128::from(to.numerator) * u128::from(from.denominator);
    let denominator = u128::from(to.denominator) * u128::from(from.numerator);
    let scaled = numerator.saturating_mul(10_000) / denominator.max(1);
    let bps = i128::try_from(scaled).unwrap_or(i128::MAX) - 10_000;
    i64::try_from(bps).unwrap_or(if bps > 0 { i64::MAX } else { i64::MIN })
}

/// units × (anchor per unit), floor division — the one place a quantity
/// becomes an anchor amount.
pub(crate) fn anchor_value(units: u128, value: &Ratio) -> u128 {
    units.saturating_mul(u128::from(value.numerator)) / u128::from(value.denominator).max(1)
}

/// The imbalance reading. Quiet outranks direction: with next to nothing on
/// both sides there is no direction to read. The ratio itself is unit-free
/// (both sides are in the asset's own units), so classification works even
/// before the asset has an anchor value; only the quiet floor needs one.
fn classify(
    supply_units: u128,
    demand_units: u128,
    supply_anchor: Option<u128>,
    demand_anchor: Option<u128>,
    thresholds: &AnalyticsThresholds,
) -> LiquidityClass {
    let quiet = match (supply_anchor, demand_anchor) {
        (Some(supply), Some(demand)) => {
            supply < thresholds.quiet_floor_anchor_units
                && demand < thresholds.quiet_floor_anchor_units
        }
        _ => supply_units == 0 && demand_units == 0,
    };
    if quiet {
        return LiquidityClass::Quiet;
    }
    let ratio = u128::from(thresholds.scarce_ratio_percent);
    if demand_units * 100 >= supply_units.saturating_mul(ratio) && demand_units > 0 {
        return LiquidityClass::Scarce;
    }
    if supply_units * 100 >= demand_units.saturating_mul(ratio) && supply_units > 0 {
        return LiquidityClass::Oversupplied;
    }
    LiquidityClass::Balanced
}

fn parse_day(utc_day: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(utc_day, "%Y-%m-%d").ok()
}
