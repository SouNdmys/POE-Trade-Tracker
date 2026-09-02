//! Root view: status strip + monitor content (last book, opportunities,
//! skip histogram). P3 skeleton — layout only, visuals iterate later.

mod hud;
pub mod pages;
// 自更新整条路都挂在 `cfg(windows)` 上(见 `crate::update`),它在界面这一侧的
// 状态机跟着一起门控。
#[cfg(windows)]
mod updater;
// 交易所历史同步和自更新同理:网络依赖全在 windows target 下。
#[cfg(windows)]
mod exchange_sync;

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::StyledExt as _;

use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, breathing_dot, button, hairline_soft, mono, panel, panel_header,
    spaced,
};

#[cfg(windows)]
use crate::backend::{Backend, HotkeyRegistration, ShellMsg, UiEvent, spawn_hotkey_thread};

const LOG_CAPACITY: usize = 120;

/// Where the overlay card sits, and how big it is.
///
/// Top-left rather than centred: the currency panel occupies the middle of
/// the screen, which is exactly what the card must not cover.
const HUD_ORIGIN: (i32, i32) = (24, 24);
/// 摆放模式顶条高度;必须和平台侧 `PLACEMENT_BAR_H` 一致,否则顶条会
/// 挤掉卡片最底一行(高度算在这里,画在平台侧)。
#[cfg(windows)]
const HUD_PLACEMENT_BAR: i32 = 22;

/// §4 定稿的两档尺寸。
///
/// 展开 210 = 头行 26 + 线 1 + 两栏 137(上边距 6 + 栏头 16 + 6×18 行 +
/// 下边距 4 - 1) + 线 1 + 结论 24 + 线 1 + 待抓 20:左右两栏只要 6 行,
/// 高度比上下排砍掉三分之一,遮住的游戏画面更少。迷你 88 = 内边距 5+5 +
/// 状态 20 + 通货对 20 + 结论 20 + 待抓 18。
const HUD_SIZE_MINI: (i32, i32) = (260, 88);
const HUD_SIZE_EXPANDED: (i32, i32) = (420, 210);
/// 待抓队列空了整条消失,浮窗自动矮这么多。
const HUD_PROBE_STRIP: i32 = 20;

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
    Analytics,
    Exchange,
    Settings,
}

impl Page {
    // 用户定的导航序:按"每天先看什么"排——先看市场活没活(监视器),再看行情
    // (市场分析/关注列表),再找机会(雷达/兑换),历史和一次性的校准、设置沉底。
    const ALL: [Self; 9] = [
        Self::Monitor,
        Self::Analytics,
        // 交易所紧跟市场分析:同属"看行情",一个读 OCR 的账,一个读官方的账。
        Self::Exchange,
        Self::Watchlist,
        Self::Opportunities,
        Self::Convert,
        Self::History,
        Self::Calibrate,
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
            Self::Analytics => text.page_analytics,
            Self::Exchange => text.page_exchange,
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
            | Self::Analytics
            | Self::Exchange
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
            | Self::History
            | Self::Analytics
            | Self::Exchange => true,
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
            Self::Analytics => "page-analytics",
            Self::Exchange => "page-exchange",
            Self::Settings => "page-settings",
        }
    }
}

/// The settings page's segments (§10):基本 / 浮窗 / 赛季与存储 / 算法参数,
/// 后面跟着不改任何设置的两段——使用说明与关于。
///
/// 这两段沉在最底下是因为它们是「久没打开、忘了怎么用」时才翻的:排在
/// 前面会把每天都要碰的四段往下推。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsSegment {
    Basic,
    Hud,
    Season,
    Params,
    Guide,
    About,
}

impl SettingsSegment {
    const ALL: [Self; 6] = [
        Self::Basic,
        Self::Hud,
        Self::Season,
        Self::Params,
        Self::Guide,
        Self::About,
    ];

    fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Basic => text.seg_basic,
            Self::Hud => text.seg_hud,
            Self::Season => text.seg_season,
            Self::Params => text.seg_params,
            Self::Guide => text.seg_guide,
            Self::About => text.seg_about,
        }
    }

    const fn element_id(self) -> &'static str {
        match self {
            Self::Basic => "seg-basic",
            Self::Hud => "seg-hud",
            Self::Season => "seg-season",
            Self::Params => "seg-params",
            Self::Guide => "seg-guide",
            Self::About => "seg-about",
        }
    }
}

/// 雷达页的两个页签：交易所雷达（官方成交均价，给线索）/ 抓取雷达（真实
/// 盘口，做裁决）。两段共用一张表和一个明细栏，切段只换行。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadarSegment {
    Exchange,
    Capture,
}

impl RadarSegment {
    pub(crate) const ALL: [Self; 2] = [Self::Exchange, Self::Capture];

    pub(crate) fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Exchange => text.radar_segment_exchange,
            Self::Capture => text.radar_segment_capture,
        }
    }

    pub(crate) const fn element_id(self) -> &'static str {
        match self {
            Self::Exchange => "radar-seg-exchange",
            Self::Capture => "radar-seg-capture",
        }
    }
}

/// 交易所页的时段档位：明细栏曲线画多远、表格按多远的成交额排。
/// 不持久化——这是"现在想看多远"，不是设置；窗口终点是账本最新小时。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExchangeRange {
    Hours24,
    Days3,
    Days7,
    AllKept,
}

impl ExchangeRange {
    pub(crate) const ALL: [Self; 4] = [Self::Hours24, Self::Days3, Self::Days7, Self::AllKept];

    /// 窗口小时数；None = 账本里保留的全部。
    pub(crate) const fn hours(self) -> Option<u32> {
        match self {
            Self::Hours24 => Some(24),
            Self::Days3 => Some(72),
            Self::Days7 => Some(168),
            Self::AllKept => None,
        }
    }

    pub(crate) fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Hours24 => text.exchange_range_24h,
            Self::Days3 => text.exchange_range_3d,
            Self::Days7 => text.exchange_range_7d,
            Self::AllKept => text.exchange_range_all,
        }
    }

    pub(crate) const fn element_id(self) -> &'static str {
        match self {
            Self::Hours24 => "exchange-range-24h",
            Self::Days3 => "exchange-range-3d",
            Self::Days7 => "exchange-range-7d",
            Self::AllKept => "exchange-range-all",
        }
    }
}

/// Everything one accepted book said, kept as a single value.
///
/// These fields are only meaningful together, and keeping them apart cost a
/// real wrong reading: the card titled its rows with [`AppShell::report_pair`],
/// which was the same thing until the convert page grew its own pickers.
/// After that, picking a pair by hand relabelled a live panel with a currency
/// it was not showing — correct rows under a wrong name, which is worse than
/// no card at all. Arriving and being replaced as one value leaves nothing to
/// drift.
struct LastBook {
    /// Position in the run, and how long the read took.
    sequence: u64,
    elapsed_ms: u64,
    /// The panel's own two slots, never overridden by a page's selection.
    have: String,
    need: String,
    /// The same rows with their fields intact, for the window.
    order_rows: Vec<ptt_runtime::pipeline::BookRow>,
    /// Typed facts about the pair, drawn as the monitor's earn table.
    analysis: ptt_runtime::analysis::PairAnalysis,
    /// When this book reached the interface — the health band's "last frame
    /// {}s ago" is measured from here, not from capture time.
    received_at: std::time::Instant,
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
    /// 摆放模式:浮窗暂时吃鼠标、顶条可见。退出即回点击穿透。
    #[cfg(windows)]
    pub(crate) hud_placement: bool,
    /// 浮窗待抓条的独立数据源。
    ///
    /// 不能借当前页面的报表:那份只有停在监视器页时才是待抓队列,人在
    /// 游戏里(浮窗存在的意义)时主窗口停在哪一页是随机的。
    #[cfg(windows)]
    hud_probes: Option<Box<ptt_runtime::reports::ProbeQueueModel>>,
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
    /// The last accepted book, whole. `None` until the first one lands.
    last_book: Option<LastBook>,
    log: VecDeque<String>,
    fault: Option<String>,
    page: Page,
    /// The pair the report pages describe: the last book that was accepted.
    report_pair: Option<(String, String)>,
    /// The currency whose day-by-day detail the Analytics page shows.
    pub(crate) analytics_selected: Option<String>,
    /// 交易所页选中的通货（明细栏画它的小时账本）。
    pub(crate) exchange_selected: Option<String>,
    /// 交易所页的时段档位（明细栏曲线与表格排序共用）。
    pub(crate) exchange_range: ExchangeRange,
    /// 明细栏是按小时铺开还是按一天里的时段汇总。
    pub(crate) exchange_hour_of_day: bool,
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
    /// Whether the watchlist's ignored-pairs list is unfolded.
    ///
    /// 唯一的后悔药,不藏进设置页——但默认收起,它是给后悔用的,不是给
    /// 每天看的。
    pub(crate) show_ignored_probes: bool,
    /// 兑换页的排序开关:false = 按汇率(模型的名次),true = 按吃得下的量。
    ///
    /// 两种排序并存是 §7 的核心裁定:最优汇率 ≠ 做得完,+78.9% 只吃得下
    /// 14,而 +75.1% 吃得下 63——真正决定走哪条的常常是深度。
    pub(crate) convert_sort_by_depth: bool,
    /// 兑换页选中的路线(在当前排序下的行号对应的路线身份,存路径本身,
    /// 排序切换后仍指向同一条路线)。
    pub(crate) convert_selected_route: Option<Vec<String>>,
    /// 设置页当前打开的分段。
    pub(crate) settings_segment: SettingsSegment,
    /// 雷达页当前页签。默认抓取雷达:那是裁决层,也是老用户熟悉的那页。
    pub(crate) radar_segment: RadarSegment,
    /// 上一次算出的交易所雷达：水位没变就直接复用，不再每来一本书重跑一次
    /// 770 ms 的环路搜索。
    exchange_radar_cache: Option<Box<ptt_runtime::reports::ExchangeRadarModel>>,
    /// 交易所页的小时账本，按水位缓存：整段保留期的小时行只在水位前进时重读一次。
    exchange_ledger_cache: Option<ptt_runtime::reports::ExchangeLedgerModel>,
    /// The market tuning boxes on the settings page.
    #[cfg(windows)]
    tuning_inputs: pages::tuning::TuningInputs,
    /// The new-season label box on the settings page.
    pub(crate) season_input: gpui::Entity<gpui_component::input::InputState>,
    /// 交易所联赛名（GGG 英文名）。空 = 不抓取，是历史同步的总开关。
    pub(crate) exchange_league_input: gpui::Entity<gpui_component::input::InputState>,
    /// 交易所回补天数与小时线保留天数（首测反馈：设置得有地方改）。
    pub(crate) exchange_backfill_input: gpui::Entity<gpui_component::input::InputState>,
    pub(crate) exchange_retention_input: gpui::Entity<gpui_component::input::InputState>,
    /// 涨跌天数下拉（二测反馈：轮换按钮不如选单，且上限要跟数据走）。
    pub(crate) exchange_trend_select: pages::convert::AssetSelect,
    /// 选单上次按 (数据天数, 选中值) 装配的签名，变了才重建选项。
    pub(crate) exchange_trend_synced: (u32, u64),
    /// 交易所页"截至哪天看"的日期框（YYYY-MM-DD；空 = 现在）。
    pub(crate) exchange_as_of_input: gpui::Entity<gpui_component::input::InputState>,
    /// The season boundary date box (YYYY-MM-DD; empty = right now). One box
    /// serves both "start on this date" and "ended on this date".
    pub(crate) season_date_input: gpui::Entity<gpui_component::input::InputState>,
    /// Cached season/storage lines for the settings page; `None` means load
    /// on the next draw (cleared after every season action).
    pub(crate) season_info: Option<Vec<String>>,
    /// True while the background count is running: a second draw must not
    /// start a second count.
    pub(crate) season_info_loading: bool,
    /// Bumped by every invalidation; a count that started before the bump
    /// is thrown away when it lands.
    pub(crate) season_info_generation: u64,
    /// Two-click confirmation state for the destructive purge button.
    pub(crate) purge_armed: bool,
    /// The convert page's currency pickers and holdings box.
    ///
    /// Entities rather than values because a select owns its open menu and an
    /// input owns its cursor: rebuilding either on refresh throws away
    /// whatever the user was in the middle of doing.
    convert_have: pages::convert::AssetSelect,
    convert_need: pages::convert::AssetSelect,
    /// The settings page's picker for adding a settlement currency.
    settlement_select: pages::convert::AssetSelect,
    holdings_input: gpui::Entity<gpui_component::input::InputState>,
    /// The radar detail panel's "try a size" box. The radar itself never
    /// assumes a stake (user ruling); this is where the reader brings one,
    /// and the walk is priced at draw time from the row's saved leg books —
    /// no page rebuild, no second trip to the store.
    walk_input: gpui::Entity<gpui_component::input::InputState>,
    /// What the pickers were last filled for: the catalogue in play and the
    /// language its labels were written in. Rebuilding a thousand-entry list
    /// on a frame that changed neither is work nobody asked for.
    convert_choices_key: Option<(usize, ptt_settings::UiLanguage)>,
    /// That list, kept so assigning a selection can reset a searched picker
    /// without walking the catalogue again.
    convert_choices: Vec<pages::convert::AssetChoice>,
    /// The pair last pushed into the pickers. The sync compares against this
    /// rather than the pickers' own `selected_value`, which lags assignment.
    convert_synced_pair: Option<(String, String)>,
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
    /// 更新检查走到哪一步了。关于段画它,顶条在有新版本时挂一枚字。
    #[cfg(windows)]
    pub(crate) update_state: updater::UpdateState,
    /// 启动那次自动检查的插销:`tick` 每 120ms 来一趟,这个让它只响一次。
    #[cfg(windows)]
    update_checked: bool,
    /// 交易所历史同步的插销,和 `update_checked` 同一个道理。
    #[cfg(windows)]
    exchange_sync_kicked: bool,
    /// 哪一条同步链有资格续命。设置换了游戏/联赛时代次前进,旧链自然断掉,
    /// 不会出现两条链同时在每小时抓一遍。
    #[cfg(windows)]
    exchange_sync_generation: u64,
    /// 交易所页每分钟例行刷新的上一次时刻(见 `tick`)。
    #[cfg(windows)]
    exchange_page_refreshed: Option<std::time::Instant>,
    /// 一轮抓取正在后台跑。六测教训:回补进行中再点"立即同步"会开出
    /// 第二条并发链,同一段小时被抓两遍。
    #[cfg(windows)]
    exchange_sync_running: bool,
    /// 哪一次检查/安装的答案有资格写回来。
    ///
    /// 和 `report_generation` 同一个道理,只是这里的迟到更夸张:一次下载可以
    /// 跑好几分钟,期间用户完全可能又按了一次检查。
    #[cfg(windows)]
    update_generation: u64,
    /// 这一次安装收到哪了。后台线程写,`tick` 读。
    ///
    /// 每按一次「安装」换一份新的:被作废的那条线程于是只写得到一个没人读的
    /// 计数器,进度这条路上不需要再复制一遍代次那道闸。
    #[cfg(windows)]
    update_progress: std::sync::Arc<crate::update::Progress>,
    /// 上一帧已经画出去的那个进度。
    ///
    /// 存一份是为了**不**重画:下载卡住的时候数字不动,画面跟着不动才是实话,
    /// 而无脑每 120ms 重画一次会让一个死掉的下载看起来还活着。
    #[cfg(windows)]
    update_progress_shown: crate::update::ProgressSnapshot,
    /// 上一次真的向 GitHub 发问的时刻。手动按钮的冷却从这里算。
    #[cfg(windows)]
    last_update_check: Option<std::time::Instant>,
}

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 上一轮更新留下的 `*.old` / `*.new-update` 在这里收掉。它们上次删不了
        // ——exe 和那个 dll 还被自己占着;这一次启动占用它们的进程已经不在了。
        //
        // 扔到后台执行器上,不在开窗这条路上走:扫的是磁盘,而这一刻窗口还没
        // 画出第一帧。扫失败就算了,清垃圾不该让程序起不来(`clean_leftovers`
        // 自己吞掉所有错误,只回报删掉了几个)。
        #[cfg(windows)]
        cx.background_executor()
            .spawn(async {
                crate::update::clean_leftovers();
            })
            .detach();

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
        let radar_table = {
            let (layout, _) = ptt_runtime::pipeline::route_for(settings.active_profile);
            Self::new_radar_table(window, cx, language, (layout.catalog)())
        };
        #[cfg(windows)]
        let tuning_inputs = {
            let tuning = settings.market_tuning(settings.active_profile.game);
            Self::new_tuning_inputs(window, cx, &tuning)
        };
        let convert_have = Self::new_asset_select(window, cx);
        let convert_need = Self::new_asset_select(window, cx);
        let settlement_select = Self::new_asset_select(window, cx);
        let holdings_input = Self::new_holdings_input(window, cx);
        // Same box as the convert holding: empty or a whole number.
        let walk_input = Self::new_holdings_input(window, cx);
        let season_input =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("0.6"));
        // 预填当前值:这是"改一个已有设置"的框,空着会让人以为没配过。
        let (exchange_league_input, exchange_backfill_input, exchange_retention_input) = {
            let exchange = settings
                .market_tuning(settings.active_profile.game)
                .exchange
                .clone();
            (
                cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx)
                        .default_value(exchange.league.clone())
                        .placeholder("Runes of Aldur")
                }),
                cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx)
                        .default_value(exchange.backfill_days.to_string())
                        .placeholder("14")
                }),
                cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx)
                        .default_value(exchange.hour_retention_days.to_string())
                        .placeholder("14")
                }),
            )
        };
        // 涨跌天数下拉。选项列表跟着数据天数走,在 render 里装配
        // (重建选项需要 window,后台答案带不动它,和兑换页选择器同一个理由)。
        let exchange_trend_select = Self::new_asset_select(window, cx);
        cx.subscribe(
            &exchange_trend_select,
            |this: &mut AppShell, _, event, cx| {
                let gpui_component::select::SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                let Ok(days) = value.to_string().parse::<u64>() else {
                    return;
                };
                let game = this.settings.active_profile.game;
                if this.settings.market_tuning(game).exchange.trend_days == days {
                    return;
                }
                this.settings.market_tuning_mut(game).exchange.trend_days = days;
                match this.settings_store.save(&this.settings) {
                    Ok(()) => this.report_stale = true,
                    Err(error) => this.push_log(format!("settings save failed: {error}")),
                }
                cx.notify();
            },
        )
        .detach();
        let season_date_input = cx
            .new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("YYYY-MM-DD"));
        // 重启后仍在历史视角时,框里要写着那天——否则芯片说"历史"而框是空的。
        let exchange_as_of_input = {
            let as_of = settings
                .market_tuning(settings.active_profile.game)
                .exchange
                .as_of_day
                .clone();
            cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder("YYYY-MM-DD")
                    .default_value(as_of)
            })
        };
        // A picked currency or a typed holding is a new question, so the page
        // is rebuilt; the read itself is backgrounded, so this stays cheap.
        for (select, is_have) in [(convert_have.clone(), true), (convert_need.clone(), false)] {
            cx.subscribe(&select, move |this: &mut AppShell, _, event, cx| {
                let gpui_component::select::SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                // Which box changed does not matter: the pair is whatever
                // the two of them now say.
                let _ = (is_have, value);
                this.choose_pair(cx);
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
        // A typed walk amount only redraws the panel: the evaluation is pure
        // arithmetic on the row's saved leg books, so nothing needs reloading.
        cx.subscribe(&walk_input, |_: &mut AppShell, _, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&radar_table, |_, table, event, cx| {
            use gpui_component::table::TableEvent;
            match event {
                TableEvent::SelectRow(_) | TableEvent::DoubleClickedRow(_) => cx.notify(),
                // A dragged column width lives only in the table until it is
                // written back to the delegate, and the next scan rebuilds
                // the table's copy from the delegate — so without this the
                // drag survives until the next accepted book and no longer.
                TableEvent::ColumnWidthsChanged(widths) => {
                    let widths = widths.clone();
                    table.update(cx, |state, _| {
                        state.delegate_mut().set_column_widths(&widths);
                    });
                }
                _ => {}
            }
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            #[cfg(windows)]
            tuning_inputs,
            season_input,
            exchange_league_input,
            exchange_backfill_input,
            exchange_retention_input,
            exchange_trend_select,
            exchange_trend_synced: (0, 0),
            exchange_as_of_input,
            season_date_input,
            season_info: None,
            season_info_loading: false,
            season_info_generation: 0,
            purge_armed: false,
            radar_table,
            convert_have,
            convert_need,
            settlement_select,
            holdings_input,
            walk_input,
            convert_choices_key: None,
            convert_choices: Vec::new(),
            convert_synced_pair: None,
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
            hud_placement: false,
            #[cfg(windows)]
            hud_probes: None,
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
            last_book: None,
            log: VecDeque::new(),
            fault: None,
            page: Page::Monitor,
            report_pair: None,
            analytics_selected: None,
            exchange_selected: None,
            exchange_range: ExchangeRange::Hours24,
            exchange_hour_of_day: false,
            report: crate::state::PageData::Empty,
            report_generation: 0,
            probe_queue: crate::state::ProbeQueue::default(),
            show_ignored_probes: false,
            convert_sort_by_depth: false,
            convert_selected_route: None,
            settings_segment: SettingsSegment::Basic,
            radar_segment: RadarSegment::Capture,
            exchange_radar_cache: None,
            exchange_ledger_cache: None,
            report_stale: true,
            #[cfg(windows)]
            update_state: updater::UpdateState::default(),
            #[cfg(windows)]
            update_checked: false,
            exchange_sync_kicked: false,
            exchange_sync_generation: 0,
            exchange_page_refreshed: None,
            exchange_sync_running: false,
            #[cfg(windows)]
            update_generation: 0,
            #[cfg(windows)]
            update_progress: std::sync::Arc::new(crate::update::Progress::default()),
            #[cfg(windows)]
            update_progress_shown: crate::update::ProgressSnapshot::default(),
            #[cfg(windows)]
            last_update_check: None,
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
            // 启动之后问一次有没有新版本。放在这里而不是 `new` 里,是为了让
            // 第一帧先画出来;插销在函数内部,这里每 120ms 叫一次也只响一次。
            self.kick_update_check(cx);
            // 交易所历史同步同理:第一轮补拉在后台跑,之后每小时自续。
            self.kick_exchange_sync(cx);
            // 交易所页上的"落后 N 小时"是对着钟走的数,光等同步事件它会
            // 陈旧。每分钟标脏一次:有界的一次读,不违反"报表不上帧循环"。
            if self.page == Page::Exchange
                && self
                    .exchange_page_refreshed
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
            {
                self.exchange_page_refreshed = Some(std::time::Instant::now());
                self.report_stale = true;
            }
            // 摆放模式的回声:拖动落点与顶条按钮点击都由 wndproc 留言,
            // 这里是唯一取走留言的地方。
            self.poll_hud_placement(cx);
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
            // 首次打开浮窗时待抓缓存还是空的,补一次;之后由接受事件维持。
            if dirty && self.hud_visible && self.hud_probes.is_none() {
                self.refresh_hud_probes(cx);
            }
            if dirty {
                self.refresh_hud();
            }
            // 进度只写在关于页那一小块上,跟浮窗和报表都没关系,所以它单独一个
            // 标记,不并进 `dirty`——并进去的话,一次下载会白白重建 240 次浮窗。
            //
            // 不装更新的时候这里只是一次 `matches!`,连一条原子读都不多做;
            // 数字没动就不重画,因为下载卡住时画面停着才是实话。
            if matches!(self.update_state, updater::UpdateState::Downloading(_)) {
                let seen = self.update_progress.snapshot();
                if seen != self.update_progress_shown {
                    self.update_progress_shown = seen;
                    dirty = true;
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
            let mut book_accepted = false;
            for event in events {
                match event {
                    UiEvent::Accepted {
                        sequence,
                        elapsed_ms,
                        need_asset_id,
                        have_asset_id,
                        order_rows,
                        analysis,
                    } => {
                        self.accepted += 1;
                        book_accepted = true;
                        // A pair the user picked by hand outranks whatever
                        // panel happens to be open in game — for the report
                        // pages, which answer a question the user asked. The
                        // card over the panel keeps describing the panel.
                        if !self.pair_chosen_by_user {
                            self.report_pair = Some((have_asset_id.clone(), need_asset_id.clone()));
                        }
                        self.last_book = Some(LastBook {
                            sequence,
                            elapsed_ms,
                            have: have_asset_id,
                            need: need_asset_id,
                            order_rows,
                            analysis: *analysis,
                            received_at: std::time::Instant::now(),
                        });
                        if let Some(line) = self.book_headline() {
                            self.push_log(line);
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
            // 待抓条只在盘口真的进来时才需要重算(覆盖缺口随书变),
            // 跳过帧不动它——不然待机时每一帧都去读一遍库。
            if book_accepted && self.hud_visible {
                self.refresh_hud_probes(cx);
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
                // 装完更新还没重启就开监视,是把新的原生识别库加载进旧进程
                // ——见 `UpdateState::blocks_a_new_watch`。拦在这里,而不是
                // 让它闪退。
                if self.update_state.blocks_a_new_watch() {
                    self.push_log(self.text().update_restart_before_watch.to_owned());
                    cx.notify();
                    return;
                }
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

    /// An asset id as the game writes it, in the client's own language.
    ///
    /// The pipeline speaks ids, and `chaos-orb` is not what the panel says. On
    /// a card read at a glance beside the game, the id costs a translation
    /// step every time; the catalogue already holds the name, keyed by the
    /// profile the watcher is running.
    #[cfg(windows)]
    fn display_name(&self, asset_id: &str) -> String {
        crate::names::asset_name(self.catalog(), self.language(), asset_id)
    }

    /// A pair, named the way the reader knows it.
    fn pair_label(&self, from: &str, to: &str) -> String {
        crate::names::pair_name(self.catalog(), self.language(), from, to)
    }

    /// The active game's market tuning, for pages that state its numbers.
    pub(crate) fn settings_tuning(&self) -> ptt_settings::MarketTuning {
        #[cfg(windows)]
        {
            self.settings
                .market_tuning(self.settings.active_profile.game)
        }
        #[cfg(not(windows))]
        {
            ptt_settings::MarketTuning::default()
        }
    }

    /// The catalogue for the game being watched.
    fn catalog(&self) -> &'static ptt_runtime::domain::Catalog {
        let (layout, _) = ptt_runtime::pipeline::route_for(self.settings.active_profile);
        (layout.catalog)()
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
        let requested_at = request.requested_at;
        cx.spawn(async move |this, cx| {
            let data = cx
                .background_executor()
                .spawn(async move { build_page_data(&request) })
                .await;
            this.update(cx, |this: &mut AppShell, cx| {
                if this.report_generation == generation {
                    this.report = data;
                    // 监视器页刚算好的待抓队列顺手喂给浮窗缓存,两处
                    // 永远说同一份话。
                    if let crate::state::PageData::Probes(model) = &this.report {
                        this.hud_probes = Some(model.clone());
                    }
                    this.sync_radar_table(cx);
                    if let crate::state::PageData::Opportunities(model) = &this.report
                        && let Some(exchange) = &model.exchange
                    {
                        this.exchange_radar_cache = Some(Box::new(exchange.clone()));
                    }
                    // 先把账本克隆出来再改 self：借着 report 的时候不能写日志。
                    let ledger = match &this.report {
                        crate::state::PageData::Exchange(model) => model.ledger.clone(),
                        _ => None,
                    };
                    if let Some(ledger) = ledger {
                        // 账本只在水位前进时真读一次库；那一次的耗时写进日志，
                        // 用户不用开探针也看得见这本账多贵。
                        if ledger.load_millis > 0 {
                            this.push_log(format!(
                                "exchange: ledger {} hours / {} rows in {} ms",
                                ledger.hours_loaded, ledger.rows_loaded, ledger.load_millis
                            ));
                        }
                        this.exchange_ledger_cache = Some(ledger);
                    }
                    this.close_answered_probes(requested_at);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 重算浮窗待抓条的数据(后台读库,回来时刷新浮窗)。
    ///
    /// 和 `refresh_report` 平行的一条小管线:浮窗不属于任何页面,它的
    /// 数据也就不能搭页面报表的便车。
    #[cfg(windows)]
    fn refresh_hud_probes(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.page_request(cx) else {
            return;
        };
        let requested_at = request.requested_at;
        cx.spawn(async move |this, cx| {
            let model = cx
                .background_executor()
                .spawn(async move { load_probe_queue(&request) })
                .await;
            this.update(cx, |this: &mut AppShell, _cx| {
                if let Ok(model) = model {
                    // 浮窗这条管线也关闭已回答的钉子:只靠监视页落地,钉子会活到
                    // 下次打开监视页——大雷达钉的腿抓完了还挂在浮窗上赶人。
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
                    this.probe_queue.retain_missing(&missing, requested_at);
                    this.hud_probes = Some(Box::new(model));
                    this.refresh_hud();
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
            sticky_probes: self
                .probe_queue
                .entries()
                .iter()
                .filter(|pin| pin.sticky)
                .map(|pin| {
                    (
                        pin.from_asset_id.clone(),
                        pin.to_asset_id.clone(),
                        pin.reason.clone(),
                        pin.pinned_at,
                    )
                })
                .collect(),
            requested_at: chrono::Utc::now(),
            exchange_radar_cache: self.exchange_radar_cache.clone(),
            exchange_ledger_cache: self.exchange_ledger_cache.clone(),
            exchange_window_hours: self.exchange_range.hours(),
        })
    }

    /// Drops pinned probes for pairs the newest answer can already price.
    ///
    /// Only the watchlist and the monitor know which pairs are still
    /// incomplete, so this runs where their answers arrive rather than on a
    /// timer.
    #[cfg(windows)]
    fn close_answered_probes(&mut self, asked_at: chrono::DateTime<chrono::Utc>) {
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
        self.probe_queue.retain_missing(&missing, asked_at);
    }

    /// Queues a pair for the user to go and flip.
    #[cfg(windows)]
    pub(crate) fn pin_probe(&mut self, from: &str, to: &str, reason: &str, sticky: bool) {
        self.probe_queue.pin(crate::state::PinnedProbe {
            from_asset_id: from.to_owned(),
            to_asset_id: to.to_owned(),
            reason: reason.to_owned(),
            sticky,
            pinned_at: chrono::Utc::now(),
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

    /// Every currency the catalogue holds, as pickable choices.
    ///
    /// The catalogue rather than what has been captured: a picker that only
    /// offers pairs already seen cannot be used to ask about the pair you have
    /// not looked at yet, which is the question a person actually arrives
    /// with. Whether there is data for a choice is the page's answer, not the
    /// picker's business.
    fn catalog_choices(&self) -> Vec<pages::convert::AssetChoice> {
        let mut choices: Vec<pages::convert::AssetChoice> = self
            .catalog()
            .assets()
            .iter()
            .map(|asset| {
                // The domain's spelling, because the choice becomes the pair
                // the report is asked for, and `MarketAssetId` rejects the
                // catalogue's underscores — a picked currency would be
                // filtered out on the way to the store rather than refused.
                let id = ptt_runtime::live::domain_asset_id(&asset.id)
                    .map_or_else(|_| asset.id.clone(), |id| id.as_str().to_owned());
                let label = self.display_name(&id);
                pages::convert::AssetChoice::new(id, label, crate::names::search_keys(asset))
            })
            .collect();
        // By what the reader sees, so the list reads alphabetically to them
        // rather than to the database.
        choices.sort_by(|left, right| left.label().cmp(right.label()));
        choices
    }

    /// One line naming the last accepted book.
    ///
    /// Composed here rather than on the backend thread because it names
    /// currencies, and the name depends on the interface's language.
    fn book_headline(&self) -> Option<String> {
        let book = self.last_book.as_ref()?;
        Some(format!(
            "#{} [{}ms] {} ({})",
            book.sequence,
            book.elapsed_ms,
            self.pair_label(&book.have, &book.need),
            ptt_runtime::report_text::fill(
                self.text().book_rows,
                &[&book.order_rows.len().to_string()]
            ),
        ))
    }

    /// The pair the last accepted book was read off, as display names.
    ///
    /// The panel's own two slots — never [`Self::report_pair`], which the
    /// convert page can point somewhere else entirely.
    #[cfg(windows)]
    pub(crate) fn last_book_pair(&self) -> String {
        match &self.last_book {
            Some(book) => format!(
                "{} -> {}",
                self.display_name(&book.have),
                self.display_name(&book.need)
            ),
            None => self.text().no_pair_yet.to_owned(),
        }
    }

    /// The lines a text page draws, including the ones that describe an
    /// absence.
    ///
    /// An empty answer, a page still reading, a page with no pair yet and a
    /// page whose read failed are four different things, and a bare empty
    /// list says the same nothing for all four.
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
            | PageData::History(_)
            | PageData::Analytics(_)
            | PageData::Exchange(_) => vec![text.nothing_yet.to_owned()],
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

    /// 左导航:108px 栏,28px 条目(原型 1a)。
    ///
    /// 激活项是「色字=主题」的示范:2px 金左条 + panel 底 + 金字 600。
    /// 左内边距从 14 减到 12 抵掉 2px 边框,文字不因选中而移位——
    /// 和表格选中行同一条规矩。
    fn nav_rail(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(px(W_NAV))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(1.))
            .py_2()
            .bg(c(RAIL))
            .border_r_1()
            .border_color(c(HAIRLINE))
            .children(Page::ALL.into_iter().map(|page| {
                let active = page == self.page;
                let row = div()
                    .id(page.element_id())
                    .h(px(H_BUTTON))
                    .flex_none()
                    .flex()
                    .items_center()
                    .text_size(fs(FS_12))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_page(page);
                        cx.notify();
                    }));
                let row = if active {
                    row.pl(px(12.))
                        .border_l_2()
                        .border_color(c(ACCENT))
                        .bg(c(PANEL))
                        .font_semibold()
                        .text_color(c(ACCENT_TEXT))
                } else {
                    row.pl(px(14.))
                        .text_color(c(TEXT_SECONDARY))
                        .hover(|style| style.bg(c(HOVER)))
                };
                row.child(SharedString::from(page.label(self.text()).to_string()))
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
    /// 大雷达钉的腿 (from, to, 理由):待抓队列是派生的,这些要带过去
    /// 才能在没抓到之前一直挂着。
    sticky_probes: Vec<(String, String, String, chrono::DateTime<chrono::Utc>)>,
    /// When this snapshot was taken. A pin placed after it was never seen by
    /// the answer it produces, so that answer must not close it.
    requested_at: chrono::DateTime<chrono::Utc>,
    /// The exchange radar the shell last drew; reused while the watermark and
    /// the knobs it was built with have not moved.
    exchange_radar_cache: Option<Box<ptt_runtime::reports::ExchangeRadarModel>>,
    /// 上一次画的小时账本；联赛、水位、锚、保留天数都没变就直接复用。
    exchange_ledger_cache: Option<ptt_runtime::reports::ExchangeLedgerModel>,
    /// 交易所页当前的时段档位（小时数；None = 账本全部）。表格按它重排。
    exchange_window_hours: Option<u32>,
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
    if request.page == Page::Analytics {
        return match load_analytics(request) {
            Ok(model) => PageData::Analytics(Box::new(model)),
            Err(reason) => PageData::Failed(reason),
        };
    }
    if request.page == Page::Exchange {
        return match load_exchange(request) {
            Ok(Some(model)) => PageData::Exchange(Box::new(model)),
            // 没配联赛不是错误,是一句"去哪打开"的指路。
            Ok(None) => PageData::Text(vec![
                crate::i18n::text(request.language)
                    .exchange_no_league
                    .to_owned(),
            ]),
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
    let analytics = load_pulse(request);
    let mut model = ptt_runtime::reports::probe_queue_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
        analytics.as_ref().map(|model| &model.pulse),
    )?;
    // 大雷达钉的腿:派生候选里没有它们,没抓到新鲜档之前要一直挂在队首。
    let pins: Vec<(
        ptt_trade_domain::MarketAssetId,
        ptt_trade_domain::MarketAssetId,
        String,
        chrono::DateTime<chrono::Utc>,
    )> = request
        .sticky_probes
        .iter()
        .filter_map(|(from, to, reason, pinned_at)| {
            Some((
                ptt_runtime::live::domain_asset_id(from).ok()?,
                ptt_runtime::live::domain_asset_id(to).ok()?,
                reason.clone(),
                *pinned_at,
            ))
        })
        .collect();
    let sticky = ptt_runtime::reports::sticky_probe_candidates(&pins, &observations);
    for candidate in sticky.into_iter().rev() {
        let held = model.candidates.iter().any(|held| {
            held.from_asset_id == candidate.from_asset_id
                && held.to_asset_id == candidate.to_asset_id
        });
        if !held {
            model.candidates.insert(0, candidate);
        }
    }
    Ok(model)
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
    // A configured season bounds every page read on both sides: the start
    // floors the window, a recorded end caps it. None configured, no clamp.
    let now = chrono::Utc::now();
    let season = store
        .active_season(request.profile.game.as_str())
        .ok()
        .flatten();
    let since = ptt_runtime::rollup::clamp_to_season(
        now - chrono::Duration::hours(window_hours),
        season.as_ref().map(|row| row.started_at),
    );
    let until = ptt_runtime::rollup::clamp_end_to_season(
        now + chrono::Duration::hours(1),
        season.as_ref().and_then(|row| row.ended_at),
    );
    let observations = store
        .load_observations_between(&context_key, since, until)
        .map_err(|error| format!("load: {error}"))?;
    Ok((context_key, observations))
}

/// The Analytics page: the same pulse the annotations read, with its notes
/// and season banner attached. The builder runs first so yesterday's books
/// are always rolled up by the time the page reads them.
#[cfg(windows)]
fn load_analytics(request: &PageRequest) -> Result<ptt_runtime::reports::AnalyticsModel, String> {
    use ptt_runtime::pipeline::default_database_path;
    use ptt_runtime::rollup;

    // Lazily roll up any fully-elapsed days first (bounded per run). A
    // failure here degrades to "less history", never to a failed page.
    let mut store = ptt_storage::MarketStore::open(default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let game = request.profile.game.as_str();
    let now = chrono::Utc::now();
    let _ = rollup::ensure_daily_rollups(
        &mut store,
        game,
        now,
        rollup::MAX_ROLLUP_DAYS_PER_RUN,
        request.tuning.risk.top_book_outlier_factor,
    );
    if request.tuning.analytics.raw_retention_days > 0 {
        let _ = rollup::prune_raw_days(
            &mut store,
            game,
            now,
            request.tuning.analytics.raw_retention_days,
        );
    }
    drop(store);

    load_pulse(request).ok_or_else(|| "analytics unavailable".to_owned())
}

/// The market pulse the structural annotations read: persisted day rollups
/// (season-clamped) plus a live fold of today, read across every context of
/// the game. `None` when the store cannot answer — annotations simply stay
/// absent, the page itself is unaffected.
///
/// It loads its own window rather than taking the page's: the page reads one
/// context key by design (a book must be self-consistent), and on a release
/// day that key rotates at midday, which would drop the morning out of the
/// fold.
#[cfg(windows)]
fn load_pulse(request: &PageRequest) -> Option<ptt_runtime::reports::AnalyticsModel> {
    use ptt_runtime::pipeline::{LIVE_LEAGUE, default_database_path};

    let store = ptt_storage::MarketStore::open(default_database_path()).ok()?;
    let game = request.profile.game.as_str();
    let season = store.active_season(game).ok().flatten();
    let now = chrono::Utc::now();
    let from_day = season.as_ref().map_or_else(
        || "0001-01-01".to_owned(),
        |row| row.started_at.format("%Y-%m-%d").to_string(),
    );
    // 已结束的赛季连"今天"也不再往后长:天级 rollup 和今天的活折叠都
    // 停在结束那天。
    let end_cap =
        ptt_runtime::rollup::clamp_end_to_season(now, season.as_ref().and_then(|row| row.ended_at));
    let to_day = end_cap.format("%Y-%m-%d").to_string();
    let rollup_rows = store.load_rollups(game, &from_day, &to_day).ok()?;
    let today = ptt_runtime::rollup::today_window(&store, game, now, season.as_ref()).ok()?;
    Some(ptt_runtime::reports::analytics_model(
        &rollup_rows,
        &today,
        season.as_ref(),
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
    ))
}

/// The radar's ranked routes.
#[cfg(windows)]
fn load_opportunities(
    request: &PageRequest,
) -> Result<ptt_runtime::reports::OpportunitiesModel, String> {
    use ptt_runtime::pipeline::LIVE_LEAGUE;

    let (context_key, observations) = load_window(request)?;
    let analytics = load_pulse(request);
    let mut model = ptt_runtime::reports::opportunities_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
        analytics.as_ref().map(|model| &model.pulse),
    )?;
    // 交易所雷达（大雷达）是同一页的另一个页签。算不出来只记一条 note，
    // 不拖垮抓取雷达——两层各自独立，坏一层另一层照常。
    match load_exchange_radar(request) {
        Ok(exchange) => model.exchange = exchange,
        Err(error) => model.exchange_error = Some(error),
    }
    Ok(model)
}

/// 官方小时行 → 大雷达模型。联赛没配 = `None`。
#[cfg(windows)]
fn load_exchange_radar(
    request: &PageRequest,
) -> Result<Option<ptt_runtime::reports::ExchangeRadarModel>, String> {
    let league = request.tuning.exchange.league.trim().to_owned();
    if league.is_empty() {
        return Ok(None);
    }
    let store = ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let game = request.profile.game.as_str();
    let now = chrono::Utc::now();
    // 水位是存储层的事实，模型函数不碰 store（与交易所页同一条规矩）。
    let watermark = store
        .exchange_watermark(game, &league)
        .map_err(|error| format!("watermark: {error}"))?;
    let newest_complete = now.timestamp().div_euclid(3600) * 3600 - 3600;
    let hours_behind = watermark.map_or(0, |mark| ((newest_complete - mark) / 3600).max(0));
    // 同一水位、同一联赛、同样的门槛和环长 = 同一批小时数据、同一个答案。
    // 书每几秒落一本，环路搜索 770 ms 一次——不复用就是每本书白烧一次。
    let (minimum_bps, max_cycle_length) =
        ptt_runtime::reports::exchange_radar_knobs(&request.tuning);
    if let Some(cached) = &request.exchange_radar_cache
        && cached.league == league
        && cached.synced_through == watermark
        && cached.minimum_profit_basis_points == minimum_bps
        && cached.max_cycle_length == max_cycle_length
    {
        let mut model = (**cached).clone();
        model.hours_behind = hours_behind;
        return Ok(Some(model));
    }
    // 48 小时：模型只取每对最新一小时，但 API 落后一两小时、周末更久，
    // 窗口宽一点才不会一到周一就空白。
    let hour_rows = store
        .load_exchange_hours(game, &league, now.timestamp() - 48 * 3600, now.timestamp())
        .map_err(|error| format!("hours: {error}"))?;
    let mut model =
        ptt_runtime::reports::exchange_radar_model(&hour_rows, &league, &request.tuning, now)?;
    model.hours_behind = hours_behind;
    model.synced_through = watermark;
    Ok(Some(model))
}

/// The focus set, its valuations and its gaps.
#[cfg(windows)]
fn load_watchlist(request: &PageRequest) -> Result<ptt_runtime::reports::WatchlistModel, String> {
    use ptt_runtime::pipeline::LIVE_LEAGUE;

    let (context_key, observations) = load_window(request)?;
    let analytics = load_pulse(request);
    ptt_runtime::reports::watchlist_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &request.tuning,
        request.language,
        analytics.as_ref().map(|model| &model.pulse),
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
    let analytics = load_pulse(request);
    ptt_runtime::reports::convert_model(
        &observations,
        &context_key,
        &have,
        &need,
        request.holdings,
        &request.tuning,
        request.language,
        analytics.as_ref().map(|model| &model.pulse),
    )
    .map(Some)
}

/// 官方交易所总览。读的是 exchange 四表,不是 OCR 的账——联赛没配就返回
/// `None`,页面显示指路一句话而不是报错。
#[cfg(windows)]
fn load_exchange(
    request: &PageRequest,
) -> Result<Option<ptt_runtime::reports::ExchangeModel>, String> {
    let league = request.tuning.exchange.league.trim().to_owned();
    if league.is_empty() {
        return Ok(None);
    }
    let store = ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let game = request.profile.game.as_str();
    let now = chrono::Utc::now();
    // 截至日期（历史视角）：日窗口的终点挪到那天，小时行一行不读——
    // 它们是"现在"的账，模型也不会看。
    let as_of =
        chrono::NaiveDate::parse_from_str(request.tuning.exchange.as_of_day.trim(), "%Y-%m-%d")
            .ok();
    // 小时窗口 48h:激增基准要 8+ 小时,最新价要最近的完整小时。
    let hour_rows = if as_of.is_some() {
        Vec::new()
    } else {
        store
            .load_exchange_hours(game, &league, now.timestamp() - 48 * 3600, now.timestamp())
            .map_err(|error| format!("hours: {error}"))?
    };
    // 日窗口 60 天:日折行小而永久,窗口宽一点让"回补 30 天 + 30 天涨跌"
    // 都装得下;真正算多少天由涨跌选择器和数据长度决定。
    let window_end = as_of.unwrap_or_else(|| now.date_naive());
    let from_day = (window_end - chrono::Duration::days(60)).to_string();
    let to_day = window_end.to_string();
    let day_rows = store
        .load_exchange_days(game, &league, &from_day, &to_day)
        .map_err(|error| format!("days: {error}"))?;
    // 水位与欠账在这里补进模型：进度是存储层的事实，模型函数不碰 store。
    let watermark = store
        .exchange_watermark(game, &league)
        .map_err(|error| format!("watermark: {error}"))?;
    let mut model =
        ptt_runtime::reports::exchange_model(&day_rows, &hour_rows, &league, &request.tuning)?;
    let newest_complete = now.timestamp().div_euclid(3600) * 3600 - 3600;
    model.synced_through = watermark;
    model.hours_behind = watermark.map_or(0, |mark| ((newest_complete - mark) / 3600).max(0));
    // 小时账本是"现在"的读数，历史视角下不算（与小时行一样）。
    if as_of.is_none() {
        model.ledger = load_exchange_ledger(&store, request, &league, watermark)?;
    }
    // 有账本就按用户选的时段重排表格（Arc 克隆一下，免得借着 model 改 model）。
    let ledger = model.ledger.as_ref().map(|ledger| ledger.ledger.clone());
    if let Some(ledger) = ledger {
        ptt_runtime::reports::apply_exchange_window(
            &mut model,
            &ledger,
            request.exchange_window_hours,
        );
    }

    // ---- 面板核对：按抓取时刻逐点查官方小时行 ----
    // 窗口 = 小时明细保留天数（明细清掉后就没区间可比；0 = 不清理，取一年），
    // 再被赛季起点钳住：上季的抓取对新联赛的小时行本来就对不上，
    // 不该混进"没对上"里吓人。
    let retention = request.tuning.exchange.hour_retention_days;
    let window_days = u32::try_from(if retention == 0 {
        365
    } else {
        retention.min(365)
    })
    .unwrap_or(14);
    let context =
        ptt_runtime::live::live_context(request.profile, ptt_runtime::pipeline::LIVE_LEAGUE)
            .map_err(|error| format!("{error:?}"))?;
    let season = store.active_season(game).ok().flatten();
    let since = ptt_runtime::rollup::clamp_to_season(
        now - chrono::Duration::days(i64::from(window_days)),
        season.as_ref().map(|row| row.started_at),
    );
    let observations = store
        .load_observations_between(
            &context.stable_key(),
            since,
            now + chrono::Duration::hours(1),
        )
        .map_err(|error| format!("observations: {error}"))?;
    let mut matched_rows = Vec::new();
    for (hour_ts, asset_a, asset_b) in ptt_runtime::reports::exchange_reconcile_keys(&observations)?
    {
        if let Some(row) = store
            .load_exchange_hour_market(game, &league, hour_ts, &asset_a, &asset_b)
            .map_err(|error| format!("hour market: {error}"))?
        {
            matched_rows.push(row);
        }
    }
    model.reconcile = Some(ptt_runtime::reports::exchange_reconcile(
        &observations,
        &matched_rows,
        window_days,
    )?);
    Ok(Some(model))
}

/// 整段保留期的小时账本。同联赛、同水位、同锚、同保留天数 = 同一本账，
/// 直接复用；否则读精简行重建——1.8M 行一两秒，只在水位前进时付一次。
/// 复用时 `load_millis` 归零：只有真读了库的那次才值得写进日志。
#[cfg(windows)]
fn load_exchange_ledger(
    store: &ptt_storage::MarketStore,
    request: &PageRequest,
    league: &str,
    watermark: Option<i64>,
) -> Result<Option<ptt_runtime::reports::ExchangeLedgerModel>, String> {
    let Some(watermark) = watermark else {
        return Ok(None);
    };
    let anchor = ptt_runtime::reports::exchange_anchor(&request.tuning)?;
    let retention_days = request.tuning.exchange.hour_retention_days;
    if let Some(cached) = &request.exchange_ledger_cache
        && cached.league == league
        && cached.synced_through == Some(watermark)
        && cached.anchor_asset_id == anchor
        && cached.retention_days == retention_days
    {
        let mut model = cached.clone();
        model.load_millis = 0;
        return Ok(Some(model));
    }
    let game = request.profile.game.as_str();
    let window_hours = ptt_runtime::reports::exchange_ledger_window_hours(&request.tuning);
    let from_ts = watermark - i64::from(window_hours) * 3600;
    let started = std::time::Instant::now();
    let rows = store
        .load_exchange_hour_volumes(game, league, from_ts, watermark + 3600)
        .map_err(|error| format!("hour volumes: {error}"))?;
    let mut model = ptt_runtime::reports::exchange_ledger_model(&rows, league, &request.tuning)?;
    model.synced_through = Some(watermark);
    model.load_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(Some(model))
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
        // Answered as a model by `load_analytics`.
        Page::Analytics => Ok(Vec::new()),
        // Answered as a model by `load_exchange`.
        Page::Exchange => Ok(Vec::new()),
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

/// 顶条里的一对「label 值」:label 走当前容器的 meta 灰,值用等宽数据色。
fn band_stat(label: &str, value: String) -> gpui::Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(4.))
        .child(SharedString::from(label.to_string()))
        .child(
            div()
                .text_color(c(TEXT_DATA))
                .child(SharedString::from(value)),
        )
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rebuilding a select needs a window, which the background answer
        // does not carry, so the pickers are brought up to date on the first
        // frame after one lands.
        #[cfg(windows)]
        self.sync_convert_selects(window, cx);
        #[cfg(windows)]
        self.sync_exchange_trend_select(window, cx);
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
        // 顶条上那枚"有新版本"。
        //
        // 借的是既有的位置——状态条右侧本来就是空的,只有开关按钮——而不是弹一个
        // 对话框:更新是一件可以永远不理的事,它不该打断任何一次会话。没有好消息
        // 的时候这里什么都不画,顶条回到原样。点一下直接跳到关于段,因为完整的
        // 说明只有那里有。
        #[cfg(windows)]
        let update_badge = {
            // 下载和校验也要在顶条露面:关于段以外的任何一页都看不到那条
            // 进度,而这两段恰恰是唯一会让人等的。用户第一次装更新就以为
            // 卡死了,原因就是走开之后界面上再没有任何动静。
            let downloading = matches!(
                self.update_state,
                updater::UpdateState::Downloading(_) | updater::UpdateState::Installing
            );
            let busy_label = downloading.then(|| {
                let snapshot = self.update_progress.snapshot();
                match updater::progress_percent(snapshot.done, snapshot.total) {
                    Some(percent) => {
                        format!("{} {percent}%", updater::stage_line(snapshot.stage, text))
                    }
                    None => updater::stage_line(snapshot.stage, text).to_owned(),
                }
            });
            let label = match &self.update_state {
                updater::UpdateState::Available(_) => Some(text.update_badge.to_owned()),
                updater::UpdateState::Installed(_) => Some(text.update_state_installed.to_owned()),
                _ => busy_label,
            };
            label.map(|label| {
                div()
                    .id("update-badge")
                    .h_flex()
                    .items_center()
                    .px(px(6.))
                    .h(px(H_CHIP))
                    .text_size(fs(FS_11))
                    .text_color(c(ACCENT_TEXT))
                    .bg(c(ACCENT_WASH))
                    .border_1()
                    .border_color(c(ACCENT_LINE))
                    .cursor_pointer()
                    .hover(|style| style.bg(c(HOVER)))
                    .child(SharedString::from(label.to_string()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_page(Page::Settings);
                        this.settings_segment = SettingsSegment::About;
                        cx.notify();
                    }))
            })
        };
        #[cfg(not(windows))]
        let update_badge: Option<gpui::Div> = None;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(CANVAS))
            .text_color(c(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .child(
                // 顶部状态条(36px,原型 1a):点 · 标题 · 状态字 · 计数对 · 开关。
                div()
                    .h(px(H_STATUS_TOP))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE_STRONG))
                    .child(breathing_dot("watch-dot", dot_kind))
                    .child(div().text_size(fs(FS_12_5)).child(spaced(text.app_title)))
                    .child(
                        div()
                            .text_size(fs(FS_12))
                            .text_color(c(dot_kind.text()))
                            .child(SharedString::from(state_label.to_string())),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap(px(14.))
                            .font_family(FONT_MONO)
                            .text_size(fs(FS_11))
                            .text_color(c(TEXT_META))
                            .child(band_stat(text.accepted_label, self.accepted.to_string()))
                            .child(band_stat(text.skips_label, skip_total.to_string())),
                    )
                    .child(div().flex_grow())
                    .children(update_badge)
                    .child(
                        button("watch-toggle", button_kind, button_label, cx)
                            .h(px(H_ROW))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_watch(cx))),
                    ),
            )
            .child(
                // Body: navigation rail plus the active page.
                //
                // `min_h(0)` all the way down, because a flex item's automatic
                // minimum height is its content: without it a long list makes
                // its panel taller than the window instead of scrolling inside
                // it, and `overflow_y_scroll` further in never has anything to
                // clip.
                //
                // `min_w(0)` on the page is the same sentence about the other
                // axis, and it is what makes prose wrap. gpui measures text at
                // "min-content" by laying the whole run out on one line, so a
                // paragraph's automatic minimum width is its full unwrapped
                // length. The page could not shrink below that, every panel
                // inside stretched to match, no line ever ran out of room to
                // wrap in, and `overflow_hidden` here cut the overhang off at
                // the window edge — which reads as text that simply stops.
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .overflow_hidden()
                    .child(self.nav_rail(cx))
                    .child(
                        (if self.page == Page::Settings {
                            self.render_settings_page(cx)
                        } else if self.page == Page::Monitor {
                            self.render_monitor(cx)
                        } else if self.page == Page::Opportunities {
                            self.render_opportunities(cx)
                        } else if self.page == Page::Convert {
                            self.render_convert(cx)
                        } else if self.page == Page::Watchlist {
                            self.render_watchlist(cx)
                        } else if self.page == Page::Analytics {
                            self.render_analytics(cx)
                        } else if self.page == Page::Exchange {
                            self.render_exchange(cx)
                        } else if self.page == Page::History {
                            self.render_history(cx)
                        } else if self.page == Page::Calibrate {
                            self.render_calibrate(cx)
                        } else {
                            div()
                                .flex_grow()
                                .flex()
                                .flex_col()
                                .gap(px(SP_8))
                                .p(px(SP_10))
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
                        })
                        .min_w(px(0.)),
                    ),
            )
            .child(
                // 底部状态栏(22px):故障或最近一条日志。
                div()
                    .h(px(H_STATUS_BOTTOM))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(c(RAIL))
                    .border_t_1()
                    .border_color(c(HAIRLINE))
                    .child(match &self.fault {
                        Some(fault) => mono(format!("{}: {fault}", text.fault_prefix))
                            .text_size(fs(FS_10_5))
                            .text_color(c(DANGER_TEXT)),
                        None => mono(self.log.back().cloned().unwrap_or_default())
                            .text_size(fs(FS_10_5))
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
        // The most frequent key of them all, and the one that is not a
        // problem: the exchange panel simply was not open.
        "not-book-view" => text.skip_not_book_view.to_owned(),
        other => other.to_owned(),
    }
}

/// The skip tally, split into (real problems, normal standby).
///
/// `not-book-view` means the exchange panel simply was not open — the watcher
/// idling as designed. Counting those frames among the skips buries the
/// number that matters: 549 standby frames next to 17 real failures read as
/// "566 problems", and a reader trained on that learns to ignore the count.
fn standby_skip_split(skips: &BTreeMap<String, u64>) -> (u64, u64) {
    let standby = skips.get("not-book-view").copied().unwrap_or(0);
    let total: u64 = skips.values().sum();
    (total - standby, standby)
}

#[cfg(test)]
mod skip_label_tests {
    use super::{skip_label, standby_skip_split};
    use ptt_settings::UiLanguage;

    /// `not-book-view` is the most frequent skip key of them all — 549 of 566
    /// frames on the owner's real session — and the label function had no
    /// branch for it, so the interface printed the English key into a Chinese
    /// sentence. The bug the 4a design review found.
    #[test]
    fn the_most_frequent_skip_reason_has_a_chinese_name() {
        assert_eq!(
            skip_label("not-book-view", UiLanguage::Chinese),
            "面板没开着",
            "the interface is showing the raw bucket key for the most \
             common skip reason"
        );
        assert_eq!(
            skip_label("not-book-view", UiLanguage::English),
            "panel not open",
        );
    }

    /// Standby is not failure: counting the closed-panel frames among the
    /// skips trains the reader to ignore the number that matters.
    #[test]
    fn standby_frames_are_counted_apart_from_real_skips() {
        let mut skips = std::collections::BTreeMap::new();
        skips.insert("not-book-view".to_owned(), 549_u64);
        skips.insert("rows:NoBands".to_owned(), 16_u64);
        skips.insert("confirmation-mismatch".to_owned(), 1_u64);
        let (real, standby) = standby_skip_split(&skips);
        assert_eq!(standby, 549);
        assert_eq!(real, 17);
    }
}

#[cfg(test)]
mod hud_tests {
    use super::{HUD_SIZE_EXPANDED, HUD_SIZE_MINI};

    /// The expanded card must hold the whole panel: six rows a side plus the
    /// header, verdict and probe strip. The painter stacks fixed-height rows
    /// and clips silently, so a height written small eats the bottom row —
    /// the aggregate, which is the one that says how much is behind the
    /// front. This mirrors crates/ptt-platform-win/src/win32/hud.rs.
    #[test]
    fn the_expanded_card_adds_up_to_its_sections() {
        const HEADER: i32 = 26;
        const HAIRLINE: i32 = 1;
        // 上边距 6 + 栏头 16 + 6×18 行 + 聚合行 hairline 1+2 + 下边距 4(§4)。
        const BODY: i32 = 6 + 16 + 6 * 18 + 1 + 2 + 4;
        const VERDICT: i32 = 24;
        const PROBE: i32 = 20;
        assert_eq!(
            HEADER + HAIRLINE + BODY + HAIRLINE + VERDICT + HAIRLINE + PROBE,
            HUD_SIZE_EXPANDED.1,
            "a section changed height without the card following"
        );
    }

    /// 迷你档 88 = 内边距 5+5 + 状态 20 + 通货对 20 + 结论 20 + 待抓 18。
    #[test]
    fn the_mini_card_adds_up_to_its_rows() {
        assert_eq!(5 + 20 + 20 + 20 + 18 + 5, HUD_SIZE_MINI.1);
    }
}
