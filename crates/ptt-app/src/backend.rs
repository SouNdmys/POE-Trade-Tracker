//! Watch-loop backend: one worker thread running the full pipeline, a
//! bounded event channel toward the UI. Start/stop is cancellation-based —
//! native OCR calls are never force-killed, the loop simply stops pacing.

#[cfg(windows)]
pub use windows_backend::{
    Backend, HotkeyRegistration, RegionSlot, ShellMsg, UiEvent, spawn_hotkey_thread,
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

    /// Shell-level messages from the hotkey thread and the file picker.
    #[derive(Debug)]
    pub enum ShellMsg {
        HotkeyToggle,
        HotkeyHud,
        /// A screenshot chosen for the calibration screen, or `None` if the
        /// picker was dismissed.
        ScreenshotPicked(Option<std::path::PathBuf>),
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

    #[derive(Debug)]
    pub enum UiEvent {
        Accepted {
            /// Position in this run, and how long the read took.
            ///
            /// Carried as numbers rather than pre-formatted into a sentence:
            /// the sentence has to name currencies, and only the interface
            /// knows which language to name them in. A backend that formats
            /// is a backend that shipped `chaos-orb` to a Chinese window.
            sequence: u64,
            elapsed_ms: u64,
            /// The pair as the panel showed it.
            need_asset_id: String,
            have_asset_id: String,
            /// The same rows with their fields intact, for the monitor.
            order_rows: Vec<ptt_runtime::pipeline::BookRow>,
            /// Typed facts about the pair; the interface renders them in its
            /// own language. Boxed so this variant stays near the others in
            /// size — every skip event would otherwise pay for the analysis.
            analysis: Box<ptt_runtime::analysis::PairAnalysis>,
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
                    let book = *book;
                    let _ = sender.send(UiEvent::Accepted {
                        sequence: book.sequence,
                        elapsed_ms: book.elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
                        need_asset_id: book.need_asset_id,
                        have_asset_id: book.have_asset_id,
                        order_rows: book.order_rows,
                        analysis: Box::new(book.analysis),
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
