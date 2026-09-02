//! 官方交易所日线的导出（P11 阶段 5）：把整季经济摊成一张扁平表，喂给 AI
//! 或表格软件，程序自己不在这上面做判断。
//!
//! 为什么是日线：小时明细两周就清，日折行永久保留——能跨赛季对比的只有它。
//! 为什么三个锚一起给：锚随赛季轮动（开荒崇高 → 神圣 → 混沌+神圣并行），
//! 历史分析该用哪个锚由分析者定，程序把三份都算出来，不替人挑。
//! 为什么带"开服第 N 天"：跨赛季对齐的时间轴是相对日，不是日历日。
//! 为什么成交额只按一个锚：三个锚三列成交额是同一个数换三种单位，读者要的是
//! "按我现在用的锚"；POE1 的锚是神圣/混沌，POE2 开荒是崇高——由设置决定，
//! 锚是谁写在旁边那一列（`anchor`），别让读者猜。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ptt_trade_domain::{MarketAssetId, Ratio};
use serde::Serialize;

/// 一行 = 一天里的一个资产。字段都是可直接读的字符串/整数：导出边界上
/// 把有理数写成小数是允许的，写之前它一直是精确的。
#[derive(Clone, Debug, Serialize)]
pub struct ExchangeExportRow {
    pub league: String,
    /// UTC 日历日（YYYY-MM-DD）。
    pub day: String,
    /// 开服第 N 天（1 = 开服当天）。赛季起点没记录、或这天在开服之前时为空；
    /// 两种情况靠 `phase` 区分（前者也空，后者是 "pre_season"）。
    pub day_index: Option<u32>,
    /// 相位标签（`season_phase`）。
    pub phase: Option<&'static str>,
    /// 域层资产 id；没映射的资产用路径末段顶上，别让整行消失。
    pub asset_id: String,
    pub catalog_id: Option<String>,
    /// 游戏内交易所分类的英文 slug；没映射 = "unmapped"。
    pub category: &'static str,
    /// GGG 原始路径：映射迭代后仍能回溯。
    pub path: String,
    pub value_exalted: Option<String>,
    pub value_divine: Option<String>,
    pub value_chaos: Option<String>,
    /// 该日所有含此资产的市场里，此资产成交的单位数合计。
    pub units_traded: u64,
    /// 成交额按哪个锚计价（域 id）。POE1 是神圣/混沌，POE2 开荒是崇高——由设置决定。
    pub anchor: String,
    /// `units_traded` 按当日 `anchor` 价折算；当日算不出锚价则为空。
    pub volume_anchor: Option<u64>,
}

/// 赛季相位。边界是经验值，不是算出来的：开荒三天、首周、半月、首月、
/// 季中、季末。闪回/活动赛季节奏不同，读者要自己看联赛名。
#[must_use]
pub fn season_phase(day_index: u32) -> &'static str {
    match day_index {
        0..=3 => "launch",
        4..=7 => "week1",
        8..=15 => "half_month",
        16..=30 => "month1",
        31..=75 => "mid",
        _ => "late",
    }
}

/// 日折行 → 导出行。`season_start` 给了才有相对日与相位。
pub fn exchange_export_rows(
    day_rows: &[ptt_storage::ExchangeDayMarketRow],
    league: &str,
    game: ptt_core::Game,
    anchor: &MarketAssetId,
    season_start: Option<NaiveDate>,
) -> Result<Vec<ExchangeExportRow>, String> {
    use ptt_exchange_history::mapping::{CHAOS_PATH, DIVINE_PATH, EXALTED_PATH, categories, index};

    let mapping = index(game).map_err(|error| format!("mapping: {error}"))?;
    let categories = categories(game).map_err(|error| format!("categories: {error}"))?;
    let mut resolved: BTreeMap<String, Option<(String, MarketAssetId)>> = BTreeMap::new();
    let mut resolve = |path: &str| -> Option<(String, MarketAssetId)> {
        if let Some(hit) = resolved.get(path) {
            return hit.clone();
        }
        let hit = mapping.get(path).and_then(|catalog_id| {
            crate::live::domain_asset_id(catalog_id)
                .ok()
                .map(|domain| (catalog_id.clone(), domain))
        });
        resolved.insert(path.to_owned(), hit.clone());
        hit
    };

    // 成交单位数按 (日, 路径) 累计——没映射的也算，它们是诚实的缺口。
    let mut units: BTreeMap<(NaiveDate, String), u64> = BTreeMap::new();
    let mut day_stats = Vec::with_capacity(day_rows.len());
    for row in day_rows {
        let day = NaiveDate::parse_from_str(&row.utc_day, "%Y-%m-%d")
            .map_err(|error| format!("day {}: {error}", row.utc_day))?;
        *units.entry((day, row.asset_a.clone())).or_insert(0) += row.volume_a;
        *units.entry((day, row.asset_b.clone())).or_insert(0) += row.volume_b;
        if let (Some((_, asset_a)), Some((_, asset_b))) =
            (resolve(&row.asset_a), resolve(&row.asset_b))
        {
            day_stats.push(ptt_strategy::ExchangePairDay {
                day,
                asset_a,
                asset_b,
                volume_a: row.volume_a,
                volume_b: row.volume_b,
            });
        }
    }

    // 三个锚各算一份日价。锚自己对自己永远 1:1。
    let anchors: Vec<MarketAssetId> = [EXALTED_PATH, DIVINE_PATH, CHAOS_PATH]
        .iter()
        .map(|path| {
            resolve(path)
                .map(|(_, domain)| domain)
                .ok_or_else(|| format!("anchor path {path} is not mapped"))
        })
        .collect::<Result<_, _>>()?;
    let thresholds = ptt_strategy::AnalyticsThresholds::default();
    let mut values: Vec<BTreeMap<(MarketAssetId, NaiveDate), Ratio>> = Vec::new();
    let mut anchor_volume: BTreeMap<(MarketAssetId, NaiveDate), u64> = BTreeMap::new();
    for trio_anchor in &anchors {
        let pulse =
            ptt_strategy::exchange_pulse(&day_stats, &[], trio_anchor, 1, &thresholds, None);
        let mut table = BTreeMap::new();
        for asset in &pulse.assets {
            for (day, rate) in &asset.value_by_day {
                table.insert((asset.asset_id.clone(), *day), rate.clone());
            }
            if trio_anchor == anchor {
                for (day, volume) in &asset.anchor_volume_by_day {
                    anchor_volume.insert((asset.asset_id.clone(), *day), *volume);
                }
            }
        }
        values.push(table);
    }
    if !anchors.contains(anchor) {
        // 用户的锚不在三件套里：多跑一遍脉搏，只为按它折算成交额。
        let pulse = ptt_strategy::exchange_pulse(&day_stats, &[], anchor, 1, &thresholds, None);
        for asset in &pulse.assets {
            for (day, volume) in &asset.anchor_volume_by_day {
                anchor_volume.insert((asset.asset_id.clone(), *day), *volume);
            }
        }
    }
    let one = Ratio::from_parts(1, 1).map_err(|error| format!("{error:?}"))?;
    let value_of = |index: usize, asset: &MarketAssetId, day: NaiveDate| -> Option<Ratio> {
        if *asset == anchors[index] {
            return Some(one.clone());
        }
        values[index].get(&(asset.clone(), day)).cloned()
    };

    let mut rows = Vec::with_capacity(units.len());
    for ((day, path), units_traded) in units {
        if units_traded == 0 {
            continue;
        }
        let day_index = season_start.and_then(|start| {
            u32::try_from((day - start).num_days() + 1)
                .ok()
                .filter(|index| *index >= 1)
        });
        let resolved = resolve(&path);
        let (asset_id, catalog_id, category) = match &resolved {
            Some((catalog_id, domain)) => (
                domain.to_string(),
                Some(catalog_id.clone()),
                categories.get(catalog_id).copied().unwrap_or("other"),
            ),
            None => (
                path.rsplit('/').next().unwrap_or(&path).to_owned(),
                None,
                "unmapped",
            ),
        };
        let domain = resolved.as_ref().map(|(_, domain)| domain);
        let value = |index: usize| {
            domain
                .and_then(|asset| value_of(index, asset, day))
                .map(|rate| ratio_decimal(&rate, 6))
        };
        let volume_anchor = domain.and_then(|asset| {
            if asset == anchor {
                Some(units_traded)
            } else {
                anchor_volume.get(&(asset.clone(), day)).copied()
            }
        });
        rows.push(ExchangeExportRow {
            league: league.to_owned(),
            day: day.to_string(),
            day_index,
            // 赛季记了但这天在开服前 = "pre_season";没记赛季才是双空。
            phase: match (season_start, day_index) {
                (None, _) => None,
                (Some(_), None) => Some("pre_season"),
                (Some(_), Some(index)) => Some(season_phase(index)),
            },
            asset_id,
            catalog_id,
            category,
            path,
            value_exalted: value(0),
            value_divine: value(1),
            value_chaos: value(2),
            units_traded,
            anchor: anchor.to_string(),
            volume_anchor,
        });
    }
    Ok(rows)
}

/// 一次导出落下的东西：文件基名（加 .csv/.json 就是两份文件）与写进去的行。
pub struct ExchangeExportOutcome {
    pub base: std::path::PathBuf,
    pub rows: Vec<ExchangeExportRow>,
    pub season_start: Option<NaiveDate>,
}

/// 读生产日线表 → 导出行 → 写 `exchange-<league>-<时间戳>.csv/.json` 到 `directory`。
/// 页面按钮与探针 `--export` 走的是同一个函数：探针镜像生产路径的老规矩。
pub fn write_exchange_export(
    game: ptt_core::Game,
    league: &str,
    anchor: &MarketAssetId,
    directory: &std::path::Path,
) -> Result<ExchangeExportOutcome, String> {
    let store = ptt_storage::MarketStore::open(crate::pipeline::default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let days = store
        .load_exchange_days(game.as_str(), league, "2000-01-01", "2999-12-31")
        .map_err(|error| format!("days: {error}"))?;
    // 赛季记录不分联赛（`SeasonRow` 没有 league 字段）：导出别的联赛时,
    // 相对日按当前赛季算——单人自用,一次只跟一个赛季,先这样。
    let season_start = store
        .active_season(game.as_str())
        .ok()
        .flatten()
        .map(|season| season.started_at.date_naive());
    let rows = exchange_export_rows(&days, league, game, anchor, season_start)?;
    std::fs::create_dir_all(directory).map_err(|error| format!("mkdir: {error}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let slug = league.trim().to_lowercase().replace(' ', "-");
    let base = directory.join(format!("exchange-{slug}-{stamp}"));
    std::fs::write(base.with_extension("csv"), export_csv(&rows))
        .map_err(|error| format!("write csv: {error}"))?;
    std::fs::write(base.with_extension("json"), export_json(&rows))
        .map_err(|error| format!("write json: {error}"))?;
    Ok(ExchangeExportOutcome {
        base,
        rows,
        season_start,
    })
}

/// CSV：一行表头，字段顺序与结构体一致。逗号/引号/换行才加引号。
#[must_use]
pub fn export_csv(rows: &[ExchangeExportRow]) -> String {
    let mut out = String::from(
        "league,day,day_index,phase,asset_id,catalog_id,category,path,value_exalted,value_divine,value_chaos,units_traded,anchor,volume_anchor\n",
    );
    let opt = |value: &Option<String>| value.clone().unwrap_or_default();
    for row in rows {
        let fields = [
            csv_field(&row.league),
            row.day.clone(),
            row.day_index
                .map(|index| index.to_string())
                .unwrap_or_default(),
            row.phase.unwrap_or_default().to_owned(),
            csv_field(&row.asset_id),
            csv_field(&opt(&row.catalog_id)),
            row.category.to_owned(),
            csv_field(&row.path),
            opt(&row.value_exalted),
            opt(&row.value_divine),
            opt(&row.value_chaos),
            row.units_traded.to_string(),
            csv_field(&row.anchor),
            row.volume_anchor
                .map(|volume| volume.to_string())
                .unwrap_or_default(),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

/// JSON：对象数组，字段名同 CSV 表头。
#[must_use]
pub fn export_json(rows: &[ExchangeExportRow]) -> String {
    serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".to_owned())
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// 有理数 → 小数字符串（最多 `scale` 位，去掉尾零）。整数除法，不经 f64。
#[must_use]
pub fn ratio_decimal(ratio: &Ratio, scale: u32) -> String {
    let numerator = u128::from(ratio.numerator);
    let denominator = u128::from(ratio.denominator).max(1);
    let whole = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        return whole.to_string();
    }
    let fraction = remainder * 10u128.pow(scale) / denominator;
    let digits = format!("{fraction:0width$}", width = scale as usize);
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{digits}")
    }
}

#[cfg(test)]
mod exchange_export_tests {
    use super::*;

    const EXALTED: &str = "Metadata/Items/Currency/CurrencyAddModToRare";
    const DIVINE: &str = "Metadata/Items/Currency/CurrencyModValues";
    const CHAOS: &str = "Metadata/Items/Currency/CurrencyRerollRare";

    fn row(
        day: &str,
        a: &str,
        b: &str,
        volume_a: u64,
        volume_b: u64,
    ) -> ptt_storage::ExchangeDayMarketRow {
        ptt_storage::ExchangeDayMarketRow {
            utc_day: day.to_owned(),
            asset_a: a.to_owned(),
            asset_b: b.to_owned(),
            volume_a,
            volume_b,
            hours_covered: 24,
        }
    }

    fn id(text: &str) -> MarketAssetId {
        MarketAssetId::try_new(text).expect("asset id")
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("day")
    }

    fn find<'a>(rows: &'a [ExchangeExportRow], day: &str, asset: &str) -> &'a ExchangeExportRow {
        rows.iter()
            .find(|row| row.day == day && row.asset_id == asset)
            .unwrap_or_else(|| panic!("no row for {asset} on {day}"))
    }

    #[test]
    fn the_anchor_trio_is_priced_in_each_other_with_relative_days() {
        // 路径序：AddModToRare(崇高) < ModValues(神圣) < RerollRare(混沌)。
        // 神圣 = 400 崇高；混沌：100 崇高换 1000 混沌 = 0.1 崇高。
        let rows = vec![
            row("2026-09-05", EXALTED, DIVINE, 4000, 10),
            row("2026-09-05", EXALTED, CHAOS, 100, 1000),
            row("2026-09-06", EXALTED, DIVINE, 4200, 10),
        ];
        let export = exchange_export_rows(
            &rows,
            "Runes of Aldur",
            ptt_core::Game::Poe2,
            &id("exalted-orb"),
            Some(day("2026-09-05")),
        )
        .expect("rows");
        let divine = find(&export, "2026-09-05", "divine-orb");
        assert_eq!((divine.day_index, divine.phase), (Some(1), Some("launch")));
        assert_eq!(divine.category, "currency");
        assert_eq!(divine.catalog_id.as_deref(), Some("divine_orb"));
        assert_eq!(divine.value_exalted.as_deref(), Some("400"));
        assert_eq!(divine.value_divine.as_deref(), Some("1"));
        // 神圣没有直连混沌市场：经崇高一步桥接，400 × 10 = 4000 混沌。
        assert_eq!(divine.value_chaos.as_deref(), Some("4000"));
        assert_eq!(divine.units_traded, 10);
        assert_eq!(divine.anchor, "exalted-orb");
        assert_eq!(divine.volume_anchor, Some(4000));

        let exalted = find(&export, "2026-09-05", "exalted-orb");
        assert_eq!(exalted.value_exalted.as_deref(), Some("1"));
        assert_eq!(exalted.value_divine.as_deref(), Some("0.0025"));
        assert_eq!(exalted.value_chaos.as_deref(), Some("10"));
        assert_eq!(exalted.units_traded, 4100);
        assert_eq!(exalted.volume_anchor, Some(4100));

        let next = find(&export, "2026-09-06", "divine-orb");
        assert_eq!(next.day_index, Some(2));
        assert_eq!(next.value_exalted.as_deref(), Some("420"));
    }

    #[test]
    fn volume_is_priced_in_the_requested_anchor() {
        // POE1 的锚是神圣：同一批行，成交额按神圣折算，锚那一列写明白。
        let rows = vec![
            row("2026-09-05", EXALTED, DIVINE, 4000, 10),
            row("2026-09-05", EXALTED, CHAOS, 100, 1000),
        ];
        let export = exchange_export_rows(
            &rows,
            "Allflame",
            ptt_core::Game::Poe1,
            &id("divine-orb"),
            None,
        )
        .expect("rows");
        let divine = find(&export, "2026-09-05", "divine-orb");
        assert_eq!(divine.anchor, "divine-orb");
        assert_eq!(divine.volume_anchor, Some(10));
        // 崇高 4100 单位 × 1/400 神圣 = 10 神圣（整数除法向下取整）。
        let exalted = find(&export, "2026-09-05", "exalted-orb");
        assert_eq!(exalted.value_divine.as_deref(), Some("0.0025"));
        assert_eq!(exalted.volume_anchor, Some(10));
        // 三列价格照旧都在：分析者仍可自己换锚。
        assert_eq!(exalted.value_exalted.as_deref(), Some("1"));
        assert_eq!(exalted.value_chaos.as_deref(), Some("10"));
    }

    #[test]
    fn unmapped_paths_stay_in_the_export_as_honest_gaps() {
        let rows = vec![
            row("2026-09-05", EXALTED, DIVINE, 4000, 10),
            row(
                "2026-09-05",
                "Metadata/Items/Currency/BrandNewThing",
                DIVINE,
                7,
                1,
            ),
        ];
        let export = exchange_export_rows(
            &rows,
            "Runes of Aldur",
            ptt_core::Game::Poe2,
            &id("exalted-orb"),
            None,
        )
        .expect("rows");
        let gap = find(&export, "2026-09-05", "BrandNewThing");
        assert_eq!(gap.category, "unmapped");
        assert_eq!(gap.catalog_id, None);
        assert_eq!(gap.value_exalted, None);
        assert_eq!(gap.units_traded, 7);
        // 没记赛季起点：相对日与相位诚实留空。
        assert_eq!((gap.day_index, gap.phase), (None, None));
        // 神圣的单位数把两个市场都算进去。
        assert_eq!(find(&export, "2026-09-05", "divine-orb").units_traded, 11);
    }

    #[test]
    fn csv_and_json_carry_the_same_rows() {
        let rows = vec![row("2026-09-05", EXALTED, DIVINE, 4000, 10)];
        let export = exchange_export_rows(
            &rows,
            "Runes of Aldur",
            ptt_core::Game::Poe2,
            &id("exalted-orb"),
            Some(day("2026-09-01")),
        )
        .expect("rows");
        let csv = export_csv(&export);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), export.len() + 1);
        // 表头改名是格式变更：成交额按当前锚折算，锚是谁写在旁边那一列。
        assert_eq!(
            lines[0],
            "league,day,day_index,phase,asset_id,catalog_id,category,path,value_exalted,value_divine,value_chaos,units_traded,anchor,volume_anchor"
        );
        assert!(lines.iter().skip(1).any(|line| {
            line.starts_with("Runes of Aldur,2026-09-05,5,week1,divine-orb,divine_orb,currency,")
        }));
        let json: Vec<serde_json::Value> =
            serde_json::from_str(&export_json(&export)).expect("json");
        assert_eq!(json.len(), export.len());
        assert_eq!(json[0]["league"], "Runes of Aldur");
    }

    #[test]
    fn phases_and_decimals() {
        assert_eq!(season_phase(1), "launch");
        assert_eq!(season_phase(3), "launch");
        assert_eq!(season_phase(4), "week1");
        assert_eq!(season_phase(8), "half_month");
        assert_eq!(season_phase(16), "month1");
        assert_eq!(season_phase(31), "mid");
        assert_eq!(season_phase(76), "late");
        assert_eq!(
            ratio_decimal(&Ratio::from_parts(1, 3).unwrap(), 6),
            "0.333333"
        );
        assert_eq!(ratio_decimal(&Ratio::from_parts(400, 1).unwrap(), 6), "400");
        assert_eq!(
            ratio_decimal(&Ratio::from_parts(1, 400).unwrap(), 6),
            "0.0025"
        );
        assert_eq!(csv_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn a_day_before_the_season_is_labelled_pre_season_not_left_blank() {
        // 赛季起点记了,但这天在开服之前(回补的上季尾巴):相对日留空,
        // 相位写 pre_season——和"没记赛季"的双空区分开,AI 读表才分得清。
        let rows = [row("2026-09-05", EXALTED, DIVINE, 4000, 10)];
        let start = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).expect("date");
        let export = exchange_export_rows(
            &rows,
            "Runes of Aldur",
            ptt_core::Game::Poe2,
            &id("exalted-orb"),
            Some(start),
        )
        .expect("rows");
        let before = find(&export, "2026-09-05", "divine-orb");
        assert_eq!((before.day_index, before.phase), (None, Some("pre_season")));
    }
}
