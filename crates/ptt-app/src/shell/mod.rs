//! Root view: status strip + monitor content (last book, opportunities,
//! skip histogram). P3 skeleton — layout only, visuals iterate later.

mod hud;
pub mod pages;

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Duration;

use gpui::{
    Context, FocusHandle, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px,
};
use gpui_component::StyledExt as _;

use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, chip, hairline_soft, mono, panel, panel_header, spaced,
    status_dot,
};

#[cfg(windows)]
use crate::backend::{Backend, HotkeyRegistration, ShellMsg, UiEvent, spawn_hotkey_thread};

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

/// The report window when settings hold nonsense. The real value comes from
/// `MarketTuning::report_window_hours` — wide enough to load the data the
/// yellow and red freshness lights exist to warn about. (The watch loop's own
/// 2h analysis window stays narrow deliberately: it runs inside the capture
/// loop under a latency budget, and the pair it describes was captured
/// seconds ago — always green.)
const FALLBACK_REPORT_WINDOW_HOURS: i64 = 24;

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
    Settings,
}

impl Page {
    const ALL: [Self; 7] = [
        Self::Monitor,
        Self::Calibrate,
        Self::Opportunities,
        Self::Convert,
        Self::Watchlist,
        Self::History,
        Self::Settings,
    ];

    fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Monitor => text.page_monitor,
            Self::Calibrate => text.page_calibrate,
            Self::Opportunities => text.page_opportunities,
            Self::Convert => text.page_convert,
            Self::Watchlist => text.page_watchlist,
            Self::History => text.page_history,
            Self::Settings => text.page_settings,
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
            // The watchlist is about the focus set, not about a pair. It
            // waited for a book it never used, so an empty session showed it
            // as "waiting" forever while it had coverage to report all along.
            Self::Monitor
            | Self::Opportunities
            | Self::Calibrate
            | Self::Watchlist
            | Self::Settings => false,
            Self::Convert | Self::History => true,
        }
    }

    /// Whether this page answers from the store rather than from its own
    /// state. Calibrate draws a screenshot and Settings draws the settings
    /// file; neither has a book to read.
    const fn reads_the_store(self) -> bool {
        match self {
            Self::Calibrate | Self::Settings => false,
            Self::Monitor
            | Self::Opportunities
            | Self::Convert
            | Self::Watchlist
            | Self::History => true,
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
            Self::Settings => "page-settings",
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
    /// The same rows with their fields intact.
    last_order_rows: Vec<ptt_runtime::pipeline::BookRow>,
    last_analysis: Vec<String>,
    log: VecDeque<String>,
    fault: Option<String>,
    page: Page,
    /// The pair the report pages describe: the last book that was accepted.
    report_pair: Option<(String, String)>,
    /// What the visible page is showing.
    report: crate::state::PageData,
    /// Which request the displayed answer came from.
    ///
    /// The store read happens off the interface thread, so answers can arrive
    /// after the question stopped being the current one — a slow page the
    /// user has already navigated away from, or two refreshes racing after a
    /// book lands. Only the newest generation is allowed to write.
    report_generation: u64,
    /// Pairs the user asked to be reminded about, newest first.
    probe_queue: crate::state::ProbeQueue,
    /// The market tuning boxes on the settings page.
    #[cfg(windows)]
    tuning_inputs: pages::tuning::TuningInputs,
    /// The convert page's currency pickers and holdings box.
    ///
    /// Entities rather than values because a select owns its open menu and an
    /// input owns its cursor: rebuilding either on refresh throws away
    /// whatever the user was in the middle of doing.
    convert_have: pages::convert::AssetSelect,
    convert_need: pages::convert::AssetSelect,
    holdings_input: gpui::Entity<gpui_component::input::InputState>,
    /// The asset list the pickers were last filled from.
    convert_assets: Vec<String>,
    /// True once the user picked a pair by hand, after which an accepted book
    /// for some other pair must not drag the page away.
    pair_chosen_by_user: bool,
    /// The radar's table.
    ///
    /// Created once and refilled, never rebuilt: it owns the scroll position
    /// and the selected row, and a book lands every few seconds.
    radar_table: gpui::Entity<gpui_component::table::TableState<pages::opportunities::RadarTable>>,
    /// True when a new book landed since the visible page was last built.
    report_stale: bool,
}

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        // The table is created here rather than on first render so it can
        // keep its scroll and selection from the very first refresh, and so
        // the shell can subscribe to it: a selection change redraws the table
        // but the detail panel beside it belongs to the shell.
        #[cfg(windows)]
        let language = settings.ui_language;
        #[cfg(not(windows))]
        let language = ptt_settings::UiLanguage::English;
        let radar_table = Self::new_radar_table(window, cx, language);
        #[cfg(windows)]
        let tuning_inputs = {
            let tuning = settings.market_tuning(settings.active_profile.game);
            Self::new_tuning_inputs(window, cx, &tuning)
        };
        let convert_have = Self::new_asset_select(window, cx);
        let convert_need = Self::new_asset_select(window, cx);
        let holdings_input = Self::new_holdings_input(window, cx);
        // A picked currency or a typed holding is a new question, so the page
        // is rebuilt; the read itself is backgrounded, so this stays cheap.
        for (select, is_have) in [(convert_have.clone(), true), (convert_need.clone(), false)] {
            cx.subscribe(&select, move |this: &mut AppShell, _, event, cx| {
                let gpui_component::select::SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                if is_have {
                    this.choose_pair(Some(value.clone()), None);
                } else {
                    this.choose_pair(None, Some(value.clone()));
                }
                cx.notify();
            })
            .detach();
        }
        cx.subscribe(&holdings_input, |this: &mut AppShell, _, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::Change) {
                this.report_stale = true;
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&radar_table, |_, _, event, cx| {
            if matches!(
                event,
                gpui_component::table::TableEvent::SelectRow(_)
                    | gpui_component::table::TableEvent::DoubleClickedRow(_)
            ) {
                cx.notify();
            }
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            #[cfg(windows)]
            tuning_inputs,
            radar_table,
            convert_have,
            convert_need,
            holdings_input,
            convert_assets: Vec::new(),
            pair_chosen_by_user: false,
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
            last_order_rows: Vec::new(),
            last_analysis: Vec::new(),
            log: VecDeque::new(),
            fault: None,
            page: Page::Monitor,
            report_pair: None,
            report: crate::state::PageData::Empty,
            report_generation: 0,
            probe_queue: crate::state::ProbeQueue::default(),
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
                    ShellMsg::ScreenshotPicked(path) => self.screenshot_picked(path),
                }
            }
            if self.report_stale {
                self.refresh_report(cx);
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
                        order_rows,
                        analysis,
                    } => {
                        self.accepted += 1;
                        self.push_log(header.clone());
                        self.last_header = Some(header);
                        self.last_rows = rows;
                        self.last_order_rows = order_rows;
                        self.last_analysis = analysis;
                        // A pair the user picked by hand outranks whatever
                        // panel happens to be open in game.
                        if !self.pair_chosen_by_user {
                            self.report_pair = Some((have_asset_id, need_asset_id));
                        }
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

    /// Opens the calibration page on one region.
    ///
    /// It used to launch a full-screen drag over the live game, which was a
    /// second way to write the same three settings, on a different page, with
    /// no indication which of the two the watcher would use. There is now one
    /// place these numbers are changed. Dragging over the game also asked a
    /// person to find an invisible edge on a moving screen; the same drag on a
    /// still screenshot can be zoomed into.
    #[cfg(windows)]
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
                self.backend = Some(Backend::start(self.settings.ui_language));
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
}

impl AppShell {
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
    /// Rebuilds the visible page's answer, off the interface thread.
    ///
    /// The read is a database open, a windowed query and a full book build,
    /// which on the interface thread showed up as the window locking for the
    /// duration every time a book landed. It runs on the background executor
    /// instead, and the answer is only accepted if it is still the answer to
    /// the current question.
    #[cfg(windows)]
    fn refresh_report(&mut self, cx: &mut Context<Self>) {
        use crate::state::PageData;

        self.report_stale = false;
        self.report_generation = self.report_generation.wrapping_add(1);
        let generation = self.report_generation;

        if !self.page.reads_the_store() {
            self.report = PageData::Empty;
            return;
        }
        if self.page.needs_a_pair() && self.report_pair.is_none() {
            self.report = PageData::WaitingForPair;
            return;
        }
        let Some(request) = self.page_request(cx) else {
            self.report = PageData::WaitingForPair;
            return;
        };
        // Only announce loading when there is nothing to look at; replacing a
        // rendered page with a spinner on every accepted book is a flicker,
        // not information.
        if !self.report.is_content() {
            self.report = PageData::Loading;
        }
        cx.spawn(async move |this, cx| {
            let data = cx
                .background_executor()
                .spawn(async move { build_page_data(&request) })
                .await;
            this.update(cx, |this: &mut AppShell, cx| {
                if this.report_generation == generation {
                    this.report = data;
                    this.sync_radar_table(cx);
                    this.close_answered_probes();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Everything the background read needs, copied out of the shell.
    ///
    /// A snapshot rather than a borrow: the task outlives this frame, and the
    /// settings behind it can change while it runs.
    #[cfg(windows)]
    fn page_request(&self, cx: &gpui::App) -> Option<PageRequest> {
        let profile = self.settings.active_profile;
        Some(PageRequest {
            page: self.page,
            pair: self.report_pair.clone(),
            holdings: self.holdings_value(cx),
            profile,
            language: self.settings.ui_language,
            tuning: self.settings.market_tuning(profile.game),
        })
    }

    /// Drops pinned probes for pairs the newest answer can already price.
    ///
    /// Only the watchlist and the monitor know which pairs are still
    /// incomplete, so this runs where their answers arrive rather than on a
    /// timer.
    #[cfg(windows)]
    fn close_answered_probes(&mut self) {
        // Only the coverage pass knows which pairs are still incomplete, so
        // this runs where its answer arrives rather than on a timer.
        let crate::state::PageData::Probes(model) = &self.report else {
            return;
        };
        let missing: Vec<(String, String)> = model
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.from_asset_id.as_str().to_owned(),
                    candidate.to_asset_id.as_str().to_owned(),
                )
            })
            .collect();
        self.probe_queue.retain_missing(&missing);
    }

    /// Queues a pair for the user to go and flip.
    #[cfg(windows)]
    pub(crate) fn pin_probe(&mut self, from: &str, to: &str, reason: &str) {
        self.probe_queue.pin(crate::state::PinnedProbe {
            from_asset_id: from.to_owned(),
            to_asset_id: to.to_owned(),
            reason: reason.to_owned(),
        });
    }

    #[cfg(windows)]
    pub(crate) fn unpin_probe(&mut self, from: &str, to: &str) {
        self.probe_queue.unpin(from, to);
    }

    #[cfg(not(windows))]
    fn show_page(&mut self, page: Page) {
        self.page = page;
    }

    #[cfg(not(windows))]
    fn refresh_report(&mut self, _cx: &mut Context<Self>) {
        self.report_stale = false;
    }

    /// The lines a text page draws, including the ones that describe an
    /// absence.
    ///
    /// An empty answer, a page still reading, a page with no pair yet and a
    /// page whose read failed are four different things, and a bare empty
    /// list says the same nothing for all four.
    /// The probe queue: what to go and flip next.
    ///
    /// Pinned pairs sit above the suggestions, because a pair the user chose
    /// to keep is a commitment and a suggestion is an opinion. Both leave the
    /// list the same way — by being captured.
    #[cfg(windows)]
    fn probe_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::state::PageData;

        let text = self.text();
        let language = self.language();
        let mut body = div().p_3().flex().flex_col().gap_1();

        for entry in self.probe_queue.entries() {
            let (from, to) = (entry.from_asset_id.clone(), entry.to_asset_id.clone());
            body = body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(chip(StatusKind::Monitoring, text.pinned_label))
                    .child(
                        mono(format!("{from} -> {to}"))
                            .text_size(fs(FS_12))
                            .flex_grow(),
                    )
                    .child(
                        mono(entry.reason.clone())
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META)),
                    )
                    .child(
                        button("probe-unpin", LedgerButton::Quiet, text.unpin_label, cx).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.unpin_probe(&from, &to);
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }

        match &self.report {
            PageData::Probes(model) => {
                let candidates: Vec<_> = model
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        !self.probe_queue.is_pinned(
                            candidate.from_asset_id.as_str(),
                            candidate.to_asset_id.as_str(),
                        )
                    })
                    .take(6)
                    .collect();
                if self.probe_queue.entries().is_empty() && candidates.is_empty() {
                    body = body.child(mono(text.nothing_yet).text_size(fs(FS_12)));
                }
                for candidate in candidates {
                    let from = candidate.from_asset_id.as_str().to_owned();
                    let to = candidate.to_asset_id.as_str().to_owned();
                    let reason = ptt_runtime::report_text::probe_reason(language, candidate.reason);
                    let (pin_from, pin_to, pin_reason) =
                        (from.clone(), to.clone(), reason.to_owned());
                    body = body.child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                mono(format!("{from} -> {to}"))
                                    .text_size(fs(FS_12))
                                    .flex_grow(),
                            )
                            .child(mono(reason).text_size(fs(FS_10_5)).text_color(c(TEXT_META)))
                            .child(
                                button("probe-pin", LedgerButton::Quiet, text.pin_label, cx)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pin_probe(&pin_from, &pin_to, &pin_reason);
                                        cx.notify();
                                    })),
                            ),
                    );
                }
            }
            _ => {
                body = body.children(
                    self.report_body()
                        .into_iter()
                        .map(|line| mono(line).text_size(fs(FS_12))),
                );
            }
        }

        panel()
            .overflow_hidden()
            .child(panel_header(text.panel_probe_queue))
            .child(body)
    }

    #[cfg(not(windows))]
    fn probe_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        panel()
    }

    /// The most recent accepted book, as the panel showed it.
    ///
    /// Twelve rows at most, so nothing here needs virtualising; the columns
    /// exist so a rate can be read without reading the sentence around it,
    /// and so the aggregate row — which restates a tier as "this and
    /// everything worse" — is visibly not a listing of its own.
    fn last_book_panel(&self) -> gpui::Div {
        let text = self.text();
        let Some(header) = &self.last_header else {
            return div()
                .p_3()
                .child(mono(text.waiting_for_book).text_size(fs(FS_12)));
        };
        let mut body = div().p_3().flex().flex_col().gap_1().child(
            mono(header.clone())
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_META)),
        );
        for row in &self.last_order_rows {
            body = body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .h(px(H_ROW))
                    .text_size(fs(FS_11_5))
                    .child(
                        div()
                            .w(px(78.))
                            .flex_none()
                            .text_color(c(TEXT_META))
                            .child(SharedString::from(row.side.to_owned())),
                    )
                    .child(
                        mono(format!("#{}", row.row_index))
                            .w(px(30.))
                            .flex_none()
                            .text_color(c(TEXT_DISABLED)),
                    )
                    .child(mono(row.rate.clone()).w(px(110.)).flex_none())
                    .child(
                        mono(row.stock.to_string())
                            .w(px(70.))
                            .flex_none()
                            .text_color(c(TEXT_SECONDARY)),
                    )
                    .children(
                        row.aggregate
                            .then(|| chip(StatusKind::Idle, text.aggregate_row)),
                    ),
            );
        }
        body
    }

    fn report_body(&self) -> Vec<String> {
        use crate::state::PageData;

        let text = self.text();
        match &self.report {
            PageData::Text(lines) if !lines.is_empty() => lines.clone(),
            // `probe_panel` draws these as rows; reaching here means some
            // other page is showing the monitor's answer.
            PageData::Text(_)
            | PageData::Empty
            | PageData::Probes(_)
            | PageData::Opportunities(_)
            | PageData::Convert(_)
            | PageData::Watchlist(_)
            | PageData::History(_) => vec![text.nothing_yet.to_owned()],
            PageData::WaitingForPair => vec![text.waiting_for_book.to_owned()],
            PageData::Loading => vec![crate::state::loading_label(self.language()).to_owned()],
            PageData::Failed(reason) => vec![format!("{}: {reason}", text.fault_prefix)],
        }
    }

    /// The reader's language.
    fn language(&self) -> ptt_settings::UiLanguage {
        #[cfg(windows)]
        {
            self.settings.ui_language
        }
        #[cfg(not(windows))]
        {
            ptt_settings::UiLanguage::English
        }
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
        let lines = self.report_body();
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
                                    this.refresh_report(cx);
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

/// Everything one background read needs.
#[cfg(windows)]
#[derive(Clone)]
struct PageRequest {
    page: Page,
    pair: Option<(String, String)>,
    /// The holding the convert page is pricing, when the box holds a number.
    holdings: Option<u64>,
    profile: ptt_core::ProfileId,
    language: ptt_settings::UiLanguage,
    tuning: ptt_settings::MarketTuning,
}

/// Reads the store and builds one page's answer.
///
/// A free function on purpose: it runs on the background executor, where a
/// borrow of the shell could not follow it.
#[cfg(windows)]
fn build_page_data(request: &PageRequest) -> crate::state::PageData {
    use crate::state::PageData;

    if request.page == Page::Monitor {
        return match load_probe_queue(request) {
            Ok(model) => PageData::Probes(Box::new(model)),
            Err(reason) => PageData::Failed(reason),
        };
    }
    if request.page == Page::Opportunities {
        return match load_opportunities(request) {
            Ok(model) => PageData::Opportunities(Box::new(model)),
            Err(reason) => PageData::Failed(reason),
        };
    }
    if request.page == Page::Watchlist {
        return match load_watchlist(request) {
            Ok(model) => PageData::Watchlist(Box::new(model)),
            Err(reason) => PageData::Failed(reason),
        };
    }
    if request.page == Page::History {
        return match load_history(request) {
            Ok(Some(model)) => PageData::History(Box::new(model)),
            Ok(None) => PageData::WaitingForPair,
            Err(reason) => PageData::Failed(reason),
        };
    }
    if request.page == Page::Convert {
        return match load_convert(request) {
            Ok(Some(model)) => PageData::Convert(Box::new(model)),
            Ok(None) => PageData::WaitingForPair,
            Err(reason) => PageData::Failed(reason),
        };
    }
    match load_page_lines(request) {
        Ok(lines) => PageData::Text(lines),
        Err(reason) => PageData::Failed(reason),
    }
}

/// The probe queue as rows, so each pair can carry its own controls.
#[cfg(windows)]
fn load_probe_queue(
    request: &PageRequest,
) -> Result<ptt_runtime::reports::ProbeQueueModel, String> {
    use ptt_runtime::pipeline::LIVE_LEAGUE;

    let (context_key, observations) = load_window(request)?;
    ptt_runtime::reports::probe_queue_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
    )
}

/// The observation window the report pages read.
///
/// The league is the pipeline's, not a second literal: the writer and the
/// reader agree on the context key or every page silently reads an empty
/// book.
#[cfg(windows)]
fn load_window(
    request: &PageRequest,
) -> Result<(String, Vec<ptt_trade_domain::MarketEdgeObservation>), String> {
    use ptt_runtime::live::live_context;
    use ptt_runtime::pipeline::{LIVE_LEAGUE, default_database_path};

    let store = ptt_storage::MarketStore::open(default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    // The profile is the pipeline's too, for the same reason the league is:
    // the context key mixes in the game, so a reader on POE2 and a writer on
    // POE1 agree on the league and still share nothing.
    let context =
        live_context(request.profile, LIVE_LEAGUE).map_err(|error| format!("{error:?}"))?;
    let context_key = context.stable_key();
    // Clamped to a year: the window only bounds how much history a page
    // loads, and anything past that is the whole database anyway.
    let window_hours = i64::try_from(request.tuning.report_window_hours)
        .unwrap_or(FALLBACK_REPORT_WINDOW_HOURS)
        .clamp(1, 24 * 365);
    let observations = store
        .load_observations(
            &context_key,
            Some(chrono::Utc::now() - chrono::Duration::hours(window_hours)),
        )
        .map_err(|error| format!("load: {error}"))?;
    Ok((context_key, observations))
}

/// The radar's ranked routes.
#[cfg(windows)]
fn load_opportunities(
    request: &PageRequest,
) -> Result<ptt_runtime::reports::OpportunitiesModel, String> {
    use ptt_runtime::pipeline::LIVE_LEAGUE;

    let (context_key, observations) = load_window(request)?;
    ptt_runtime::reports::opportunities_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
    )
}

/// The focus set, its valuations and its gaps.
#[cfg(windows)]
fn load_watchlist(request: &PageRequest) -> Result<ptt_runtime::reports::WatchlistModel, String> {
    use ptt_runtime::pipeline::LIVE_LEAGUE;

    let (context_key, observations) = load_window(request)?;
    ptt_runtime::reports::watchlist_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
    )
}

/// One pair's price series.
#[cfg(windows)]
fn load_history(
    request: &PageRequest,
) -> Result<Option<ptt_runtime::reports::HistoryModel>, String> {
    use ptt_runtime::live::domain_asset_id;

    let Some((have, need)) = &request.pair else {
        return Ok(None);
    };
    let (context_key, observations) = load_window(request)?;
    let have = domain_asset_id(have).map_err(|error| format!("{error:?}"))?;
    let need = domain_asset_id(need).map_err(|error| format!("{error:?}"))?;
    ptt_runtime::reports::history_model(
        &observations,
        &context_key,
        &have,
        &need,
        &request.tuning,
        request.language,
    )
    .map(Some)
}

/// "I hold X and want Y", priced at the requested size.
#[cfg(windows)]
fn load_convert(
    request: &PageRequest,
) -> Result<Option<ptt_runtime::reports::ConvertModel>, String> {
    use ptt_runtime::live::domain_asset_id;

    let Some((have, need)) = &request.pair else {
        return Ok(None);
    };
    let (context_key, observations) = load_window(request)?;
    let have = domain_asset_id(have).map_err(|error| format!("{error:?}"))?;
    let need = domain_asset_id(need).map_err(|error| format!("{error:?}"))?;
    ptt_runtime::reports::convert_model(
        &observations,
        &context_key,
        &have,
        &need,
        request.holdings,
        &request.tuning,
        request.language,
    )
    .map(Some)
}

#[cfg(windows)]
fn load_page_lines(request: &PageRequest) -> Result<Vec<String>, String> {
    use ptt_runtime::live::domain_asset_id;
    use ptt_runtime::reports;

    let (context_key, observations) = load_window(request)?;
    let tuning = &request.tuning;
    let language = request.language;

    // Page dispatch happens here and nowhere else. It used to happen twice,
    // at two depths, and the two disagreed: one returned early on Monitor
    // while the other carried a Monitor branch that could therefore never
    // run, leaving the probe queue permanently blank.
    match request.page {
        // Answered as a model by `load_probe_queue`.
        Page::Monitor => Ok(Vec::new()),
        // Answered as a model by `load_opportunities`.
        Page::Opportunities => Ok(Vec::new()),
        // Answered as a model by `load_watchlist`.
        Page::Watchlist => Ok(Vec::new()),
        Page::Convert | Page::History => {
            let Some((have, need)) = &request.pair else {
                return Ok(Vec::new());
            };
            let have = domain_asset_id(have).map_err(|error| format!("{error:?}"))?;
            let need = domain_asset_id(need).map_err(|error| format!("{error:?}"))?;
            if request.page == Page::Convert {
                // Answered as a model by `load_convert`.
                Ok(Vec::new())
            } else {
                reports::history_report(&observations, &context_key, &have, &need, tuning, language)
            }
        }
        // `reads_the_store` keeps these from reaching a background read.
        Page::Calibrate | Page::Settings => Ok(Vec::new()),
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rebuilding a select needs a window, which the background answer
        // does not carry, so the pickers are brought up to date on the first
        // frame after one lands.
        #[cfg(windows)]
        self.sync_convert_selects(window, cx);
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
            .map(|(reason, count)| {
                format!(
                    "{count:>5}  {}",
                    skip_label(reason, self.settings.ui_language)
                )
            })
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
                    if self.page == Page::Settings {
                        div()
                            .flex_grow()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .child(self.settings_panel(cx))
                            .child(self.tuning_panel(cx))
                    } else if self.page == Page::Monitor {
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
                                    .child(self.last_book_panel()),
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
                                    .child(self.probe_panel(cx)),
                            )
                    } else if self.page == Page::Opportunities {
                        self.render_opportunities(cx)
                    } else if self.page == Page::Convert {
                        self.render_convert(cx)
                    } else if self.page == Page::Watchlist {
                        self.render_watchlist(cx)
                    } else if self.page == Page::History {
                        self.render_history(cx)
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
/// A skip reason in the reader's language.
///
/// Keyed by the stable bucket name rather than by the typed reason, because
/// that name is what the session tallies by and what the histogram groups on.
/// Translating the key itself would split one bucket into two the moment the
/// language changed mid-session.
///
/// An unrecognised key is shown as itself. A bucket name on screen is a poor
/// label, but it is honest — far better than folding an unknown skip into some
/// known one and reporting the wrong cause.
fn skip_label(key: &str, language: ptt_settings::UiLanguage) -> String {
    let text = crate::i18n::text(language);
    // The row rejects carry their own typed detail after the colon, and that
    // detail is worth keeping: `rows:NoBands` and `rows:ImplausibleBand` are
    // different problems with different fixes.
    if let Some(detail) = key.strip_prefix("rows:") {
        return format!("{} ({detail})", text.skip_rows);
    }
    match key {
        "decode" => text.skip_decode.to_owned(),
        "ocr" => text.skip_ocr.to_owned(),
        "need-name" => text.skip_need_name.to_owned(),
        "have-name" => text.skip_have_name.to_owned(),
        "empty-book" => text.skip_empty_book.to_owned(),
        "rows-out-of-order" => text.skip_out_of_order.to_owned(),
        "confirmation-mismatch" => text.skip_confirmation.to_owned(),
        "duplicate" => text.skip_duplicate.to_owned(),
        other => other.to_owned(),
    }
}

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
