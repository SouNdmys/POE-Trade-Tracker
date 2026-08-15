//! POE2 Traditional Chinese profile: default 2560×1440 geometry plus the
//! Windows OCR route from a decoded frame to a `BookObservation`.
//!
//! Defaults come from the calibrated corpus (docs/P1-CALIBRATION-NOTES.md).
//! Users on other resolutions calibrate their own regions; these constants
//! are the factory preset, not a requirement.

use crate::fields::FieldReject;
use crate::rows::{RowLayout, RowsReject, Side};

/// (x, y, width, height) desktop-pixel presets for 2560×1440 windowed
/// fullscreen with the exchange panel in its centered default position.
pub const TABLES_REGION: (i32, i32, u32, u32) = (1150, 220, 320, 560);
/// "I need" name text, icon excluded.
pub const NEED_NAME_REGION: (i32, i32, u32, u32) = (855, 296, 240, 52);
/// "I have" name text, icon and the right-edge favorite star excluded.
pub const HAVE_NAME_REGION: (i32, i32, u32, u32) = (1520, 296, 210, 52);

pub fn default_row_layout() -> RowLayout {
    RowLayout::default()
}

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

#[cfg(windows)]
pub use windows_route::{RecognizedBook, Route};

#[cfg(windows)]
mod windows_route {
    use super::{
        HAVE_NAME_REGION, NEED_NAME_REGION, RowSkip, SkipReason, TABLES_REGION, default_row_layout,
    };
    use crate::book::{BookIdentity, BookObservation, RowObservation};
    use crate::fields::{Comparator, FieldReject, parse_ratio, parse_stock, split_row_line};
    use crate::rows::{BandGeometry, classify_rows};
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
        /// `ptt_catalog::poe2().assets()`.
        zh_matchers: Vec<ptt_core::FullLineAffixMatcher>,
    }

    impl Route {
        pub fn new() -> Result<Self, SkipReason> {
            let paddle = Self::start_paddle_session();
            if paddle.is_none() {
                eprintln!(
                    "warning: ONNX name fallback unavailable                      (onnxruntime.dll not found; set PTT_ONNXRUNTIME_DLL)"
                );
            }
            let zh_matchers = ptt_catalog::poe2()
                .assets()
                .iter()
                .map(|asset| {
                    ptt_core::FullLineAffixMatcher::new(&asset.name_zh_tw)
                        .map_err(|error| SkipReason::Ocr(format!("matcher: {error:?}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                worker: OcrWorker::start()
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?,
                decoder: WicScreenshotDecoder::new()
                    .map_err(|error| SkipReason::Decode(format!("{error:?}")))?,
                paddle,
                zh_matchers,
            })
        }

        fn start_paddle_session() -> Option<std::sync::Mutex<ptt_ocr_onnx::PaddleCtcSession>> {
            let dll = std::env::var_os("PTT_ONNXRUNTIME_DLL")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|exe| Some(exe.parent()?.join("onnxruntime.dll")))
                })
                .filter(|path| path.is_file())?;
            ptt_ocr_onnx::initialize_onnx_runtime(&dll).ok()?;
            let assets = ptt_ocr_onnx::PaddleAssets::load_source_tree().ok()?;
            let session = ptt_ocr_onnx::PaddleCtcSession::from_assets(
                &assets,
                ptt_ocr_onnx::PaddleSessionConfig::default(),
            )
            .ok()?;
            Some(std::sync::Mutex::new(session))
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
                .recognize_batch(view, &self.zh_matchers)
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

        /// Preset region, overridable via `PTT_POE2_<NAME>_ROI=x,y,w,h` for
        /// calibration experiments without recompiling.
        fn region(name: &str, preset: (i32, i32, u32, u32)) -> CaptureRegion {
            let from_env = std::env::var(format!("PTT_POE2_{name}_ROI"))
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
            path: &std::path::Path,
            preset: (&str, (i32, i32, u32, u32)),
            catalog: &'catalog ptt_catalog::Catalog,
        ) -> Result<(Option<&'catalog ptt_catalog::CatalogAsset>, String), SkipReason> {
            let frame = self
                .decoder
                .decode(path, Some(Self::region(preset.0, preset.1)))
                .map_err(|error| SkipReason::Decode(format!("{error:?}")))?;
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
                        OcrLanguagePreference::TraditionalChinese,
                        Self::upscaled_frame_rect(&frame, rect, factor)?,
                    )
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
                let text = recognition.text();
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!(
                        "debug {} x{}: lines={} text={:?}",
                        preset.0,
                        factor,
                        recognition.lines.len(),
                        text
                    );
                }
                if let Some(asset) = crate::identity::resolve_zh_name(&text, catalog) {
                    return Ok((Some(asset), text));
                }
                if !text.trim().is_empty() {
                    last_text = text;
                }
            }
            if let Some((asset, text)) = self.paddle_name_fallback(&frame, catalog) {
                if std::env::var_os("PTT_DEBUG_OCR").is_some() {
                    eprintln!("debug {} paddle: text={:?} -> {}", preset.0, text, asset.id);
                }
                return Ok((Some(asset), text));
            }
            Ok((None, last_text))
        }

        /// Full offline route over one screenshot file.
        pub fn recognize_screenshot(
            &self,
            path: &std::path::Path,
        ) -> Result<RecognizedBook, SkipReason> {
            let catalog = ptt_catalog::poe2();

            let (need, need_text) =
                self.ocr_name_resolved(path, ("NEED", NEED_NAME_REGION), catalog)?;
            let need = need.ok_or(SkipReason::NeedNameUnresolved {
                text: need_text.clone(),
            })?;
            let (have, have_text) =
                self.ocr_name_resolved(path, ("HAVE", HAVE_NAME_REGION), catalog)?;
            let have = have.ok_or(SkipReason::HaveNameUnresolved {
                text: have_text.clone(),
            })?;

            let frame = self
                .decoder
                .decode(path, Some(Self::region("TABLES", TABLES_REGION)))
                .map_err(|error| SkipReason::Decode(format!("{error:?}")))?;
            let mask = build_warm_mask(&frame, WarmMaskSettings::default());
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
            let plan = classify_rows(&geometry, &default_row_layout()).map_err(SkipReason::Rows)?;

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
            for (band_index, row_band) in plan.bands.iter().enumerate() {
                let index = match row_band.side {
                    super::Side::Available => &mut available_index,
                    super::Side::Competing => &mut competing_index,
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
                        Self::upscaled_frame_rect_2x(
                            &frame,
                            detection.bands[band_index].crop.source_rect,
                        )?,
                    )
                    .map_err(|error| SkipReason::Ocr(format!("{error:?}")))?;
                let raw = recognition.text();

                if row_band.estimated_rows > 1 {
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
                if recognition.lines.len() > 2 {
                    skipped.push(RowSkip::LineCount {
                        side: row_band.side,
                        row_index: first_row_index,
                        raw,
                    });
                    continue;
                }
                match parsed {
                    Ok((ratio, stock)) => {
                        let expects_comparator = row_band.band.left + 6 < median_left;
                        let comparator_consistent = match ratio.comparator {
                            Comparator::Exact => !expects_comparator,
                            _ => expects_comparator,
                        };
                        if comparator_consistent {
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
                    Err(reject) => skipped.push(RowSkip::Grammar {
                        side: row_band.side,
                        row_index: first_row_index,
                        reject,
                        raw,
                    }),
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
