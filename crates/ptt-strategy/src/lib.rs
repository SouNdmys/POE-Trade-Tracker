//! Trading strategy layer: everything that turns engine output into advice.
//!
//! The engine answers "what does the market arithmetic say"; this crate
//! answers "should you act on it, how much, and what could go wrong". It is
//! the Rust home of the six algorithm modules the Electron build carried in
//! `electron/domain/algorithms/`, re-expressed on the exact-rational engine
//! types instead of `f64` and string flags.

mod exact;
mod execution_safety;
mod route_accounting;

pub use execution_safety::{
    Actionability, ExecutionRisk, ModelCaveat, RiskAssessment, RiskThresholds, assess_path,
    assess_steps, assess_triangle,
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
