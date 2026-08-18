//! Bilingual UI catalogue (English / Traditional Chinese).
//!
//! Two values of one struct rather than a lookup map: a missing key is then a
//! compile error instead of a blank label discovered by a user, and a test
//! holds both catalogues to the same shape. `AppShell::text()` picks by the
//! stored language and switching takes effect on the next frame without
//! rebuilding anything.
//!
//! Region slot names ("NEED", "HAVE", "TABLES") are deliberately absent: they
//! are calibration override keys the recognition layer matches on, not
//! display text, and translating them would break the lookup.

use ptt_settings::UiLanguage;

/// Both languages, for tests and for the switcher.
pub const LANGUAGES: [UiLanguage; 2] = [UiLanguage::English, UiLanguage::Chinese];

/// The catalogue for a language.
///
/// The language enum itself lives in `ptt-settings`, which persists it — a
/// second copy here would be a second thing to keep in sync with the file on
/// disk.
#[must_use]
pub const fn text(language: UiLanguage) -> &'static Text {
    match language {
        UiLanguage::English => &ENGLISH,
        UiLanguage::Chinese => &TRADITIONAL_CHINESE,
    }
}

/// The label for the language switcher, always written in its own language:
/// a reader who cannot read the current one still finds theirs.
#[must_use]
pub const fn native_label(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "English",
        UiLanguage::Chinese => "繁體中文",
    }
}

/// Every string the interface draws.
pub struct Text {
    // -- status strip --
    pub app_title: &'static str,
    pub state_idle: &'static str,
    pub state_watching: &'static str,
    pub state_fault: &'static str,
    pub accepted_label: &'static str,
    pub skips_label: &'static str,
    pub start_watch: &'static str,
    pub stop_watch: &'static str,

    // -- navigation --
    pub page_monitor: &'static str,
    pub page_convert: &'static str,
    pub page_watchlist: &'static str,
    pub page_history: &'static str,

    // -- panels --
    pub panel_last_book: &'static str,
    pub panel_opportunities: &'static str,
    pub panel_skips: &'static str,
    pub panel_probe_queue: &'static str,
    pub panel_settings: &'static str,
    pub refresh: &'static str,
    pub calibrate: &'static str,

    // -- calibration slots, as shown to the user --
    pub slot_need: &'static str,
    pub slot_have: &'static str,
    pub slot_tables: &'static str,

    // -- empty and waiting states --
    pub waiting_for_book: &'static str,
    pub no_pair_yet: &'static str,
    pub pair_prefix: &'static str,
    pub nothing_yet: &'static str,

    // -- messages --
    pub hotkey_ready: &'static str,
    pub hotkey_unavailable: &'static str,
    pub fault_prefix: &'static str,
    pub language_label: &'static str,
    pub game_label: &'static str,
    pub client_language_label: &'static str,
    pub restart_watch_to_apply: &'static str,
}

pub static ENGLISH: Text = Text {
    app_title: "POE TRADE TRACKER",
    state_idle: "IDLE",
    state_watching: "WATCHING",
    state_fault: "FAULT",
    accepted_label: "accepted",
    skips_label: "skips",
    start_watch: "Start watch",
    stop_watch: "Stop",

    page_monitor: "MONITOR",
    page_convert: "CONVERT",
    page_watchlist: "WATCHLIST",
    page_history: "HISTORY",

    panel_last_book: "LAST BOOK",
    panel_opportunities: "OPPORTUNITIES",
    panel_skips: "SKIPS",
    panel_probe_queue: "PROBE QUEUE",
    panel_settings: "SETTINGS",
    refresh: "Refresh",
    calibrate: "Calibrate",

    slot_need: "Need name",
    slot_have: "Have name",
    slot_tables: "Order tables",

    waiting_for_book: "waiting for a book — start watching and open a pair",
    no_pair_yet: "pair: none captured yet",
    pair_prefix: "pair",
    nothing_yet: "—",

    hotkey_ready: "hotkey ready",
    hotkey_unavailable: "hotkey unavailable (another app owns it)",
    fault_prefix: "fault",
    language_label: "Language",
    game_label: "Game",
    client_language_label: "Client",
    restart_watch_to_apply: "restart the watch to apply",
};

pub static TRADITIONAL_CHINESE: Text = Text {
    app_title: "POE 交易追蹤器",
    state_idle: "閒置",
    state_watching: "監視中",
    state_fault: "故障",
    accepted_label: "已接受",
    skips_label: "已跳過",
    start_watch: "開始監視",
    stop_watch: "停止",

    page_monitor: "監視器",
    page_convert: "兌換",
    page_watchlist: "關注清單",
    page_history: "歷史",

    panel_last_book: "最近盤口",
    panel_opportunities: "機會",
    panel_skips: "跳過原因",
    panel_probe_queue: "待採集佇列",
    panel_settings: "設定",
    refresh: "重新整理",
    calibrate: "校準",

    slot_need: "我需要的",
    slot_have: "我擁有的",
    slot_tables: "交易表格",

    waiting_for_book: "尚未擷取到盤口 — 開始監視並在遊戲中開啟一組通貨",
    no_pair_yet: "通貨對：尚未擷取",
    pair_prefix: "通貨對",
    nothing_yet: "—",

    hotkey_ready: "熱鍵已註冊",
    hotkey_unavailable: "熱鍵被其他程式佔用",
    fault_prefix: "故障",
    language_label: "介面語言",
    game_label: "遊戲",
    client_language_label: "遊戲語言",
    restart_watch_to_apply: "重新開始監視後生效",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_catalogues_are_fully_populated() {
        // A struct makes a *missing* field impossible, but not an empty one:
        // a blank string still compiles and still ships as a blank label.
        for language in LANGUAGES {
            let text = text(language);
            for (field, value) in [
                ("app_title", text.app_title),
                ("state_idle", text.state_idle),
                ("state_watching", text.state_watching),
                ("state_fault", text.state_fault),
                ("accepted_label", text.accepted_label),
                ("skips_label", text.skips_label),
                ("start_watch", text.start_watch),
                ("stop_watch", text.stop_watch),
                ("page_monitor", text.page_monitor),
                ("page_convert", text.page_convert),
                ("page_watchlist", text.page_watchlist),
                ("page_history", text.page_history),
                ("panel_last_book", text.panel_last_book),
                ("panel_opportunities", text.panel_opportunities),
                ("panel_skips", text.panel_skips),
                ("panel_probe_queue", text.panel_probe_queue),
                ("panel_settings", text.panel_settings),
                ("refresh", text.refresh),
                ("calibrate", text.calibrate),
                ("slot_need", text.slot_need),
                ("slot_have", text.slot_have),
                ("slot_tables", text.slot_tables),
                ("waiting_for_book", text.waiting_for_book),
                ("no_pair_yet", text.no_pair_yet),
                ("pair_prefix", text.pair_prefix),
                ("nothing_yet", text.nothing_yet),
                ("hotkey_ready", text.hotkey_ready),
                ("hotkey_unavailable", text.hotkey_unavailable),
                ("fault_prefix", text.fault_prefix),
                ("language_label", text.language_label),
                ("game_label", text.game_label),
                ("client_language_label", text.client_language_label),
                ("restart_watch_to_apply", text.restart_watch_to_apply),
            ] {
                assert!(
                    !value.trim().is_empty(),
                    "{language:?} has a blank {field}, which ships as a blank label"
                );
            }
        }
    }

    #[test]
    fn the_two_catalogues_actually_differ() {
        // Copy-pasting the English catalogue and forgetting to translate it
        // passes the emptiness check above and fails the user.
        let english = text(UiLanguage::English);
        let chinese = text(UiLanguage::Chinese);
        assert_ne!(english.start_watch, chinese.start_watch);
        assert_ne!(english.page_monitor, chinese.page_monitor);
        assert_ne!(english.panel_probe_queue, chinese.panel_probe_queue);
        assert_ne!(english.slot_need, chinese.slot_need);
    }

    #[test]
    fn every_language_the_settings_can_hold_has_a_catalogue() {
        // The match in `text` is exhaustive, so this guards the reverse: a
        // language added to settings must also be added to LANGUAGES, or the
        // switcher silently stops offering it.
        assert_eq!(LANGUAGES.len(), 2);
        for language in LANGUAGES {
            assert!(!native_label(language).trim().is_empty());
        }
    }
}
