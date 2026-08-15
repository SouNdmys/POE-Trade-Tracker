//! Watch-loop backend: one worker thread running the full pipeline, a
//! bounded event channel toward the UI. Start/stop is cancellation-based —
//! native OCR calls are never force-killed, the loop simply stops pacing.

#[cfg(windows)]
pub use windows_backend::{
    Backend, RegionSlot, ShellMsg, UiEvent, spawn_calibration, spawn_hotkey_thread,
};

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

    /// Which of the three calibrated regions a wizard run targets.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RegionSlot {
        Need,
        Have,
        Tables,
    }

    impl RegionSlot {
        pub fn override_name(self) -> &'static str {
            match self {
                RegionSlot::Need => "NEED",
                RegionSlot::Have => "HAVE",
                RegionSlot::Tables => "TABLES",
            }
        }

        pub fn label(self) -> &'static str {
            match self {
                RegionSlot::Need => "Need name",
                RegionSlot::Have => "Have name",
                RegionSlot::Tables => "Order tables",
            }
        }
    }

    /// Shell-level messages from the hotkey thread and calibration runs.
    #[derive(Debug)]
    pub enum ShellMsg {
        HotkeyToggle,
        Calibrated {
            slot: RegionSlot,
            x: i32,
            y: i32,
            width: u32,
            height: u32,
        },
        CalibrationCancelled(RegionSlot),
        CalibrationFailed(RegionSlot, String),
    }

    /// Registers the global watch-toggle hotkey on a dedicated thread with
    /// its own message loop (RegisterHotKey requires one). Returns false when
    /// registration failed (another app owns the combination).
    pub fn spawn_hotkey_thread(sender: std::sync::mpsc::Sender<ShellMsg>, binding: String) -> bool {
        let (ready_tx, ready_rx) = channel();
        std::thread::Builder::new()
            .name("ptt-hotkeys".to_owned())
            .spawn(move || {
                use ptt_platform_win::{
                    HotKeyAction, HotKeyConfig, HotKeyManager, StartMonitoringHotKey,
                };
                use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

                let config = HotKeyConfig {
                    start: StartMonitoringHotKey::parse_or_default(Some(&binding)),
                };
                let mut manager = match HotKeyManager::register_for_current_thread(config) {
                    Ok(manager) => manager,
                    Err(error) => {
                        eprintln!("global hotkey registration failed: {error}");
                        let _ = ready_tx.send(false);
                        return;
                    }
                };
                // Only the toggle is wanted in P3.
                manager.unregister(HotKeyAction::SelectRegion);
                manager.unregister(HotKeyAction::StopOrAcknowledge);
                let _ = ready_tx.send(true);
                let mut message = MSG::default();
                // SAFETY: standard thread message loop; the manager outlives it.
                while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                    if message.message == WM_HOTKEY
                        && manager
                            .action_for_message(message.message, message.wParam.0)
                            .is_some()
                        && sender.send(ShellMsg::HotkeyToggle).is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn hotkey thread");
        ready_rx.recv().unwrap_or(false)
    }

    /// Runs the fullscreen region-selection overlay on its own thread and
    /// reports the outcome back to the shell.
    pub fn spawn_calibration(sender: std::sync::mpsc::Sender<ShellMsg>, slot: RegionSlot) {
        std::thread::Builder::new()
            .name("ptt-calibrate".to_owned())
            .spawn(move || {
                use ptt_platform_win::{RegionSelectionOverlay, SelectionOverlayConfig};
                let message = match RegionSelectionOverlay::select(SelectionOverlayConfig::default())
                {
                    Ok(Some(rect)) => match (u32::try_from(rect.width), u32::try_from(rect.height))
                    {
                        (Ok(width), Ok(height)) if width > 0 && height > 0 => {
                            ShellMsg::Calibrated {
                                slot,
                                x: rect.x,
                                y: rect.y,
                                width,
                                height,
                            }
                        }
                        _ => ShellMsg::CalibrationFailed(slot, "degenerate region".to_owned()),
                    },
                    Ok(None) => ShellMsg::CalibrationCancelled(slot),
                    Err(error) => ShellMsg::CalibrationFailed(slot, error.to_string()),
                };
                let _ = sender.send(message);
            })
            .expect("spawn calibration thread");
    }

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

    /// Compact label from a debug-formatted skip reason, keeping the inner
    /// rows-reject variant visible ("Rows(LeadOutOfWindow ...)" -> "LeadOutOfWindow").
    fn skip_label(debug_text: &str) -> String {
        let inner = debug_text.strip_prefix("Rows(").unwrap_or(debug_text);
        inner
            .split(['{', '('])
            .next()
            .unwrap_or(inner)
            .trim()
            .to_owned()
    }
}
