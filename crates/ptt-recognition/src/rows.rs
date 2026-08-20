//! Pure geometric classification of detected bands into order-book rows.
//!
//! Input is the band list from `ptt-vision` over the tables ROI (crop rects
//! are padded by the detector's vertical margin). Output is a two-table row
//! plan, or a typed rejection that skips the frame. Wrapped-decimal rows
//! arrive as merged bands; they are flagged with an estimated logical row
//! count and split later against OCR line boxes.

/// One detected band in tables-ROI coordinates (padded crop rect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandGeometry {
    pub top: i32,
    pub height: u32,
    pub left: i32,
    pub width: u32,
    pub content_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Available,
    Competing,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Available => "available",
            Side::Competing => "competing",
        }
    }
}

/// A band assigned to a table with an estimated logical-row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowBand {
    pub side: Side,
    pub band: BandGeometry,
    /// 1 for a normal band; 2+ when wrapped text merged neighbouring rows.
    pub estimated_rows: u8,
    /// Where this band sits in its table, counted from the top.
    ///
    /// Derived from the band's position, not from counting the bands before
    /// it. Counting only works while every band is present, and the whole
    /// point of dropping an unreadable one is that it is not — a counted
    /// index would silently move every row below it up by one, which the
    /// aggregate-row and row-order checks then read as a different panel.
    pub row_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPlan {
    pub bands: Vec<RowBand>,
    pub available_rows: u8,
    pub competing_rows: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowsReject {
    NoBands,
    /// A band's shape matches neither a single row nor a plausible merge.
    ImplausibleBand {
        top: i32,
        height: u32,
    },
    /// The topmost band does not start where the available table's first
    /// row lives — leading junk or partial detection; sides can't be trusted.
    LeadOutOfWindow {
        top: i32,
    },
    /// Zero or one inter-table gap is interpretable; more is not.
    TableSplitAmbiguous {
        gaps: usize,
    },
    /// More logical rows than the game can display on one side.
    /// Two bands landed on the same slot, so the pitch did not describe this
    /// frame. See [`classify_rows`].
    AmbiguousRowIndex {
        side: Side,
        row_index: u8,
    },
    TooManyRows {
        side: Side,
        rows: u8,
    },
}

/// Geometry for the poe2 2560x1440 profile, in padded-crop terms
/// (detector vertical margin 14 on each side, text height ~17, pitch 31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowLayout {
    pub single_height: (u32, u32),
    pub merged_height_max: u32,
    pub row_pitch: u32,
    /// Window the topmost band must start in. The available table is
    /// anchored at the popup top even though the competing table floats up
    /// when the available side has fewer rows, so this guard pins the split
    /// search to a trustworthy origin: partial detection that loses leading
    /// rows (the live-session mislabel bug) now rejects the frame instead of
    /// shifting rows across tables.
    pub first_row_top: (i32, i32),
    /// Minimum bottom-to-top distance that separates the two tables.
    pub table_gap_min: i32,
    pub max_rows_per_side: u8,
}

impl Default for RowLayout {
    fn default() -> Self {
        Self {
            single_height: (38, 52),
            merged_height_max: 130,
            row_pitch: 31,
            // The guard's job is rejecting frames whose LEADING rows went
            // undetected (top jumps by a pitch multiple). Preset ROI puts the
            // first crop top at 60; a user-calibrated tight box puts it near
            // the detector margin (~5-20). (0, 90) accepts both while a
            // missing first row (top >= ~107 in either geometry) still fails.
            first_row_top: (0, 90),
            table_gap_min: 40,
            max_rows_per_side: 6,
        }
    }
}

impl RowLayout {
    fn estimated_rows(&self, height: u32) -> Option<u8> {
        let (single_min, single_max) = self.single_height;
        if height < single_min {
            return None;
        }
        if height <= single_max {
            return Some(1);
        }
        if height > self.merged_height_max {
            return None;
        }
        // A single crop is text (pitch - gap) plus detector padding; each
        // extra merged row adds one pitch: h ≈ padding + rows * pitch.
        let padding = (single_min + single_max) / 2 - self.row_pitch;
        let rows = (height - padding + self.row_pitch / 2) / self.row_pitch;
        u8::try_from(rows.max(1)).ok()
    }
}

/// Whether a band's height matches some whole number of rows.
///
/// The predicate `classify_rows` drops on, exposed so a caller carrying a
/// parallel array — the detector's crops — can drop the same elements before
/// classifying rather than after. Filtering both lists through one call keeps
/// them index-aligned by construction, which is not a property worth trusting
/// to two separate filters: crops read out of a misaligned array OCR one row's
/// rectangle as another row's text.
#[must_use]
pub fn classifiable(band: &BandGeometry, layout: &RowLayout) -> bool {
    layout.estimated_rows(band.height).is_some()
}

/// Classifies bands (sorted by top, as the detector emits them) into the
/// available/competing tables.
///
/// A band whose height matches no row count is dropped, not fatal. It used to
/// reject the frame, on the reasoning that skipping is free and guessing is
/// not — which is right about guessing and wrong about the cost: a wrapped row
/// leaves a stub of about 34px, well under a row's 38, and one stub was taking
/// eleven good rows down with it. Dropping the stub keeps the choice between
/// skipping and guessing exactly where it was, at the level of a row rather
/// than a frame.
///
/// What makes that safe is that positions, not sequence, decide row indices,
/// so removing a band cannot renumber the ones below it.
pub fn classify_rows(bands: &[BandGeometry], layout: &RowLayout) -> Result<RowPlan, RowsReject> {
    if bands.is_empty() {
        return Err(RowsReject::NoBands);
    }

    let mut estimated = Vec::with_capacity(bands.len());
    for band in bands {
        // `None` means no whole number of rows fits this height. That is a
        // fragment -- most often the tail of a rate the panel wrapped -- and
        // it is dropped rather than interpreted.
        if let Some(rows) = layout.estimated_rows(band.height) {
            estimated.push((band, rows));
        }
    }
    if estimated.is_empty() {
        return Err(RowsReject::NoBands);
    }

    let first_top = estimated[0].0.top;
    if first_top < layout.first_row_top.0 || first_top > layout.first_row_top.1 {
        return Err(RowsReject::LeadOutOfWindow { top: first_top });
    }
    let mut gaps = Vec::new();
    for index in 1..estimated.len() {
        let previous = estimated[index - 1].0;
        let gap = estimated[index].0.top - (previous.top + previous.height as i32);
        if gap >= layout.table_gap_min {
            gaps.push(index);
        }
    }
    if gaps.len() > 1 {
        return Err(RowsReject::TableSplitAmbiguous { gaps: gaps.len() });
    }
    let split = gaps.first().copied().unwrap_or(estimated.len());

    let mut plan = RowPlan {
        bands: Vec::with_capacity(estimated.len()),
        available_rows: 0,
        competing_rows: 0,
    };
    for (index, (band, rows)) in estimated.iter().enumerate() {
        let side = if index < split {
            Side::Available
        } else {
            Side::Competing
        };
        let counter = match side {
            Side::Available => &mut plan.available_rows,
            Side::Competing => &mut plan.competing_rows,
        };
        // A merged band's row count is an estimate: a wrapped decimal makes
        // one logical row span two visual lines, so the true count may be one
        // lower. The capacity check uses the minimum so a wrap near a full
        // table cannot reject the whole frame.
        let minimum_rows = if *rows > 1 { *rows - 1 } else { *rows };
        *counter += minimum_rows;
        // The index this band would have if its table began at `origin` and
        // every row were one pitch apart -- which is how the panel draws them.
        // Rounded, because a band's top carries the detector's own margin.
        let origin = match side {
            Side::Available => estimated[0].0.top,
            Side::Competing => estimated[split.min(estimated.len() - 1)].0.top,
        };
        let offset = band.top - origin;
        let pitch = i32::try_from(layout.row_pitch).unwrap_or(1).max(1);
        let row_index = u8::try_from((offset + pitch / 2).max(0) / pitch).unwrap_or(u8::MAX);
        plan.bands.push(RowBand {
            side,
            band: **band,
            estimated_rows: *rows,
            row_index,
        });
        // Restore the nominal count so the capacity check keeps counting rows
        // rather than bands.
        *counter = *counter - minimum_rows + rows;
    }
    // A row past the last slot the panel has. Counted by position rather than
    // by adding up bands: a rate the panel wrapped leaves a stub between two
    // rows, and counting bands charges that stub as a row, so a full table
    // with one wrap came to seven and the frame was refused for holding more
    // rows than it has. No real row moved -- there were still six, with
    // something in between them.
    for row in &plan.bands {
        if row.row_index >= layout.max_rows_per_side {
            return Err(RowsReject::TooManyRows {
                side: row.side,
                rows: row.row_index + 1,
            });
        }
    }

    // Two bands cannot occupy one slot. When they do, the arithmetic that
    // produced the indices did not describe this frame -- a merged blob of two
    // rows shifts everything after it off the pitch -- and a silent collision
    // would have one band overwrite the other's rank. Rejecting is the same
    // choice the rest of this function makes, applied to the one failure the
    // positional scheme can have.
    for side in [Side::Available, Side::Competing] {
        let mut taken = Vec::new();
        for row in plan.bands.iter().filter(|row| row.side == side) {
            if taken.contains(&row.row_index) {
                return Err(RowsReject::AmbiguousRowIndex {
                    side,
                    row_index: row.row_index,
                });
            }
            taken.push(row.row_index);
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(top: i32, height: u32) -> BandGeometry {
        BandGeometry {
            top,
            height,
            left: 42,
            width: 200,
            content_fingerprint: top as u64,
        }
    }

    /// The exact geometry the probe reported for a normal frame.
    fn normal_frame() -> Vec<BandGeometry> {
        [60, 91, 121, 152, 183, 213]
            .into_iter()
            .chain([321, 352, 382, 413, 444, 474])
            .map(|top| band(top, 45))
            .collect()
    }

    /// A wrapped rate leaves a stub, and the stub costs one row, not twelve.
    ///
    /// The panel splits a long rate across two lines; the tail lands as a band
    /// far under a row's height. That used to reject the frame outright, which
    /// is how one unreadable row took eleven good ones with it.
    #[test]
    fn a_fragment_costs_its_own_row_and_no_others() {
        let mut bands = normal_frame();
        // A 34px stub between the third and fourth available rows, which is
        // what the detector reported on a real frame.
        bands.push(band(140, 34));
        bands.sort_by_key(|band| band.top);

        let plan = classify_rows(&bands, &RowLayout::default()).expect("the frame survives");
        assert_eq!(plan.available_rows, 6, "{plan:#?}");
        assert_eq!(plan.competing_rows, 6, "{plan:#?}");
        assert!(
            plan.bands.iter().all(|row| row.band.height != 34),
            "the stub was kept: {plan:#?}"
        );
    }

    /// Dropping a band must not renumber the rows below it.
    #[test]
    fn row_indices_come_from_position_not_from_counting() {
        let full = classify_rows(&normal_frame(), &RowLayout::default()).expect("plan");
        let indices: Vec<(Side, u8)> = full
            .bands
            .iter()
            .map(|row| (row.side, row.row_index))
            .collect();
        assert_eq!(
            indices,
            [
                (Side::Available, 0),
                (Side::Available, 1),
                (Side::Available, 2),
                (Side::Available, 3),
                (Side::Available, 4),
                (Side::Available, 5),
                (Side::Competing, 0),
                (Side::Competing, 1),
                (Side::Competing, 2),
                (Side::Competing, 3),
                (Side::Competing, 4),
                (Side::Competing, 5),
            ]
        );

        // The same frame with a stub spliced in: every real row keeps its rank.
        let mut with_stub = normal_frame();
        with_stub.push(band(140, 34));
        with_stub.sort_by_key(|band| band.top);
        let patched = classify_rows(&with_stub, &RowLayout::default()).expect("plan");
        let patched_indices: Vec<(Side, u8)> = patched
            .bands
            .iter()
            .map(|row| (row.side, row.row_index))
            .collect();
        assert_eq!(patched_indices, indices);
    }

    /// Two bands on one slot means the pitch did not describe the frame.
    #[test]
    fn colliding_row_indices_reject_the_frame() {
        // Six available rows where two sit barely a third of a pitch apart:
        // whatever this frame is, it is not six evenly spaced rows.
        let bands: Vec<BandGeometry> = [60, 91, 121, 131, 183, 213]
            .into_iter()
            .chain([321, 352, 382, 413, 444, 474])
            .map(|top| band(top, 45))
            .collect();
        assert!(matches!(
            classify_rows(&bands, &RowLayout::default()),
            Err(RowsReject::AmbiguousRowIndex { .. })
        ));
    }

    #[test]
    fn normal_frame_classifies_six_and_six() {
        let plan = classify_rows(&normal_frame(), &RowLayout::default()).unwrap();
        assert_eq!(plan.available_rows, 6);
        assert_eq!(plan.competing_rows, 6);
        assert!(plan.bands[..6].iter().all(|b| b.side == Side::Available));
        assert!(plan.bands[6..].iter().all(|b| b.side == Side::Competing));
        assert!(plan.bands.iter().all(|b| b.estimated_rows == 1));
    }

    #[test]
    fn wrapped_frame_merged_bands_estimate_extra_rows() {
        // From the 04.58.50 corpus shot: h=110 merges three logical rows,
        // h=78 merges two.
        let bands = vec![
            band(60, 45),
            band(91, 45),
            band(121, 45),
            band(152, 110),
            band(321, 45),
            band(352, 78),
            band(413, 45),
            band(444, 45),
            band(474, 45),
        ];
        let plan = classify_rows(&bands, &RowLayout::default()).unwrap();
        assert_eq!(plan.available_rows, 6, "3 singles + one triple merge");
        assert_eq!(plan.competing_rows, 6, "4 singles + one double merge");
        assert_eq!(plan.bands[3].estimated_rows, 3);
        assert_eq!(plan.bands[5].estimated_rows, 2);
    }

    /// Junk is dropped; a frame that is only junk is still refused.
    ///
    /// This used to assert that any junk band rejected the frame. That was the
    /// old contract and it was too expensive: the shapes this drops are mostly
    /// wrap stubs sitting between perfectly readable rows. What has to survive
    /// is the other half of it — junk must never be read *as* a row, and a
    /// frame with nothing else in it must not come back empty-handed but
    /// successful.
    #[test]
    fn junk_shapes_are_dropped_rather_than_read() {
        let mut bands = normal_frame();
        bands.push(band(560, 20));
        let plan = classify_rows(&bands, &RowLayout::default()).expect("the real rows survive");
        assert_eq!(plan.available_rows, 6);
        assert_eq!(plan.competing_rows, 6);
        assert!(
            plan.bands.iter().all(|row| row.band.height != 20),
            "junk was read as a row: {plan:#?}"
        );

        // Nothing but junk: 34 and 20 are both under a row's height, so there
        // is no frame left once they are dropped.
        assert!(matches!(
            classify_rows(&[band(303, 34), band(354, 20)], &RowLayout::default()),
            Err(RowsReject::NoBands)
        ));
    }

    #[test]
    fn short_available_table_shifts_competing_up_and_still_splits() {
        // With four available rows the competing table floats up; the gap
        // split must follow it (a fixed y-threshold mislabeled these).
        let bands: Vec<BandGeometry> = [60, 91, 121, 152]
            .into_iter()
            .chain([246, 277, 308, 339, 370, 401])
            .map(|t| band(t, 45))
            .collect();
        let plan = classify_rows(&bands, &RowLayout::default()).unwrap();
        assert_eq!(plan.available_rows, 4);
        assert_eq!(plan.competing_rows, 6);
    }

    #[test]
    fn partial_leading_detection_rejects_instead_of_shifting_sides() {
        // Missing early available rows (the live-session mislabel bug):
        // the top anchor window rejects the frame outright.
        let partial: Vec<BandGeometry> = [152, 183, 213, 321, 352]
            .into_iter()
            .map(|t| band(t, 45))
            .collect();
        assert!(matches!(
            classify_rows(&partial, &RowLayout::default()),
            Err(RowsReject::LeadOutOfWindow { top: 152 })
        ));

        // A one-sided detection anchored correctly stays one-sided.
        let one_table: Vec<BandGeometry> = [60, 91, 121].into_iter().map(|t| band(t, 45)).collect();
        let plan = classify_rows(&one_table, &RowLayout::default()).unwrap();
        assert_eq!(plan.available_rows, 3);
        assert_eq!(plan.competing_rows, 0);
    }

    #[test]
    fn overfull_side_rejects() {
        let bands: Vec<BandGeometry> = (0..7).map(|i| band(60 + i * 31, 45)).collect();
        assert!(matches!(
            classify_rows(&bands, &RowLayout::default()),
            Err(RowsReject::TooManyRows {
                side: Side::Available,
                ..
            })
        ));
    }
}
