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

/// catalog 资产 id → 游戏内交易所分类（繁中原文，14 类）。
///
/// 来源是 catalog 旁的 transcription-order.json（那份文件不进二进制：
/// catalog 只回答"这个名字是哪个资产"，分类是这里的事）。生成命令：
/// 读 order 文件、只留 id 与 in_game_category、写成本文件——0.5.5 合入
/// 新通货后重跑一遍，和 ggg_paths.json 同一批。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryEntry {
    pub id: String,
    pub category: String,
}

const POE2_CATEGORIES_JSON: &str = include_str!("../data/poe2/categories.json");

/// asset_id → 分类 slug（`category_slug` 的英文短名）。赛季节奏按类别聚合、
/// 导出给 AI 时都用 slug：跨语言稳定，且不需要读者认得繁中标签。
pub fn poe2_categories() -> Result<BTreeMap<String, &'static str>, serde_json::Error> {
    let entries: Vec<CategoryEntry> = serde_json::from_str(POE2_CATEGORIES_JSON)?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let slug = category_slug(&entry.category);
            (entry.id, slug)
        })
        .collect())
}

/// 游戏内分类标签 → 英文 slug。认不出的标签归 "other"，单测锁住"没有一条
/// 落进 other"——新分类出现时是这里先响，不是导出文件里悄悄多一列问号。
pub fn category_slug(label: &str) -> &'static str {
    match label {
        "通貨" => "currency",
        "碎片" => "fragment",
        "精髓" => "essence",
        "符文" => "rune",
        "靈魂核心" => "soul_core",
        "寶石" => "gem",
        "未切割的寶石" => "uncut_gem",
        "魔偶" => "idol",
        "祭祀" => "ritual",
        "死境探險" => "expedition",
        "譫妄異域" => "delirium",
        "裂痕聯盟" => "breach",
        "深淵" => "abyss",
        "阿茲里的神廟" => "temple",
        _ => "other",
    }
}

#[cfg(test)]
mod category_tests {
    use super::*;

    #[test]
    fn every_categorised_asset_exists_and_every_label_is_known() {
        let catalog = ptt_catalog::poe2();
        let entries: Vec<CategoryEntry> =
            serde_json::from_str(POE2_CATEGORIES_JSON).expect("categories json parses");
        assert_eq!(
            entries.len(),
            catalog.len(),
            "one category per catalog asset"
        );
        for entry in &entries {
            assert!(
                catalog.by_id(&entry.id).is_some(),
                "{} is not in the catalog",
                entry.id
            );
            assert_ne!(
                category_slug(&entry.category),
                "other",
                "{} has an unknown category label {:?}",
                entry.id,
                entry.category
            );
        }
        let categories = poe2_categories().expect("categories");
        assert_eq!(categories.get("exalted_orb").copied(), Some("currency"));
    }

    #[test]
    fn every_mapped_path_has_a_category() {
        // 映射表与分类表同源同批重生成；一边有一边没有 = 忘了重跑其中一份。
        let categories = poe2_categories().expect("categories");
        for entry in poe2_entries().expect("mapping json parses") {
            assert!(
                categories.contains_key(&entry.asset_id),
                "{} maps to {} which has no category",
                entry.path,
                entry.asset_id
            );
        }
    }
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
