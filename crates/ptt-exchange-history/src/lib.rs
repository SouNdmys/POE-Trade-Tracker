//! GGG 官方通货交易所（浮士德）历史 API 的响应解析。
//!
//! 端点：`https://web.poecdn.com/api/currency-exchange/<realm>/<整点 unix 秒>`，
//! 公开无鉴权，只有历史——当前小时返回空 `markets`，上一个整点才有数据。
//! 本模块只做纯解析与规整，不碰网络：release 档是 `panic = "abort"`，
//! 所以网络输入必须全部落进 `Result`，而纯函数才测得动。

pub mod fetch;

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

/// 一个整点小时的完整响应。
#[derive(Clone, Debug)]
pub struct HourSnapshot {
    /// 下一个整点的 unix 秒。正常情况恒等于请求的 ts + 3600；
    /// 不等说明 GGG 跳了小时，抓取层要把它当异常信号记录而不是照单全信。
    pub next_change_id: u64,
    pub markets: Vec<MarketRow>,
}

impl HourSnapshot {
    /// 按联赛过滤。响应里混着所有联赛（标准、专家、私人联赛全在），
    /// 我们通常只关心其中一个，入库前先砍掉五到十倍的量。
    pub fn rows_for_league<'a>(&'a self, league: &str) -> impl Iterator<Item = &'a MarketRow> {
        let league = league.to_owned();
        self.markets.iter().filter(move |row| row.league == league)
    }
}

/// 规整后的一条市场行：两个资产按字典序排成 `asset_a < asset_b`，
/// 对齐 `MarketPairKey` 的无向对惯例——API 里 `market_pair` 的顺序不稳定
/// （实测神圣在前崇高在后），不规整的话同一交易对会在不同小时落成两个身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketRow {
    pub league: String,
    /// GGG 原始 Metadata 路径。刻意不在解析层映射成 catalog id：
    /// 映射表会迭代，存原始路径才能让映射每改一版都零成本追溯生效。
    pub asset_a: String,
    pub asset_b: String,
    /// 该小时内两侧各自的成交单位数。两个整数的比值就是精确的 VWAP，
    /// 这是唯一参与计算的汇率来源（绝不浮点的仓规靠它成立）。
    pub volume_a: u64,
    pub volume_b: u64,
    pub lowest_stock_a: u64,
    pub lowest_stock_b: u64,
    pub highest_stock_a: u64,
    pub highest_stock_b: u64,
    /// 汇率区间以原文数字存证（API 给的是快照比值对，可能整数可能小数），
    /// 只做展示与对账，不参与计算。
    pub lowest_ratio_a: String,
    pub lowest_ratio_b: String,
    pub highest_ratio_a: String,
    pub highest_ratio_b: String,
}

#[derive(Debug, Error)]
pub enum ExchangeHistoryError {
    #[error("response is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("market_pair must have exactly two assets, got {actual} (market_id={market_id})")]
    PairLength { market_id: String, actual: usize },
    #[error("asset paired with itself (market_id={market_id})")]
    SamePair { market_id: String },
    #[error("{side} is missing key {path} (market_id={market_id})")]
    MissingSide {
        market_id: String,
        side: &'static str,
        path: String,
    },
    #[error("{side} value for {path} is not a non-negative integer (market_id={market_id})")]
    NonIntegerCount {
        market_id: String,
        side: &'static str,
        path: String,
    },
}

/// 原始响应的信封。不 `deny_unknown_fields`：这是别人的 API，
/// GGG 加新字段不该弄崩我们的解析——严格只留给我们自己 pin 过的数据。
#[derive(Deserialize)]
struct RawHour {
    next_change_id: u64,
    #[serde(default)]
    markets: Vec<RawMarket>,
}

#[derive(Deserialize)]
struct RawMarket {
    league: String,
    market_id: String,
    market_pair: Vec<String>,
    volume_traded: BTreeMap<String, serde_json::Number>,
    lowest_stock: BTreeMap<String, serde_json::Number>,
    highest_stock: BTreeMap<String, serde_json::Number>,
    lowest_ratio: BTreeMap<String, serde_json::Number>,
    highest_ratio: BTreeMap<String, serde_json::Number>,
}

/// 解析一个小时的原始字节。成交量/库存必须是非负整数——
/// 出现小数说明我们对 API 的理解错了，宁可当场报错也不静默截断。
pub fn parse_hour(bytes: &[u8]) -> Result<HourSnapshot, ExchangeHistoryError> {
    let raw: RawHour = serde_json::from_slice(bytes)?;
    let mut markets = Vec::with_capacity(raw.markets.len());
    for market in raw.markets {
        markets.push(normalize_market(market)?);
    }
    Ok(HourSnapshot {
        next_change_id: raw.next_change_id,
        markets,
    })
}

fn normalize_market(raw: RawMarket) -> Result<MarketRow, ExchangeHistoryError> {
    if raw.market_pair.len() != 2 {
        return Err(ExchangeHistoryError::PairLength {
            market_id: raw.market_id,
            actual: raw.market_pair.len(),
        });
    }
    if raw.market_pair[0] == raw.market_pair[1] {
        return Err(ExchangeHistoryError::SamePair {
            market_id: raw.market_id,
        });
    }
    let (asset_a, asset_b) = if raw.market_pair[0] < raw.market_pair[1] {
        (raw.market_pair[0].clone(), raw.market_pair[1].clone())
    } else {
        (raw.market_pair[1].clone(), raw.market_pair[0].clone())
    };

    let count = |dict: &BTreeMap<String, serde_json::Number>,
                 side: &'static str,
                 path: &str|
     -> Result<u64, ExchangeHistoryError> {
        let number = dict
            .get(path)
            .ok_or_else(|| ExchangeHistoryError::MissingSide {
                market_id: raw.market_id.clone(),
                side,
                path: path.to_owned(),
            })?;
        number
            .as_u64()
            .ok_or_else(|| ExchangeHistoryError::NonIntegerCount {
                market_id: raw.market_id.clone(),
                side,
                path: path.to_owned(),
            })
    };
    let ratio = |dict: &BTreeMap<String, serde_json::Number>,
                 side: &'static str,
                 path: &str|
     -> Result<String, ExchangeHistoryError> {
        dict.get(path)
            .map(|number| number.to_string())
            .ok_or_else(|| ExchangeHistoryError::MissingSide {
                market_id: raw.market_id.clone(),
                side,
                path: path.to_owned(),
            })
    };

    Ok(MarketRow {
        volume_a: count(&raw.volume_traded, "volume_traded", &asset_a)?,
        volume_b: count(&raw.volume_traded, "volume_traded", &asset_b)?,
        lowest_stock_a: count(&raw.lowest_stock, "lowest_stock", &asset_a)?,
        lowest_stock_b: count(&raw.lowest_stock, "lowest_stock", &asset_b)?,
        highest_stock_a: count(&raw.highest_stock, "highest_stock", &asset_a)?,
        highest_stock_b: count(&raw.highest_stock, "highest_stock", &asset_b)?,
        lowest_ratio_a: ratio(&raw.lowest_ratio, "lowest_ratio", &asset_a)?,
        lowest_ratio_b: ratio(&raw.lowest_ratio, "lowest_ratio", &asset_b)?,
        highest_ratio_a: ratio(&raw.highest_ratio, "highest_ratio", &asset_a)?,
        highest_ratio_b: ratio(&raw.highest_ratio, "highest_ratio", &asset_b)?,
        league: raw.league,
        asset_a,
        asset_b,
    })
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    const DIVINE: &str = "Metadata/Items/Currency/CurrencyModValues";
    const EXALTED: &str = "Metadata/Items/Currency/CurrencyAddModToRare";

    /// 真实响应截段（2026-08-31 实拉，数字未改）：
    /// 神圣|崇高在 Runes of Aldur，外加一条 Standard 的行当联赛过滤的陪练。
    const FIXTURE: &str = r#"{
      "next_change_id": 1788163200,
      "markets": [
        {
          "league": "Runes of Aldur",
          "market_id": "Metadata/Items/Currency/CurrencyModValues|Metadata/Items/Currency/CurrencyAddModToRare",
          "market_pair": ["Metadata/Items/Currency/CurrencyModValues", "Metadata/Items/Currency/CurrencyAddModToRare"],
          "volume_traded": {"Metadata/Items/Currency/CurrencyModValues": 2416, "Metadata/Items/Currency/CurrencyAddModToRare": 1004431},
          "lowest_stock": {"Metadata/Items/Currency/CurrencyModValues": 6291, "Metadata/Items/Currency/CurrencyAddModToRare": 4758920},
          "highest_stock": {"Metadata/Items/Currency/CurrencyModValues": 6803, "Metadata/Items/Currency/CurrencyAddModToRare": 4884606},
          "lowest_ratio": {"Metadata/Items/Currency/CurrencyModValues": 1, "Metadata/Items/Currency/CurrencyAddModToRare": 434},
          "highest_ratio": {"Metadata/Items/Currency/CurrencyModValues": 1, "Metadata/Items/Currency/CurrencyAddModToRare": 373}
        },
        {
          "league": "Standard",
          "market_id": "Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss|Metadata/Items/Currency/CurrencyModValues",
          "market_pair": ["Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss", "Metadata/Items/Currency/CurrencyModValues"],
          "volume_traded": {"Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss": 5, "Metadata/Items/Currency/CurrencyModValues": 5},
          "lowest_stock": {"Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss": 469, "Metadata/Items/Currency/CurrencyModValues": 100},
          "highest_stock": {"Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss": 469, "Metadata/Items/Currency/CurrencyModValues": 100},
          "lowest_ratio": {"Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss": 1, "Metadata/Items/Currency/CurrencyModValues": 1},
          "highest_ratio": {"Metadata/Items/Currency/CurrencyCorruptedEssenceAbyss": 1, "Metadata/Items/Currency/CurrencyModValues": 1}
        }
      ]
    }"#;

    fn market_json(pair: &str) -> String {
        format!(
            r#"{{
              "next_change_id": 3600,
              "markets": [{{
                "league": "Runes of Aldur",
                "market_id": "test",
                "market_pair": {pair},
                "volume_traded": {{"{DIVINE}": 1, "{EXALTED}": 2}},
                "lowest_stock": {{"{DIVINE}": 1, "{EXALTED}": 2}},
                "highest_stock": {{"{DIVINE}": 1, "{EXALTED}": 2}},
                "lowest_ratio": {{"{DIVINE}": 1, "{EXALTED}": 2}},
                "highest_ratio": {{"{DIVINE}": 1, "{EXALTED}": 2}}
              }}]
            }}"#
        )
    }

    #[test]
    fn parses_the_real_shape() {
        let hour = parse_hour(FIXTURE.as_bytes()).expect("fixture parses");
        assert_eq!(hour.next_change_id, 1_788_163_200);
        assert_eq!(hour.markets.len(), 2);
    }

    #[test]
    fn pair_order_is_normalized_lexicographically() {
        // API 原文里神圣（ModValues）在前，字典序上崇高（AddModToRare）更小，
        // 规整后必须换位且各侧数值跟着资产走。
        let hour = parse_hour(FIXTURE.as_bytes()).expect("fixture parses");
        let row = &hour.markets[0];
        assert_eq!(row.asset_a, EXALTED);
        assert_eq!(row.asset_b, DIVINE);
        assert_eq!(row.volume_a, 1_004_431);
        assert_eq!(row.volume_b, 2_416);
        assert_eq!(row.lowest_ratio_a, "434");
        assert_eq!(row.lowest_ratio_b, "1");
        assert_eq!(row.highest_ratio_a, "373");
    }

    #[test]
    fn league_filter_keeps_only_that_league() {
        let hour = parse_hour(FIXTURE.as_bytes()).expect("fixture parses");
        let rows: Vec<_> = hour.rows_for_league("Runes of Aldur").collect();
        assert_eq!(rows.len(), 1);
        assert!(hour.rows_for_league("HC Runes of Aldur").next().is_none());
    }

    #[test]
    fn empty_hour_is_valid() {
        // 赛季前和"还没发布"的小时都长这样，解析层不该报错——
        // 空的含义交给抓取层的护栏去区分。
        let hour = parse_hour(br#"{"next_change_id": 1733518800, "markets": []}"#)
            .expect("empty hour parses");
        assert!(hour.markets.is_empty());
    }

    #[test]
    fn rejects_self_pair() {
        let json = market_json(&format!(r#"["{DIVINE}", "{DIVINE}"]"#));
        assert!(matches!(
            parse_hour(json.as_bytes()),
            Err(ExchangeHistoryError::SamePair { .. })
        ));
    }

    #[test]
    fn rejects_missing_side_in_dict() {
        let json = market_json(&format!(
            r#"["{DIVINE}", "Metadata/Items/Currency/CurrencyRerollRare"]"#
        ));
        assert!(matches!(
            parse_hour(json.as_bytes()),
            Err(ExchangeHistoryError::MissingSide { .. })
        ));
    }

    #[test]
    fn rejects_fractional_volume() {
        let json = market_json(&format!(r#"["{DIVINE}", "{EXALTED}"]"#)).replace(
            &format!(r#""volume_traded": {{"{DIVINE}": 1"#),
            &format!(r#""volume_traded": {{"{DIVINE}": 1.5"#),
        );
        assert!(matches!(
            parse_hour(json.as_bytes()),
            Err(ExchangeHistoryError::NonIntegerCount { .. })
        ));
    }

    #[test]
    fn fractional_ratio_is_preserved_as_text() {
        let json = market_json(&format!(r#"["{DIVINE}", "{EXALTED}"]"#)).replace(
            &format!(r#""lowest_ratio": {{"{DIVINE}": 1"#),
            &format!(r#""lowest_ratio": {{"{DIVINE}": 0.25"#),
        );
        let hour = parse_hour(json.as_bytes()).expect("fractional ratio parses");
        assert_eq!(hour.markets[0].lowest_ratio_b, "0.25");
    }
}
