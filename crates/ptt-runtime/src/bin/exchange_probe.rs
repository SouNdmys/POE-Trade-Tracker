//! 官方通货历史 API 的验证探针（阶段 0 spike 的验证面）。
//!
//! 在任何数据进库、进页面之前，先用它确认四件事：端点行为和我们理解的
//! 一致（空小时、next_change_id 连续性）、解析扛得住真实数据、映射覆盖率
//! 够用、以及 API 数据算出的趋势和 OCR 实测对得上账。
//!
//! Usage:
//!   `exchange_probe --fetch --hours N --league "<联赛名>" [--cache DIR]`
//!       从最新完整小时往回抓 N 小时，原始字节落盘缓存（CDN 数据不可变，
//!       缓存永不过期），重复运行只补缺的。
//!   `exchange_probe --ingest --league "<联赛名>" [--cache DIR]`
//!       把缓存里这个联赛的小时写进生产表并折出日线：app 同步的写路径镜像，
//!       让一个联赛不开 GUI 也能从零跑到能看页面（换个 LOCALAPPDATA 就是干净沙盒）。
//!   `exchange_probe --paths --league "<联赛名>" [--cache DIR] [--top N]`
//!       聚合缓存，按当前锚计价成交量降序输出资产路径 = 映射工作清单。
//!   `exchange_probe --trend --league "<联赛名>" [--cache DIR] [--top N]`
//!       小时 VWAP → 日折 → 近 2 天 vs 基线的趋势 bps（原始 + 扣市场中位），
//!       同时给出成交量口径与挂单库存口径的两份流动性读数，供证据分工裁定。
//!   `exchange_probe --reconcile`
//!       镜像交易所页的「面板核对」条：同一个模型函数、同一个窗口、同样逐点查
//!       生产小时表，打印每对的越界率与典型偏离。
//!   `exchange_probe --status`
//!       读生产表：水位、近 48 小时覆盖、日折/清理进度、映射覆盖率。
//!       镜像 app 同步的读路径，app 里看到的数字和这里对不上就是漂移。
//!   `exchange_probe --audit`
//!       复查 market_count=0 的小时 mark：CDN 不可变、可重查，假空直接
//!       重抓覆写修复（replace 连 mark 带行一起换，不需要删除原语）。
//!   `exchange_probe --export [--out DIR]`
//!       镜像交易所页的导出按钮：读生产日线表，写 CSV + JSON 到 DIR
//!       （默认数据库旁的 exports/），并打印行数与几行样本核对数量级。
//!   `exchange_probe --radar`
//!       镜像雷达页「交易所雷达」段：读近 48 小时的小时行，按官方成交均价跑
//!       与抓取雷达同一套环路搜索，打印耗时、数据小时与每条环。
//!   `exchange_probe --series --asset <目录 id> [--range 24h|3d|7d|all]`
//!       镜像交易所页的明细栏与时段档位：按水位读整段保留期的精简小时行，建小时
//!       账本（打印读库/建账耗时），打印该资产逐小时的价与成交额、峰值时段，
//!       再按同一档位重排表格打印前 15 行——页面上点一行/切一档看到的就是这些。
//!
//! `--reconcile`/`--status`/`--audit`/`--export`/`--radar`/`--series` 的联赛默认取设置里的 `exchange.league`，
//! `--league` 可覆盖；其余子命令仍要求显式 `--league`。

#[cfg(windows)]
mod probe {
    use std::collections::{BTreeMap, BTreeSet};

    use ptt_exchange_history::fetch::ExchangeFetcher;
    use ptt_exchange_history::{HourSnapshot, MarketRow, parse_hour};

    pub fn run() -> Result<(), String> {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let has = |name: &str| arguments.iter().any(|argument| argument == name);
        let option = |name: &str| -> Option<String> {
            arguments
                .iter()
                .position(|argument| argument == name)
                .and_then(|index| arguments.get(index + 1))
                .cloned()
        };
        let any_command = has("--fetch")
            || has("--ingest")
            || has("--paths")
            || has("--trend")
            || has("--reconcile")
            || has("--status")
            || has("--audit")
            || has("--export")
            || has("--radar")
            || has("--series");
        if !any_command {
            return Err("nothing to do: see the file header for subcommands".to_owned());
        }

        let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
        let settings = ptt_settings::SettingsStore::release_default_from(&local)
            .load()
            .settings;
        let configured_league = settings
            .market_tuning(settings.active_profile.game)
            .exchange
            .league
            .trim()
            .to_owned();
        let league = option("--league")
            .or_else(|| (!configured_league.is_empty()).then(|| configured_league.clone()))
            .ok_or("--league \"<联赛英文名>\" is required (or set it in Settings)")?;
        let hours: u64 = option("--hours")
            .unwrap_or_else(|| "48".to_owned())
            .parse()
            .map_err(|error| format!("--hours: {error}"))?;
        let top: usize = option("--top")
            .unwrap_or_else(|| "60".to_owned())
            .parse()
            .map_err(|error| format!("--top: {error}"))?;

        let realm = settings.active_profile.game.as_str().to_owned();

        let cache_dir = option("--cache").map_or_else(
            || local.join("PoeTradeTracker").join("exchange-cache"),
            std::path::PathBuf::from,
        );
        std::fs::create_dir_all(&cache_dir).map_err(|error| format!("cache dir: {error}"))?;

        let game = settings.active_profile.game;
        let tuning = settings.market_tuning(game);
        let anchor = ptt_runtime::reports::exchange_anchor(&tuning)?;
        let anchor_path = ptt_runtime::reports::exchange_path_of(game, &anchor)?
            .ok_or_else(|| format!("anchor {anchor} has no GGG path in the {game:?} mapping"))?;
        let bridge_path = tuning
            .settlement_assets
            .iter()
            .filter_map(|slug| ptt_runtime::live::domain_asset_id(slug).ok())
            .find(|asset| *asset != anchor)
            .and_then(|bridge| {
                ptt_runtime::reports::exchange_path_of(game, &bridge)
                    .ok()
                    .flatten()
            });
        let session = Session {
            realm,
            game,
            anchor,
            anchor_path,
            bridge_path,
            league,
            cache_dir,
            fetcher: ExchangeFetcher::new(),
        };

        if has("--fetch") {
            session.fetch(hours)?;
        }
        if has("--ingest") {
            session.ingest()?;
        }
        if has("--paths") {
            session.paths(top)?;
        }
        if has("--trend") {
            session.trend(if has("--top") { top } else { 15 })?;
        }
        if has("--reconcile") {
            session.reconcile()?;
        }
        if has("--status") {
            session.status()?;
        }
        if has("--audit") {
            session.audit()?;
        }
        if has("--export") {
            session.export(option("--out").map(std::path::PathBuf::from))?;
        }
        if has("--radar") {
            session.radar()?;
        }
        if has("--series") {
            session.series(option("--asset"), option("--range"))?;
        }
        Ok(())
    }

    impl Session {
        /// 镜像雷达页「交易所雷达」段：同一个模型函数，多打印耗时——
        /// 大雷达的图是全连接的，先在真实库上量一次，再决定要不要按小时缓存。
        fn radar(&self) -> Result<(), String> {
            let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
            let settings = ptt_settings::SettingsStore::release_default_from(&local)
                .load()
                .settings;
            let tuning = settings.market_tuning(settings.active_profile.game);
            let store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let now = chrono::Utc::now();
            let hour_rows = store
                .load_exchange_hours(
                    &self.realm,
                    &self.league,
                    now.timestamp() - 48 * 3600,
                    now.timestamp(),
                )
                .map_err(|error| format!("hours: {error}"))?;
            let started = std::time::Instant::now();
            let mut model = ptt_runtime::reports::exchange_radar_model(
                &hour_rows,
                &self.league,
                self.game,
                &tuning,
                now,
            )?;
            let elapsed = started.elapsed();
            let watermark = store
                .exchange_watermark(&self.realm, &self.league)
                .map_err(|error| format!("watermark: {error}"))?;
            let newest_complete = now.timestamp().div_euclid(3600) * 3600 - 3600;
            model.hours_behind =
                watermark.map_or(0, |mark| ((newest_complete - mark) / 3600).max(0));
            println!(
                "hour rows (48h): {} · model built in {} ms",
                hour_rows.len(),
                elapsed.as_millis()
            );
            for line in ptt_runtime::reports::render_exchange_radar(&model, settings.ui_language) {
                println!("{line}");
            }
            if let ptt_runtime::reports::RadarScan::Ran(scan) = &model.scan {
                println!("diagnostics: {:?}", scan.diagnostics);
            }
            Ok(())
        }

        /// 与 app 的 `load_exchange_ledger` 同一条路径：水位 → 窗口小时数 → 精简行 →
        /// `exchange_ledger_model`。返回 (模型, 读库毫秒, 建账毫秒)。
        fn ledger(
            &self,
            store: &ptt_storage::MarketStore,
            tuning: &ptt_settings::MarketTuning,
        ) -> Result<Option<(ptt_runtime::reports::ExchangeLedgerModel, u64, u64)>, String> {
            let Some(watermark) = store
                .exchange_watermark(&self.realm, &self.league)
                .map_err(|error| format!("watermark: {error}"))?
            else {
                return Ok(None);
            };
            let window_hours = ptt_runtime::reports::exchange_ledger_window_hours(tuning);
            let from_ts = watermark - i64::from(window_hours) * 3600;
            let started = std::time::Instant::now();
            let rows = store
                .load_exchange_hour_volumes(&self.realm, &self.league, from_ts, watermark + 3600)
                .map_err(|error| format!("hour volumes: {error}"))?;
            let load_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let started = std::time::Instant::now();
            let mut model = ptt_runtime::reports::exchange_ledger_model(
                &rows,
                &self.league,
                self.game,
                tuning,
            )?;
            let build_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            model.synced_through = Some(watermark);
            model.load_millis = load_millis;
            Ok(Some((model, load_millis, build_millis)))
        }

        /// 镜像交易所页的明细栏（小时账本）与表格的时段档位。
        fn series(&self, asset: Option<String>, range: Option<String>) -> Result<(), String> {
            let asset = asset.ok_or("--asset <catalog id> is required")?;
            let asset = ptt_runtime::live::domain_asset_id(&asset)
                .map_err(|error| format!("--asset: {error:?}"))?;
            let hours = match range.as_deref().unwrap_or("24h") {
                "24h" => Some(24),
                "3d" => Some(72),
                "7d" => Some(168),
                "all" => None,
                other => return Err(format!("--range {other}: expected 24h, 3d, 7d or all")),
            };
            let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
            let settings = ptt_settings::SettingsStore::release_default_from(&local)
                .load()
                .settings;
            let tuning = settings.market_tuning(settings.active_profile.game);
            let store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let Some((ledger, load_millis, build_millis)) = self.ledger(&store, &tuning)? else {
                println!("ledger: no watermark -- this (game, league) has never synced");
                return Ok(());
            };
            println!(
                "ledger: {} hours / {} rows / {} assets · loaded in {load_millis} ms, built in {build_millis} ms",
                ledger.hours_loaded,
                ledger.rows_loaded,
                ledger.ledger.series.len(),
            );
            let offset = chrono::Local::now().offset().local_minus_utc();
            for line in ptt_runtime::reports::render_exchange_series(
                &ledger,
                &asset,
                hours,
                offset,
                settings.ui_language,
            ) {
                println!("{line}");
            }

            // 表格按同一档位重排：和页面一样先算 48 小时的默认口径，再套窗口。
            let now = chrono::Utc::now();
            let hour_rows = store
                .load_exchange_hours(
                    &self.realm,
                    &self.league,
                    now.timestamp() - 48 * 3600,
                    now.timestamp(),
                )
                .map_err(|error| format!("hours: {error}"))?;
            let mut model = ptt_runtime::reports::exchange_model(
                &[],
                &hour_rows,
                &self.league,
                self.game,
                &tuning,
            )?;
            ptt_runtime::reports::apply_exchange_window(&mut model, &ledger.ledger, hours);
            println!(
                "table by window {}: top 15 of {} rows",
                range.as_deref().unwrap_or("24h"),
                model.rows.len()
            );
            for row in model.rows.iter().take(15) {
                println!(
                    "  {:<40} vol/h {}",
                    row.asset_id, row.volume_per_hour_anchor
                );
            }
            Ok(())
        }

        /// 与 app 的导出按钮同一条路径（同一个模型函数、同样的文件名规则），
        /// 只多打印样本：数量级对不对，一眼看得出来。
        fn export(&self, out: Option<std::path::PathBuf>) -> Result<(), String> {
            let directory = out.unwrap_or_else(|| {
                ptt_runtime::pipeline::default_database_path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("exports")
            });
            let outcome = ptt_runtime::exchange_export::write_exchange_export(
                self.game,
                &self.league,
                &self.anchor,
                &directory,
            )?;
            let (base, rows, season_start) = (outcome.base, outcome.rows, outcome.season_start);

            let days_seen: BTreeSet<&str> = rows.iter().map(|row| row.day.as_str()).collect();
            let unmapped = rows.iter().filter(|row| row.category == "unmapped").count();
            println!(
                "export: {} rows, {} days, {} unmapped rows, season start {:?} -> {}.csv/.json",
                rows.len(),
                days_seen.len(),
                unmapped,
                season_start,
                base.display(),
            );
            // 样本：锚三件套 + 成交额最大的几行，最后一天。
            let Some(last_day) = days_seen.iter().next_back().copied() else {
                return Ok(());
            };
            let mut sample: Vec<_> = rows.iter().filter(|row| row.day == last_day).collect();
            sample.sort_by(|left, right| {
                right
                    .volume_anchor
                    .unwrap_or(0)
                    .cmp(&left.volume_anchor.unwrap_or(0))
            });
            for row in sample.iter().take(12) {
                println!(
                    "  {} d{} {:<10} {:<28} ex={:<10} div={:<10} chaos={:<10} units={} vol({})={}",
                    row.day,
                    row.day_index
                        .map_or("-".to_owned(), |index| index.to_string()),
                    row.phase.unwrap_or("-"),
                    row.asset_id,
                    row.value_exalted.as_deref().unwrap_or("-"),
                    row.value_divine.as_deref().unwrap_or("-"),
                    row.value_chaos.as_deref().unwrap_or("-"),
                    row.units_traded,
                    row.anchor,
                    row.volume_anchor
                        .map_or("-".to_owned(), |volume| volume.to_string()),
                );
            }
            Ok(())
        }
    }

    struct Session {
        /// 存储键与缓存文件名（"poe1"/"poe2"）。
        realm: String,
        /// 映射表、目录、锚都按它取——与 app 的 `request.profile.game` 同源。
        game: ptt_core::Game,
        /// 成交额按它折算，与交易所页同一条选法（`exchange_anchor`）。
        anchor: ptt_trade_domain::MarketAssetId,
        /// 锚在 CDN 行里的样子（GGG 路径）——`--paths`/`--trend` 直接对原始行比，不过域层。
        anchor_path: String,
        /// 结算资产里锚之外的第一个（POE2 默认神圣、POE1 默认混沌），只和它成对的资产
        /// 经它折算一步。设置里没有第二个结算资产就不桥接。
        bridge_path: Option<String>,
        league: String,
        cache_dir: std::path::PathBuf,
        fetcher: ExchangeFetcher,
    }

    impl Session {
        fn cache_path(&self, hour_ts: u64) -> std::path::PathBuf {
            self.cache_dir
                .join(format!("{}-{hour_ts}.json", self.realm))
        }

        /// 缓存优先取小时；缺了就现抓并落盘。`throttled` 控制对 CDN 的礼貌间隔。
        fn load_hour(&self, hour_ts: u64, throttled: &mut bool) -> Result<Vec<u8>, String> {
            let path = self.cache_path(hour_ts);
            if path.exists() {
                return std::fs::read(&path)
                    .map_err(|error| format!("read cache {hour_ts}: {error}"));
            }
            if *throttled {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            *throttled = true;
            let bytes = self
                .fetcher
                .fetch_hour(&self.realm, hour_ts)
                .map_err(|error| format!("fetch {hour_ts}: {error}"))?;
            std::fs::write(&path, &bytes)
                .map_err(|error| format!("write cache {hour_ts}: {error}"))?;
            Ok(bytes)
        }

        /// 缓存里当前 realm 的所有小时，按时间升序解析出联赛行。
        fn cached_hours(&self) -> Result<Vec<(u64, HourSnapshot)>, String> {
            let mut result = Vec::new();
            let prefix = format!("{}-", self.realm);
            let entries = std::fs::read_dir(&self.cache_dir)
                .map_err(|error| format!("cache dir: {error}"))?;
            for entry in entries {
                let entry = entry.map_err(|error| format!("cache dir: {error}"))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(ts_text) = name
                    .strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(".json"))
                else {
                    continue;
                };
                let Ok(hour_ts) = ts_text.parse::<u64>() else {
                    continue;
                };
                let bytes =
                    std::fs::read(entry.path()).map_err(|error| format!("read {name}: {error}"))?;
                let hour = parse_hour(&bytes).map_err(|error| format!("parse {name}: {error}"))?;
                result.push((hour_ts, hour));
            }
            result.sort_by_key(|(hour_ts, _)| *hour_ts);
            Ok(result)
        }

        fn fetch(&self, hours: u64) -> Result<(), String> {
            let now = chrono::Utc::now().timestamp().max(0) as u64;
            let newest = now / 3600 * 3600 - 3600;
            let mut throttled = false;
            let mut empty_hours: Vec<u64> = Vec::new();
            let mut chain_breaks: Vec<u64> = Vec::new();
            for step in 0..hours {
                let hour_ts = newest - step * 3600;
                let bytes = self.load_hour(hour_ts, &mut throttled)?;
                let hour =
                    parse_hour(&bytes).map_err(|error| format!("parse {hour_ts}: {error}"))?;
                if hour.next_change_id != hour_ts + 3600 {
                    chain_breaks.push(hour_ts);
                }
                if hour.markets.is_empty() {
                    empty_hours.push(hour_ts);
                }
                println!(
                    "{hour_ts} ({}) markets={} league-rows={}",
                    format_hour(hour_ts),
                    hour.markets.len(),
                    hour.rows_for_league(&self.league).count(),
                );
            }
            println!(
                "fetch: hours={hours} empty={} chain-breaks={}",
                empty_hours.len(),
                chain_breaks.len(),
            );
            for hour_ts in &chain_breaks {
                println!("  CHAIN BREAK at {hour_ts} ({})", format_hour(*hour_ts));
            }
            Ok(())
        }

        /// 缓存 → 生产表 → 日线，与 `exchange_sync` 同一条写路径（`storage_row` +
        /// `replace_exchange_hour` + `ensure_exchange_day_rollups`）。CDN 不可变，
        /// 所以重复 ingest 只是幂等覆写；不做清理——沙盒里想留多久留多久。
        fn ingest(&self) -> Result<(), String> {
            let mut store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let hours = self.cached_hours()?;
            let now = chrono::Utc::now();
            let mut league_rows = 0usize;
            for (hour_ts, hour) in &hours {
                let hour_ts = *hour_ts as i64;
                let rows: Vec<ptt_storage::ExchangeHourMarketRow> = hour
                    .rows_for_league(&self.league)
                    .map(|row| storage_row(hour_ts, row))
                    .collect();
                league_rows += rows.len();
                store
                    .replace_exchange_hour(&self.realm, &self.league, hour_ts, &rows, now)
                    .map_err(|error| format!("store {hour_ts}: {error}"))?;
            }
            let fold = ptt_runtime::exchange_rollup::ensure_exchange_day_rollups(
                &mut store,
                &self.realm,
                &self.league,
                now,
                32,
            )?;
            println!(
                "ingest: hours={} league-rows={league_rows} days-folded={} skipped={} already-done={}",
                hours.len(),
                fold.days_processed.len(),
                fold.days_skipped.len(),
                fold.days_already_done,
            );
            for (day, reason) in &fold.days_skipped {
                println!("  skipped {day}: {reason}");
            }
            Ok(())
        }

        fn paths(&self, top: usize) -> Result<(), String> {
            struct PathStat {
                anchor_volume: u64,
                own_volume: u64,
                appearances: u64,
            }
            let mut stats: BTreeMap<String, PathStat> = BTreeMap::new();
            let hours = self.cached_hours()?;
            for (_, hour) in &hours {
                for row in hour.rows_for_league(&self.league) {
                    let mut credit = |path: &str, own: u64, anchor: u64| {
                        let stat = stats.entry(path.to_owned()).or_insert(PathStat {
                            anchor_volume: 0,
                            own_volume: 0,
                            appearances: 0,
                        });
                        stat.anchor_volume += anchor;
                        stat.own_volume += own;
                        stat.appearances += 1;
                    };
                    if row.asset_a == self.anchor_path {
                        credit(&row.asset_b, row.volume_b, row.volume_a);
                    } else if row.asset_b == self.anchor_path {
                        credit(&row.asset_a, row.volume_a, row.volume_b);
                    } else {
                        credit(&row.asset_a, row.volume_a, 0);
                        credit(&row.asset_b, row.volume_b, 0);
                    }
                }
            }
            let mut ranked: Vec<(&String, &PathStat)> = stats.iter().collect();
            ranked.sort_by(|left, right| right.1.anchor_volume.cmp(&left.1.anchor_volume));
            let total: u64 = ranked.iter().map(|(_, stat)| stat.anchor_volume).sum();
            let head: u64 = ranked
                .iter()
                .take(top)
                .map(|(_, stat)| stat.anchor_volume)
                .sum();
            println!(
                "paths: hours={} distinct={} top-{top} covers {}% of anchor volume",
                hours.len(),
                ranked.len(),
                if total == 0 { 0 } else { head * 100 / total },
            );
            for (rank, (path, stat)) in ranked.iter().take(top).enumerate() {
                println!(
                    "{:>3}. {:<60} anchor-vol={:<12} own-vol={:<12} markets={}",
                    rank + 1,
                    path.strip_prefix("Metadata/Items/").unwrap_or(path),
                    stat.anchor_volume,
                    stat.own_volume,
                    stat.appearances,
                );
            }
            Ok(())
        }

        fn trend(&self, top: usize) -> Result<(), String> {
            let mapping = ptt_exchange_history::mapping::index(self.game)
                .map_err(|error| format!("mapping: {error}"))?;
            let catalog = match self.game {
                ptt_core::Game::Poe1 => ptt_catalog::poe1(),
                ptt_core::Game::Poe2 => ptt_catalog::poe2(),
            };
            let hours = self.cached_hours()?;
            // 没配第二个结算资产就没有桥：空串永远对不上任何路径。
            let bridge_path = self.bridge_path.as_deref().unwrap_or("");
            let anchor_id = mapping.get(&self.anchor_path).cloned().unwrap_or_default();

            // 每资产每天累计：锚计价成交额 + 自身单位数 + 库存深度样本。
            struct DayFold {
                exalted_value: f64,
                own_units: f64,
            }
            struct AssetFold {
                days: BTreeMap<String, DayFold>,
                depth_exalted_sum: f64,
                depth_samples: u64,
                hours_with_volume: u64,
            }
            let mut folds: BTreeMap<&str, AssetFold> = BTreeMap::new();
            let mut hours_used = 0u64;

            for (hour_ts, hour) in &hours {
                let rows: Vec<&MarketRow> = hour.rows_for_league(&self.league).collect();
                if rows.is_empty() {
                    continue;
                }
                hours_used += 1;
                let day = format_day(*hour_ts);
                // 该小时的桥→锚换算率，给"只和桥成对"的资产折算用。
                let exalted_per_divine = rows.iter().find_map(|row| {
                    (row.asset_a == self.anchor_path && row.asset_b == bridge_path)
                        .then(|| row.volume_a as f64 / row.volume_b.max(1) as f64)
                });
                for row in &rows {
                    // (资产, 锚) 或 (资产, 桥) 两类市场参与估值；其余跳过。
                    let (asset_path, own_volume, own_stock, quote_volume, to_exalted) =
                        if row.asset_a == self.anchor_path {
                            (
                                &row.asset_b,
                                row.volume_b,
                                avg(row.lowest_stock_b, row.highest_stock_b),
                                row.volume_a,
                                1.0,
                            )
                        } else if row.asset_b == self.anchor_path {
                            (
                                &row.asset_a,
                                row.volume_a,
                                avg(row.lowest_stock_a, row.highest_stock_a),
                                row.volume_b,
                                1.0,
                            )
                        } else if row.asset_a == bridge_path {
                            let Some(rate) = exalted_per_divine else {
                                continue;
                            };
                            (
                                &row.asset_b,
                                row.volume_b,
                                avg(row.lowest_stock_b, row.highest_stock_b),
                                row.volume_a,
                                rate,
                            )
                        } else if row.asset_b == bridge_path {
                            let Some(rate) = exalted_per_divine else {
                                continue;
                            };
                            (
                                &row.asset_a,
                                row.volume_a,
                                avg(row.lowest_stock_a, row.highest_stock_a),
                                row.volume_b,
                                rate,
                            )
                        } else {
                            continue;
                        };
                    let Some(asset_id) = mapping.get(asset_path.as_str()) else {
                        continue;
                    };
                    if own_volume == 0 || quote_volume == 0 {
                        continue;
                    }
                    let exalted_value = quote_volume as f64 * to_exalted;
                    let unit_value = exalted_value / own_volume as f64;
                    let fold = folds.entry(asset_id).or_insert_with(|| AssetFold {
                        days: BTreeMap::new(),
                        depth_exalted_sum: 0.0,
                        depth_samples: 0,
                        hours_with_volume: 0,
                    });
                    let day_fold = fold.days.entry(day.clone()).or_insert(DayFold {
                        exalted_value: 0.0,
                        own_units: 0.0,
                    });
                    day_fold.exalted_value += exalted_value;
                    day_fold.own_units += own_volume as f64;
                    fold.depth_exalted_sum += own_stock * unit_value;
                    fold.depth_samples += 1;
                    fold.hours_with_volume += 1;
                }
            }

            // 日 VWAP 序列 → 趋势。基线 = 除最后 2 天外的日值中位，近期 = 最后 2 天中位。
            struct TrendRow {
                asset_id: String,
                latest_value: f64,
                raw_bps: f64,
                volume_per_hour: f64,
                depth_exalted: f64,
                days: usize,
            }
            let mut rows: Vec<TrendRow> = Vec::new();
            for (asset_id, fold) in &folds {
                if **asset_id == anchor_id {
                    continue;
                }
                let values: Vec<f64> = fold
                    .days
                    .values()
                    .map(|day| day.exalted_value / day.own_units)
                    .collect();
                if values.len() < 5 {
                    continue;
                }
                let (baseline_days, recent_days) = values.split_at(values.len() - 2);
                let baseline = lower_middle(baseline_days);
                let recent = lower_middle(recent_days);
                if baseline <= 0.0 {
                    continue;
                }
                let total_exalted: f64 = fold.days.values().map(|day| day.exalted_value).sum();
                rows.push(TrendRow {
                    asset_id: (*asset_id).to_owned(),
                    latest_value: *values.last().unwrap_or(&0.0),
                    raw_bps: (recent / baseline - 1.0) * 10000.0,
                    volume_per_hour: total_exalted / hours_used.max(1) as f64,
                    depth_exalted: fold.depth_exalted_sum / fold.depth_samples.max(1) as f64,
                    days: values.len(),
                });
            }
            let market_median_bps =
                lower_middle(&rows.iter().map(|row| row.raw_bps).collect::<Vec<_>>());

            rows.sort_by(|left, right| right.volume_per_hour.total_cmp(&left.volume_per_hour));
            let mut depth_rank: Vec<&str> = rows.iter().map(|row| row.asset_id.as_str()).collect();
            {
                let depth_of: BTreeMap<&str, f64> = rows
                    .iter()
                    .map(|row| (row.asset_id.as_str(), row.depth_exalted))
                    .collect();
                depth_rank.sort_by(|left, right| depth_of[right].total_cmp(&depth_of[left]));
            }
            let depth_rank_of = |asset_id: &str| {
                depth_rank
                    .iter()
                    .position(|candidate| *candidate == asset_id)
                    .map_or(0, |index| index + 1)
            };

            println!(
                "trend: anchor={} hours={hours_used} assets-with-5-days={} market-median-drift={market_median_bps:+.0}bps",
                self.anchor,
                rows.len(),
            );
            println!(
                "{:<4} {:<18} {:>12} {:>9} {:>9}  {:<4} {:>14} {:>14} {:>5}",
                "#",
                "asset",
                format!("value({})", self.anchor),
                "raw bps",
                "rel bps",
                "",
                format!("vol/h({})", self.anchor),
                format!("depth({})", self.anchor),
                "d-rk",
            );
            for (rank, row) in rows.iter().take(top).enumerate() {
                let relative = row.raw_bps - market_median_bps;
                let verdict = if relative > 300.0 {
                    "↑"
                } else if relative < -300.0 {
                    "↓"
                } else {
                    "→"
                };
                let name = catalog
                    .by_id(&row.asset_id)
                    .map_or_else(|| row.asset_id.clone(), |asset| asset.name_en.clone());
                println!(
                    "{:<4} {:<18} {:>12.3} {:>+9.0} {:>+9.0}  {:<4} {:>14.0} {:>14.0} {:>5}",
                    rank + 1,
                    name,
                    row.latest_value,
                    row.raw_bps,
                    relative,
                    verdict,
                    row.volume_per_hour,
                    row.depth_exalted,
                    depth_rank_of(&row.asset_id),
                );
                let _ = row.days;
            }
            Ok(())
        }

        /// 镜像交易所页的「面板核对」条：同一个模型函数（`exchange_reconcile`）、
        /// 同一个窗口（小时保留天数，钳到赛季起点）、同一种逐点查库。
        ///
        /// 以前这里是 spike 时代的版本：读 CDN 缓存、双向都算命中、不限窗口、
        /// 扫全部上下文——和页面的数字对不上账，正是 CLAUDE.md 警告的那种
        /// 漂移（审查抓出来的）。现在页面和探针只差一个 println。
        fn reconcile(&self) -> Result<(), String> {
            let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
            let settings = ptt_settings::SettingsStore::release_default_from(&local)
                .load()
                .settings;
            let tuning = settings.market_tuning(settings.active_profile.game);
            let store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let now = chrono::Utc::now();
            let retention = tuning.exchange.hour_retention_days;
            let window_days = u32::try_from(if retention == 0 {
                365
            } else {
                retention.min(365)
            })
            .unwrap_or(14);
            let context = ptt_runtime::live::live_context(
                settings.active_profile,
                ptt_runtime::pipeline::LIVE_LEAGUE,
            )
            .map_err(|error| format!("{error:?}"))?;
            let season = store.active_season(&self.realm).ok().flatten();
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
            let keys = ptt_runtime::reports::exchange_reconcile_keys(&observations, self.game)?;
            let mut matched_rows = Vec::new();
            for (hour_ts, asset_a, asset_b) in &keys {
                if let Some(row) = store
                    .load_exchange_hour_market(
                        &self.realm,
                        &self.league,
                        *hour_ts,
                        asset_a,
                        asset_b,
                    )
                    .map_err(|error| format!("hour market: {error}"))?
                {
                    matched_rows.push(row);
                }
            }
            println!(
                "window: {} days since {} · observations={} · (hour, pair) keys={} · official rows matched={}",
                window_days,
                since.format("%m-%d %H:%M"),
                observations.len(),
                keys.len(),
                matched_rows.len(),
            );
            let reconcile = ptt_runtime::reports::exchange_reconcile(
                &observations,
                self.game,
                &matched_rows,
                window_days,
            )?;
            for line in
                ptt_runtime::reports::render_exchange_reconcile(&reconcile, settings.ui_language)
            {
                println!("{line}");
            }
            Ok(())
        }
    }

    impl Session {
        /// 生产表的健康面：水位、近 48 小时覆盖、日折/清理进度、映射覆盖。
        /// app 的同步读的就是这些数字，这里对不上 = 探针或生产漂移了。
        fn status(&self) -> Result<(), String> {
            let store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let now_ts = chrono::Utc::now().timestamp();
            let watermark = store
                .exchange_watermark(&self.realm, &self.league)
                .map_err(|error| format!("watermark: {error}"))?;
            match watermark {
                Some(mark) => {
                    // 页面说的是"落后几个完整小时",和"几分钟前"是两个数,都印。
                    let newest_complete = now_ts.div_euclid(3600) * 3600 - 3600;
                    println!(
                        "watermark: {mark} ({}) age {} min -- page reads this as {} h behind",
                        format_hour(mark.max(0) as u64),
                        (now_ts - mark) / 60,
                        ((newest_complete - mark) / 3600).max(0),
                    );
                }
                None => println!("watermark: none -- this (game, league) has never synced"),
            }
            let marks = store
                .list_exchange_hour_marks(&self.realm, &self.league)
                .map_err(|error| format!("hour marks: {error}"))?;
            let recent_floor = now_ts - 48 * 3600;
            let recent: Vec<_> = marks
                .iter()
                .filter(|mark| mark.hour_ts >= recent_floor)
                .collect();
            let empties = recent.iter().filter(|mark| mark.market_count == 0).count();
            println!(
                "hours: total={} last-48h={} (empty {empties})",
                marks.len(),
                recent.len(),
            );
            let day_marks = store
                .list_exchange_day_marks(&self.realm, &self.league)
                .map_err(|error| format!("day marks: {error}"))?;
            println!(
                "days folded: {} ({} .. {})",
                day_marks.len(),
                day_marks.first().map_or("-", |mark| &mark.utc_day),
                day_marks.last().map_or("-", |mark| &mark.utc_day),
            );

            // 映射覆盖:与交易所页表头同一口径——60 天窗口的日折行里两侧都映射
            // 得上的占比。以前按近 24 小时的小时行算,和页面对不上却不是漂移。
            let mapping = ptt_exchange_history::mapping::index(self.game)
                .map_err(|error| format!("mapping: {error}"))?;
            let today = chrono::Utc::now().date_naive();
            let from_day = (today - chrono::Duration::days(60)).to_string();
            let rows = store
                .load_exchange_days(&self.realm, &self.league, &from_day, &today.to_string())
                .map_err(|error| format!("days: {error}"))?;
            let mapped = rows
                .iter()
                .filter(|row| {
                    mapping.contains_key(&row.asset_a) && mapping.contains_key(&row.asset_b)
                })
                .count();
            if rows.is_empty() {
                println!("mapping: no day rows in the last 60 days to measure");
            } else {
                println!(
                    "mapping: {mapped}/{} day rows fully mapped ({}%) -- same base as the page header",
                    rows.len(),
                    mapped * 100 / rows.len(),
                );
            }
            // 小时账本的账单：这个数决定按水位缓存够不够（>3 s 再考虑按天分块）。
            let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
            let settings = ptt_settings::SettingsStore::release_default_from(&local)
                .load()
                .settings;
            let tuning = settings.market_tuning(settings.active_profile.game);
            match self.ledger(&store, &tuning)? {
                Some((ledger, load_millis, build_millis)) => println!(
                    "ledger: {} hours, {} rows, {} assets, load {load_millis} ms + build {build_millis} ms",
                    ledger.hours_loaded,
                    ledger.rows_loaded,
                    ledger.ledger.series.len(),
                ),
                None => println!("ledger: no watermark"),
            }
            Ok(())
        }

        /// 复查确认为空的小时。CDN 不可变、可重查：当时空、现在有数据,
        /// 说明护栏被穿了(发布延迟超过三小时)。修复就是重抓覆写——
        /// `replace_exchange_hour` 连 mark 带行整体换,不需要删除原语。
        fn audit(&self) -> Result<(), String> {
            let mut store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let empties: Vec<i64> = store
                .list_exchange_hour_marks(&self.realm, &self.league)
                .map_err(|error| format!("hour marks: {error}"))?
                .into_iter()
                .filter(|mark| mark.market_count == 0)
                .map(|mark| mark.hour_ts)
                .collect();
            println!("audit: {} confirmed-empty hours to re-check", empties.len());
            let now = chrono::Utc::now();
            let mut throttled = false;
            let mut repaired = 0usize;
            for hour_ts in empties {
                // 走缓存优先的同一条取数路,审计本身不该打爆 CDN。
                let bytes = self.load_hour(hour_ts.max(0) as u64, &mut throttled)?;
                let hour =
                    parse_hour(&bytes).map_err(|error| format!("parse {hour_ts}: {error}"))?;
                let rows: Vec<ptt_storage::ExchangeHourMarketRow> = hour
                    .rows_for_league(&self.league)
                    .map(|row| storage_row(hour_ts, row))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                store
                    .replace_exchange_hour(&self.realm, &self.league, hour_ts, &rows, now)
                    .map_err(|error| format!("repair {hour_ts}: {error}"))?;
                println!(
                    "  REPAIRED {hour_ts} ({}): {} rows were published after all",
                    format_hour(hour_ts.max(0) as u64),
                    rows.len(),
                );
                repaired += 1;
            }
            println!("audit: repaired {repaired}");
            Ok(())
        }
    }

    fn storage_row(hour_ts: i64, row: &MarketRow) -> ptt_storage::ExchangeHourMarketRow {
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

    fn avg(low: u64, high: u64) -> f64 {
        (low as f64 + high as f64) / 2.0
    }

    /// 展示层的 lower-middle 中位：取真实观测值，不平均。空集给 0。
    fn lower_middle(values: &[f64]) -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[(sorted.len() - 1) / 2]
    }

    pub fn format_hour(hour_ts: u64) -> String {
        chrono::DateTime::from_timestamp(hour_ts as i64, 0)
            .map_or_else(|| "?".to_owned(), |ts| ts.format("%m-%d %H:%M").to_string())
    }

    fn format_day(hour_ts: u64) -> String {
        chrono::DateTime::from_timestamp(hour_ts as i64, 0)
            .map_or_else(|| "?".to_owned(), |ts| ts.format("%Y-%m-%d").to_string())
    }
}

#[cfg(windows)]
fn main() -> Result<(), String> {
    probe::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only: mirrors the production fetch path and reads app settings");
}
