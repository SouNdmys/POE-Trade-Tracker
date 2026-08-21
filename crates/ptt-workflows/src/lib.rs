//! What to watch, what is missing, and what to look at next.
//!
//! Ported from the POE1 `decision-workflows` crate, trimmed to the four
//! modules this product still needs. Its `maker` and `history` modules are
//! gone: [`ptt_strategy::calculate_maker_strategy`] and
//! [`ptt_strategy::candles`] replace them, and keeping two implementations of
//! either would guarantee they eventually disagree.

mod focus;
mod probe;
mod radar;
mod watch;

pub use focus::{
    DirectedFocusPair, FocusGroup, FocusGroupDraft, FocusGroupItem, FocusPairRelation, FocusRole,
    FocusRoleCounts, FocusScope, FocusScopePolicy, FocusScopeStatus, ProbePriority,
};
pub use probe::{
    FocusCoverage, FocusCoverageStatus, ProbeCandidate, ProbeReason, ProbeSource, ProbeStatus,
    ProbeSuggestion, derive_focus_probe_candidates,
};
pub use radar::{
    RadarBudget, RadarCategory, RadarDiagnostics, RadarItem, RadarItemKind, RadarProgress,
    RadarReason, RadarRequest, RadarResult, RadarStage, RadarStart, run_opportunity_radar,
};
pub use watch::{
    WatchHealth, WatchItem, WatchItemDraft, WatchItemStatus, WatchPriority, WatchTarget,
    evaluate_watch_health,
};

use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkflowError {
    #[error("focus scope is invalid")]
    InvalidFocusScope,
    #[error("workflow request is invalid")]
    InvalidRequest,
    #[error("workflow exceeded an exact numeric bound")]
    NumericOverflow,
    #[error("market selection could not construct a decision index")]
    InvalidMarketSelection,
    #[error("workflow was cancelled")]
    Cancelled,
}
