//! 官方通货历史 API 的验证探针（阶段 0 spike 的验证面）。
//!
//! 在任何数据进库、进页面之前，先用它确认三件事：端点行为和我们理解的
//! 一致（空小时、next_change_id 连续性）、解析扛得住真实数据、以及映射
//! 该先映射谁（按锚计价成交量排的工作清单）。
//!
//! Usage:
//!   `exchange_probe --fetch --hours N --league "<联赛名>" [--cache DIR]`
//!       从最新完整小时往回抓 N 小时，原始字节落盘缓存（CDN 数据不可变，
//!       缓存永不过期），重复运行只补缺的。
//!   `exchange_probe --paths --league "<联赛名>" [--cache DIR] [--top N]`
//!       聚合缓存里的所有小时，按锚（崇高）计价成交量降序输出资产路径
//!       ——这就是映射表的工作清单。

#[cfg(windows)]
fn main() -> Result<(), String> {
    use std::collections::BTreeMap;

    use ptt_exchange_history::fetch::ExchangeFetcher;
    use ptt_exchange_history::{HourSnapshot, parse_hour};

    /// 锚资产的 GGG 路径。名实反直觉（AddModToRare = 崇高石），
    /// 这条常量的正确性由 S0.3 的对账交叉验证背书，别凭肉眼相信它。
    const EXALTED_PATH: &str = "Metadata/Items/Currency/CurrencyAddModToRare";

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let run_fetch = arguments.iter().any(|argument| argument == "--fetch");
    let run_paths = arguments.iter().any(|argument| argument == "--paths");
    if !run_fetch && !run_paths {
        return Err("nothing to do: pass --fetch and/or --paths (see file header)".to_owned());
    }

    let option = |name: &str| -> Option<String> {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1))
            .cloned()
    };

    let league = option("--league").ok_or("--league \"<联赛英文名>\" is required")?;
    let hours: u64 = option("--hours")
        .unwrap_or_else(|| "48".to_owned())
        .parse()
        .map_err(|error| format!("--hours: {error}"))?;
    let top: usize = option("--top")
        .unwrap_or_else(|| "60".to_owned())
        .parse()
        .map_err(|error| format!("--top: {error}"))?;

    let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
    let settings = ptt_settings::SettingsStore::release_default_from(&local)
        .load()
        .settings;
    let realm = settings.active_profile.game.as_str();

    let cache_dir = option("--cache").map_or_else(
        || local.join("PoeTradeTracker").join("exchange-cache"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&cache_dir).map_err(|error| format!("cache dir: {error}"))?;
    let cache_path = |hour_ts: u64| cache_dir.join(format!("{realm}-{hour_ts}.json"));

    // 最新完整小时再往前一格：当前小时结构性为空，最新完整小时也可能还没
    // 发布（1–2 小时延迟），探针不猜，抓回来如实报告空小时就行。
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    let newest = now / 3600 * 3600 - 3600;

    if run_fetch {
        let fetcher = ExchangeFetcher::new();
        let mut fetched = 0u64;
        let mut cached = 0u64;
        let mut empty_hours: Vec<u64> = Vec::new();
        let mut chain_breaks: Vec<u64> = Vec::new();

        for step in 0..hours {
            let hour_ts = newest - step * 3600;
            let path = cache_path(hour_ts);
            let bytes = if path.exists() {
                cached += 1;
                std::fs::read(&path).map_err(|error| format!("read cache {hour_ts}: {error}"))?
            } else {
                if fetched > 0 {
                    // 对公开 CDN 的礼貌节流。历史不可变，不赶时间。
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                let bytes = fetcher
                    .fetch_hour(realm, hour_ts)
                    .map_err(|error| format!("fetch {hour_ts}: {error}"))?;
                std::fs::write(&path, &bytes)
                    .map_err(|error| format!("write cache {hour_ts}: {error}"))?;
                fetched += 1;
                bytes
            };

            let hour = parse_hour(&bytes).map_err(|error| format!("parse {hour_ts}: {error}"))?;
            let league_rows = hour.rows_for_league(&league).count();
            let chain_ok = hour.next_change_id == hour_ts + 3600;
            if !chain_ok {
                chain_breaks.push(hour_ts);
            }
            if hour.markets.is_empty() {
                empty_hours.push(hour_ts);
            }
            println!(
                "{hour_ts} ({}) markets={} league-rows={league_rows} next-change={}",
                format_hour(hour_ts),
                hour.markets.len(),
                if chain_ok { "ok" } else { "MISMATCH" },
            );
        }

        println!(
            "fetch: hours={hours} fetched={fetched} cached={cached} empty={} chain-breaks={}",
            empty_hours.len(),
            chain_breaks.len(),
        );
        for hour_ts in &empty_hours {
            println!("  empty {hour_ts} ({})", format_hour(*hour_ts));
        }
        for hour_ts in &chain_breaks {
            println!("  CHAIN BREAK at {hour_ts} ({})", format_hour(*hour_ts));
        }
    }

    if run_paths {
        struct PathStat {
            anchor_volume: u64,
            own_volume: u64,
            appearances: u64,
        }
        let mut stats: BTreeMap<String, PathStat> = BTreeMap::new();
        let mut hours_read = 0u64;

        let entries =
            std::fs::read_dir(&cache_dir).map_err(|error| format!("cache dir: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cache dir: {error}"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&format!("{realm}-")) {
                continue;
            }
            let bytes =
                std::fs::read(entry.path()).map_err(|error| format!("read {name}: {error}"))?;
            let hour: HourSnapshot =
                parse_hour(&bytes).map_err(|error| format!("parse {name}: {error}"))?;
            hours_read += 1;
            for row in hour.rows_for_league(&league) {
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
                // 锚计价只对"直接和崇高成对"的市场有定义；映射还不存在，
                // 跨对合成是 S0.3 之后的事。头部资产几乎都直连锚，够排序用。
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
        ranked.sort_by(|left, right| {
            right
                .1
                .anchor_volume
                .cmp(&left.1.anchor_volume)
                .then(right.1.own_volume.cmp(&left.1.own_volume))
        });
        let total_anchor: u64 = ranked.iter().map(|(_, stat)| stat.anchor_volume).sum();
        let top_anchor: u64 = ranked
            .iter()
            .take(top)
            .map(|(_, stat)| stat.anchor_volume)
            .sum();

        println!(
            "paths: hours={hours_read} league=\"{league}\" distinct-paths={} top-{top} covers {}% of anchor volume",
            ranked.len(),
            if total_anchor == 0 {
                0
            } else {
                top_anchor * 100 / total_anchor
            },
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
    }
    Ok(())
}

#[cfg(windows)]
fn format_hour(hour_ts: u64) -> String {
    chrono::DateTime::from_timestamp(hour_ts as i64, 0)
        .map_or_else(|| "?".to_owned(), |ts| ts.format("%m-%d %H:%M").to_string())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only: mirrors the production fetch path and reads app settings");
}
