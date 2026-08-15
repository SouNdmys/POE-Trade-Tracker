//! Watch-loop backend: one worker thread running the full pipeline, a
//! bounded event channel toward the UI. Start/stop is cancellation-based —
//! native OCR calls are never force-killed, the loop simply stops pacing.

#[cfg(windows)]
pub use windows_backend::{Backend, UiEvent};

#[cfg(windows)]
mod windows_backend {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use ptt_monitoring::{SessionConfig, SessionEvent, run_session};
    use ptt_recognition::profiles::poe2_zhtw::Route;
    use ptt_runtime::live::{capture_from_book, domain_asset_id, poe2_live_context};
    use ptt_storage::MarketStore;

    #[derive(Debug)]
    pub enum UiEvent {
        Accepted {
            header: String,
            rows: Vec<String>,
            analysis: Vec<String>,
        },
        Skipped(String),
        Fault(String),
        Stopped,
    }

    pub struct Backend {
        events: Receiver<UiEvent>,
        cancel: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl Backend {
        pub fn start() -> Self {
            let (sender, events) = channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            let worker = std::thread::Builder::new()
                .name("ptt-watch".to_owned())
                .spawn(move || run_watch(&worker_cancel, &sender))
                .expect("spawn watch thread");
            Self {
                events,
                cancel,
                worker: Some(worker),
            }
        }

        /// Signals the loop to stop; the thread winds down on its own within
        /// one pacing interval plus any in-flight OCR call.
        pub fn stop(&mut self) {
            self.cancel.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }

        pub fn drain_events(&self) -> Vec<UiEvent> {
            let mut drained = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                drained.push(event);
            }
            drained
        }
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn run_watch(cancel: &AtomicBool, sender: &Sender<UiEvent>) {
        let route = match Route::new() {
            Ok(route) => route,
            Err(reason) => {
                let _ = sender.send(UiEvent::Fault(format!("route init failed: {reason:?}")));
                return;
            }
        };
        let db_path = format!(
            "{}\\PoeTradeTracker\\market.sqlite",
            std::env::var("LOCALAPPDATA").unwrap_or_default()
        );
        if let Some(parent) = std::path::Path::new(&db_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut store = match MarketStore::open(&db_path) {
            Ok(store) => store,
            Err(error) => {
                let _ = sender.send(UiEvent::Fault(format!("storage open failed: {error}")));
                return;
            }
        };
        let context = match poe2_live_context("live-league") {
            Ok(context) => context,
            Err(error) => {
                let _ = sender.send(UiEvent::Fault(format!("context failed: {error:?}")));
                return;
            }
        };
        let context_key = context.stable_key();
        let mut sequence: u64 = 0;

        run_session(
            &route,
            &SessionConfig::default(),
            // Effectively unbounded; stop is cancellation-driven.
            Duration::from_secs(60 * 60 * 24),
            cancel,
            |event| match event {
                SessionEvent::Accepted { book, elapsed } => {
                    sequence += 1;
                    let need_id = book.observation.identity.need_asset_id.clone();
                    let have_id = book.observation.identity.have_asset_id.clone();
                    let header = format!(
                        "#{sequence} [{:.0}ms] {} -> {} ({} rows)",
                        elapsed.as_secs_f64() * 1e3,
                        need_id,
                        have_id,
                        book.observation.rows.len(),
                    );
                    let rows: Vec<String> = book
                        .observation
                        .rows
                        .iter()
                        .map(|row| {
                            format!(
                                "{} #{} {} stock {}",
                                row.side.as_str(),
                                row.row_index,
                                row.ratio.normalized,
                                row.stock
                            )
                        })
                        .collect();

                    let analysis = (|| -> Result<Vec<String>, String> {
                        let capture =
                            capture_from_book(&book, &context, chrono::Utc::now(), sequence)
                                .map_err(|error| format!("mapping: {error:?}"))?;
                        store
                            .persist_capture(&capture)
                            .map_err(|error| format!("persist: {error}"))?;
                        let observations = store
                            .load_observations(&context_key)
                            .map_err(|error| format!("load: {error}"))?;
                        let need =
                            domain_asset_id(&need_id).map_err(|error| format!("{error:?}"))?;
                        let have =
                            domain_asset_id(&have_id).map_err(|error| format!("{error:?}"))?;
                        ptt_runtime::analysis::pair_analysis_lines(
                            &observations,
                            &context_key,
                            &need,
                            &have,
                        )
                        .map_err(|error| format!("analysis: {error}"))
                    })()
                    .unwrap_or_else(|error| vec![format!("pipeline error: {error}")]);

                    let _ = sender.send(UiEvent::Accepted {
                        header,
                        rows,
                        analysis,
                    });
                }
                SessionEvent::FrameSkipped { reason } => {
                    let _ = sender.send(UiEvent::Skipped(skip_label(&format!("{reason:?}"))));
                }
                SessionEvent::ConfirmationMismatch => {
                    let _ = sender.send(UiEvent::Skipped("double-read mismatch".to_owned()));
                }
                SessionEvent::Duplicate => {}
                SessionEvent::CaptureError(error) => {
                    let _ = sender.send(UiEvent::Skipped(format!("capture: {error}")));
                }
            },
        );
        let _ = sender.send(UiEvent::Stopped);
    }

    /// Compact one-word-ish label from a debug-formatted skip reason.
    fn skip_label(debug_text: &str) -> String {
        debug_text
            .split(['{', '('])
            .next()
            .unwrap_or(debug_text)
            .trim()
            .to_owned()
    }
}
