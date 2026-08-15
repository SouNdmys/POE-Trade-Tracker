mod depth;
mod quantity;
mod route;
mod triangle;

pub use depth::{
    CaptureTimeEvidence, ExecutionRiskFlag, FillKind, MarketDepthIndex, PairBottleneck, PairFill,
    QuoteLevelFill,
};
pub use quantity::{AssetAmount, AssetUnit, AssetUnitCatalog, FeePolicy};
pub use route::{
    ComparisonDirection, ConversionComparison, ConversionComparisonStatus, ConversionDiagnostics,
    ConversionPath, ConversionRequest, ConversionResult, ResidualAmount, SearchCancellation,
    find_best_conversion,
};
pub use triangle::{
    ProfitKind, TriangleDiagnostics, TriangleEvaluation, TriangleEvaluationStatus, TriangleRequest,
    TriangleResult, canonical_cycle_key, find_triangle_opportunities,
};

use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EngineError {
    #[error("asset minimum unit must be a strict positive rational")]
    InvalidAssetUnit,
    #[error("asset unit catalog contains a duplicate or is missing an asset")]
    InvalidAssetUnitCatalog,
    #[error("amount is not aligned to the asset minimum unit")]
    AmountNotAligned,
    #[error("amount must be positive for this operation")]
    NonPositiveAmount,
    #[error("exact trade arithmetic exceeded its bounded integer range")]
    NumericOverflow,
    #[error("fee policy is invalid")]
    InvalidFeePolicy,
    #[error("quote selection cannot construct an executable depth index")]
    InvalidQuoteSelection,
    #[error("requested assets or strategy do not match the depth index")]
    InvalidAnalysisRequest,
    #[error("search limits are invalid")]
    InvalidSearchLimits,
    #[error("analysis was cancelled")]
    Cancelled,
}
