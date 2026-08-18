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
    Backend, HotkeyRegistration, RegionSlot, ShellMsg, UiEvent, spawn_calibration,
    spawn_hotkey_thread,
};

const LOG_CAPACITY: usize = 120;

/// Where the overlay card sits, and how big it is.
///
/// Top-left rather than centred: the currency panel occupies the middle of
/// the screen, which is exactly what the card must not cover.
const HUD_ORIGIN: (i32, i32) = (24, 24);
const HUD_SIZE: (i32, i32) = (400, 200);

/// How far back the pages read. Matches the analysis window the watch loop
/// uses, so a page and the live line never describe different books.
const REPORT_WINDOW_HOURS: i64 = 2;

/// The pages of the app. Monitor answers "is the watcher healthy", the rest
/// answer questions about what it has collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Monitor,
    Convert,
    Watchlist,
    History,
}

impl Page {
    const ALL: [Self; 4] = [Self::Monitor, Self::Convert, Self::Watchlist, Self::History];

    fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Monitor => text.page_monitor,
            Self::Convert => text.page_convert,
            Self::Watchlist => text.page_watchlist,
            Self::History => text.page_history,
        }
    }

    /// A stable, language-independent element id.
    ///
    /// Ids must not move with the interface language, or a language switch
    /// reads to the framework as a different set of controls.
    const fn element_id(self) -> &'static str {
        match self {
            Self::Monitor => "page-monitor",
            Self::Convert => "page-convert",
            Self::Watchlist => "page-watchlist",
            Self::History => "page-history",
        }
    }
}

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
    hotkey_ok: HotkeyRegistration,
    /// The overlay card, created lazily the first time it is asked for.
    #[cfg(windows)]
    hud: Option<ptt_platform_win::HudWindow>,
    hud_visible: bool,
    watching: bool,
    accepted: u64,
    skips: BTreeMap<String, u64>,
    last_header: Option<String>,
    last_rows: Vec<String>,
    last_analysis: Vec<String>,
    log: VecDeque<String>,
    fault: Option<String>,
    page: Page,
    /// The pair the report pages describe: the last book that was accepted.
    report_pair: Option<(String, String)>,
    report_lines: Vec<String>,
    /// True when a new book landed since the visible page was last built.
    report_stale: bool,
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
                        && !ptt_recognition::route::set_region_override(
                            ptt_runtime::pipeline::route_for(settings.active_profile)
                                .0
                                .key_prefix,
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
            let resolved_watch = ptt_platform_win::StartMonitoringHotKey::parse_or_default(Some(
                &settings.hotkeys.toggle_watch,
            ))
            .setting_value()
            .to_owned();
            let resolved_hud = ptt_platform_win::HudToggleHotKey::parse_or_default(Some(
                &settings.hotkeys.toggle_hud,
            ))
            .setting_value()
            .to_owned();
            if settings.hotkeys.toggle_watch != resolved_watch
                || settings.hotkeys.toggle_hud != resolved_hud
            {
                settings.hotkeys.toggle_watch = resolved_watch;
                settings.hotkeys.toggle_hud = resolved_hud;
                let _ = store.save(&settings);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let hotkey_ok = spawn_hotkey_thread(
                tx.clone(),
                settings.hotkeys.toggle_watch.clone(),
                settings.hotkeys.toggle_hud.clone(),
            );
            (store, settings, tx, rx, hotkey_ok)
        };
        #[cfg(not(windows))]
        let hotkey_ok = HotkeyRegistration {
            watch: false,
            hud: false,
        };

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
            #[cfg(windows)]
            hud: None,
            hud_visible: false,
            watching: false,
            accepted: 0,
            skips: BTreeMap::new(),
            last_header: None,
            last_rows: Vec::new(),
            last_analysis: Vec::new(),
            log: VecDeque::new(),
            fault: None,
            page: Page::Monitor,
            report_pair: None,
            report_lines: Vec::new(),
            report_stale: true,
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
                    ShellMsg::HotkeyHud => self.toggle_hud(),
                    ShellMsg::Calibrated {
                        slot,
                        x,
                        y,
                        width,
                        height,
                    } => self.apply_calibration(slot, x, y, width, height),
                    ShellMsg::CalibrationCancelled(slot) => {
                        self.push_log(format!(
                            "calibration cancelled: {}",
                            slot.label(self.text())
                        ));
                    }
                    ShellMsg::CalibrationFailed(slot, error) => {
                        self.push_log(format!(
                            "calibration failed: {} — {error}",
                            slot.label(self.text())
                        ));
                    }
                }
            }
            if self.report_stale {
                self.refresh_report();
                dirty = true;
            }
            if dirty {
                self.refresh_hud();
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
                        need_asset_id,
                        have_asset_id,
                        rows,
                        analysis,
                    } => {
                        self.accepted += 1;
                        self.push_log(header.clone());
                        self.last_header = Some(header);
                        self.last_rows = rows;
                        self.last_analysis = analysis;
                        self.report_pair = Some((have_asset_id, need_asset_id));
                        self.report_stale = true;
                    }
                    UiEvent::Skipped(reason) => {
                        *self.skips.entry(reason).or_default() += 1;
                    }
                    UiEvent::Fault(message) => {
                        self.fault = Some(message);
                        self.watching = false;
                        self.backend = None;
                    }
                    UiEvent::Stopped => {
                        self.watching = false;
                        self.backend = None;
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
        // Keyed by the profile's own panel. Storing a POE1 rectangle under
        // POE2's prefix leaves the route reading its factory preset while the
        // interface shows the region as calibrated.
        ptt_recognition::route::set_region_override(
            ptt_runtime::pipeline::route_for(profile).0.key_prefix,
            slot.override_name(),
            (x, y, width, height),
        );
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.push_log(format!(
                "calibrated {}: {x},{y} {width}x{height}",
                slot.label(self.text())
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
            slot.label(self.text())
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
    /// The catalogue for the stored interface language.
    fn text(&self) -> &'static crate::i18n::Text {
        #[cfg(windows)]
        {
            crate::i18n::text(self.settings.ui_language)
        }
        #[cfg(not(windows))]
        {
            crate::i18n::text(ptt_settings::UiLanguage::English)
        }
    }

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
        let text = self.text();
        let hotkey_line = if self.hotkey_ok.watch {
            format!(
                "{} — {}",
                self.settings.hotkeys.toggle_watch, text.hotkey_ready
            )
        } else {
            format!(
                "{} — {}",
                self.settings.hotkeys.toggle_watch, text.hotkey_unavailable
            )
        };
        panel().child(panel_header(text.panel_settings)).child(
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
                                .child(slot.label(text)),
                        )
                        .child(
                            mono(Self::region_text(region))
                                .text_size(fs(FS_12))
                                .flex_grow(),
                        )
                        .child(
                            button(id, LedgerButton::Quiet, text.calibrate, cx).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.start_calibration(slot);
                                    cx.notify();
                                }),
                            ),
                        )
                }))
                .child(
                    self.profile_row(
                        text.game_label,
                        [
                            ("profile-poe1", ptt_core::Game::Poe1, "PoE 1"),
                            ("profile-poe2", ptt_core::Game::Poe2, "PoE 2"),
                        ]
                        .map(|(id, game, label)| {
                            (
                                id,
                                game == profile.game,
                                label,
                                ptt_core::ProfileId::new(game, profile.language),
                            )
                        })
                        .to_vec(),
                        cx,
                    ),
                )
                .child(
                    self.profile_row(
                        text.client_language_label,
                        [
                            ("client-en", ptt_core::ContentLanguage::English, "EN"),
                            (
                                "client-zh",
                                ptt_core::ContentLanguage::TraditionalChinese,
                                "繁中",
                            ),
                        ]
                        .map(|(id, language, label)| {
                            (
                                id,
                                language == profile.language,
                                label,
                                ptt_core::ProfileId::new(profile.game, language),
                            )
                        })
                        .to_vec(),
                        cx,
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(90.0))
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_META))
                                .child(text.language_label),
                        )
                        .children(crate::i18n::LANGUAGES.into_iter().map(|language| {
                            let active = language == self.settings.ui_language;
                            button(
                                match language {
                                    ptt_settings::UiLanguage::English => "lang-en",
                                    ptt_settings::UiLanguage::Chinese => "lang-zh",
                                },
                                if active {
                                    LedgerButton::Primary
                                } else {
                                    LedgerButton::Quiet
                                },
                                // Always in its own language: someone who
                                // cannot read the current one still finds
                                // theirs.
                                crate::i18n::native_label(language),
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.set_language(language);
                                    cx.notify();
                                },
                            ))
                        })),
                )
                .child(
                    mono(hotkey_line)
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                ),
        )
    }

    /// One row of mutually exclusive profile buttons.
    ///
    /// The profile decides which panel geometry the watcher reads and which
    /// catalog language it matches names against, so it is a setting the user
    /// has to be able to reach — POE1 recognition was otherwise only usable
    /// from the probes.
    #[cfg(windows)]
    fn profile_row(
        &self,
        label: &'static str,
        options: Vec<(&'static str, bool, &'static str, ptt_core::ProfileId)>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(90.0))
                    .text_size(fs(FS_12))
                    .text_color(c(TEXT_META))
                    .child(label),
            )
            .children(
                options
                    .into_iter()
                    .map(|(option_id, active, option_label, profile)| {
                        button(
                            option_id,
                            if active {
                                LedgerButton::Primary
                            } else {
                                LedgerButton::Quiet
                            },
                            option_label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_profile(profile);
                            cx.notify();
                        }))
                    }),
            )
    }

    /// Switches the watched profile and persists it.
    ///
    /// Takes effect on the next watch start rather than mid-session: the route
    /// holds its layout and its OCR language from construction, and swapping
    /// them under a running capture would mix two panels' rows into one book.
    #[cfg(windows)]
    fn set_profile(&mut self, profile: ptt_core::ProfileId) {
        if self.settings.active_profile == profile {
            return;
        }
        self.settings.active_profile = profile;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("could not save profile: {error}"));
            return;
        }
        self.push_log(format!(
            "profile {profile} — {}",
            self.text().restart_watch_to_apply
        ));
    }

    /// Switches the interface language and persists it.
    #[cfg(windows)]
    fn set_language(&mut self, language: ptt_settings::UiLanguage) {
        if self.settings.ui_language == language {
            return;
        }
        self.settings.ui_language = language;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("could not save language: {error}"));
        }
    }

    #[cfg(not(windows))]
    fn settings_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        panel().child(panel_header(self.text().panel_settings))
    }
}

impl AppShell {
    /// Shows or hides the overlay card, creating it on first use.
    ///
    /// The card is created excluded from capture and click-through from the
    /// moment it exists: a HUD that appears in a screenshot would be read
    /// back as part of the panel it is describing, and one that takes clicks
    /// would steal them from the game.
    #[cfg(windows)]
    fn toggle_hud(&mut self) {
        use ptt_platform_win::{
            CaptureAffinity, HudInteractionMode, HudWindow, HudWindowConfig, HudWindowPolicy, RectI,
        };

        if self.hud.is_none() {
            let Some(bounds) = RectI::new(HUD_ORIGIN.0, HUD_ORIGIN.1, HUD_SIZE.0, HUD_SIZE.1)
            else {
                self.push_log("HUD bounds are invalid".to_owned());
                return;
            };
            let config = HudWindowConfig {
                bounds,
                policy: HudWindowPolicy {
                    interaction: HudInteractionMode::Passive,
                    capture_affinity: CaptureAffinity::Exclude,
                },
                visible: false,
            };
            match HudWindow::create(config) {
                Ok(hud) => self.hud = Some(hud),
                Err(error) => {
                    self.push_log(format!("HUD unavailable: {error}"));
                    return;
                }
            }
        }
        let Some(hud) = self.hud.as_mut() else {
            return;
        };
        let outcome = if self.hud_visible {
            hud.hide()
        } else {
            hud.show()
        };
        match outcome {
            Ok(()) => {
                self.hud_visible = !self.hud_visible;
                self.refresh_hud();
            }
            Err(error) => self.push_log(format!("HUD toggle failed: {error}")),
        }
    }

    /// Pushes the current state onto the card. Cheap and idempotent, so it
    /// can run on the tick without a dirty flag.
    #[cfg(windows)]
    fn refresh_hud(&mut self) {
        use ptt_platform_win::HudContent;

        if !self.hud_visible {
            return;
        }
        let status = if self.fault.is_some() {
            "FAULT"
        } else if self.watching {
            "WATCHING"
        } else {
            "IDLE"
        };
        let pair = self.report_pair.as_ref().map_or_else(
            || "no pair yet".to_owned(),
            |(have, need)| format!("{have} -> {need}"),
        );
        // The card answers two questions and no others: what is this pair
        // worth, and where should I go next.
        let mut lines = vec![pair];
        lines.extend(self.last_analysis.iter().take(3).cloned());
        if self.page == Page::Monitor {
            lines.extend(self.report_lines.iter().take(4).cloned());
        }
        let content = HudContent {
            monitoring: self.watching,
            status_text: status.to_owned(),
            elapsed: format!("{} ok", self.accepted),
            lines,
        };
        if let Some(hud) = self.hud.as_mut()
            && let Err(error) = hud.set_content(content)
        {
            self.push_log(format!("HUD update failed: {error}"));
        }
    }

    #[cfg(not(windows))]
    fn toggle_hud(&mut self) {
        self.hud_visible = !self.hud_visible;
    }

    #[cfg(not(windows))]
    fn refresh_hud(&mut self) {}

    #[cfg(windows)]
    fn show_page(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.report_stale = true;
        }
    }

    /// Rebuilds the visible page's report from the store.
    ///
    /// Reads happen when the page changes, on request, or after a new book —
    /// never on the frame tick, so a growing database cannot turn into a
    /// per-frame query.
    #[cfg(windows)]
    fn refresh_report(&mut self) {
        self.report_stale = false;
        // Page dispatch happens here and nowhere else. It used to happen
        // twice, at two depths, and the two disagreed: this function returned
        // early on Monitor while `build_report` carried a Monitor branch that
        // could therefore never run, leaving the probe queue permanently
        // blank.
        //
        // Monitor is the one page that needs no pair — the probe queue is
        // about what has *not* been captured — so it is answered before the
        // pair guard.
        if self.page == Page::Monitor {
            self.report_lines = match self.probe_queue_report() {
                Ok(lines) => lines,
                Err(reason) => vec![format!("probe queue unavailable: {reason}")],
            };
            return;
        }
        let Some((have, need)) = self.report_pair.clone() else {
            self.report_lines = vec![self.text().waiting_for_book.to_owned()];
            return;
        };
        self.report_lines = match self.build_report(&have, &need) {
            Ok(lines) => lines,
            Err(reason) => vec![format!("report unavailable: {reason}")],
        };
    }

    /// Loads the window the report pages read.
    ///
    /// The league is the pipeline's, not a second literal: the writer and the
    /// reader agree on the context key or every page silently reads an empty
    /// book.
    #[cfg(windows)]
    fn load_window(
        &self,
    ) -> Result<(String, Vec<ptt_trade_domain::MarketEdgeObservation>), String> {
        use ptt_runtime::live::live_context;
        use ptt_runtime::pipeline::{LIVE_LEAGUE, default_database_path};

        let store = ptt_storage::MarketStore::open(default_database_path())
            .map_err(|error| format!("storage: {error}"))?;
        // The profile is the pipeline's too, for the same reason the league
        // is: the context key mixes in the game, so a reader on POE2 and a
        // writer on POE1 agree on the league and still share nothing.
        let context = live_context(self.settings.active_profile, LIVE_LEAGUE)
            .map_err(|error| format!("{error:?}"))?;
        let context_key = context.stable_key();
        let observations = store
            .load_observations(
                &context_key,
                Some(chrono::Utc::now() - chrono::Duration::hours(REPORT_WINDOW_HOURS)),
            )
            .map_err(|error| format!("load: {error}"))?;
        Ok((context_key, observations))
    }

    #[cfg(windows)]
    fn probe_queue_report(&self) -> Result<Vec<String>, String> {
        use ptt_runtime::pipeline::LIVE_LEAGUE;

        let (context_key, observations) = self.load_window()?;
        ptt_runtime::reports::probe_queue(&observations, &context_key, LIVE_LEAGUE)
    }

    #[cfg(windows)]
    fn build_report(&self, have: &str, need: &str) -> Result<Vec<String>, String> {
        use ptt_runtime::live::domain_asset_id;
        use ptt_runtime::pipeline::LIVE_LEAGUE;

        let (context_key, observations) = self.load_window()?;
        let have = domain_asset_id(have).map_err(|error| format!("{error:?}"))?;
        let need = domain_asset_id(need).map_err(|error| format!("{error:?}"))?;

        match self.page {
            // Answered before this function is reached; see refresh_report.
            Page::Monitor => Ok(Vec::new()),
            Page::Convert => {
                ptt_runtime::reports::convert_report(&observations, &context_key, &have, &need)
            }
            Page::Watchlist => {
                ptt_runtime::reports::watchlist_report(&observations, &context_key, LIVE_LEAGUE)
            }
            Page::History => {
                ptt_runtime::reports::history_report(&observations, &context_key, &have, &need)
            }
        }
    }

    #[cfg(not(windows))]
    fn show_page(&mut self, page: Page) {
        self.page = page;
    }

    #[cfg(not(windows))]
    fn refresh_report(&mut self) {
        self.report_stale = false;
    }

    fn nav_rail(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(px(132.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(c(RAIL))
            .border_r_1()
            .border_color(c(HAIRLINE))
            .children(Page::ALL.into_iter().map(|page| {
                let active = page == self.page;
                button(
                    page.element_id(),
                    if active {
                        LedgerButton::Primary
                    } else {
                        LedgerButton::Secondary
                    },
                    page.label(self.text()),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.show_page(page);
                    cx.notify();
                }))
            }))
    }

    fn report_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let title = self.page.label(text);
        let lines = if self.report_lines.is_empty() {
            vec![text.nothing_yet.to_owned()]
        } else {
            self.report_lines.clone()
        };
        panel()
            .flex_grow()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(panel_header(title))
                    .child(div().flex_grow())
                    .child(
                        div().pr_3().child(
                            button("report-refresh", LedgerButton::Secondary, text.refresh, cx)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_report();
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(
                div().p_3().flex().flex_col().gap_1().children(
                    lines
                        .into_iter()
                        .map(|line| mono(line).text_size(fs(FS_12))),
                ),
            )
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text();
        let (dot_kind, state_label) = if self.fault.is_some() {
            (StatusKind::Error, text.state_fault)
        } else if self.watching {
            (StatusKind::Monitoring, text.state_watching)
        } else {
            (StatusKind::Idle, text.state_idle)
        };
        let skip_total: u64 = self.skips.values().sum();
        let button_label = if self.watching {
            text.stop_watch
        } else {
            text.start_watch
        };
        let button_kind = if self.watching {
            LedgerButton::Secondary
        } else {
            LedgerButton::Primary
        };

        // Highest counts first: an alphabetical slice would hide the
        // dominant failure mode once reasons exceed the display cap.
        let mut ranked: Vec<(&String, &u64)> = self.skips.iter().collect();
        ranked.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
        let mut skip_lines: Vec<String> = ranked
            .into_iter()
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
                    .child(div().text_size(fs(FS_12_5)).child(spaced(text.app_title)))
                    .child(
                        mono(format!(
                            "{state_label}   {} {}   {} {}",
                            text.accepted_label, self.accepted, text.skips_label, skip_total
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
                // Body: navigation rail plus the active page.
                div().flex_grow().flex().child(self.nav_rail(cx)).child(
                    if self.page == Page::Monitor {
                        div()
                            .flex_grow()
                            .flex()
                            .gap_3()
                            .p_3()
                            .child(
                                panel()
                                    .flex_grow()
                                    .overflow_hidden()
                                    .child(panel_header(text.panel_last_book))
                                    .child(
                                        div().p_3().flex().flex_col().gap_1().children(
                                            std::iter::once(
                                                self.last_header.clone().unwrap_or_else(|| {
                                                    text.waiting_for_book.to_owned()
                                                }),
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
                                            .child(panel_header(text.panel_opportunities))
                                            .child(div().p_3().flex().flex_col().gap_1().children(
                                                if self.last_analysis.is_empty() {
                                                    vec![
                                                        mono(text.nothing_yet).text_size(fs(FS_12)),
                                                    ]
                                                } else {
                                                    self.last_analysis
                                                        .iter()
                                                        .map(|line| {
                                                            mono(line.clone()).text_size(fs(FS_12))
                                                        })
                                                        .collect()
                                                },
                                            )),
                                    )
                                    .child(panel().child(panel_header(text.panel_skips)).child(
                                        div().p_3().flex().flex_col().gap_1().children(
                                            if skip_lines.is_empty() {
                                                vec![mono(text.nothing_yet).text_size(fs(FS_12))]
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
                                    .child(
                                        panel()
                                            .overflow_hidden()
                                            .child(panel_header(text.panel_probe_queue))
                                            .child(div().p_3().flex().flex_col().gap_1().children(
                                                if self.report_lines.is_empty() {
                                                    vec![
                                                        mono(text.nothing_yet).text_size(fs(FS_12)),
                                                    ]
                                                } else {
                                                    self.report_lines
                                                        .iter()
                                                        .map(|line| {
                                                            mono(line.clone()).text_size(fs(FS_12))
                                                        })
                                                        .collect()
                                                },
                                            )),
                                    )
                                    .child(self.settings_panel(cx)),
                            )
                    } else {
                        div()
                            .flex_grow()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .child(
                                mono(match &self.report_pair {
                                    Some((have, need)) => {
                                        format!("{}: {have} -> {need}", text.pair_prefix)
                                    }
                                    None => text.no_pair_yet.to_owned(),
                                })
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_META)),
                            )
                            .child(self.report_panel(cx))
                    },
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
                        Some(fault) => mono(format!("{}: {fault}", text.fault_prefix))
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
