//! Versioned, crash-safe JSON settings store.
//!
//! Reading is lenient: a missing file yields defaults; a malformed file is moved
//! aside as `settings.json.corrupt-<stamp>` and reported, so the next save can
//! never clobber it. Writing is strict and atomic: pretty
//! JSON to a sibling temp file, fsync, then rename. A file written by a newer
//! schema puts the store into read-only mode instead of being clobbered.
//! (Same posture as POE Alarm's settings store, with a fresh v1 schema.)

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ptt_core::{ContentLanguage, Game, ProfileId};
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
const APP_DIR_NAME: &str = "PoeTradeTracker";
const SETTINGS_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiLanguage {
    #[default]
    English,
    Chinese,
}

/// 界面配色。深色是定稿的那一套,浅色是同一批语义整个翻过来的对位。
///
/// 存在设置里而不是跟着系统走:这个工具是叠在游戏窗口边上用的,亮暗该由
/// 用户按当时的游戏画面定,不该由操作系统的日夜设置替他定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiTheme {
    #[default]
    Dark,
    Light,
}

/// A desktop-pixel rectangle (virtual-screen coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Per-profile calibration. Keyed by `ProfileId` display form (e.g.
/// `poe2-zh-TW`); regions are tied to the desktop resolution they were drawn
/// on. The user calibrates three text-only regions (icons deliberately
/// excluded — they degrade OCR): the "I need" name, the "I have" name, and
/// the order-tables area holding at most 12 ratio rows (6 available + 6
/// competing, sometimes fewer). The exchange panel shifts sideways when the
/// stash or character panels are open; calibration assumes the centered
/// default position, and a shifted panel simply fails the anchor/identity
/// gates and is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProfileSettings {
    #[serde(default)]
    pub need_name_region: Option<Region>,
    #[serde(default)]
    pub have_name_region: Option<Region>,
    #[serde(default)]
    pub tables_region: Option<Region>,
}

impl ProfileSettings {
    /// All three regions framed. Anything less and the recognition route
    /// falls back to the built-in 2560×1440 coordinates, which on any other
    /// desktop means every frame is skipped without saying why.
    pub fn is_calibrated(&self) -> bool {
        self.need_name_region.is_some()
            && self.have_name_region.is_some()
            && self.tables_region.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    #[serde(default = "default_toggle_watch")]
    pub toggle_watch: String,
    #[serde(default = "default_toggle_hud")]
    pub toggle_hud: String,
    #[serde(default = "default_manual_capture")]
    pub manual_capture: String,
}

fn default_toggle_watch() -> String {
    "Ctrl+Alt+F10".to_string()
}
fn default_toggle_hud() -> String {
    "Alt+F11".to_string()
}
fn default_manual_capture() -> String {
    "Alt+F12".to_string()
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            toggle_watch: default_toggle_watch(),
            toggle_hud: default_toggle_hud(),
            manual_capture: default_manual_capture(),
        }
    }
}

/// Per-game algorithm tuning, keyed by [`Game::as_str`] in [`AppSettings`].
///
/// Settings hold plain strings and integers only — no domain types. The
/// conversion into `FreshnessPolicy`, `RiskThresholds`, asset ids and so on
/// happens where they are consumed (ptt-runtime), behind each type's own
/// validity gate, so a hand-edited bad value degrades to the shipped default
/// with a visible report line rather than poisoning a computation or making
/// the whole settings file unreadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketTuning {
    /// Settlement currencies (主要结算通货), in catalog slug form. These are
    /// the currencies every arbitrage cycle starts from — and therefore closes
    /// back into — and the first one is the anchor everything else is valued
    /// against. They must be the most liquid assets in the league.
    #[serde(default = "default_settlement_assets")]
    pub settlement_assets: Vec<String>,
    /// The settlement currency everything else is valued against (结算锚).
    ///
    /// 单独存,不用列表顺序表达:设置页的 chips 按字母序排,锚要是隐含在
    /// "列表第一个"里,加一个字母靠前的通货就会悄悄换锚。`None` 或指向
    /// 不在结算列表里的通货时,回落到列表第一个——赛季主流换了,用户换
    /// 锚,所有相对价值跟着走。
    #[serde(default)]
    pub anchor_asset: Option<String>,
    /// Focus targets: currencies the user actively trades in and out of.
    /// Empty means "every asset seen in the book", today's behavior.
    #[serde(default)]
    pub focus_assets: Vec<String>,
    /// Currencies routes may pass through but never end on.
    #[serde(default)]
    pub bridge_assets: Vec<String>,
    /// Currencies tracked for price only, excluded from every route.
    #[serde(default)]
    pub watch_only_assets: Vec<String>,
    /// Suggestions the user has dismissed, and how prominent each was when
    /// they did.
    ///
    /// A capture is not always an intention: a few frames taken while testing
    /// are enough to put a currency at the top of "you keep flipping this",
    /// and with no way to say otherwise it stays there. Separate from
    /// `watch_only_assets`, which is a statement about routing rather than
    /// about being asked again.
    ///
    /// The count is what keeps a dismissal from being a life sentence — see
    /// [`IgnoredSuggestion`].
    #[serde(default)]
    pub ignored_suggestions: Vec<IgnoredSuggestion>,
    /// Pairs the user refuses to capture (「这一对我不抓」).
    ///
    /// 三个列表,别合并:隐藏 = 不看这行估值(`hidden_assets`),忽略建议 =
    /// 别再推荐我关注这个通货(`ignored_suggestions`),忽略去抓 = 这一对
    /// 我不抓(这里)。三件事含义不同,合成一个会静默改变引擎允许做的事。
    /// 永久生效——不会因为又缺一次数据自己冒回来;唯一的后悔药是关注列表
    /// 页底部的「查看并恢复」。
    #[serde(default)]
    pub ignored_probes: Vec<IgnoredProbe>,
    /// Currencies whose valuation row the watchlist stops drawing.
    ///
    /// Display only: a hidden currency is still priced, still arbitraged and
    /// still suggestible. The watchlist lists every currency the window has
    /// seen, which after a day of capturing is a wall; this is the "I do not
    /// need this row" for the rows that are only there because the watcher
    /// walked past them once.
    #[serde(default)]
    pub hidden_assets: Vec<String>,
    /// Whether a route may pass through a focus target.
    ///
    /// Bridges say "route through me, I am not a destination". Nothing said
    /// the reverse — that a destination cannot also be passed through — and
    /// with it off, a user who lists everything they trade has told the search
    /// it may only ever route through the settlement currencies. Default on;
    /// the search is bounded by `radar.max_total_expansions` either way and
    /// reports when that bound is what stopped it.
    #[serde(default = "default_route_through_targets")]
    pub route_through_targets: bool,
    #[serde(default)]
    pub freshness: FreshnessTuning,
    #[serde(default)]
    pub convert: ConvertTuning,
    #[serde(default)]
    pub radar: RadarTuning,
    #[serde(default)]
    pub risk: RiskTuning,
    #[serde(default)]
    pub analytics: AnalyticsTuning,
    #[serde(default)]
    pub exchange: ExchangeTuning,
    /// How much history each report page loads, in hours. Must reach past the
    /// red freshness band: a window shorter than `usable_seconds` can never
    /// even load the data the yellow and red lights exist to warn about.
    #[serde(default = "default_report_window_hours")]
    pub report_window_hours: u64,
}

/// A dismissed suggestion.
///
/// Dismissing is "not this, not now", not "never". A currency whose evidence
/// doubles what it was when it was waved away has become a different fact
/// about the market, and it is offered again; a mis-click therefore costs
/// one more click later rather than burying the currency for good. Each
/// dismissal records the prominence afresh, so the bar rises rather than
/// resetting.
///
/// The prominence is whatever the suggestion itself is ranked by — since the
/// evidence rewrite (P10) that is anchor-valued buy pressure in the report
/// window, not a capture count. The serde field keeps its historical name so
/// older settings files read cleanly; an old snapshot-count value just makes
/// the currency due again sooner, which errs on the side of asking.
/// One pair the user will not capture, in domain id spelling.
///
/// Directed, because probes are directed: ignoring 混沌石→高階混沌石 says
/// nothing about the reverse reading.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredProbe {
    pub from_asset_id: String,
    pub to_asset_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredSuggestion {
    pub asset_id: String,
    /// The suggestion's prominence at dismissal (buy pressure, anchor units).
    #[serde(default)]
    pub snapshots_when_ignored: u64,
}

impl IgnoredSuggestion {
    /// Whether a currency at this prominence should be offered again.
    #[must_use]
    pub const fn is_due_again(&self, prominence_now: u64) -> bool {
        let bar = match self.snapshots_when_ignored.checked_mul(2) {
            // Dismissed at nothing, which only a hand-edited file produces:
            // the next capture asks again rather than never.
            Some(0) => 1,
            // Dismissed at an absurd count. The bar is simply out of reach,
            // which is the honest reading of what the file says.
            None => u64::MAX,
            Some(bar) => bar,
        };
        prominence_now >= bar
    }
}

fn default_settlement_assets() -> Vec<String> {
    vec!["divine-orb".to_owned(), "chaos-orb".to_owned()]
}

fn default_report_window_hours() -> u64 {
    24
}

const fn default_route_through_targets() -> bool {
    true
}

impl MarketTuning {
    /// Whether the user has refused to capture this pair.
    #[must_use]
    pub fn is_probe_ignored(&self, from_asset_id: &str, to_asset_id: &str) -> bool {
        self.ignored_probes
            .iter()
            .any(|held| held.from_asset_id == from_asset_id && held.to_asset_id == to_asset_id)
    }
}

impl Default for MarketTuning {
    fn default() -> Self {
        Self {
            settlement_assets: default_settlement_assets(),
            anchor_asset: None,
            focus_assets: Vec::new(),
            bridge_assets: Vec::new(),
            watch_only_assets: Vec::new(),
            ignored_suggestions: Vec::new(),
            ignored_probes: Vec::new(),
            hidden_assets: Vec::new(),
            route_through_targets: default_route_through_targets(),
            freshness: FreshnessTuning::default(),
            convert: ConvertTuning::default(),
            radar: RadarTuning::default(),
            risk: RiskTuning::default(),
            analytics: AnalyticsTuning::default(),
            exchange: ExchangeTuning::default(),
            report_window_hours: default_report_window_hours(),
        }
    }
}

/// The freshness traffic light, in seconds of data age. Green (fresh) is
/// trusted as-is; yellow (usable) means verify the rate in game before acting
/// on it; red (stale, and archived past `stale_seconds`) is excluded from
/// execution by default and asks for a recapture. Values follow the league's
/// capture rhythm, not wall-clock precision — the enforcement (strict
/// ordering, non-zero) lives in `FreshnessPolicy::try_new` at the consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessTuning {
    #[serde(default = "default_fresh_seconds")]
    pub fresh_seconds: u64,
    #[serde(default = "default_usable_seconds")]
    pub usable_seconds: u64,
    #[serde(default = "default_stale_seconds")]
    pub stale_seconds: u64,
    /// Maximum capture-time spread between the legs of one route before it is
    /// flagged as spanning different market moments. Half the green window:
    /// with green at two hours, the old 600s default would flag nearly every
    /// multi-leg route built from separately captured pairs.
    #[serde(default = "default_capture_skew_seconds")]
    pub capture_skew_seconds: u64,
}

fn default_fresh_seconds() -> u64 {
    2 * 60 * 60
}
fn default_usable_seconds() -> u64 {
    6 * 60 * 60
}
fn default_stale_seconds() -> u64 {
    24 * 60 * 60
}
fn default_capture_skew_seconds() -> u64 {
    60 * 60
}

impl Default for FreshnessTuning {
    fn default() -> Self {
        Self {
            fresh_seconds: default_fresh_seconds(),
            usable_seconds: default_usable_seconds(),
            stale_seconds: default_stale_seconds(),
            capture_skew_seconds: default_capture_skew_seconds(),
        }
    }
}

/// Knobs for the Convert page ("I hold X and want Y").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertTuning {
    /// The sizes priced when the user has not typed a holding, in whole units.
    #[serde(default = "default_convert_sizes")]
    pub sizes: Vec<u64>,
    /// Route search depth. The engine accepts 1..=4; out-of-range values fail
    /// its own validation and the report says so.
    #[serde(default = "default_max_hops")]
    pub max_hops: u64,
}

fn default_convert_sizes() -> Vec<u64> {
    vec![1, 10, 100]
}
fn default_max_hops() -> u64 {
    3
}

impl Default for ConvertTuning {
    fn default() -> Self {
        Self {
            sizes: default_convert_sizes(),
            max_hops: default_max_hops(),
        }
    }
}

/// Knobs for the opportunity radar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadarTuning {
    /// Total node-expansion budget shared across the whole scan.
    #[serde(default = "default_radar_expansions")]
    pub max_total_expansions: u64,
    /// How many ranked items the page shows.
    #[serde(default = "default_radar_results")]
    pub max_results: u64,
    /// Opportunities below this margin are reported as rejections, not items.
    #[serde(default = "default_minimum_profit_basis_points")]
    pub minimum_profit_basis_points: u64,
    /// The most currencies one closed loop may pass through.
    ///
    /// Six rather than four because the graph, not the bound, is what limits
    /// this: a captured book is hub-and-spoke -- on a live one, 78% of pairs
    /// touch a settlement currency and 43 of 60 assets have two onward pairs
    /// or fewer -- so a loop has to alternate between the handful of hubs and
    /// cannot keep going. Measured on that book, lengths five, six and seven
    /// walk exactly the same 420 cycles as four does, in the same 7ms. The
    /// headroom costs nothing today and is there for a denser book.
    #[serde(default = "default_radar_max_cycle_length")]
    pub max_cycle_length: u64,
}

fn default_radar_max_cycle_length() -> u64 {
    6
}

fn default_radar_expansions() -> u64 {
    60_000
}
fn default_radar_results() -> u64 {
    12
}
fn default_minimum_profit_basis_points() -> u64 {
    100
}

impl Default for RadarTuning {
    fn default() -> Self {
        Self {
            max_total_expansions: default_radar_expansions(),
            max_results: default_radar_results(),
            minimum_profit_basis_points: default_minimum_profit_basis_points(),
            max_cycle_length: default_radar_max_cycle_length(),
        }
    }
}

/// Liquidity and outlier thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskTuning {
    /// Visible depth below this stock count is flagged as thin liquidity.
    #[serde(default = "default_thin_liquidity_stock")]
    pub thin_liquidity_stock: u64,
    /// A listing whose rate differs from its side's baseline by more than this
    /// factor is a price outlier. The book enforces its own minimum of 2.
    #[serde(default = "default_outlier_factor")]
    pub top_book_outlier_factor: u64,
    /// A latest rate this far from the median (basis points) is a spike/drop.
    #[serde(default = "default_spike_basis_points")]
    pub spike_basis_points: u64,
    /// ...and this far is high severity.
    #[serde(default = "default_severe_spike_basis_points")]
    pub severe_spike_basis_points: u64,
    /// A maker/taker spread wider than this (basis points) is an anomaly.
    #[serde(default = "default_wide_spread_basis_points")]
    pub wide_spread_basis_points: u64,
    #[serde(default = "default_severe_spread_basis_points")]
    pub severe_spread_basis_points: u64,
}

fn default_thin_liquidity_stock() -> u64 {
    100
}
fn default_outlier_factor() -> u64 {
    3
}
fn default_spike_basis_points() -> u64 {
    2_000
}
fn default_severe_spike_basis_points() -> u64 {
    5_000
}
fn default_wide_spread_basis_points() -> u64 {
    1_000
}
fn default_severe_spread_basis_points() -> u64 {
    2_000
}

impl Default for RiskTuning {
    fn default() -> Self {
        Self {
            thin_liquidity_stock: default_thin_liquidity_stock(),
            top_book_outlier_factor: default_outlier_factor(),
            spike_basis_points: default_spike_basis_points(),
            severe_spike_basis_points: default_severe_spike_basis_points(),
            wide_spread_basis_points: default_wide_spread_basis_points(),
            severe_spread_basis_points: default_severe_spread_basis_points(),
        }
    }
}

/// Knobs for the market analytics page (value trends, supply/demand pressure,
/// anchor health) and the raw-data retention that feeds it. All first-pass
/// defaults awaiting live calibration; consumers validate before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyticsTuning {
    /// Days in the "recent" trend window (the numerator of a trend verdict).
    #[serde(default = "default_trend_recent_days")]
    pub trend_recent_days: u64,
    /// Days in the baseline trend window the recent one is compared against.
    #[serde(default = "default_trend_window_days")]
    pub trend_window_days: u64,
    /// Percentage of trending assets that must move the same way against the
    /// primary settlement currency before the move is read as the anchor
    /// itself inflating or deflating rather than the market moving.
    #[serde(default = "default_breadth_threshold_percent")]
    pub breadth_threshold_percent: u64,
    /// Market-relative trend beyond ±this many basis points is a verdict
    /// (appreciating / depreciating); inside it, holding.
    #[serde(default = "default_verdict_threshold_bps")]
    pub verdict_threshold_bps: u64,
    /// Demand at or above this percent of supply (or vice versa) classifies a
    /// currency as scarce (or oversupplied). 300 = a 3x imbalance.
    #[serde(default = "default_scarce_ratio_percent")]
    pub scarce_ratio_percent: u64,
    /// Below this many anchor units on both sides, a market is quiet rather
    /// than imbalanced — too little on either side to read a direction.
    #[serde(default = "default_quiet_floor_anchor_units")]
    pub quiet_floor_anchor_units: u64,
    /// Thin-liquidity threshold as a percent of a currency's own median daily
    /// circulation, replacing the global stock constant once history exists.
    #[serde(default = "default_thin_norm_percent")]
    pub thin_norm_percent: u64,
    /// Days of raw captures kept once their daily rollups are persisted.
    /// 0 disables pruning entirely (the shipped default): nothing is ever
    /// deleted unless the user turns retention on or purges from Settings.
    #[serde(default)]
    pub raw_retention_days: u64,
}

fn default_trend_recent_days() -> u64 {
    2
}
fn default_trend_window_days() -> u64 {
    7
}
fn default_breadth_threshold_percent() -> u64 {
    70
}
fn default_verdict_threshold_bps() -> u64 {
    500
}
fn default_scarce_ratio_percent() -> u64 {
    300
}
fn default_quiet_floor_anchor_units() -> u64 {
    10
}
fn default_thin_norm_percent() -> u64 {
    25
}

impl Default for AnalyticsTuning {
    fn default() -> Self {
        Self {
            trend_recent_days: default_trend_recent_days(),
            trend_window_days: default_trend_window_days(),
            breadth_threshold_percent: default_breadth_threshold_percent(),
            verdict_threshold_bps: default_verdict_threshold_bps(),
            scarce_ratio_percent: default_scarce_ratio_percent(),
            quiet_floor_anchor_units: default_quiet_floor_anchor_units(),
            thin_norm_percent: default_thin_norm_percent(),
            raw_retention_days: 0,
        }
    }
}

/// 官方通货历史 API（浮士德小时线）的接入配置。
///
/// 和 OCR 侧一个字段都不共享：API 数据按 GGG 的真实联赛名分账，
/// OCR 侧的 "live-league" 常量不动，两个世界只靠 game + 小时时间戳对账。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeTuning {
    /// GGG 的联赛英文名（如 "Runes of Aldur"）。**空字符串 = 不抓取**——
    /// 这是总开关。换赛季改这里，新联赛的水位自动从零开始回补。
    #[serde(default)]
    pub league: String,
    /// 首次回补往回拉多少天。之后的增量抓取与此无关（水位接着走）。
    #[serde(default = "default_exchange_backfill_days")]
    pub backfill_days: u64,
    /// 小时线折成日线后再保留多少天，过期整天清理。0 = 不清理。
    /// 默认开着（14 天）而不是像 raw_retention_days 那样默认 0：
    /// 小时行是可再生的（CDN 一年内可重抓），日线才是唯一副本。
    #[serde(default = "default_exchange_hour_retention_days")]
    pub hour_retention_days: u64,
    /// 交易所页涨跌列的天数（"N 天涨跌"）。表头点击轮换，
    /// 数据不足 N 天时计算自动退到最早那天——上限受已有数据限制。
    #[serde(default = "default_exchange_trend_days")]
    pub trend_days: u64,
    /// 交易所页"截至哪天看"（YYYY-MM-DD；空 = 现在）。设了就是历史视角：
    /// 涨跌与走势的终点钉在那天，小时级的列（最新价/成交/深度）留空。
    /// 存成设置而不是页面临时状态：页面数据是后台按设置算的，
    /// 临时状态带不过去。
    #[serde(default)]
    pub as_of_day: String,
}

fn default_exchange_backfill_days() -> u64 {
    14
}
fn default_exchange_hour_retention_days() -> u64 {
    14
}
fn default_exchange_trend_days() -> u64 {
    7
}

impl Default for ExchangeTuning {
    fn default() -> Self {
        Self {
            league: String::new(),
            backfill_days: default_exchange_backfill_days(),
            hour_retention_days: default_exchange_hour_retention_days(),
            trend_days: default_exchange_trend_days(),
            as_of_day: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    pub schema_version: u32,
    #[serde(default)]
    pub ui_language: UiLanguage,
    #[serde(default)]
    pub ui_theme: UiTheme,
    #[serde(default = "default_active_profile")]
    pub active_profile: ProfileId,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileSettings>,
    #[serde(default)]
    pub hotkeys: Hotkeys,
    /// Per-game algorithm tuning, keyed by [`Game::as_str`] ("poe1"/"poe2").
    /// Absent games use [`MarketTuning::default`].
    #[serde(default)]
    pub market: BTreeMap<String, MarketTuning>,
    /// The overlay card:档位、不透明度、摆放(§4 浮窗定稿)。
    #[serde(default)]
    pub hud: HudSettings,
}

/// 浮窗两档:迷你只答"识别还活着吗",展开加上十二行报价原样。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HudTier {
    Mini,
    #[default]
    Expanded,
}

/// 浮窗摆放。位置存相对比例(千分位整数)不存像素:换分辨率/换显示器
/// 不会飞到屏幕外(`resolve_hud_position` 已按比例换算)。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum HudPlacementSetting {
    #[default]
    Automatic,
    Manual {
        relative_x_permille: u32,
        relative_y_permille: u32,
    },
}

/// The overlay card's persisted knobs (§4 浮窗:现在只存了热键的历史欠账)。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HudSettings {
    #[serde(default)]
    pub tier: HudTier,
    /// 60–100,步长 5。下限 60 是因为再低 10px 的灰字就糊了(`3b`)。
    #[serde(default = "default_hud_opacity")]
    pub opacity_percent: u8,
    #[serde(default)]
    pub placement: HudPlacementSetting,
}

const fn default_hud_opacity() -> u8 {
    85
}

impl Default for HudSettings {
    fn default() -> Self {
        Self {
            tier: HudTier::default(),
            opacity_percent: default_hud_opacity(),
            placement: HudPlacementSetting::default(),
        }
    }
}

impl HudSettings {
    /// The opacity a hand-edited file actually gets: clamped into [60, 100]
    /// and snapped to the 5% step.
    #[must_use]
    pub fn clamped_opacity(&self) -> u8 {
        (self.opacity_percent.clamp(60, 100) / 5) * 5
    }

    /// `LWA_ALPHA` 的 0–255 值。
    #[must_use]
    pub fn alpha(&self) -> u8 {
        let percent = u16::from(self.clamped_opacity());
        u8::try_from(percent * 255 / 100).unwrap_or(216)
    }
}

fn default_active_profile() -> ProfileId {
    ProfileId::new(Game::Poe2, ContentLanguage::TraditionalChinese)
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            ui_language: UiLanguage::default(),
            ui_theme: UiTheme::default(),
            active_profile: default_active_profile(),
            profiles: BTreeMap::new(),
            hotkeys: Hotkeys::default(),
            market: BTreeMap::new(),
            hud: HudSettings::default(),
        }
    }
}

impl AppSettings {
    pub fn profile_mut(&mut self, profile: ProfileId) -> &mut ProfileSettings {
        self.profiles.entry(profile.to_string()).or_default()
    }

    pub fn profile(&self, profile: ProfileId) -> Option<&ProfileSettings> {
        self.profiles.get(&profile.to_string())
    }

    /// The tuning for one game, defaults where the file has none. Returns a
    /// clone: tuning is read once per report build, not per frame, and the
    /// caller must not observe later edits mid-computation.
    pub fn market_tuning(&self, game: Game) -> MarketTuning {
        self.market.get(game.as_str()).cloned().unwrap_or_default()
    }

    pub fn market_tuning_mut(&mut self, game: Game) -> &mut MarketTuning {
        self.market.entry(game.as_str().to_owned()).or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// File parsed at a supported schema.
    Loaded,
    /// No file / unreadable / malformed — defaults returned.
    Defaults,
    /// File written by a newer schema — defaults returned, saving refused.
    FutureSchemaReadOnly { detected: u32 },
    /// File existed but could not be parsed. It was moved aside to `backup`
    /// so the next save cannot silently overwrite a file that may still be
    /// mostly recoverable by hand; defaults returned.
    Corrupt { backup: PathBuf, reason: String },
}

#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub settings: AppSettings,
    pub status: LoadStatus,
}

#[derive(Debug)]
pub enum SaveError {
    SchemaTooNew { detected: u32, supported: u32 },
    Io(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::SchemaTooNew {
                detected,
                supported,
            } => write!(
                f,
                "settings on disk use schema {detected}, newer than supported {supported}"
            ),
            SaveError::Io(error) => write!(f, "settings write failed: {error}"),
        }
    }
}

impl std::error::Error for SaveError {}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Production location: `<local_app_data>\PoeTradeTracker\settings.json`.
    /// Injectable root so tests never touch the real profile.
    pub fn release_default_from(local_app_data: &Path) -> Self {
        Self {
            path: local_app_data.join(APP_DIR_NAME).join(SETTINGS_FILE_NAME),
        }
    }

    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> LoadedSettings {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return LoadedSettings {
                settings: AppSettings::default(),
                status: LoadStatus::Defaults,
            };
        };
        match serde_json::from_str::<AppSettings>(&raw) {
            Ok(settings) if settings.schema_version <= CURRENT_SCHEMA_VERSION => LoadedSettings {
                settings,
                status: LoadStatus::Loaded,
            },
            Ok(settings) => LoadedSettings {
                status: LoadStatus::FutureSchemaReadOnly {
                    detected: settings.schema_version,
                },
                settings: AppSettings::default(),
            },
            Err(error) => {
                // Tolerate a partially-known file: retry just the version gate so a
                // future schema is still detected as such rather than "malformed".
                if let Some(detected) = detect_schema_version(&raw)
                    && detected > CURRENT_SCHEMA_VERSION
                {
                    return LoadedSettings {
                        settings: AppSettings::default(),
                        status: LoadStatus::FutureSchemaReadOnly { detected },
                    };
                }
                // A file that exists but does not parse is not "no file": move
                // it aside before anyone saves, so the broken copy survives.
                let backup = self.move_corrupt_aside();
                LoadedSettings {
                    settings: AppSettings::default(),
                    status: LoadStatus::Corrupt {
                        backup,
                        reason: error.to_string(),
                    },
                }
            }
        }
    }

    /// Renames the unreadable file to `settings.json.corrupt-<unix seconds>`.
    /// If the rename itself fails the original path is returned so the
    /// message the user sees still points at a real file.
    fn move_corrupt_aside(&self) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let file_name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| SETTINGS_FILE_NAME.to_owned());
        let backup = self
            .path
            .with_file_name(format!("{file_name}.corrupt-{stamp}"));
        match fs::rename(&self.path, &backup) {
            Ok(()) => backup,
            Err(_) => self.path.clone(),
        }
    }

    /// Atomic save. Re-checks the on-disk schema immediately before replacing so
    /// a newer process that wrote the file mid-flight still wins.
    pub fn save(&self, settings: &AppSettings) -> Result<(), SaveError> {
        let mut normalized = settings.clone();
        normalized.schema_version = CURRENT_SCHEMA_VERSION;
        let body =
            serde_json::to_vec_pretty(&normalized).expect("settings model always serializes");

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(SaveError::Io)?;
        }
        self.refuse_future_schema()?;

        let temp_path = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(SaveError::Io)?;
            file.write_all(&body).map_err(SaveError::Io)?;
            file.sync_all().map_err(SaveError::Io)?;
        }
        if let Err(error) = self.refuse_future_schema() {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        fs::rename(&temp_path, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temp_path);
            SaveError::Io(error)
        })
    }

    fn refuse_future_schema(&self) -> Result<(), SaveError> {
        if let Ok(raw) = fs::read_to_string(&self.path)
            && let Some(detected) = detect_schema_version(&raw)
            && detected > CURRENT_SCHEMA_VERSION
        {
            return Err(SaveError::SchemaTooNew {
                detected,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

fn detect_schema_version(raw: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct VersionOnly {
        schema_version: u32,
    }
    serde_json::from_str::<VersionOnly>(raw)
        .ok()
        .map(|value| value.schema_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> SettingsStore {
        let dir = std::env::temp_dir()
            .join("ptt-settings-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        SettingsStore::at_path(dir.join(SETTINGS_FILE_NAME))
    }

    #[test]
    fn missing_file_yields_defaults() {
        let store = temp_store("missing");
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Defaults);
        assert_eq!(loaded.settings, AppSettings::default());
    }

    #[test]
    fn a_profile_is_calibrated_only_when_all_three_regions_are_framed() {
        let region = Region {
            x: 1,
            y: 1,
            width: 10,
            height: 10,
        };
        let mut profile = ProfileSettings::default();
        assert!(!profile.is_calibrated());
        profile.need_name_region = Some(region);
        profile.have_name_region = Some(region);
        assert!(!profile.is_calibrated(), "two of three is still not framed");
        profile.tables_region = Some(region);
        assert!(profile.is_calibrated());
    }

    #[test]
    fn corrupt_file_is_backed_up_and_reported() {
        let store = temp_store("corrupt");
        fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
        fs::write(store.path(), "{ this is not json").expect("write");
        let loaded = store.load();
        let LoadStatus::Corrupt { backup, reason } = loaded.status else {
            panic!("expected Corrupt, got {:?}", loaded.status);
        };
        assert!(backup.exists(), "backup must exist at {}", backup.display());
        assert!(!store.path().exists(), "the broken file must be moved away");
        assert!(!reason.is_empty());
        assert_eq!(loaded.settings, AppSettings::default());
    }

    #[test]
    fn round_trip_preserves_settings() {
        let store = temp_store("round-trip");
        let mut settings = AppSettings {
            ui_language: UiLanguage::Chinese,
            ui_theme: UiTheme::Light,
            ..AppSettings::default()
        };
        settings.profile_mut(default_active_profile()).tables_region = Some(Region {
            x: 458,
            y: 60,
            width: 720,
            height: 620,
        });
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings, settings);
    }

    #[test]
    fn malformed_file_is_reported_and_the_store_stays_writable() {
        let store = temp_store("malformed");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"{not json").unwrap();
        assert!(matches!(store.load().status, LoadStatus::Corrupt { .. }));
        store.save(&AppSettings::default()).unwrap();
        assert_eq!(store.load().status, LoadStatus::Loaded);
    }

    /// A settings file from before P7 has no `market` key at all; it must
    /// load as `Loaded` (not fall back to defaults wholesale) and hand out
    /// the shipped tuning.
    #[test]
    fn a_pre_p7_file_loads_and_yields_default_tuning() {
        let store = temp_store("pre-p7");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            br#"{"schema_version": 1, "ui_language": "Chinese"}"#,
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings.ui_language, UiLanguage::Chinese);
        // 配色是后加的键。老文件里没有它,整份设置也不许因此读不出来——
        // 缺一个键就回落成全盘默认值,等于用户的标定和热键一起没了。
        assert_eq!(loaded.settings.ui_theme, UiTheme::Dark);
        let tuning = loaded.settings.market_tuning(Game::Poe2);
        assert_eq!(tuning, MarketTuning::default());
        assert_eq!(tuning.settlement_assets, ["divine-orb", "chaos-orb"]);
        assert_eq!(tuning.freshness.fresh_seconds, 7200);
        assert_eq!(tuning.freshness.usable_seconds, 21600);
        assert_eq!(tuning.freshness.capture_skew_seconds, 3600);
        assert_eq!(tuning.report_window_hours, 24);
    }

    /// A partially specified tuning keeps its stated fields and defaults the
    /// rest — hand-editing one number must not require writing them all.
    #[test]
    fn a_partial_tuning_defaults_what_it_does_not_state() {
        let store = temp_store("partial-tuning");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            br#"{
                "schema_version": 1,
                "market": {
                    "poe2": {
                        "settlement_assets": ["chaos-orb"],
                        "freshness": { "fresh_seconds": 3600 }
                    }
                }
            }"#,
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        let tuning = loaded.settings.market_tuning(Game::Poe2);
        assert_eq!(tuning.settlement_assets, ["chaos-orb"]);
        assert_eq!(tuning.freshness.fresh_seconds, 3600);
        assert_eq!(tuning.freshness.usable_seconds, 21600);
        assert_eq!(tuning.convert.sizes, [1, 10, 100]);
        assert_eq!(tuning.risk.thin_liquidity_stock, 100);
        // The other game is untouched by poe2's entry.
        assert_eq!(
            loaded.settings.market_tuning(Game::Poe1),
            MarketTuning::default()
        );
    }

    /// Customized tuning survives a save/load round trip bit-for-bit.
    #[test]
    fn tuning_round_trips() {
        let store = temp_store("tuning-round-trip");
        let mut settings = AppSettings::default();
        {
            let tuning = settings.market_tuning_mut(Game::Poe2);
            tuning.settlement_assets = vec![
                "divine-orb".to_owned(),
                "chaos-orb".to_owned(),
                "exalted-orb".to_owned(),
            ];
            tuning.focus_assets = vec!["perfect-chaos-orb".to_owned()];
            tuning.freshness.fresh_seconds = 1800;
            tuning.report_window_hours = 48;
        }
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.settings, settings);
    }

    #[test]
    fn future_schema_is_read_only() {
        let store = temp_store("future");
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), br#"{"schema_version": 99}"#).unwrap();
        match store.load().status {
            LoadStatus::FutureSchemaReadOnly { detected } => assert_eq!(detected, 99),
            other => panic!("expected read-only, got {other:?}"),
        }
        assert!(matches!(
            store.save(&AppSettings::default()),
            Err(SaveError::SchemaTooNew { detected: 99, .. })
        ));
    }
}

#[cfg(test)]
mod ignored_suggestion_tests {
    use super::IgnoredSuggestion;

    fn dismissed_at(count: u64) -> IgnoredSuggestion {
        IgnoredSuggestion {
            asset_id: "omen-of-light".to_owned(),
            snapshots_when_ignored: count,
        }
    }

    /// Dismissing is "not now". A currency that becomes twice the fact it was
    /// is a different answer to the same question, and gets asked again.
    #[test]
    fn a_dismissal_lapses_once_the_currency_doubles() {
        let ignored = dismissed_at(5);
        assert!(!ignored.is_due_again(5));
        assert!(!ignored.is_due_again(9));
        assert!(ignored.is_due_again(10));
    }

    /// A dismissal recorded at nothing must not silence the currency forever.
    /// Only a hand-edited file can record one, and it must not bury the
    /// currency: the next capture asks again.
    #[test]
    fn a_zero_count_is_not_a_life_sentence() {
        assert!(!dismissed_at(0).is_due_again(0));
        assert!(dismissed_at(0).is_due_again(1));
    }

    /// The bar cannot overflow into being trivially met.
    #[test]
    fn an_absurd_count_does_not_wrap() {
        assert!(!dismissed_at(u64::MAX).is_due_again(1));
    }
}
