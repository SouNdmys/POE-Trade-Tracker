//! The recognition route: a decoded frame in, a `BookObservation` out.
//!
//! One route serves both games. Everything here — the OCR ladder, the field
//! parsers, the comparator classifier, row planning and book assembly — reads
//! its geometry from a [`crate::profiles::PanelLayout`], so a second game is a
//! second layout value rather than a second copy of the pipeline. This lived in
//! `profiles::poe2` while POE2 was the only caller, which left POE1-only code
//! (the fixed-grid planner) sitting in a file named after the other game.
//!
//! The client language is a parameter too. Panel geometry does not vary with
//! it and the numeric fields are Arabic numerals everywhere, so language picks
//! only which catalog name the identity slots match against and which OCR
//! language reads them.

use crate::fields::FieldReject;
use crate::rows::{RowsReject, Side};

/// Why a frame (or a row within an accepted frame) was not ingested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Decode(String),
    Ocr(String),
    NeedNameUnresolved {
        text: String,
    },
    HaveNameUnresolved {
        text: String,
    },
    Rows(RowsReject),
    /// Identity and structure were fine but not a single row parsed.
    EmptyBook,
}

/// A row that failed inside an otherwise-accepted frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSkip {
    Grammar {
        side: Side,
        row_index: u8,
        reject: FieldReject,
        raw: String,
    },
    /// OCR produced an unexpected line count for a single-row band.
    LineCount {
        side: Side,
        row_index: u8,
        raw: String,
    },
    /// The rate contradicts the rows around it, so it did not come from this
    /// panel. See [`crate::book::out_of_order_rows`].
    OutOfOrder {
        side: Side,
        row_index: u8,
        ratio: String,
    },
    /// Band geometry demands a comparator but OCR read none (or vice
    /// versa); skipped until the template classifier lands.
    ComparatorUnverified {
        side: Side,
        row_index: u8,
        raw: String,
    },
    /// Wrapped-decimal merge; v1 skips its rows and preserves the raw text
    /// so the join heuristic can be built from real evidence.
    MergedBand {
        side: Side,
        first_row_index: u8,
        estimated_rows: u8,
        raw: String,
    },
}

/// Calibration overrides for the three profile regions, keyed by "NEED",
/// "HAVE", "TABLES". Set by the app when the user draws regions; captured
/// once per watch session start (Route::regions()).
type RegionRect = (i32, i32, u32, u32);
type RegionOverrideMap = std::sync::RwLock<std::collections::BTreeMap<String, RegionRect>>;

static REGION_OVERRIDES: std::sync::OnceLock<RegionOverrideMap> = std::sync::OnceLock::new();

fn overrides() -> &'static RegionOverrideMap {
    REGION_OVERRIDES.get_or_init(|| std::sync::RwLock::new(std::collections::BTreeMap::new()))
}

/// Installs an override after validating it as a capturable region; returns
/// false (and installs nothing) for degenerate geometry, so a corrupt
/// settings file can never panic the watch thread.
pub fn set_region_override(prefix: &str, name: &str, region: RegionRect) -> bool {
    if ptt_vision::CaptureRegion::new(region.0, region.1, region.2, region.3).is_err() {
        return false;
    }
    overrides()
        .write()
        .expect("region override lock")
        .insert(override_key(prefix, name), region);
    true
}

#[must_use]
pub fn region_override(prefix: &str, name: &str) -> Option<RegionRect> {
    overrides()
        .read()
        .expect("region override lock")
        .get(&override_key(prefix, name))
        .copied()
}

fn override_key(prefix: &str, name: &str) -> String {
    format!("{prefix}:{name}")
}

#[cfg(windows)]
pub use windows_route::{RecognizedBook, Route};

#[cfg(windows)]
mod windows_route {
    use super::{RowSkip, SkipReason, region_override};
    use crate::book::{BookIdentity, BookObservation, RowObservation};
    use crate::fields::{Comparator, FieldReject, parse_ratio, parse_stock, split_row_line};
    use crate::profiles::ProfileLanguage;
    use crate::rows::{BandGeometry, RowBand, RowPlan, RowsReject, Side, classify_rows};
    use ptt_core::CaptureTimestamp;
    use ptt_ocr_win::{OcrLanguagePreference, OcrWorker, OwnedBgraImage};
    use ptt_vision::{
        BandDetectionSettings, CaptureRegion, PhysicalBandDetector, WarmMaskSettings,
        WicScreenshotDecoder, build_warm_mask,
    };

    /// An accepted frame: the observation plus per-row skips and raw OCR
    /// text retained for probes and the review queue.
    #[derive(Debug, Clone)]
    pub struct RecognizedBook {
        pub observation: BookObservation,
        pub skipped_rows: Vec<RowSkip>,
        pub need_text: String,
        pub have_text: String,
    }

    pub struct Route {
        worker: OcrWorker,
        decoder: WicScreenshotDecoder,
        /// ONNX fallback for names Windows OCR cannot read at any scale
        /// (e.g. 崇高石 — the 崇 glyph is a known Windows zh-Hant blind
        /// spot). `None` when the runtime DLL is unavailable; the route then
        /// runs ladder-only and those frames skip.
        paddle: Option<std::sync::Mutex<ptt_ocr_onnx::PaddleCtcSession>>,
        /// One matcher per catalog asset, index-aligned with
        /// `ptt_catalog::poe2().assets()`, built from the active language's
        /// names.
        name_matchers: Vec<ptt_core::FullLineAffixMatcher>,
        language: ProfileLanguage,
        layout: crate::profiles::PanelLayout,
    }

    /// The two rectangles a row is read from: a padded one for Windows OCR,
    /// which wants context around the glyphs, and a tight one for the
    /// PP-OCRv5 retry, which reads a neighbouring row's descender as junk if
    /// given any.
    #[derive(Clone, Copy)]
    struct RowCrops {
        source: ptt_vision::PixelRect,
        content: ptt_vision::PixelRect,
    }

    impl Route {
        /// The Traditional Chinese route, which is the corpus-verified one.
        pub fn new() -> Result<Self, SkipReason> {
            Self::new_for(ProfileLanguage::TraditionalChinese)
        }

        pub fn new_for(language: ProfileLanguage) -> Result<Self, SkipReason> {
            Self::new_with(crate::profiles::poe2::LAYOUT, language)
        }

        /// A route over an explicit panel layout, for a second game.
        pub fn new_with(
            layout: crate::profiles::PanelLayout,
            language: ProfileLanguage,
        ) -> Result<Self, SkipReason> {
            // The bootstrap has four ways to fail and each says which, so a
            // degraded build is diagnosable. Reporting one fixed cause here
            // was worse than saying nothing: it named the DLL as missing on a
            // machine where the DLL was present.
            let paddle = match Self::start_paddle_session() {
                Ok(session) => Some(session),
                Err(reason) => {
                    eprintln!("warning: ONNX name fallback unavailable: {reason}");
                    None
                }
            };
            // A blank name means that language has not been authored for this
            // game. Refusing here beats building a matcher that silently
            // matches nothing and then skips every frame forever.
            let name_matchers = (layout.catalog)()
                .assets()
                .iter()
                .map(|asset| {
                    let name = match language {
                        ProfileLanguage::TraditionalChinese => &asset.name_zh_tw,
                        ProfileLanguage::English => &asset.name_en,
                    };
                    if name.trim().is_empty() {
                        return Err(SkipReason::Ocr(format!(
                            "catalog has no {language:?} name for {}; that language is not \
                             available for this game yet",
                            asset.id
                        )));
                    }
                    ptt_core::FullLineAffixMatcher::new(name)
                        .map_err(|error| SkipReason::Ocr(format!("matcher: {error:?}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                worker: OcrWorker::start()
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?,
                decoder: WicScreenshotDecoder::new()
                    .map_err(|error| SkipReason::Decode(format!("{error:?}")))?,
                paddle,
                name_matchers,
                language,
                layout,
            })
        }

        /// The scanline each table's rows actually start on.
        ///
        /// Returns the first run of [`ANCHOR_RUN`] consecutive scanlines each
        /// holding at least [`ANCHOR_MIN_LIT`] mask pixels, searched from
        /// `nominal` through `nominal + window`. A single stray scanline is
        /// not a row, and the column header ("Ratio"/"比率") is drawn too dim
        /// to survive the warm mask at all, so the first run found is the
        /// first row of data.
        fn find_table_anchor(
            mask: &ptt_vision::TextInkMask,
            nominal: u32,
            window: u32,
        ) -> Option<u32> {
            const ANCHOR_MIN_LIT: usize = 3;
            const ANCHOR_RUN: u32 = 3;

            let limit = (nominal + window).min(mask.height() as u32);
            let mut run_started: Option<u32> = None;
            for y in nominal..limit {
                let lit = (0..mask.width())
                    .filter(|&x| {
                        mask.intensity_at(x, y as usize)
                            .is_some_and(|value| value > 0)
                    })
                    .take(ANCHOR_MIN_LIT)
                    .count();
                if lit >= ANCHOR_MIN_LIT {
                    let start = *run_started.get_or_insert(y);
                    if y - start + 1 >= ANCHOR_RUN {
                        return Some(start);
                    }
                } else {
                    run_started = None;
                }
            }
            None
        }

        /// Slices each table into rows, keeping the ones that have ink.
        ///
        /// No band detection and no split inference: the split between the two
        /// tables is a constant, so the header between them is never looked at,
        /// and two adjacent rows cannot merge because the boundary is
        /// arithmetic rather than a gap in the mask. Only each table's origin
        /// is measured, because that is the one part of the geometry the
        /// client's language moves.
        fn plan_fixed_grid(
            mask: &ptt_vision::TextInkMask,
            grid: crate::profiles::FixedGrid,
        ) -> Result<(RowPlan, Vec<RowCrops>), RowsReject> {
            let mut bands = Vec::new();
            let mut crops = Vec::new();
            let mut available_rows = 0_u8;
            let mut competing_rows = 0_u8;

            for (side, nominal_top) in [
                (Side::Available, grid.available_top),
                (Side::Competing, grid.competing_top),
            ] {
                // No ink within a row's reach of where the table should be
                // means the table is empty. Slicing from the nominal top then
                // finds nothing either, which is the same answer by a slower
                // route, so skip it.
                let Some(first_ink) =
                    Self::find_table_anchor(mask, nominal_top, grid.anchor_window)
                else {
                    continue;
                };
                let table_top = first_ink.saturating_sub(grid.anchor_lead_in);
                if std::env::var_os("PTT_DEBUG_GRID").is_some() {
                    eprintln!(
                        "grid {side:?}: nominal={nominal_top} window={} first_ink={first_ink} top={table_top} mask={}x{}",
                        grid.anchor_window,
                        mask.width(),
                        mask.height()
                    );
                }
                for index in 0..grid.rows_per_side {
                    let top = table_top + u32::from(index) * grid.pitch;
                    let bottom = (top + grid.row_height).min(mask.height() as u32);
                    if top >= bottom {
                        break;
                    }
                    // How much ink the slice holds and where it starts, which
                    // is what the aggregate-zone subtraction downstream reads.
                    let bounds = crate::comparator::ink_bounds(
                        mask,
                        0,
                        top as usize,
                        mask.width(),
                        (bottom - top) as usize,
                    );
                    let Some(bounds) = bounds else { continue };
                    if bounds.count < grid.min_lit_pixels as usize {
                        continue;
                    }
                    let (left, right) = (bounds.left, bounds.right);
                    match side {
                        Side::Available => available_rows += 1,
                        Side::Competing => competing_rows += 1,
                    }
                    let slice_top = i32::try_from(top).unwrap_or(i32::MAX);
                    // Windows OCR gets the full row width; the retry gets the
                    // ink box, which is what stops a neighbouring row's
                    // descender from arriving as a second text line.
                    let height = (bottom - top) as usize;
                    let source = ptt_vision::PixelRect::new(0, top as usize, mask.width(), height)
                        .map_err(|_| RowsReject::ImplausibleBand {
                            top: slice_top,
                            height: bottom - top,
                        })?;
                    let content =
                        ptt_vision::PixelRect::new(left, top as usize, right - left, height)
                            .unwrap_or(source);
                    crops.push(RowCrops { source, content });
                    bands.push(RowBand {
                        side,
                        // The grid slices by index, so this is the index.
                        row_index: index,
                        band: BandGeometry {
                            top: slice_top,
                            height: bottom - top,
                            left: i32::try_from(left).unwrap_or(0),
                            width: u32::try_from(right.saturating_sub(left)).unwrap_or(0),
                            content_fingerprint: 0,
                        },
                        estimated_rows: 1,
                    });
                }
            }

            if bands.is_empty() {
                return Err(RowsReject::NoBands);
            }
            Ok((
                RowPlan {
                    bands,
                    available_rows,
                    competing_rows,
                },
                crops,
            ))
        }

        /// Which OCR language reads the identity slots.
        ///
        /// Numeric lanes are unaffected: ratios and stock are Arabic numerals
        /// in every client.
        const fn name_language(&self) -> OcrLanguagePreference {
            match self.language {
                ProfileLanguage::TraditionalChinese => OcrLanguagePreference::TraditionalChinese,
                ProfileLanguage::English => OcrLanguagePreference::English,
            }
        }

        /// Brings up the ONNX fallback, or explains why it could not.
        ///
        /// This path is what reads the currencies Windows OCR cannot — losing
        /// it costs rows on every affected frame and looks exactly like a
        /// normal skip, so every failure names itself.
        fn start_paddle_session() -> Result<std::sync::Mutex<ptt_ocr_onnx::PaddleCtcSession>, String>
        {
            let configured = std::env::var_os("PTT_ONNXRUNTIME_DLL").map(std::path::PathBuf::from);
            let beside_exe = std::env::current_exe()
                .ok()
                .and_then(|exe| Some(exe.parent()?.join("onnxruntime.dll")));
            let dll = configured
                .clone()
                .or_else(|| beside_exe.clone())
                .ok_or_else(|| "could not locate the executable directory".to_owned())?;
            if !dll.is_file() {
                return Err(format!(
                    "onnxruntime.dll not found at {} (set PTT_ONNXRUNTIME_DLL to override)",
                    dll.display()
                ));
            }
            ptt_ocr_onnx::initialize_onnx_runtime(&dll)
                .map_err(|error| format!("loading {}: {error:?}", dll.display()))?;
            let assets =
                ptt_ocr_onnx::PaddleAssets::load_installed().map_err(|error| format!("{error}"))?;
            let session = ptt_ocr_onnx::PaddleCtcSession::from_assets(
                &assets,
                ptt_ocr_onnx::PaddleSessionConfig::default(),
            )
            .map_err(|error| format!("starting the recognizer: {error:?}"))?;
            Ok(std::sync::Mutex::new(session))
        }

        /// One inference over the name region's warm mask, re-scored against
        /// every catalog name. Accepted only when exactly one target is
        /// strongly supported — two plausible neighbours mean skip.
        ///
        /// A wrapped name is read line by line first: the recognizer is a
        /// single-line model, and a two-line region squashed whole into its
        /// input loses glyphs (the corpus frame reads 9 of the name's 11, so
        /// no target can shape-match). The joined per-line text must hit the
        /// closed catalog exactly — the same gate the Windows OCR ladder
        /// passes through, so nothing is loosened — and a miss falls through
        /// to the whole-region read below unchanged.
        fn paddle_name_fallback<'catalog>(
            &self,
            frame: &ptt_vision::CapturedFrame,
            catalog: &'catalog ptt_catalog::Catalog,
        ) -> Option<(&'catalog ptt_catalog::CatalogAsset, String)> {
            let paddle = self.paddle.as_ref()?;
            let mask = build_warm_mask(frame, WarmMaskSettings::default());
            let view = ptt_ocr_onnx::ImageView::gray8(
                mask.width(),
                mask.height(),
                mask.stride(),
                mask.intensities(),
            )
            .ok()?;
            let mut session = paddle.lock().expect("paddle session lock");

            let bands = mask_line_bands(mask.width(), mask.height(), 2, |x, y| {
                mask.intensity_at(x, y).unwrap_or(0) != 0
            });
            if bands.len() >= 2 {
                let mut joined = String::new();
                for &(top, bottom) in &bands {
                    let Ok(line) = view.with_logical_content(top, bottom) else {
                        break;
                    };
                    let Ok(recognition) = session.recognize(line) else {
                        break;
                    };
                    if !joined.is_empty() {
                        joined.push(' ');
                    }
                    joined.push_str(&recognition.text);
                }
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!("debug paddle lines: bands={bands:?} joined={joined:?}");
                }
                if let Some(asset) = crate::identity::resolve_name(&joined, catalog, self.language)
                {
                    return Some((asset, joined));
                }
            }

            let batch = session.recognize_batch(view, &self.name_matchers).ok()?;
            if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                eprintln!(
                    "debug paddle greedy: text={:?} targets={} strong={}",
                    batch.recognition.text,
                    batch.target_supports.len(),
                    batch
                        .target_supports
                        .iter()
                        .filter(|entry| entry.support.strongly_supported)
                        .count(),
                );
                let mut reasons = std::collections::HashMap::new();
                for entry in &batch.target_supports {
                    *reasons
                        .entry(format!("{}", entry.support.reason))
                        .or_insert(0usize) += 1;
                }
                let mut reasons: Vec<_> = reasons.into_iter().collect();
                reasons.sort_by(|a, b| b.1.cmp(&a.1));
                for (reason, count) in reasons.iter().take(5) {
                    eprintln!("debug paddle reject: {count} x {reason}");
                }
            }
            let winner = unique_winner_asset(&batch.target_supports, &self.name_matchers)?;
            Some((&catalog.assets()[winner], batch.recognition.text))
        }

        /// PP-OCRv5 fallback for a row whose Windows OCR text failed the
        /// grammar (highlighted market-rate rows lose their "1:", tiny
        /// stocks vanish, `1` reads as `I`). Runs greedy CTC on the raw
        /// luminance crop and splits ratio|stock on the spatial gap between
        /// emission time steps. The same strict grammar re-validates the
        /// result, so this recovers rows without loosening any gate.
        /// One row's two fields, each parsed on its own.
        ///
        /// Separately, because they fail separately -- see the ladder at the
        /// call site. A `None` here means only that this engine did not read
        /// that field; another one still might.
        fn split_row_fields(
            lines: &[ptt_ocr_win::RecognizedLine],
        ) -> (Option<crate::fields::RatioField>, Option<u64>) {
            match lines {
                // One line holding both, split on the gap between them.
                [single] => Self::split_row_text(&single.text),
                [first, second] => {
                    let (ratio_line, stock_line) = if first.left <= second.left {
                        (first, second)
                    } else {
                        (second, first)
                    };
                    // A lone ratio line cannot glue with a wrapped fragment
                    // (those arrive as merged bands), so whitespace inside it
                    // is safe to drop here.
                    let ratio_text: String = ratio_line
                        .text
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    (
                        parse_ratio(&ratio_text).ok(),
                        parse_stock(&stock_line.text).ok(),
                    )
                }
                // A row with only one line read is not malformed, it is
                // half-read: whichever field is there is still worth keeping,
                // and the other engine is asked for the rest.
                [] => (None, None),
                _ => (None, None),
            }
        }

        /// The same, from a single text line the engine did not split.
        fn split_row_text(text: &str) -> (Option<crate::fields::RatioField>, Option<u64>) {
            match split_row_line(text) {
                Ok((ratio_text, stock_text)) => {
                    (parse_ratio(&ratio_text).ok(), parse_stock(&stock_text).ok())
                }
                // No gap to split on, so the line holds at most one field.
                // Which one is decided by the grammars, not guessed: exactly
                // one of them must accept it. A line both would take is
                // ambiguous and yields nothing, and a line neither takes was
                // never a field.
                //
                // This is what keeps a row whose rate Windows OCR dropped
                // entirely -- `89,176` for a row reading `8 : 1   89,176`.
                // The stock is right there and strictly parsed; only the rate
                // has to come from somewhere else.
                Err(_) => {
                    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                    match (parse_ratio(&compact).ok(), parse_stock(text).ok()) {
                        (Some(ratio), None) => (Some(ratio), None),
                        (None, Some(stock)) => (None, Some(stock)),
                        _ => (None, None),
                    }
                }
            }
        }

        /// Greedy PP-OCRv5 text over a frame rectangle's raw luminance.
        fn paddle_recognize_rect(
            &self,
            frame: &ptt_vision::CapturedFrame,
            mask: Option<&ptt_vision::TextInkMask>,
            rect: ptt_vision::PixelRect,
        ) -> Option<ptt_ocr_onnx::CtcRecognition> {
            let paddle = self.paddle.as_ref()?;
            rect.validate_within(frame.width(), frame.height()).ok()?;
            let source = frame.bgra_pixels();
            let mut gray = vec![0u8; rect.width * rect.height];
            for y in 0..rect.height {
                let source_row = (rect.y + y) * frame.stride();
                for x in 0..rect.width {
                    let pixel = source_row + (rect.x + x) * 4;
                    gray[y * rect.width + x] = ((u32::from(source[pixel + 2]) * 299
                        + u32::from(source[pixel + 1]) * 587
                        + u32::from(source[pixel]) * 114)
                        / 1000) as u8;
                }
            }
            // Experiment: hand the model the ink mask's verdict instead of the
            // raw panel. The mask already knows the stone is not a glyph.
            // With a mask, everything it calls background is blanked before
            // the model sees it. Reserved for the second reading of a row the
            // ordering gate rejected — see `reread_contradictory_rows`.
            if let Some(mask) = mask {
                for y in 0..rect.height {
                    for x in 0..rect.width {
                        if mask.intensity_at(rect.x + x, rect.y + y).unwrap_or(0) == 0 {
                            gray[y * rect.width + x] = 0;
                        }
                    }
                }
            }
            let view =
                ptt_ocr_onnx::ImageView::gray8(rect.width, rect.height, rect.width, &gray).ok()?;
            paddle
                .lock()
                .expect("paddle session lock")
                .recognize(view)
                .ok()
        }

        /// Reads the rows the panel's own order rejects one more time, and
        /// keeps the result only if it restores that order.
        ///
        /// Safe by construction: these rows are already being discarded, so a
        /// second reading either wins one back or changes nothing. Returns
        /// whatever is still contradictory afterwards.
        ///
        /// The second opinion is the ink mask. Both recognizers are handed raw
        /// panel pixels — the mask decides geometry, not what the model sees —
        /// so the stone texture in the blank column between the rate and the
        /// stock reaches them, and is occasionally recognized as a glyph:
        /// `97 : 1` came back as `97 : 10`, with the phantom `0` emitted
        /// between the two columns and near enough to the rate to join it.
        /// Blanking everything the mask calls background removes exactly that.
        ///
        /// It is not a better reader in general, which is why it is not the
        /// first one: run over the frozen corpora it loses about as many rows
        /// as it wins. Confined to rows already lost, only the winning half
        /// can happen.
        fn reread_contradictory_rows(
            &self,
            frame: &ptt_vision::CapturedFrame,
            mask: &ptt_vision::TextInkMask,
            rows: &mut Vec<RowObservation>,
            row_rects: &[((Side, u8), ptt_vision::PixelRect)],
        ) -> Vec<(Side, u8)> {
            let contradictory = crate::book::out_of_order_rows(rows);
            if contradictory.is_empty() {
                return contradictory;
            }
            let mut repaired = rows.clone();
            let mut changed = false;
            for &(side, row_index) in &contradictory {
                let Some(&(_, rect)) = row_rects.iter().find(|&&(key, _)| key == (side, row_index))
                else {
                    continue;
                };
                let Some((Some(ratio), Some(stock))) = self
                    .paddle_row_line(frame, mask, rect, true)
                    .map(|line| Self::split_row_text(&line))
                else {
                    continue;
                };
                let Some(row) = repaired
                    .iter_mut()
                    .find(|row| row.side == side && row.row_index == row_index)
                else {
                    continue;
                };
                if row.ratio.left == ratio.left && row.ratio.right == ratio.right {
                    continue;
                }
                // The comparator is read off this frame's ink geometry rather
                // than off the text, so the first pass's verdict on it stands.
                row.ratio = crate::fields::RatioField {
                    comparator: row.ratio.comparator,
                    ..ratio
                };
                row.stock = stock;
                changed = true;
            }
            if !changed {
                return contradictory;
            }
            let after = crate::book::out_of_order_rows(&repaired);
            // Strictly better or not at all. A reading that merely swaps which
            // rows disagree has not learned anything about the panel.
            if after.len() < contradictory.len() {
                *rows = repaired;
                return after;
            }
            contradictory
        }

        /// The row's two columns, from one recognition of the whole row.
        ///
        /// `masked` blanks everything the ink mask calls background before the
        /// model sees the crop. Off for the first reading: over the frozen
        /// corpora it loses about as many rows as it wins, so it is reserved
        /// for [`Self::reread_contradictory_rows`], where the rows in question
        /// are already being discarded.
        ///
        /// Unmasked, the columns are split on the model's own letter spacing —
        /// 3x the median inter-glyph delta — which is what lets panel texture
        /// in the blank column join the rate when it is recognized as a glyph.
        fn paddle_row_line(
            &self,
            frame: &ptt_vision::CapturedFrame,
            mask: &ptt_vision::TextInkMask,
            rect: ptt_vision::PixelRect,
            masked: bool,
        ) -> Option<String> {
            let recognition = self.paddle_recognize_rect(frame, masked.then_some(mask), rect)?;

            // The tensor is height-normalized, so absolute step distances
            // scale with the crop: use 3x the median inter-glyph delta as the
            // column boundary (intra-number deltas cluster tightly; the
            // ratio|stock gap is a far outlier at any scale).
            let mut deltas: Vec<usize> = recognition
                .emissions
                .windows(2)
                .map(|pair| pair[1].time_step.saturating_sub(pair[0].time_step))
                .collect();
            deltas.sort_unstable();
            let median_delta = deltas.get(deltas.len() / 2).copied().unwrap_or(1);
            let column_gap_steps = (median_delta * 3).max(4);

            let mut tokens: Vec<String> = Vec::new();
            let mut previous_step: Option<usize> = None;
            for emission in &recognition.emissions {
                let is_gap = previous_step.is_some_and(|step| {
                    emission.time_step.saturating_sub(step) >= column_gap_steps
                });
                if tokens.is_empty() || is_gap {
                    tokens.push(String::new());
                }
                tokens
                    .last_mut()
                    .expect("token pushed above")
                    .push_str(&emission.text);
                previous_step = Some(emission.time_step);
            }
            if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                // Per-emission steps, not just the tokens: a phantom glyph
                // struck in the blank column between the rate and the stock
                // is invisible in the joined token but obvious as a step
                // sitting alone in the gap. See the known-defect note on
                // `paddle_row_line`.
                let emissions: Vec<(usize, &str)> = recognition
                    .emissions
                    .iter()
                    .map(|emission| (emission.time_step, emission.text.as_str()))
                    .collect();
                eprintln!(
                    "debug paddle-row: tokens={tokens:?} gap>={column_gap_steps}                      emissions={emissions:?}"
                );
            }
            // A leading bare comparator is part of the ratio.
            if tokens.len() > 2 && (tokens[0] == "<" || tokens[0] == ">") {
                let comparator = tokens.remove(0);
                tokens[0] = format!("{comparator}{}", tokens[0]);
            }
            match tokens.as_slice() {
                // Exactly ratio + stock: the two columns the row holds.
                [_, _] => Some(tokens.join(" ")),
                // One token, but with a space the model itself emitted: the
                // column gap was read as a glyph instead of as silence, which
                // halves the time-step deltas around it and defeats the gap
                // splitter above — `> 1 : 11.42   607,760` came back as the
                // single token "1：11.42 607,760". The literal space is the
                // same boundary evidence a time gap is, and the caller's
                // grammars judge the halves either way — but only after the
                // mask attests that the crop's two sides share one text
                // line. A repaint tear offsets the rate and stock columns by
                // half a row, the squashed single-line model erases exactly
                // that vertical structure, and a rate paired with the stock
                // drawn beside-but-below it is a wrong value, not a hard
                // row. (The corpus keeps such a tear as a permanent
                // negative; this branch is the one reader that accepted it.)
                //
                // A token with no whitespace stays rejected: one column
                // really did fill the crop, and a stray partial digit must
                // never be mistaken for a stock value.
                [single]
                    if single.chars().any(char::is_whitespace)
                        && crop_sides_share_one_line(rect.width, rect.height, |x, y| {
                            mask.intensity_at(rect.x + x, rect.y + y).unwrap_or(0) != 0
                        }) =>
                {
                    Some(single.clone())
                }
                _ => None,
            }
        }

        /// The text lines with ink inside one slot's slice, as absolute
        /// inclusive row ranges.
        fn slice_lines(
            mask: &ptt_vision::TextInkMask,
            grid: crate::profiles::FixedGrid,
            slice_top: usize,
        ) -> Vec<(usize, usize)> {
            let ink = |x: usize, y: usize| mask.intensity_at(x, y).unwrap_or(0) != 0;
            let slice_bottom = (slice_top + grid.row_height as usize).min(mask.height());
            let span_top = slice_top.saturating_sub(grid.pitch as usize / 2);
            let span_bottom = (slice_bottom + grid.pitch as usize / 2).min(mask.height());
            if span_bottom <= span_top {
                return Vec::new();
            }
            // Bands come from the wider span so a line straddling the slice
            // edge is seen once as a whole line, not twice as clipped pieces.
            mask_line_bands(mask.width(), span_bottom - span_top, 0, |x, y| {
                ink(x, span_top + y)
            })
            .into_iter()
            .map(|(top, bottom)| (span_top + top, span_top + bottom))
            .filter(|band| band.0 < slice_bottom && band.1 >= slice_top)
            .collect()
        }

        /// Re-reads a slot from its own text line, after the engine ladder
        /// has already failed it.
        ///
        /// The grid slices arithmetically, so when the row above wraps, its
        /// second line hangs down into this slot's rectangle and both
        /// engines read the intruder together with this row's own text:
        /// `122 : 1` comes back as `122: ±` from Windows and `122:t` from
        /// PP-OCRv5, the stray `1` above fusing with the real one. Nothing
        /// is wrong with the row — only with the rectangle.
        ///
        /// So the crop follows the slot's own line band instead. Ownership
        /// is by overlap with the slice, which is not a close call: the
        /// row's own line lies wholly inside it (17 of 17 rows on the corpus
        /// frame) while an intruding tail shows only its bottom few (7).
        /// A band taller than the grid's own row height is two lines fused
        /// and is refused — that is the wrapped shape, and it has its own
        /// reader.
        fn own_band_retry(
            &self,
            frame: &ptt_vision::CapturedFrame,
            mask: &ptt_vision::TextInkMask,
            grid: crate::profiles::FixedGrid,
            slice_top: usize,
        ) -> Option<(crate::fields::RatioField, u64)> {
            let slice_bottom = (slice_top + grid.row_height as usize).min(mask.height());
            let overlap = |band: &(usize, usize)| {
                band.1
                    .min(slice_bottom.saturating_sub(1))
                    .saturating_sub(band.0.max(slice_top))
                    + 1
            };
            let own = Self::slice_lines(mask, grid, slice_top)
                .into_iter()
                .max_by_key(overlap)?;
            // A band taller than the grid's own row height is two lines with
            // the blank row between them missing, and this reader has no way
            // to say where one ends: splitting it by the stock column's ink
            // was tried and put the boundary one scanline out, which left a
            // neighbour's leading digit inside and turned `167 : 1` into
            // `1167 : 1`. Refused instead — see the known-defect note.
            if own.1 - own.0 + 1 > grid.row_height as usize {
                return None;
            }

            let bounds =
                crate::comparator::ink_bounds(mask, 0, own.0, mask.width(), own.1 - own.0 + 1)?;
            let margin = 2usize;
            let left = bounds.left.saturating_sub(margin);
            let rect = ptt_vision::PixelRect::new(
                left,
                own.0.saturating_sub(margin),
                (bounds.right + margin).min(frame.width()) - left,
                (own.1 + 1 + margin).min(frame.height()) - own.0.saturating_sub(margin),
            )
            .ok()?;

            // Both engines again, on the corrected rectangle, judged by the
            // same grammars as every other row. Windows OCR splits the two
            // columns into its own lines; PP-OCRv5 gets the gap splitter.
            let windows = self
                .worker
                .recognize(
                    OcrLanguagePreference::English,
                    Self::upscaled_frame_rect_2x(frame, rect).ok()?,
                )
                .ok()
                .map(|recognition| Self::split_row_fields(recognition.lines.as_slice()))
                .unwrap_or((None, None));
            let paddle = self
                .paddle_row_line(frame, mask, rect, false)
                .map_or((None, None), |line| Self::split_row_text(&line));
            if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                eprintln!(
                    "debug own-band: band={own:?} rect={rect:?} windows={:?} paddle={:?}",
                    (windows.0.is_some(), windows.1),
                    (paddle.0.is_some(), paddle.1)
                );
            }
            match (windows.0.or(paddle.0), windows.1.or(paddle.1)) {
                (Some(ratio), Some(stock)) => Some((ratio, stock)),
                _ => None,
            }
        }

        /// Reads a slot whose rate wrapped onto a second line, after the
        /// engine ladder has already failed it. Fixed-grid panels only — the
        /// zone arithmetic is the grid's.
        ///
        /// A rate too wide for its column wraps inside its slot: `1 :` on
        /// one line, the decimal on a second, and the stock sits vertically
        /// between them — so both engines fail it the way the wrapped names
        /// failed, two stacked lines squashed into single-line readers. The
        /// panel's own columns undo the stack: everything left of the widest
        /// blank column run is the rate, read line band by line band;
        /// everything right of it is the stock, which never wraps.
        ///
        /// The zone's edges come from the panel, not from tuning. A wrapped
        /// block centers in its slot and overflows both ends (measured 8
        /// rows up, against a real 2-row blank gap to the row above), so the
        /// top follows the ink upward, bounded by half a slot. The bottom
        /// cannot follow ink — the block's descender tail and the next row's
        /// ascenders touch with no blank row between them — so it stops
        /// where the next slot's own text begins, which the grid knows
        /// exactly: one pitch plus the anchor's lead-in below the slice.
        fn wrapped_row_rescue(
            &self,
            frame: &ptt_vision::CapturedFrame,
            mask: &ptt_vision::TextInkMask,
            grid: crate::profiles::FixedGrid,
            slice_top: usize,
        ) -> Option<(crate::fields::RatioField, u64)> {
            let ink = |x: usize, y: usize| mask.intensity_at(x, y).unwrap_or(0) != 0;

            // Which of the reach's line bands this slot owns. The reach
            // extends half a slot above the slice (a centered block cannot
            // overflow further without belonging to the row above) and down
            // to where the next slot's own text begins. Ownership is by
            // where a band ends: a band that ends above the slice top lies
            // wholly in the row above and is discarded, which is what keeps
            // the row above's glyphs out of the column projection below.
            let span_top = slice_top.saturating_sub(grid.pitch as usize / 2);
            let bottom =
                (slice_top + (grid.pitch + grid.anchor_lead_in) as usize).min(mask.height());
            if bottom <= span_top {
                return None;
            }
            let owned: Vec<(usize, usize)> =
                mask_line_bands(mask.width(), bottom - span_top, 0, |x, y| {
                    ink(x, span_top + y)
                })
                .into_iter()
                .map(|(band_top, band_bottom)| (span_top + band_top, span_top + band_bottom))
                .filter(|&(_, band_bottom)| band_bottom >= slice_top)
                .collect();
            let owned_ink = |x: usize, y: usize| {
                owned
                    .iter()
                    .any(|&(band_top, band_bottom)| (band_top..=band_bottom).contains(&y))
                    && ink(x, y)
            };

            // The rate/stock boundary is the widest blank column run between
            // the outermost owned ink. Measured 40..85px between the columns
            // against at most 8 inside a number; a zone without a gap that
            // wide has no second column to find.
            let mut ink_columns = vec![false; mask.width()];
            for &(band_top, band_bottom) in &owned {
                for y in band_top..=band_bottom {
                    for (x, seen) in ink_columns.iter_mut().enumerate() {
                        *seen = *seen || ink(x, y);
                    }
                }
            }
            let first = ink_columns.iter().position(|&seen| seen)?;
            let last = ink_columns.iter().rposition(|&seen| seen)?;
            let mut widest = (0_usize, 0_usize);
            let mut run_start = None;
            for (x, &lit) in ink_columns.iter().enumerate().take(last + 1).skip(first) {
                match (lit, run_start) {
                    (false, None) => run_start = Some(x),
                    (true, Some(start)) => {
                        if x - start > widest.1 {
                            widest = (start, x - start);
                        }
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if widest.1 < 20 {
                return None;
            }
            let split = widest.0 + widest.1 / 2;

            // Exactly two rate lines and one stock line: the wrap shape and
            // nothing else. One left band is an ordinary row, which the
            // ladder already judged; three mean a neighbour's ink leaked in.
            let height = bottom - span_top;
            let left_bands = mask_line_bands(split, height, 0, |x, y| owned_ink(x, span_top + y));
            let right_bands = mask_line_bands(mask.width() - split, height, 0, |x, y| {
                owned_ink(split + x, span_top + y)
            });
            let debug = std::env::var_os("PTT_DEBUG_OCR").is_some();
            if debug {
                eprintln!(
                    "debug wrap-rescue: span={span_top}..{bottom} owned={owned:?} \
                     split={split} left={left_bands:?} right={right_bands:?}"
                );
            }
            let [head_band, tail_band] = left_bands.as_slice() else {
                return None;
            };
            let [stock_band] = right_bands.as_slice() else {
                return None;
            };

            // A band's ink box plus breathing room — the same lesson as the
            // retry's tight content rect: the recognizer keys its scale off
            // the crop, and `1 :` — three small glyphs — reads as nothing
            // from a crop that is mostly empty panel.
            let band_rect = |x0: usize, band: (usize, usize), x1: usize| {
                let bounds = crate::comparator::ink_bounds(
                    mask,
                    x0,
                    span_top + band.0,
                    x1 - x0,
                    band.1 - band.0 + 1,
                )?;
                let margin = 2usize;
                let left = bounds.left.saturating_sub(margin);
                let right = (bounds.right + margin).min(frame.width());
                let rect_top = (span_top + band.0).saturating_sub(margin);
                let rect_bottom = (span_top + band.1 + 1 + margin).min(frame.height());
                ptt_vision::PixelRect::new(left, rect_top, right - left, rect_bottom - rect_top)
                    .ok()
            };
            let compact = |text: &str| -> String {
                text.chars()
                    .filter(|c| !c.is_whitespace())
                    .map(|c| if c == '：' { ':' } else { c })
                    .collect()
            };
            // Both engines offer readings and the grammars arbitrate,
            // exactly like the row ladder. PP-OCRv5 reads each line from
            // its tight ink box — it reads the wrapped decimal that Windows
            // drops — while Windows OCR reads the whole rate column at
            // magnification and splits the two lines itself: the tiny `1 :`
            // head defeats PP-OCRv5 (three small glyphs come back as "19"
            // or "1°" from the tight crop) but reads cleanly for Windows
            // with the second line beneath it as context. A candidate pair
            // survives only if the join shapes allow it and the rate
            // grammar parses the result.
            let paddle_read = |rect: ptt_vision::PixelRect| -> Option<String> {
                let text = compact(&self.paddle_recognize_rect(frame, None, rect)?.text);
                (!text.is_empty()).then_some(text)
            };
            // The head's separator needs no OCR at all — which is well,
            // because both engines misread it (`°`, `9`, or dropped): a
            // colon is two short dot runs, answered by the mask pixels the
            // way the comparator chevron is. The head's digits, which OCR
            // does read, are whatever precedes its last glyph group.
            let glyph_head = || -> Option<String> {
                let head_top = span_top + head_band.0;
                let head_height = head_band.1 - head_band.0 + 1;
                let mut columns = vec![false; split];
                for y in head_top..head_top + head_height {
                    for (x, seen) in columns.iter_mut().enumerate() {
                        *seen = *seen || ink(x, y);
                    }
                }
                // Glyph groups split on gaps of 3+ blank columns: digits
                // inside a number sit 1-2 apart, the colon 8-11 from the
                // last digit.
                let mut groups: Vec<(usize, usize)> = Vec::new();
                for (x, &seen) in columns.iter().enumerate() {
                    if !seen {
                        continue;
                    }
                    match groups.last_mut() {
                        Some(group) if x - group.1 <= 3 => group.1 = x,
                        _ => groups.push((x, x)),
                    }
                }
                let (&(colon_left, colon_right), digit_groups) = groups.split_last()?;
                let (first_digits, last_digits) = (digit_groups.first()?, digit_groups.last()?);
                if !glyph_box_is_colon(colon_right - colon_left + 1, head_height, |x, y| {
                    ink(colon_left + x, head_top + y)
                }) {
                    return None;
                }
                let margin = 2usize;
                let left = first_digits.0.saturating_sub(margin);
                let rect = ptt_vision::PixelRect::new(
                    left,
                    head_top.saturating_sub(margin),
                    (last_digits.1 + 1 + margin).min(frame.width()) - left,
                    (head_top + head_height + margin).min(frame.height())
                        - head_top.saturating_sub(margin),
                )
                .ok()?;
                let digits = paddle_read(rect).or_else(|| {
                    let image = Self::upscaled_frame_rect(frame, rect, 4).ok()?;
                    let text = compact(
                        &self
                            .worker
                            .recognize(OcrLanguagePreference::English, image)
                            .ok()?
                            .text(),
                    );
                    (!text.is_empty()).then_some(text)
                })?;
                digits
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| format!("{digits}:"))
            };
            let mut pairs: Vec<(String, String)> = Vec::new();
            let tail = band_rect(0, *tail_band, split).and_then(paddle_read);
            if let (Some(head), Some(tail)) = (
                band_rect(0, *head_band, split).and_then(paddle_read),
                tail.clone(),
            ) {
                pairs.push((head, tail));
            }
            if let (Some(head), Some(tail)) = (glyph_head(), tail) {
                let pair = (head, tail);
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
            if let Some(rect) = band_rect(0, (head_band.0, tail_band.1), split) {
                for factor in [2_usize, 4] {
                    let Ok(image) = Self::upscaled_frame_rect(frame, rect, factor) else {
                        continue;
                    };
                    let Ok(recognition) =
                        self.worker.recognize(OcrLanguagePreference::English, image)
                    else {
                        continue;
                    };
                    let mut lines: Vec<_> = recognition.lines.iter().collect();
                    lines.sort_by(|a, b| a.top.total_cmp(&b.top));
                    if debug {
                        let texts: Vec<&str> =
                            lines.iter().map(|line| line.text.as_str()).collect();
                        eprintln!("debug wrap-rescue: windows x{factor} lines={texts:?}");
                    }
                    if let [head, tail] = lines.as_slice() {
                        let pair = (compact(&head.text), compact(&tail.text));
                        if !pair.0.is_empty() && !pair.1.is_empty() && !pairs.contains(&pair) {
                            pairs.push(pair);
                        }
                    }
                }
            }
            if debug {
                eprintln!("debug wrap-rescue: rate pairs={pairs:?}");
            }
            let ratio = pairs.iter().find_map(|(head, tail)| {
                let joined = join_wrapped_rate(head, tail)?;
                crate::fields::parse_ratio(&joined).ok()
            })?;
            // The stock is one line; PP-OCRv5 first, then Windows at 2x.
            let stock_rect = band_rect(split, *stock_band, mask.width())?;
            let stock = [
                paddle_read(stock_rect),
                Self::upscaled_frame_rect(frame, stock_rect, 2)
                    .ok()
                    .and_then(|image| {
                        self.worker
                            .recognize(OcrLanguagePreference::English, image)
                            .ok()
                    })
                    .map(|recognition| compact(&recognition.text())),
            ]
            .into_iter()
            .flatten()
            .find_map(|text| crate::fields::parse_stock(&text).ok())?;
            Some((ratio, stock))
        }

        /// Preset region, with two override layers: a runtime override set by
        /// the app's calibration wizard (wins), then the
        /// `PTT_POE2_<NAME>_ROI=x,y,w,h` env override for probe experiments.
        fn region(&self, name: &str, preset: (i32, i32, u32, u32)) -> CaptureRegion {
            let prefix = self.layout.key_prefix;
            if let Some(region) = region_override(prefix, name)
                && let Ok(valid) = CaptureRegion::new(region.0, region.1, region.2, region.3)
            {
                return valid;
            }
            let from_env = std::env::var(format!("PTT_{prefix}_{name}_ROI"))
                .ok()
                .and_then(|value| {
                    let parts: Vec<i64> = value
                        .split(',')
                        .filter_map(|part| part.trim().parse().ok())
                        .collect();
                    match parts.as_slice() {
                        [x, y, w, h] => Some((
                            *x as i32,
                            *y as i32,
                            u32::try_from(*w).ok()?,
                            u32::try_from(*h).ok()?,
                        )),
                        _ => None,
                    }
                })
                .unwrap_or(preset);
            CaptureRegion::new(from_env.0, from_env.1, from_env.2, from_env.3)
                .expect("profile regions are valid")
        }

        fn scale_factor() -> usize {
            Self::scale_override().unwrap_or(2)
        }

        fn scale_override() -> Option<usize> {
            std::env::var("PTT_OCR_SCALE")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| (1..=4).contains(value))
        }

        /// Nearest-neighbour upscale of one frame rectangle's luminance.
        /// Raw pixels beat a thresholded mask as OCR input: Windows OCR's own
        /// binarization keeps the tiny ratio colon that masking erodes.
        fn upscaled_frame_rect_2x(
            frame: &ptt_vision::CapturedFrame,
            rect: ptt_vision::PixelRect,
        ) -> Result<OwnedBgraImage, SkipReason> {
            Self::upscaled_frame_rect(frame, rect, Self::scale_factor())
        }

        fn upscaled_frame_rect(
            frame: &ptt_vision::CapturedFrame,
            rect: ptt_vision::PixelRect,
            factor: usize,
        ) -> Result<OwnedBgraImage, SkipReason> {
            rect.validate_within(frame.width(), frame.height())
                .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
            let width = rect.width * factor;
            let height = rect.height * factor;
            let source = frame.bgra_pixels();
            let mut pixels = vec![0u8; width * height];
            for y in 0..height {
                let source_row = (rect.y + y / factor) * frame.stride();
                for x in 0..width {
                    let pixel = source_row + (rect.x + x / factor) * 4;
                    let luminance = (u32::from(source[pixel + 2]) * 299
                        + u32::from(source[pixel + 1]) * 587
                        + u32::from(source[pixel]) * 114)
                        / 1000;
                    pixels[y * width + x] = luminance as u8;
                }
            }
            OwnedBgraImage::gray8(width, height, width, pixels)
                .map_err(|error| SkipReason::Ocr(format!("{error:?}")))
        }

        /// Name resolution with an escalating upscale ladder. Windows OCR
        /// reads some tiny zh glyph sets only at higher magnification (e.g.
        /// 崇高石 needs 4× where 神聖石 reads at 2×). An exact catalog hit at
        /// any scale is fail-closed safe — garbage never hits the closed
        /// lexicon — so the ladder stops at the first hit and unresolved
        /// frames simply skip.
        fn ocr_name_resolved<'catalog>(
            &self,
            frame: &ptt_vision::CapturedFrame,
            label: &str,
            catalog: &'catalog ptt_catalog::Catalog,
        ) -> Result<(Option<&'catalog ptt_catalog::CatalogAsset>, String), SkipReason> {
            let rect = ptt_vision::PixelRect::new(0, 0, frame.width(), frame.height())
                .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
            let ladder: &[usize] = &[2, 3, 4];
            let mut last_text = String::new();
            for &factor in Self::scale_override()
                .as_ref()
                .map(std::slice::from_ref)
                .unwrap_or(ladder)
            {
                let recognition = self
                    .worker
                    .recognize(
                        self.name_language(),
                        Self::upscaled_frame_rect(frame, rect, factor)?,
                    )
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
                let text = recognition.text();
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!(
                        "debug {label} x{factor}: lines={} text={:?}",
                        recognition.lines.len(),
                        text
                    );
                }
                if let Some(asset) = crate::identity::resolve_name(&text, catalog, self.language) {
                    return Ok((Some(asset), text));
                }
                if !text.trim().is_empty() {
                    last_text = text;
                }
            }
            if let Some((asset, text)) = self.paddle_name_fallback(frame, catalog) {
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!("debug {label} paddle: text={:?} -> {}", text, asset.id);
                }
                return Ok((Some(asset), text));
            }
            Ok((None, last_text))
        }

        /// The three capture regions this profile reads (env overrides
        /// applied), for live capture callers.
        pub fn regions(&self) -> (CaptureRegion, CaptureRegion, CaptureRegion) {
            (
                self.region("NEED", self.layout.need_name),
                self.region("HAVE", self.layout.have_name),
                self.region("TABLES", self.layout.tables_for(self.language)),
            )
        }

        /// Full offline route over one screenshot file.
        pub fn recognize_screenshot(
            &self,
            path: &std::path::Path,
        ) -> Result<RecognizedBook, SkipReason> {
            let (need_region, have_region, tables_region) = self.regions();
            let decode = |region: CaptureRegion| {
                self.decoder
                    .decode(path, Some(region))
                    .map_err(|error| SkipReason::Decode(format!("{error:?}")))
            };
            let need_frame = decode(need_region)?;
            let have_frame = decode(have_region)?;
            let tables_frame = decode(tables_region)?;
            self.recognize_frames(&need_frame, &have_frame, &tables_frame)
        }

        /// Live route over already-captured frames (name slots + tables).
        pub fn recognize_frames(
            &self,
            need_frame: &ptt_vision::CapturedFrame,
            have_frame: &ptt_vision::CapturedFrame,
            tables_frame: &ptt_vision::CapturedFrame,
        ) -> Result<RecognizedBook, SkipReason> {
            let catalog = (self.layout.catalog)();

            let (need, need_text) = self.ocr_name_resolved(need_frame, "NEED", catalog)?;
            let need = need.ok_or(SkipReason::NeedNameUnresolved {
                text: need_text.clone(),
            })?;
            let (have, have_text) = self.ocr_name_resolved(have_frame, "HAVE", catalog)?;
            let have = have.ok_or(SkipReason::HaveNameUnresolved {
                text: have_text.clone(),
            })?;

            let frame = tables_frame;
            let mask = build_warm_mask(frame, WarmMaskSettings::default());
            let comparator_mask = self
                .layout
                .comparator_mask
                .map(|settings| build_warm_mask(frame, settings));
            let detection = PhysicalBandDetector::new()
                .detect(&mask, BandDetectionSettings::default())
                .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
            let geometry: Vec<BandGeometry> = detection
                .bands
                .iter()
                .map(|band| BandGeometry {
                    top: band.crop.source_rect.y as i32,
                    height: band.crop.source_rect.height as u32,
                    left: band.crop.source_rect.x as i32,
                    width: band.crop.source_rect.width as u32,
                    content_fingerprint: band.identity.content_fingerprint,
                })
                .collect();
            // Crops travel with the plan. Reading them out of the detector's
            // array by row index only works while the two lists are 1:1,
            // which the fixed grid is not — that mismatch had rows OCR'd from
            // a rectangle belonging to a different row.
            let (plan, row_crops) = match self.layout.row_source {
                crate::profiles::RowSource::DetectedBands => {
                    // Filtered together, before classifying, so the two lists
                    // stay index-aligned. Dropping fragments inside
                    // `classify_rows` alone left the crops holding every band
                    // and the plan holding only some, which is the mismatch
                    // the comment above warns about -- rows then OCR a
                    // rectangle belonging to a different row, and come back
                    // holding one column of it.
                    let layout = (self.layout.rows)();
                    let (kept, crops): (Vec<BandGeometry>, Vec<RowCrops>) = geometry
                        .iter()
                        .zip(detection.bands.iter())
                        .filter(|(band, _)| crate::rows::classifiable(band, &layout))
                        .map(|(band, detected)| {
                            // Full table width for the OCR crop, as the fixed
                            // grid already does. The detector's rectangle is
                            // drawn around ink, and a row's two columns are
                            // separated by a wide gap -- so a rate whose
                            // digits are faint leaves a box holding only the
                            // stock, and OCR dutifully returns `3,636` for a
                            // row that reads `36 : 1   3,636`. The row then
                            // fails the grammar and is thrown away for having
                            // been cropped in half.
                            let source = ptt_vision::PixelRect::new(
                                0,
                                detected.crop.source_rect.y,
                                mask.width(),
                                detected.crop.source_rect.height,
                            )
                            .unwrap_or(detected.crop.source_rect);
                            (
                                *band,
                                RowCrops {
                                    source,
                                    // Ink-derived still, because this one is
                                    // used to place the chevron.
                                    content: detected.crop.content_rect,
                                },
                            )
                        })
                        .unzip();
                    let plan = classify_rows(&kept, &layout).map_err(SkipReason::Rows)?;
                    (plan, crops)
                }
                crate::profiles::RowSource::FixedGrid(grid) => {
                    Self::plan_fixed_grid(&mask, grid).map_err(SkipReason::Rows)?
                }
            };

            let mut rows = Vec::new();
            // Where each accepted row was read from, so a row the ordering
            // gate rejects can be read a second time.
            let mut row_rects: Vec<((Side, u8), ptt_vision::PixelRect)> = Vec::new();
            let mut skipped = Vec::new();
            // The row directly above, per table: where its ink starts and what
            // rate it quoted. Both are what locates the aggregate row's
            // chevron; see `aggregate_chevron_zone`.
            let mut previous_row: Option<(
                super::Side,
                u8,
                usize,
                ptt_core::Decimal,
                ptt_core::Decimal,
            )> = None;
            for (band_index, row_band) in plan.bands.iter().enumerate() {
                // The plan's own index, not a running total. A dropped band --
                // the stub a wrapped rate leaves behind -- would otherwise
                // shift every row under it up by one, and rows filed under the
                // wrong rank are wrong in a way nothing downstream can see.
                let first_row_index = row_band.row_index;

                // Raw luminance beats the thresholded mask for OCR input:
                // Windows OCR's own binarization keeps the tiny ratio colon
                // that the mask erodes (same lesson as the name slots).
                let recognition = self
                    .worker
                    .recognize(
                        OcrLanguagePreference::English,
                        Self::upscaled_frame_rect_2x(frame, row_crops[band_index].source)?,
                    )
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
                let raw = recognition.text();
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!(
                        "debug row: {:?} #{first_row_index} crop y={} h={} raw={raw:?}",
                        row_band.side,
                        row_crops[band_index].source.y,
                        row_crops[band_index].source.height,
                    );
                }

                if row_band.estimated_rows > 1 {
                    previous_row = None;
                    skipped.push(RowSkip::MergedBand {
                        side: row_band.side,
                        first_row_index,
                        estimated_rows: row_band.estimated_rows,
                        raw,
                    });
                    continue;
                }
                // Windows OCR may return the row as one line ("<1:9.87 23,902")
                // or split ratio and stock into two lines by their horizontal
                // gap. Both are valid shapes; anything else is skipped.
                let windows = Self::split_row_fields(recognition.lines.as_slice());
                // Field by field, not row by row. The two engines fail at
                // different things: Windows OCR drops a short `1 : 3` on the
                // floor while reading `6,634` perfectly, and PP-OCRv5 reads
                // the ratio but turns the thousands comma into a decimal
                // point. Taking one engine's whole row therefore throws away a
                // field the other read exactly right -- every row skipped as
                // `raw: "6,634"` was a row the two of them had between them.
                //
                // Nothing loosens: each field still faces the same strict
                // grammar, and a field neither engine could read is still a
                // skipped row. The retry is only asked for when something is
                // actually missing.
                let parsed = if windows.0.is_some() && windows.1.is_some() {
                    windows
                } else {
                    // A complete read from one engine beats two halves from
                    // two, because halves can disagree about where the fields
                    // are. Windows OCR returned `1 : 22` for a row reading
                    // `1 : 22   2` -- the whole ratio and no stock -- and the
                    // split, having only spaces to go on, offered `22` as the
                    // stock. Pairing that with the retry's ratio produced a
                    // row where the stock was half of its own rate. The retry
                    // had read both fields correctly; it just was not asked.
                    //
                    // The tight content rect avoids the padded crop's bleed
                    // from neighbouring rows (partial digits read as junk).
                    // Mixing is still what saves the row neither engine read
                    // whole, which is the common case: Windows drops a short
                    // `1 : 3` and reads `6,634`, PP-OCRv5 reads the rate and
                    // writes the thousands comma as a decimal point.
                    let retry = self
                        .paddle_row_line(frame, &mask, row_crops[band_index].content, false)
                        .map_or((None, None), |line| Self::split_row_text(&line));
                    if retry.0.is_some() && retry.1.is_some() {
                        retry
                    } else {
                        (windows.0.or(retry.0), windows.1.or(retry.1))
                    }
                };
                // A slot holding more than one text line was not read, it was
                // read across. The grid slices arithmetically, so a wrapped
                // neighbour's second line hangs into this rectangle and both
                // engines splice it onto this row's own text: `135 : 1` with
                // the top of `152.75 :` beneath it came back as `1535 : 1`,
                // which parses, and which the row-order invariant cannot see
                // because it stays in order. That was an accepted wrong value.
                //
                // So when the slice is contaminated the arithmetic read is
                // discarded outright, however confident it looks, and the row
                // is read from its own line band instead. One band in the
                // slice is the overwhelming majority and takes the path above
                // untouched.
                let parsed = match parsed {
                    (Some(ratio), Some(stock)) => Ok((ratio, stock)),
                    // Incomplete through both engines, or read across two
                    // lines. Each shape has its own reader; a `None` from both
                    // means the slot really is unreadable, and skipping is the
                    // design.
                    _ => match self.layout.row_source {
                        crate::profiles::RowSource::FixedGrid(grid) => {
                            let slice_top = row_crops[band_index].source.y;
                            self.own_band_retry(frame, &mask, grid, slice_top)
                                .or_else(|| self.wrapped_row_rescue(frame, &mask, grid, slice_top))
                                .ok_or(FieldReject::Malformed)
                        }
                        crate::profiles::RowSource::DetectedBands => Err(FieldReject::Malformed),
                    },
                };
                match parsed {
                    Ok((mut ratio, stock)) => {
                        // Where the chevron is, measured off this frame.
                        //
                        // A pinned column cannot work here: the ratio and its
                        // chevron are right-aligned as one unit, so the glyph
                        // slides with the ratio's width. Measured across both
                        // corpora its left edge runs x=29 to x=39 and its
                        // right edge out to 47 — a fixed 24..36 window misses
                        // short ratios entirely and, because the comparator
                        // mask is more permissive than the row mask, catches
                        // the soft edge of a long ratio's leading digit and
                        // writes a comparator onto a row OCR never saw one on.
                        //
                        // What is stable is the panel's own structure. The
                        // aggregate row quotes the same rate as the row above
                        // it — it means "this rate and everything past it" —
                        // and the two are right-aligned, so the only ink the
                        // aggregate has that its neighbour lacks is the
                        // chevron. That makes the zone a subtraction:
                        // `[this row's leftmost ink, the row above's)`. It
                        // needs no constant, holds in either client, and
                        // declines by itself when the rates differ, which is
                        // what a mid-repaint frame looks like. Across 39
                        // last-slot rows it found all 37 real chevrons and
                        // rejected both torn frames.
                        //
                        // The same structure makes the last slot's aggregate
                        // mandatory rather than merely possible. Rows fill
                        // downward, so ink in the last slot means the table is
                        // full, and a full table's last row is the aggregate —
                        // true of all 39 last-slot rows on hand. A last row
                        // that cannot be confirmed as one is therefore a torn
                        // frame, not an ordinary order, and is skipped. Without
                        // that, the frame whose chevron and ratio repaint on
                        // separate lines came back as a plain `1 : 101`.
                        let last_slot = match self.layout.row_source {
                            crate::profiles::RowSource::FixedGrid(grid) => {
                                u32::from(first_row_index) + 1 == u32::from(grid.rows_per_side)
                            }
                            crate::profiles::RowSource::DetectedBands => false,
                        };
                        let aggregate_zone = match self.layout.row_source {
                            crate::profiles::RowSource::FixedGrid(_) => {
                                let here = row_crops[band_index].content.x;
                                previous_row.and_then(|(side, index, left, num, den)| {
                                    let adjacent = side == row_band.side
                                        && u32::from(index) + 1 == u32::from(first_row_index);
                                    let same_rate = num == ratio.left && den == ratio.right;
                                    // `then`, not `then_some`: the latter
                                    // evaluates its argument before testing
                                    // the condition, so `left - here` ran even
                                    // when the guard beside it said not to --
                                    // a debug-build panic, and in release a
                                    // wrapped width computed and discarded.
                                    (last_slot && adjacent && same_rate && left > here)
                                        .then(|| (here, left - here))
                                })
                            }
                            crate::profiles::RowSource::DetectedBands => {
                                // The same subtraction, minus the slot test:
                                // POE2's tables are not a fixed six rows, so
                                // where the aggregate sits is not known in
                                // advance -- but what it does is. It repeats
                                // the rate above it and stays right-aligned,
                                // and that is enough to place the chevron.
                                //
                                // This used to compare each row's ink against
                                // the median row's, and a row six pixels left
                                // of it was presumed to carry a comparator.
                                // Ink starts further left because the number
                                // is wider: `33.50 : 1` against `36 : 1` trips
                                // it, finds no chevron where it expected one,
                                // and the row -- read perfectly -- is thrown
                                // away. Four of the seven field frames lost
                                // rows exactly that way.
                                previous_row.and_then(|(side, index, left, num, den)| {
                                    let adjacent = side == row_band.side
                                        && u32::from(index) + 1 == u32::from(first_row_index);
                                    let same_rate = num == ratio.left && den == ratio.right;
                                    let here = row_crops[band_index].content.x;
                                    (adjacent && same_rate && left > here)
                                        .then(|| (here, left - here))
                                })
                            }
                        };
                        let mut comparator_ok = true;
                        // Trim the zone to the chevron itself before reading
                        // it; the gap to the ratio is what makes that safe.
                        let aggregate_zone = aggregate_zone.and_then(|(x, width)| {
                            crate::comparator::leading_ink_run(
                                comparator_mask.as_ref().unwrap_or(&mask),
                                x,
                                row_crops[band_index].source.y,
                                width,
                                row_crops[band_index].source.height,
                            )
                        });
                        if let Some((zone_x, zone_width)) = aggregate_zone {
                            // Read the glyph from the mask pixels; OCR is
                            // unreliable here. Cross-checks: the pixel class
                            // must match the table-side invariant (available
                            // boundary rows aggregate downward `<`, competing
                            // upward `>`) and must not contradict OCR.
                            let rect = row_crops[band_index].source;
                            let glyph = crate::comparator::classify_comparator(
                                comparator_mask.as_ref().unwrap_or(&mask),
                                zone_x,
                                rect.y,
                                zone_width,
                                rect.height,
                            );
                            let expected_by_side = match row_band.side {
                                Side::Available => Comparator::LessThan,
                                Side::Competing => Comparator::GreaterThan,
                            };
                            match glyph {
                                Some(read) if read == expected_by_side => match ratio.comparator {
                                    Comparator::Exact => {
                                        ratio.comparator = read;
                                        ratio.normalized =
                                            format!("{}{}", read.as_str(), ratio.normalized);
                                    }
                                    ocr if ocr == read => {}
                                    _ => comparator_ok = false,
                                },
                                _ => comparator_ok = false,
                            }
                        } else if ratio.comparator != Comparator::Exact || last_slot {
                            comparator_ok = false;
                        }
                        // Recorded whether or not the comparator check passed:
                        // the row below reads this one's geometry and rate, not
                        // its verdict. A row that failed to parse leaves this
                        // empty instead, since a rate nobody could read cannot
                        // anchor anything.
                        previous_row = Some((
                            row_band.side,
                            first_row_index,
                            row_crops[band_index].content.x,
                            ratio.left,
                            ratio.right,
                        ));
                        if comparator_ok {
                            row_rects.push((
                                (row_band.side, first_row_index),
                                row_crops[band_index].content,
                            ));
                            rows.push(RowObservation {
                                side: row_band.side,
                                row_index: first_row_index,
                                ratio,
                                stock,
                                band_fingerprint: row_band.band.content_fingerprint,
                            });
                        } else {
                            skipped.push(RowSkip::ComparatorUnverified {
                                side: row_band.side,
                                row_index: first_row_index,
                                raw,
                            });
                        }
                    }
                    Err(reject) => {
                        previous_row = None;
                        skipped.push(RowSkip::Grammar {
                            side: row_band.side,
                            row_index: first_row_index,
                            reject,
                            raw,
                        });
                    }
                }
            }

            if rows.is_empty() {
                return Err(SkipReason::EmptyBook);
            }
            // Checked before the book is built rather than after: a book that
            // contradicts itself should not exist long enough to be signed and
            // deduplicated, or a misread gets remembered as a distinct frame.
            //
            // The offending rows go, not the book. Eleven rows read correctly
            // are eleven rows the panel really shows, and throwing them away
            // over a twelfth is the same mistake as throwing away a frame over
            // one unreadable band.
            let contradictory = self.reread_contradictory_rows(frame, &mask, &mut rows, &row_rects);
            if !contradictory.is_empty() {
                rows.retain(|row| {
                    let keep = !contradictory.contains(&(row.side, row.row_index));
                    if !keep {
                        skipped.push(RowSkip::OutOfOrder {
                            side: row.side,
                            row_index: row.row_index,
                            ratio: row.ratio.normalized.clone(),
                        });
                    }
                    keep
                });
                if rows.is_empty() {
                    return Err(SkipReason::EmptyBook);
                }
            }
            let observation = BookObservation::assemble(
                BookIdentity {
                    need_asset_id: need.id.clone(),
                    have_asset_id: have.id.clone(),
                },
                rows,
                CaptureTimestamp {
                    wall_unix_ms: 0,
                    mono_ms: 0,
                },
            );
            Ok(RecognizedBook {
                observation,
                skipped_rows: skipped,
                need_text,
                have_text,
            })
        }
    }

    /// Contiguous ink-row runs, top to bottom, as inclusive row ranges. One
    /// run per rendered text line.
    ///
    /// The quantities involved sit an order of magnitude apart on the corpus
    /// frames, so these are regime boundaries, not tuning knobs: a text line
    /// is at least 13 rows tall with no blank row inside it, and stray marks
    /// (panel border residue, descender specks) are at most 3 rows tall — a
    /// run shorter than 8 rows is discarded as noise. A row joins a line
    /// only with at least 2 lit pixels: 1-pixel rows are antialiasing (the
    /// panel border leaves them above every table), while 2 is the width of
    /// the narrowest stroke the font draws — the digit `1`, measured at 2px
    /// across its middle rows. That glyph is exactly what a wrapped rate
    /// leaves alone on its second line, so a 3-pixel floor silently drops
    /// the tail and the whole row with it.
    ///
    /// How many blank rows may sit inside one line is the caller's, because
    /// it is a property of the region: a name slot's lines are 13 blank rows
    /// apart, so bridging 2 for antialiasing is safe, while a table row's
    /// neighbours come as close as 2 blank rows and must not be bridged.
    fn mask_line_bands(
        width: usize,
        height: usize,
        bridged_blank_rows: usize,
        ink_at: impl Fn(usize, usize) -> bool,
    ) -> Vec<(usize, usize)> {
        const MINIMUM_LINE_HEIGHT: usize = 8;
        const MINIMUM_LIT_PIXELS: usize = 2;
        let mut bands: Vec<(usize, usize)> = Vec::new();
        for row in 0..height {
            let lit = (0..width)
                .filter(|&column| ink_at(column, row))
                .take(MINIMUM_LIT_PIXELS)
                .count();
            if lit < MINIMUM_LIT_PIXELS {
                continue;
            }
            match bands.last_mut() {
                Some(band) if row - band.1 <= bridged_blank_rows + 1 => band.1 = row,
                _ => bands.push((row, row)),
            }
        }
        bands.retain(|&(top, bottom)| bottom - top + 1 >= MINIMUM_LINE_HEIGHT);
        bands
    }

    /// Whether the crop's mask ink attests two columns on one text line.
    ///
    /// Two independent conditions, and the repaint tear this guards against
    /// fails each for its own reason. First, the ink must form two column
    /// groups around a real gap (at least 12 blank columns — the rate/stock
    /// gap measures 40..85 against at most 8 inside a field): during a
    /// repaint one column is drawn too dim to reach the mask at all, so a
    /// crop whose mask holds only one column cannot vouch for a two-field
    /// read, no matter what the recognizer saw in the raw pixels. Second,
    /// the two groups' mean ink rows must agree within 6: healthy glyphs
    /// share a baseline (centers vary by at most ~4 rows), while the tear
    /// offsets the columns by half a row — 15.
    fn crop_sides_share_one_line(
        width: usize,
        height: usize,
        ink_at: impl Fn(usize, usize) -> bool,
    ) -> bool {
        let mut ink_columns = vec![false; width];
        for (x, seen) in ink_columns.iter_mut().enumerate() {
            *seen = (0..height).any(|y| ink_at(x, y));
        }
        let Some(first) = ink_columns.iter().position(|&seen| seen) else {
            return false;
        };
        let Some(last) = ink_columns.iter().rposition(|&seen| seen) else {
            return false;
        };
        let mut widest = (0_usize, 0_usize);
        let mut run_start = None;
        for (x, &lit) in ink_columns.iter().enumerate().take(last + 1).skip(first) {
            match (lit, run_start) {
                (false, None) => run_start = Some(x),
                (true, Some(start)) => {
                    if x - start > widest.1 {
                        widest = (start, x - start);
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        if widest.1 < 12 {
            return false;
        }
        let split = widest.0 + widest.1 / 2;
        // One contiguous ink-row run per side (a single blank row inside is
        // antialiasing), centered together. Runs, not just means: the tear
        // leaves the rate column as two half-clipped fragments — the row
        // above's tail and the row below's head — whose *mean* lands right
        // back at the stock's center. Two fragments are two runs, and no
        // number of aligned means makes them one text line.
        let side_run = |x0: usize, x1: usize| -> Option<(usize, usize)> {
            let mut run: Option<(usize, usize)> = None;
            for y in 0..height {
                if !(x0..x1).any(|x| ink_at(x, y)) {
                    continue;
                }
                match &mut run {
                    Some((_, bottom)) if y <= *bottom + 2 => *bottom = y,
                    Some(_) => return None,
                    None => run = Some((y, y)),
                }
            }
            run
        };
        let Some((left_top, left_bottom)) = side_run(0, split) else {
            return false;
        };
        let Some((right_top, right_bottom)) = side_run(split, width) else {
            return false;
        };
        let left_center = (left_top + left_bottom) / 2;
        let right_center = (right_top + right_bottom) / 2;
        left_center.abs_diff(right_center) <= 6
    }

    /// Whether a glyph box holds a colon, read from the mask itself: exactly
    /// two short ink runs of similar height with blank rows between them, in
    /// a box no wider than a dot. Measured on the corpus wrap heads: dots 3
    /// rows tall, 4 blank rows apart, in a 4-column box.
    fn glyph_box_is_colon(
        width: usize,
        height: usize,
        ink_at: impl Fn(usize, usize) -> bool,
    ) -> bool {
        if width > 8 {
            return false;
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for y in 0..height {
            if !(0..width).any(|x| ink_at(x, y)) {
                continue;
            }
            match runs.last_mut() {
                Some(run) if y == run.1 + 1 => run.1 = y,
                _ => runs.push((y, y)),
            }
        }
        matches!(
            runs.as_slice(),
            [(a0, a1), (b0, b1)]
                if (2..=6).contains(&(a1 - a0 + 1))
                    && (2..=6).contains(&(b1 - b0 + 1))
                    && b0 - a1 >= 3
        )
    }

    /// Joins a wrapped rate's two lines, or refuses.
    ///
    /// Only the two shapes a wrap can actually take are joined, both taken
    /// from corpus frames: the renderer breaks at a space, leaving a head
    /// that ends with the separator and a tail that is the whole right side
    /// (`1 :` + `102.50`), or it breaks a long decimal, leaving a bare
    /// decimal tail (`1 : 332` + `.46`). Everything else is refused — in
    /// particular a complete rate followed by another complete rate, which
    /// is what the zone holds when a neighbouring row's ink leaked in, and
    /// which would otherwise concatenate into a well-formed wrong value
    /// (`102.50` + `1:103` → `102.501:103`). The join proposes; the rate
    /// grammar still judges the result.
    ///
    /// The result must carry a decimal point, because that is why the row
    /// wrapped at all: a rate is drawn on two lines when it is too wide for
    /// its column, and what makes it too wide is the two fraction digits.
    /// Every wrapped rate on the corpus has one — 130.33, 102.50, 100.33,
    /// 122.33, 129.33, 152.75, 168.33 — and the one join that produced a
    /// point-free rate was a misread: `1 : 130.33` came back as `1 : 13033`,
    /// a hundredfold error that the grammar accepts and the row-order
    /// invariant cannot see, because dropping a point keeps the ordering.
    /// The point is not always recoverable — on that frame it does not reach
    /// the warm mask at all — so the row is refused rather than repaired.
    fn join_wrapped_rate(head: &str, tail: &str) -> Option<String> {
        let whole_right_side = tail.chars().any(|c| c.is_ascii_digit())
            && tail
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '.');
        let decimal_tail = tail.strip_prefix('.').is_some_and(|digits| {
            !digits.is_empty() && digits.len() <= 2 && digits.chars().all(|c| c.is_ascii_digit())
        });
        let breaks_at_the_separator = head.ends_with(':') && whole_right_side;
        let breaks_inside_the_decimal = decimal_tail && !head.ends_with(':');
        if !breaks_at_the_separator && !breaks_inside_the_decimal {
            return None;
        }
        let joined = format!("{head}{tail}");
        joined.contains('.').then_some(joined)
    }

    /// The catalog index of the single strongly supported target, or nothing.
    ///
    /// `target_supports` is deduplicated by canonical text, so positions in
    /// it do not line up with the catalog: 58 of the 660 POE2 names collapse
    /// into earlier entries (every per-level 「等級N」 family shares one
    /// canonical form, because numbers canonicalize away), and indexing the
    /// catalog with a support position handed back an asset up to 58 entries
    /// early — a confidently wrong identity. The winner is matched back to
    /// the catalog by its canonical text instead, and a canonical form
    /// shared by several assets is refused as ambiguous: the evidence that
    /// crowned it cannot tell those assets apart either.
    fn unique_winner_asset(
        supports: &[ptt_ocr_onnx::CtcTargetEvaluation],
        matchers: &[ptt_core::FullLineAffixMatcher],
    ) -> Option<usize> {
        let mut winners = supports
            .iter()
            .filter(|entry| entry.support.strongly_supported);
        let winner = winners.next()?;
        if winners.next().is_some() {
            return None;
        }
        let mut assets = matchers
            .iter()
            .enumerate()
            .filter(|(_, matcher)| matcher.template().text == winner.canonical_target)
            .map(|(index, _)| index);
        let index = assets.next()?;
        if assets.next().is_some() {
            return None;
        }
        Some(index)
    }

    #[cfg(test)]
    mod tests {
        use super::{
            crop_sides_share_one_line, glyph_box_is_colon, join_wrapped_rate, mask_line_bands,
            unique_winner_asset,
        };
        use ptt_ocr_onnx::{CtcTargetEvaluation, CtcTargetSupport, CtcTargetSupportReason};

        /// A healthy row's two columns share a baseline; the corpus tear
        /// offsets them by half a row (15px) and must be refused.
        #[test]
        fn torn_half_row_columns_are_not_one_line() {
            let healthy = |x: usize, y: usize| (5..23).contains(&y) && !(60..120).contains(&x);
            assert!(crop_sides_share_one_line(180, 28, healthy));
            let torn = |x: usize, y: usize| {
                if x < 60 {
                    y < 13 // rate column drawn half a row high
                } else if x >= 120 {
                    (10..28).contains(&y)
                } else {
                    false
                }
            };
            assert!(!crop_sides_share_one_line(180, 28, torn));
            let empty_side = |x: usize, y: usize| x < 60 && (5..23).contains(&y);
            assert!(
                !crop_sides_share_one_line(180, 28, empty_side),
                "one column of mask ink cannot vouch for a two-field read"
            );
            let no_gap = |x: usize, y: usize| (5..23).contains(&y) && x < 170;
            assert!(
                !crop_sides_share_one_line(180, 28, no_gap),
                "without a column gap there is nothing to split"
            );
            // The corpus tear's exact shape: the rate column shows the row
            // above's tail and the row below's head, whose mean lands back
            // at the stock's center.
            let bimodal = |x: usize, y: usize| {
                if x < 60 {
                    !(8..21).contains(&y)
                } else if x >= 120 {
                    (5..23).contains(&y)
                } else {
                    false
                }
            };
            assert!(
                !crop_sides_share_one_line(180, 28, bimodal),
                "two rate fragments are not one text line however their mean falls"
            );
        }

        /// The corpus colon (two 3-row dots, 4 blank rows apart, 4 columns
        /// wide) classifies; a period, a bar, and anything wide do not.
        #[test]
        fn a_colon_is_two_dot_runs_and_nothing_else_is() {
            let colon = |_x: usize, y: usize| (3..6).contains(&y) || (10..13).contains(&y);
            assert!(glyph_box_is_colon(4, 15, colon));
            let period = |_x: usize, y: usize| (10..13).contains(&y);
            assert!(!glyph_box_is_colon(4, 15, period));
            let bar = |_x: usize, y: usize| (2..13).contains(&y);
            assert!(!glyph_box_is_colon(4, 15, bar));
            let touching = |_x: usize, y: usize| (3..8).contains(&y) || (9..13).contains(&y);
            assert!(
                !glyph_box_is_colon(4, 15, touching),
                "one blank row is antialiasing, not a gap"
            );
            assert!(
                !glyph_box_is_colon(12, 15, colon),
                "a colon is never this wide"
            );
        }

        /// The two corpus wrap shapes join; every neighbour-contamination
        /// shape refuses — most importantly two complete rates, which would
        /// concatenate into a well-formed wrong value.
        #[test]
        fn wrapped_rates_join_only_in_the_two_wrap_shapes() {
            assert_eq!(
                join_wrapped_rate("1:", "102.50").as_deref(),
                Some("1:102.50")
            );
            assert_eq!(
                join_wrapped_rate("1:332", ".46").as_deref(),
                Some("1:332.46")
            );
            assert_eq!(
                join_wrapped_rate("102.50", "1:103"),
                None,
                "a neighbour's complete rate must never be absorbed"
            );
            assert_eq!(join_wrapped_rate("1:102", "103"), None);
            assert_eq!(join_wrapped_rate("1:", ".."), None);
            assert_eq!(join_wrapped_rate("1:", "10a"), None);
            assert_eq!(join_wrapped_rate("1:332", ".461"), None);
            assert_eq!(join_wrapped_rate("1:", ""), None);
        }

        /// A wrap without a decimal point is a lost decimal point, not a
        /// wide integer: the fraction digits are why the rate did not fit.
        #[test]
        fn a_point_free_join_is_refused() {
            assert_eq!(
                join_wrapped_rate("1:", "13033"),
                None,
                "1 : 130.33 misread as 1 : 13033 is a hundredfold error the \
                 grammar accepts and row order cannot see"
            );
            assert_eq!(join_wrapped_rate("129:", "1"), None);
            assert_eq!(
                join_wrapped_rate("129.33:", "1").as_deref(),
                Some("129.33:1"),
                "the same shape with its point intact still joins"
            );
        }

        /// The wrapped tail is the narrowest glyph the panel draws, so the
        /// per-row floor decides whether the row survives at all.
        #[test]
        fn a_two_pixel_stroke_is_a_text_line() {
            // A digit `1`: 2 lit pixels per row, 12 rows tall, the way it
            // measures on the corpus frame.
            let tail = |x: usize, y: usize| (85..97).contains(&y) && (101..103).contains(&x);
            assert_eq!(
                mask_line_bands(136, 120, 0, tail),
                vec![(85, 96)],
                "a 2px stroke is the wrapped tail, not noise"
            );
            // One lit pixel is antialiasing and must not become a line, nor
            // bridge across to one.
            let speck = |x: usize, y: usize| y == 60 && x == 50;
            assert!(mask_line_bands(136, 120, 0, speck).is_empty());
        }

        /// The corpus geometry in miniature: a 3-row border speck separated
        /// by a blank row, two 20-row lines 13 rows apart, and a 1-row gap
        /// inside the second line that must not split it.
        #[test]
        fn line_bands_split_on_gaps_and_drop_border_specks() {
            let inky_rows: Vec<usize> = (5..8).chain(9..29).chain(42..50).chain(51..62).collect();
            let bands = mask_line_bands(240, 66, 2, |_, row| inky_rows.contains(&row));
            assert_eq!(bands, vec![(5, 28), (42, 61)]);
        }

        #[test]
        fn a_single_line_is_a_single_band() {
            let bands = mask_line_bands(240, 66, 2, |_, row| (24..=45).contains(&row));
            assert_eq!(bands, vec![(24, 45)]);
        }

        fn support(strongly_supported: bool) -> CtcTargetSupport {
            CtcTargetSupport {
                shape_compatible: strongly_supported,
                strongly_supported,
                mismatches: Vec::new(),
                reason: CtcTargetSupportReason::CanonicalTextMatches,
            }
        }

        /// Builds the deduplicated support list exactly the way
        /// `analyze_set` does, so the test exercises the real misalignment.
        fn deduplicated_supports(
            matchers: &[ptt_core::FullLineAffixMatcher],
        ) -> Vec<CtcTargetEvaluation> {
            let mut seen = std::collections::HashSet::new();
            let mut supports = Vec::new();
            for matcher in matchers {
                let canonical = matcher.template().text.clone();
                if !seen.insert(canonical.clone()) {
                    continue;
                }
                supports.push(CtcTargetEvaluation {
                    source_template: matcher.source_template().to_owned(),
                    canonical_target: canonical,
                    support: support(false),
                });
            }
            supports
        }

        fn poe2_matchers() -> Vec<ptt_core::FullLineAffixMatcher> {
            ptt_catalog::poe2()
                .assets()
                .iter()
                .map(|asset| {
                    ptt_core::FullLineAffixMatcher::new(&asset.name_zh_tw)
                        .expect("catalog names build matchers")
                })
                .collect()
        }

        #[test]
        fn winner_is_resolved_by_canonical_text_not_by_position() {
            let catalog = ptt_catalog::poe2();
            let matchers = poe2_matchers();
            let mut supports = deduplicated_supports(&matchers);
            assert!(
                supports.len() < matchers.len(),
                "the dedup this mapping guards against must actually occur"
            );
            let asset_index = catalog
                .assets()
                .iter()
                .position(|asset| asset.name_zh_tw == "瓦爾護甲鍛造師的灌注器")
                .expect("the corpus currency exists");
            let canonical = matchers[asset_index].template().text.clone();
            let position = supports
                .iter()
                .position(|entry| entry.canonical_target == canonical)
                .expect("a unique name survives dedup");
            assert_ne!(
                position, asset_index,
                "positions must disagree for this test to prove anything"
            );
            supports[position].support = support(true);
            assert_eq!(
                unique_winner_asset(&supports, &matchers),
                Some(asset_index),
                "the winner must map back to its own asset, not the one at its position"
            );
        }

        #[test]
        fn winner_shared_by_a_per_level_family_is_refused() {
            let matchers = poe2_matchers();
            let mut supports = deduplicated_supports(&matchers);
            let position = supports
                .iter()
                .position(|entry| entry.source_template.starts_with("奇術溶劑"))
                .expect("the per-level family exists");
            supports[position].support = support(true);
            assert_eq!(
                unique_winner_asset(&supports, &matchers),
                None,
                "evidence that cannot tell family members apart must not pick one"
            );
        }

        #[test]
        fn two_strong_winners_are_refused() {
            let matchers = poe2_matchers();
            let mut supports = deduplicated_supports(&matchers);
            let unique: Vec<usize> = supports
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.source_template.contains('（'))
                .map(|(index, _)| index)
                .take(2)
                .collect();
            for &index in &unique {
                supports[index].support = support(true);
            }
            assert_eq!(unique_winner_asset(&supports, &matchers), None);
        }
    }
}
