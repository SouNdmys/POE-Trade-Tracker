//! Daily pair rollups: the per-snapshot fold and per-day medians that turn
//! raw quote edges into the compact history the analytics layer reads.
//!
//! Stock denomination (TASK-50, adjudicated on the live game): the panel
//! records stock on the side the lister pays out. On a book (need=X, have=Y)
//! the available side pays out X — need units — and the competing side pays
//! out Y — have units. This module is the single auditable point for that
//! assumption; every consumer reads these sums, none re-derives the side.
//!
//! Two disciplines hold throughout:
//! - Unit conversions use each row's own quoted rate, never a market rate:
//!   u128 multiply first, then floor division, per row, then sum.
//! - Day values are lower-middle medians across the day's snapshots (the
//!   `price_curve` convention), never sums — listings are stock, not flow,
//!   and the same standing orders recaptured must not count twice. The rate
//!   median is therefore always an actually-observed ratio, never averaged.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ptt_trade_domain::{Comparator, MarketAssetId, MarketEdgeObservation, QuoteEdgeRole, Ratio};

/// One snapshot of one book, folded from its panel-orientation edges.
///
/// Only the panel-orientation roles (`available_taker`,
/// `competing_maker_reference`) are counted: the other two roles are the same
/// panel rows restated in reverse, and counting them would double every sum.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPairFold {
    pub snapshot_id: String,
    pub context_key: String,
    pub captured_at: DateTime<Utc>,
    pub available_rows: u32,
    pub competing_rows: u32,
    /// Σ available-side stock, in need units (what those listers pay out).
    pub available_sum_need_units: u128,
    /// Σ available-side stock converted to have units via each row's rate.
    pub available_sum_have_units: u128,
    /// Σ competing-side stock, in have units (what those listers pay out).
    pub competing_sum_have_units: u128,
    /// Σ competing-side stock converted to need units via each row's rate.
    pub competing_sum_need_units: u128,
    /// Best available-side taker rate by exact cross-multiply comparison.
    /// Aggregate rows (`<`/`>`) are excluded: their boundary rate prices the
    /// whole tail, not the front. `None` when the snapshot had no exact
    /// available row (a maker-only view).
    pub top_taker_rate: Option<Ratio>,
}

/// One (need, have) book's day summary: lower-middle medians over the day's
/// snapshot folds, each metric independently. A rollup row is deliberately
/// not one coherent snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairDayRollup {
    pub need_asset_id: MarketAssetId,
    pub have_asset_id: MarketAssetId,
    pub snapshot_count: u32,
    /// Distinct context keys among the pair's snapshots — merging across
    /// contexts is how history survives the per-release context rotation.
    pub contexts_merged: u32,
    pub first_captured_at: DateTime<Utc>,
    pub last_captured_at: DateTime<Utc>,
    pub median_available_rows: u32,
    pub median_competing_rows: u32,
    pub median_available_sum_need_units: u128,
    pub median_available_sum_have_units: u128,
    pub median_competing_sum_have_units: u128,
    pub median_competing_sum_need_units: u128,
    pub median_top_taker_rate: Option<Ratio>,
}

/// Folds one day's edges (any number of contexts) into per-book day rollups.
///
/// The caller scopes the slice to exactly one UTC day; this function does not
/// re-check timestamps. Snapshots from different context keys land in the
/// same book group because grouping keys on the original need/have assets.
#[must_use]
pub fn build_pair_day_rollups(edges: &[MarketEdgeObservation]) -> Vec<PairDayRollup> {
    let mut books: BTreeMap<(MarketAssetId, MarketAssetId), BTreeMap<String, SnapshotPairFold>> =
        BTreeMap::new();

    for observation in edges {
        let edge = &observation.edge;
        // Panel-orientation roles only; the reverse edges restate the same rows.
        if !matches!(
            edge.role,
            QuoteEdgeRole::AvailableTaker | QuoteEdgeRole::CompetingMakerReference
        ) {
            continue;
        }
        let key = (
            edge.original_need_asset_id.clone(),
            edge.original_have_asset_id.clone(),
        );
        let fold = books
            .entry(key)
            .or_default()
            .entry(edge.snapshot_id.clone())
            .or_insert_with(|| SnapshotPairFold {
                snapshot_id: edge.snapshot_id.clone(),
                context_key: edge.context_key.clone(),
                captured_at: edge.captured_at,
                available_rows: 0,
                competing_rows: 0,
                available_sum_need_units: 0,
                available_sum_have_units: 0,
                competing_sum_have_units: 0,
                competing_sum_need_units: 0,
                top_taker_rate: None,
            });

        // Panel-orientation rate = need per have (to-asset per from-asset).
        let numerator = u128::from(edge.rate.numerator);
        let denominator = u128::from(edge.rate.denominator);
        let stock = u128::from(edge.stock);
        match edge.role {
            QuoteEdgeRole::AvailableTaker => {
                fold.available_rows += 1;
                fold.available_sum_need_units += stock;
                // need -> have: multiply first, floor divide (per row).
                fold.available_sum_have_units += stock * denominator / numerator;
                if edge.comparator == Comparator::Exact {
                    // Max by exact value; equal-value ties resolve to the
                    // smaller (numerator, denominator) so the winner does not
                    // depend on input order.
                    let replace = match &fold.top_taker_rate {
                        None => true,
                        Some(current) => match edge.rate.compare_value(current) {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Less => false,
                            std::cmp::Ordering::Equal => {
                                (edge.rate.numerator, edge.rate.denominator)
                                    < (current.numerator, current.denominator)
                            }
                        },
                    };
                    if replace {
                        fold.top_taker_rate = Some(edge.rate.clone());
                    }
                }
            }
            QuoteEdgeRole::CompetingMakerReference => {
                fold.competing_rows += 1;
                fold.competing_sum_have_units += stock;
                // have -> need: multiply first, floor divide (per row).
                fold.competing_sum_need_units += stock * numerator / denominator;
            }
            _ => unreachable!("filtered above"),
        }
    }

    books
        .into_iter()
        .map(|((need_asset_id, have_asset_id), snapshots)| {
            let folds: Vec<SnapshotPairFold> = snapshots.into_values().collect();
            let contexts: std::collections::BTreeSet<&str> =
                folds.iter().map(|fold| fold.context_key.as_str()).collect();
            let snapshot_count =
                u32::try_from(folds.len()).expect("snapshot count fits u32 by construction");
            let contexts_merged =
                u32::try_from(contexts.len()).expect("context count fits u32 by construction");
            let first_captured_at = folds
                .iter()
                .map(|fold| fold.captured_at)
                .min()
                .expect("group is non-empty");
            let last_captured_at = folds
                .iter()
                .map(|fold| fold.captured_at)
                .max()
                .expect("group is non-empty");
            PairDayRollup {
                need_asset_id,
                have_asset_id,
                snapshot_count,
                contexts_merged,
                first_captured_at,
                last_captured_at,
                median_available_rows: lower_middle(
                    folds.iter().map(|fold| fold.available_rows).collect(),
                )
                .unwrap_or(0),
                median_competing_rows: lower_middle(
                    folds.iter().map(|fold| fold.competing_rows).collect(),
                )
                .unwrap_or(0),
                median_available_sum_need_units: lower_middle(
                    folds
                        .iter()
                        .map(|fold| fold.available_sum_need_units)
                        .collect(),
                )
                .unwrap_or(0),
                median_available_sum_have_units: lower_middle(
                    folds
                        .iter()
                        .map(|fold| fold.available_sum_have_units)
                        .collect(),
                )
                .unwrap_or(0),
                median_competing_sum_have_units: lower_middle(
                    folds
                        .iter()
                        .map(|fold| fold.competing_sum_have_units)
                        .collect(),
                )
                .unwrap_or(0),
                median_competing_sum_need_units: lower_middle(
                    folds
                        .iter()
                        .map(|fold| fold.competing_sum_need_units)
                        .collect(),
                )
                .unwrap_or(0),
                median_top_taker_rate: median_rate(
                    folds
                        .iter()
                        .filter_map(|fold| fold.top_taker_rate.clone())
                        .collect(),
                ),
            }
        })
        .collect()
}

/// Lower-middle median: sort ascending, take index `(len - 1) / 2` — the
/// `price_curve` convention, which stays exact for even counts by selecting
/// an observed value instead of averaging two.
pub(crate) fn lower_middle<T: Ord>(mut values: Vec<T>) -> Option<T> {
    if values.is_empty() {
        return None;
    }
    values.sort();
    let index = (values.len() - 1) / 2;
    Some(values.swap_remove(index))
}

/// Lower-middle median over observed ratios, ordered by exact cross-multiply
/// value with (numerator, denominator) as the deterministic tiebreak.
fn median_rate(mut rates: Vec<Ratio>) -> Option<Ratio> {
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
