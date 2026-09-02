//! 官方交易所小时史的市场脉搏（P11 阶段 2）。
//!
//! 与 `market_analytics::market_pulse` 平行而不合并：那边的输入是挂单簿
//! 语义（挂单量、供需归属、`LiquidityClass`），这边的输入是**真实成交量**
//! ——两类证据分域并存是已拍板的裁定（2026-08-31），成交量灌进挂单容器
//! 会冒名顶替。复用的是原语（`compose`、`window_trend_bps`、`lower_middle`、
//! `anchor_value`、`TrendVerdict`），不复用容器。
//!
//! 汇率全部来自成交量比值（两个整数的比 = 精确有理数），API 的快照区间
//! 不进这里——那是展示与对账的存证，不是计算材料。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ptt_trade_domain::{MarketAssetId, Ratio};

use crate::day_rollup::lower_middle;
use crate::market_analytics::{
    AnalyticsThresholds, TrendVerdict, anchor_value, bps_between, compose,
};

/// 一天里一个无向对的成交合计（已映射到 domain 资产 id）。
#[derive(Clone, Debug)]
pub struct ExchangePairDay {
    pub day: NaiveDate,
    pub asset_a: MarketAssetId,
    pub asset_b: MarketAssetId,
    pub volume_a: u64,
    pub volume_b: u64,
}

/// 一小时里一个无向对的成交与库存区间（已映射）。
#[derive(Clone, Debug)]
pub struct ExchangePairHour {
    pub hour_ts: i64,
    pub asset_a: MarketAssetId,
    pub asset_b: MarketAssetId,
    pub volume_a: u64,
    pub volume_b: u64,
    pub lowest_stock_a: u64,
    pub highest_stock_a: u64,
    pub lowest_stock_b: u64,
    pub highest_stock_b: u64,
}

#[derive(Clone, Debug)]
pub struct ExchangeAssetPulse {
    pub asset_id: MarketAssetId,
    /// 最新有成交的小时的锚计价 VWAP。没有直连锚市场时经一次桥接合成；
    /// 合成不出来就是 `None`——"算不出"是答案，不是猜。
    pub value_in_anchor: Option<Ratio>,
    /// 日 VWAP 序列（升序），ninja 式 7 天走势的原料。
    pub value_by_day: Vec<(NaiveDate, Ratio)>,
    pub trend_bps_raw: Option<i64>,
    /// 扣掉市场中位漂移之后的趋势。赛季末实测漂移 +794bps/周——
    /// 不扣的话满屏都是假升值。
    pub trend_bps_relative: Option<i64>,
    pub verdict: Option<TrendVerdict>,
    /// 小时窗口内的锚计价成交量 / 有数据的小时数。表格的默认排序键。
    pub volume_per_hour_anchor: u64,
    /// 库存区间中点的锚计价（辅助列）。挂单深度语义归 OCR 侧管，
    /// 这里只是"挂着多少货"的粗读数。
    pub depth_anchor: Option<u64>,
    /// 最新小时锚计价成交量相对自身小时中位的百分比（200 = 两倍）。
    pub surge_percent: Option<u64>,
    /// 成交额最大的对手资产——"最流行交易对"列。
    pub top_partner: Option<MarketAssetId>,
    /// 每日锚计价成交额（升序）：该日所有含此资产的市场里，此资产的成交
    /// 单位数 × 当日锚价。日线永久保留，所以这条是赛季节奏与导出的原料；
    /// 小时侧只有 0 时（历史视角）它还兼任排序键。
    pub anchor_volume_by_day: Vec<(NaiveDate, u64)>,
}

#[derive(Clone, Debug)]
pub struct ExchangePulse {
    pub anchor: MarketAssetId,
    pub as_of_day: Option<NaiveDate>,
    /// 全市场 raw 趋势的 lower-middle 中位。锚自己在通胀/通缩时它非零，
    /// relative = raw − 它。
    pub market_median_move_bps: Option<i64>,
    pub hours_seen: u64,
    /// 按锚计价成交量降序。
    pub assets: Vec<ExchangeAssetPulse>,
}

/// 从成交量比值里读出整个市场的脉搏。
///
/// 桥接只走一步（直连锚的资产做桥），桥选当小时/当日成交量最大的那个——
/// 多级桥每级都在放大噪声，一步是"能算"与"可信"的平衡点，与
/// `market_analytics` 的一步合成规则一致。
///
/// `as_of`：截至哪一天看。给了就是历史视角——日线只算到那天，涨跌的终点
/// 钉在那天；小时侧（最新价、成交/小时、深度、放量、对手）是"现在"的事，
/// 历史视角下整个不算，留空而不是拿旧数冒充。
pub fn exchange_pulse(
    day_stats: &[ExchangePairDay],
    hour_stats: &[ExchangePairHour],
    anchor: &MarketAssetId,
    trend_days: u32,
    thresholds: &AnalyticsThresholds,
    as_of: Option<NaiveDate>,
) -> ExchangePulse {
    let hour_stats: &[ExchangePairHour] = if as_of.is_some() { &[] } else { hour_stats };
    let day_stats: Vec<&ExchangePairDay> = day_stats
        .iter()
        .filter(|stat| as_of.is_none_or(|as_of| stat.day <= as_of))
        .collect();
    let day_stats = day_stats.as_slice();
    // ---- 每日锚计价：先直连，后桥接 ----
    // day → (asset → (anchor_volume, own_volume))，直连锚市场的成交量合计。
    let mut direct_by_day: BTreeMap<NaiveDate, BTreeMap<&MarketAssetId, (u128, u128)>> =
        BTreeMap::new();
    for stat in day_stats {
        let (asset, own, quote) = if stat.asset_a == *anchor {
            (&stat.asset_b, stat.volume_b, stat.volume_a)
        } else if stat.asset_b == *anchor {
            (&stat.asset_a, stat.volume_a, stat.volume_b)
        } else {
            continue;
        };
        if own == 0 || quote == 0 {
            continue;
        }
        let fold = direct_by_day
            .entry(stat.day)
            .or_default()
            .entry(asset)
            .or_insert((0, 0));
        fold.0 += u128::from(quote);
        fold.1 += u128::from(own);
    }
    let day_rate = |day: NaiveDate, asset: &MarketAssetId| -> Option<Ratio> {
        let (quote, own) = *direct_by_day.get(&day)?.get(asset)?;
        ratio_from_u128(quote, own)
    };

    let mut value_by_day: BTreeMap<&MarketAssetId, BTreeMap<NaiveDate, Ratio>> = BTreeMap::new();
    // 直连的先落。
    for (day, assets) in &direct_by_day {
        for (asset, (quote, own)) in assets {
            if let Some(rate) = ratio_from_u128(*quote, *own) {
                value_by_day.entry(asset).or_default().insert(*day, rate);
            }
        }
    }
    // 桥接：没有直连锚价的资产，取当日成交量最大的、当日有锚价的对手做桥。
    let mut bridged: BTreeMap<&MarketAssetId, BTreeMap<NaiveDate, Ratio>> = BTreeMap::new();
    for stat in day_stats {
        if stat.asset_a == *anchor || stat.asset_b == *anchor {
            continue;
        }
        for (asset, own, partner, partner_volume) in [
            (&stat.asset_a, stat.volume_a, &stat.asset_b, stat.volume_b),
            (&stat.asset_b, stat.volume_b, &stat.asset_a, stat.volume_a),
        ] {
            if own == 0 || partner_volume == 0 {
                continue;
            }
            if value_by_day
                .get(asset)
                .is_some_and(|days| days.contains_key(&stat.day))
            {
                continue;
            }
            let Some(anchor_per_partner) = day_rate(stat.day, partner) else {
                continue;
            };
            let Some(partner_per_asset) = ratio_from_u128(partner_volume.into(), own.into()) else {
                continue;
            };
            let Some(value) = compose(&anchor_per_partner, &partner_per_asset) else {
                continue;
            };
            // 同一天出现多个可用桥时留成交量更大的那个：桥越粗越可信。
            let slot = bridged.entry(asset).or_default();
            match slot.get(&stat.day) {
                Some(existing) if existing.compare_value(&value).is_ge() => {}
                _ => {
                    slot.insert(stat.day, value);
                }
            }
        }
    }
    for (asset, days) in bridged {
        let target = value_by_day.entry(asset).or_default();
        for (day, rate) in days {
            target.entry(day).or_insert(rate);
        }
    }

    // ---- 小时侧：最新价、成交量、深度、激增、对手 ----
    let mut direct_by_hour: BTreeMap<i64, BTreeMap<&MarketAssetId, (u128, u128)>> = BTreeMap::new();
    for stat in hour_stats {
        let (asset, own, quote) = if stat.asset_a == *anchor {
            (&stat.asset_b, stat.volume_b, stat.volume_a)
        } else if stat.asset_b == *anchor {
            (&stat.asset_a, stat.volume_a, stat.volume_b)
        } else {
            continue;
        };
        if own == 0 || quote == 0 {
            continue;
        }
        let fold = direct_by_hour
            .entry(stat.hour_ts)
            .or_default()
            .entry(asset)
            .or_insert((0, 0));
        fold.0 += u128::from(quote);
        fold.1 += u128::from(own);
    }
    let hour_rate = |hour_ts: i64, asset: &MarketAssetId| -> Option<Ratio> {
        let (quote, own) = *direct_by_hour.get(&hour_ts)?.get(asset)?;
        ratio_from_u128(quote, own)
    };

    struct HourFold {
        latest_value: Option<(i64, Ratio)>,
        anchor_volume_by_hour: BTreeMap<i64, u128>,
        depth_sum: u128,
        depth_samples: u64,
        partner_volume: BTreeMap<MarketAssetId, u128>,
    }
    let mut folds: BTreeMap<&MarketAssetId, HourFold> = BTreeMap::new();
    let hours_seen: u64 = {
        let mut hours: Vec<i64> = hour_stats.iter().map(|stat| stat.hour_ts).collect();
        hours.sort_unstable();
        hours.dedup();
        hours.len() as u64
    };

    for stat in hour_stats {
        for (asset, own, low, high, partner, partner_volume) in [
            (
                &stat.asset_a,
                stat.volume_a,
                stat.lowest_stock_a,
                stat.highest_stock_a,
                &stat.asset_b,
                stat.volume_b,
            ),
            (
                &stat.asset_b,
                stat.volume_b,
                stat.lowest_stock_b,
                stat.highest_stock_b,
                &stat.asset_a,
                stat.volume_a,
            ),
        ] {
            if asset == anchor || own == 0 || partner_volume == 0 {
                continue;
            }
            // 该小时这资产的锚价：直连优先，否则经对手一步桥接。
            let value = hour_rate(stat.hour_ts, asset).or_else(|| {
                let anchor_per_partner = hour_rate(stat.hour_ts, partner)?;
                let partner_per_asset = ratio_from_u128(partner_volume.into(), own.into())?;
                compose(&anchor_per_partner, &partner_per_asset)
            });
            let Some(value) = value else { continue };
            let fold = folds.entry(asset).or_insert_with(|| HourFold {
                latest_value: None,
                anchor_volume_by_hour: BTreeMap::new(),
                depth_sum: 0,
                depth_samples: 0,
                partner_volume: BTreeMap::new(),
            });
            let traded_anchor = anchor_value(u128::from(own), &value);
            *fold.anchor_volume_by_hour.entry(stat.hour_ts).or_insert(0) += traded_anchor;
            *fold.partner_volume.entry(partner.clone()).or_insert(0) += traded_anchor;
            let stock_mid = (u128::from(low) + u128::from(high)) / 2;
            fold.depth_sum += anchor_value(stock_mid, &value);
            fold.depth_samples += 1;
            if fold
                .latest_value
                .as_ref()
                .is_none_or(|(at, _)| *at <= stat.hour_ts)
            {
                fold.latest_value = Some((stat.hour_ts, value));
            }
        }
    }

    // 只有日线的资产也进表：历史视角下没有任何小时，表不能因此空掉。
    for asset in value_by_day.keys() {
        folds.entry(asset).or_insert_with(|| HourFold {
            latest_value: None,
            anchor_volume_by_hour: BTreeMap::new(),
            depth_sum: 0,
            depth_samples: 0,
            partner_volume: BTreeMap::new(),
        });
    }

    // ---- 日成交额（锚计价）----
    // 该日所有含此资产的市场里，此资产的成交单位数 × 当日锚价（直连或桥接）。
    // 没算出当日锚价的那天不计——"算不出"不冒充零。
    let mut day_volume: BTreeMap<&MarketAssetId, BTreeMap<NaiveDate, u128>> = BTreeMap::new();
    for stat in day_stats {
        for (asset, own) in [
            (&stat.asset_a, stat.volume_a),
            (&stat.asset_b, stat.volume_b),
        ] {
            if asset == anchor || own == 0 {
                continue;
            }
            let Some(value) = value_by_day.get(asset).and_then(|days| days.get(&stat.day)) else {
                continue;
            };
            *day_volume
                .entry(asset)
                .or_default()
                .entry(stat.day)
                .or_insert(0) += anchor_value(u128::from(own), value);
        }
    }

    // ---- 趋势与汇总 ----
    let as_of_day = as_of.or_else(|| {
        value_by_day
            .values()
            .filter_map(|days| days.keys().next_back().copied())
            .max()
    });
    let mut raw_trend: BTreeMap<&MarketAssetId, i64> = BTreeMap::new();
    for (asset, days) in &value_by_day {
        if let Some(bps) = endpoint_trend_bps(days, trend_days) {
            raw_trend.insert(asset, bps);
        }
    }
    let market_median_move_bps = lower_middle(raw_trend.values().copied().collect());

    let mut assets: Vec<ExchangeAssetPulse> = folds
        .into_iter()
        .map(|(asset_id, fold)| {
            let volume_total: u128 = fold.anchor_volume_by_hour.values().sum();
            let hours_present = fold.anchor_volume_by_hour.len() as u128;
            let latest_hour_volume = fold
                .anchor_volume_by_hour
                .last_key_value()
                .map(|(_, volume)| *volume);
            // 激增基准用"排除最新小时后的自身中位"：拿自己和自己比，
            // 新小时才算得上"异常"。
            let baseline_volumes: Vec<u128> = fold
                .anchor_volume_by_hour
                .iter()
                .rev()
                .skip(1)
                .map(|(_, volume)| *volume)
                .collect();
            let surge_percent = match (latest_hour_volume, lower_middle(baseline_volumes)) {
                (Some(latest), Some(median))
                    if median > 0 && fold.anchor_volume_by_hour.len() >= 8 =>
                {
                    u64::try_from(latest * 100 / median).ok()
                }
                _ => None,
            };
            let trend_bps_raw = raw_trend.get(&asset_id).copied();
            let trend_bps_relative = match (trend_bps_raw, market_median_move_bps) {
                (Some(raw), Some(median)) => Some(raw - median),
                _ => None,
            };
            let verdict = trend_bps_relative.map(|relative| {
                if relative > thresholds.verdict_threshold_bps {
                    TrendVerdict::Appreciating
                } else if relative < -thresholds.verdict_threshold_bps {
                    TrendVerdict::Depreciating
                } else {
                    TrendVerdict::Holding
                }
            });
            let top_partner = fold
                .partner_volume
                .iter()
                .max_by_key(|(_, volume)| **volume)
                .map(|(partner, _)| partner.clone());
            ExchangeAssetPulse {
                value_in_anchor: fold.latest_value.map(|(_, value)| value),
                value_by_day: value_by_day
                    .get(&asset_id)
                    .map(|days| {
                        days.iter()
                            .map(|(day, rate)| (*day, rate.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                trend_bps_raw,
                trend_bps_relative,
                verdict,
                volume_per_hour_anchor: u64::try_from(volume_total / hours_present.max(1))
                    .unwrap_or(u64::MAX),
                depth_anchor: (fold.depth_samples > 0).then(|| {
                    u64::try_from(fold.depth_sum / u128::from(fold.depth_samples))
                        .unwrap_or(u64::MAX)
                }),
                surge_percent,
                top_partner,
                anchor_volume_by_day: day_volume
                    .remove(&asset_id)
                    .map(|days| {
                        days.into_iter()
                            .map(|(day, volume)| (day, u64::try_from(volume).unwrap_or(u64::MAX)))
                            .collect()
                    })
                    .unwrap_or_default(),
                asset_id: asset_id.clone(),
            }
        })
        .collect();
    // 小时成交在前；全是 0（历史视角）时日成交额接手排序。
    let day_total = |pulse: &ExchangeAssetPulse| -> u128 {
        pulse
            .anchor_volume_by_day
            .iter()
            .map(|(_, volume)| u128::from(*volume))
            .sum()
    };
    assets.sort_by(|left, right| {
        right
            .volume_per_hour_anchor
            .cmp(&left.volume_per_hour_anchor)
            .then_with(|| day_total(right).cmp(&day_total(left)))
            .then_with(|| left.asset_id.cmp(&right.asset_id))
    });

    ExchangePulse {
        anchor: anchor.clone(),
        as_of_day,
        market_median_move_bps,
        hours_seen,
        assets,
    }
}

/// N 天涨跌：最新一天的日 VWAP 对"N 天前或更早的最近一天"的 bps。
///
/// 端点比较而不是窗口中位——这列回答的是 ninja 式的"过去 N 天涨了多少"，
/// N 由用户在表头轮换。数据不足 N 天时退到最早那天：
/// 上限自动受已有数据限制（用户裁定），"不足"不该变成一列空白。
fn endpoint_trend_bps(days: &BTreeMap<NaiveDate, Ratio>, trend_days: u32) -> Option<i64> {
    let (latest_day, latest) = days.iter().next_back()?;
    let target = *latest_day - chrono::Days::new(u64::from(trend_days));
    let (baseline_day, baseline) = days
        .range(..=target)
        .next_back()
        .or_else(|| days.iter().next())?;
    if baseline_day == latest_day {
        return None;
    }
    Some(bps_between(baseline, latest))
}

/// u128 累计量折回 Ratio。约分后仍溢出 u64 就放弃——不近似。
fn ratio_from_u128(numerator: u128, denominator: u128) -> Option<Ratio> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let divisor = gcd_u128(numerator, denominator);
    let numerator = u64::try_from(numerator / divisor).ok()?;
    let denominator = u64::try_from(denominator / divisor).ok()?;
    Ratio::from_parts(numerator, denominator).ok()
}

const fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left == 0 { 1 } else { left }
}

#[cfg(test)]
mod exchange_pulse_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn thresholds() -> AnalyticsThresholds {
        AnalyticsThresholds::try_new(2, 7, 70, 500, 300, 0).expect("thresholds")
    }

    fn day(offset: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 1 + offset).expect("day")
    }

    /// asset 对锚：每天 own 单位换 quote 锚。
    fn day_stat(offset: u32, id: &str, own: u64, quote: u64) -> ExchangePairDay {
        ExchangePairDay {
            day: day(offset),
            asset_a: asset("exalted-orb"),
            asset_b: asset(id),
            volume_a: quote,
            volume_b: own,
        }
    }

    fn hour_stat(hour: i64, a: &str, b: &str, volume_a: u64, volume_b: u64) -> ExchangePairHour {
        ExchangePairHour {
            hour_ts: 1_788_000_000 / 3600 * 3600 + hour * 3600,
            asset_a: asset(a),
            asset_b: asset(b),
            volume_a,
            volume_b,
            lowest_stock_a: volume_a,
            highest_stock_a: volume_a,
            lowest_stock_b: volume_b,
            highest_stock_b: volume_b,
        }
    }

    #[test]
    fn direct_market_prices_and_trends() {
        // 9 天：前 7 天 100 锚，最后 2 天 120 锚 → raw ≈ +2000bps。
        let mut days = Vec::new();
        for offset in 0..7 {
            days.push(day_stat(offset, "divine-orb", 10, 1000));
        }
        days.push(day_stat(7, "divine-orb", 10, 1200));
        days.push(day_stat(8, "divine-orb", 10, 1200));
        let hours = vec![hour_stat(0, "exalted-orb", "divine-orb", 1200, 10)];
        let pulse = exchange_pulse(&days, &hours, &asset("exalted-orb"), 7, &thresholds(), None);
        let divine = &pulse.assets[0];
        assert_eq!(divine.asset_id, asset("divine-orb"));
        assert_eq!(
            divine.value_in_anchor,
            Some(Ratio::from_parts(120, 1).expect("ratio"))
        );
        assert_eq!(divine.trend_bps_raw, Some(2000));
        // 只有一个资产 → 市场中位 = 它自己 → relative 0 → Holding。
        assert_eq!(divine.verdict, Some(TrendVerdict::Holding));
        assert_eq!(divine.value_by_day.len(), 9);
    }

    #[test]
    fn market_median_separates_anchor_drift_from_real_moves() {
        let mut days = Vec::new();
        // 三个资产同涨 20%（锚在贬值），第四个涨 50%（真升值）。
        for id in ["divine-orb", "chaos-orb", "vaal-orb"] {
            for offset in 0..7 {
                days.push(day_stat(offset, id, 10, 1000));
            }
            days.push(day_stat(7, id, 10, 1200));
            days.push(day_stat(8, id, 10, 1200));
        }
        for offset in 0..7 {
            days.push(day_stat(offset, "mirror-shard", 10, 1000));
        }
        days.push(day_stat(7, "mirror-shard", 10, 1500));
        days.push(day_stat(8, "mirror-shard", 10, 1500));
        let hours: Vec<ExchangePairHour> = ["divine-orb", "chaos-orb", "vaal-orb", "mirror-shard"]
            .iter()
            .map(|id| hour_stat(0, "exalted-orb", id, 1000, 10))
            .collect();
        let pulse = exchange_pulse(&days, &hours, &asset("exalted-orb"), 7, &thresholds(), None);
        assert_eq!(pulse.market_median_move_bps, Some(2000));
        let mirror = pulse
            .assets
            .iter()
            .find(|pulse| pulse.asset_id == asset("mirror-shard"))
            .expect("mirror");
        assert_eq!(mirror.trend_bps_raw, Some(5000));
        assert_eq!(mirror.trend_bps_relative, Some(3000));
        assert_eq!(mirror.verdict, Some(TrendVerdict::Appreciating));
        let divine = pulse
            .assets
            .iter()
            .find(|pulse| pulse.asset_id == asset("divine-orb"))
            .expect("divine");
        assert_eq!(divine.trend_bps_relative, Some(0));
        assert_eq!(divine.verdict, Some(TrendVerdict::Holding));
    }

    #[test]
    fn one_step_bridge_composes_through_the_biggest_partner() {
        // 魔镜没有直连锚市场：1 魔镜 = 100 神圣，1 神圣 = 400 锚 → 40000 锚。
        let days = vec![
            day_stat(0, "divine-orb", 10, 4000),
            ExchangePairDay {
                day: day(0),
                asset_a: asset("divine-orb"),
                asset_b: asset("mirror-of-kalandra"),
                volume_a: 100,
                volume_b: 1,
            },
        ];
        let pulse = exchange_pulse(&days, &[], &asset("exalted-orb"), 7, &thresholds(), None);
        // 只有日线也进表（历史视角就是这样看的）：日价必须合成出来，
        // 最新价属于小时侧，没有就是 None。
        let mirror = pulse
            .assets
            .iter()
            .find(|entry| entry.asset_id == asset("mirror-of-kalandra"))
            .expect("mirror priced via bridge from days alone");
        assert_eq!(mirror.value_in_anchor, None);
        assert_eq!(
            mirror.value_by_day,
            vec![(day(0), Ratio::from_parts(40_000, 1).expect("ratio"))]
        );
        // 带小时数据时最新价也合成出来。
        let hours = vec![
            hour_stat(0, "divine-orb", "exalted-orb", 10, 4000),
            hour_stat(0, "divine-orb", "mirror-of-kalandra", 100, 1),
        ];
        let pulse = exchange_pulse(&days, &hours, &asset("exalted-orb"), 7, &thresholds(), None);
        let mirror = pulse
            .assets
            .iter()
            .find(|entry| entry.asset_id == asset("mirror-of-kalandra"))
            .expect("mirror priced via bridge");
        assert_eq!(
            mirror.value_in_anchor,
            Some(Ratio::from_parts(40_000, 1).expect("ratio"))
        );
        assert_eq!(mirror.value_by_day.len(), 1);
    }

    #[test]
    fn volume_ranking_and_top_partner() {
        let hours = vec![
            hour_stat(0, "exalted-orb", "divine-orb", 8000, 20),
            hour_stat(0, "exalted-orb", "chaos-orb", 500, 50),
            hour_stat(0, "chaos-orb", "divine-orb", 40, 1),
        ];
        let pulse = exchange_pulse(&[], &hours, &asset("exalted-orb"), 7, &thresholds(), None);
        assert_eq!(pulse.assets[0].asset_id, asset("divine-orb"));
        assert_eq!(pulse.assets[0].top_partner, Some(asset("exalted-orb")));
        assert!(pulse.assets[0].volume_per_hour_anchor >= pulse.assets[1].volume_per_hour_anchor);
    }

    #[test]
    fn surge_needs_history_and_fires_on_a_spike() {
        // 9 个小时:前 8 个每小时 1000 锚,最新一小时 5000 锚 → 500%。
        let mut hours = Vec::new();
        for hour in 0..8 {
            hours.push(hour_stat(hour, "exalted-orb", "divine-orb", 1000, 10));
        }
        hours.push(hour_stat(8, "exalted-orb", "divine-orb", 5000, 50));
        let pulse = exchange_pulse(&[], &hours, &asset("exalted-orb"), 7, &thresholds(), None);
        assert_eq!(pulse.assets[0].surge_percent, Some(500));
        // 只有两个小时的历史撑不起"异常"的说法。
        let short = vec![
            hour_stat(0, "exalted-orb", "divine-orb", 1000, 10),
            hour_stat(1, "exalted-orb", "divine-orb", 5000, 50),
        ];
        let pulse = exchange_pulse(&[], &short, &asset("exalted-orb"), 7, &thresholds(), None);
        assert_eq!(pulse.assets[0].surge_percent, None);
    }

    #[test]
    fn as_of_freezes_the_trend_and_ignores_the_hours() {
        // 9 天：前 7 天 100 锚，后 2 天 120 锚。截至第 6 天（offset 5）看：
        // 涨跌只看那天及之前的日线，所以还是平的；小时侧（最新价/成交/
        // 深度）是"现在"的东西，历史视角下一概不算——诚实的空，不是旧数。
        let mut days = Vec::new();
        for offset in 0..7 {
            days.push(day_stat(offset, "divine-orb", 10, 1000));
        }
        days.push(day_stat(7, "divine-orb", 10, 1200));
        days.push(day_stat(8, "divine-orb", 10, 1200));
        let hours = vec![hour_stat(0, "exalted-orb", "divine-orb", 1200, 10)];
        let pulse = exchange_pulse(
            &days,
            &hours,
            &asset("exalted-orb"),
            7,
            &thresholds(),
            Some(day(5)),
        );
        assert_eq!(pulse.as_of_day, Some(day(5)));
        assert_eq!(pulse.hours_seen, 0);
        let divine = &pulse.assets[0];
        assert_eq!(divine.value_in_anchor, None);
        assert_eq!(divine.volume_per_hour_anchor, 0);
        assert_eq!(divine.depth_anchor, None);
        assert_eq!(divine.value_by_day.len(), 6);
        assert_eq!(divine.trend_bps_raw, Some(0));
        // 日成交量按锚计价随日线走：每天 10 颗 × 100 锚 = 1000 锚。
        assert_eq!(divine.anchor_volume_by_day.len(), 6);
        assert_eq!(divine.anchor_volume_by_day[0], (day(0), 1000));
    }

    #[test]
    fn a_day_only_asset_still_appears_ranked_by_its_day_volume() {
        // 没有小时数据的资产（历史视角、或者刚清理过明细）也得进表，
        // 小时成交都是 0 时按日成交量（锚计价）排。
        let days = vec![
            day_stat(0, "chaos-orb", 100, 200),
            day_stat(0, "divine-orb", 10, 1000),
        ];
        let pulse = exchange_pulse(&days, &[], &asset("exalted-orb"), 7, &thresholds(), None);
        assert_eq!(pulse.assets.len(), 2);
        assert_eq!(pulse.assets[0].asset_id, asset("divine-orb"));
        assert_eq!(pulse.assets[1].asset_id, asset("chaos-orb"));
        assert_eq!(pulse.assets[1].anchor_volume_by_day, vec![(day(0), 200)]);
    }
}
