//! Watch-loop backend: one worker thread running the full pipeline, a
//! bounded event channel toward the UI. Start/stop is cancellation-based —
//! native OCR calls are never force-killed, the loop simply stops pacing.

#[cfg(windows)]
pub use windows_backend::{
    Backend, HotkeyRegistration, RegionSlot, ShellMsg, UiEvent, spawn_calibration,
    spawn_hotkey_thread,
};

#[cfg(windows)]
mod windows_backend {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use ptt_runtime::pipeline::{LivePipeline, PipelineEvent};

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

        pub fn label(self, text: &'static crate::i18n::Text) -> &'static str {
            match self {
                RegionSlot::Need => text.slot_need,
                RegionSlot::Have => text.slot_have,
                RegionSlot::Tables => text.slot_tables,
            }
        }
    }

    /// Shell-level messages from the hotkey thread and calibration runs.
    #[derive(Debug)]
    pub enum ShellMsg {
        HotkeyToggle,
        HotkeyHud,
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

    /// Which global shortcuts came up.
    #[derive(Clone, Copy, Debug)]
    pub struct HotkeyRegistration {
        pub watch: bool,
        pub hud: bool,
    }

    /// Registers the global shortcuts on a dedicated thread with its own
    /// message loop (RegisterHotKey requires one).
    ///
    /// The two are registered independently: an unrelated app owning one
    /// combination must not cost the user the other.
    pub fn spawn_hotkey_thread(
        sender: std::sync::mpsc::Sender<ShellMsg>,
        binding: String,
        hud_binding: String,
    ) -> HotkeyRegistration {
        let (ready_tx, ready_rx) = channel();
        std::thread::Builder::new()
            .name("ptt-hotkeys".to_owned())
            .spawn(move || {
                use ptt_platform_win::{
                    HotKeyAction, HotKeyConfig, HotKeyManager, HudToggleHotKey,
                    StartMonitoringHotKey,
                };
                use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

                use ptt_platform_win::HotKeyTarget;

                let config = HotKeyConfig {
                    start: StartMonitoringHotKey::parse_or_default(Some(&binding)),
                    hud: HudToggleHotKey::parse_or_default(Some(&hud_binding)),
                };
                // Register the two we use, one at a time: the all-or-nothing
                // helper would let an unrelated app owning any of the legacy
                // combinations veto everything.
                let mut manager = HotKeyManager::unregistered(HotKeyTarget::CurrentThread, config);
                let watch = manager.register(HotKeyAction::StartMonitoring);
                if let Err(error) = &watch {
                    eprintln!("watch hotkey registration failed: {error}");
                }
                let hud = manager.register(HotKeyAction::ToggleHud);
                if let Err(error) = &hud {
                    eprintln!("HUD hotkey registration failed: {error}");
                }
                let registration = HotkeyRegistration {
                    watch: watch.is_ok(),
                    hud: hud.is_ok(),
                };
                let _ = ready_tx.send(registration);
                if !registration.watch && !registration.hud {
                    return;
                }
                let mut message = MSG::default();
                // SAFETY: standard thread message loop; the manager outlives it.
                while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                    if message.message != WM_HOTKEY {
                        continue;
                    }
                    let outgoing =
                        match manager.action_for_message(message.message, message.wParam.0) {
                            Some(HotKeyAction::StartMonitoring) => ShellMsg::HotkeyToggle,
                            Some(HotKeyAction::ToggleHud) => ShellMsg::HotkeyHud,
                            _ => continue,
                        };
                    if sender.send(outgoing).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn hotkey thread");
        ready_rx.recv().unwrap_or(HotkeyRegistration {
            watch: false,
            hud: false,
        })
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
            /// The pair as the panel showed it, so pages can report on it
            /// without re-parsing the header line.
            need_asset_id: String,
            have_asset_id: String,
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

    /// Sends a Fault if the worker unwinds without a normal exit, so a panic
    /// can never leave the UI stuck on WATCHING with a silently dead channel.
    struct FaultOnDrop {
        sender: Sender<UiEvent>,
        armed: bool,
    }

    impl Drop for FaultOnDrop {
        fn drop(&mut self) {
            if self.armed {
                let _ = self.sender.send(UiEvent::Fault(
                    "watch worker terminated unexpectedly".into(),
                ));
            }
        }
    }

    fn run_watch(cancel: &AtomicBool, sender: &Sender<UiEvent>) {
        let mut sentinel = FaultOnDrop {
            sender: sender.clone(),
            armed: true,
        };
        let mut pipeline = match LivePipeline::open("live-league", None) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                let _ = sender.send(UiEvent::Fault(error.to_string()));
                return;
            }
        };

        pipeline.run(
            // Effectively unbounded; stopping is cancellation-driven.
            Duration::from_secs(60 * 60 * 24),
            cancel,
            |event| match event {
                PipelineEvent::Accepted(book) => {
                    let header = format!(
                        "#{} [{:.0}ms] {} -> {} ({} rows)",
                        book.sequence,
                        book.elapsed.as_secs_f64() * 1e3,
                        book.need_asset_id,
                        book.have_asset_id,
                        book.rows.len(),
                    );
                    let book = *book;
                    let _ = sender.send(UiEvent::Accepted {
                        header,
                        need_asset_id: book.need_asset_id,
                        have_asset_id: book.have_asset_id,
                        rows: book.rows,
                        analysis: book.analysis,
                    });
                }
                PipelineEvent::Skipped(reason) => {
                    // Duplicates are the steady state while a panel sits open
                    // and are not worth a histogram row.
                    if reason != "duplicate" {
                        let _ = sender.send(UiEvent::Skipped(reason));
                    }
                }
                PipelineEvent::Fault(message) => {
                    let _ = sender.send(UiEvent::Fault(message));
                }
            },
        );
        sentinel.armed = false;
        let _ = sender.send(UiEvent::Stopped);
    }
}
