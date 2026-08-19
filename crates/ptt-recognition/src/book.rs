//! Order-book assembly: recognized fields → one `BookObservation` with a
//! deterministic content signature for monitor-loop deduplication.

use std::cmp::Ordering;

use ptt_core::{BookSignature, CaptureTimestamp, Decimal, SignatureHasher};

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

/// A row that contradicts the row above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutOfOrder {
    pub side: Side,
    /// The row that broke the ordering, and the one it broke it against.
    pub row_index: u8,
    pub previous_index: u8,
    pub previous: String,
    pub current: String,
}

/// Finds the first row that is priced better than the row above it.
///
/// Each table in the panel is sorted: the best offer a taker can hit is at the
/// top, and every row below is worse. That is a property of the widget, not an
/// assumption about the market, so a row that breaks it did not come from the
/// panel — it came from a misread of one.
///
/// This is the only check that catches a ratio which parses cleanly and is
/// simply wrong, and grammar cannot substitute for it: `9950` is a well-formed
/// ratio and so is `9.9`. Both were accepted at 0.95 confidence in a live
/// session — they were `99.50` with its decimal point dropped and `199.90`
/// with its leading digits lost — and both sat between neighbours they
/// contradict. Eight of fifty-eight tables in that session held one.
///
/// The aggregate row repeats the rate above it, which is equal rather than
/// better, so it passes by construction.
#[must_use]
pub fn out_of_order(rows: &[RowObservation]) -> Option<OutOfOrder> {
    for side in [Side::Available, Side::Competing] {
        let mut ordered: Vec<&RowObservation> =
            rows.iter().filter(|row| row.side == side).collect();
        ordered.sort_by_key(|row| row.row_index);
        for pair in ordered.windows(2) {
            let (above, below) = (pair[0], pair[1]);
            // `None` means the comparison overflowed, which is not evidence of
            // anything; declining to judge beats inventing a violation.
            let Some(order) = compare_rates(&above.ratio, &below.ratio) else {
                continue;
            };
            let broken = match side {
                // Available lists what you can buy, cheapest first, so the
                // rate a taker pays only rises.
                Side::Available => order == Ordering::Less,
                // Competing lists what others are asking, and it runs the
                // other way for the same reason.
                Side::Competing => order == Ordering::Greater,
            };
            if broken {
                return Some(OutOfOrder {
                    side,
                    row_index: below.row_index,
                    previous_index: above.row_index,
                    previous: above.ratio.normalized.clone(),
                    current: below.ratio.normalized.clone(),
                });
            }
        }
    }
    None
}

/// Orders two rates exactly, as rationals.
///
/// Cross-multiplied rather than divided: these are decimals with a scale, and
/// turning them into a quotient would either lose the difference between
/// `199.90` and `199.9` or introduce a float into a layer that has none.
fn compare_rates(left: &RatioField, right: &RatioField) -> Option<Ordering> {
    let (left_num, left_den) = as_fraction(left)?;
    let (right_num, right_den) = as_fraction(right)?;
    // Both denominators are panel quantities and therefore positive, so the
    // inequality survives multiplying through by them.
    if left_den <= 0 || right_den <= 0 {
        return None;
    }
    let lhs = left_num.checked_mul(right_den)?;
    let rhs = right_num.checked_mul(left_den)?;
    Some(lhs.cmp(&rhs))
}

/// A ratio as a whole-number fraction: `left / right` with the scales cleared.
fn as_fraction(ratio: &RatioField) -> Option<(i128, i128)> {
    let lift = |value: Decimal, by: u32| -> Option<i128> {
        value.coefficient().checked_mul(10i128.checked_pow(by)?)
    };
    Some((
        lift(ratio.left, ratio.right.scale())?,
        lift(ratio.right, ratio.left.scale())?,
    ))
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

    fn row(side: Side, index: u8, ratio: &str) -> RowObservation {
        RowObservation {
            side,
            row_index: index,
            ratio: parse_ratio(ratio).unwrap_or_else(|_| panic!("{ratio} should parse")),
            stock: 1,
            band_fingerprint: 0,
        }
    }

    /// A well-ordered panel passes, in both orientations.
    ///
    /// The two tables run opposite ways, and the panel writes the rate with
    /// the `1` on whichever side suits the pair — `197:1` for one direction
    /// and `1:894` for the other — so a check that only understood one of
    /// those would reject half of all real books.
    #[test]
    fn a_book_in_panel_order_is_accepted() {
        let ones_on_the_right = vec![
            row(Side::Available, 0, "1:893"),
            row(Side::Available, 1, "1:894"),
            row(Side::Available, 2, "1:899"),
            // The aggregate repeats the rate above it.
            row(Side::Available, 3, "1:899"),
            row(Side::Competing, 0, "1:888"),
            row(Side::Competing, 1, "1:885"),
            row(Side::Competing, 2, "1:880"),
        ];
        assert_eq!(out_of_order(&ones_on_the_right), None);

        let ones_on_the_left = vec![
            row(Side::Available, 0, "198:1"),
            row(Side::Available, 1, "197.25:1"),
            row(Side::Available, 2, "196.8:1"),
            row(Side::Competing, 0, "199:1"),
            row(Side::Competing, 1, "199.5:1"),
            row(Side::Competing, 2, "199.9:1"),
        ];
        assert_eq!(out_of_order(&ones_on_the_left), None);
    }

    /// The misreads a live session actually accepted.
    ///
    /// Every one of these is a real row from a real capture, written into the
    /// store at 0.95 confidence with nothing objecting. They are here as
    /// literals because that is what they were: values that parse, that pass
    /// the grammar, and that only a neighbour can contradict.
    #[test]
    fn the_misreads_that_reached_the_store_are_now_refused() {
        // `199.90` with its leading digits gone.
        let dropped_leading = vec![
            row(Side::Competing, 0, "199:1"),
            row(Side::Competing, 1, "199.5:1"),
            row(Side::Competing, 2, "199.8:1"),
            row(Side::Competing, 3, "9.9:1"),
        ];
        let broken = out_of_order(&dropped_leading).expect("9.9 contradicts 199.8");
        assert_eq!(broken.side, Side::Competing);
        assert_eq!(broken.row_index, 3);
        assert_eq!(broken.previous, "199.8:1");

        // `99.50` with its decimal point gone.
        let dropped_point = vec![
            row(Side::Competing, 2, "9950:1"),
            row(Side::Competing, 3, "101:1"),
        ];
        assert!(out_of_order(&dropped_point).is_some(), "9950 then 101");

        // `197.25` with its decimal point gone.
        let run_together = vec![
            row(Side::Available, 0, "198:1"),
            row(Side::Available, 1, "197125:1"),
        ];
        assert!(out_of_order(&run_together).is_some(), "197125 after 198");

        // A competing table that simply runs backwards.
        let backwards = vec![
            row(Side::Competing, 0, "100:1"),
            row(Side::Competing, 1, "99:1"),
        ];
        assert!(out_of_order(&backwards).is_some(), "99 after 100");
    }

    /// Row index decides the order, not position in the vector.
    ///
    /// Rows arrive grouped by side, and a book with a skipped row leaves gaps,
    /// so neither their order in the slice nor their contiguity can be assumed.
    #[test]
    fn ordering_follows_the_row_index_through_gaps_and_shuffling() {
        let shuffled = vec![
            row(Side::Competing, 3, "9.9:1"),
            row(Side::Available, 2, "196.8:1"),
            row(Side::Competing, 0, "199:1"),
            row(Side::Available, 0, "198:1"),
        ];
        let broken = out_of_order(&shuffled).expect("the competing pair is still adjacent");
        assert_eq!(broken.side, Side::Competing);
        assert_eq!(broken.previous_index, 0);
        assert_eq!(broken.row_index, 3);
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
