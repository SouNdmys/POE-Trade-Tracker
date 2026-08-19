//! Windows live-watch driver: GDI capture → gate → double-read recognition.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use ptt_recognition::route::{RecognizedBook, Route, SkipReason};
use ptt_vision::{
    CaptureRegion, CapturedFrame, GdiScreenCapture, ScreenCapture, TextInkMask, WarmMaskSettings,
    build_warm_mask_into,
};

#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Pace between captures when nothing changed (POE Alarm's cached pace).
    pub idle_delay: Duration,
    /// Pace between the first sighting of a change and the stability recheck.
    pub stability_delay: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_delay: Duration::from_millis(8),
            stability_delay: Duration::from_millis(30),
        }
    }
}

#[derive(Debug)]
pub enum SessionEvent {
    Accepted {
        book: Box<RecognizedBook>,
        elapsed: Duration,
        /// Wall time the first confirmed frame was captured (not dispatched).
        captured_at: SystemTime,
        /// SHA-256 of the two independently captured tables frames that
        /// agreed — the double-read evidence, one digest per read.
        frame_hashes: [String; 2],
    },
    FrameSkipped {
        reason: SkipReason,
    },
    /// Two back-to-back reads of one stable frame disagreed (OCR jitter).
    ConfirmationMismatch,
    Duplicate,
    CaptureError(String),
}

#[derive(Debug, Default)]
pub struct SessionStats {
    pub ticks: u64,
    pub idle_ticks: u64,
    pub recognitions: u64,
    pub accepted: u64,
    pub duplicates: u64,
    pub confirmation_mismatches: u64,
    pub frame_skips: BTreeMap<String, u64>,
    pub capture_errors: u64,
}

/// The stable bucket name for a skip reason.
///
/// Public so callers group skips by the same key the session stats use,
/// rather than formatting the reason with `Debug` and parsing it back.
#[must_use]
pub fn skip_key(reason: &SkipReason) -> String {
    match reason {
        SkipReason::Decode(_) => "decode".into(),
        SkipReason::Ocr(_) => "ocr".into(),
        SkipReason::NeedNameUnresolved { .. } => "need-name".into(),
        SkipReason::HaveNameUnresolved { .. } => "have-name".into(),
        SkipReason::Rows(reject) => format!("rows:{reject:?}")
            .split(' ')
            .next()
            .unwrap_or("rows")
            .trim_end_matches('{')
            .to_string(),
        SkipReason::EmptyBook => "empty-book".into(),
        SkipReason::RowsOutOfOrder(_) => "rows-out-of-order".into(),
    }
}

/// Runs the watch loop until `cancel` is set or `deadline` elapses.
/// Emits every event to `on_event`; returns the tallied stats.
pub fn run_session(
    route: &Route,
    config: &SessionConfig,
    deadline: Duration,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(SessionEvent),
) -> SessionStats {
    let (need_region, have_region, tables_region) = route.regions();
    let mut capture = GdiScreenCapture::new();
    let mut tables_frame = CapturedFrame::default();
    let mut need_frame = CapturedFrame::default();
    let mut have_frame = CapturedFrame::default();
    let mut tables_frame_second = CapturedFrame::default();
    let mut need_frame_second = CapturedFrame::default();
    let mut have_frame_second = CapturedFrame::default();
    let mut mask = TextInkMask::default();
    let mut mask_second = TextInkMask::default();
    let mut gate = crate::MonitorGate::new();
    let mut stats = SessionStats::default();
    let started = Instant::now();

    while !cancel.load(Ordering::Relaxed) && started.elapsed() < deadline {
        stats.ticks += 1;
        if let Err(error) = capture.capture_into(tables_region, &mut tables_frame) {
            stats.capture_errors += 1;
            on_event(SessionEvent::CaptureError(error.to_string()));
            std::thread::sleep(config.idle_delay.max(Duration::from_millis(100)));
            continue;
        }
        build_warm_mask_into(&tables_frame, WarmMaskSettings::default(), &mut mask);

        match gate.on_fingerprint(mask.fingerprint()) {
            crate::GateStep::Idle => {
                stats.idle_ticks += 1;
                std::thread::sleep(config.idle_delay);
                continue;
            }
            crate::GateStep::AwaitStability => {
                std::thread::sleep(config.stability_delay);
                continue;
            }
            crate::GateStep::Recognize => {}
        }

        let recognize = |need: &CapturedFrame, have: &CapturedFrame, tables: &CapturedFrame| {
            route.recognize_frames(need, have, tables)
        };
        let outcome = (|| -> Result<SessionEvent, SessionEvent> {
            let recognition_started = Instant::now();
            capture_pair(&mut capture, need_region, &mut need_frame)
                .and_then(|()| capture_pair(&mut capture, have_region, &mut have_frame))
                .map_err(SessionEvent::CaptureError)?;

            let first = recognize(&need_frame, &have_frame, &tables_frame)
                .map_err(|reason| SessionEvent::FrameSkipped { reason })?;
            // Double-read confirmation on a SECOND, independent capture:
            // re-reading the same buffers could never disagree with itself,
            // so the confirmation pass gets fresh pixels. The screen must
            // still show the same content (fingerprint check) and the two
            // recognitions must agree on the content signature.
            capture_pair(&mut capture, tables_region, &mut tables_frame_second)
                .and_then(|()| capture_pair(&mut capture, need_region, &mut need_frame_second))
                .and_then(|()| capture_pair(&mut capture, have_region, &mut have_frame_second))
                .map_err(SessionEvent::CaptureError)?;
            build_warm_mask_into(
                &tables_frame_second,
                WarmMaskSettings::default(),
                &mut mask_second,
            );
            if mask_second.fingerprint() != mask.fingerprint() {
                return Err(SessionEvent::ConfirmationMismatch);
            }
            let second = recognize(&need_frame_second, &have_frame_second, &tables_frame_second)
                .map_err(|reason| SessionEvent::FrameSkipped { reason })?;
            if first.observation.signature != second.observation.signature {
                return Err(SessionEvent::ConfirmationMismatch);
            }
            Ok(SessionEvent::Accepted {
                book: Box::new(first),
                elapsed: recognition_started.elapsed(),
                captured_at: tables_frame.captured_at(),
                frame_hashes: [
                    frame_sha256(&tables_frame),
                    frame_sha256(&tables_frame_second),
                ],
            })
        })();

        stats.recognitions += 1;
        gate.mark_handled();
        match outcome {
            Ok(SessionEvent::Accepted {
                book,
                elapsed,
                captured_at,
                frame_hashes,
            }) => {
                if gate.should_emit(book.observation.signature) {
                    stats.accepted += 1;
                    on_event(SessionEvent::Accepted {
                        book,
                        elapsed,
                        captured_at,
                        frame_hashes,
                    });
                } else {
                    stats.duplicates += 1;
                    on_event(SessionEvent::Duplicate);
                }
            }
            Ok(_) => unreachable!("closure only returns Accepted on success"),
            Err(SessionEvent::FrameSkipped { reason }) => {
                *stats.frame_skips.entry(skip_key(&reason)).or_default() += 1;
                on_event(SessionEvent::FrameSkipped { reason });
            }
            Err(SessionEvent::ConfirmationMismatch) => {
                stats.confirmation_mismatches += 1;
                on_event(SessionEvent::ConfirmationMismatch);
            }
            Err(SessionEvent::CaptureError(error)) => {
                stats.capture_errors += 1;
                on_event(SessionEvent::CaptureError(error));
            }
            Err(_) => unreachable!("closure only returns the variants above"),
        }
    }
    stats
}

/// SHA-256 over the frame's pixel buffer, hex-encoded — real image digests
/// for the provenance frame-hash slots.
fn frame_sha256(frame: &CapturedFrame) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(frame.bgra_pixels());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").expect("string write");
    }
    hex
}

fn capture_pair(
    capture: &mut GdiScreenCapture,
    region: CaptureRegion,
    frame: &mut CapturedFrame,
) -> Result<(), String> {
    capture
        .capture_into(region, frame)
        .map_err(|error| error.to_string())
}
