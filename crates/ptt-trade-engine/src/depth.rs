use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use ptt_market_book::{
    EvaluatedQuoteEdge, PolicyCalibrationStatus, QuoteRiskFlag, QuoteSelectionPolicy,
    QuoteSelectionResult, QuoteSelectionStrategy,
};
use ptt_trade_domain::{ExecutionType, MarketAssetId, Ratio};
use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::quantity::{AssetAmount, AssetUnit, AssetUnitCatalog, FeePolicy};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRiskFlag {
    ReverseFromCompeting,
    MakerReference,
    FillNotGuaranteed,
    MakerDepthExceeded,
    LiquidityCapped,
    /// The side this fill priced against holds exactly one listing: there is
    /// no second quote to corroborate the price, so the row that a median
    /// band cannot judge is named instead of trusted silently.
    SingleListingBook,
    BelowMinimumOutput,
    CapacityRoundedToUnit,
    UnknownFee,
    PartialRoute,
    ResidualInventory,
    MultiHopMaker,
    SearchTruncated,
    UnverifiedProductPolicy,
    UnverifiedMinimumLots,
    CaptureSkewUnverified,
    CaptureSkewExceeded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillKind {
    TakerDepth,
    MakerTheory,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteLevelFill {
    pub quote_edge_id: String,
    pub snapshot_id: String,
    pub captured_at: DateTime<Utc>,
    pub rate: Ratio,
    pub amount_in: AssetAmount,
    pub gross_amount_out: AssetAmount,
    pub net_amount_out: Option<AssetAmount>,
    pub capacity_from: AssetAmount,
    pub stock: u64,
    pub quote_risk_flags: Vec<QuoteRiskFlag>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureTimeEvidence {
    pub earliest_captured_at: DateTime<Utc>,
    pub latest_captured_at: DateTime<Utc>,
    pub capture_skew_seconds: u64,
    pub max_capture_skew_seconds: Option<u64>,
    pub calibration_status: PolicyCalibrationStatus,
    pub exceeds_max_capture_skew: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairBottleneck {
    pub asset_id: MarketAssetId,
    pub max_consumable_input: AssetAmount,
    pub max_gross_output: AssetAmount,
    pub reason: ExecutionRiskFlag,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairFill {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub kind: FillKind,
    pub requested_input: AssetAmount,
    pub consumed_input: AssetAmount,
    pub unfilled_input: AssetAmount,
    pub gross_amount_out: AssetAmount,
    pub net_amount_out: Option<AssetAmount>,
    pub fills: Vec<QuoteLevelFill>,
    pub capture_time_evidence: Option<CaptureTimeEvidence>,
    pub is_fully_filled: bool,
    pub execution_eligible: bool,
    pub bottleneck: Option<PairBottleneck>,
    pub risk_flags: Vec<ExecutionRiskFlag>,
}

#[derive(Clone, Debug)]
struct PairDepth {
    primary: Option<EvaluatedQuoteEdge>,
    levels: Vec<EvaluatedQuoteEdge>,
}

#[derive(Clone, Debug)]
pub struct MarketDepthIndex {
    context_key: String,
    strategy: QuoteSelectionStrategy,
    units: AssetUnitCatalog,
    analysis_policy: QuoteSelectionPolicy,
    pairs: BTreeMap<(MarketAssetId, MarketAssetId), PairDepth>,
}

impl MarketDepthIndex {
    pub fn try_from_selection(
        selection: &QuoteSelectionResult,
        units: AssetUnitCatalog,
    ) -> Result<Self, EngineError> {
        if matches!(
            selection.policy.strategy,
            QuoteSelectionStrategy::Probe | QuoteSelectionStrategy::Historical
        ) || selection.context_key.trim().is_empty()
        {
            return Err(EngineError::InvalidQuoteSelection);
        }
        selection
            .policy
            .validate()
            .map_err(|_| EngineError::InvalidQuoteSelection)?;
        let mut pairs = BTreeMap::new();
        let mut edge_ids = BTreeSet::new();
        for selected in &selection.selections {
            if selected.strategy != selection.policy.strategy
                || selected.from_asset_id == selected.to_asset_id
                || !units.contains(&selected.from_asset_id)
                || !units.contains(&selected.to_asset_id)
            {
                return Err(EngineError::InvalidQuoteSelection);
            }
            let levels = selected
                .candidate_edges
                .iter()
                .filter(|candidate| {
                    candidate.accepted_for_selection
                        && candidate.eligible_for_depth_analysis
                        && candidate.observation.edge.context_key == selection.context_key
                })
                .cloned()
                .collect::<Vec<_>>();
            for level in &levels {
                if !edge_ids.insert(level.observation.edge.edge_id.clone()) {
                    return Err(EngineError::InvalidQuoteSelection);
                }
            }
            let primary = selected
                .selected_edge
                .as_ref()
                .filter(|candidate| {
                    candidate.accepted_for_selection
                        && candidate.eligible_for_depth_analysis
                        && candidate.observation.edge.context_key == selection.context_key
                })
                .cloned();
            if !levels.is_empty() {
                pairs.insert(
                    (selected.from_asset_id.clone(), selected.to_asset_id.clone()),
                    PairDepth { primary, levels },
                );
            }
        }
        Ok(Self {
            context_key: selection.context_key.clone(),
            strategy: selection.policy.strategy,
            units,
            analysis_policy: selection.policy.clone(),
            pairs,
        })
    }

    #[must_use]
    pub fn context_key(&self) -> &str {
        &self.context_key
    }

    #[must_use]
    pub const fn strategy(&self) -> QuoteSelectionStrategy {
        self.strategy
    }

    #[must_use]
    pub fn units(&self) -> &AssetUnitCatalog {
        &self.units
    }

    #[must_use]
    pub fn analysis_policy(&self) -> &QuoteSelectionPolicy {
        &self.analysis_policy
    }

    #[must_use]
    pub const fn effective_fee_policy(&self, requested: FeePolicy) -> FeePolicy {
        if self.analysis_policy.cost_verification.all_verified() {
            requested
        } else {
            FeePolicy::Unknown
        }
    }

    #[must_use]
    pub fn contains_pair(&self, from: &MarketAssetId, to: &MarketAssetId) -> bool {
        self.pairs.contains_key(&(from.clone(), to.clone()))
    }

    #[must_use]
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    #[must_use]
    pub fn neighbors(&self, from: &MarketAssetId) -> Vec<MarketAssetId> {
        self.pairs
            .keys()
            .filter(|(candidate, _)| candidate == from)
            .map(|(_, to)| to.clone())
            .collect()
    }

    pub fn fill_pair(
        &self,
        input: &AssetAmount,
        to_asset_id: &MarketAssetId,
        fee_policy: FeePolicy,
    ) -> Result<Option<PairFill>, EngineError> {
        fee_policy.validate()?;
        let fee_policy = self.effective_fee_policy(fee_policy);
        if input.quanta == 0
            || input.unit != self.units.unit(&input.asset_id)?
            || input.asset_id == *to_asset_id
        {
            return Err(EngineError::InvalidAnalysisRequest);
        }
        let Some(depth) = self
            .pairs
            .get(&(input.asset_id.clone(), to_asset_id.clone()))
        else {
            return Ok(None);
        };
        let mut fill = match self.strategy {
            QuoteSelectionStrategy::Instant => self
                .fill_taker_depth(input, to_asset_id, fee_policy, depth)
                .map(Some)?,
            QuoteSelectionStrategy::FastMaker
            | QuoteSelectionStrategy::BalancedMaker
            | QuoteSelectionStrategy::GreedyMaker => self
                .fill_maker_theory(input, to_asset_id, fee_policy, depth)
                .map(Some)?,
            QuoteSelectionStrategy::Probe | QuoteSelectionStrategy::Historical => {
                return Err(EngineError::InvalidAnalysisRequest);
            }
        };
        if let Some(fill) = &mut fill {
            self.apply_product_safety(fill);
        }
        Ok(fill)
    }

    fn apply_product_safety(&self, fill: &mut PairFill) {
        if !self.analysis_policy.product_execution_allowed {
            insert_execution_risk(
                &mut fill.risk_flags,
                ExecutionRiskFlag::UnverifiedProductPolicy,
            );
            fill.execution_eligible = false;
        }
        if !self.analysis_policy.cost_verification.minimum_lots_verified {
            insert_execution_risk(
                &mut fill.risk_flags,
                ExecutionRiskFlag::UnverifiedMinimumLots,
            );
            fill.execution_eligible = false;
        }
    }

    fn fill_taker_depth(
        &self,
        input: &AssetAmount,
        to_asset_id: &MarketAssetId,
        fee_policy: FeePolicy,
        depth: &PairDepth,
    ) -> Result<PairFill, EngineError> {
        let mut levels = depth
            .levels
            .iter()
            .filter(|level| level.observation.edge.execution_type == ExecutionType::Taker)
            .cloned()
            .collect::<Vec<_>>();
        levels.sort_by(compare_taker_levels);

        let mut remaining_quanta = input.quanta;
        let mut consumed_quanta = 0_u64;
        let mut gross_output = AssetAmount::zero(to_asset_id.clone(), &self.units)?;
        let mut net_output = (!fee_policy.is_unknown())
            .then(|| AssetAmount::zero(to_asset_id.clone(), &self.units))
            .transpose()?;
        let mut fills = Vec::new();
        let mut risks = BTreeSet::new();
        if fee_policy.is_unknown() {
            risks.insert(ExecutionRiskFlag::UnknownFee);
        }
        if levels.len() == 1 {
            risks.insert(ExecutionRiskFlag::SingleListingBook);
        }

        for level in levels {
            if remaining_quanta == 0 {
                break;
            }
            let edge = &level.observation.edge;
            let scale = conversion_scale(input.unit, &edge.rate, self.units.unit(to_asset_id)?)?;
            let (capacity_from_quanta, capacity_rounded) = stock_input_capacity(
                STOCK_DENOMINATION,
                edge.stock,
                ExecutionType::Taker,
                input.unit,
                self.units.unit(to_asset_id)?,
                scale,
            )?;
            if capacity_rounded {
                risks.insert(ExecutionRiskFlag::CapacityRoundedToUnit);
            }
            let use_quanta = remaining_quanta.min(capacity_from_quanta);
            if use_quanta == 0 {
                continue;
            }
            let output_quanta = floor_scaled(use_quanta, scale)?;
            if output_quanta == 0 {
                risks.insert(ExecutionRiskFlag::BelowMinimumOutput);
                continue;
            }
            let net_quanta = fee_policy.apply_to_quanta(output_quanta)?;
            remaining_quanta -= use_quanta;
            consumed_quanta = consumed_quanta
                .checked_add(use_quanta)
                .ok_or(EngineError::NumericOverflow)?;
            gross_output.checked_add_quanta(output_quanta)?;
            if let (Some(total), Some(net)) = (&mut net_output, net_quanta) {
                total.checked_add_quanta(net)?;
            }
            if level
                .risk_flags
                .contains(&QuoteRiskFlag::ReverseFromCompeting)
            {
                risks.insert(ExecutionRiskFlag::ReverseFromCompeting);
            }
            fills.push(QuoteLevelFill {
                quote_edge_id: edge.edge_id.clone(),
                snapshot_id: edge.snapshot_id.clone(),
                captured_at: edge.captured_at,
                rate: edge.rate.clone(),
                amount_in: AssetAmount::from_quanta(
                    input.asset_id.clone(),
                    use_quanta,
                    &self.units,
                )?,
                gross_amount_out: AssetAmount::from_quanta(
                    to_asset_id.clone(),
                    output_quanta,
                    &self.units,
                )?,
                net_amount_out: net_quanta
                    .map(|net| AssetAmount::from_quanta(to_asset_id.clone(), net, &self.units))
                    .transpose()?,
                capacity_from: AssetAmount::from_quanta(
                    input.asset_id.clone(),
                    capacity_from_quanta,
                    &self.units,
                )?,
                stock: edge.stock,
                quote_risk_flags: level.risk_flags.clone(),
            });
        }

        let is_fully_filled = remaining_quanta == 0;
        let bottleneck = if is_fully_filled {
            None
        } else {
            risks.insert(ExecutionRiskFlag::LiquidityCapped);
            Some(PairBottleneck {
                asset_id: input.asset_id.clone(),
                max_consumable_input: AssetAmount::from_quanta(
                    input.asset_id.clone(),
                    consumed_quanta,
                    &self.units,
                )?,
                max_gross_output: gross_output.clone(),
                reason: if fills.is_empty()
                    && risks.contains(&ExecutionRiskFlag::BelowMinimumOutput)
                {
                    ExecutionRiskFlag::BelowMinimumOutput
                } else {
                    ExecutionRiskFlag::LiquidityCapped
                },
            })
        };
        let capture_time_evidence = capture_time_evidence_for_levels(
            &fills,
            self.analysis_policy.capture_skew.max_capture_skew_seconds,
            self.analysis_policy.capture_skew.calibration_status,
        );
        Ok(PairFill {
            from_asset_id: input.asset_id.clone(),
            to_asset_id: to_asset_id.clone(),
            kind: FillKind::TakerDepth,
            requested_input: input.clone(),
            consumed_input: AssetAmount::from_quanta(
                input.asset_id.clone(),
                consumed_quanta,
                &self.units,
            )?,
            unfilled_input: AssetAmount::from_quanta(
                input.asset_id.clone(),
                remaining_quanta,
                &self.units,
            )?,
            gross_amount_out: gross_output,
            net_amount_out: net_output,
            fills,
            capture_time_evidence,
            is_fully_filled,
            execution_eligible: is_fully_filled && !fee_policy.is_unknown(),
            bottleneck,
            risk_flags: risks.into_iter().collect(),
        })
    }

    fn fill_maker_theory(
        &self,
        input: &AssetAmount,
        to_asset_id: &MarketAssetId,
        fee_policy: FeePolicy,
        depth: &PairDepth,
    ) -> Result<PairFill, EngineError> {
        let level = depth
            .primary
            .as_ref()
            .filter(|candidate| {
                candidate.observation.edge.execution_type == ExecutionType::MakerReference
            })
            .or_else(|| {
                depth.levels.iter().find(|candidate| {
                    candidate.observation.edge.execution_type == ExecutionType::MakerReference
                })
            })
            .ok_or(EngineError::InvalidQuoteSelection)?;
        let edge = &level.observation.edge;
        let scale = conversion_scale(input.unit, &edge.rate, self.units.unit(to_asset_id)?)?;
        let gross_quanta = floor_scaled(input.quanta, scale)?;
        let net_quanta = fee_policy.apply_to_quanta(gross_quanta)?;
        let (visible_reference_quanta, capacity_rounded) = stock_input_capacity(
            STOCK_DENOMINATION,
            edge.stock,
            ExecutionType::MakerReference,
            input.unit,
            self.units.unit(to_asset_id)?,
            scale,
        )?;
        let mut risks = BTreeSet::from([
            ExecutionRiskFlag::MakerReference,
            ExecutionRiskFlag::FillNotGuaranteed,
        ]);
        let reference_count = depth
            .levels
            .iter()
            .filter(|candidate| {
                candidate.observation.edge.execution_type == ExecutionType::MakerReference
            })
            .count();
        if reference_count == 1 {
            risks.insert(ExecutionRiskFlag::SingleListingBook);
        }
        if input.quanta > visible_reference_quanta {
            risks.insert(ExecutionRiskFlag::MakerDepthExceeded);
        }
        if capacity_rounded {
            risks.insert(ExecutionRiskFlag::CapacityRoundedToUnit);
        }
        if fee_policy.is_unknown() {
            risks.insert(ExecutionRiskFlag::UnknownFee);
        }
        if gross_quanta == 0 {
            risks.insert(ExecutionRiskFlag::BelowMinimumOutput);
        }
        let capture_time_evidence = Some(CaptureTimeEvidence::single(
            edge.captured_at,
            self.analysis_policy.capture_skew.max_capture_skew_seconds,
            self.analysis_policy.capture_skew.calibration_status,
        ));
        Ok(PairFill {
            from_asset_id: input.asset_id.clone(),
            to_asset_id: to_asset_id.clone(),
            kind: FillKind::MakerTheory,
            requested_input: input.clone(),
            consumed_input: input.clone(),
            unfilled_input: AssetAmount::zero(input.asset_id.clone(), &self.units)?,
            gross_amount_out: AssetAmount::from_quanta(
                to_asset_id.clone(),
                gross_quanta,
                &self.units,
            )?,
            net_amount_out: net_quanta
                .map(|net| AssetAmount::from_quanta(to_asset_id.clone(), net, &self.units))
                .transpose()?,
            fills: vec![QuoteLevelFill {
                quote_edge_id: edge.edge_id.clone(),
                snapshot_id: edge.snapshot_id.clone(),
                captured_at: edge.captured_at,
                rate: edge.rate.clone(),
                amount_in: input.clone(),
                gross_amount_out: AssetAmount::from_quanta(
                    to_asset_id.clone(),
                    gross_quanta,
                    &self.units,
                )?,
                net_amount_out: net_quanta
                    .map(|net| AssetAmount::from_quanta(to_asset_id.clone(), net, &self.units))
                    .transpose()?,
                capacity_from: AssetAmount::from_quanta(
                    input.asset_id.clone(),
                    visible_reference_quanta,
                    &self.units,
                )?,
                stock: edge.stock,
                quote_risk_flags: level.risk_flags.clone(),
            }],
            capture_time_evidence,
            is_fully_filled: true,
            execution_eligible: false,
            bottleneck: None,
            risk_flags: risks.into_iter().collect(),
        })
    }
}

impl CaptureTimeEvidence {
    fn single(
        captured_at: DateTime<Utc>,
        max_capture_skew_seconds: Option<u64>,
        calibration_status: PolicyCalibrationStatus,
    ) -> Self {
        Self {
            earliest_captured_at: captured_at,
            latest_captured_at: captured_at,
            capture_skew_seconds: 0,
            max_capture_skew_seconds,
            calibration_status,
            exceeds_max_capture_skew: max_capture_skew_seconds.map(|_| false),
        }
    }

    pub(crate) fn from_pair_fills(
        fills: &[PairFill],
        max_capture_skew_seconds: Option<u64>,
        calibration_status: PolicyCalibrationStatus,
    ) -> Option<Self> {
        let earliest = fills
            .iter()
            .filter_map(|fill| fill.capture_time_evidence)
            .map(|evidence| evidence.earliest_captured_at)
            .min()?;
        let latest = fills
            .iter()
            .filter_map(|fill| fill.capture_time_evidence)
            .map(|evidence| evidence.latest_captured_at)
            .max()?;
        Some(Self::from_range(
            earliest,
            latest,
            max_capture_skew_seconds,
            calibration_status,
        ))
    }

    fn from_range(
        earliest_captured_at: DateTime<Utc>,
        latest_captured_at: DateTime<Utc>,
        max_capture_skew_seconds: Option<u64>,
        calibration_status: PolicyCalibrationStatus,
    ) -> Self {
        let capture_skew_seconds = u64::try_from(
            latest_captured_at
                .signed_duration_since(earliest_captured_at)
                .num_seconds(),
        )
        .unwrap_or(u64::MAX);
        Self {
            earliest_captured_at,
            latest_captured_at,
            capture_skew_seconds,
            max_capture_skew_seconds,
            calibration_status,
            exceeds_max_capture_skew: max_capture_skew_seconds
                .map(|maximum| capture_skew_seconds > maximum),
        }
    }
}

fn capture_time_evidence_for_levels(
    fills: &[QuoteLevelFill],
    max_capture_skew_seconds: Option<u64>,
    calibration_status: PolicyCalibrationStatus,
) -> Option<CaptureTimeEvidence> {
    let earliest = fills.iter().map(|fill| fill.captured_at).min()?;
    let latest = fills.iter().map(|fill| fill.captured_at).max()?;
    Some(CaptureTimeEvidence::from_range(
        earliest,
        latest,
        max_capture_skew_seconds,
        calibration_status,
    ))
}

pub(crate) fn apply_capture_skew_safety(
    risks: &mut BTreeSet<ExecutionRiskFlag>,
    evidence: Option<CaptureTimeEvidence>,
    pair_count: usize,
) -> bool {
    if pair_count <= 1 {
        return true;
    }
    if evidence.is_none_or(|item| item.calibration_status != PolicyCalibrationStatus::Verified) {
        risks.insert(ExecutionRiskFlag::CaptureSkewUnverified);
        return false;
    }
    match evidence.and_then(|item| item.exceeds_max_capture_skew) {
        Some(false) => true,
        Some(true) => {
            risks.insert(ExecutionRiskFlag::CaptureSkewExceeded);
            false
        }
        None => {
            risks.insert(ExecutionRiskFlag::CaptureSkewUnverified);
            false
        }
    }
}

fn insert_execution_risk(risks: &mut Vec<ExecutionRiskFlag>, risk: ExecutionRiskFlag) {
    if !risks.contains(&risk) {
        risks.push(risk);
        risks.sort();
    }
}

#[derive(Clone, Copy, Debug)]
struct ConversionScale {
    numerator: u128,
    denominator: u128,
}

fn conversion_scale(
    from_unit: AssetUnit,
    rate: &Ratio,
    to_unit: AssetUnit,
) -> Result<ConversionScale, EngineError> {
    let numerator = u128::from(from_unit.numerator)
        .checked_mul(u128::from(rate.numerator))
        .and_then(|value| value.checked_mul(u128::from(to_unit.denominator)))
        .ok_or(EngineError::NumericOverflow)?;
    let denominator = u128::from(from_unit.denominator)
        .checked_mul(u128::from(rate.denominator))
        .and_then(|value| value.checked_mul(u128::from(to_unit.numerator)))
        .ok_or(EngineError::NumericOverflow)?;
    let divisor = greatest_common_divisor(numerator, denominator);
    Ok(ConversionScale {
        numerator: numerator / divisor,
        denominator: denominator / divisor,
    })
}

fn floor_scaled(input_quanta: u64, scale: ConversionScale) -> Result<u64, EngineError> {
    let value = u128::from(input_quanta)
        .checked_mul(scale.numerator)
        .ok_or(EngineError::NumericOverflow)?
        / scale.denominator;
    u64::try_from(value).map_err(|_| EngineError::NumericOverflow)
}

fn max_input_for_target_cap(
    target_cap_quanta: u64,
    scale: ConversionScale,
) -> Result<u64, EngineError> {
    let capacity = u128::from(target_cap_quanta)
        .checked_mul(scale.denominator)
        .ok_or(EngineError::NumericOverflow)?;
    let capacity = capacity / scale.numerator;
    Ok(u64::try_from(capacity).unwrap_or(u64::MAX))
}

fn whole_units_to_quanta_floor(
    whole_units: u64,
    unit: AssetUnit,
) -> Result<(u64, bool), EngineError> {
    let scaled = u128::from(whole_units)
        .checked_mul(u128::from(unit.denominator))
        .ok_or(EngineError::NumericOverflow)?;
    let divisor = u128::from(unit.numerator);
    let quanta = scaled / divisor;
    Ok((
        u64::try_from(quanta).map_err(|_| EngineError::NumericOverflow)?,
        scaled % divisor != 0,
    ))
}

/// Which side of a quote the panel's stock column counts.
///
/// Stock counts **what the lister pays out**: the to-asset on a taker row (the
/// lister owns what the route wants), the from-asset on a maker-reference row
/// (the competing lister owns what the route holds).
///
/// Settled by observation, TASK-50, on a POE2 chaos → volatile-vaal panel: the
/// available rows counted single-digit orbs (1, 1, 4, 4, 9) while the competing
/// rows counted thousands (5755, 2300, 1100, 8584, 1072). Both sides are the
/// payout — available listers hand over the orb, competing listers hand over
/// chaos — and the arithmetic seals it: every competing stock is an exact
/// multiple of its own ratio (5755 = 5×1151, 2300 = 2×1150, 8584 = 8×1073).
/// Orders are placed in whole orbs, so the chaos they pay out lands on a
/// multiple of the rate; nothing about the receive-side reading would make five
/// separate listings divide evenly.
///
/// Observed on POE2. POE1's exchange panel is the same feature and is assumed
/// to match; a POE1 book whose depth reads a hundredfold wrong is the symptom
/// that would say otherwise.
///
/// The enum and its four-quadrant tests stay as the executable statement of
/// what the column means. Every depth computation funnels through
/// [`stock_input_capacity`], so the contrast is what would catch a silent
/// drift back to the other reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StockDenomination {
    /// The asset the lister hands over. The verified reading, used everywhere.
    ListerPayout,
    /// The asset the lister asks to receive. Ruled out by TASK-50; kept so the
    /// tests below can state what it would have computed.
    #[allow(dead_code)]
    ListerReceives,
}

/// The single place the interpretation is chosen.
const STOCK_DENOMINATION: StockDenomination = StockDenomination::ListerPayout;

/// How much input (from-asset quanta) one level's stock allows, plus whether
/// unit conversion had to round. When the stock counts the output side it is
/// inverted through the level's rate; when it counts the input side it is the
/// cap directly.
fn stock_input_capacity(
    denomination: StockDenomination,
    stock: u64,
    execution_type: ExecutionType,
    input_unit: AssetUnit,
    output_unit: AssetUnit,
    scale: ConversionScale,
) -> Result<(u64, bool), EngineError> {
    // On a taker row the lister pays out the route's target; on a
    // maker-reference row the competing lister pays out the route's input.
    let payout_is_output = matches!(execution_type, ExecutionType::Taker);
    let stock_counts_output = match denomination {
        StockDenomination::ListerPayout => payout_is_output,
        StockDenomination::ListerReceives => !payout_is_output,
    };
    if stock_counts_output {
        let (output_cap, rounded) = whole_units_to_quanta_floor(stock, output_unit)?;
        Ok((max_input_for_target_cap(output_cap, scale)?, rounded))
    } else {
        whole_units_to_quanta_floor(stock, input_unit)
    }
}

fn compare_taker_levels(left: &EvaluatedQuoteEdge, right: &EvaluatedQuoteEdge) -> Ordering {
    right
        .observation
        .edge
        .rate
        .compare_value(&left.observation.edge.rate)
        .then_with(|| {
            right
                .effective_confidence_ppm
                .cmp(&left.effective_confidence_ppm)
        })
        .then_with(|| {
            right
                .observation
                .edge
                .stock
                .cmp(&left.observation.edge.stock)
        })
        .then_with(|| {
            left.observation
                .edge
                .edge_id
                .cmp(&right.observation.edge.edge_id)
        })
}

const fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_capacity_is_the_largest_input_below_the_exact_stock_boundary() {
        for numerator in 1_u128..=16 {
            for denominator in 1_u128..=16 {
                let scale = ConversionScale {
                    numerator,
                    denominator,
                };
                for target_cap_quanta in 0_u64..=64 {
                    let capacity = max_input_for_target_cap(target_cap_quanta, scale)
                        .expect("bounded capacity");
                    let exact_stock_boundary = u128::from(target_cap_quanta) * denominator;
                    assert!(u128::from(capacity) * numerator <= exact_stock_boundary);
                    assert!(
                        u128::from(capacity + 1) * numerator > exact_stock_boundary,
                        "rate={numerator}/{denominator}, stock={target_cap_quanta}"
                    );
                    assert!(
                        floor_scaled(capacity, scale).expect("bounded output") <= target_cap_quanta
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod capture_skew_tests {
    use super::*;
    use chrono::TimeZone;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + seconds, 0)
            .single()
            .expect("time")
    }

    /// F3: legs captured more than the armed maximum apart are flagged and
    /// lose eligibility; legs inside the window pass.
    #[test]
    fn f3_cross_leg_capture_skew_gate_flags_and_blocks() {
        let exceeded = CaptureTimeEvidence::from_range(
            at(0),
            at(120),
            Some(90),
            PolicyCalibrationStatus::Verified,
        );
        assert_eq!(exceeded.capture_skew_seconds, 120);
        assert_eq!(exceeded.exceeds_max_capture_skew, Some(true));
        let mut risks = BTreeSet::new();
        assert!(!apply_capture_skew_safety(&mut risks, Some(exceeded), 3));
        assert!(risks.contains(&ExecutionRiskFlag::CaptureSkewExceeded));

        let inside = CaptureTimeEvidence::from_range(
            at(0),
            at(60),
            Some(90),
            PolicyCalibrationStatus::Verified,
        );
        let mut risks = BTreeSet::new();
        assert!(apply_capture_skew_safety(&mut risks, Some(inside), 3));
        assert!(risks.is_empty());
    }
}

#[cfg(test)]
mod stock_denomination_tests {
    use super::*;

    /// Ten stock at a 1→2 rate, whole units on both sides. Which side the
    /// ten caps decides whether the input capacity is 10 or 5, so each of
    /// the four combinations pins one number — and the two `lister_receives`
    /// tests keep the ruled-out reading legible next to the shipped one.
    fn capacity(denomination: StockDenomination, execution_type: ExecutionType) -> u64 {
        let doubles = ConversionScale {
            numerator: 2,
            denominator: 1,
        };
        let (quanta, rounded) = stock_input_capacity(
            denomination,
            10,
            execution_type,
            AssetUnit::whole(),
            AssetUnit::whole(),
            doubles,
        )
        .expect("whole units cannot overflow");
        assert!(!rounded, "whole units never round");
        quanta
    }

    #[test]
    fn lister_payout_on_a_taker_row_caps_the_output_side() {
        // The lister pays out the target: 10 target units back through the
        // 1→2 rate is 5 input units.
        assert_eq!(
            capacity(StockDenomination::ListerPayout, ExecutionType::Taker),
            5
        );
    }

    #[test]
    fn lister_payout_on_a_maker_row_caps_the_input_side() {
        // The competing lister pays out the route's own input asset.
        assert_eq!(
            capacity(
                StockDenomination::ListerPayout,
                ExecutionType::MakerReference
            ),
            10
        );
    }

    #[test]
    fn lister_receives_on_a_taker_row_caps_the_input_side() {
        assert_eq!(
            capacity(StockDenomination::ListerReceives, ExecutionType::Taker),
            10
        );
    }

    #[test]
    fn lister_receives_on_a_maker_row_caps_the_output_side() {
        assert_eq!(
            capacity(
                StockDenomination::ListerReceives,
                ExecutionType::MakerReference
            ),
            5
        );
    }

    /// The shipped switch is the payout interpretation — verified on a real
    /// panel (TASK-50) and the one every engine test's end-to-end numbers
    /// assume. Changing the constant has to break this test on the way.
    #[test]
    fn the_shipped_interpretation_is_lister_payout() {
        assert_eq!(STOCK_DENOMINATION, StockDenomination::ListerPayout);
    }
}
