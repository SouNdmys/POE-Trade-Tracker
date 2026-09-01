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
        self.begin_exchange_sync(cx, false);
    }

    /// 手动来一轮（按钮/设置变更）。`begin` 自己会前进代次，旧链在下一次
    /// 醒来时发现代次不对就安静退场。手动轮永远出声——四测反馈：
    /// 点了没反馈，分不清生效、无事可做还是网断了。
    pub(crate) fn restart_exchange_sync(&mut self, cx: &mut Context<Self>) {
        self.push_log("exchange: sync started".to_owned());
        self.begin_exchange_sync(cx, true);
    }

    fn begin_exchange_sync(&mut self, cx: &mut Context<Self>, manual: bool) {
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
            this.update(cx, |this: &mut AppShell, cx| {
                if this.exchange_sync_generation != generation {
                    return;
                }
                match outcome {
                    Some(Ok(round)) => {
                        if round.stored > 0 || round.days_folded > 0 {
                            // 新数据落库了，正开着的页面得知道账变了。
                            this.report_stale = true;
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
                        this.push_log(format!("exchange: {error}"));
                        cx.notify();
                    }
                    None => {}
                }
            })
            .ok();
            // 追平了才睡到下一个 HH:05（数据整点后才发布，错开 5 分钟）；
            // 睡过头（系统休眠）也没关系，醒来照样从水位续传。
            let delay = if errored {
                Duration::from_secs(30)
            } else if caught_up {
                until_next_five_past()
            } else {
                Duration::from_secs(1)
            };
            cx.background_executor().timer(delay).await;
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

/// 一轮同步的账目。安静的轮次（没新东西）不占日志——流水灯只留得住一句话。
struct SyncRound {
    stored: usize,
    days_folded: usize,
    days_pruned: usize,
    /// 小时发布了、这个联赛却一行都没有——最常见的原因是联赛名拼错。
    league_name_suspect: bool,
    /// 这轮之后水位已到最新。false = 撞上单轮上限，还有历史欠着，
    /// 立刻续跑下一轮而不是睡到整点。
    caught_up: bool,
}

impl SyncRound {
    fn worth_a_log_line(&self) -> bool {
        self.stored > 0 || self.days_folded > 0 || self.days_pruned > 0 || self.league_name_suspect
    }

    fn log_line(&self) -> String {
        if self.league_name_suspect {
            return format!(
                "exchange: {} hours stored but 0 league rows -- check the league name",
                self.stored
            );
        }
        format!(
            "exchange: +{}h (folded {}d, pruned {}d)",
            self.stored, self.days_folded, self.days_pruned
        )
    }
}

/// 纯后台侧：开自己的连接，补到最新，顺手折叠清理。不碰任何界面状态。
fn run_sync_round(
    game: &str,
    exchange: &ptt_settings::ExchangeTuning,
) -> Result<SyncRound, String> {
    use ptt_exchange_history::fetch::ExchangeFetcher;
    use ptt_exchange_history::plan::{EmptyHourVerdict, classify_empty, plan_backward, plan_fetch};

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
    let floor = store.active_season(game).ok().flatten().map(|row| {
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
    let backward = if forward.len() < 48 {
        let earliest = store
            .list_exchange_hour_marks(game, league)
            .map_err(|error| format!("marks: {error}"))?
            .first()
            .map(|mark| mark.hour_ts);
        plan_backward(
            earliest,
            now.timestamp(),
            floor,
            exchange.backfill_days,
            48 - forward.len(),
        )
    } else {
        Vec::new()
    };

    let fetcher = ExchangeFetcher::new();
    let planned = forward.len() + backward.len();
    let mut stored = 0usize;
    let mut league_rows = 0usize;
    let mut published_hours = 0usize;
    let mut throttled = false;
    let mut fetch_one =
        |hour_ts: i64, store: &mut ptt_storage::MarketStore| -> Result<bool, String> {
            if throttled {
                // 对公开 CDN 的礼貌节流；历史不可变，不赶时间。
                std::thread::sleep(Duration::from_millis(250));
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
                    EmptyHourVerdict::RetryLater => return Ok(false),
                    EmptyHourVerdict::ConfirmedEmpty => {
                        store
                            .replace_exchange_hour(game, league, hour_ts, &[], now)
                            .map_err(|error| format!("store {hour_ts}: {error}"))?;
                        stored += 1;
                    }
                }
            } else {
                published_hours += 1;
                let rows: Vec<ptt_storage::ExchangeHourMarketRow> = hour
                    .rows_for_league(league)
                    .map(|row| to_storage_row(hour_ts, row))
                    .collect();
                league_rows += rows.len();
                store
                    .replace_exchange_hour(game, league, hour_ts, &rows, now)
                    .map_err(|error| format!("store {hour_ts}: {error}"))?;
                stored += 1;
            }
            Ok(true)
        };
    for hour_ts in &forward {
        if !fetch_one(*hour_ts, &mut store)? {
            break;
        }
    }
    for hour_ts in &backward {
        // 回补的小时都足够老，不会撞"还没发布"；撞上也照常跳段。
        if !fetch_one(*hour_ts, &mut store)? {
            break;
        }
    }

    // 折叠和清理搭这班车，不再开第二个定时器。
    let fold = ptt_runtime::exchange_rollup::ensure_exchange_day_rollups(
        &mut store, game, league, now, 32,
    )?;
    let prune = ptt_runtime::exchange_rollup::prune_exchange_hours(
        &mut store,
        game,
        league,
        now,
        exchange.hour_retention_days,
    )?;
    Ok(SyncRound {
        stored,
        days_folded: fold.days_processed.len(),
        days_pruned: prune.days_deleted.len(),
        league_name_suspect: published_hours >= 3 && league_rows == 0,
        // 计划排满 48（正向或回补被预算截断）就必然还有欠账；
        // 计划不满时，即便最新小时"还没发布"（deferred），剩下的也只有
        // 等发布这一件事，照常睡到整点。恰好整 48 的边界多空转一轮，无害。
        caught_up: planned < 48,
    })
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
    fn quiet_rounds_stay_out_of_the_log() {
        let quiet = SyncRound {
            stored: 0,
            days_folded: 0,
            days_pruned: 0,
            league_name_suspect: false,
            caught_up: true,
        };
        assert!(!quiet.worth_a_log_line());
    }

    #[test]
    fn a_suspect_league_name_always_speaks_up() {
        let suspect = SyncRound {
            stored: 5,
            days_folded: 0,
            days_pruned: 0,
            league_name_suspect: true,
            caught_up: true,
        };
        assert!(suspect.worth_a_log_line());
        assert!(suspect.log_line().contains("league name"));
    }

    #[test]
    fn next_wakeup_is_between_one_minute_and_an_hour_and_five() {
        let delay = until_next_five_past();
        assert!(delay >= Duration::from_secs(60));
        assert!(delay <= Duration::from_secs(3600 + 300));
    }
}
