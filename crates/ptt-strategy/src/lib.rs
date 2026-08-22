//! Trading strategy layer: everything that turns engine output into advice.
//!
//! The engine answers "what does the market arithmetic say"; this crate
//! answers "should you act on it, how much, and what could go wrong". It is
//! the Rust home of the six algorithm modules the Electron build carried in
//! `electron/domain/algorithms/`, re-expressed on the exact-rational engine
//! types instead of `f64` and string flags.

mod anchor_value;
mod day_rollup;
mod exact;
mod execution_safety;
mod maker_strategy;
mod market_analytics;
mod market_policy;
mod price_curve;
mod route_accounting;
mod units;

pub use anchor_value::{
    Valuation, ValuationMode, ValuationRequest, ValuationStatus, value_against_anchor,
};
pub use day_rollup::{PairDayRollup, SnapshotPairFold, build_pair_day_rollups};
pub use execution_safety::{
    Actionability, ExecutionRisk, ModelCaveat, RiskAssessment, RiskThresholds, assess_path,
    assess_steps, assess_triangle,
};
pub use maker_strategy::{
    MakerExcludedListing, MakerMode, MakerQueueExclusion, MakerQueueLevel, MakerRecommendation,
    MakerRequest, MakerStrategy, MakerWall, calculate_maker_strategy,
};
pub use market_analytics::{
    AnalyticsThresholds, AnchorCross, AnchorDrift, AnchorHealth, AssetPulse, DailyPairStat,
    LiquidityClass, MarketPulse, TrendVerdict, market_pulse,
};
pub use market_policy::{
    AnchorAction, AnchorEvidence, AnchorRecommendation, DEFAULT_CORE_LIQUIDITY, MarketPolicy,
    MarketRole, RoleCapabilities, recommend_liquidity_anchors,
};
pub use price_curve::{
    AnomalySeverity, AnomalyThresholds, BucketSize, PriceAnomaly, PriceAnomalyKind, PriceCandle,
    PricePoint, PriceSummary, anomalies, candles, price_points, summarize,
};
pub use route_accounting::{
    MarkRateSource, MarkRateTable, ProfitTier, ResidualPosition, RouteAccounting,
    RouteAccountingRequest, derive_route_accounting,
};

use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StrategyError {
    #[error("strategy request is missing a required field or is self-referential")]
    InvalidRequest,
    #[error("route cannot be accounted for: a leg consumed nothing or the arithmetic overflowed")]
    UnusableRoute,
    #[error("engine rejected the request: {0}")]
    Engine(#[from] ptt_trade_engine::EngineError),
}
