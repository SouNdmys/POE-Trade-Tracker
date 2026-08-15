//! Per-profile field recognition for POE Trade Tracker.
//!
//! Layering: `fields` (strict cell grammars) and `rows` (geometric band
//! classification) and `book` (assembly + signature) are platform-free and
//! fully unit-tested; `profiles` binds them to real OCR backends per
//! game × language. The route-wide contract is fail-skip: any unexplained
//! shape or failed grammar skips the frame with a typed reason.

pub mod book;
pub mod fields;
pub mod identity;
pub mod profiles;
pub mod rows;

pub use book::{BookIdentity, BookObservation, RowObservation, compute_signature};
pub use fields::{Comparator, FieldReject, RatioField, parse_ratio, parse_stock, split_row_line};
pub use identity::{resolve_en_name, resolve_zh_name};
pub use rows::{BandGeometry, RowBand, RowLayout, RowPlan, RowsReject, Side, classify_rows};
