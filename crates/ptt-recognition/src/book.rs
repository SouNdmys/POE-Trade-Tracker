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

/// Rows that contradict the panel's ordering, as `(side, row_index)`.
///
/// Each table in the panel is sorted: the best offer a taker can hit is at the
/// top, and every row below is worse. That is a property of the widget, not an
/// assumption about the market, so a row that breaks it did not come from the
/// panel — it came from a misread of one.
///
/// This is the only check that catches a rate which parses cleanly and is
/// simply wrong, and grammar cannot substitute for it: `9950` is a well-formed
/// ratio and so is `9.9`. Both were accepted at 0.95 confidence in a live
/// session — `99.50` with its decimal point dropped and `199.90` with its
/// leading digits lost — and both sat between neighbours they contradict.
///
/// It names rows rather than condemning the frame. A misread row is a misread
/// row; the eleven around it were read correctly and the panel still says what
/// it says. Rejecting the book for one bad rate is the same mistake as
/// rejecting it for one unreadable band, one row at a time being the whole
/// point of a fail-skip design.
///
/// Which row is wrong is decided, not guessed. In a monotone sequence a single
/// bad value shows up as the one whose neighbours agree with each other across
/// it. When they do not — two adjacent rows each consistent with their far
/// side — nothing here can tell which is the misread, and both go.
#[must_use]
pub fn out_of_order_rows(rows: &[RowObservation]) -> Vec<(Side, u8)> {
    let mut suspect = Vec::new();
    for side in [Side::Available, Side::Competing] {
        let mut ordered: Vec<&RowObservation> =
            rows.iter().filter(|row| row.side == side).collect();
        ordered.sort_by_key(|row| row.row_index);

        // The aggregate row repeats the rate above it, which is equal rather
        // than better, so equality is in order on both sides.
        let ordered_pair = |above: &RowObservation, below: &RowObservation| {
            compare_rates(&above.ratio, &below.ratio).is_none_or(|order| match side {
                // Available lists what you can buy, cheapest first, so the
                // rate a taker pays only rises.
                Side::Available => order != Ordering::Less,
                // Competing lists what others are asking, and it runs the
                // other way for the same reason.
                Side::Competing => order != Ordering::Greater,
            })
        };

        let mut index = 1;
        while index < ordered.len() {
            if ordered_pair(ordered[index - 1], ordered[index]) {
                index += 1;
                continue;
            }
            // The pair disagrees. Ask the rows on either side of it which of
            // the two is the odd one out.
            // Symmetric to the case below: removing the last row of a table
            // cannot break anything under it, so at the end of the sequence
            // the lower row is always a candidate.
            let without_below = index + 1 == ordered.len()
                || ordered
                    .get(index + 1)
                    .is_some_and(|next| ordered_pair(ordered[index - 1], next));
            // Removing the first row of a table cannot break anything above
            // it, so at the start of the sequence the upper row is always a
            // candidate. Without this the corpus frame whose first rate reads
            // `1:133` for `1 : 1.33` lost the good row under it as well: the
            // misread was the outlier and there was nothing before it to say
            // so.
            let without_above = index == 1 || ordered_pair(ordered[index - 2], ordered[index]);
            match (without_below, without_above) {
                (true, false) => suspect.push((side, ordered[index].row_index)),
                (false, true) => suspect.push((side, ordered[index - 1].row_index)),
                // Either both readings survive or neither does; in both cases
                // the sequence does not say which row to believe.
                _ => {
                    suspect.push((side, ordered[index - 1].row_index));
                    suspect.push((side, ordered[index].row_index));
                }
            }
            index += 1;
        }
    }
    suspect.sort_unstable();
    suspect.dedup();
    suspect
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
        assert!(out_of_order_rows(&ones_on_the_right).is_empty());

        let ones_on_the_left = vec![
            row(Side::Available, 0, "198:1"),
            row(Side::Available, 1, "197.25:1"),
            row(Side::Available, 2, "196.8:1"),
            row(Side::Competing, 0, "199:1"),
            row(Side::Competing, 1, "199.5:1"),
            row(Side::Competing, 2, "199.9:1"),
        ];
        assert!(out_of_order_rows(&ones_on_the_left).is_empty());
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
        // Named, and only it: the rows above agree with each other across it,
        // and being last it is the only row whose removal explains them.
        assert_eq!(
            out_of_order_rows(&dropped_leading),
            vec![(Side::Competing, 3)]
        );

        // `99.50` with its decimal point gone.
        let dropped_point = vec![
            row(Side::Competing, 2, "9950:1"),
            row(Side::Competing, 3, "101:1"),
        ];
        assert!(
            !out_of_order_rows(&dropped_point).is_empty(),
            "9950 then 101"
        );

        // `197.25` with its decimal point gone.
        let run_together = vec![
            row(Side::Available, 0, "198:1"),
            row(Side::Available, 1, "197125:1"),
        ];
        assert!(
            !out_of_order_rows(&run_together).is_empty(),
            "197125 after 198"
        );

        // A competing table that simply runs backwards.
        let backwards = vec![
            row(Side::Competing, 0, "100:1"),
            row(Side::Competing, 1, "99:1"),
        ];
        assert!(!out_of_order_rows(&backwards).is_empty(), "99 after 100");
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
        // Two rows that disagree and nothing else to appeal to: neither can
        // be exonerated, so both go. What matters here is that the pair was
        // found at all -- they are neighbours by row index, not by position in
        // the slice, and are not adjacent in it.
        assert_eq!(
            out_of_order_rows(&shuffled),
            vec![(Side::Competing, 0), (Side::Competing, 3)]
        );
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
