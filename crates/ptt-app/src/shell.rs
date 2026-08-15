//! Root view: status strip + monitor content (last book, opportunities,
//! skip histogram). P3 skeleton — layout only, visuals iterate later.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Duration;

use gpui::{Context, FocusHandle, IntoElement, ParentElement, Render, Styled, Window, div, px};

use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, hairline_soft, mono, panel, panel_header, spaced, status_dot,
};

#[cfg(windows)]
use crate::backend::{
    Backend, RegionSlot, ShellMsg, UiEvent, spawn_calibration, spawn_hotkey_thread,
};

const LOG_CAPACITY: usize = 120;

pub struct AppShell {
    pub focus_handle: FocusHandle,
    #[cfg(windows)]
    backend: Option<Backend>,
    #[cfg(windows)]
    settings_store: ptt_settings::SettingsStore,
    #[cfg(windows)]
    settings: ptt_settings::AppSettings,
    #[cfg(windows)]
    shell_rx: std::sync::mpsc::Receiver<ShellMsg>,
    #[cfg(windows)]
    shell_tx: std::sync::mpsc::Sender<ShellMsg>,
    hotkey_ok: bool,
    watching: bool,
    accepted: u64,
    skips: BTreeMap<String, u64>,
    last_header: Option<String>,
    last_rows: Vec<String>,
    last_analysis: Vec<String>,
    log: VecDeque<String>,
    fault: Option<String>,
}

impl AppShell {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                if this
                    .update(cx, |this: &mut AppShell, cx| this.tick(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        #[cfg(windows)]
        let (settings_store, settings, shell_tx, shell_rx, hotkey_ok) = {
            let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let store =
                ptt_settings::SettingsStore::release_default_from(std::path::Path::new(&local));
            let loaded = store.load();
            let settings = loaded.settings;
            // Re-apply persisted calibration to the recognition route.
            if let Some(profile) = settings.profile(settings.active_profile) {
                for (name, region) in [
                    ("NEED", profile.need_name_region),
                    ("HAVE", profile.have_name_region),
                    ("TABLES", profile.tables_region),
                ] {
                    if let Some(region) = region
                        && !ptt_recognition::profiles::poe2_zhtw::set_region_override(
                            name,
                            (region.x, region.y, region.width, region.height),
                        )
                    {
                        eprintln!("ignoring invalid persisted region for {name}");
                    }
                }
            }
            // Normalize the stored binding through the supported set and
            // write the resolved value back, so the UI never advertises a
            // combination that is not actually registered (legacy files hold
            // "Ctrl+Alt+F11", which is outside the supported options).
            let mut settings = settings;
            let resolved = ptt_platform_win::StartMonitoringHotKey::parse_or_default(Some(
                &settings.hotkeys.toggle_watch,
            ))
            .setting_value()
            .to_owned();
            if settings.hotkeys.toggle_watch != resolved {
                settings.hotkeys.toggle_watch = resolved;
                let _ = store.save(&settings);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let hotkey_ok = spawn_hotkey_thread(tx.clone(), settings.hotkeys.toggle_watch.clone());
            (store, settings, tx, rx, hotkey_ok)
        };
        #[cfg(not(windows))]
        let hotkey_ok = false;

        Self {
            focus_handle: cx.focus_handle(),
            #[cfg(windows)]
            backend: None,
            #[cfg(windows)]
            settings_store,
            #[cfg(windows)]
            settings,
            #[cfg(windows)]
            shell_rx,
            #[cfg(windows)]
            shell_tx,
            hotkey_ok,
            watching: false,
            accepted: 0,
            skips: BTreeMap::new(),
            last_header: None,
            last_rows: Vec::new(),
            last_analysis: Vec::new(),
            log: VecDeque::new(),
            fault: None,
        }
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            let mut dirty = false;
            let messages: Vec<ShellMsg> =
                std::iter::from_fn(|| self.shell_rx.try_recv().ok()).collect();
            for message in messages {
                dirty = true;
                match message {
                    ShellMsg::HotkeyToggle => self.toggle_watch(cx),
                    ShellMsg::Calibrated {
                        slot,
                        x,
                        y,
                        width,
                        height,
                    } => self.apply_calibration(slot, x, y, width, height),
                    ShellMsg::CalibrationCancelled(slot) => {
                        self.push_log(format!("calibration cancelled: {}", slot.label()));
                    }
                    ShellMsg::CalibrationFailed(slot, error) => {
                        self.push_log(format!("calibration failed: {} — {error}", slot.label()));
                    }
                }
            }
            if dirty {
                cx.notify();
            }
            let events: Vec<UiEvent> = self
                .backend
                .as_ref()
                .map(|backend| backend.drain_events())
                .unwrap_or_default();
            if events.is_empty() {
                return;
            }
            for event in events {
                match event {
                    UiEvent::Accepted {
                        header,
                        rows,
                        analysis,
                    } => {
                        self.accepted += 1;
                        self.push_log(header.clone());
                        self.last_header = Some(header);
                        self.last_rows = rows;
                        self.last_analysis = analysis;
                    }
                    UiEvent::Skipped(reason) => {
                        *self.skips.entry(reason).or_default() += 1;
                    }
                    UiEvent::Fault(message) => {
                        self.fault = Some(message);
                        self.watching = false;
                    }
                    UiEvent::Stopped => {
                        self.watching = false;
                    }
                }
            }
            cx.notify();
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
        }
    }

    #[cfg(windows)]
    fn apply_calibration(&mut self, slot: RegionSlot, x: i32, y: i32, width: u32, height: u32) {
        let region = ptt_settings::Region {
            x,
            y,
            width,
            height,
        };
        let profile = self.settings.active_profile;
        let entry = self.settings.profile_mut(profile);
        match slot {
            RegionSlot::Need => entry.need_name_region = Some(region),
            RegionSlot::Have => entry.have_name_region = Some(region),
            RegionSlot::Tables => entry.tables_region = Some(region),
        }
        ptt_recognition::profiles::poe2_zhtw::set_region_override(
            slot.override_name(),
            (x, y, width, height),
        );
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.push_log(format!(
                "calibrated {}: {x},{y} {width}x{height}",
                slot.label()
            )),
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
        // A running session captured its regions at start; restart it so the
        // new geometry takes effect immediately.
        if self.watching {
            if let Some(mut backend) = self.backend.take() {
                backend.stop();
            }
            self.backend = Some(Backend::start());
        }
    }

    #[cfg(windows)]
    fn start_calibration(&mut self, slot: RegionSlot) {
        self.push_log(format!(
            "drag the {} region on screen (Esc cancels)",
            slot.label()
        ));
        spawn_calibration(self.shell_tx.clone(), slot);
    }

    fn toggle_watch(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            if self.watching {
                if let Some(mut backend) = self.backend.take() {
                    backend.stop();
                }
                self.watching = false;
            } else {
                self.fault = None;
                self.backend = Some(Backend::start());
                self.watching = true;
            }
            cx.notify();
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
        }
    }
}

impl AppShell {
    #[cfg(windows)]
    fn region_text(region: Option<ptt_settings::Region>) -> String {
        match region {
            Some(region) => format!(
                "{},{}  {}x{}",
                region.x, region.y, region.width, region.height
            ),
            None => "preset (2560x1440)".to_owned(),
        }
    }

    #[cfg(windows)]
    fn settings_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let profile = self.settings.active_profile;
        let entry = self.settings.profile(profile).cloned().unwrap_or_default();
        let rows: [(RegionSlot, &'static str, Option<ptt_settings::Region>); 3] = [
            (RegionSlot::Need, "cal-need", entry.need_name_region),
            (RegionSlot::Have, "cal-have", entry.have_name_region),
            (RegionSlot::Tables, "cal-tables", entry.tables_region),
        ];
        let hotkey_line = if self.hotkey_ok {
            format!(
                "hotkey {}  toggles watch",
                self.settings.hotkeys.toggle_watch
            )
        } else {
            format!(
                "hotkey {} unavailable (in use elsewhere)",
                self.settings.hotkeys.toggle_watch
            )
        };
        panel().child(panel_header("SETTINGS")).child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows.into_iter().map(|(slot, id, region)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(90.0))
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_META))
                                .child(slot.label()),
                        )
                        .child(
                            mono(Self::region_text(region))
                                .text_size(fs(FS_12))
                                .flex_grow(),
                        )
                        .child(button(id, LedgerButton::Quiet, "Calibrate", cx).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.start_calibration(slot);
                                cx.notify();
                            }),
                        ))
                }))
                .child(
                    mono(hotkey_line)
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                ),
        )
    }

    #[cfg(not(windows))]
    fn settings_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        panel().child(panel_header("SETTINGS"))
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot_kind, state_label) = if self.fault.is_some() {
            (StatusKind::Error, "FAULT")
        } else if self.watching {
            (StatusKind::Monitoring, "WATCHING")
        } else {
            (StatusKind::Idle, "IDLE")
        };
        let skip_total: u64 = self.skips.values().sum();
        let button_label = if self.watching { "Stop" } else { "Start watch" };
        let button_kind = if self.watching {
            LedgerButton::Secondary
        } else {
            LedgerButton::Primary
        };

        let mut skip_lines: Vec<String> = self
            .skips
            .iter()
            .map(|(reason, count)| format!("{count:>5}  {reason}"))
            .collect();
        skip_lines.truncate(10);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(CANVAS))
            .text_color(c(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .child(
                // Status strip.
                div()
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE_STRONG))
                    .child(status_dot(dot_kind))
                    .child(
                        div()
                            .text_size(fs(FS_12_5))
                            .child(spaced("POE TRADE TRACKER")),
                    )
                    .child(
                        mono(format!(
                            "{state_label}   accepted {}   skips {}",
                            self.accepted, skip_total
                        ))
                        .text_color(c(TEXT_META)),
                    )
                    .child(div().flex_grow())
                    .child(
                        button("watch-toggle", button_kind, button_label, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_watch(cx))),
                    ),
            )
            .child(
                // Body: three panels.
                div()
                    .flex_grow()
                    .flex()
                    .gap_3()
                    .p_3()
                    .child(
                        panel()
                            .flex_grow()
                            .overflow_hidden()
                            .child(panel_header("LAST BOOK"))
                            .child(
                                div().p_3().flex().flex_col().gap_1().children(
                                    std::iter::once(
                                        self.last_header
                                            .clone()
                                            .unwrap_or_else(|| "waiting for a book…".to_owned()),
                                    )
                                    .chain(self.last_rows.iter().cloned())
                                    .map(|line| mono(line).text_size(fs(FS_12))),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                panel()
                                    .overflow_hidden()
                                    .child(panel_header("OPPORTUNITIES"))
                                    .child(div().p_3().flex().flex_col().gap_1().children(
                                        if self.last_analysis.is_empty() {
                                            vec![mono("—").text_size(fs(FS_12))]
                                        } else {
                                            self.last_analysis
                                                .iter()
                                                .map(|line| mono(line.clone()).text_size(fs(FS_12)))
                                                .collect()
                                        },
                                    )),
                            )
                            .child(panel().child(panel_header("SKIPS")).child(
                                div().p_3().flex().flex_col().gap_1().children(
                                    if skip_lines.is_empty() {
                                        vec![mono("—").text_size(fs(FS_12))]
                                    } else {
                                        skip_lines
                                            .into_iter()
                                            .map(|line| {
                                                mono(line)
                                                    .text_size(fs(FS_12))
                                                    .text_color(c(TEXT_META))
                                            })
                                            .collect()
                                    },
                                ),
                            ))
                            .child(self.settings_panel(cx)),
                    ),
            )
            .child(
                // Footer: fault or recent log line.
                div()
                    .h(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_4()
                    .bg(c(RAIL))
                    .border_t_1()
                    .border_color(c(HAIRLINE))
                    .child(match &self.fault {
                        Some(fault) => mono(format!("fault: {fault}"))
                            .text_size(fs(FS_11_5))
                            .text_color(c(DANGER)),
                        None => mono(self.log.back().cloned().unwrap_or_default())
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META)),
                    })
                    .child(hairline_soft()),
            )
    }
}
