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
            let batch = paddle
                .lock()
                .expect("paddle session lock")
                .recognize_batch(view, &self.name_matchers)
                .ok()?;
            let mut winners = batch
                .target_supports
                .iter()
                .enumerate()
                .filter(|(_, support)| support.support.strongly_supported)
                .map(|(index, _)| index);
            let winner = winners.next()?;
            if winners.next().is_some() {
                return None;
            }
            Some((&catalog.assets()[winner], batch.recognition.text))
        }

        /// PP-OCRv5 fallback for a row whose Windows OCR text failed the
        /// grammar (highlighted market-rate rows lose their "1:", tiny
        /// stocks vanish, `1` reads as `I`). Runs greedy CTC on the raw
        /// luminance crop and splits ratio|stock on the spatial gap between
        /// emission time steps. The same strict grammar re-validates the
        /// result, so this recovers rows without loosening any gate.
        fn paddle_row_line(
            &self,
            frame: &ptt_vision::CapturedFrame,
            rect: ptt_vision::PixelRect,
        ) -> Option<String> {
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
            let view =
                ptt_ocr_onnx::ImageView::gray8(rect.width, rect.height, rect.width, &gray).ok()?;
            let recognition = paddle
                .lock()
                .expect("paddle session lock")
                .recognize(view)
                .ok()?;

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
                eprintln!("debug paddle-row: tokens={tokens:?}");
            }
            // A leading bare comparator is part of the ratio.
            if tokens.len() > 2 && (tokens[0] == "<" || tokens[0] == ">") {
                let comparator = tokens.remove(0);
                tokens[0] = format!("{comparator}{}", tokens[0]);
            }
            // Exactly ratio + stock, nothing else: a stray token (e.g. a
            // neighbouring row's partial digit) must never be mistaken for a
            // stock value, so anything but two columns is rejected.
            if tokens.len() != 2 {
                return None;
            }
            Some(tokens.join(" "))
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
                    let plan = classify_rows(&geometry, &(self.layout.rows)())
                        .map_err(SkipReason::Rows)?;
                    let crops = detection
                        .bands
                        .iter()
                        .map(|band| RowCrops {
                            source: band.crop.source_rect,
                            content: band.crop.content_rect,
                        })
                        .collect::<Vec<_>>();
                    (plan, crops)
                }
                crate::profiles::RowSource::FixedGrid(grid) => {
                    Self::plan_fixed_grid(&mask, grid).map_err(SkipReason::Rows)?
                }
            };

            // Boundary rows (the `<`/`>` comparator rows) start further left
            // than the column norm. Until a template classifier lands, a row
            // whose geometry demands a comparator but whose OCR read none is
            // skipped — accepting it would silently drop the boundary flag.
            let mut single_lefts: Vec<i32> = plan
                .bands
                .iter()
                .filter(|band| band.estimated_rows == 1)
                .map(|band| band.band.left)
                .collect();
            single_lefts.sort_unstable();
            let median_left = single_lefts
                .get(single_lefts.len() / 2)
                .copied()
                .unwrap_or(i32::MIN);

            let mut rows = Vec::new();
            let mut skipped = Vec::new();
            let mut available_index: u8 = 0;
            let mut competing_index: u8 = 0;
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
                let index = match row_band.side {
                    Side::Available => &mut available_index,
                    Side::Competing => &mut competing_index,
                };
                let first_row_index = *index;
                *index += row_band.estimated_rows;

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
                let parsed = match recognition.lines.as_slice() {
                    [single] => {
                        split_row_line(&single.text).and_then(|(ratio_text, stock_text)| {
                            Ok((parse_ratio(&ratio_text)?, parse_stock(&stock_text)?))
                        })
                    }
                    [first, second] => {
                        let (ratio_line, stock_line) = if first.left <= second.left {
                            (first, second)
                        } else {
                            (second, first)
                        };
                        // A lone ratio line cannot glue with a wrapped
                        // fragment (those arrive as merged bands), so
                        // whitespace inside it is safe to drop here.
                        let ratio_text: String = ratio_line
                            .text
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect();
                        parse_ratio(&ratio_text)
                            .and_then(|ratio| Ok((ratio, parse_stock(&stock_line.text)?)))
                    }
                    _ => Err(FieldReject::Malformed),
                };
                // Grammar-failed Windows reads get one PP-OCRv5 retry; the
                // same strict grammar re-validates, so nothing loosens.
                let parsed = parsed.or_else(|windows_reject| {
                    // The tight content rect avoids the padded crop's bleed
                    // from neighbouring rows (partial digits read as junk).
                    self.paddle_row_line(frame, row_crops[band_index].content)
                        .and_then(|line| {
                            split_row_line(&line)
                                .and_then(|(ratio_text, stock_text)| {
                                    Ok((parse_ratio(&ratio_text)?, parse_stock(&stock_text)?))
                                })
                                .ok()
                        })
                        .ok_or(windows_reject)
                });
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
                                let rect = row_crops[band_index].source;
                                (row_band.band.left + 6 < median_left).then(|| {
                                    (
                                        rect.x,
                                        usize::try_from(median_left - row_band.band.left + 8)
                                            .unwrap_or(0),
                                    )
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
}
