// Each test binary compiles this module separately, so a builder method used
// by one binary looks unused to the others.
#![allow(dead_code)]

//! Shared quote-edge builder for the strategy tests.
//!
//! Building an `EvaluatedQuoteEdge` by hand is twenty lines of plumbing that
//! says nothing about the case under test, so it lives here once and the
//! tests say only what they vary.

use chrono::{DateTime, TimeZone, Utc};
use ptt_market_book::{EvaluatedQuoteEdge, FreshnessAssessment, FreshnessStatus};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};

#[must_use]
pub fn asset(id: &str) -> MarketAssetId {
    MarketAssetId::try_new(id).expect("asset id")
}

#[must_use]
pub fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 16, 10, minute, 0)
        .single()
        .expect("timestamp")
}

pub struct EdgeSpec {
    id: String,
    from: String,
    to: String,
    /// Rate as `numerator / 100`, so 1033 reads as 10.33.
    rate_hundredths: u64,
    stock: u64,
    minute: u32,
    execution: ExecutionType,
    freshness: FreshnessStatus,
    future: bool,
}

impl EdgeSpec {
    #[must_use]
    pub fn new(id: &str, rate_hundredths: u64, minute: u32) -> Self {
        Self {
            id: id.to_owned(),
            from: "divine-orb".to_owned(),
            to: "chaos-orb".to_owned(),
            rate_hundredths,
            stock: 1_000,
            minute,
            execution: ExecutionType::Taker,
            freshness: FreshnessStatus::Fresh,
            future: false,
        }
    }

    #[must_use]
    pub fn between(mut self, from: &str, to: &str) -> Self {
        self.id = format!("{from}->{to}");
        self.from = from.to_owned();
        self.to = to.to_owned();
        self
    }

    #[must_use]
    pub fn maker(mut self) -> Self {
        self.execution = ExecutionType::MakerReference;
        self
    }

    #[must_use]
    pub fn reversed(mut self) -> Self {
        std::mem::swap(&mut self.from, &mut self.to);
        self
    }

    #[must_use]
    pub fn stock(mut self, stock: u64) -> Self {
        self.stock = stock;
        self
    }

    #[must_use]
    pub fn freshness(mut self, freshness: FreshnessStatus) -> Self {
        self.freshness = freshness;
        self
    }

    #[must_use]
    pub fn ahead_of_the_clock(mut self) -> Self {
        self.future = true;
        self
    }

    #[must_use]
    pub fn build(self) -> EvaluatedQuoteEdge {
        let taker = self.execution == ExecutionType::Taker;
        EvaluatedQuoteEdge {
            observation: MarketEdgeObservation {
                edge: QuoteEdge {
                    edge_id: self.id.clone(),
                    snapshot_id: format!("snapshot-{}", self.minute),
                    quote_id: format!("quote-{}", self.id),
                    context_key: "test-context".to_owned(),
                    from_asset_id: asset(&self.from),
                    to_asset_id: asset(&self.to),
                    rate: Ratio::from_parts(self.rate_hundredths, 100).expect("rate"),
                    source_side: if taker {
                        QuoteSide::Available
                    } else {
                        QuoteSide::Competing
                    },
                    execution_type: self.execution,
                    role: if taker {
                        QuoteEdgeRole::AvailableTaker
                    } else {
                        QuoteEdgeRole::CompetingMakerReference
                    },
                    stock: self.stock,
                    original_need_asset_id: asset(&self.to),
                    original_have_asset_id: asset(&self.from),
                    original_row_index: 0,
                    comparator: Comparator::Exact,
                    user_edited: false,
                    machine_confidence_ppm: Some(990_000),
                    captured_at: at(self.minute),
                    confirmed_at: at(self.minute),
                },
                snapshot_complete: true,
                record_status: SnapshotRecordStatus::Active,
                record_revision: 1,
                record_reason: None,
            },
            freshness: FreshnessAssessment {
                status: self.freshness,
                age_seconds: 60,
                future_timestamp: self.future,
            },
            effective_confidence_ppm: 990_000,
            risk_flags: Vec::new(),
            selection_rejections: Vec::new(),
            execution_blockers: Vec::new(),
            accepted_for_selection: true,
            eligible_for_depth_analysis: true,
        }
    }
}
