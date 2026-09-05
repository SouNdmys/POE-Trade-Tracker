//! 官方通货历史的每小时后台同步：什么时候抓、抓完顺手折叠清理、下一轮几点。
//!
//! 形状照抄 updater.rs：tick 插销一次性点火、输入拷成自己的一份、逻辑扔到
//! 后台执行器、回来按代次验旧。后台自己开第二个 MarketStore 连接——
//! busy_timeout 5 秒本来就是为多连接共存设计的，不碰界面线程那份。
//!
//! "打开程序自动抓"和"运行中每小时抓"是同一条代码路径：都只是
//! "把缺的补上"（`plan_fetch` 的水位算术），所以没有第二套逻辑可以漂移。

use std::time::Duration;

use gpui::Context;

use crate::shell::AppShell;

impl AppShell {
    /// 一次启动只点一次火；之后每轮结束时自己排下一轮。
    /// 放在 `tick` 而不是 `new`：开窗那一帧不该等任何网络。
    pub(crate) fn kick_exchange_sync(&mut self, cx: &mut Context<Self>) {
        if self.exchange_sync_kicked {
            return;
        }
        self.exchange_sync_kicked = true;
        // 启动那轮也出声：第一轮补拉要跑十几秒才有第一条进度，
        // 没有这句用户会以为程序根本没动（六测反馈）。
        if !self
            .settings
            .market_tuning(self.settings.active_profile.game)
            .exchange
            .league
            .trim()
            .is_empty()
        {
            self.push_log("exchange: sync started".to_owned());
        }
        self.begin_exchange_sync(cx, false);
    }

    /// 手动来一轮（按钮/设置变更）。`begin` 自己会前进代次，旧链在下一次
    /// 醒来时发现代次不对就安静退场。手动轮永远出声——四测反馈：
    /// 点了没反馈，分不清生效、无事可做还是网断了。
    pub(crate) fn restart_exchange_sync(&mut self, cx: &mut Context<Self>) {
        if self.exchange_sync_running {
            // 正在跑就别再开一条：同一段小时会被两条链抓两遍。
            // 但记一笔"跑完立刻再来"——改了联赛名却要等到下个整点才生效，
            // 用户看到的就是"我改了名字，页面还是那句错"。
            self.exchange_sync_restart_pending = true;
            self.push_log(
                "exchange: sync already running -- will restart when it finishes".to_owned(),
            );
            return;
        }
        self.push_log("exchange: sync started".to_owned());
        self.begin_exchange_sync(cx, true);
    }

    fn begin_exchange_sync(&mut self, cx: &mut Context<Self>, manual: bool) {
        if self.exchange_sync_running {
            return;
        }
        self.exchange_sync_running = true;
        let game = self.settings.active_profile.game;
        let exchange = self.settings.market_tuning(game).exchange.clone();
        let game_key = game.as_str().to_owned();
        self.exchange_sync_generation = self.exchange_sync_generation.wrapping_add(1);
        let generation = self.exchange_sync_generation;

        cx.spawn(async move |this, cx| {
            // 联赛名是总开关：空着整轮跳过，但下一轮照排——用户随时可能填上。
            let outcome = if exchange.league.trim().is_empty() {
                None
            } else {
                let exchange = exchange.clone();
                let game_key = game_key.clone();
                Some(
                    cx.background_executor()
                        .spawn(async move { run_sync_round(&game_key, &exchange) })
                        .await,
                )
            };
            // 没追平就立刻续跑：单轮 48 小时是界不是目标，冷启动的 336 小时
            // 要是每小时才走一轮，追平要七个钟头——首测就是这么卡住的。
            let caught_up = match &outcome {
                Some(Ok(round)) => round.caught_up,
                // 没配联赛：等下一个整点（用户随时可能填上）。
                None => true,
                // 出错半分钟后重试，而不是睡到整点：回补半途一个 CDN 抖动
                // 就把 157 小时的欠账挂一个钟头，看起来和卡死没有区别。
                Some(Err(_)) => false,
            };
            let errored = matches!(&outcome, Some(Err(_)));
            let disposition = this
                .update(cx, |this: &mut AppShell, cx| {
                    // 无论代次是否还有效，这条链的抓取都真的结束了。
                    this.exchange_sync_running = false;
                    if this.exchange_sync_generation != generation {
                        // 旧链：结论作废，待重启的标记留给还活着的那条链。
                        return RoundDisposition {
                            publish_error: false,
                            next_delay: Duration::ZERO,
                        };
                    }
                    // 用户在这轮跑到一半时改了联赛名（设置每轮开头才读，
                    // 所以这轮抓的还是旧联赛）。
                    let restart_pending = std::mem::take(&mut this.exchange_sync_restart_pending);
                    let failures_after = if errored {
                        this.exchange_sync_failures.saturating_add(1)
                    } else {
                        0
                    };
                    let disposition =
                        settle_round(restart_pending, errored, caught_up, failures_after);
                    match outcome {
                        Some(Ok(round)) => {
                            this.exchange_sync_failures = 0;
                            if round.stored > 0 || round.days_folded > 0 || round.repaired > 0 {
                                // 新数据落库了，正开着的页面得知道账变了。
                                this.report_stale = true;
                            }
                            // 联赛下拉的选项来自这里。只在非空时写：一轮全是
                            // "还没发布"的空小时会看到零个联赛，那不是
                            // "联赛都没了"，别把上一轮的好名单擦掉。
                            if !round.leagues_seen.is_empty()
                                && this.settings.market_tuning(game).exchange.leagues_seen
                                    != round.leagues_seen
                            {
                                this.settings
                                    .market_tuning_mut(game)
                                    .exchange
                                    .leagues_seen
                                    .clone_from(&round.leagues_seen);
                                if let Err(error) = this.settings_store.save(&this.settings) {
                                    this.push_log(format!("settings save failed: {error}"));
                                }
                            }
                            // 页面级落点：联赛名可疑要一直挂在交易所页上，
                            // 直到某一轮真的收到了这个联赛的行；其它成功轮清掉旧错。
                            let notice = round.league_name_suspect.then(|| {
                                league_suspect_notice(
                                    this.text(),
                                    round.stored,
                                    &round.leagues_seen,
                                )
                            });
                            if disposition.publish_error {
                                this.exchange_sync_error = notice;
                            }
                            if round.worth_a_log_line() {
                                this.push_log(round.log_line());
                            } else if manual {
                                // 自动轮安静无妨，手动轮必须交代"没事可做"。
                                this.push_log(
                                    "exchange: nothing to fetch -- already at the latest hour"
                                        .to_owned(),
                                );
                            }
                            cx.notify();
                        }
                        Some(Err(error)) => {
                            // 断网只是"下一轮再试"，水位停在原地，补拉天然填洞。
                            // 但原因要留在交易所页上：日志行下一条就被盖掉。
                            if disposition.publish_error {
                                this.exchange_sync_error = Some(error.to_string());
                            }
                            this.exchange_sync_failures = failures_after;
                            this.push_log(format!("exchange: {error}"));
                            cx.notify();
                        }
                        None => {}
                    }
                    disposition
                })
                .unwrap_or(RoundDisposition {
                    publish_error: false,
                    next_delay: Duration::ZERO,
                });
            // 追平了才睡到下一个 HH:05（数据整点后才发布，错开 5 分钟）；
            // 睡过头（系统休眠）也没关系，醒来照样从水位续传。
            cx.background_executor().timer(disposition.next_delay).await;
            this.update(cx, |this: &mut AppShell, cx| {
                if this.exchange_sync_generation == generation {
                    this.begin_exchange_sync(cx, false);
                }
            })
            .ok();
        })
        .detach();
    }
}

/// 抓一个小时的三种下场。正向和回补只关心"要不要继续"，复查段却要把
/// "又查了一次"和"真的补回来了"分开——所以不能只返回一个 bool。
#[derive(Debug, PartialEq, Eq)]
enum Fetched {
    /// 还没发布：这一段收工，下一轮再续。
    Deferred,
    /// 写下了 mark，这一小时确实没有行。
    Empty,
    /// 写下了 mark 和行。
    Rows,
}

/// 这一小时是为哪一段抓的。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    /// 正向补到最新，或往回补历史。
    Sync,
    /// 顺路复查早先记成"确认为空"的小时。
    Recheck,
}

/// "联赛名可疑"的票箱。
#[derive(Default)]
struct SuspectVotes {
    published_hours: usize,
    league_rows: usize,
}

impl SuspectVotes {
    /// 只有正向/回补的小时有投票权。复查段专挑早就确认为空的小时，多半躺在
    /// 赛季开始之前——0 行是它们的常态，不是联赛名的罪证。让它们投票，
    /// 一个已追平、这轮只做了复查的轮次就能凑够三小时零行，
    /// 对完全正确的联赛名喊"检查联赛名"。
    fn record(&mut self, segment: Segment, league_rows: usize) {
        if segment == Segment::Recheck {
            return;
        }
        self.published_hours += 1;
        self.league_rows += league_rows;
    }

    /// 发布了的小时够多、却一行都没筛出来——最常见的原因是联赛名拼错。
    fn suspect(&self) -> bool {
        self.published_hours >= 3 && self.league_rows == 0
    }
}

/// 一轮同步的账目。安静的轮次（没新东西）不占日志——流水灯只留得住一句话。
struct SyncRound {
    stored: usize,
    days_folded: usize,
    days_pruned: usize,
    /// 这轮复查了几个软过期的空小时。
    rechecked: usize,
    /// 其中几个真的补回了数据——发布延迟穿过了三小时护栏的证据。
    repaired: usize,
    /// 小时发布了、这个联赛却一行都没有——最常见的原因是联赛名拼错。
    league_name_suspect: bool,
    /// 数据里实际出现的**全部**联赛名，按市场数从多到少。联赛名错了的时候，
    /// 光说"检查联赛名"没用——POE1 的 3.29 在 CDN 里叫 "Allflame"，
    /// 不叫 "Curse of the Allflame"，用户猜不到，得把正确答案摆出来。
    /// 不截断是因为它同时喂着联赛下拉：用户要追的可能正是沉在底下的
    /// 私人联赛。摆不下是显示那一侧的事，见 `LEAGUE_HINT_LIMIT`。
    leagues_seen: Vec<String>,
    /// 这轮之后水位已到最新。false = 撞上单轮上限，还有历史欠着，
    /// 立刻续跑下一轮而不是睡到整点。
    caught_up: bool,
    /// 这轮结束时手上真实握有的日线天数（不含赛季前的确认空天）。
    total_days: usize,
}

impl SyncRound {
    fn worth_a_log_line(&self) -> bool {
        self.stored > 0
            || self.days_folded > 0
            || self.days_pruned > 0
            || self.repaired > 0
            || self.league_name_suspect
    }

    /// 六测反馈：`folded 1d` 被读成了同步进度。进度就说进度——
    /// 手上共有几天数据，才是用户在等的那个数。
    fn log_line(&self) -> String {
        if self.league_name_suspect {
            let mut line = format!(
                "exchange: {} hours stored but 0 league rows -- check the league name",
                self.stored
            );
            if !self.leagues_seen.is_empty() {
                line.push_str(&format!(
                    " (leagues in the data: {})",
                    league_hint(&self.leagues_seen)
                ));
            }
            return line;
        }
        let mut line = format!("exchange: +{}h, data {}d", self.stored, self.total_days);
        if self.days_pruned > 0 {
            line.push_str(&format!(" (pruned {}d of hourly detail)", self.days_pruned));
        }
        if self.rechecked > 0 {
            // 复查段自己出声，别混进 `+Nh`：那个数是同步进度，这个是补洞。
            line.push_str(&format!(
                " (re-checked {} empty h, refilled {})",
                self.rechecked, self.repaired
            ));
        }
        line
    }
}

/// 纯后台侧：开自己的连接，补到最新，顺手折叠清理。不碰任何界面状态。
fn run_sync_round(
    game: &str,
    exchange: &ptt_settings::ExchangeTuning,
) -> Result<SyncRound, String> {
    use ptt_exchange_history::fetch::ExchangeFetcher;
    use ptt_exchange_history::plan::{
        EmptyHourVerdict, classify_empty, plan_backward, plan_fetch, plan_recheck,
    };

    let league = exchange.league.trim();
    let mut store = ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let now = chrono::Utc::now();
    let watermark = store
        .exchange_watermark(game, league)
        .map_err(|error| format!("watermark: {error}"))?;
    // 四测修正：两个下限**都**生效——不早于赛季起点，也不超过用户要的
    // 天数窗口。此前赛季起点完全接管，"拉取历史(天)"成了摆设；想拉全季
    // 就把天数填大，回补自动停在赛季起点。
    // 读失败要报出来(走同步错误落点),不能当"没有赛季"把回补拉过赛季起点。
    let season = store
        .active_season(game)
        .map_err(|error| format!("season: {error}"))?;
    let floor = season.map(|row| {
        let window_floor =
            now.timestamp() - (exchange.backfill_days as i64).saturating_mul(24 * 3600);
        row.started_at.timestamp().max(window_floor)
    });
    let forward = plan_fetch(
        watermark,
        now.timestamp(),
        floor,
        exchange.backfill_days,
        48,
    );
    // 剩余预算往回补：用户把回补天数改大时，历史从这里长出来。
    // 首测教训：正向计划只会从水位往前走，"设置没生效"就是缺了这半边。
    // 两段分开循环——正向最新小时"还没发布"只该停下正向，不该饿死回补。
    let marks = store
        .list_exchange_hour_marks(game, league)
        .map_err(|error| format!("marks: {error}"))?;
    let backward = if forward.len() < 48 {
        let earliest = marks.first().map(|mark| mark.hour_ts);
        // 已折成完整日线的天不再抓（五测的死循环：小时明细超出保留窗被清，
        // 只认小时 mark 的计划就永远重抓那段）。小时层是脚手架，日线是账本。
        let folded: std::collections::BTreeSet<String> = store
            .list_exchange_day_marks(game, league)
            .map_err(|error| format!("day marks: {error}"))?
            .into_iter()
            .filter(|mark| mark.hour_count >= 24)
            .map(|mark| mark.utc_day)
            .collect();
        plan_backward(
            earliest,
            now.timestamp(),
            floor,
            exchange.backfill_days,
            48 - forward.len(),
            |hour_ts| {
                chrono::DateTime::from_timestamp(hour_ts, 0)
                    .map(|ts| ts.format("%Y-%m-%d").to_string())
                    .is_some_and(|day| folded.contains(&day))
            },
        )
    } else {
        Vec::new()
    };
    // 复查一小撮软过期的空小时。三小时护栏之外写下的"确认为空"可能只是
    // 官方发布晚了，一天后再看一眼几乎零成本；预算刻意压到 4，复查是
    // 顺路修补，不该和正事抢这一轮的请求。
    let recheck = plan_recheck(
        &marks
            .iter()
            .map(|mark| (mark.hour_ts, mark.market_count, mark.fetched_at.timestamp()))
            .collect::<Vec<_>>(),
        now.timestamp(),
        4,
    );

    let fetcher = ExchangeFetcher::new();
    let planned = forward.len() + backward.len();
    let mut stored = 0usize;
    let mut votes = SuspectVotes::default();
    // 联赛名单相反：复查段见到的名字照收。下拉多一个名字总比少一个好。
    let mut league_counts = std::collections::BTreeMap::<String, usize>::new();
    let mut throttled = false;
    let mut fetch_one = |hour_ts: i64,
                         segment: Segment,
                         store: &mut ptt_storage::MarketStore|
     -> Result<Fetched, String> {
        if throttled {
            // 对公开 CDN 的礼貌间隔。六测嫌慢，从 250ms 降到 100ms——
            // 串行请求本身就是限速，这只是别贴脸的余量。
            std::thread::sleep(Duration::from_millis(100));
        }
        throttled = true;
        let bytes = fetcher
            .fetch_hour(game, hour_ts as u64)
            .map_err(|error| format!("fetch {hour_ts}: {error}"))?;
        let hour = ptt_exchange_history::parse_hour(&bytes)
            .map_err(|error| format!("parse {hour_ts}: {error}"))?;
        if hour.markets.is_empty() {
            match classify_empty(hour_ts, now.timestamp()) {
                // 可能只是还没发布：不写 mark，这一段收工，下一轮再续。
                EmptyHourVerdict::RetryLater => return Ok(Fetched::Deferred),
                EmptyHourVerdict::ConfirmedEmpty => {
                    // 重写 mark 也刷新 fetched_at，复查的一周地平线就是
                    // 靠这个往前走的：查一次、冷却一天，六次之后自然冻结。
                    store
                        .replace_exchange_hour(game, league, hour_ts, &[], now)
                        .map_err(|error| format!("store {hour_ts}: {error}"))?;
                    return Ok(Fetched::Empty);
                }
            }
        }
        for row in &hour.markets {
            *league_counts.entry(row.league.clone()).or_default() += 1;
        }
        let rows: Vec<ptt_storage::ExchangeHourMarketRow> = hour
            .rows_for_league(league)
            .map(|row| to_storage_row(hour_ts, row))
            .collect();
        votes.record(segment, rows.len());
        store
            .replace_exchange_hour(game, league, hour_ts, &rows, now)
            .map_err(|error| format!("store {hour_ts}: {error}"))?;
        Ok(league_hour_verdict(rows.len()))
    };
    for hour_ts in &forward {
        match fetch_one(*hour_ts, Segment::Sync, &mut store)? {
            Fetched::Deferred => break,
            _ => stored += 1,
        }
    }
    for hour_ts in &backward {
        // 回补的小时都足够老，不会撞"还没发布"；撞上也照常跳段。
        match fetch_one(*hour_ts, Segment::Sync, &mut store)? {
            Fetched::Deferred => break,
            _ => stored += 1,
        }
    }
    let mut rechecked = 0usize;
    let mut repaired = 0usize;
    let mut repaired_days = std::collections::BTreeSet::<String>::new();
    for hour_ts in &recheck {
        rechecked += 1;
        // 复查不算"同步进度"：还是空的那一次只刷新了冷却时间，把它记进
        // `stored` 会让日志每轮都报"+4h"，用户读到的是假的进度。
        if let Fetched::Rows = fetch_one(*hour_ts, Segment::Recheck, &mut store)? {
            repaired += 1;
            repaired_days.insert(utc_day_of(*hour_ts));
        }
    }

    // 折叠和清理搭这班车，不再开第二个定时器。
    let fold = ptt_runtime::exchange_rollup::ensure_exchange_day_rollups(
        &mut store,
        game,
        league,
        now,
        32,
        &repaired_days,
    )?;
    let prune = ptt_runtime::exchange_rollup::prune_exchange_hours(
        &mut store,
        game,
        league,
        now,
        exchange.hour_retention_days,
    )?;
    // 手上共有几天日线（赛季前的确认空天不算"数据"）。
    let total_days = store
        .list_exchange_day_marks(game, league)
        .map(|marks| marks.iter().filter(|mark| mark.market_count > 0).count())
        .unwrap_or(0);
    Ok(SyncRound {
        stored,
        days_folded: fold.days_processed.len(),
        days_pruned: prune.days_deleted.len(),
        rechecked,
        repaired,
        total_days,
        league_name_suspect: votes.suspect(),
        leagues_seen: top_leagues(league_counts, usize::MAX),
        // 计划排满 48（正向或回补被预算截断）就必然还有欠账；
        // 计划不满时，即便最新小时"还没发布"（deferred），剩下的也只有
        // 等发布这一件事，照常睡到整点。恰好整 48 的边界多空转一轮，无害。
        caught_up: planned < 48,
    })
}

/// 一个已发布小时的下场，只按**这个联赛**筛出来的行数算。
///
/// CDN 的小时文件装着所有联赛，"文件里有市场"回答的是"这一小时发布了没有"；
/// 复查段问的却是另一件事——那个洞补上了吗。拿前者当后者，复查就会把
/// 筛完仍是 0 行的小时报成"refilled"，白白重折那一天还把页面标脏。
/// 探针 `--audit` 一直是对的那一侧：筛完 `rows.is_empty()` 就跳过。
fn league_hour_verdict(league_rows: usize) -> Fetched {
    if league_rows == 0 {
        Fetched::Empty
    } else {
        Fetched::Rows
    }
}

/// 小时属于哪个 UTC 日。日折的键是这个字符串，补洞后要照它点名重折。
fn utc_day_of(hour_ts: i64) -> String {
    chrono::DateTime::from_timestamp(hour_ts, 0)
        .map_or_else(|| "?".to_owned(), |ts| ts.format("%Y-%m-%d").to_string())
}

fn to_storage_row(
    hour_ts: i64,
    row: &ptt_exchange_history::MarketRow,
) -> ptt_storage::ExchangeHourMarketRow {
    ptt_storage::ExchangeHourMarketRow {
        hour_ts,
        asset_a: row.asset_a.clone(),
        asset_b: row.asset_b.clone(),
        volume_a: row.volume_a,
        volume_b: row.volume_b,
        lowest_stock_a: row.lowest_stock_a,
        lowest_stock_b: row.lowest_stock_b,
        highest_stock_a: row.highest_stock_a,
        highest_stock_b: row.highest_stock_b,
        lowest_ratio_a: row.lowest_ratio_a.clone(),
        lowest_ratio_b: row.lowest_ratio_b.clone(),
        highest_ratio_a: row.highest_ratio_a.clone(),
        highest_ratio_b: row.highest_ratio_b.clone(),
    }
}

/// 联赛名可疑时挂在交易所页上的那句话。
///
/// 和 `log_line` 分开写：日志行按惯例是英文，而页面这句是用户照着改设置的
/// 说明书——界面切成中文，它就得是中文。联赛名本身不翻译，它们要被原样
/// 抄回设置框里。
fn league_suspect_notice(text: &crate::i18n::Text, stored: usize, leagues: &[String]) -> String {
    let mut notice =
        ptt_runtime::report_text::fill(text.exchange_league_rows_missing, &[&stored.to_string()]);
    if !leagues.is_empty() {
        notice.push_str(" · ");
        notice.push_str(&ptt_runtime::report_text::fill(
            text.exchange_leagues_in_data,
            &[&league_hint(leagues)],
        ));
    }
    notice
}

/// 一行日志和一句页面提示各自摆得下几个联赛名。
///
/// 全量名单归 `leagues_seen`（下拉要它），这里只管"读得完"：一个赛季的
/// CDN 里几十个私人联赛，全铺出来就没人会去读那句话了。
const LEAGUE_HINT_LIMIT: usize = 5;

fn league_hint(leagues: &[String]) -> String {
    leagues
        .iter()
        .take(LEAGUE_HINT_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
}

/// 联赛名按市场数从多到少排：正式联赛和标准都在前面，私人联赛（PL 编号）
/// 市场少，自然沉底。`limit` 是给提示用的；下拉要全量，传 `usize::MAX`。
fn top_leagues(counts: std::collections::BTreeMap<String, usize>, limit: usize) -> Vec<String> {
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(limit)
        .map(|(name, _)| name)
        .collect()
}

/// 一轮跑完时的处置：这轮的结论还算不算数，下一轮什么时候开。
#[derive(Debug, PartialEq, Eq)]
struct RoundDisposition {
    /// 把这轮的失败原因挂到交易所页上。
    publish_error: bool,
    next_delay: Duration,
}

/// 待重启的一轮（用户中途改了联赛名）跑完时，结论作废、立刻再来一轮。
///
/// 换联赛就是换一本账：旧账上的"联赛名可疑"贴在新名字旁边，用户只会
/// 以为新名字也错了；而每轮开头都会重读设置，所以立刻续跑的那一轮
/// 用的就是刚存下的联赛名，不必等到下个整点。
fn settle_round(
    restart_pending: bool,
    errored: bool,
    caught_up: bool,
    consecutive_failures: u32,
) -> RoundDisposition {
    if restart_pending {
        RoundDisposition {
            publish_error: false,
            next_delay: Duration::ZERO,
        }
    } else {
        RoundDisposition {
            publish_error: true,
            next_delay: retry_delay(errored, caught_up, consecutive_failures),
        }
    }
}

/// 出错半分钟后重试；连续三次都失败就退到下一个整点过五分。永久性错误
/// （联赛不存在、某小时永远解析失败）每 30 秒敲一次 CDN 只是稳定地制造噪音，
/// 而原因已经挂在交易所页上，不需要靠频繁重试来提醒。
fn retry_delay(errored: bool, caught_up: bool, consecutive_failures: u32) -> Duration {
    if errored {
        if consecutive_failures >= 3 {
            until_next_five_past()
        } else {
            Duration::from_secs(30)
        }
    } else if caught_up {
        until_next_five_past()
    } else {
        Duration::from_secs(1)
    }
}

/// 距下一个"整点过五分"的时长。最少也睡一分钟，防止边界上打转。
fn until_next_five_past() -> Duration {
    let now = chrono::Utc::now().timestamp();
    let next = (now.div_euclid(3600) + 1) * 3600 + 300;
    Duration::from_secs(u64::try_from(next - now).unwrap_or(60).max(60))
}

#[cfg(test)]
mod exchange_sync_tests {
    use super::*;

    #[test]
    fn repeated_failures_back_off_to_the_hourly_slot() {
        // 一次 CDN 抖动半分钟后重试是对的；联赛不存在这种永久性错误
        // 每 30 秒敲一次只是稳定地制造噪音，三次之后退到整点过五分。
        assert_eq!(retry_delay(true, false, 1), Duration::from_secs(30));
        assert_eq!(retry_delay(true, false, 2), Duration::from_secs(30));
        assert!(retry_delay(true, false, 3) >= Duration::from_secs(60));
        assert_eq!(retry_delay(false, false, 9), Duration::from_secs(1));
        assert!(retry_delay(false, true, 0) >= Duration::from_secs(60));
    }

    /// 用户改联赛名时正好有一轮在跑：那轮抓的是旧联赛，它的结论
    /// （"联赛名可疑"、超时）说的是旧账，挂在页面上会让用户以为
    /// 刚改的名字也不对。新名字也不该等到下个整点才生效。
    #[test]
    fn a_pending_restart_drops_the_old_rounds_verdict_and_starts_over_at_once() {
        let pending = settle_round(true, true, false, 1);
        assert!(!pending.publish_error);
        assert_eq!(pending.next_delay, Duration::ZERO);
        let normal = settle_round(false, true, false, 1);
        assert!(normal.publish_error);
        assert_eq!(normal.next_delay, retry_delay(true, false, 1));
        let caught_up = settle_round(false, false, true, 0);
        assert!(caught_up.publish_error);
        assert_eq!(caught_up.next_delay, retry_delay(false, true, 0));
    }

    #[test]
    fn quiet_rounds_stay_out_of_the_log() {
        let quiet = SyncRound {
            stored: 0,
            days_folded: 0,
            days_pruned: 0,
            rechecked: 0,
            repaired: 0,
            league_name_suspect: false,
            caught_up: true,
            total_days: 30,
            leagues_seen: Vec::new(),
        };
        assert!(!quiet.worth_a_log_line());
    }

    #[test]
    fn a_suspect_league_name_always_speaks_up() {
        let suspect = SyncRound {
            stored: 5,
            days_folded: 0,
            days_pruned: 0,
            rechecked: 0,
            repaired: 0,
            league_name_suspect: true,
            caught_up: true,
            total_days: 0,
            leagues_seen: Vec::new(),
        };
        assert!(suspect.worth_a_log_line());
        assert!(suspect.log_line().contains("league name"));
    }

    /// 用户把 POE1 联赛填成 "Curse of the Allflame"，CDN 里其实叫 "Allflame"：
    /// 提示必须把数据里看到的名字列出来，不然用户只能一个个瞎猜。
    #[test]
    fn a_suspect_league_name_lists_the_leagues_actually_seen() {
        let suspect = SyncRound {
            stored: 5,
            days_folded: 0,
            days_pruned: 0,
            rechecked: 0,
            repaired: 0,
            league_name_suspect: true,
            caught_up: true,
            total_days: 0,
            leagues_seen: vec!["Allflame".to_owned(), "Standard".to_owned()],
        };
        let line = suspect.log_line();
        assert!(line.contains("Allflame, Standard"), "{line}");
    }

    /// 日志行按约定留英文，但页面上那句是用户照着改设置的说明书：
    /// 界面切成中文，它也得是中文。联赛名本身不翻译（要照抄进设置框）。
    #[test]
    fn the_page_notice_speaks_the_interface_language() {
        let leagues = vec!["Allflame".to_owned(), "Standard".to_owned()];
        let english = league_suspect_notice(
            crate::i18n::text(ptt_settings::UiLanguage::English),
            48,
            &leagues,
        );
        assert!(english.contains("check the league name"), "{english}");
        assert!(english.contains("Allflame, Standard"), "{english}");
        let chinese = league_suspect_notice(
            crate::i18n::text(ptt_settings::UiLanguage::Chinese),
            48,
            &leagues,
        );
        assert!(chinese.contains("联赛名"), "{chinese}");
        assert!(chinese.contains("Allflame, Standard"), "{chinese}");
        assert!(!chinese.contains("check the league name"), "{chinese}");
        // 一个联赛都没见到（比如整轮都是还没发布的空小时）就别摆空名单。
        let bare = league_suspect_notice(
            crate::i18n::text(ptt_settings::UiLanguage::English),
            48,
            &[],
        );
        assert!(bare.contains("48"), "{bare}");
        assert!(!bare.contains('·'), "{bare}");
    }

    #[test]
    fn top_leagues_rank_by_market_count_then_name() {
        let counts = std::collections::BTreeMap::from([
            ("Standard".to_owned(), 415),
            ("Allflame".to_owned(), 1465),
            ("Bored BroSF III (PL85538)".to_owned(), 1),
            ("Hardcore Allflame".to_owned(), 323),
        ]);
        assert_eq!(
            top_leagues(counts, 3),
            vec!["Allflame", "Standard", "Hardcore Allflame"]
        );
    }

    /// 联赛下拉要列全：用户想追的很可能就是沉在底下的私人联赛（PLxxxxx）
    /// 或硬核分账，砍到前五就等于告诉他"你那个联赛不存在"。
    /// 于是这一轮记住全部，而"摆不下"的责任落到显示那一侧——
    /// 一行日志和一句页面提示都塞不进四十个名字。
    #[test]
    fn every_league_in_the_data_survives_not_just_the_top_five() {
        let counts = std::collections::BTreeMap::from([
            ("Allflame".to_owned(), 1465),
            ("Standard".to_owned(), 415),
            ("Hardcore Allflame".to_owned(), 323),
            ("Hardcore".to_owned(), 120),
            ("Solo Self-Found Allflame".to_owned(), 60),
            ("Bored BroSF III (PL85538)".to_owned(), 1),
            ("Zzz Private (PL99999)".to_owned(), 1),
        ]);
        let all = top_leagues(counts, usize::MAX);
        assert_eq!(all.len(), 7);
        // 排序不变：正式联赛在前，私人联赛沉底。
        assert_eq!(all[0], "Allflame");
        assert_eq!(all.last().unwrap(), "Zzz Private (PL99999)");

        // 记全了，但两处提示各自只摆前五。
        let round = SyncRound {
            stored: 5,
            days_folded: 0,
            days_pruned: 0,
            rechecked: 0,
            repaired: 0,
            league_name_suspect: true,
            caught_up: true,
            total_days: 0,
            leagues_seen: all.clone(),
        };
        let line = round.log_line();
        assert!(line.contains("Solo Self-Found Allflame"), "{line}");
        assert!(!line.contains("PL99999"), "{line}");
        let notice = league_suspect_notice(
            crate::i18n::text(ptt_settings::UiLanguage::English),
            5,
            &all,
        );
        assert!(notice.contains("Solo Self-Found Allflame"), "{notice}");
        assert!(!notice.contains("PL99999"), "{notice}");
    }

    /// CDN 的小时文件装的是**所有**联赛。文件里有市场，不等于我们这个联赛
    /// 有行——复查段挑的正是"文件有市场、本联赛 0 行"那种 mark，所以拿
    /// "文件非空"当修复成功，等于每次复查都宣布洞补好了：日志报
    /// "refilled 4"、那天被强制重折、页面被标脏，而洞一个都没少。
    /// 探针 `--audit` 一直是对的那一侧（筛完 `rows.is_empty()` 就 continue）。
    #[test]
    fn an_hour_counts_as_repaired_only_when_this_league_got_rows_back() {
        assert_eq!(league_hour_verdict(0), Fetched::Empty);
        assert_eq!(league_hour_verdict(7), Fetched::Rows);
    }

    /// 复查段专挑早就确认为空的小时，多半躺在赛季开始之前——它们 0 行是常态。
    /// 让它们投票，一个"已追平、这轮只做了复查"的轮次就会凑够三小时、
    /// 零行，对完全正确的联赛名喊"检查联赛名"（`stored == 0` 时那句还会
    /// 写成"0 hours stored but 0 league rows"，读起来更莫名其妙）。
    #[test]
    fn rechecked_hours_do_not_vote_on_whether_the_league_name_is_wrong() {
        let mut recheck_only = SuspectVotes::default();
        for _ in 0..4 {
            recheck_only.record(Segment::Recheck, 0);
        }
        assert!(!recheck_only.suspect());

        // 正向段的空小时照旧该报警：那些小时是"应该有我们联赛的数据"的。
        let mut forward = SuspectVotes::default();
        for _ in 0..3 {
            forward.record(Segment::Sync, 0);
        }
        assert!(forward.suspect());

        // 只要正向段筛出过一行，联赛名就不冤枉。
        let mut with_rows = SuspectVotes::default();
        with_rows.record(Segment::Sync, 12);
        with_rows.record(Segment::Sync, 0);
        with_rows.record(Segment::Sync, 0);
        assert!(!with_rows.suspect());
    }

    #[test]
    fn next_wakeup_is_between_one_minute_and_an_hour_and_five() {
        let delay = until_next_five_past();
        assert!(delay >= Duration::from_secs(60));
        assert!(delay <= Duration::from_secs(3600 + 300));
    }
}
