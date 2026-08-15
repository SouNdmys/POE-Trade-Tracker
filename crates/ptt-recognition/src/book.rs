//! Order-book assembly: recognized fields → one `BookObservation` with a
//! deterministic content signature for monitor-loop deduplication.

use ptt_core::{BookSignature, CaptureTimestamp, SignatureHasher};

use crate::fields::RatioField;
use crate::rows::Side;

/// One fully-accepted order row. Every field carries provenance back to the
/// band crop it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowObservation {
    pub side: Side,
    /// 0-based position within its table, top to bottom.
    pub row_index: u8,
    pub ratio: RatioField,
    pub stock: u64,
    pub band_fingerprint: u64,
}

/// Resolved need/have identities (catalog asset ids, never raw OCR strings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookIdentity {
    pub need_asset_id: String,
    pub have_asset_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookObservation {
    pub identity: BookIdentity,
    pub rows: Vec<RowObservation>,
    pub signature: BookSignature,
    pub captured: CaptureTimestamp,
}

impl BookObservation {
    pub fn assemble(
        identity: BookIdentity,
        rows: Vec<RowObservation>,
        captured: CaptureTimestamp,
    ) -> Self {
        let signature = compute_signature(&identity, &rows);
        Self {
            identity,
            rows,
            signature,
            captured,
        }
    }
}

/// Deterministic content signature: pair identity plus every row's side,
/// position, normalized ratio text, and stock. Equal signatures mean the same
/// book is still displayed — the monitor loop dedupes on this.
pub fn compute_signature(identity: &BookIdentity, rows: &[RowObservation]) -> BookSignature {
    let mut hasher = SignatureHasher::new();
    hasher.write_str(&identity.need_asset_id);
    hasher.write_str(&identity.have_asset_id);
    for row in rows {
        hasher.write_u64(match row.side {
            Side::Available => 1,
            Side::Competing => 2,
        });
        hasher.write_u64(u64::from(row.row_index));
        hasher.write_str(&row.ratio.normalized);
        hasher.write_u64(row.stock);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::parse_ratio;

    fn observation(stock: u64) -> RowObservation {
        RowObservation {
            side: Side::Available,
            row_index: 0,
            ratio: parse_ratio("1:9.80").unwrap(),
            stock,
            band_fingerprint: 7,
        }
    }

    fn identity() -> BookIdentity {
        BookIdentity {
            need_asset_id: "divine_orb".into(),
            have_asset_id: "chaos_orb".into(),
        }
    }

    #[test]
    fn signature_is_deterministic_and_content_sensitive() {
        let rows = vec![observation(920)];
        let first = compute_signature(&identity(), &rows);
        let second = compute_signature(&identity(), &rows);
        assert_eq!(first, second);

        let changed_stock = vec![observation(921)];
        assert_ne!(first, compute_signature(&identity(), &changed_stock));

        let swapped = BookIdentity {
            need_asset_id: "chaos_orb".into(),
            have_asset_id: "divine_orb".into(),
        };
        assert_ne!(first, compute_signature(&swapped, &rows));
    }
}
