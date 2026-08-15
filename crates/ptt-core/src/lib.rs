//! Platform-free domain core for POE Trade Tracker.
//!
//! Carries the exact base-10 decimal and the text canonicalization/matching
//! machinery (consumed by the CTC target-support OCR path), plus the small
//! shared identity types every other crate agrees on: which game, which
//! content language, capture timestamps, and order-book signatures.

pub mod decimal;
pub mod matching;
pub mod types;

pub use decimal::{Decimal, DecimalParseError};
pub use matching::{
    AffixToken, AffixTokenKind, CanonicalAffix, FullLineAffixMatcher, LogicalAffixMatch,
    canonicalize, extract_values, normalize_numeric_presentation,
};
pub use types::{
    BookSignature, CaptureTimestamp, ContentLanguage, Game, ProfileId, SignatureHasher, fnv1a64,
};
