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
        self.begin_exchange_sync(cx);
    }

    fn begin_exchange_sync(&mut self, cx: &mut Context<Self>) {
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
            this.update(cx, |this: &mut AppShell, cx| {
                if this.exchange_sync_generation != generation {
                    return;
                }
                match outcome {
                    Some(Ok(round)) if round.worth_a_log_line() => {
                        this.push_log(round.log_line());
                        cx.notify();
                    }
                    Some(Err(error)) => {
                        // 断网只是"下一轮再试"，水位停在原地，补拉天然填洞。
                        this.push_log(format!("exchange: {error}"));
                        cx.notify();
                    }
                    _ => {}
                }
            })
            .ok();
            // 睡到下一个 HH:05：数据整点后才发布且有延迟，错开 5 分钟。
            // 睡过头（系统休眠）也没关系，醒来这轮照样从水位续传。
            cx.background_executor().timer(until_next_five_past()).await;
            this.update(cx, |this: &mut AppShell, cx| {
                if this.exchange_sync_generation == generation {
                    this.begin_exchange_sync(cx);
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
    use ptt_exchange_history::plan::{EmptyHourVerdict, classify_empty, plan_fetch};

    let league = exchange.league.trim();
    let mut store = ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let now = chrono::Utc::now();
    let watermark = store
        .exchange_watermark(game, league)
        .map_err(|error| format!("watermark: {error}"))?;
    // 配置过赛季就从赛季起点回补，没配就用默认窗口（backfill_days）。
    let floor = store
        .active_season(game)
        .ok()
        .flatten()
        .map(|row| row.started_at.timestamp());
    let hours = plan_fetch(
        watermark,
        now.timestamp(),
        floor,
        exchange.backfill_days,
        48,
    );

    let fetcher = ExchangeFetcher::new();
    let mut stored = 0usize;
    let mut league_rows = 0usize;
    let mut published_hours = 0usize;
    for (index, hour_ts) in hours.iter().enumerate() {
        if index > 0 {
            // 对公开 CDN 的礼貌节流；历史不可变，不赶时间。
            std::thread::sleep(Duration::from_millis(250));
        }
        let bytes = fetcher
            .fetch_hour(game, *hour_ts as u64)
            .map_err(|error| format!("fetch {hour_ts}: {error}"))?;
        let hour = ptt_exchange_history::parse_hour(&bytes)
            .map_err(|error| format!("parse {hour_ts}: {error}"))?;
        if hour.markets.is_empty() {
            match classify_empty(*hour_ts, now.timestamp()) {
                // 可能只是还没发布：不写 mark，收工，下一轮从这里续。
                EmptyHourVerdict::RetryLater => break,
                EmptyHourVerdict::ConfirmedEmpty => {
                    store
                        .replace_exchange_hour(game, league, *hour_ts, &[], now)
                        .map_err(|error| format!("store {hour_ts}: {error}"))?;
                    stored += 1;
                }
            }
        } else {
            published_hours += 1;
            let rows: Vec<ptt_storage::ExchangeHourMarketRow> = hour
                .rows_for_league(league)
                .map(|row| to_storage_row(*hour_ts, row))
                .collect();
            league_rows += rows.len();
            store
                .replace_exchange_hour(game, league, *hour_ts, &rows, now)
                .map_err(|error| format!("store {hour_ts}: {error}"))?;
            stored += 1;
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
