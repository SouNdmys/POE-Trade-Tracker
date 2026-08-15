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
            // Corpus: first available crop top is 60; competing follows the
            // available rows at a ~62px visual gap wherever they end.
            first_row_top: (45, 80),
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

/// Classifies bands (sorted by top, as the detector emits them) into the
/// available/competing tables. Every band must classify; any stray shape
/// rejects the frame — skipping is free, guessing is not.
pub fn classify_rows(bands: &[BandGeometry], layout: &RowLayout) -> Result<RowPlan, RowsReject> {
    if bands.is_empty() {
        return Err(RowsReject::NoBands);
    }

    let mut estimated = Vec::with_capacity(bands.len());
    for band in bands {
        let rows = layout
            .estimated_rows(band.height)
            .ok_or(RowsReject::ImplausibleBand {
                top: band.top,
                height: band.height,
            })?;
        estimated.push((band, rows));
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
        if *counter > layout.max_rows_per_side {
            return Err(RowsReject::TooManyRows {
                side,
                rows: *counter,
            });
        }
        plan.bands.push(RowBand {
            side,
            band: **band,
            estimated_rows: *rows,
        });
        // Restore the nominal count so downstream row indices stay stable.
        *counter = *counter - minimum_rows + rows;
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

    #[test]
    fn junk_shapes_reject_the_frame() {
        let mut bands = normal_frame();
        bands.push(band(560, 20));
        assert!(matches!(
            classify_rows(&bands, &RowLayout::default()),
            Err(RowsReject::ImplausibleBand { height: 20, .. })
        ));

        let negative = vec![band(303, 34), band(354, 40)];
        // 34 is below single_min → implausible.
        assert!(matches!(
            classify_rows(&negative, &RowLayout::default()),
            Err(RowsReject::ImplausibleBand { .. })
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
