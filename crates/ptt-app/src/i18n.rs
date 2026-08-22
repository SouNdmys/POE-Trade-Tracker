//! Bilingual UI catalogue (English / Simplified Chinese).
//!
//! Two values of one struct rather than a lookup map: a missing key is then a
//! compile error instead of a blank label discovered by a user, and a test
//! holds both catalogues to the same shape. `AppShell::text()` picks by the
//! stored language and switching takes effect on the next frame without
//! rebuilding anything.
//!
//! The interface's Chinese is Simplified while the clients this reads are
//! English and Traditional. That is not an inconsistency: the client language
//! says which lexicon the recogniser matches currency names against, and the
//! interface language says what the person reading the interface prefers.
//! Someone playing the Traditional Chinese client because that is the only
//! Chinese client Path of Exile ships is not thereby a Traditional Chinese
//! reader. The two settings sit in different places for that reason.
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
        UiLanguage::Chinese => &SIMPLIFIED_CHINESE,
    }
}

/// The label for the language switcher, always written in its own language:
/// a reader who cannot read the current one still finds theirs.
#[must_use]
pub const fn native_label(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "English",
        UiLanguage::Chinese => "简体中文",
    }
}

/// The label for one leg of a route.
///
/// A function rather than a field: English counts before the noun and Chinese
/// after it, so no single template with one slot fits both.
#[must_use]
pub fn leg_label(language: UiLanguage, index: usize) -> String {
    match language {
        UiLanguage::English => format!("leg {index}"),
        UiLanguage::Chinese => format!("第 {index} 腿"),
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
    pub page_opportunities: &'static str,
    pub page_convert: &'static str,
    pub page_watchlist: &'static str,
    pub page_history: &'static str,

    // -- panels --
    pub panel_last_book: &'static str,
    pub panel_opportunities: &'static str,
    pub panel_skips: &'static str,
    pub panel_probe_queue: &'static str,
    /// How many order rows a book carried. `{}` is the count.
    pub book_rows: &'static str,
    pub panel_settings: &'static str,
    pub refresh: &'static str,
    pub use_preset: &'static str,
    pub page_calibrate: &'static str,
    pub page_settings: &'static str,

    // -- probe queue --
    pub pinned_label: &'static str,
    pub pin_label: &'static str,
    pub unpin_label: &'static str,

    // -- route detail panel --
    pub detail_header: &'static str,
    pub detail_route: &'static str,
    pub detail_stake: &'static str,
    pub detail_return: &'static str,
    pub detail_leg: &'static str,
    pub detail_capture: &'static str,
    pub detail_risks: &'static str,
    pub detail_reasons: &'static str,

    // -- convert page --
    pub convert_have_label: &'static str,
    pub convert_need_label: &'static str,
    pub convert_holdings_label: &'static str,
    pub convert_pick: &'static str,
    pub convert_size_down: &'static str,
    pub convert_stranded: &'static str,
    pub maker_header: &'static str,
    pub maker_instant_label: &'static str,
    pub maker_spread_label: &'static str,
    pub maker_depth_label: &'static str,

    // -- watchlist page --
    pub role_target: &'static str,
    pub role_bridge: &'static str,
    pub role_watch_only: &'static str,
    pub role_unlisted: &'static str,
    pub settlement_label: &'static str,
    /// What settlement currencies are for, said where they are shown.
    ///
    /// The four role buttons are named for intent and act on mechanics,
    /// and the two only sometimes coincide. Left unsaid, a reader
    /// reasonably concludes that a currency they did not list is a
    /// currency the program ignores, which has never been true.
    pub settlement_hint: &'static str,
    /// What each of the four roles actually changes, one line each, in
    /// the order the buttons sit in. Newline separated so the page can
    /// lay them out as rows without four catalogue fields to keep aligned.
    pub roles_legend: &'static str,
    /// Adds the picked currency to the settlement set.
    pub settlement_add: &'static str,
    /// Why the last settlement currency cannot be removed.
    pub settlement_last: &'static str,
    pub valuation_two_sided: &'static str,
    pub valuation_one_sided: &'static str,
    pub suggestion_label: &'static str,
    pub adopt_label: &'static str,
    /// Stops a suggestion being offered again.
    pub ignore_label: &'static str,
    /// Removes a valuation row from the list (display only).
    pub hide_label: &'static str,
    /// "{} hidden" + the action that brings them back.
    pub hidden_count: &'static str,
    pub unhide_all: &'static str,
    /// One unit's worth in the anchor: "≈ {} {} each".
    pub per_unit: &'static str,
    /// Whether routes may pass through a focus target.
    pub route_through_targets: &'static str,
    pub toggle_on: &'static str,
    pub toggle_off: &'static str,
    pub coverage_label: &'static str,
    pub coverage_header: &'static str,

    // -- history page --
    pub history_pair: &'static str,
    pub history_latest: &'static str,
    pub history_taker: &'static str,
    pub history_maker: &'static str,
    pub history_band: &'static str,
    pub history_range: &'static str,
    pub history_spread: &'static str,
    pub history_points: &'static str,
    pub history_chart: &'static str,
    pub history_too_short: &'static str,

    // -- monitor page --
    pub aggregate_row: &'static str,

    // -- market tuning --
    pub tuning_header: &'static str,
    pub tuning_fresh: &'static str,
    pub tuning_usable: &'static str,
    pub tuning_stale: &'static str,
    pub tuning_skew: &'static str,
    pub tuning_sizes: &'static str,
    pub tuning_max_hops: &'static str,
    pub tuning_stake: &'static str,
    pub tuning_results: &'static str,
    pub tuning_min_bps: &'static str,
    pub tuning_expansions: &'static str,
    pub tuning_thin: &'static str,
    pub tuning_outlier: &'static str,
    pub tuning_window: &'static str,
    pub tuning_apply: &'static str,
    pub tuning_saved: &'static str,
    pub tuning_invalid: &'static str,
    pub load_screenshot: &'static str,
    pub zoom_in: &'static str,
    pub zoom_out: &'static str,
    pub fit_window: &'static str,
    pub actual_size: &'static str,
    pub apply_regions: &'static str,
    pub drag_to_draw: &'static str,
    pub no_screenshot: &'static str,
    pub hud_accepted_count: &'static str,
    pub skip_decode: &'static str,
    pub skip_ocr: &'static str,
    pub skip_need_name: &'static str,
    pub skip_have_name: &'static str,
    pub skip_empty_book: &'static str,
    pub skip_rows: &'static str,
    pub skip_out_of_order: &'static str,
    pub skip_confirmation: &'static str,
    pub skip_duplicate: &'static str,
    pub preset_applied: &'static str,
    pub applied: &'static str,
    pub nothing_to_apply: &'static str,
    pub guide_hint: &'static str,
    pub hint_need: &'static str,
    pub hint_have: &'static str,
    pub hint_tables: &'static str,

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

    // -- data-page table columns --
    pub radar_column_kind: &'static str,
    pub radar_column_route: &'static str,
    pub radar_column_edge: &'static str,
    pub radar_column_out: &'static str,
    /// How much of the route's currencies the market is showing, in the
    /// settlement anchor. The list's primary sort key, so it has to be on
    /// screen — and named for circulation rather than for a ceiling, because
    /// listings are replaced as they are taken.
    pub radar_column_depth: &'static str,
    pub radar_column_verdict: &'static str,
    pub radar_column_light: &'static str,
    pub radar_column_risks: &'static str,
}

impl Text {
    /// Every field, paired with its name.
    ///
    /// Here rather than inside one test because two tests walk it, and a
    /// list maintained twice is a list that ends up covering different
    /// fields in each — with the newest ones, which are the ones most
    /// likely to be wrong, missing from whichever copy was forgotten.
    #[cfg(test)]
    fn fields(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("app_title", self.app_title),
            ("state_idle", self.state_idle),
            ("state_watching", self.state_watching),
            ("state_fault", self.state_fault),
            ("accepted_label", self.accepted_label),
            ("skips_label", self.skips_label),
            ("start_watch", self.start_watch),
            ("stop_watch", self.stop_watch),
            ("page_monitor", self.page_monitor),
            ("page_opportunities", self.page_opportunities),
            ("page_convert", self.page_convert),
            ("page_watchlist", self.page_watchlist),
            ("page_history", self.page_history),
            ("panel_last_book", self.panel_last_book),
            ("panel_opportunities", self.panel_opportunities),
            ("panel_skips", self.panel_skips),
            ("panel_probe_queue", self.panel_probe_queue),
            ("book_rows", self.book_rows),
            ("panel_settings", self.panel_settings),
            ("refresh", self.refresh),
            ("use_preset", self.use_preset),
            ("page_calibrate", self.page_calibrate),
            ("page_settings", self.page_settings),
            ("pinned_label", self.pinned_label),
            ("pin_label", self.pin_label),
            ("unpin_label", self.unpin_label),
            ("detail_header", self.detail_header),
            ("detail_route", self.detail_route),
            ("detail_stake", self.detail_stake),
            ("detail_return", self.detail_return),
            ("detail_leg", self.detail_leg),
            ("detail_capture", self.detail_capture),
            ("detail_risks", self.detail_risks),
            ("detail_reasons", self.detail_reasons),
            ("convert_have_label", self.convert_have_label),
            ("convert_need_label", self.convert_need_label),
            ("convert_holdings_label", self.convert_holdings_label),
            ("convert_pick", self.convert_pick),
            ("convert_size_down", self.convert_size_down),
            ("convert_stranded", self.convert_stranded),
            ("maker_header", self.maker_header),
            ("maker_instant_label", self.maker_instant_label),
            ("maker_spread_label", self.maker_spread_label),
            ("maker_depth_label", self.maker_depth_label),
            ("role_target", self.role_target),
            ("role_bridge", self.role_bridge),
            ("role_watch_only", self.role_watch_only),
            ("role_unlisted", self.role_unlisted),
            ("settlement_label", self.settlement_label),
            ("settlement_hint", self.settlement_hint),
            ("roles_legend", self.roles_legend),
            ("settlement_add", self.settlement_add),
            ("settlement_last", self.settlement_last),
            ("valuation_two_sided", self.valuation_two_sided),
            ("valuation_one_sided", self.valuation_one_sided),
            ("suggestion_label", self.suggestion_label),
            ("adopt_label", self.adopt_label),
            ("ignore_label", self.ignore_label),
            ("hide_label", self.hide_label),
            ("hidden_count", self.hidden_count),
            ("unhide_all", self.unhide_all),
            ("per_unit", self.per_unit),
            ("route_through_targets", self.route_through_targets),
            ("toggle_on", self.toggle_on),
            ("toggle_off", self.toggle_off),
            ("coverage_label", self.coverage_label),
            ("coverage_header", self.coverage_header),
            ("history_pair", self.history_pair),
            ("history_latest", self.history_latest),
            ("history_taker", self.history_taker),
            ("history_maker", self.history_maker),
            ("history_band", self.history_band),
            ("history_range", self.history_range),
            ("history_spread", self.history_spread),
            ("history_points", self.history_points),
            ("history_chart", self.history_chart),
            ("history_too_short", self.history_too_short),
            ("aggregate_row", self.aggregate_row),
            ("tuning_header", self.tuning_header),
            ("tuning_fresh", self.tuning_fresh),
            ("tuning_usable", self.tuning_usable),
            ("tuning_stale", self.tuning_stale),
            ("tuning_skew", self.tuning_skew),
            ("tuning_sizes", self.tuning_sizes),
            ("tuning_max_hops", self.tuning_max_hops),
            ("tuning_stake", self.tuning_stake),
            ("tuning_results", self.tuning_results),
            ("tuning_min_bps", self.tuning_min_bps),
            ("tuning_expansions", self.tuning_expansions),
            ("tuning_thin", self.tuning_thin),
            ("tuning_outlier", self.tuning_outlier),
            ("tuning_window", self.tuning_window),
            ("tuning_apply", self.tuning_apply),
            ("tuning_saved", self.tuning_saved),
            ("tuning_invalid", self.tuning_invalid),
            ("load_screenshot", self.load_screenshot),
            ("zoom_in", self.zoom_in),
            ("zoom_out", self.zoom_out),
            ("fit_window", self.fit_window),
            ("actual_size", self.actual_size),
            ("apply_regions", self.apply_regions),
            ("drag_to_draw", self.drag_to_draw),
            ("no_screenshot", self.no_screenshot),
            ("hud_accepted_count", self.hud_accepted_count),
            ("skip_decode", self.skip_decode),
            ("skip_ocr", self.skip_ocr),
            ("skip_need_name", self.skip_need_name),
            ("skip_have_name", self.skip_have_name),
            ("skip_empty_book", self.skip_empty_book),
            ("skip_rows", self.skip_rows),
            ("skip_out_of_order", self.skip_out_of_order),
            ("skip_confirmation", self.skip_confirmation),
            ("skip_duplicate", self.skip_duplicate),
            ("preset_applied", self.preset_applied),
            ("applied", self.applied),
            ("nothing_to_apply", self.nothing_to_apply),
            ("guide_hint", self.guide_hint),
            ("hint_need", self.hint_need),
            ("hint_have", self.hint_have),
            ("hint_tables", self.hint_tables),
            ("slot_need", self.slot_need),
            ("slot_have", self.slot_have),
            ("slot_tables", self.slot_tables),
            ("waiting_for_book", self.waiting_for_book),
            ("no_pair_yet", self.no_pair_yet),
            ("pair_prefix", self.pair_prefix),
            ("nothing_yet", self.nothing_yet),
            ("hotkey_ready", self.hotkey_ready),
            ("hotkey_unavailable", self.hotkey_unavailable),
            ("fault_prefix", self.fault_prefix),
            ("language_label", self.language_label),
            ("game_label", self.game_label),
            ("client_language_label", self.client_language_label),
            ("restart_watch_to_apply", self.restart_watch_to_apply),
            ("radar_column_kind", self.radar_column_kind),
            ("radar_column_route", self.radar_column_route),
            ("radar_column_edge", self.radar_column_edge),
            ("radar_column_out", self.radar_column_out),
            ("radar_column_depth", self.radar_column_depth),
            ("radar_column_verdict", self.radar_column_verdict),
            ("radar_column_light", self.radar_column_light),
            ("radar_column_risks", self.radar_column_risks),
        ]
    }
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
    page_opportunities: "RADAR",
    page_convert: "CONVERT",
    page_watchlist: "WATCHLIST",
    page_history: "HISTORY",

    panel_last_book: "LAST BOOK",
    panel_opportunities: "OPPORTUNITIES",
    panel_skips: "SKIPS",
    panel_probe_queue: "PROBE QUEUE",
    book_rows: "{} rows",
    panel_settings: "SETTINGS",
    refresh: "Refresh",
    use_preset: "Preset 2560x1440",
    page_calibrate: "CALIBRATE",
    page_settings: "SETTINGS",
    pinned_label: "pinned",
    pin_label: "pin",
    unpin_label: "remove",
    detail_header: "route detail",
    detail_route: "route",
    detail_stake: "stake",
    detail_return: "return",
    detail_leg: "leg",
    detail_capture: "captured",
    detail_risks: "risks",
    detail_reasons: "why",
    convert_have_label: "have",
    convert_need_label: "want",
    convert_holdings_label: "holding",
    convert_pick: "pick a currency",
    convert_size_down: "size down",
    convert_stranded: "stranded",
    maker_header: "listing strategy",
    maker_instant_label: "take now at",
    maker_spread_label: "front vs take",
    maker_depth_label: "visible depth",
    role_target: "target",
    role_bridge: "bridge",
    role_watch_only: "watch",
    role_unlisted: "off",
    settlement_label: "settlement",
    settlement_hint: "profit is measured in these, and every cycle starts and ends here - change them on the settings page",
    roles_legend: concat!(
        "target  ·  keeps watching its gaps even when nothing has been captured lately
",
        "bridge  ·  routed through, never a destination
",
        "price only  ·  priced, never routed
",
        "off  ·  not watched - but anything captured is still used for arbitrage",
    ),
    settlement_add: "add",
    settlement_last: "the last one cannot be removed - everything is priced against it",
    valuation_two_sided: "both sides",
    valuation_one_sided: "one side",
    suggestion_label: "often flipped",
    adopt_label: "follow",
    ignore_label: "ignore",
    hide_label: "hide",
    hidden_count: "{} hidden",
    unhide_all: "show all",
    per_unit: "\u{2248} {} {} each",
    route_through_targets: "route through targets",
    toggle_on: "on",
    toggle_off: "off",
    coverage_label: "pairs complete",
    coverage_header: "coverage & gaps",
    history_pair: "pair",
    history_latest: "latest",
    history_taker: "taker",
    history_maker: "maker",
    history_band: "low · mid · high",
    history_range: "range",
    history_spread: "maker over taker",
    history_points: "points / snapshots",
    history_chart: "five-minute candles",
    history_too_short: "not enough history to draw yet",
    aggregate_row: "and worse",
    tuning_header: "market tuning",
    tuning_fresh: "green up to (s)",
    tuning_usable: "amber up to (s)",
    tuning_stale: "red beyond (s)",
    tuning_skew: "cross-leg window (s)",
    tuning_sizes: "convert sizes",
    tuning_max_hops: "max hops",
    tuning_stake: "radar stake",
    tuning_results: "max results",
    tuning_min_bps: "min profit (bp)",
    tuning_expansions: "scan budget",
    tuning_thin: "thin stock below",
    tuning_outlier: "outlier factor",
    tuning_window: "report window (h)",
    tuning_apply: "apply",
    tuning_saved: "market tuning saved",
    tuning_invalid: "some values are not usable — nothing written",
    load_screenshot: "Load screenshot",
    zoom_in: "Zoom in",
    zoom_out: "Zoom out",
    fit_window: "Fit",
    actual_size: "100%",
    apply_regions: "Apply regions",
    drag_to_draw: "drag on the screenshot to draw the highlighted region",
    no_screenshot: "load a screenshot of the exchange panel to begin",
    hud_accepted_count: "{} accepted",
    skip_decode: "could not decode the frame",
    skip_ocr: "OCR unavailable",
    skip_need_name: "could not read the left currency",
    skip_have_name: "could not read the right currency",
    skip_empty_book: "the panel had no rows",
    skip_rows: "could not slice the rows",
    skip_out_of_order: "a rate contradicted the row above it",
    skip_confirmation: "two reads disagreed",
    skip_duplicate: "same book as last time",
    preset_applied: "factory regions for 2560x1440 applied",
    applied: "regions written to settings:",
    nothing_to_apply: "already applied - nothing changed",
    guide_hint: "amber box = where this region usually sits",
    hint_need: "the currency name on the left, icon excluded",
    hint_have: "the currency name on the right, icon and star excluded",
    hint_tables: "from the Available Trades title bar down past the last competing row",

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

    radar_column_kind: "kind",
    radar_column_route: "route",
    radar_column_edge: "edge",
    radar_column_out: "out",
    radar_column_depth: "liquidity",
    radar_column_verdict: "verdict",
    radar_column_light: "data",
    radar_column_risks: "risks",
};

pub static SIMPLIFIED_CHINESE: Text = Text {
    app_title: "POE 交易追踪器",
    state_idle: "空闲",
    state_watching: "监视中",
    state_fault: "故障",
    accepted_label: "已接受",
    skips_label: "已跳过",
    start_watch: "开始监视",
    stop_watch: "停止",

    page_monitor: "监视器",
    page_opportunities: "雷达",
    page_convert: "兑换",
    page_watchlist: "关注列表",
    page_history: "历史",

    panel_last_book: "最近盘口",
    panel_opportunities: "机会",
    panel_skips: "跳过原因",
    panel_probe_queue: "待采集队列",
    book_rows: "{} 行",
    panel_settings: "设置",
    refresh: "刷新",
    use_preset: "套用预设 2560x1440",
    page_calibrate: "校准",
    page_settings: "设置",
    pinned_label: "已排队",
    pin_label: "排队",
    unpin_label: "移除",
    detail_header: "路线明细",
    detail_route: "路径",
    detail_stake: "投入",
    detail_return: "得到",
    detail_leg: "第",
    detail_capture: "抓取",
    detail_risks: "风险",
    detail_reasons: "依据",
    convert_have_label: "拥有",
    convert_need_label: "想要",
    convert_holdings_label: "持仓",
    convert_pick: "选择通货",
    convert_size_down: "建议减量",
    convert_stranded: "零头",
    maker_header: "挂单策略",
    maker_instant_label: "立即成交价",
    maker_spread_label: "队首价差",
    maker_depth_label: "可见深度",
    role_target: "目标",
    role_bridge: "桥梁",
    role_watch_only: "仅观察",
    role_unlisted: "不关注",
    settlement_label: "结算通货",
    settlement_hint: "利润以它计价，套利从它出发并回到它 — 在设置页更改",
    roles_legend: concat!(
        "目标  ·  即使最近没翻到，也持续盯它缺哪些数据
",
        "桥梁  ·  只当中转，不作为终点
",
        "仅观察  ·  只看价格，永不路过
",
        "不关注  ·  不盯它 — 但抓到过的照样算套利",
    ),
    settlement_add: "添加",
    settlement_last: "最后一种不能移除 — 所有价格都以它为基准",
    valuation_two_sided: "两侧",
    valuation_one_sided: "单侧",
    suggestion_label: "常翻",
    adopt_label: "加入关注",
    ignore_label: "忽略",
    hide_label: "隐藏",
    hidden_count: "已隐藏 {} 项",
    unhide_all: "全部恢复",
    per_unit: "每个 \u{2248} {} {}",
    route_through_targets: "允许路过关注目标",
    toggle_on: "开",
    toggle_off: "关",
    coverage_label: "已齐全",
    coverage_header: "覆盖与缺口",
    history_pair: "通货对",
    history_latest: "最新",
    history_taker: "吃单",
    history_maker: "挂单",
    history_band: "低 · 中位 · 高",
    history_range: "波动幅度",
    history_spread: "挂单高于吃单",
    history_points: "价格点 / 盘口",
    history_chart: "五分钟蜡烛图",
    history_too_short: "历史还不够，画不出图",
    aggregate_row: "及更差",
    tuning_header: "算法参数",
    tuning_fresh: "绿灯上限（秒）",
    tuning_usable: "黄灯上限（秒）",
    tuning_stale: "红灯起点（秒）",
    tuning_skew: "跨腿时间窗（秒）",
    tuning_sizes: "兑换规模",
    tuning_max_hops: "最大跳数",
    tuning_stake: "雷达投入",
    tuning_results: "结果条数",
    tuning_min_bps: "最低收益（bp）",
    tuning_expansions: "扫描预算",
    tuning_thin: "薄库存阈值",
    tuning_outlier: "离群倍数",
    tuning_window: "报表窗口（小时）",
    tuning_apply: "应用",
    tuning_saved: "算法参数已保存",
    tuning_invalid: "有数值不可用 — 未写入",
    load_screenshot: "载入截图",
    zoom_in: "放大",
    zoom_out: "缩小",
    fit_window: "适应窗口",
    actual_size: "原图 100%",
    apply_regions: "套用区域",
    drag_to_draw: "在截图上拖动，框出高亮的那个区域",
    no_screenshot: "先载入一张兑换面板的截图",
    hud_accepted_count: "已接受 {}",
    skip_decode: "画面解码失败",
    skip_ocr: "OCR 不可用",
    skip_need_name: "左侧通货名没读出来",
    skip_have_name: "右侧通货名没读出来",
    skip_empty_book: "面板里没有行",
    skip_rows: "行切分失败",
    skip_out_of_order: "有一行的比率和上一行矛盾",
    skip_confirmation: "两次读取不一致",
    skip_duplicate: "和上一帧是同一个盘口",
    preset_applied: "已套用 2560x1440 的预设区域",
    applied: "已写入设置的区域：",
    nothing_to_apply: "与当前设置相同，没有变更",
    guide_hint: "琥珀色框 = 这个区域通常的位置",
    hint_need: "左侧的通货名称，不要框进图标",
    hint_have: "右侧的通货名称，不要框进图标和星标",
    hint_tables: "从「可用交易」标题栏开始，一直框到「竞争交易」最后一行下方",

    slot_need: "我需要的",
    slot_have: "我拥有的",
    slot_tables: "交易表格",

    waiting_for_book: "还没有抓到盘口 — 开始监视，并在游戏里打开一组通货",
    no_pair_yet: "通货对：还没抓到",
    pair_prefix: "通货对",
    nothing_yet: "—",

    hotkey_ready: "热键已注册",
    hotkey_unavailable: "热键被其他程序占用",
    fault_prefix: "故障",
    language_label: "界面语言",
    game_label: "游戏",
    client_language_label: "游戏语言",
    restart_watch_to_apply: "重新开始监视后生效",

    radar_column_kind: "种类",
    radar_column_route: "路径",
    radar_column_edge: "收益",
    radar_column_out: "得到",
    radar_column_depth: "流动性",
    radar_column_verdict: "可执行性",
    radar_column_light: "数据",
    radar_column_risks: "风险",
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The Chinese catalogue must actually be Simplified.
    ///
    /// It was written in Traditional first, to match the only Chinese client
    /// the game ships, and converting it is exactly the kind of edit that gets
    /// done nine tenths of the way. A missed character does not fail anywhere
    /// — it renders, it is even readable — so nothing but a check like this
    /// would notice.
    ///
    /// A spot-check, not a converter: these are the Traditional forms whose
    /// Simplified counterparts differ *and* that belong to this catalogue's
    /// own vocabulary, so a partial conversion trips at least one of them.
    #[test]
    fn the_chinese_catalogue_is_simplified() {
        const TRADITIONAL_ONLY: [char; 70] = [
            '訊', '設', '監', '視', '準', '載', '圖', '區', '應', '過', '閒', '開', '關', '註',
            '鍵', '熱', '擷', '盤', '貨', '對', '為', '個', '這', '們', '時', '實', '點', '選',
            '態', '樣', '進', '標', '語', '題', '錄', '檔', '稱', '顯', '讀', '數', '動', '現',
            '種', '說', '歷', '機', '會', '場', '達', '業', '產', '務', '覽', '隨', '沒', '麼',
            '變', '執', '錯', '誤', '頁', '單', '輸', '導', '換', '兌', '觀', '讓', '適', '復',
        ];
        for (field, value) in SIMPLIFIED_CHINESE.fields() {
            for character in TRADITIONAL_ONLY {
                assert!(
                    !value.contains(character),
                    "{field} still reads {value:?}, which holds the traditional {character}"
                );
            }
        }
    }

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
                ("page_opportunities", text.page_opportunities),
                ("page_convert", text.page_convert),
                ("page_watchlist", text.page_watchlist),
                ("page_history", text.page_history),
                ("panel_last_book", text.panel_last_book),
                ("panel_opportunities", text.panel_opportunities),
                ("panel_skips", text.panel_skips),
                ("panel_probe_queue", text.panel_probe_queue),
                ("book_rows", text.book_rows),
                ("panel_settings", text.panel_settings),
                ("refresh", text.refresh),
                ("use_preset", text.use_preset),
                ("page_calibrate", text.page_calibrate),
                ("page_settings", text.page_settings),
                ("pinned_label", text.pinned_label),
                ("pin_label", text.pin_label),
                ("unpin_label", text.unpin_label),
                ("detail_header", text.detail_header),
                ("detail_route", text.detail_route),
                ("detail_stake", text.detail_stake),
                ("detail_return", text.detail_return),
                ("detail_leg", text.detail_leg),
                ("detail_capture", text.detail_capture),
                ("detail_risks", text.detail_risks),
                ("detail_reasons", text.detail_reasons),
                ("convert_have_label", text.convert_have_label),
                ("convert_need_label", text.convert_need_label),
                ("convert_holdings_label", text.convert_holdings_label),
                ("convert_pick", text.convert_pick),
                ("convert_size_down", text.convert_size_down),
                ("convert_stranded", text.convert_stranded),
                ("maker_header", text.maker_header),
                ("maker_instant_label", text.maker_instant_label),
                ("maker_spread_label", text.maker_spread_label),
                ("maker_depth_label", text.maker_depth_label),
                ("role_target", text.role_target),
                ("role_bridge", text.role_bridge),
                ("role_watch_only", text.role_watch_only),
                ("role_unlisted", text.role_unlisted),
                ("settlement_label", text.settlement_label),
                ("settlement_hint", text.settlement_hint),
                ("roles_legend", text.roles_legend),
                ("valuation_two_sided", text.valuation_two_sided),
                ("valuation_one_sided", text.valuation_one_sided),
                ("suggestion_label", text.suggestion_label),
                ("adopt_label", text.adopt_label),
                ("ignore_label", text.ignore_label),
                ("hide_label", text.hide_label),
                ("hidden_count", text.hidden_count),
                ("unhide_all", text.unhide_all),
                ("per_unit", text.per_unit),
                ("route_through_targets", text.route_through_targets),
                ("toggle_on", text.toggle_on),
                ("toggle_off", text.toggle_off),
                ("coverage_label", text.coverage_label),
                ("coverage_header", text.coverage_header),
                ("history_pair", text.history_pair),
                ("history_latest", text.history_latest),
                ("history_taker", text.history_taker),
                ("history_maker", text.history_maker),
                ("history_band", text.history_band),
                ("history_range", text.history_range),
                ("history_spread", text.history_spread),
                ("history_points", text.history_points),
                ("history_chart", text.history_chart),
                ("history_too_short", text.history_too_short),
                ("aggregate_row", text.aggregate_row),
                ("tuning_header", text.tuning_header),
                ("tuning_fresh", text.tuning_fresh),
                ("tuning_usable", text.tuning_usable),
                ("tuning_stale", text.tuning_stale),
                ("tuning_skew", text.tuning_skew),
                ("tuning_sizes", text.tuning_sizes),
                ("tuning_max_hops", text.tuning_max_hops),
                ("tuning_stake", text.tuning_stake),
                ("tuning_results", text.tuning_results),
                ("tuning_min_bps", text.tuning_min_bps),
                ("tuning_expansions", text.tuning_expansions),
                ("tuning_thin", text.tuning_thin),
                ("tuning_outlier", text.tuning_outlier),
                ("tuning_window", text.tuning_window),
                ("tuning_apply", text.tuning_apply),
                ("tuning_saved", text.tuning_saved),
                ("tuning_invalid", text.tuning_invalid),
                ("load_screenshot", text.load_screenshot),
                ("zoom_in", text.zoom_in),
                ("zoom_out", text.zoom_out),
                ("fit_window", text.fit_window),
                ("actual_size", text.actual_size),
                ("apply_regions", text.apply_regions),
                ("drag_to_draw", text.drag_to_draw),
                ("no_screenshot", text.no_screenshot),
                ("preset_applied", text.preset_applied),
                ("applied", text.applied),
                ("nothing_to_apply", text.nothing_to_apply),
                ("guide_hint", text.guide_hint),
                ("hint_need", text.hint_need),
                ("hint_have", text.hint_have),
                ("hint_tables", text.hint_tables),
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
