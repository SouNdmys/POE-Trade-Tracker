//! GGG Metadata 路径 → catalog 资产 id 的映射表。
//!
//! 生成方式（2026-08-31 spike，见 docs/CORE-TRADING-MODEL.md P11）：
//! PathOfBuilding-PoE2 的 CurrencyNames 与 PoEformance2 的 bases 字典双源命名
//! （两源零冲突，互为交叉验证），按英文名对进 catalog 主名与别名。
//! 13 个 catalog 还没有的新资产被诚实排除（约 1.9% 加权成交量）。
//!
//! 运行时只认这份 JSON，生成脚本不进仓库：启发式只做生成器，不做运行时逻辑。
//! 映射错了最多显示错名字，不碰 OCR 识别安全线，所以这里不需要 SHA pin，
//! 普通单测锁住"路径唯一、资产存在、锚三件套正确"就够。

use std::collections::BTreeMap;

use serde::Deserialize;

/// 锚三件套的 GGG 路径。名实反直觉（AddModToRare = 崇高石），
/// 这三条的正确性由两个独立命名源 + 对账交叉验证背书，别凭肉眼相信路径名。
pub const EXALTED_PATH: &str = "Metadata/Items/Currency/CurrencyAddModToRare";
pub const DIVINE_PATH: &str = "Metadata/Items/Currency/CurrencyModValues";
pub const CHAOS_PATH: &str = "Metadata/Items/Currency/CurrencyRerollRare";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingEntry {
    /// GGG 原始 Metadata 路径，存储层的键。
    pub path: String,
    /// catalog 的资产 id（下划线风格）。到 domain 层要经
    /// `domain_asset_id()` 转成连字符，别在这里就转——转换通道只留一条。
    pub asset_id: String,
    /// 英文名，只为让人能读懂这份文件，程序不用它。
    pub note: String,
}

const POE2_MAPPING_JSON: &str = include_str!("../data/poe2/ggg_paths.json");

pub fn poe2_entries() -> Result<Vec<MappingEntry>, serde_json::Error> {
    serde_json::from_str(POE2_MAPPING_JSON)
}

/// path → asset_id 索引。文件几十 KB、解析不到一毫秒，调用方想缓存自己缓存，
/// 不值得为它上 OnceLock。
pub fn poe2_index() -> Result<BTreeMap<String, String>, serde_json::Error> {
    Ok(poe2_entries()?
        .into_iter()
        .map(|entry| (entry.path, entry.asset_id))
        .collect())
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn table_parses_and_is_not_a_stub() {
        let entries = poe2_entries().expect("mapping json parses");
        assert!(entries.len() >= 600, "only {} entries", entries.len());
    }

    #[test]
    fn paths_are_unique() {
        let entries = poe2_entries().expect("mapping json parses");
        let mut seen = std::collections::BTreeSet::new();
        for entry in &entries {
            assert!(seen.insert(&entry.path), "duplicate path {}", entry.path);
        }
    }

    #[test]
    fn every_asset_exists_in_the_catalog() {
        let catalog = ptt_catalog::poe2();
        for entry in poe2_entries().expect("mapping json parses") {
            assert!(
                catalog.by_id(&entry.asset_id).is_some(),
                "{} maps to unknown catalog id {}",
                entry.path,
                entry.asset_id,
            );
        }
    }

    #[test]
    fn anchor_trio_is_pinned() {
        // 整页估值都建立在这三条上；它们错，一切全错还不报错。
        let index = poe2_index().expect("mapping json parses");
        assert_eq!(
            index.get(EXALTED_PATH).map(String::as_str),
            Some("exalted_orb")
        );
        assert_eq!(
            index.get(DIVINE_PATH).map(String::as_str),
            Some("divine_orb")
        );
        assert_eq!(index.get(CHAOS_PATH).map(String::as_str), Some("chaos_orb"));
    }
}
