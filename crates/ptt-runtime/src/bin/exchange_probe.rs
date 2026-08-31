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
//!   `exchange_probe --paths --league "<联赛名>" [--cache DIR] [--top N]`
//!       聚合缓存，按锚（崇高）计价成交量降序输出资产路径 = 映射工作清单。
//!   `exchange_probe --trend --league "<联赛名>" [--cache DIR] [--top N]`
//!       小时 VWAP → 日折 → 近 2 天 vs 基线的趋势 bps（原始 + 扣市场中位），
//!       同时给出成交量口径与挂单库存口径的两份流动性读数，供证据分工裁定。
//!   `exchange_probe --reconcile --league "<联赛名>" [--cache DIR]`
//!       把 OCR 库里的 taker 汇率逐条对进同小时同对的 API 区间，输出命中率。
//!   `exchange_probe --status`
//!       读生产表：水位、近 48 小时覆盖、日折/清理进度、映射覆盖率。
//!       镜像 app 同步的读路径，app 里看到的数字和这里对不上就是漂移。
//!   `exchange_probe --audit`
//!       复查 market_count=0 的小时 mark：CDN 不可变、可重查，假空直接
//!       重抓覆写修复（replace 连 mark 带行一起换，不需要删除原语）。
//!
//! `--status`/`--audit` 的联赛默认取设置里的 `exchange.league`，
//! `--league` 可覆盖；其余子命令仍要求显式 `--league`。

#[cfg(windows)]
mod probe {
    use std::collections::{BTreeMap, BTreeSet};

    use ptt_exchange_history::fetch::ExchangeFetcher;
    use ptt_exchange_history::mapping::{DIVINE_PATH, EXALTED_PATH, poe2_index};
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
            || has("--paths")
            || has("--trend")
            || has("--reconcile")
            || has("--status")
            || has("--audit");
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

        let session = Session {
            realm,
            league,
            cache_dir,
            fetcher: ExchangeFetcher::new(),
        };

        if has("--fetch") {
            session.fetch(hours)?;
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
        Ok(())
    }

    struct Session {
        realm: String,
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
                    if row.asset_a == EXALTED_PATH {
                        credit(&row.asset_b, row.volume_b, row.volume_a);
                    } else if row.asset_b == EXALTED_PATH {
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
            let mapping = poe2_index().map_err(|error| format!("mapping: {error}"))?;
            let catalog = ptt_catalog::poe2();
            let hours = self.cached_hours()?;

            // 每资产每天累计：崇高计价成交额 + 自身单位数 + 库存深度样本。
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
                // 该小时的神圣→崇高换算率，给"只和神圣成对"的资产折算用。
                let exalted_per_divine = rows.iter().find_map(|row| {
                    (row.asset_a == EXALTED_PATH && row.asset_b == DIVINE_PATH)
                        .then(|| row.volume_a as f64 / row.volume_b.max(1) as f64)
                });
                for row in &rows {
                    // (资产, 崇高) 或 (资产, 神圣) 两类市场参与估值；其余跳过。
                    let (asset_path, own_volume, own_stock, quote_volume, to_exalted) =
                        if row.asset_a == EXALTED_PATH {
                            (
                                &row.asset_b,
                                row.volume_b,
                                avg(row.lowest_stock_b, row.highest_stock_b),
                                row.volume_a,
                                1.0,
                            )
                        } else if row.asset_b == EXALTED_PATH {
                            (
                                &row.asset_a,
                                row.volume_a,
                                avg(row.lowest_stock_a, row.highest_stock_a),
                                row.volume_b,
                                1.0,
                            )
                        } else if row.asset_a == DIVINE_PATH {
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
                        } else if row.asset_b == DIVINE_PATH {
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
                if *asset_id == "exalted_orb" {
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
                "trend: hours={hours_used} assets-with-5-days={} market-median-drift={market_median_bps:+.0}bps",
                rows.len(),
            );
            println!(
                "{:<4} {:<18} {:>12} {:>9} {:>9}  {:<4} {:>14} {:>14} {:>5}",
                "#",
                "asset",
                "value(ex)",
                "raw bps",
                "rel bps",
                "",
                "vol/h(ex)",
                "depth(ex)",
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
                    .map_or_else(|| row.asset_id.clone(), |asset| asset.name_zh_tw.clone());
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

        fn reconcile(&self) -> Result<(), String> {
            use ptt_trade_domain::QuoteEdgeRole;

            let mapping = poe2_index().map_err(|error| format!("mapping: {error}"))?;
            // 域层 id 是连字符风格；catalog/映射是下划线。反查这一步做一次逆转换。
            let path_of_domain: BTreeMap<String, String> = mapping
                .iter()
                .map(|(path, asset_id)| (asset_id.replace('_', "-"), path.clone()))
                .collect();

            let store =
                ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path())
                    .map_err(|error| format!("storage: {error}"))?;
            let contexts = store
                .list_contexts()
                .map_err(|error| format!("contexts: {error}"))?;

            let mut samples = 0u64;
            let mut hits_direct = 0u64;
            let mut hits_inverse = 0u64;
            // 只看每次抓取的最优档：API 区间是"该小时实际成交过的价格"，
            // 阶梯深处没成交的档位落在区间外是结构性的，不算数据错。
            let mut top_samples = 0u64;
            let mut top_hits = 0u64;
            let mut near_misses = 0u64;
            let mut misses: Vec<String> = Vec::new();
            let mut no_market = 0u64;
            let mut unmapped = 0u64;
            let mut hour_misses: BTreeSet<u64> = BTreeSet::new();
            let mut fetch_budget = 96u32;
            let mut throttled = false;

            for context in &contexts {
                let value: serde_json::Value = serde_json::from_str(&context.context_json)
                    .map_err(|error| format!("context json: {error}"))?;
                if value.get("game").and_then(|game| game.as_str()) != Some(self.realm.as_str()) {
                    continue;
                }
                let observations = store
                    .load_observations(&context.context_key, None)
                    .map_err(|error| format!("observations: {error}"))?;
                for observation in &observations {
                    let edge = &observation.edge;
                    if edge.role != QuoteEdgeRole::AvailableTaker {
                        continue;
                    }
                    let (Some(from_path), Some(to_path)) = (
                        path_of_domain.get(&edge.from_asset_id.to_string()),
                        path_of_domain.get(&edge.to_asset_id.to_string()),
                    ) else {
                        unmapped += 1;
                        continue;
                    };
                    let hour_ts = (edge.captured_at.timestamp().max(0) as u64) / 3600 * 3600;
                    let path = self.cache_path(hour_ts);
                    if !path.exists() {
                        if fetch_budget == 0 {
                            hour_misses.insert(hour_ts);
                            continue;
                        }
                        fetch_budget -= 1;
                        if self.load_hour(hour_ts, &mut throttled).is_err() {
                            hour_misses.insert(hour_ts);
                            continue;
                        }
                    }
                    let bytes = std::fs::read(&path).map_err(|error| format!("read: {error}"))?;
                    let hour =
                        parse_hour(&bytes).map_err(|error| format!("parse {hour_ts}: {error}"))?;
                    let (asset_a, asset_b) = if from_path < to_path {
                        (from_path, to_path)
                    } else {
                        (to_path, from_path)
                    };
                    let Some(row) = hour
                        .rows_for_league(&self.league)
                        .find(|row| row.asset_a == **asset_a && row.asset_b == **asset_b)
                    else {
                        no_market += 1;
                        continue;
                    };
                    // a_per_b 的小时区间，两个快照比值不保证大小序，自己排。
                    let bound_low = ratio_f64(&row.lowest_ratio_a) / ratio_f64(&row.lowest_ratio_b);
                    let bound_high =
                        ratio_f64(&row.highest_ratio_a) / ratio_f64(&row.highest_ratio_b);
                    let (low, high) = if bound_low <= bound_high {
                        (bound_low, bound_high)
                    } else {
                        (bound_high, bound_low)
                    };
                    let scalar = edge.rate.numerator as f64 / edge.rate.denominator.max(1) as f64;
                    samples += 1;
                    let top_row = edge.original_row_index == 0;
                    if top_row {
                        top_samples += 1;
                    }
                    let hit_direct = scalar >= low && scalar <= high;
                    let hit_inverse =
                        scalar > 0.0 && (1.0 / scalar) >= low && (1.0 / scalar) <= high;
                    if hit_direct || hit_inverse {
                        if top_row {
                            top_hits += 1;
                        }
                    } else {
                        let stretched_low = low * 0.95;
                        let stretched_high = high * 1.05;
                        let near = |value: f64| value >= stretched_low && value <= stretched_high;
                        if near(scalar) || (scalar > 0.0 && near(1.0 / scalar)) {
                            near_misses += 1;
                        }
                    }
                    if hit_direct {
                        hits_direct += 1;
                    } else if hit_inverse {
                        hits_inverse += 1;
                    } else if misses.len() < 20 {
                        misses.push(format!(
                            "  miss {} {}->{} rate={} interval=[{low:.4},{high:.4}] at {}",
                            format_hour(hour_ts),
                            edge.from_asset_id,
                            edge.to_asset_id,
                            edge.rate.text,
                            edge.captured_at.format("%m-%d %H:%M"),
                        ));
                    }
                }
            }

            let hits = hits_direct + hits_inverse;
            println!(
                "reconcile: samples={samples} hits={hits} ({:.1}%) direct={hits_direct} inverse={hits_inverse}",
                if samples == 0 {
                    0.0
                } else {
                    hits as f64 * 100.0 / samples as f64
                },
            );
            println!(
                "  top-of-book: samples={top_samples} hits={top_hits} ({:.1}%)  near-miss(±5%)={near_misses}",
                if top_samples == 0 {
                    0.0
                } else {
                    top_hits as f64 * 100.0 / top_samples as f64
                },
            );
            println!(
                "  no-market={no_market} unmapped-edges={unmapped} hours-unavailable={}",
                hour_misses.len(),
            );
            for line in &misses {
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
                Some(mark) => println!(
                    "watermark: {mark} ({}) age {} min",
                    format_hour(mark.max(0) as u64),
                    (now_ts - mark) / 60,
                ),
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

            // 映射覆盖：近 24 小时存进来的行里，两侧路径都映射得上的占比。
            let mapping = poe2_index().map_err(|error| format!("mapping: {error}"))?;
            let rows = store
                .load_exchange_hours(&self.realm, &self.league, now_ts - 24 * 3600, now_ts)
                .map_err(|error| format!("hours: {error}"))?;
            let mapped = rows
                .iter()
                .filter(|row| {
                    mapping.contains_key(&row.asset_a) && mapping.contains_key(&row.asset_b)
                })
                .count();
            if rows.is_empty() {
                println!("mapping: no stored rows in the last 24h to measure");
            } else {
                println!(
                    "mapping: {mapped}/{} stored rows fully mapped ({}%)",
                    rows.len(),
                    mapped * 100 / rows.len(),
                );
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

    fn ratio_f64(text: &str) -> f64 {
        text.parse::<f64>().unwrap_or(0.0).max(f64::MIN_POSITIVE)
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
