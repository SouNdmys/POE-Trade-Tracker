//! Background actor runtime — P0 skeleton.
//!
//! The full runtime is ported in P2 from POE Alarm's `poe-alarm-runtime`
//! (`actor.rs`): a `std::sync::mpsc` actor with generations + leases so stale
//! recognition results can never write into the current book, latest-value
//! snapshot coalescing toward the UI, bounded native ownership, and a 1-second
//! shutdown contract. Only the generation primitive lives here for now so
//! earlier layers can already stamp work with the session that issued it.

pub mod analysis;
#[cfg(windows)]
pub mod live;
#[cfg(windows)]
pub mod pipeline;
pub mod report_text;
pub mod reports;
pub mod rollup;

/// The domain types the page models are made of.
///
/// The app depends on this crate and not on the engine, the strategy layer or
/// the workflows layer, so without these it can hold a `RadarItem` but cannot
/// name one. Re-exporting here rather than adding four more path dependencies
/// keeps `ptt-runtime` the single seam the interface talks to — and keeps the
/// bilingual naming of these enums in `report_text`, where an unnamed new
/// variant fails to compile instead of reaching a screen as a Rust
/// identifier.
pub mod domain {
    /// The name behind an id. Every layer under the interface speaks ids; the
    /// interface has to speak names, and this is where it looks them up.
    pub use ptt_catalog::{Catalog, CatalogAsset, poe1 as poe1_catalog, poe2 as poe2_catalog};
    pub use ptt_market_book::{
        FreshnessAssessment, FreshnessPolicy, FreshnessStatus, QuoteRiskFlag, QuoteSelectionPolicy,
        QuoteSelectionStrategy,
    };
    pub use ptt_strategy::{
        Actionability, AnchorAction, AnchorCross, AnchorDrift, AnchorHealth, AnchorRecommendation,
        AnomalySeverity, AssetPulse, BucketSize, ExecutionRisk, LiquidityClass,
        MakerExcludedListing, MakerMode, MakerQueueExclusion, MakerQueueLevel, MakerRecommendation,
        MakerStrategy, MakerWall, MarketPolicy, MarketPulse, MarketRole, ModelCaveat, PriceAnomaly,
        PriceAnomalyKind, PriceCandle, PricePoint, PriceSummary, ProfitTier, ResidualPosition,
        RiskAssessment, RiskThresholds, RouteAccounting, TrendVerdict, Valuation, ValuationMode,
        ValuationStatus,
    };
    pub use ptt_trade_engine::{
        AssetAmount, AssetUnit, CaptureTimeEvidence, ComparisonDirection, ConversionPath,
        ExecutionRiskFlag, PairFill, TriangleEvaluation,
    };
    pub use ptt_workflows::{
        FocusCoverage, FocusCoverageStatus, FocusRole, FocusScopeStatus, ProbeCandidate,
        ProbePriority, ProbeReason, RadarDiagnostics, RadarItem, RadarItemKind, RadarReason,
    };
}

/// Monotonically increasing id for a runtime session. Work stamped with an old
/// generation is discarded on arrival; replacement invalidates the generation
/// *before* waiting on anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const FIRST: RuntimeGeneration = RuntimeGeneration(1);

    pub fn next(self) -> RuntimeGeneration {
        RuntimeGeneration(self.0.checked_add(1).expect("generation counter overflow"))
    }

    pub fn accepts(self, stamped: RuntimeGeneration) -> bool {
        self == stamped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generations_are_rejected() {
        let current = RuntimeGeneration::FIRST;
        let replaced = current.next();
        assert!(current.accepts(current));
        assert!(!replaced.accepts(current));
        assert!(replaced > current);
    }
}
