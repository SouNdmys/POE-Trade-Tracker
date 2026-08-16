//! Typed execution risk and the actionability ladder.
//!
//! Ported from `executionSafety.ts`, which kept its risks as free strings and
//! consequently carried several spellings of the same hazard
//! (`stale`/`stale_data`, `low_liquidity`/`thin_liquidity`,
//! `maker_reference`/`maker_reference_only`, `price_outlier`/
//! `suspicious_outlier`). Only one spelling was ever produced by the code that
//! raised each hazard, so half of the blocking set was unreachable and a
//! caller testing the other spelling silently saw a clean result. The enum
//! collapses the aliases, which is why the blocking list here is shorter than
//! the twenty-two entry set it replaces.
//!
//! The second departure is the caveat/risk split. The engine emits three
//! standing flags that describe the *model* rather than the market:
//! `UnknownFee`, `UnverifiedProductPolicy` and `UnverifiedMinimumLots`. They
//! are true of every fill this product will ever produce — the tracker never
//! executes a trade, and the personal-default policy verifies no costs — so
//! treating them as execution blockers pins every result to the bottom of the
//! ladder and destroys the ranking the ladder exists to provide. They are
//! recorded as [`ModelCaveat`]s and reported, never used to gate.

use std::collections::BTreeSet;

use ptt_market_book::QuoteRiskFlag;
use ptt_trade_engine::{ConversionPath, ExecutionRiskFlag, FillKind, PairFill, TriangleEvaluation};
use serde::{Deserialize, Serialize};

/// A market hazard that bears on whether a result can be acted on now.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRisk {
    /// The quoted rate sits behind a `<`/`>` comparator, so the true rate is
    /// bounded but unknown.
    ComparatorBoundary,
    /// A leg is a maker listing reference: nothing fills until someone takes
    /// the order.
    MakerReference,
    /// The rate was read from the competing table, so it prices the other
    /// side of the book rather than what is on offer.
    CompetingReference,
    /// A leg is older than the fresh window.
    StaleData,
    /// A leg is older than the usable window.
    ArchivedData,
    /// A capture timestamp lies in the future beyond tolerance: the clock, not
    /// the market, is the source of the number.
    ClockSkewFuture,
    /// The legs of a multi-leg result were captured too far apart to describe
    /// one market moment.
    CaptureSkewExceeded,
    /// Recognition confidence for a leg fell below the policy minimum.
    LowConfidence,
    /// Visible stock on a leg is positive but small.
    ThinLiquidity,
    /// Visible depth ran out before the requested amount was filled.
    LiquidityCapped,
    /// The route completed only partially.
    PartialRoute,
    /// Unconverted currency is left stranded mid-route.
    ResidualInventory,
    /// The requested size exceeds the visible maker-reference depth.
    MakerDepthExceeded,
    /// The rate is far enough from the rest of the book to look like a
    /// misread or a trap rather than an opportunity.
    PriceOutlier,
    /// The rate falls outside the band the top of book supports.
    OutsideTopBookBand,
    /// The record is isolated or deleted, so it has no corroboration.
    UnsupportedRecord,
    /// Data is missing outright and must be captured before judging.
    NeedsProbe,
}

impl ExecutionRisk {
    /// Whether this hazard rules out calling a result instantly executable.
    ///
    /// Every market risk blocks; the list is exhaustive rather than a lookup
    /// set so a newly added variant cannot default to "harmless".
    #[must_use]
    pub const fn blocks_instant_execution(self) -> bool {
        match self {
            Self::ComparatorBoundary
            | Self::MakerReference
            | Self::CompetingReference
            | Self::StaleData
            | Self::ArchivedData
            | Self::ClockSkewFuture
            | Self::CaptureSkewExceeded
            | Self::LowConfidence
            | Self::ThinLiquidity
            | Self::LiquidityCapped
            | Self::PartialRoute
            | Self::ResidualInventory
            | Self::MakerDepthExceeded
            | Self::PriceOutlier
            | Self::OutsideTopBookBand
            | Self::UnsupportedRecord
            | Self::NeedsProbe => true,
        }
    }

    /// Whether this hazard means the numbers themselves are suspect, as
    /// opposed to merely conditional.
    #[must_use]
    pub const fn is_outlier(self) -> bool {
        matches!(self, Self::PriceOutlier | Self::OutsideTopBookBand)
    }

    /// Whether this hazard means the result describes an order to place
    /// rather than a trade to take.
    #[must_use]
    pub const fn is_maker_shaped(self) -> bool {
        matches!(self, Self::MakerReference | Self::CompetingReference)
    }
}

/// A standing limitation of the model, true of every result this product
/// produces. Reported for honesty; never used to gate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCaveat {
    /// Profit is gross: no fee is modelled.
    GrossProfitOnly,
    /// The product never places orders; it advises and the user acts.
    AdvisoryOnly,
    /// Minimum lot sizes are enforced by asset units rather than verified
    /// against the client.
    MinimumLotsUnverified,
    /// The cross-leg skew gate is running on an uncalibrated window.
    CaptureSkewUncalibrated,
    /// A capacity figure was rounded down to a whole asset unit.
    CapacityRoundedToUnit,
    /// A figure extrapolates a leg's observed blended rate past the depth
    /// that was actually seen. Deeper levels price worse, so the number is an
    /// upper bound, not a forecast.
    ExtrapolatedBeyondObservedDepth,
    /// The search hit its budget before exhausting the space.
    SearchTruncated,
}

/// How far a result can be trusted, worst to best.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actionability {
    /// Every leg is a fresh taker fill with no blocking risk.
    InstantExecutable,
    /// The arithmetic holds only if someone takes a listed order.
    MakerTheoretical,
    /// Data must be captured or refreshed before this can be judged.
    ProbeRequired,
    /// The numbers look wrong, not good.
    SuspiciousOutlier,
}

impl Actionability {
    const fn risk_rank(self) -> u8 {
        match self {
            Self::InstantExecutable => 0,
            Self::MakerTheoretical => 1,
            Self::ProbeRequired => 2,
            Self::SuspiciousOutlier => 3,
        }
    }

    /// The more conservative of two verdicts.
    #[must_use]
    pub const fn safer(self, other: Self) -> Self {
        if other.risk_rank() > self.risk_rank() {
            other
        } else {
            self
        }
    }

    /// The most conservative verdict in a set. An empty set is
    /// [`Actionability::ProbeRequired`]: nothing observed means nothing
    /// established.
    #[must_use]
    pub fn safest(values: impl IntoIterator<Item = Self>) -> Self {
        values
            .into_iter()
            .fold(None, |worst: Option<Self>, value| {
                Some(worst.map_or(value, |current| current.safer(value)))
            })
            .unwrap_or(Self::ProbeRequired)
    }
}

/// Tunables for the hazards that are a matter of degree rather than kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskThresholds {
    /// Visible stock at or below this counts as thin.
    pub thin_liquidity_stock: u64,
}

impl Default for RiskThresholds {
    fn default() -> Self {
        // The panel routinely shows single-digit stock rows next to
        // five-figure ones; a hundred orbs is the point below which one
        // competing fill empties the level.
        Self {
            thin_liquidity_stock: 100,
        }
    }
}

/// The risks and caveats carried by one result, and the verdict they imply.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAssessment {
    pub risks: BTreeSet<ExecutionRisk>,
    pub caveats: BTreeSet<ModelCaveat>,
    pub actionability: Actionability,
}

impl RiskAssessment {
    /// The risks that keep this result off the instant-executable rung.
    #[must_use]
    pub fn blocking(&self) -> Vec<ExecutionRisk> {
        self.risks
            .iter()
            .copied()
            .filter(|risk| risk.blocks_instant_execution())
            .collect()
    }

    #[must_use]
    pub fn contains(&self, risk: ExecutionRisk) -> bool {
        self.risks.contains(&risk)
    }

    fn from_parts(
        risks: BTreeSet<ExecutionRisk>,
        caveats: BTreeSet<ModelCaveat>,
        needs_probe: bool,
    ) -> Self {
        let mut risks = risks;
        if needs_probe {
            risks.insert(ExecutionRisk::NeedsProbe);
        }
        let actionability = verdict(&risks);
        Self {
            risks,
            caveats,
            actionability,
        }
    }
}

/// The ladder, in precedence order: a suspect number outranks a conditional
/// one, which outranks a missing one, which outranks a clean one.
fn verdict(risks: &BTreeSet<ExecutionRisk>) -> Actionability {
    if risks.iter().any(|risk| risk.is_outlier()) {
        return Actionability::SuspiciousOutlier;
    }
    if risks.iter().any(|risk| risk.is_maker_shaped()) {
        return Actionability::MakerTheoretical;
    }
    if risks.iter().any(|risk| risk.blocks_instant_execution()) {
        return Actionability::ProbeRequired;
    }
    Actionability::InstantExecutable
}

fn absorb_execution_flag(
    flag: ExecutionRiskFlag,
    risks: &mut BTreeSet<ExecutionRisk>,
    caveats: &mut BTreeSet<ModelCaveat>,
) {
    match flag {
        ExecutionRiskFlag::ReverseFromCompeting => {
            risks.insert(ExecutionRisk::CompetingReference);
        }
        ExecutionRiskFlag::MakerReference
        | ExecutionRiskFlag::FillNotGuaranteed
        | ExecutionRiskFlag::MultiHopMaker => {
            risks.insert(ExecutionRisk::MakerReference);
        }
        ExecutionRiskFlag::MakerDepthExceeded => {
            risks.insert(ExecutionRisk::MakerDepthExceeded);
        }
        ExecutionRiskFlag::LiquidityCapped | ExecutionRiskFlag::BelowMinimumOutput => {
            risks.insert(ExecutionRisk::LiquidityCapped);
        }
        ExecutionRiskFlag::PartialRoute => {
            risks.insert(ExecutionRisk::PartialRoute);
        }
        ExecutionRiskFlag::ResidualInventory => {
            risks.insert(ExecutionRisk::ResidualInventory);
        }
        ExecutionRiskFlag::CaptureSkewExceeded => {
            risks.insert(ExecutionRisk::CaptureSkewExceeded);
        }
        ExecutionRiskFlag::CaptureSkewUnverified => {
            caveats.insert(ModelCaveat::CaptureSkewUncalibrated);
        }
        ExecutionRiskFlag::CapacityRoundedToUnit => {
            caveats.insert(ModelCaveat::CapacityRoundedToUnit);
        }
        ExecutionRiskFlag::SearchTruncated => {
            caveats.insert(ModelCaveat::SearchTruncated);
        }
        ExecutionRiskFlag::UnknownFee => {
            caveats.insert(ModelCaveat::GrossProfitOnly);
        }
        ExecutionRiskFlag::UnverifiedProductPolicy => {
            caveats.insert(ModelCaveat::AdvisoryOnly);
        }
        ExecutionRiskFlag::UnverifiedMinimumLots => {
            caveats.insert(ModelCaveat::MinimumLotsUnverified);
        }
    }
}

fn absorb_quote_flag(flag: QuoteRiskFlag, risks: &mut BTreeSet<ExecutionRisk>) {
    match flag {
        QuoteRiskFlag::ReverseFromCompeting | QuoteRiskFlag::ReverseFromAvailable => {
            risks.insert(ExecutionRisk::CompetingReference);
        }
        QuoteRiskFlag::ComparatorBoundary => {
            risks.insert(ExecutionRisk::ComparatorBoundary);
        }
        QuoteRiskFlag::StaleData => {
            risks.insert(ExecutionRisk::StaleData);
        }
        QuoteRiskFlag::ArchivedData => {
            risks.insert(ExecutionRisk::ArchivedData);
        }
        QuoteRiskFlag::LowConfidence => {
            risks.insert(ExecutionRisk::LowConfidence);
        }
        QuoteRiskFlag::FutureTimestamp => {
            risks.insert(ExecutionRisk::ClockSkewFuture);
        }
        QuoteRiskFlag::PriceOutlier => {
            risks.insert(ExecutionRisk::PriceOutlier);
        }
        QuoteRiskFlag::OutsideTopBookBand => {
            risks.insert(ExecutionRisk::OutsideTopBookBand);
        }
        QuoteRiskFlag::IsolatedRecord | QuoteRiskFlag::DeletedRecord => {
            risks.insert(ExecutionRisk::UnsupportedRecord);
        }
    }
}

fn absorb_step(
    step: &PairFill,
    thresholds: RiskThresholds,
    risks: &mut BTreeSet<ExecutionRisk>,
    caveats: &mut BTreeSet<ModelCaveat>,
) {
    if step.kind == FillKind::MakerTheory {
        risks.insert(ExecutionRisk::MakerReference);
    }
    if !step.is_fully_filled {
        risks.insert(ExecutionRisk::LiquidityCapped);
    }
    for flag in &step.risk_flags {
        absorb_execution_flag(*flag, risks, caveats);
    }
    for fill in &step.fills {
        for flag in &fill.quote_risk_flags {
            absorb_quote_flag(*flag, risks);
        }
        if fill.stock > 0 && fill.stock < thresholds.thin_liquidity_stock {
            risks.insert(ExecutionRisk::ThinLiquidity);
        }
    }
    // A step with no levels behind it is not a clean step; it is a step with
    // nothing observed, which the JS version silently scored as risk-free.
    if step.fills.is_empty() {
        risks.insert(ExecutionRisk::NeedsProbe);
    }
}

/// Assess a bare sequence of legs.
#[must_use]
pub fn assess_steps(
    steps: &[PairFill],
    thresholds: RiskThresholds,
    needs_probe: bool,
) -> RiskAssessment {
    let mut risks = BTreeSet::new();
    let mut caveats = BTreeSet::new();
    if steps.is_empty() {
        risks.insert(ExecutionRisk::NeedsProbe);
    }
    for step in steps {
        absorb_step(step, thresholds, &mut risks, &mut caveats);
    }
    RiskAssessment::from_parts(risks, caveats, needs_probe)
}

/// Assess a conversion path, including the path-level flags the engine
/// attaches on top of its legs.
#[must_use]
pub fn assess_path(
    path: &ConversionPath,
    thresholds: RiskThresholds,
    needs_probe: bool,
) -> RiskAssessment {
    let mut assessment = assess_steps(&path.steps, thresholds, needs_probe);
    for flag in &path.risk_flags {
        absorb_execution_flag(*flag, &mut assessment.risks, &mut assessment.caveats);
    }
    if !path.residuals.is_empty() {
        assessment.risks.insert(ExecutionRisk::ResidualInventory);
    }
    if path.gross_only {
        assessment.caveats.insert(ModelCaveat::GrossProfitOnly);
    }
    assessment.actionability = verdict(&assessment.risks);
    assessment
}

/// Assess a cycle evaluation.
#[must_use]
pub fn assess_triangle(
    evaluation: &TriangleEvaluation,
    thresholds: RiskThresholds,
    needs_probe: bool,
) -> RiskAssessment {
    let mut assessment = assess_steps(&evaluation.steps, thresholds, needs_probe);
    for flag in &evaluation.risk_flags {
        absorb_execution_flag(*flag, &mut assessment.risks, &mut assessment.caveats);
    }
    if !evaluation.residuals.is_empty() {
        assessment.risks.insert(ExecutionRisk::ResidualInventory);
    }
    assessment.actionability = verdict(&assessment.risks);
    assessment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safest_of_nothing_is_probe_required() {
        assert_eq!(
            Actionability::safest(std::iter::empty()),
            Actionability::ProbeRequired
        );
    }

    #[test]
    fn safer_keeps_the_worst_verdict_in_both_orders() {
        let pairs = [
            (
                Actionability::InstantExecutable,
                Actionability::MakerTheoretical,
            ),
            (
                Actionability::MakerTheoretical,
                Actionability::ProbeRequired,
            ),
            (
                Actionability::ProbeRequired,
                Actionability::SuspiciousOutlier,
            ),
        ];
        for (better, worse) in pairs {
            assert_eq!(better.safer(worse), worse);
            assert_eq!(worse.safer(better), worse);
        }
    }

    #[test]
    fn model_caveats_never_reach_the_risk_set() {
        // Every fill this product emits carries these three; if they were
        // treated as risks, no result could ever rank above probe_required.
        let mut risks = BTreeSet::new();
        let mut caveats = BTreeSet::new();
        for flag in [
            ExecutionRiskFlag::UnknownFee,
            ExecutionRiskFlag::UnverifiedProductPolicy,
            ExecutionRiskFlag::UnverifiedMinimumLots,
        ] {
            absorb_execution_flag(flag, &mut risks, &mut caveats);
        }
        assert!(risks.is_empty(), "model limits are not market hazards");
        assert_eq!(caveats.len(), 3);
        assert_eq!(verdict(&risks), Actionability::InstantExecutable);
    }

    #[test]
    fn outlier_outranks_maker_which_outranks_plain_blockers() {
        let ladder = [
            (ExecutionRisk::ThinLiquidity, Actionability::ProbeRequired),
            (
                ExecutionRisk::MakerReference,
                Actionability::MakerTheoretical,
            ),
            (
                ExecutionRisk::PriceOutlier,
                Actionability::SuspiciousOutlier,
            ),
        ];
        let mut risks = BTreeSet::new();
        for (risk, expected) in ladder {
            risks.insert(risk);
            assert_eq!(verdict(&risks), expected, "after adding {risk:?}");
        }
    }

    #[test]
    fn every_market_risk_blocks_instant_execution() {
        for risk in [
            ExecutionRisk::ComparatorBoundary,
            ExecutionRisk::MakerReference,
            ExecutionRisk::CompetingReference,
            ExecutionRisk::StaleData,
            ExecutionRisk::ArchivedData,
            ExecutionRisk::ClockSkewFuture,
            ExecutionRisk::CaptureSkewExceeded,
            ExecutionRisk::LowConfidence,
            ExecutionRisk::ThinLiquidity,
            ExecutionRisk::LiquidityCapped,
            ExecutionRisk::PartialRoute,
            ExecutionRisk::ResidualInventory,
            ExecutionRisk::MakerDepthExceeded,
            ExecutionRisk::PriceOutlier,
            ExecutionRisk::OutsideTopBookBand,
            ExecutionRisk::UnsupportedRecord,
            ExecutionRisk::NeedsProbe,
        ] {
            assert!(risk.blocks_instant_execution(), "{risk:?}");
            let mut risks = BTreeSet::new();
            risks.insert(risk);
            assert_ne!(verdict(&risks), Actionability::InstantExecutable);
        }
    }

    #[test]
    fn no_steps_means_probe_required_not_clean() {
        let assessment = assess_steps(&[], RiskThresholds::default(), false);
        assert_eq!(assessment.actionability, Actionability::ProbeRequired);
        assert!(assessment.contains(ExecutionRisk::NeedsProbe));
    }
}
