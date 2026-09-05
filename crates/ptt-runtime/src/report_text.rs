//! Names for the typed values the reports print, in both interface languages.
//!
//! The reports used to render these with `{:?}`, which puts a Rust identifier
//! on screen — `UnverifiedProductPolicy`, `MissingMakerReferenceQuote` — and
//! asks the reader to decode it. That is bad in English and useless in
//! Chinese, and it is the part of the output a person most needs to
//! understand: every one of these is the program explaining why it will not
//! promise something.
//!
//! Each function matches exhaustively over a foreign enum, so a variant added
//! anywhere in the domain crates fails to compile here until it is named in
//! both languages. That is the whole reason the matches live in this crate
//! rather than beside their enums: the domain layers stay free of interface
//! text, and the compiler still refuses to let a new flag reach a user as a
//! bare identifier.
//!
//! The English is deliberately lowercase prose rather than the identifier —
//! it sits inside sentences the reports build, and `risks unknown fee` reads
//! where `risks UnknownFee` does not.

use ptt_market_book::FreshnessStatus;
use ptt_settings::UiLanguage;

use ptt_strategy::{
    Actionability, AnchorAction, AnomalySeverity, ExecutionRisk, MakerQueueExclusion,
    PriceAnomalyKind,
};
use ptt_trade_engine::ExecutionRiskFlag;
use ptt_workflows::{FocusCoverageStatus, ProbePriority, ProbeReason, RadarItemKind, RadarReason};

/// Prose the reports assemble, in both interface languages.
///
/// Whole templates rather than fragments a caller concatenates. Word order
/// is not shared between these two languages -- `staking 10 divine-orb across
/// 3 targets` puts its count where the Chinese sentence does not -- so a
/// report built by gluing translated pieces together produces a sentence that
/// is grammatical in exactly one of them.
pub struct ReportText {
    pub better_than_direct: &'static str,
    pub worse_than_direct: &'static str,
    pub level_with_direct: &'static str,
    pub no_direct_route: &'static str,
    /// Slots: league, coverage percent.
    pub exchange_header: &'static str,
    /// Slot: market median drift in percent.
    pub exchange_drift: &'static str,
    pub exchange_no_data: &'static str,
    pub exchange_reconcile: &'static str,
    pub exchange_reconcile_none: &'static str,
    pub exchange_reconcile_pair: &'static str,
    pub exchange_radar_header: &'static str,
    pub exchange_radar_caveat: &'static str,
    pub exchange_radar_no_data: &'static str,
    pub exchange_radar_no_start: &'static str,
    pub exchange_radar_diagnostics: &'static str,
    pub exchange_radar_budget_exhausted: &'static str,
    pub exchange_radar_no_loop: &'static str,
    /// Hourly ledger series (probe `--series`). Header slots: asset, hours in
    /// window, traded, missing, volume, mean per hour. Point slots: local hour,
    /// value, volume. Peak slots: start hour, end hour.
    pub exchange_series_header: &'static str,
    pub exchange_series_point: &'static str,
    pub exchange_series_peak: &'static str,
    /// One leg of a route against the listings it would have to take right
    /// now. Slots: from, to, listed, taken, share.
    ///
    /// Worded as taking, never as waiting. Whether an order the reader
    /// *lists* ever fills is not a depth question in this exchange — a
    /// listing is taken at its own rate or a better one or not at all — and
    /// a line that let itself be read that way would be answering a question
    /// it never looked at.
    pub leg_take: &'static str,
    /// The share chunk `leg_take`'s last slot receives when there is one.
    pub leg_share: &'static str,
    pub leg_covered: &'static str,
    pub leg_not_enough_listed: &'static str,
    pub leg_no_listings: &'static str,
    pub leg_bound_by_next: &'static str,
    pub leg_single_listing: &'static str,
    /// How many routes pinch at the one step this row stands for.
    pub pinch_shared: &'static str,
    pub nothing_to_convert: &'static str,
    /// Both pickers on one currency. Not a failure — a question with no
    /// content, which the page has to say plainly so the reader knows to
    /// change one of them rather than wonder what broke.
    pub same_currency: &'static str,
    /// The focus list names nothing the settlement set does not already
    /// cover, so there is nothing to measure coverage of.
    pub focus_has_no_targets: &'static str,
    /// What an empty radar actually did: conversions scanned, of them
    /// priced, of them unpriceable, triangles evaluated, profit floor.
    /// Without this line an empty list cannot be told apart from a scan
    /// that never ran.
    pub scan_accounting: &'static str,
    pub core_liquidity: &'static str,
    pub no_price_capture: &'static str,
    pub coverage_unavailable: &'static str,
    pub coverage_progress: &'static str,
    pub pairs_complete: &'static str,
    pub no_core_currency: &'static str,
    pub not_enough_market: &'static str,
    pub no_start_units: &'static str,
    pub scanning_from: &'static str,
    pub partial_scan: &'static str,
    pub results_cut: &'static str,
    pub nothing_beats_holding: &'static str,
    pub unpriced: &'static str,
    pub no_pairs_captured: &'static str,
    pub no_data_in_window: &'static str,
    pub nothing_to_probe: &'static str,
    pub no_history_yet: &'static str,
    pub median_low_high: &'static str,
    pub maker_over_taker: &'static str,
    pub listings_note: &'static str,
    pub nothing_current: &'static str,
    pub radar_probe_header: &'static str,
    pub focus_suggestion: &'static str,
    pub freshness_config_invalid: &'static str,
    pub freshness_light_line: &'static str,
    pub settlement_config_invalid: &'static str,
    pub settlement_config_partial: &'static str,
    pub maker_header: &'static str,
    pub maker_instant: &'static str,
    pub maker_no_instant: &'static str,
    pub maker_no_book: &'static str,
    pub maker_undercut: &'static str,
    pub maker_match: &'static str,
    pub maker_greedy: &'static str,
    pub maker_improvement: &'static str,
    pub maker_not_worth: &'static str,
    pub maker_spread: &'static str,
    pub maker_depth: &'static str,
    /// The size ceiling, as its own phrase: the GPUI panel already has a
    /// "visible depth" label on the row, so it needs this half alone.
    pub maker_max_single: &'static str,
    pub maker_excluded: &'static str,
    /// Heading for the hazards that hold whichever way you price in a book.
    pub maker_risks: &'static str,
    pub no_route_for_pair: &'static str,
    pub route_direct_label: &'static str,
    pub route_via: &'static str,
    pub route_baseline: &'static str,
    pub route_front_depth: &'static str,
    pub route_front_short: &'static str,
    pub route_no_front_price: &'static str,
    pub no_route_beats_direct: &'static str,
    pub routes_hidden_worse_than_direct: &'static str,
    pub valuation_two_sided: &'static str,
    pub valuation_one_sided: &'static str,
    pub anchor_recommendation: &'static str,
    pub targets_without_route: &'static str,
    pub history_header: &'static str,
    pub candle_line: &'static str,
    pub risks: &'static str,
    pub probe: &'static str,
    pub flip: &'static str,
    pub analytics_config_invalid: &'static str,
    pub analytics_no_data: &'static str,
    pub analytics_season_line: &'static str,
    pub analytics_as_of: &'static str,
    pub analytics_anchor_line: &'static str,
    pub analytics_breadth_line: &'static str,
    pub analytics_cross_line: &'static str,
    pub analytics_table_header: &'static str,
    pub analytics_marker_high_turnover: &'static str,
    pub analytics_marker_greedy: &'static str,
    /// The live pipeline's identity flag. Slots: from asset, to asset, this
    /// book's best taker rate, the recent daily median it was compared to.
    ///
    /// The sentence has to end by saying the book was kept, because the flag
    /// reports and never rejects — a warning that reads like a rejection
    /// would send the user looking for a book that is already there.
    pub identity_magnitude_suspect: &'static str,
}

#[must_use]
pub const fn report(language: UiLanguage) -> &'static ReportText {
    match language {
        UiLanguage::English => &REPORT_ENGLISH,
        UiLanguage::Chinese => &REPORT_CHINESE,
    }
}

pub static REPORT_ENGLISH: ReportText = ReportText {
    better_than_direct: "+{} ({} better than direct)",
    worse_than_direct: "-{} ({} worse than direct)",
    level_with_direct: "level with direct",
    no_direct_route: "no direct route to compare",
    exchange_header: "official exchange, league {} (mapping coverage {}%)",
    exchange_drift: "market median drift {}",
    exchange_no_data: "no exchange data yet -- set the league in Settings, the sync fills in on its own",
    exchange_reconcile: "panel check, last {} days: top-of-book {}/{} inside the official traded range ({} unmatched)",
    exchange_reconcile_none: "panel check, last {} days: no panel captures to compare",
    exchange_reconcile_pair: "  {} -> {}: {}/{} outside, better {} / worse {}, typical {}, {}",
    exchange_radar_header: "exchange radar · {} · data hour {} ({} h behind) · {} assets · {} markets · floor {}",
    exchange_radar_caveat: "loops priced at the official hourly averages, ignoring listed stock - a lead for what to capture, not a promise you can execute",
    exchange_radar_no_data: "no exchange market traded in the last {} h - sync first",
    exchange_radar_no_start: "no settlement currency ({} or the others) traded in the last {} h",
    exchange_radar_diagnostics: "loops evaluated {} · above the floor {}",
    exchange_radar_budget_exhausted: "loop budget exhausted - the list may be missing loops",
    exchange_radar_no_loop: "the hourly averages agree with each other: no loop above the floor",
    exchange_series_header: "{} · {} hours in window, {} traded, {} missing · volume {} · mean/h {}",
    exchange_series_point: "  {}  value {}  volume {}",
    exchange_series_peak: "peak hours {}:00-{}:00 (local)",
    leg_take: "{} -> {}   {} listed, this trip takes {}{}",
    leg_share: " ({}%)",
    leg_covered: "listings cover it",
    leg_not_enough_listed: "more than everything listed - one pass cannot fill it",
    leg_no_listings: "nothing listed this way - no data, not a shortage",
    leg_bound_by_next: "the next leg is the tighter one",
    leg_single_listing: "one listing only",
    pinch_shared: "all {} routes pinch at this step",
    route_direct_label: "direct",
    route_via: "via {}",
    route_baseline: "baseline",
    route_front_depth: "the front rows take {} {} at this rate",
    route_front_short: "your ask is larger than that",
    route_no_front_price: "no front price on one of the legs - rate not claimed",
    no_route_beats_direct: "no route beats going direct - direct is the best rate on this book",
    routes_hidden_worse_than_direct: "{} route(s) hidden - priced worse than going direct",
    nothing_to_convert: "nothing to convert yet - capture a book first",
    same_currency: "have and want are the same currency - pick two different ones",
    focus_has_no_targets: "the focus list adds nothing to the settlement set - only the settlement currencies are being compared",
    scan_accounting: "ran {} conversion searches ({} priced, {} too small to trade, {} unpriceable) and {} loops, of which {} pay - profit floor {}",
    core_liquidity: "core liquidity: {}",
    no_price_capture: "no price - capture this pair",
    coverage_unavailable: "coverage unavailable: {}",
    coverage_progress: "coverage: {} of {} pairs complete",
    pairs_complete: "{} of {} pairs complete",
    no_core_currency: "no core currency configured for this league",
    not_enough_market: "not enough of the market captured yet - flip a few pairs first",
    no_start_units: "no captures touch a settlement currency yet - flip a {} panel once",
    scanning_from: "scanning from {} across {} targets",
    partial_scan: "partial scan - {} targets skipped, {} expansions used",
    results_cut: "{} routes cleared the floor; showing the top {}",
    nothing_beats_holding: "nothing beats holding right now",
    unpriced: "unpriced",
    no_pairs_captured: "no pairs captured yet",
    no_data_in_window: "no captures inside the report window - capture once, or widen the report window in settings",
    nothing_to_probe: "nothing to probe - the book is current",
    no_history_yet: "no history yet for {} -> {}",
    median_low_high: "median {}   low {}   high {}",
    maker_over_taker: "maker over taker: {}",
    listings_note: "  (listings)",
    nothing_current: "nothing current - this is history, not a price",
    radar_probe_header: "to firm these up, go flip:",
    focus_suggestion: "consider adding {} to focus - buy pressure {} vs listed {} (anchor units)",
    freshness_config_invalid: "freshness thresholds in settings are invalid - using defaults",
    freshness_light_line: "data freshness: {}",
    settlement_config_invalid: "settlement currencies in settings are invalid - using defaults",
    settlement_config_partial: "{} settlement entries in settings were ignored (invalid ids)",
    maker_header: "listing strategy {} -> {} at size {}",
    maker_instant: "take now at {}",
    maker_no_instant: "no instant fill - the taker side is empty",
    maker_no_book: "no competing listings - probe this pair first",
    maker_undercut: "undercut, list at {}",
    maker_match: "match the front, list at {} - queues behind it",
    maker_greedy: "greedy, list at {} - bets on drift",
    maker_improvement: "+{} {} vs taking ({})",
    maker_not_worth: "no better than taking - trade instead",
    maker_spread: "front over instant: {}",
    maker_depth: "visible depth {} {}",
    maker_max_single: "at most {} {} in one order",
    maker_excluded: "excluded listing at {} (stock {}): {}",
    maker_risks: "risks on this pair",
    no_route_for_pair: "{} -> {}: no route yet",
    valuation_two_sided: "{} (both sides)",
    valuation_one_sided: "{} (one side only)",
    anchor_recommendation: "{}: {} (score {}, {} pairs, {} two-way)",
    targets_without_route: "{} targets have no route yet - the Watchlist says which to flip",
    history_header: "{} -> {}: {} points over {} snapshots",
    candle_line: "{}  o {} h {} l {} c {}  n={}{}",
    risks: "risks",
    probe: "probe",
    flip: "flip",
    analytics_config_invalid: "analytics thresholds in settings are invalid - using defaults",
    analytics_no_data: "no market history yet - capture some books first",
    analytics_season_line: "season {} - since {}",
    analytics_as_of: "data through {} - {} day(s)",
    analytics_anchor_line: "anchor {}: {}",
    analytics_breadth_line: "breadth: {} up / {} down / {} flat - market median {}",
    analytics_cross_line: "cross {}: {} per unit ({})",
    analytics_table_header: "asset | value | supply | demand | class | trend",
    analytics_marker_high_turnover: "high-turnover",
    analytics_marker_greedy: "greedy-fit",
    identity_magnitude_suspect: "{} -> {}: this book's best rate {} is nowhere near the recent daily median {} - check the currency names, the book was stored anyway",
};

pub static REPORT_CHINESE: ReportText = ReportText {
    better_than_direct: "+{}（比直兑高 {}）",
    worse_than_direct: "-{}（比直兑低 {}）",
    level_with_direct: "与直兑持平",
    no_direct_route: "没有直兑路线可比",
    exchange_header: "官方交易所，联赛 {}（映射覆盖 {}%）",
    exchange_drift: "市场中位漂移 {}",
    exchange_no_data: "还没有交易所数据——去设置页填联赛名，同步会自己补齐",
    exchange_reconcile: "面板核对，近 {} 天：最优档 {}/{} 次落在官方成交区间内（{} 次没对上）",
    exchange_reconcile_none: "面板核对，近 {} 天：没有面板抓取可比",
    exchange_reconcile_pair: "  {} -> {}：{}/{} 越界，更好 {} / 更差 {}，典型偏离 {}，{}",
    exchange_radar_header: "交易所雷达 · {} · 数据小时 {}（落后 {} 小时）· 资产 {} · 市场 {} · 门槛 {}",
    exchange_radar_caveat: "按官方小时成交均价算的环，不看挂单库存——是去抓的线索，不是可执行的承诺",
    exchange_radar_no_data: "近 {} 小时没有任何交易所市场成交——先同步",
    exchange_radar_no_start: "近 {} 小时里没有结算通货（{} 或其余）成交",
    exchange_radar_diagnostics: "评估环 {} 个 · 过门槛 {} 个",
    exchange_radar_budget_exhausted: "环路预算耗尽——名单可能有漏",
    exchange_radar_no_loop: "各小时均价彼此自洽：没有过门槛的环",
    exchange_series_header: "{} · 窗口 {} 小时，有成交 {}，缺 {} · 成交额 {} · 每小时 {}",
    exchange_series_point: "  {}  价 {}  成交 {}",
    exchange_series_peak: "峰值时段 {}:00–{}:00（本地）",
    leg_take: "{} -> {}   市面挂着 {}，这一趟要吃掉 {}{}",
    leg_share: "（{}%）",
    leg_covered: "现有挂单够吃",
    leg_not_enough_listed: "比现有挂单还多 — 一次吃不完",
    leg_no_listings: "这个方向没抓到挂单 — 是没数据，不是不够",
    leg_bound_by_next: "下一步更紧",
    leg_single_listing: "只有一个盘口",
    pinch_shared: "{} 条路线都卡在这一步",
    route_direct_label: "直兑",
    route_via: "经 {}",
    route_baseline: "基准",
    route_front_depth: "这个汇率上市面能吃下 {} {}",
    route_front_short: "少于你要推的量",
    route_no_front_price: "其中一步没有首档报价 — 不给汇率结论",
    no_route_beats_direct: "没有比直兑更好的路线 — 直兑就是这本书上最优的汇率",
    routes_hidden_worse_than_direct: "已隐藏 {} 条路线 — 汇率不如直兑",
    nothing_to_convert: "还没有可兑换的数据 — 先抓一个盘口",
    same_currency: "拥有和想要是同一种通货 — 请选两种不同的",
    focus_has_no_targets: "关注列表没有在结算通货之外添加任何东西 — 现在只在结算通货之间比对",
    scan_accounting: "跑了 {} 次兑换搜索（{} 条可定价，{} 条投入买不起，{} 条缺价）、{} 个闭环（其中 {} 个有得赚）— 收益门槛 {}",
    core_liquidity: "核心流通币：{}",
    no_price_capture: "没有价格 — 去翻这一对",
    coverage_unavailable: "覆盖情况读不出来：{}",
    coverage_progress: "覆盖：{} / {} 对已齐全",
    pairs_complete: "{} / {} 对已齐全",
    no_core_currency: "这个赛季没有配置核心流通币",
    not_enough_market: "抓到的市场数据还不够 — 先去翻几对",
    no_start_units: "还没有任何抓取涉及结算通货 — 去翻一次 {} 的面板",
    scanning_from: "从 {} 出发，尝试 {} 个目标",
    partial_scan: "扫描不完整 — 跳过 {} 个目标，用掉 {} 次展开",
    results_cut: "有 {} 条过了收益门槛，这里显示前 {} 条",
    nothing_beats_holding: "目前没有比继续持有更好的",
    unpriced: "无法定价",
    no_pairs_captured: "还没有抓到任何通货对",
    no_data_in_window: "报表窗口里没有数据 — 去抓一次，或在设置里调大报表窗口",
    nothing_to_probe: "没有要补的 — 盘口是最新的",
    no_history_yet: "{} → {} 还没有历史数据",
    median_low_high: "中位 {}   最低 {}   最高 {}",
    maker_over_taker: "挂单高于吃单：{}",
    listings_note: "  （挂单）",
    nothing_current: "没有当前报价 — 这是历史，不是价格",
    radar_probe_header: "要坐实这些机会，去翻：",
    focus_suggestion: "建议把 {} 加入关注 — 买压 {} 对 在售 {}（锚单位）",
    freshness_config_invalid: "设置里的新鲜度阈值无效 — 已回落到默认",
    freshness_light_line: "数据新鲜度：{}",
    settlement_config_invalid: "设置里的结算通货无效 — 已回落到默认",
    settlement_config_partial: "设置里有 {} 条结算通货无效，已忽略",
    maker_header: "挂单策略 {} → {}（规模 {}）",
    maker_instant: "立即成交价 {}",
    maker_no_instant: "无法立即成交 — 可用侧没有挂单",
    maker_no_book: "竞争侧没有挂单 — 先补采集这一对",
    maker_undercut: "机会（压一档），挂 {}",
    maker_match: "跟价，挂 {} — 排在原单之后",
    maker_greedy: "贪婪，挂 {} — 赌行情走势",
    maker_improvement: "比立即成交多 {} {}（{}）",
    maker_not_worth: "不比立即成交好 — 不如直接吃单",
    maker_spread: "队首高出立即成交 {}",
    maker_depth: "可见深度 {} {}",
    maker_max_single: "单笔建议不超过 {} {}",
    maker_excluded: "已排除挂单 {}（库存 {}）：{}",
    maker_risks: "这一对通货的风险",
    no_route_for_pair: "{} → {}：还没有路线",
    valuation_two_sided: "{}（两侧都有）",
    valuation_one_sided: "{}（只有一侧）",
    anchor_recommendation: "{}：{}（评分 {}，覆盖 {} 对，其中 {} 对双向）",
    targets_without_route: "{} 个目标还没有路线 — 关注页会说先翻哪一对",
    history_header: "{} → {}：{} 个价格点，来自 {} 个盘口",
    candle_line: "{}  开 {} 高 {} 低 {} 收 {}  样本 {}{}",
    risks: "风险",
    probe: "建议采集",
    flip: "翻",
    analytics_config_invalid: "设置里的分析阈值无效 — 已回落到默认",
    analytics_no_data: "还没有市场历史 — 先抓一些盘口",
    analytics_season_line: "赛季 {} — {} 起",
    analytics_as_of: "数据截至 {} — 共 {} 天",
    analytics_anchor_line: "锚定通货 {}：{}",
    analytics_breadth_line: "广度：{} 涨 / {} 跌 / {} 平 — 市场中位 {}",
    analytics_cross_line: "锚交叉 {}：每单位 {}（{}）",
    analytics_table_header: "通货 | 价值 | 供给 | 需求 | 分类 | 趋势",
    analytics_marker_high_turnover: "高流转",
    analytics_marker_greedy: "适合贪婪",
    identity_magnitude_suspect: "{} -> {}：这本书最好的一档 {} 和最近的日中位 {} 差了一个量级——书照常落库了，但先核对一下通货名字",
};

/// Fills a template's `{}` slots, in order.
///
/// `format!` needs a literal and these templates are chosen at run time, so
/// the substitution has to be done by hand. What that gives up is the
/// compiler counting the arguments -- so a test counts the slots instead, and
/// a translation that lost one fails there rather than rendering a sentence
/// with a hole in it.
#[must_use]
pub fn fill(template: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut values = values.iter();
    let mut rest = template;
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        // A slot with nothing to put in it keeps its braces, so the gap
        // shows on screen instead of silently closing up.
        out.push_str(values.next().map_or("{}", |value| value));
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// Basis points as a two-decimal percentage — the one unit these numbers
/// reach the reader in.
///
/// Integer arithmetic on purpose. A hundred basis points is exactly one
/// percent, so the value splits into a whole-percent part and a two-digit
/// remainder with nothing left over to round: no float, no lost precision,
/// and the printed number is the stored number.
///
/// The sign is written separately rather than left to the division, which is
/// the part that is easy to get wrong. Anything smaller than a percent
/// divides to a whole part of zero, and zero carries no sign — so -1 bp
/// would print as `0.01%`, the right size pointing the wrong way.
/// A large amount at table width, in the reader's own convention: 万 / 亿 in
/// Chinese, k / M in English. Below the first step the number is printed as
/// it is — 18531 still reads. The exact value always sits in the detail
/// panel; this is for scanning a column, not for copying a figure.
///
/// One rule for every page: the analytics page used to write 1214万 (in the
/// English interface too) while the exchange page wrote 125k one tab over.
#[must_use]
pub fn compact_units(value: u128, language: UiLanguage) -> String {
    #[allow(clippy::cast_precision_loss)]
    let float = value as f64;
    match language {
        UiLanguage::Chinese => {
            if value >= 100_000_000 {
                format!("{:.1}亿", float / 100_000_000.0)
            } else if value >= 100_000 {
                format!("{}万", value / 10_000)
            } else {
                value.to_string()
            }
        }
        UiLanguage::English => {
            if value >= 1_000_000 {
                format!("{:.1}M", float / 1_000_000.0)
            } else if value >= 10_000 {
                format!("{:.0}k", float / 1_000.0)
            } else {
                value.to_string()
            }
        }
    }
}

/// Past ±999% a percentage no longer fits one 28px cell, and on launch day a
/// card priced in a scarce anchor moves by tens of millions of percent. The
/// number is right; the cell is one line high. Capped here, once, for every
/// page and both formatters.
const PERCENT_CAP_BASIS_POINTS: i64 = 999 * 100;

#[must_use]
pub fn percent_from_basis_points(points: i64) -> String {
    if points > PERCENT_CAP_BASIS_POINTS {
        return ">999%".to_owned();
    }
    if points < -PERCENT_CAP_BASIS_POINTS {
        return "<-999%".to_owned();
    }
    let magnitude = points.unsigned_abs();
    format!(
        "{}{}.{:02}%",
        if points < 0 { "-" } else { "" },
        magnitude / 100,
        magnitude % 100
    )
}

/// The same percentage, written as a *move* rather than a level.
///
/// [`percent_from_basis_points`] only ever writes the minus, so a rise comes
/// out looking exactly like a standing value. These numbers are drifts and
/// trends, and a delta with no sign in front of it reads as a level — so the
/// plus is added here, once, for every renderer.
///
/// It lives beside the unsigned formatter instead of in a page, because the
/// Analytics page and `analytics_report_lines` print the same two numbers and
/// the text lines are the page's parity reference: the moment each side owns
/// its own sign rule, the same drift reads `+2.57%` in one place and `2.57%`
/// in the other, and the reader starts wondering whether they are the same
/// number at all.
#[must_use]
pub fn signed_percent_from_basis_points(points: i64) -> String {
    if points > PERCENT_CAP_BASIS_POINTS {
        return ">+999%".to_owned();
    }
    let percent = percent_from_basis_points(points);
    if points < 0 {
        percent
    } else {
        format!("+{percent}")
    }
}

/// One route leg's numbers: what is listed against it, how much of that this
/// trip would have to take right now, and the share that works out to.
///
/// The two callers hand in their own names for the currencies -- the page uses
/// the catalogue's display names, the text report uses the raw ids -- and
/// share everything after that. Written once because the whole point of the
/// signal is that the same leg reads the same on both.
///
/// Facts only; the verdict is [`leg_take_notes`], because the page shows it as
/// a coloured chip beside the numbers and would otherwise print it twice.
#[must_use]
pub fn leg_take_facts(
    language: UiLanguage,
    from: &str,
    to: &str,
    leg: &crate::reports::LegTakeCoverage,
) -> String {
    let text = report(language);
    // The share prints only while it is a share of something. Past everything
    // listed it repeats the verdict beside it ("more than everything listed"
    // already says 132%), and at a large ask the floored percent inflates
    // into numbers no reader can weigh -- the two amounts carry it alone.
    let share = match (leg.listed, leg.share_percent) {
        (Some(listed), Some(share)) if leg.taking <= listed => {
            fill(text.leg_share, &[&share.to_string()])
        }
        _ => String::new(),
    };
    fill(
        text.leg_take,
        &[
            from,
            to,
            &leg.listed
                .map_or_else(|| "-".to_owned(), |listed| listed.to_string()),
            &leg.taking.to_string(),
            &share,
        ],
    )
}

/// Where a route stands against the direct trade, in one phrase.
///
/// Written once for both renderers, for the same reason as
/// [`leg_take_facts`]: the page and the text report print the same
/// comparison, and the moment each owns its own wording they drift.
///
/// **The percentage goes in unsigned, and that is the whole point of this
/// function existing.** Both templates already say which way it went --
/// `比直兑低`, "worse than direct" -- while [`percent_from_basis_points`]
/// writes its own minus. Handing it the raw signed number printed
/// `比直兑低 -13.38%`, a double negative that reads as the opposite of the
/// truth, in the one place a reader is deciding whether a route is ahead.
#[must_use]
pub fn versus_direct(
    language: UiLanguage,
    direction: Option<ptt_trade_engine::ComparisonDirection>,
    delta_quanta: Option<u64>,
    basis_points: Option<i64>,
) -> String {
    use ptt_trade_engine::ComparisonDirection as Direction;
    let text = report(language);
    match (direction, delta_quanta, basis_points) {
        (Some(Direction::Improved), Some(delta), Some(points)) => fill(
            text.better_than_direct,
            &[&delta.to_string(), &percent_from_basis_points(points.abs())],
        ),
        (Some(Direction::Worse), Some(delta), Some(points)) => fill(
            text.worse_than_direct,
            &[&delta.to_string(), &percent_from_basis_points(points.abs())],
        ),
        (Some(Direction::Equal), _, _) => text.level_with_direct.to_owned(),
        // No direct route observed: showing the route is useful, calling it
        // an improvement over nothing is not.
        _ => text.no_direct_route.to_owned(),
    }
}

/// A candidate route's name on the Convert page.
///
/// `hops` is the middle of the path, already in the caller's own names for
/// the currencies — the page uses the catalogue's, the text report uses raw
/// ids. The endpoints are left out because the page is already about that
/// pair, and repeating them on every row costs the width the intermediate
/// currencies need.
#[must_use]
pub fn route_quote_label(language: UiLanguage, hops: &[String]) -> String {
    let text = report(language);
    if hops.is_empty() {
        return text.route_direct_label.to_owned();
    }
    let arrow = match language {
        UiLanguage::English => " -> ",
        UiLanguage::Chinese => " → ",
    };
    fill(text.route_via, &[&hops.join(arrow)])
}

/// What one route's front rows say about the size being asked for.
///
/// Always the number first. How much the market can absorb at this rate is a
/// fact the reader weighs themselves — the program has no idea whether they
/// are willing to leave the rest listed for an hour — so this states the
/// depth and, when the ask is larger, says plainly that the remainder waits.
/// It never withholds the route: see `reports::route_quotes`.
#[must_use]
pub fn route_depth_notes(
    language: UiLanguage,
    quote: &crate::reports::RouteQuote,
    size: u64,
    source: &str,
) -> Vec<String> {
    let text = report(language);
    let Some(fillable) = quote.fillable_input else {
        return Vec::new();
    };
    let mut notes = vec![fill(
        text.route_front_depth,
        &[&fillable.to_string(), source],
    )];
    if fillable < size {
        notes.push(text.route_front_short.to_owned());
    }
    notes
}

/// What qualifies one leg: the verdict first, then whatever the verdict does
/// not say on its own.
///
/// The one thing none of these may imply is a maker's question -- nothing here
/// knows whether an order the reader *lists* will find a taker.
#[must_use]
pub fn leg_take_notes(
    language: UiLanguage,
    leg: &crate::reports::LegTakeCoverage,
) -> Vec<&'static str> {
    let text = report(language);
    // Silence is the all-clear. A chip reading "the listings cover it" is a
    // reminder that nothing is wrong, printed once per step of every route
    // on the card; the two that do matter drown in them.
    let mut notes = Vec::new();
    if leg.verdict != crate::reports::LegTakeVerdict::Covered {
        notes.push(leg_take_verdict(language, leg.verdict));
    }
    if leg.bound_by_next_leg {
        notes.push(text.leg_bound_by_next);
    }
    if leg.single_listing {
        notes.push(text.leg_single_listing);
    }
    notes
}

/// Already-translated fragments, punctuated for the language.
///
/// [`join`] does the same for typed values it can name itself; this one is
/// for callers holding strings that are already text. Here rather than in a
/// page so no business file has to spell a full-width semicolon.
#[must_use]
pub fn join_text(language: UiLanguage, parts: &[&str]) -> String {
    let separator = match language {
        UiLanguage::English => "; ",
        UiLanguage::Chinese => "；",
    };
    parts.join(separator)
}

#[must_use]
pub fn leg_take_verdict(
    language: UiLanguage,
    verdict: crate::reports::LegTakeVerdict,
) -> &'static str {
    use crate::reports::LegTakeVerdict as Verdict;
    let text = report(language);
    match verdict {
        Verdict::NoListings => text.leg_no_listings,
        Verdict::Covered => text.leg_covered,
        Verdict::NotEnoughListed => text.leg_not_enough_listed,
    }
}

/// Every field of both catalogues, paired by name.
#[cfg(test)]
fn report_pairs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "scanning_from",
            REPORT_ENGLISH.scanning_from,
            REPORT_CHINESE.scanning_from,
        ),
        (
            "exchange_header",
            REPORT_ENGLISH.exchange_header,
            REPORT_CHINESE.exchange_header,
        ),
        (
            "exchange_drift",
            REPORT_ENGLISH.exchange_drift,
            REPORT_CHINESE.exchange_drift,
        ),
        (
            "exchange_no_data",
            REPORT_ENGLISH.exchange_no_data,
            REPORT_CHINESE.exchange_no_data,
        ),
        (
            "exchange_reconcile",
            REPORT_ENGLISH.exchange_reconcile,
            REPORT_CHINESE.exchange_reconcile,
        ),
        (
            "exchange_reconcile_none",
            REPORT_ENGLISH.exchange_reconcile_none,
            REPORT_CHINESE.exchange_reconcile_none,
        ),
        (
            "exchange_reconcile_pair",
            REPORT_ENGLISH.exchange_reconcile_pair,
            REPORT_CHINESE.exchange_reconcile_pair,
        ),
        (
            "exchange_radar_header",
            REPORT_ENGLISH.exchange_radar_header,
            REPORT_CHINESE.exchange_radar_header,
        ),
        (
            "exchange_radar_caveat",
            REPORT_ENGLISH.exchange_radar_caveat,
            REPORT_CHINESE.exchange_radar_caveat,
        ),
        (
            "exchange_radar_no_data",
            REPORT_ENGLISH.exchange_radar_no_data,
            REPORT_CHINESE.exchange_radar_no_data,
        ),
        (
            "exchange_radar_no_start",
            REPORT_ENGLISH.exchange_radar_no_start,
            REPORT_CHINESE.exchange_radar_no_start,
        ),
        (
            "exchange_radar_diagnostics",
            REPORT_ENGLISH.exchange_radar_diagnostics,
            REPORT_CHINESE.exchange_radar_diagnostics,
        ),
        (
            "exchange_radar_budget_exhausted",
            REPORT_ENGLISH.exchange_radar_budget_exhausted,
            REPORT_CHINESE.exchange_radar_budget_exhausted,
        ),
        (
            "exchange_radar_no_loop",
            REPORT_ENGLISH.exchange_radar_no_loop,
            REPORT_CHINESE.exchange_radar_no_loop,
        ),
        (
            "exchange_series_header",
            REPORT_ENGLISH.exchange_series_header,
            REPORT_CHINESE.exchange_series_header,
        ),
        (
            "exchange_series_point",
            REPORT_ENGLISH.exchange_series_point,
            REPORT_CHINESE.exchange_series_point,
        ),
        (
            "exchange_series_peak",
            REPORT_ENGLISH.exchange_series_peak,
            REPORT_CHINESE.exchange_series_peak,
        ),
        (
            "leg_share",
            REPORT_ENGLISH.leg_share,
            REPORT_CHINESE.leg_share,
        ),
        (
            "route_direct_label",
            REPORT_ENGLISH.route_direct_label,
            REPORT_CHINESE.route_direct_label,
        ),
        (
            "route_baseline",
            REPORT_ENGLISH.route_baseline,
            REPORT_CHINESE.route_baseline,
        ),
        (
            "route_no_front_price",
            REPORT_ENGLISH.route_no_front_price,
            REPORT_CHINESE.route_no_front_price,
        ),
        ("unpriced", REPORT_ENGLISH.unpriced, REPORT_CHINESE.unpriced),
        ("risks", REPORT_ENGLISH.risks, REPORT_CHINESE.risks),
        ("probe", REPORT_ENGLISH.probe, REPORT_CHINESE.probe),
        ("flip", REPORT_ENGLISH.flip, REPORT_CHINESE.flip),
        (
            "no_route_for_pair",
            REPORT_ENGLISH.no_route_for_pair,
            REPORT_CHINESE.no_route_for_pair,
        ),
        (
            "route_via",
            REPORT_ENGLISH.route_via,
            REPORT_CHINESE.route_via,
        ),
        (
            "route_front_depth",
            REPORT_ENGLISH.route_front_depth,
            REPORT_CHINESE.route_front_depth,
        ),
        (
            "route_front_short",
            REPORT_ENGLISH.route_front_short,
            REPORT_CHINESE.route_front_short,
        ),
        (
            "no_route_beats_direct",
            REPORT_ENGLISH.no_route_beats_direct,
            REPORT_CHINESE.no_route_beats_direct,
        ),
        (
            "routes_hidden_worse_than_direct",
            REPORT_ENGLISH.routes_hidden_worse_than_direct,
            REPORT_CHINESE.routes_hidden_worse_than_direct,
        ),
        (
            "valuation_two_sided",
            REPORT_ENGLISH.valuation_two_sided,
            REPORT_CHINESE.valuation_two_sided,
        ),
        (
            "valuation_one_sided",
            REPORT_ENGLISH.valuation_one_sided,
            REPORT_CHINESE.valuation_one_sided,
        ),
        (
            "anchor_recommendation",
            REPORT_ENGLISH.anchor_recommendation,
            REPORT_CHINESE.anchor_recommendation,
        ),
        (
            "targets_without_route",
            REPORT_ENGLISH.targets_without_route,
            REPORT_CHINESE.targets_without_route,
        ),
        (
            "history_header",
            REPORT_ENGLISH.history_header,
            REPORT_CHINESE.history_header,
        ),
        (
            "candle_line",
            REPORT_ENGLISH.candle_line,
            REPORT_CHINESE.candle_line,
        ),
        (
            "radar_probe_header",
            REPORT_ENGLISH.radar_probe_header,
            REPORT_CHINESE.radar_probe_header,
        ),
        (
            "focus_suggestion",
            REPORT_ENGLISH.focus_suggestion,
            REPORT_CHINESE.focus_suggestion,
        ),
        (
            "freshness_config_invalid",
            REPORT_ENGLISH.freshness_config_invalid,
            REPORT_CHINESE.freshness_config_invalid,
        ),
        (
            "freshness_light_line",
            REPORT_ENGLISH.freshness_light_line,
            REPORT_CHINESE.freshness_light_line,
        ),
        (
            "settlement_config_invalid",
            REPORT_ENGLISH.settlement_config_invalid,
            REPORT_CHINESE.settlement_config_invalid,
        ),
        (
            "settlement_config_partial",
            REPORT_ENGLISH.settlement_config_partial,
            REPORT_CHINESE.settlement_config_partial,
        ),
        (
            "maker_header",
            REPORT_ENGLISH.maker_header,
            REPORT_CHINESE.maker_header,
        ),
        (
            "maker_instant",
            REPORT_ENGLISH.maker_instant,
            REPORT_CHINESE.maker_instant,
        ),
        (
            "maker_no_instant",
            REPORT_ENGLISH.maker_no_instant,
            REPORT_CHINESE.maker_no_instant,
        ),
        (
            "maker_no_book",
            REPORT_ENGLISH.maker_no_book,
            REPORT_CHINESE.maker_no_book,
        ),
        (
            "maker_undercut",
            REPORT_ENGLISH.maker_undercut,
            REPORT_CHINESE.maker_undercut,
        ),
        (
            "maker_match",
            REPORT_ENGLISH.maker_match,
            REPORT_CHINESE.maker_match,
        ),
        (
            "maker_greedy",
            REPORT_ENGLISH.maker_greedy,
            REPORT_CHINESE.maker_greedy,
        ),
        (
            "maker_improvement",
            REPORT_ENGLISH.maker_improvement,
            REPORT_CHINESE.maker_improvement,
        ),
        (
            "maker_not_worth",
            REPORT_ENGLISH.maker_not_worth,
            REPORT_CHINESE.maker_not_worth,
        ),
        (
            "maker_spread",
            REPORT_ENGLISH.maker_spread,
            REPORT_CHINESE.maker_spread,
        ),
        (
            "maker_depth",
            REPORT_ENGLISH.maker_depth,
            REPORT_CHINESE.maker_depth,
        ),
        (
            "maker_max_single",
            REPORT_ENGLISH.maker_max_single,
            REPORT_CHINESE.maker_max_single,
        ),
        (
            "maker_excluded",
            REPORT_ENGLISH.maker_excluded,
            REPORT_CHINESE.maker_excluded,
        ),
        (
            "maker_risks",
            REPORT_ENGLISH.maker_risks,
            REPORT_CHINESE.maker_risks,
        ),
        (
            "better_than_direct",
            REPORT_ENGLISH.better_than_direct,
            REPORT_CHINESE.better_than_direct,
        ),
        (
            "worse_than_direct",
            REPORT_ENGLISH.worse_than_direct,
            REPORT_CHINESE.worse_than_direct,
        ),
        (
            "level_with_direct",
            REPORT_ENGLISH.level_with_direct,
            REPORT_CHINESE.level_with_direct,
        ),
        (
            "no_direct_route",
            REPORT_ENGLISH.no_direct_route,
            REPORT_CHINESE.no_direct_route,
        ),
        ("leg_take", REPORT_ENGLISH.leg_take, REPORT_CHINESE.leg_take),
        (
            "leg_covered",
            REPORT_ENGLISH.leg_covered,
            REPORT_CHINESE.leg_covered,
        ),
        (
            "leg_not_enough_listed",
            REPORT_ENGLISH.leg_not_enough_listed,
            REPORT_CHINESE.leg_not_enough_listed,
        ),
        (
            "leg_no_listings",
            REPORT_ENGLISH.leg_no_listings,
            REPORT_CHINESE.leg_no_listings,
        ),
        (
            "leg_bound_by_next",
            REPORT_ENGLISH.leg_bound_by_next,
            REPORT_CHINESE.leg_bound_by_next,
        ),
        (
            "leg_single_listing",
            REPORT_ENGLISH.leg_single_listing,
            REPORT_CHINESE.leg_single_listing,
        ),
        (
            "pinch_shared",
            REPORT_ENGLISH.pinch_shared,
            REPORT_CHINESE.pinch_shared,
        ),
        (
            "nothing_to_convert",
            REPORT_ENGLISH.nothing_to_convert,
            REPORT_CHINESE.nothing_to_convert,
        ),
        (
            "same_currency",
            REPORT_ENGLISH.same_currency,
            REPORT_CHINESE.same_currency,
        ),
        (
            "focus_has_no_targets",
            REPORT_ENGLISH.focus_has_no_targets,
            REPORT_CHINESE.focus_has_no_targets,
        ),
        (
            "scan_accounting",
            REPORT_ENGLISH.scan_accounting,
            REPORT_CHINESE.scan_accounting,
        ),
        (
            "core_liquidity",
            REPORT_ENGLISH.core_liquidity,
            REPORT_CHINESE.core_liquidity,
        ),
        (
            "no_price_capture",
            REPORT_ENGLISH.no_price_capture,
            REPORT_CHINESE.no_price_capture,
        ),
        (
            "coverage_unavailable",
            REPORT_ENGLISH.coverage_unavailable,
            REPORT_CHINESE.coverage_unavailable,
        ),
        (
            "coverage_progress",
            REPORT_ENGLISH.coverage_progress,
            REPORT_CHINESE.coverage_progress,
        ),
        (
            "pairs_complete",
            REPORT_ENGLISH.pairs_complete,
            REPORT_CHINESE.pairs_complete,
        ),
        (
            "no_core_currency",
            REPORT_ENGLISH.no_core_currency,
            REPORT_CHINESE.no_core_currency,
        ),
        (
            "not_enough_market",
            REPORT_ENGLISH.not_enough_market,
            REPORT_CHINESE.not_enough_market,
        ),
        (
            "no_start_units",
            REPORT_ENGLISH.no_start_units,
            REPORT_CHINESE.no_start_units,
        ),
        (
            "partial_scan",
            REPORT_ENGLISH.partial_scan,
            REPORT_CHINESE.partial_scan,
        ),
        (
            "results_cut",
            REPORT_ENGLISH.results_cut,
            REPORT_CHINESE.results_cut,
        ),
        (
            "nothing_beats_holding",
            REPORT_ENGLISH.nothing_beats_holding,
            REPORT_CHINESE.nothing_beats_holding,
        ),
        ("unpriced", REPORT_ENGLISH.unpriced, REPORT_CHINESE.unpriced),
        (
            "no_pairs_captured",
            REPORT_ENGLISH.no_pairs_captured,
            REPORT_CHINESE.no_pairs_captured,
        ),
        (
            "nothing_to_probe",
            REPORT_ENGLISH.nothing_to_probe,
            REPORT_CHINESE.nothing_to_probe,
        ),
        (
            "no_data_in_window",
            REPORT_ENGLISH.no_data_in_window,
            REPORT_CHINESE.no_data_in_window,
        ),
        (
            "no_history_yet",
            REPORT_ENGLISH.no_history_yet,
            REPORT_CHINESE.no_history_yet,
        ),
        (
            "median_low_high",
            REPORT_ENGLISH.median_low_high,
            REPORT_CHINESE.median_low_high,
        ),
        (
            "maker_over_taker",
            REPORT_ENGLISH.maker_over_taker,
            REPORT_CHINESE.maker_over_taker,
        ),
        (
            "listings_note",
            REPORT_ENGLISH.listings_note,
            REPORT_CHINESE.listings_note,
        ),
        (
            "nothing_current",
            REPORT_ENGLISH.nothing_current,
            REPORT_CHINESE.nothing_current,
        ),
        ("risks", REPORT_ENGLISH.risks, REPORT_CHINESE.risks),
        ("probe", REPORT_ENGLISH.probe, REPORT_CHINESE.probe),
        ("flip", REPORT_ENGLISH.flip, REPORT_CHINESE.flip),
        (
            "analytics_config_invalid",
            REPORT_ENGLISH.analytics_config_invalid,
            REPORT_CHINESE.analytics_config_invalid,
        ),
        (
            "analytics_no_data",
            REPORT_ENGLISH.analytics_no_data,
            REPORT_CHINESE.analytics_no_data,
        ),
        (
            "analytics_season_line",
            REPORT_ENGLISH.analytics_season_line,
            REPORT_CHINESE.analytics_season_line,
        ),
        (
            "analytics_as_of",
            REPORT_ENGLISH.analytics_as_of,
            REPORT_CHINESE.analytics_as_of,
        ),
        (
            "analytics_anchor_line",
            REPORT_ENGLISH.analytics_anchor_line,
            REPORT_CHINESE.analytics_anchor_line,
        ),
        (
            "analytics_breadth_line",
            REPORT_ENGLISH.analytics_breadth_line,
            REPORT_CHINESE.analytics_breadth_line,
        ),
        (
            "analytics_cross_line",
            REPORT_ENGLISH.analytics_cross_line,
            REPORT_CHINESE.analytics_cross_line,
        ),
        (
            "analytics_table_header",
            REPORT_ENGLISH.analytics_table_header,
            REPORT_CHINESE.analytics_table_header,
        ),
        (
            "analytics_marker_high_turnover",
            REPORT_ENGLISH.analytics_marker_high_turnover,
            REPORT_CHINESE.analytics_marker_high_turnover,
        ),
        (
            "analytics_marker_greedy",
            REPORT_ENGLISH.analytics_marker_greedy,
            REPORT_CHINESE.analytics_marker_greedy,
        ),
        (
            "identity_magnitude_suspect",
            REPORT_ENGLISH.identity_magnitude_suspect,
            REPORT_CHINESE.identity_magnitude_suspect,
        ),
    ]
}

/// Picks one of a pair by language.
///
/// A local helper rather than a two-instance catalogue struct: these are
/// per-variant matches, so the pairing is already stated at each arm and a
/// struct would only move it somewhere the match cannot check.
const fn pick(language: UiLanguage, english: &'static str, chinese: &'static str) -> &'static str {
    match language {
        UiLanguage::English => english,
        UiLanguage::Chinese => chinese,
    }
}

/// How far a route can actually be trusted to execute.
#[must_use]
pub const fn actionability(language: UiLanguage, value: Actionability) -> &'static str {
    match value {
        Actionability::InstantExecutable => pick(language, "executable now", "现在就能成交"),
        Actionability::MakerTheoretical => {
            pick(language, "needs someone to take a listing", "要等人吃单")
        }
        Actionability::ProbeRequired => pick(
            language,
            "capture more before trusting",
            "数据不够，先多抓几次",
        ),
        Actionability::SuspiciousOutlier => {
            pick(language, "looks wrong, not good", "数据可疑，不是机会")
        }
    }
}

/// Why a route's execution is not guaranteed.
#[must_use]
pub const fn execution_risk(language: UiLanguage, value: ExecutionRisk) -> &'static str {
    match value {
        ExecutionRisk::ComparatorBoundary => pick(language, "aggregate row", "聚合行边界"),
        ExecutionRisk::MakerReference => pick(language, "maker reference", "挂单参考价"),
        ExecutionRisk::CompetingReference => pick(language, "competing reference", "竞争方参考价"),
        ExecutionRisk::StaleData => pick(language, "stale data", "数据过期"),
        ExecutionRisk::ArchivedData => pick(language, "archived data", "归档数据"),
        ExecutionRisk::ClockSkewFuture => pick(language, "timestamp in the future", "时间戳在未来"),
        ExecutionRisk::CaptureSkewExceeded => {
            pick(language, "captures too far apart", "各步抓取时间相差过大")
        }
        ExecutionRisk::LowConfidence => pick(language, "low confidence", "置信度低"),
        ExecutionRisk::ThinLiquidity => pick(language, "thin liquidity", "流动性薄"),
        ExecutionRisk::SingleListingBook => {
            pick(language, "only one listing on this side", "该侧仅一条挂单")
        }
        ExecutionRisk::LiquidityCapped => pick(language, "capped by liquidity", "受流动性限制"),
        ExecutionRisk::PartialRoute => pick(language, "partial route", "路径不完整"),
        ExecutionRisk::ResidualInventory => pick(language, "residual inventory", "有零头库存"),
        ExecutionRisk::MakerDepthExceeded => pick(language, "beyond maker depth", "超出挂单深度"),
        ExecutionRisk::PriceOutlier => pick(language, "price outlier", "价格离群"),
        ExecutionRisk::OutsideTopBookBand => pick(language, "off the top of book", "偏离盘口首档"),
        ExecutionRisk::UnsupportedRecord => pick(language, "unsupported record", "记录格式不支持"),
        ExecutionRisk::NeedsProbe => pick(language, "needs a probe", "需要补采集"),
    }
}

/// The traffic light one freshness class shows as. Green acts as-is,
/// yellow says verify the rate in game first, red asks for a recapture —
/// the selection and risk semantics behind the classes are untouched.
#[must_use]
pub const fn freshness_light(language: UiLanguage, value: FreshnessStatus) -> &'static str {
    match value {
        FreshnessStatus::Fresh => pick(language, "green - fresh", "绿（新鲜）"),
        FreshnessStatus::Usable => pick(
            language,
            "yellow - verify in game before acting",
            "黄（用前先核对盘口）",
        ),
        FreshnessStatus::Stale => {
            pick(language, "red - stale, recapture", "红（已过期，建议重抓）")
        }
        FreshnessStatus::Archived => pick(language, "red - archived", "红（归档数据）"),
    }
}

/// Why a listing was kept out of the maker queue math.
#[must_use]
pub const fn maker_exclusion(language: UiLanguage, value: MakerQueueExclusion) -> &'static str {
    match value {
        MakerQueueExclusion::PriceOutlier => pick(language, "price outlier", "价格离群"),
    }
}

/// An asset's move relative to the market median move — the anchor-drift
/// corrected verdict, never the raw move against the anchor.
#[must_use]
pub const fn trend_verdict(
    language: UiLanguage,
    value: ptt_strategy::TrendVerdict,
) -> &'static str {
    match value {
        ptt_strategy::TrendVerdict::Appreciating => pick(language, "appreciating", "升值"),
        ptt_strategy::TrendVerdict::Holding => pick(language, "holding", "保值"),
        ptt_strategy::TrendVerdict::Depreciating => pick(language, "depreciating", "贬值"),
    }
}

/// The mirror-vs-junk discrimination read from the imbalance direction.
#[must_use]
pub const fn liquidity_class(
    language: UiLanguage,
    value: ptt_strategy::LiquidityClass,
) -> &'static str {
    match value {
        ptt_strategy::LiquidityClass::Scarce => pick(language, "scarce", "供不应求"),
        ptt_strategy::LiquidityClass::Oversupplied => pick(language, "oversupplied", "供过于求"),
        ptt_strategy::LiquidityClass::Balanced => pick(language, "balanced", "供需均衡"),
        ptt_strategy::LiquidityClass::Quiet => pick(language, "quiet", "冷清"),
    }
}

/// Whether the settlement anchor itself is drifting. "Inflating" is the
/// anchor losing purchasing power — most asset prices in it rising.
#[must_use]
pub const fn anchor_drift(language: UiLanguage, value: ptt_strategy::AnchorDrift) -> &'static str {
    match value {
        ptt_strategy::AnchorDrift::Inflating => {
            pick(language, "inflating (losing value)", "通胀（在贬值）")
        }
        ptt_strategy::AnchorDrift::Deflating => {
            pick(language, "deflating (gaining value)", "通缩（在升值）")
        }
        ptt_strategy::AnchorDrift::Steady => pick(language, "steady", "稳定"),
    }
}

/// Why a leg of a route might not fill as quoted.
#[must_use]
pub const fn execution_risk_flag(language: UiLanguage, value: ExecutionRiskFlag) -> &'static str {
    match value {
        ExecutionRiskFlag::ReverseFromCompeting => {
            pick(language, "reverse from competing", "反向取自竞争方")
        }
        ExecutionRiskFlag::MakerReference => pick(language, "maker reference", "挂单参考价"),
        ExecutionRiskFlag::FillNotGuaranteed => pick(language, "fill not guaranteed", "不保证成交"),
        ExecutionRiskFlag::MakerDepthExceeded => {
            pick(language, "beyond maker depth", "超出挂单深度")
        }
        ExecutionRiskFlag::LiquidityCapped => pick(language, "capped by liquidity", "受流动性限制"),
        ExecutionRiskFlag::SingleListingBook => {
            pick(language, "only one listing on this side", "该侧仅一条挂单")
        }
        ExecutionRiskFlag::StockOutOfBand => pick(
            language,
            "stock unlike the rest of its side",
            "库存与同侧其他行差一个数量级",
        ),
        ExecutionRiskFlag::BelowMinimumOutput => {
            pick(language, "below minimum output", "低于最小产出")
        }
        ExecutionRiskFlag::CapacityRoundedToUnit => {
            pick(language, "rounded to a whole unit", "容量取整到整数单位")
        }
        ExecutionRiskFlag::UnknownFee => pick(language, "unknown fee", "手续费未知"),
        ExecutionRiskFlag::PartialRoute => pick(language, "partial route", "路径不完整"),
        ExecutionRiskFlag::ResidualInventory => pick(language, "residual inventory", "有零头库存"),
        ExecutionRiskFlag::MultiHopMaker => pick(language, "multi-hop maker leg", "中途需挂单"),
        ExecutionRiskFlag::SearchTruncated => pick(language, "search truncated", "搜索被截断"),
        ExecutionRiskFlag::UnverifiedProductPolicy => {
            pick(language, "trade rules unverified", "成交规则未验证")
        }
        ExecutionRiskFlag::UnverifiedMinimumLots => {
            pick(language, "minimum lot unverified", "最小成交单位未验证")
        }
        ExecutionRiskFlag::CaptureSkewUnverified => {
            pick(language, "capture gap unverified", "抓取时差未验证")
        }
        ExecutionRiskFlag::CaptureSkewExceeded => {
            pick(language, "captures too far apart", "抓取时差过大")
        }
    }
}

/// How urgently a pair wants capturing.
#[must_use]
pub const fn probe_priority(language: UiLanguage, value: ProbePriority) -> &'static str {
    match value {
        ProbePriority::High => pick(language, "high", "高"),
        ProbePriority::Medium => pick(language, "medium", "中"),
        ProbePriority::Low => pick(language, "low", "低"),
    }
}

/// What capturing a pair would supply.
#[must_use]
/// 面板核对每对的"故事"，按语言。以前直接印枚举的英文名，中文行里夹一个
/// `PanelWorse`——审查抓的。
pub const fn exchange_reading(
    language: UiLanguage,
    value: crate::reports::ExchangeReconcileReading,
) -> &'static str {
    use crate::reports::ExchangeReconcileReading as Reading;
    match value {
        Reading::SuspectMapping => pick(language, "suspect mapping", "疑似映射错"),
        Reading::PanelWorse => pick(language, "panel worse than traded", "面板比成交价差"),
        Reading::PanelBetter => pick(language, "panel better than traded", "面板比成交价好"),
        Reading::Sporadic => pick(language, "sporadic", "偶发越界"),
    }
}

pub const fn probe_reason(language: UiLanguage, value: ProbeReason) -> &'static str {
    match value {
        ProbeReason::MissingForwardQuote => pick(language, "no forward quote", "缺正向报价"),
        ProbeReason::MissingInstantQuote => pick(language, "no instant quote", "缺即时成交价"),
        ProbeReason::MissingMakerReferenceQuote => {
            pick(language, "no maker reference", "缺挂单参考价")
        }
        ProbeReason::OnlyOldData => pick(language, "only old data", "只有旧数据"),
        ProbeReason::LowConfidence => pick(language, "low confidence", "置信度低"),
        ProbeReason::ComparatorBoundary => pick(language, "aggregate row", "聚合行边界"),
        ProbeReason::ThinLiquidity => pick(language, "thin liquidity", "流动性薄"),
        ProbeReason::MissingBridgeQuote => pick(language, "no bridge quote", "缺中转报价"),
        ProbeReason::OpportunityConfirmation => {
            pick(language, "confirming an opportunity", "确认一个机会")
        }
    }
}

/// What a radar row is: a conversion, or a loop back to where you started.
#[must_use]
pub const fn radar_item_kind(language: UiLanguage, value: RadarItemKind) -> &'static str {
    match value {
        RadarItemKind::BestConversion => pick(language, "best conversion", "最优兑换"),
        RadarItemKind::Loop => pick(language, "loop", "闭环"),
    }
}

/// Why a radar row is on the list.
#[must_use]
pub const fn radar_reason(language: UiLanguage, value: RadarReason) -> &'static str {
    match value {
        RadarReason::BetterThanDirect => pick(language, "better than direct", "优于直兑"),
        RadarReason::NoDirectBaseline => pick(language, "no direct baseline", "没有直兑基准"),
        RadarReason::LoopReturn => pick(language, "loop return", "闭环收益"),
        RadarReason::GrossTheoryOnly => pick(language, "gross theory only", "仅理论毛利"),
        RadarReason::ResidualInventory => pick(language, "residual inventory", "有零头库存"),
        RadarReason::MakerReference => pick(language, "maker reference", "挂单参考价"),
        RadarReason::MirroredFromCompeting => pick(
            language,
            "priced off the competing table",
            "价格取自对侧的竞争挂单",
        ),
        RadarReason::SearchTruncated => pick(language, "search truncated", "搜索被截断"),
        RadarReason::CaptureSkewUnverified => {
            pick(language, "capture gap unverified", "抓取时差未验证")
        }
        RadarReason::CaptureSkewExceeded => {
            pick(language, "captures too far apart", "抓取时差过大")
        }
        RadarReason::StakeRaisedToMinimum => {
            pick(language, "sized to the smallest trade", "已按最小可成交量")
        }
    }
}

/// What a watched pair is still missing.
#[must_use]
pub const fn focus_coverage_status(
    language: UiLanguage,
    value: FocusCoverageStatus,
) -> &'static str {
    match value {
        FocusCoverageStatus::Complete => pick(language, "complete", "完整"),
        FocusCoverageStatus::MissingInstant => pick(language, "no instant quote", "缺即时成交价"),
        FocusCoverageStatus::MissingMakerReference => {
            pick(language, "no maker reference", "缺挂单参考价")
        }
        FocusCoverageStatus::MissingBoth => pick(language, "no quotes at all", "两侧报价都缺"),
        FocusCoverageStatus::OnlyOldData => pick(language, "only old data", "只有旧数据"),
        FocusCoverageStatus::NeedsReview => pick(language, "needs review", "需要复核"),
    }
}

/// What looks wrong about a pair's recent prices.
#[must_use]
pub const fn price_anomaly_kind(language: UiLanguage, value: PriceAnomalyKind) -> &'static str {
    match value {
        PriceAnomalyKind::RateSpike => pick(language, "rate spike", "价格突涨"),
        PriceAnomalyKind::RateDrop => pick(language, "rate drop", "价格突跌"),
        PriceAnomalyKind::SpreadWidened => pick(language, "spread widened", "价差走阔"),
        PriceAnomalyKind::ThinLiquidity => pick(language, "thin liquidity", "流动性薄"),
        PriceAnomalyKind::StaleLatest => pick(language, "latest point is stale", "最新一点已过期"),
        PriceAnomalyKind::ClockSkewFuture => {
            pick(language, "timestamp in the future", "时间戳在未来")
        }
    }
}

/// How much an anomaly matters.
#[must_use]
pub const fn anomaly_severity(language: UiLanguage, value: AnomalySeverity) -> &'static str {
    match value {
        AnomalySeverity::Low => pick(language, "minor", "轻"),
        AnomalySeverity::Medium => pick(language, "notable", "中"),
        AnomalySeverity::High => pick(language, "serious", "重"),
    }
}

/// What to do about a currency's standing in the league.
#[must_use]
pub const fn anchor_action(language: UiLanguage, value: AnchorAction) -> &'static str {
    match value {
        AnchorAction::PromoteToCore => pick(language, "promote to core", "升为核心币"),
        AnchorAction::Watch => pick(language, "keep watching", "继续观察"),
    }
}

/// Joins named values the way the reports list them.
///
/// Comma-separated in either language, but with the ideographic comma in
/// Chinese: a list punctuated with ASCII commas reads as a sentence that lost
/// its spacing.
pub fn join<T: Copy>(
    language: UiLanguage,
    values: &[T],
    name: fn(UiLanguage, T) -> &'static str,
) -> String {
    let separator = match language {
        UiLanguage::English => ", ",
        UiLanguage::Chinese => "、",
    };
    values
        .iter()
        .map(|value| name(language, *value))
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name must be present, in both languages, and actually differ.
    ///
    /// The matches are exhaustive, so a new variant cannot be *missing* — but
    /// it can very easily be added with the English text copied into the
    /// Chinese slot, which compiles and ships and looks like a translation
    /// until someone reads it.
    #[test]
    fn every_typed_name_is_translated() {
        macro_rules! check {
            ($name:path, [$($variant:expr),* $(,)?]) => {
                $(
                    let english = $name(UiLanguage::English, $variant);
                    let chinese = $name(UiLanguage::Chinese, $variant);
                    assert!(!english.trim().is_empty(), "{:?} has no English name", $variant);
                    assert!(!chinese.trim().is_empty(), "{:?} has no Chinese name", $variant);
                    assert_ne!(
                        english, chinese,
                        "{:?} was never translated -- both languages read {english:?}",
                        $variant
                    );
                    assert!(
                        !chinese.is_ascii(),
                        "{:?} reads {chinese:?} in Chinese, which is all ASCII",
                        $variant
                    );
                )*
            };
        }

        check!(
            actionability,
            [
                Actionability::InstantExecutable,
                Actionability::MakerTheoretical,
                Actionability::ProbeRequired,
                Actionability::SuspiciousOutlier,
            ]
        );
        check!(maker_exclusion, [MakerQueueExclusion::PriceOutlier]);
        check!(
            freshness_light,
            [
                FreshnessStatus::Fresh,
                FreshnessStatus::Usable,
                FreshnessStatus::Stale,
                FreshnessStatus::Archived,
            ]
        );
        check!(
            execution_risk,
            [
                ExecutionRisk::ComparatorBoundary,
                ExecutionRisk::MakerReference,
                ExecutionRisk::CompetingReference,
                ExecutionRisk::StaleData,
                ExecutionRisk::ArchivedData,
                ExecutionRisk::ClockSkewFuture,
                ExecutionRisk::CaptureSkewExceeded,
                ExecutionRisk::LowConfidence,
                ExecutionRisk::ThinLiquidity,
                ExecutionRisk::SingleListingBook,
                ExecutionRisk::LiquidityCapped,
                ExecutionRisk::PartialRoute,
                ExecutionRisk::ResidualInventory,
                ExecutionRisk::MakerDepthExceeded,
                ExecutionRisk::PriceOutlier,
                ExecutionRisk::OutsideTopBookBand,
                ExecutionRisk::UnsupportedRecord,
                ExecutionRisk::NeedsProbe,
            ]
        );
        check!(
            execution_risk_flag,
            [
                ExecutionRiskFlag::ReverseFromCompeting,
                ExecutionRiskFlag::MakerReference,
                ExecutionRiskFlag::FillNotGuaranteed,
                ExecutionRiskFlag::MakerDepthExceeded,
                ExecutionRiskFlag::LiquidityCapped,
                ExecutionRiskFlag::SingleListingBook,
                ExecutionRiskFlag::StockOutOfBand,
                ExecutionRiskFlag::BelowMinimumOutput,
                ExecutionRiskFlag::CapacityRoundedToUnit,
                ExecutionRiskFlag::UnknownFee,
                ExecutionRiskFlag::PartialRoute,
                ExecutionRiskFlag::ResidualInventory,
                ExecutionRiskFlag::MultiHopMaker,
                ExecutionRiskFlag::SearchTruncated,
                ExecutionRiskFlag::UnverifiedProductPolicy,
                ExecutionRiskFlag::UnverifiedMinimumLots,
                ExecutionRiskFlag::CaptureSkewUnverified,
                ExecutionRiskFlag::CaptureSkewExceeded,
            ]
        );
        check!(
            probe_priority,
            [
                ProbePriority::High,
                ProbePriority::Medium,
                ProbePriority::Low
            ]
        );
        check!(
            probe_reason,
            [
                ProbeReason::MissingForwardQuote,
                ProbeReason::MissingInstantQuote,
                ProbeReason::MissingMakerReferenceQuote,
                ProbeReason::OnlyOldData,
                ProbeReason::LowConfidence,
                ProbeReason::ComparatorBoundary,
                ProbeReason::ThinLiquidity,
                ProbeReason::MissingBridgeQuote,
                ProbeReason::OpportunityConfirmation,
            ]
        );
        check!(
            radar_item_kind,
            [RadarItemKind::BestConversion, RadarItemKind::Loop]
        );
        check!(
            radar_reason,
            [
                RadarReason::BetterThanDirect,
                RadarReason::NoDirectBaseline,
                RadarReason::LoopReturn,
                RadarReason::GrossTheoryOnly,
                RadarReason::ResidualInventory,
                RadarReason::MakerReference,
                RadarReason::SearchTruncated,
                RadarReason::CaptureSkewUnverified,
                RadarReason::CaptureSkewExceeded,
                RadarReason::StakeRaisedToMinimum,
            ]
        );
        check!(
            focus_coverage_status,
            [
                FocusCoverageStatus::Complete,
                FocusCoverageStatus::MissingInstant,
                FocusCoverageStatus::MissingMakerReference,
                FocusCoverageStatus::MissingBoth,
                FocusCoverageStatus::OnlyOldData,
                FocusCoverageStatus::NeedsReview,
            ]
        );
        check!(
            price_anomaly_kind,
            [
                PriceAnomalyKind::RateSpike,
                PriceAnomalyKind::RateDrop,
                PriceAnomalyKind::SpreadWidened,
                PriceAnomalyKind::ThinLiquidity,
                PriceAnomalyKind::StaleLatest,
                PriceAnomalyKind::ClockSkewFuture,
            ]
        );
        check!(
            anomaly_severity,
            [
                AnomalySeverity::Low,
                AnomalySeverity::Medium,
                AnomalySeverity::High,
            ]
        );
        check!(
            anchor_action,
            [AnchorAction::PromoteToCore, AnchorAction::Watch]
        );
    }

    /// A translated template must keep every slot the original had.
    ///
    /// This is the failure nothing else catches. `format!` counts its
    /// arguments; these templates are picked at run time, so nothing does --
    /// a Chinese template that dropped a slot renders a sentence missing its
    /// number, and one that gained a slot renders a literal brace pair. Both
    /// look like a typo in the output and neither fails anywhere else.
    #[test]
    fn both_languages_have_the_same_slots() {
        for (field, english, chinese) in report_pairs() {
            assert!(!english.trim().is_empty(), "{field} has no English text");
            assert!(!chinese.trim().is_empty(), "{field} has no Chinese text");
            assert_eq!(
                english.matches("{}").count(),
                chinese.matches("{}").count(),
                "{field} has {english:?} against {chinese:?}"
            );
            assert_ne!(
                english, chinese,
                "{field} was never translated -- both read {english:?}"
            );
        }
    }

    #[test]
    fn a_template_keeps_an_unfilled_slot_visible() {
        assert_eq!(fill("a {} b {} c", &["1", "2"]), "a 1 b 2 c");
        assert_eq!(fill("a {} b {} c", &["1"]), "a 1 b {} c");
        assert_eq!(fill("no slots", &["1"]), "no slots");
    }

    #[test]
    fn a_list_is_punctuated_for_its_language() {
        let flags = [
            ExecutionRiskFlag::UnknownFee,
            ExecutionRiskFlag::PartialRoute,
        ];
        assert_eq!(
            join(UiLanguage::English, &flags, execution_risk_flag),
            "unknown fee, partial route"
        );
        assert_eq!(
            join(UiLanguage::Chinese, &flags, execution_risk_flag),
            "手续费未知、路径不完整"
        );
        assert_eq!(join(UiLanguage::English, &[], execution_risk_flag), "");
    }
}

/// The unit these numbers reach the reader in.
#[cfg(test)]
mod percentage_tests {
    use super::{
        REPORT_ENGLISH, fill, percent_from_basis_points, report_pairs,
        signed_percent_from_basis_points,
    };

    /// "bp" is desk jargon. A hundred basis points is one percent, and the
    /// person reading this screen has no reason to know that -- worse, the
    /// radar page already prints percentages, so the same quantity arrives
    /// in two different units depending on which page you are on.
    #[test]
    fn no_report_template_prints_basis_points() {
        for (field, english, chinese) in report_pairs() {
            assert!(
                !english.contains("bp"),
                "{field} still prints basis points in English: {english:?}"
            );
            assert!(
                !chinese.contains("bp"),
                "{field} still prints basis points in Chinese: {chinese:?}"
            );
        }
    }

    /// The real reading off the convert page on 2026-08-23: a route 1338
    /// basis points behind the direct trade.
    ///
    /// The magnitude goes into the template, never the signed value: the
    /// template's own words already point the direction. See
    /// [`super::versus_direct`].
    #[test]
    fn the_worse_than_direct_line_reads_as_a_percentage() {
        assert_eq!(
            fill(
                REPORT_ENGLISH.worse_than_direct,
                &["3451", &percent_from_basis_points(1338)]
            ),
            "-3451 (13.38% worse than direct)"
        );
    }

    /// The conversion is exact -- a hundred basis points is one percent, so
    /// there is a whole-percent part and a two-digit remainder and nothing
    /// left to round. The edges worth pinning are the ones smaller than a
    /// percent, where the whole part is zero and cannot carry the sign.
    #[test]
    fn basis_points_convert_to_two_decimals_without_loss() {
        assert_eq!(percent_from_basis_points(0), "0.00%");
        assert_eq!(percent_from_basis_points(1), "0.01%");
        assert_eq!(percent_from_basis_points(-1), "-0.01%");
        assert_eq!(percent_from_basis_points(99), "0.99%");
        assert_eq!(percent_from_basis_points(-99), "-0.99%");
        assert_eq!(percent_from_basis_points(100), "1.00%");
        assert_eq!(percent_from_basis_points(-1338), "-13.38%");
        assert_eq!(percent_from_basis_points(10_000), "100.00%");
    }

    /// A move says which way it went; a level does not. Zero counts as "not
    /// a fall", which is why it keeps the plus rather than going bare -- a
    /// bare number in a column of signed ones reads as a different kind of
    /// number, not as a smaller one.
    #[test]
    fn a_drift_is_written_with_the_sign_it_moved_in() {
        assert_eq!(signed_percent_from_basis_points(257), "+2.57%");
        assert_eq!(signed_percent_from_basis_points(-257), "-2.57%");
        assert_eq!(signed_percent_from_basis_points(0), "+0.00%");
        assert_eq!(signed_percent_from_basis_points(1), "+0.01%");
        assert_eq!(signed_percent_from_basis_points(-1), "-0.01%");
    }

    /// A card priced in a scarce anchor on launch day moves by tens of
    /// millions of percent. The number is right; the cell is one line high.
    /// Capped once here, for every page, instead of on the one page that
    /// happened to notice.
    #[test]
    fn runaway_percentages_are_capped_to_one_cell_on_both_formatters() {
        assert_eq!(signed_percent_from_basis_points(99_900), "+999.00%");
        assert_eq!(signed_percent_from_basis_points(99_901), ">+999%");
        assert_eq!(signed_percent_from_basis_points(4_844_094_326), ">+999%");
        assert_eq!(signed_percent_from_basis_points(-99_901), "<-999%");
        assert_eq!(percent_from_basis_points(99_901), ">999%");
        assert_eq!(percent_from_basis_points(-99_901), "<-999%");
    }
}

#[cfg(test)]
mod compact_units_tests {
    use super::compact_units;
    use ptt_settings::UiLanguage;

    /// The analytics page wrote 1214万 (hard-coded Chinese, in English too)
    /// while the exchange page wrote 125k / 3.5M for the same kind of number
    /// one tab over. One rule, chosen by the interface language.
    #[test]
    fn large_amounts_follow_the_interface_language() {
        assert_eq!(compact_units(1_200_000, UiLanguage::English), "1.2M");
        assert_eq!(compact_units(85_000, UiLanguage::English), "85k");
        assert_eq!(compact_units(9_999, UiLanguage::English), "9999");
        assert_eq!(compact_units(1_200_000, UiLanguage::Chinese), "120万");
        assert_eq!(compact_units(250_000_000, UiLanguage::Chinese), "2.5亿");
        assert_eq!(compact_units(18_531, UiLanguage::Chinese), "18531");
    }
}
