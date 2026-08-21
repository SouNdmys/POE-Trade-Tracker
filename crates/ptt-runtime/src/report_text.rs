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

use ptt_settings::UiLanguage;

use ptt_strategy::{
    Actionability, AnchorAction, AnomalySeverity, ExecutionRisk, MakerQueueExclusion,
    PriceAnomalyKind,
};
use ptt_trade_engine::ExecutionRiskFlag;
use ptt_workflows::{
    FocusCoverageStatus, ProbePriority, ProbeReason, RadarCategory, RadarItemKind, RadarReason,
};

/// Prose the reports assemble, in both interface languages.
///
/// Whole templates rather than fragments a caller concatenates. Word order
/// is not shared between these two languages -- `staking 10 divine-orb across
/// 3 targets` puts its count where the Chinese sentence does not -- so a
/// report built by gluing translated pieces together produces a sentence that
/// is grammatical in exactly one of them.
pub struct ReportText {
    pub tier_closed: &'static str,
    pub tier_theoretical: &'static str,
    pub tier_mark_to_market: &'static str,
    pub better_than_direct: &'static str,
    pub worse_than_direct: &'static str,
    pub level_with_direct: &'static str,
    pub no_direct_route: &'static str,
    pub size_down_to: &'static str,
    pub stranded: &'static str,
    pub no_cost_basis: &'static str,
    pub break_even_at: &'static str,
    pub nothing_to_convert: &'static str,
    pub core_liquidity: &'static str,
    pub no_price_capture: &'static str,
    pub coverage_unavailable: &'static str,
    pub coverage_progress: &'static str,
    pub pairs_complete: &'static str,
    pub no_core_currency: &'static str,
    pub not_enough_market: &'static str,
    pub cannot_stake: &'static str,
    pub staking: &'static str,
    pub partial_scan: &'static str,
    pub results_cut: &'static str,
    pub nothing_beats_holding: &'static str,
    pub unpriced: &'static str,
    pub out_amount: &'static str,
    pub no_pairs_captured: &'static str,
    pub nothing_to_probe: &'static str,
    pub no_history_yet: &'static str,
    pub median_low_high: &'static str,
    pub maker_over_taker: &'static str,
    pub listings_note: &'static str,
    pub nothing_current: &'static str,
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
    pub maker_excluded: &'static str,
    pub risks: &'static str,
    pub probe: &'static str,
    pub flip: &'static str,
}

#[must_use]
pub const fn report(language: UiLanguage) -> &'static ReportText {
    match language {
        UiLanguage::English => &REPORT_ENGLISH,
        UiLanguage::Chinese => &REPORT_CHINESE,
    }
}

pub static REPORT_ENGLISH: ReportText = ReportText {
    tier_closed: "closed",
    tier_theoretical: "theoretical",
    tier_mark_to_market: "mark-to-mkt",
    better_than_direct: "+{} ({}bp vs direct)",
    worse_than_direct: "-{} ({}bp vs direct)",
    level_with_direct: "level with direct",
    no_direct_route: "no direct route to compare",
    size_down_to: "size down to {} {}: past that, depth runs out",
    stranded: "stranded {} {}   {}",
    no_cost_basis: "no cost basis",
    break_even_at: "break even at 1 : {}",
    nothing_to_convert: "nothing to convert yet - capture a book first",
    core_liquidity: "core liquidity: {}",
    no_price_capture: "no price - capture this pair",
    coverage_unavailable: "coverage unavailable: {}",
    coverage_progress: "coverage: {} of {} pairs complete",
    pairs_complete: "{} of {} pairs complete",
    no_core_currency: "no core currency configured for this league",
    not_enough_market: "not enough of the market captured yet - flip a few pairs first",
    cannot_stake: "cannot stake {} {}",
    staking: "staking {} {} across {} targets",
    partial_scan: "partial scan - {} targets skipped, {} expansions used{}",
    results_cut: ", results cut to the top few",
    nothing_beats_holding: "nothing beats holding right now",
    unpriced: "unpriced",
    out_amount: "out {} {}",
    no_pairs_captured: "no pairs captured yet",
    nothing_to_probe: "nothing to probe - the book is current",
    no_history_yet: "no history yet for {} -> {}",
    median_low_high: "median {}   low {}   high {}",
    maker_over_taker: "maker over taker: {}bp",
    listings_note: "  (listings)",
    nothing_current: "nothing current - this is history, not a price",
    maker_header: "listing strategy {} -> {} at size {}",
    maker_instant: "take now at {}",
    maker_no_instant: "no instant fill - the taker side is empty",
    maker_no_book: "no competing listings - probe this pair first",
    maker_undercut: "undercut, list at {}",
    maker_match: "match the front, list at {} - queues behind it",
    maker_greedy: "greedy, list at {} - bets on drift",
    maker_improvement: "+{} {} vs taking ({}bp)",
    maker_not_worth: "no better than taking - trade instead",
    maker_spread: "front over instant: {}bp",
    maker_depth: "visible depth {} {}, max single order {} {}",
    maker_excluded: "excluded listing at {} (stock {}): {}",
    risks: "risks",
    probe: "probe",
    flip: "flip",
};

pub static REPORT_CHINESE: ReportText = ReportText {
    tier_closed: "已结算",
    tier_theoretical: "理论",
    tier_mark_to_market: "按市价",
    better_than_direct: "+{}（比直兑高 {}bp）",
    worse_than_direct: "-{}（比直兑低 {}bp）",
    level_with_direct: "与直兑持平",
    no_direct_route: "没有直兑路线可比",
    size_down_to: "减到 {} {}：再多深度就不够了",
    stranded: "剩下 {} {}   {}",
    no_cost_basis: "没有成本基准",
    break_even_at: "保本价 1 : {}",
    nothing_to_convert: "还没有可兑换的数据 — 先抓一个盘口",
    core_liquidity: "核心流通币：{}",
    no_price_capture: "没有价格 — 去翻这一对",
    coverage_unavailable: "覆盖情况读不出来：{}",
    coverage_progress: "覆盖：{} / {} 对已齐全",
    pairs_complete: "{} / {} 对已齐全",
    no_core_currency: "这个赛季没有配置核心流通币",
    not_enough_market: "抓到的市场数据还不够 — 先去翻几对",
    cannot_stake: "无法投入 {} {}",
    staking: "投入 {} {}，尝试 {} 个目标",
    partial_scan: "扫描不完整 — 跳过 {} 个目标，用掉 {} 次展开{}",
    results_cut: "，结果只留了前几条",
    nothing_beats_holding: "目前没有比继续持有更好的",
    unpriced: "无法定价",
    out_amount: "得到 {} {}",
    no_pairs_captured: "还没有抓到任何通货对",
    nothing_to_probe: "没有要补的 — 盘口是最新的",
    no_history_yet: "{} → {} 还没有历史数据",
    median_low_high: "中位 {}   最低 {}   最高 {}",
    maker_over_taker: "挂单高于吃单：{}bp",
    listings_note: "  （挂单）",
    nothing_current: "没有当前报价 — 这是历史，不是价格",
    maker_header: "挂单策略 {} → {}（规模 {}）",
    maker_instant: "立即成交价 {}",
    maker_no_instant: "无法立即成交 — 可用侧没有挂单",
    maker_no_book: "竞争侧没有挂单 — 先补采集这一对",
    maker_undercut: "机会（压一档），挂 {}",
    maker_match: "跟价，挂 {} — 排在原单之后",
    maker_greedy: "贪婪，挂 {} — 赌行情走势",
    maker_improvement: "比立即成交多 {} {}（{}bp）",
    maker_not_worth: "不比立即成交好 — 不如直接吃单",
    maker_spread: "队首高出立即成交 {}bp",
    maker_depth: "可见深度 {} {}，单笔建议不超过 {} {}",
    maker_excluded: "已排除挂单 {}（库存 {}）：{}",
    risks: "风险",
    probe: "建议采集",
    flip: "翻",
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

/// Every field of both catalogues, paired by name.
#[cfg(test)]
fn report_pairs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
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
            "maker_excluded",
            REPORT_ENGLISH.maker_excluded,
            REPORT_CHINESE.maker_excluded,
        ),
        (
            "tier_closed",
            REPORT_ENGLISH.tier_closed,
            REPORT_CHINESE.tier_closed,
        ),
        (
            "tier_theoretical",
            REPORT_ENGLISH.tier_theoretical,
            REPORT_CHINESE.tier_theoretical,
        ),
        (
            "tier_mark_to_market",
            REPORT_ENGLISH.tier_mark_to_market,
            REPORT_CHINESE.tier_mark_to_market,
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
        (
            "size_down_to",
            REPORT_ENGLISH.size_down_to,
            REPORT_CHINESE.size_down_to,
        ),
        ("stranded", REPORT_ENGLISH.stranded, REPORT_CHINESE.stranded),
        (
            "no_cost_basis",
            REPORT_ENGLISH.no_cost_basis,
            REPORT_CHINESE.no_cost_basis,
        ),
        (
            "break_even_at",
            REPORT_ENGLISH.break_even_at,
            REPORT_CHINESE.break_even_at,
        ),
        (
            "nothing_to_convert",
            REPORT_ENGLISH.nothing_to_convert,
            REPORT_CHINESE.nothing_to_convert,
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
            "cannot_stake",
            REPORT_ENGLISH.cannot_stake,
            REPORT_CHINESE.cannot_stake,
        ),
        ("staking", REPORT_ENGLISH.staking, REPORT_CHINESE.staking),
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
            "out_amount",
            REPORT_ENGLISH.out_amount,
            REPORT_CHINESE.out_amount,
        ),
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
            pick(language, "captures too far apart", "各腿抓取时间相差过大")
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

/// Why a listing was kept out of the maker queue math.
#[must_use]
pub const fn maker_exclusion(language: UiLanguage, value: MakerQueueExclusion) -> &'static str {
    match value {
        MakerQueueExclusion::PriceOutlier => pick(language, "price outlier", "价格离群"),
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
        ExecutionRiskFlag::BelowMinimumOutput => {
            pick(language, "below minimum output", "低于最小产出")
        }
        ExecutionRiskFlag::CapacityRoundedToUnit => {
            pick(language, "rounded to a whole unit", "容量取整到整数单位")
        }
        ExecutionRiskFlag::UnknownFee => pick(language, "unknown fee", "手续费未知"),
        ExecutionRiskFlag::PartialRoute => pick(language, "partial route", "路径不完整"),
        ExecutionRiskFlag::ResidualInventory => pick(language, "residual inventory", "有零头库存"),
        ExecutionRiskFlag::MultiHopMaker => pick(language, "multi-hop maker leg", "多跳挂单腿"),
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
        ProbeReason::MissingBridgeQuote => pick(language, "no bridge quote", "缺中转腿报价"),
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
        RadarItemKind::Triangle => pick(language, "triangle", "三角环"),
    }
}

/// Whether a radar row can be acted on now.
#[must_use]
pub const fn radar_category(language: UiLanguage, value: RadarCategory) -> &'static str {
    match value {
        RadarCategory::Executable => pick(language, "executable now", "现在就能成交"),
        RadarCategory::Theoretical => pick(language, "needs a taker", "要等人吃单"),
        RadarCategory::ProbeRequired => pick(language, "capture more first", "先多抓几次"),
    }
}

/// Why a radar row is on the list.
#[must_use]
pub const fn radar_reason(language: UiLanguage, value: RadarReason) -> &'static str {
    match value {
        RadarReason::BetterThanDirect => pick(language, "better than direct", "优于直兑"),
        RadarReason::NoDirectBaseline => pick(language, "no direct baseline", "没有直兑基准"),
        RadarReason::TriangleReturn => pick(language, "triangle return", "三角回环收益"),
        RadarReason::GrossTheoryOnly => pick(language, "gross theory only", "仅理论毛利"),
        RadarReason::ResidualInventory => pick(language, "residual inventory", "有零头库存"),
        RadarReason::MakerReference => pick(language, "maker reference", "挂单参考价"),
        RadarReason::SearchTruncated => pick(language, "search truncated", "搜索被截断"),
        RadarReason::CaptureSkewUnverified => {
            pick(language, "capture gap unverified", "抓取时差未验证")
        }
        RadarReason::CaptureSkewExceeded => {
            pick(language, "captures too far apart", "抓取时差过大")
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
            [RadarItemKind::BestConversion, RadarItemKind::Triangle]
        );
        check!(
            radar_category,
            [
                RadarCategory::Executable,
                RadarCategory::Theoretical,
                RadarCategory::ProbeRequired,
            ]
        );
        check!(
            radar_reason,
            [
                RadarReason::BetterThanDirect,
                RadarReason::NoDirectBaseline,
                RadarReason::TriangleReturn,
                RadarReason::GrossTheoryOnly,
                RadarReason::ResidualInventory,
                RadarReason::MakerReference,
                RadarReason::SearchTruncated,
                RadarReason::CaptureSkewUnverified,
                RadarReason::CaptureSkewExceeded,
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
