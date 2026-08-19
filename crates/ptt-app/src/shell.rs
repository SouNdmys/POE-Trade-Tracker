//! Root view: status strip + monitor content (last book, opportunities,
//! skip histogram). P3 skeleton — layout only, visuals iterate later.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Duration;

use gpui::{
    Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement, Render, Styled,
    Window, div, px,
};

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

/// Sized to the panel it mirrors, not to a round number.
///
/// The exchange shows at most twelve rows — six available, six competing — and
/// the point of the card is to read them without alt-tabbing, so all twelve
/// have to fit or it answers half the question. The painter stacks 17px lines
/// from 30px down with a 4px foot, so sixteen lines need 306: twelve rows, the
/// pair, a blank, and the recognition verdict, with one spare.
const HUD_SIZE: (i32, i32) = (420, 310);
/// Twelve rows plus pair, spacer and verdict.
const HUD_BODY_LINES: usize = 16;

/// How far back the pages read. Matches the analysis window the watch loop
/// uses, so a page and the live line never describe different books.
const REPORT_WINDOW_HOURS: i64 = 2;

/// The pages of the app. Monitor answers "is the watcher healthy", the rest
/// answer questions about what it has collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Monitor,
    Calibrate,
    Opportunities,
    Convert,
    Watchlist,
    History,
}

impl Page {
    const ALL: [Self; 6] = [
        Self::Monitor,
        Self::Calibrate,
        Self::Opportunities,
        Self::Convert,
        Self::Watchlist,
        Self::History,
    ];

    fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Monitor => text.page_monitor,
            Self::Calibrate => text.page_calibrate,
            Self::Opportunities => text.page_opportunities,
            Self::Convert => text.page_convert,
            Self::Watchlist => text.page_watchlist,
            Self::History => text.page_history,
        }
    }

    /// Whether this page is about one currency pair.
    ///
    /// Monitor and the radar are about the whole market, so they must be
    /// answered before the pair guard rather than after it. Getting that
    /// wrong is quiet: the page just says "waiting for a book" forever, even
    /// though it never needed one.
    const fn needs_a_pair(self) -> bool {
        match self {
            Self::Monitor | Self::Opportunities | Self::Calibrate => false,
            Self::Convert | Self::Watchlist | Self::History => true,
        }
    }

    /// A stable, language-independent element id.
    ///
    /// Ids must not move with the interface language, or a language switch
    /// reads to the framework as a different set of controls.
    const fn element_id(self) -> &'static str {
        match self {
            Self::Monitor => "page-monitor",
            Self::Calibrate => "page-calibrate",
            Self::Opportunities => "page-opportunities",
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
    #[cfg(windows)]
    calibration: crate::calibrate::Calibration,
    /// Where the calibration canvas landed, written by its paint pass.
    ///
    /// Mouse events arrive in window coordinates and the rectangles are in the
    /// screenshot's, so the element's own origin is the missing term. GPUI
    /// hands it to a `canvas` callback rather than to the event, and a `Cell`
    /// carries it across because both ends run on the UI thread.
    #[cfg(windows)]
    canvas_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// Holds the decoded screenshot across frames.
    ///
    /// Without it every frame decodes the file again, and a 2560x1440 PNG
    /// takes long enough that a zoom click landed seconds later. The
    /// calibration screen shows one image at a time, so retaining all of them
    /// retains one.
    #[cfg(windows)]
    image_cache: gpui::Entity<gpui::RetainAllImageCache>,
    watching: bool,
    accepted: u64,
    skips: BTreeMap<String, u64>,
    /// The most recent frame that was not used, and why.
    ///
    /// The tally answers "how often"; a HUD in front of a live panel has to
    /// answer "did *that* one land", and a skip with no reason on screen is
    /// indistinguishable from the watcher having stopped.
    last_skip: Option<String>,
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
            #[cfg(windows)]
            calibration: crate::calibrate::Calibration::default(),
            #[cfg(windows)]
            canvas_bounds: std::rc::Rc::new(std::cell::Cell::new(None)),
            #[cfg(windows)]
            image_cache: gpui::RetainAllImageCache::new(cx),
            watching: false,
            accepted: 0,
            skips: BTreeMap::new(),
            last_skip: None,
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
                    ShellMsg::ScreenshotPicked(path) => self.screenshot_picked(path),
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
                        self.last_skip = None;
                    }
                    UiEvent::Skipped(reason) => {
                        *self.skips.entry(reason.clone()).or_default() += 1;
                        self.last_skip = Some(reason);
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
            // The card has to move when the panel does. Books and skips arrive
            // here, not through the message pump above, so refreshing only
            // there left the HUD showing whatever was true when it was opened
            // — which is worse than showing nothing, because a stale card
            // still looks like a live one.
            self.refresh_hud();
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
                .child(div().flex().items_center().gap_2().child(
                    button("use-preset", LedgerButton::Quiet, text.use_preset, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.apply_preset_regions();
                            cx.notify();
                        }),
                    ),
                ))
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

    /// The calibration screen: a screenshot, and three rectangles drawn on it.
    #[cfg(windows)]
    fn render_calibrate(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::calibrate::{MAGNIFIER_ZOOM, Target};

        let text = self.text();
        let view = self.calibration.view();
        let size = self.calibration.image_size;
        let image = self.calibration.image.clone();
        let bounds_slot = self.canvas_bounds.clone();
        let active_target = self.calibration.target();

        let toolbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .child(
                button("cal-load", LedgerButton::Primary, text.load_screenshot, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.load_screenshot();
                        cx.notify();
                    }),
                ),
            )
            .children(Target::ALL.into_iter().map(|target| {
                let active = target == active_target;
                button(
                    target.element_id(),
                    if active {
                        LedgerButton::Primary
                    } else {
                        LedgerButton::Quiet
                    },
                    match target {
                        Target::Need => text.slot_need,
                        Target::Have => text.slot_have,
                        Target::Tables => text.slot_tables,
                    },
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.calibration.target = Some(target);
                    cx.notify();
                }))
            }))
            .child(
                button("cal-zoom-in", LedgerButton::Quiet, text.zoom_in, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.zoom_calibration(1.25);
                        cx.notify();
                    },
                )),
            )
            .child(
                button("cal-zoom-out", LedgerButton::Quiet, text.zoom_out, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.zoom_calibration(0.8);
                        cx.notify();
                    }),
                ),
            )
            .child(
                button("cal-fit", LedgerButton::Quiet, text.fit_window, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.fit_calibration();
                        cx.notify();
                    },
                )),
            )
            .child(
                button("cal-actual", LedgerButton::Quiet, text.actual_size, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.zoom_calibration_to(1.0);
                        cx.notify();
                    }),
                ),
            )
            .child(
                button("cal-apply", LedgerButton::Primary, text.apply_regions, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.apply_drawn_regions();
                        cx.notify();
                    }),
                ),
            );

        let drawn: Vec<(bool, f32, f32, f32, f32)> = Target::ALL
            .into_iter()
            .filter_map(|target| {
                let rect = self.calibration.rect(target)?;
                let (left, top) = view.to_canvas(rect.x as f32, rect.y as f32);
                Some((
                    target == active_target,
                    left,
                    top,
                    rect.width as f32 * view.zoom,
                    rect.height as f32 * view.zoom,
                ))
            })
            .collect();

        let canvas_area = div()
            .relative()
            .flex_grow()
            .overflow_hidden()
            .bg(c(WELL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(gpui::canvas(
                move |bounds, _, _| bounds_slot.set(Some(bounds)),
                |_, (), _, _| {},
            ))
            .children(image.clone().map(|path| {
                let (width, height) = size.unwrap_or((0, 0));
                gpui::img(path)
                    .image_cache(&self.image_cache)
                    .absolute()
                    .left(px(view.pan_x))
                    .top(px(view.pan_y))
                    .w(px(width as f32 * view.zoom))
                    .h(px(height as f32 * view.zoom))
            }))
            .children(drawn.into_iter().map(|(active, left, top, w, h)| {
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(w))
                    .h(px(h))
                    .border_2()
                    .border_color(c(if active { ACCENT } else { HAIRLINE_STRONG }))
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(point) = this.canvas_point(event.position) {
                        this.calibration.drag_from = Some(point);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.calibration.pan_from = this.raw_canvas_point(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Right,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.calibration.pan_from = None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                // Panning first: while it is happening the transform is
                // moving, so a source-pixel reading taken now would describe
                // the previous frame.
                if let (Some(from), Some(to)) = (
                    this.calibration.pan_from,
                    this.raw_canvas_point(event.position),
                ) {
                    let mut view = this.calibration.view();
                    view.pan_x += to.0 - from.0;
                    view.pan_y += to.1 - from.1;
                    this.calibration.view = Some(view);
                    this.calibration.pan_from = Some(to);
                }
                if let Some(point) = this.canvas_point(event.position) {
                    this.calibration.cursor = Some(point);
                }
                cx.notify();
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    this.finish_drag(event.position);
                    cx.notify();
                }),
            );

        // The magnifier: the same picture, scaled up and shifted so the pixel
        // under the cursor sits at the centre of a small window. Sampling a
        // decoded image would mean decoding it here; letting the renderer
        // scale it costs nothing and shows exactly what will be captured.
        let magnifier = self.calibration.cursor.zip(image).map(|(point, path)| {
            let (width, height) = size.unwrap_or((0, 0));
            let zoom = MAGNIFIER_ZOOM;
            const BOX: f32 = 160.0;
            div()
                .absolute()
                .right(px(12.0))
                .top(px(12.0))
                .w(px(BOX))
                .h(px(BOX))
                .overflow_hidden()
                .border_2()
                .border_color(c(ACCENT))
                .bg(c(WELL))
                .child(
                    gpui::img(path)
                        .image_cache(&self.image_cache)
                        .absolute()
                        .left(px(BOX / 2.0 - point.0 * zoom))
                        .top(px(BOX / 2.0 - point.1 * zoom))
                        .w(px(width as f32 * zoom))
                        .h(px(height as f32 * zoom)),
                )
        });

        // What each target currently holds, so the page says whether a click
        // did anything. Without it the only feedback was the drawn rectangle,
        // and with nothing rendering there was none at all.
        let saved: Vec<String> = Target::ALL
            .into_iter()
            .map(|target| {
                let label = match target {
                    Target::Need => text.slot_need,
                    Target::Have => text.slot_have,
                    Target::Tables => text.slot_tables,
                };
                match self.calibration.rect(target) {
                    Some(rect) => format!(
                        "{label} {},{} {}x{}",
                        rect.x, rect.y, rect.width, rect.height
                    ),
                    None => format!("{label} {}", text.nothing_yet),
                }
            })
            .collect();

        let status = self.calibration.message.clone().unwrap_or_else(|| {
            if self.calibration.image.is_none() {
                text.no_screenshot.to_owned()
            } else {
                let cursor = self
                    .calibration
                    .cursor
                    .map(|(x, y)| format!("  {}, {}", x.round(), y.round()))
                    .unwrap_or_default();
                format!("{}{cursor}", text.drag_to_draw)
            }
        });

        div()
            .flex_grow()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                // A flex column, not a bare block. `canvas_area` grows into
                // this, and growth in a non-flex parent is no growth at all:
                // the box collapsed to zero height, so the screenshot was
                // being drawn correctly into nothing. The magnifier, being
                // absolutely positioned, kept rendering — which is why the
                // loupe showed a picture the canvas did not.
                div()
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .relative()
                    .px_3()
                    .pb_2()
                    .child(canvas_area)
                    .children(magnifier),
            )
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div().flex().gap_4().children(
                            saved.into_iter().map(|line| {
                                mono(line).text_size(fs(FS_12)).text_color(c(TEXT_DATA))
                            }),
                        ),
                    )
                    .child(mono(status).text_size(fs(FS_12)).text_color(c(TEXT_META))),
            )
    }

    /// Window position to a position inside the canvas, or `None` outside it.
    #[cfg(windows)]
    fn raw_canvas_point(&self, position: gpui::Point<gpui::Pixels>) -> Option<(f32, f32)> {
        let bounds = self.canvas_bounds.get()?;
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        (x >= 0.0
            && y >= 0.0
            && x <= f32::from(bounds.size.width)
            && y <= f32::from(bounds.size.height))
        .then_some((x, y))
    }

    /// Window position to a point in the screenshot, or `None` off-canvas.
    #[cfg(windows)]
    fn canvas_point(&self, position: gpui::Point<gpui::Pixels>) -> Option<(f32, f32)> {
        let (x, y) = self.raw_canvas_point(position)?;
        let (source_x, source_y) = self.calibration.view().to_source(x, y);
        let (width, height) = self.calibration.image_size?;
        // Clamped rather than rejected: a drag that runs past the edge should
        // stop at the edge, not discard the whole rectangle.
        Some((
            source_x.clamp(0.0, width as f32),
            source_y.clamp(0.0, height as f32),
        ))
    }

    #[cfg(windows)]
    fn finish_drag(&mut self, position: gpui::Point<gpui::Pixels>) {
        let Some(from) = self.calibration.drag_from.take() else {
            return;
        };
        let Some(to) = self.canvas_point(position) else {
            return;
        };
        let target = self.calibration.target();
        let label = match target {
            crate::calibrate::Target::Need => self.text().slot_need,
            crate::calibrate::Target::Have => self.text().slot_have,
            crate::calibrate::Target::Tables => self.text().slot_tables,
        };
        match crate::calibrate::SourceRect::from_corners(from, to) {
            Some(rect) => {
                self.calibration.set_rect(target, rect);
                self.calibration.message = Some(format!(
                    "{label}  {},{}  {}x{}",
                    rect.x, rect.y, rect.width, rect.height
                ));
            }
            None => self.calibration.message = None,
        }
    }

    #[cfg(windows)]
    fn zoom_calibration(&mut self, factor: f32) {
        let (anchor_x, anchor_y) = self.canvas_bounds.get().map_or((0.0, 0.0), |bounds| {
            (
                f32::from(bounds.size.width) / 2.0,
                f32::from(bounds.size.height) / 2.0,
            )
        });
        let view = self
            .calibration
            .view()
            .zoomed_about(factor, anchor_x, anchor_y);
        self.calibration.view = Some(view);
    }

    #[cfg(windows)]
    fn zoom_calibration_to(&mut self, zoom: f32) {
        let current = self.calibration.view().zoom;
        if current > 0.0 {
            self.zoom_calibration(zoom / current);
        }
    }

    #[cfg(windows)]
    fn fit_calibration(&mut self) {
        let (Some(size), Some(bounds)) = (self.calibration.image_size, self.canvas_bounds.get())
        else {
            return;
        };
        self.calibration.view = Some(crate::calibrate::View::fitted(
            size,
            (f32::from(bounds.size.width), f32::from(bounds.size.height)),
        ));
    }

    /// Starts the system picker on its own thread.
    ///
    /// Not called inline: the dialog pumps messages while it is open, and from
    /// inside an event handler that re-enters the framework while this view is
    /// already mutably borrowed. The process does not hang, it dies — which is
    /// what clicking this button used to do.
    #[cfg(windows)]
    fn load_screenshot(&mut self) {
        let sender = self.shell_tx.clone();
        ptt_platform_win::spawn_pick_image(move |path| {
            let _ = sender.send(ShellMsg::ScreenshotPicked(path));
        });
    }

    /// Measures whatever the picker returned.
    #[cfg(windows)]
    fn screenshot_picked(&mut self, path: Option<std::path::PathBuf>) {
        let Some(path) = path else {
            return;
        };
        let size = std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::calibrate::image_size(&bytes));
        match size {
            Some(size) => {
                self.calibration.image = Some(path);
                self.calibration.image_size = Some(size);
                self.calibration.message = None;
                self.fit_calibration();
            }
            None => {
                // Refused rather than shown at a guessed size: every rectangle
                // drawn on a wrongly scaled picture lands somewhere else.
                self.calibration.message =
                    Some(format!("unreadable image header: {}", path.display()));
            }
        }
    }

    /// Writes the drawn rectangles into the active profile.
    #[cfg(windows)]
    fn apply_drawn_regions(&mut self) {
        let drawn = self.calibration.completed();
        if drawn.is_empty() {
            return;
        }
        let count = drawn.len();
        for (target, rect) in drawn {
            let slot = match target {
                crate::calibrate::Target::Need => RegionSlot::Need,
                crate::calibrate::Target::Have => RegionSlot::Have,
                crate::calibrate::Target::Tables => RegionSlot::Tables,
            };
            self.apply_calibration(slot, rect.x, rect.y, rect.width, rect.height);
        }
        self.calibration.message = Some(format!("{count} applied"));
    }

    /// Installs the profile's factory rectangles for 2560x1440.
    ///
    /// The panel's geometry is fixed for a given resolution, so the numbers
    /// that the corpus was calibrated against are the right ones — drawing
    /// them by hand can only be less accurate. A hand-drawn region that starts
    /// above the tables puts the panel's title and market ratio inside it, and
    /// the row grid then anchors on furniture and reads two rows of twelve.
    ///
    /// Only useful at 2560x1440; on any other desktop the user still has to
    /// draw, which is why this sits beside the calibrate buttons rather than
    /// replacing them.
    #[cfg(windows)]
    fn apply_preset_regions(&mut self) {
        let profile = self.settings.active_profile;
        let (layout, _) = ptt_runtime::pipeline::route_for(profile);
        for (slot, (x, y, width, height)) in [
            (RegionSlot::Need, layout.need_name),
            (RegionSlot::Have, layout.have_name),
            (RegionSlot::Tables, layout.tables),
        ] {
            self.apply_calibration(slot, x, y, width, height);
        }
    }

    /// An asset id as the game writes it, in the client's own language.
    ///
    /// The pipeline speaks ids, and `chaos-orb` is not what the panel says. On
    /// a card read at a glance beside the game, the id costs a translation
    /// step every time; the catalogue already holds the name, keyed by the
    /// profile the watcher is running.
    #[cfg(windows)]
    fn display_name(&self, asset_id: &str) -> String {
        let profile = self.settings.active_profile;
        let (layout, language) = ptt_runtime::pipeline::route_for(profile);
        let Some(asset) = (layout.catalog)().by_id(asset_id) else {
            return asset_id.to_owned();
        };
        let name = match language {
            ptt_recognition::profiles::ProfileLanguage::TraditionalChinese => &asset.name_zh_tw,
            ptt_recognition::profiles::ProfileLanguage::English => &asset.name_en,
        };
        if name.trim().is_empty() {
            asset_id.to_owned()
        } else {
            name.clone()
        }
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
        // The card mirrors the panel the user is looking at: which pair, the
        // rows read off it, and whether the last frame landed. Anything else
        // belongs in the window, which they can alt-tab to; this is for the
        // moment when they cannot.
        let pair = self.report_pair.as_ref().map_or_else(
            || self.text().no_pair_yet.to_owned(),
            |(have, need)| format!("{} -> {}", self.display_name(have), self.display_name(need)),
        );
        // Stated either way — "nothing since the last book" and "the watcher
        // died" look identical on a card that only reports success.
        let verdict = match (&self.fault, &self.last_skip) {
            (Some(fault), _) => format!("{}: {fault}", self.text().fault_prefix),
            (None, Some(reason)) => format!("{} {reason}", self.text().skips_label),
            (None, None) if self.accepted > 0 => {
                format!("{} {}", self.text().accepted_label, self.accepted)
            }
            (None, None) => self.text().nothing_yet.to_owned(),
        };
        let lines = hud_lines(
            &pair,
            &self.last_rows,
            self.text().waiting_for_book,
            &verdict,
        );
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
        if !self.page.needs_a_pair() {
            self.report_lines = match self.pairless_report() {
                Ok(lines) => lines,
                Err(reason) => vec![format!("unavailable: {reason}")],
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

    /// The pages that describe the whole market rather than one pair.
    #[cfg(windows)]
    fn pairless_report(&self) -> Result<Vec<String>, String> {
        use ptt_runtime::pipeline::LIVE_LEAGUE;

        let (context_key, observations) = self.load_window()?;
        match self.page {
            Page::Monitor => {
                ptt_runtime::reports::probe_queue(&observations, &context_key, LIVE_LEAGUE)
            }
            Page::Opportunities => {
                ptt_runtime::reports::opportunities_report(&observations, &context_key, LIVE_LEAGUE)
            }
            // Draws rather than reports; `render_calibrate` reads its own state.
            Page::Calibrate => Ok(Vec::new()),
            // `needs_a_pair` sends the rest through `build_report`.
            page => Err(format!("{page:?} is a pair page")),
        }
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
            Page::Monitor | Page::Opportunities | Page::Calibrate => Ok(Vec::new()),
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
                                    // First in the column, not last. Everything
                                    // below it is a list that grows with the
                                    // market — the probe queue alone runs to
                                    // six suggestions — and a fixed panel at
                                    // the bottom of growing lists is a panel
                                    // that scrolls out of reach. The calibrate
                                    // buttons vanishing after a profile switch
                                    // was exactly that: more missing pairs,
                                    // longer queue, settings pushed off.
                                    .child(self.settings_panel(cx))
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
                                    ),
                            )
                    } else if self.page == Page::Calibrate {
                        self.render_calibrate(cx)
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

/// The HUD's body, composed.
///
/// Pulled out of `refresh_hud` so the shape can be tested: the twelve rows are
/// the point of the card, and `truncate` drops from the end, so a budget too
/// small eats the verdict first and then the last rows — quietly, and only on
/// the frames where the panel is full, which are the ones that matter.
fn hud_lines(pair: &str, rows: &[String], waiting: &str, verdict: &str) -> Vec<String> {
    let mut lines = Vec::with_capacity(HUD_BODY_LINES);
    lines.push(pair.to_owned());
    if rows.is_empty() {
        lines.push(waiting.to_owned());
    } else {
        lines.extend(rows.iter().cloned());
    }
    // Last, so it sits where the eye lands after reading the rows.
    lines.push(String::new());
    lines.push(verdict.to_owned());
    lines.truncate(HUD_BODY_LINES);
    lines
}

#[cfg(test)]
mod hud_tests {
    use super::{HUD_BODY_LINES, HUD_SIZE};

    /// The card must hold a full panel.
    ///
    /// Twelve rows is not a target, it is the panel's maximum — six available
    /// and six competing — and a card that fits eleven answers the question
    /// wrongly rather than partially: the row it drops is the aggregate, which
    /// is the one that says how much is behind the front. The painter stacks
    /// `LINE_HEIGHT` rows from `BODY_TOP` and stops at `FOOT`, silently, so
    /// this is checked here rather than discovered on screen.
    #[test]
    fn the_card_has_room_for_twelve_rows_and_what_frames_them() {
        // Mirrors crates/ptt-platform-win/src/win32/hud.rs.
        const BODY_TOP: i32 = 30;
        const LINE_HEIGHT: i32 = 17;
        const FOOT: i32 = 4;

        let painted = (HUD_SIZE.1 - BODY_TOP - FOOT) / LINE_HEIGHT;
        assert!(
            painted >= HUD_BODY_LINES as i32,
            "the card paints {painted} lines but is asked for {HUD_BODY_LINES}"
        );
    }

    /// A full panel survives composition with its verdict.
    #[test]
    fn a_full_panel_keeps_every_row_and_the_verdict() {
        let rows: Vec<String> = (0..6)
            .map(|index| format!("available #{index} 1:100 stock 5"))
            .chain((0..6).map(|index| format!("competing #{index} 1:101 stock 5")))
            .collect();
        let lines = super::hud_lines("A -> B", &rows, "waiting", "skips need-name");
        for row in &rows {
            assert!(lines.contains(row), "{row} was dropped from the card");
        }
        assert_eq!(
            lines.last().map(String::as_str),
            Some("skips need-name"),
            "the verdict fell off the end: {lines:#?}"
        );
        assert!(lines.len() <= HUD_BODY_LINES);
    }

    /// With nothing captured the card says so rather than showing a bare pair.
    #[test]
    fn an_empty_book_says_it_is_waiting() {
        let lines = super::hud_lines("A -> B", &[], "waiting for a book", "—");
        assert!(
            lines.iter().any(|line| line == "waiting for a book"),
            "{lines:#?}"
        );
    }
}
