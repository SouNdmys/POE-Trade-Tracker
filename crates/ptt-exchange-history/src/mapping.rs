//! GGG Metadata 路径 → catalog 资产 id 的映射表，每个游戏一份。
//!
//! POE2（2026-08-31 spike，见 docs/CORE-TRADING-MODEL.md P11）：PathOfBuilding-PoE2 的
//! CurrencyNames 与 PoEformance2 的 bases 字典双源命名（两源零冲突，互为交叉验证），
//! 按英文名对进 catalog 主名与别名。13 个 catalog 还没有的新资产被诚实排除。
//!
//! POE1（2026-09-02，P11 追记）：RePoE 的 base_items（原版 + 维护中的 fork，fork 覆盖）
//! 给出路径 → 英文名，按英文名对进 POE1 catalog 的主名与别名；联赛 Allflame 十七个
//! 小时（后来扩到 168 小时的并集）里出现过的 1046 条路径全部对上，只有一张神谕卡（Prometheus' Armoury 对
//! catalog 的 "Prometheus"）是手工钉的。
//!
//! 运行时只认这份 JSON，生成脚本不进仓库：启发式只做生成器，不做运行时逻辑。
//! 映射错了最多显示错名字，不碰 OCR 识别安全线，所以这里不需要 SHA pin，
//! 普通单测锁住"路径唯一、资产存在、锚三件套正确"就够——两个游戏各锁一遍。

use std::collections::BTreeMap;

use ptt_core::Game;
use serde::Deserialize;

/// 锚三件套的 GGG 路径。名实反直觉（AddModToRare = 崇高石），
/// 这三条的正确性由两个独立命名源 + 对账交叉验证背书，别凭肉眼相信路径名。
/// 两个游戏用的是同一串路径；差别只在 catalog id 的拼法（POE2 下划线，POE1 连字符）。
pub const EXALTED_PATH: &str = "Metadata/Items/Currency/CurrencyAddModToRare";
pub const DIVINE_PATH: &str = "Metadata/Items/Currency/CurrencyModValues";
pub const CHAOS_PATH: &str = "Metadata/Items/Currency/CurrencyRerollRare";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingEntry {
    /// GGG 原始 Metadata 路径，存储层的键。
    pub path: String,
    /// catalog 的资产 id，照那个游戏的 catalog 拼法（POE2 下划线，POE1 连字符）。
    /// 到 domain 层要经 `domain_asset_id()` 归一成连字符，别在这里就转——转换通道只留一条。
    pub asset_id: String,
    /// 英文名，只为让人能读懂这份文件，程序不用它。
    pub note: String,
}

const POE2_MAPPING_JSON: &str = include_str!("../data/poe2/ggg_paths.json");
const POE1_MAPPING_JSON: &str = include_str!("../data/poe1/ggg_paths.json");
const POE2_CATEGORIES_JSON: &str = include_str!("../data/poe2/categories.json");
const POE1_CATEGORIES_JSON: &str = include_str!("../data/poe1/categories.json");

const fn mapping_json(game: Game) -> &'static str {
    match game {
        Game::Poe1 => POE1_MAPPING_JSON,
        Game::Poe2 => POE2_MAPPING_JSON,
    }
}

const fn categories_json(game: Game) -> &'static str {
    match game {
        Game::Poe1 => POE1_CATEGORIES_JSON,
        Game::Poe2 => POE2_CATEGORIES_JSON,
    }
}

pub fn entries(game: Game) -> Result<Vec<MappingEntry>, serde_json::Error> {
    serde_json::from_str(mapping_json(game))
}

/// path → asset_id 索引。文件几十 KB、解析不到一毫秒，调用方想缓存自己缓存，
/// 不值得为它上 OnceLock。
pub fn index(game: Game) -> Result<BTreeMap<String, String>, serde_json::Error> {
    Ok(entries(game)?
        .into_iter()
        .map(|entry| (entry.path, entry.asset_id))
        .collect())
}

/// catalog 资产 id → 游戏内交易所分类。
///
/// 来源是 catalog 旁的 transcription-order.json（那份文件不进二进制：
/// catalog 只回答"这个名字是哪个资产"，分类是这里的事）。生成命令：
/// 读 order 文件、只留 id 与 in_game_category、写成本文件，和 ggg_paths.json
/// 同一批。POE2 的标签是繁中原文（14 类），POE1 的 order 文件本来就是英文 slug（16 类）。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryEntry {
    pub id: String,
    pub category: String,
}

/// asset_id → 分类 slug（`category_slug` 的英文短名）。赛季节奏按类别聚合、
/// 导出给 AI 时都用 slug：跨语言稳定，且不需要读者认得繁中标签。
pub fn categories(game: Game) -> Result<BTreeMap<String, &'static str>, serde_json::Error> {
    let entries: Vec<CategoryEntry> = serde_json::from_str(categories_json(game))?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let slug = category_slug(game, &entry.category);
            (entry.id, slug)
        })
        .collect())
}

/// 游戏内分类标签 → 英文 slug。认不出的标签归 "other"，单测锁住"没有一条
/// 落进 other"——新分类出现时是这里先响，不是导出文件里悄悄多一列问号。
/// POE1 的标签已经是 slug，这里只是把它钉成 `'static`，顺便挡住拼错的。
pub fn category_slug(game: Game, label: &str) -> &'static str {
    match game {
        Game::Poe2 => match label {
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
        },
        Game::Poe1 => match label {
            "allflame" => "allflame",
            "catalysts" => "catalysts",
            "currency" => "currency",
            "delirium" => "delirium",
            "delve" => "delve",
            "divination-cards" => "divination-cards",
            "essences" => "essences",
            "expedition" => "expedition",
            "fragments" => "fragments",
            "harvest" => "harvest",
            "legion" => "legion",
            "oils" => "oils",
            "omens" => "omens",
            "runegrafts" => "runegrafts",
            "scarabs" => "scarabs",
            "tattoos" => "tattoos",
            _ => "other",
        },
    }
}

#[cfg(test)]
mod category_tests {
    use super::*;

    fn games() -> [(Game, &'static ptt_catalog::Catalog, &'static str); 2] {
        [
            (Game::Poe2, ptt_catalog::poe2(), "exalted_orb"),
            (Game::Poe1, ptt_catalog::poe1(), "exalted-orb"),
        ]
    }

    #[test]
    fn every_categorised_asset_exists_and_every_label_is_known() {
        for (game, catalog, exalted) in games() {
            let entries: Vec<CategoryEntry> =
                serde_json::from_str(categories_json(game)).expect("categories json parses");
            assert_eq!(
                entries.len(),
                catalog.len(),
                "{game:?}: one category per catalog asset"
            );
            // 数量相等挡不住"一个重复 + 一个缺失":重复的在 BTreeMap 里静默合并,
            // 缺的那个资产就悄悄导出成 other。
            let unique: std::collections::BTreeSet<&str> =
                entries.iter().map(|entry| entry.id.as_str()).collect();
            assert_eq!(
                unique.len(),
                entries.len(),
                "{game:?}: duplicate ids in categories.json"
            );
            for entry in &entries {
                assert!(
                    catalog.by_id(&entry.id).is_some(),
                    "{game:?}: {} is not in the catalog",
                    entry.id
                );
                assert_ne!(
                    category_slug(game, &entry.category),
                    "other",
                    "{game:?}: {} has an unknown category label {:?}",
                    entry.id,
                    entry.category
                );
            }
            let categories = categories(game).expect("categories");
            assert_eq!(categories.get(exalted).copied(), Some("currency"));
        }
    }

    #[test]
    fn every_mapped_path_has_a_category() {
        // 映射表与分类表同源同批重生成；一边有一边没有 = 忘了重跑其中一份。
        for (game, _, _) in games() {
            let categories = categories(game).expect("categories");
            for entry in entries(game).expect("mapping json parses") {
                assert!(
                    categories.contains_key(&entry.asset_id),
                    "{game:?}: {} maps to {} which has no category",
                    entry.path,
                    entry.asset_id
                );
            }
        }
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    fn games() -> [(Game, &'static ptt_catalog::Catalog, usize); 2] {
        [
            (Game::Poe2, ptt_catalog::poe2(), 600),
            (Game::Poe1, ptt_catalog::poe1(), 750),
        ]
    }

    #[test]
    fn table_parses_and_is_not_a_stub() {
        for (game, _, floor) in games() {
            let entries = entries(game).expect("mapping json parses");
            assert!(
                entries.len() >= floor,
                "{game:?}: only {} entries",
                entries.len()
            );
        }
    }

    #[test]
    fn paths_are_unique() {
        for (game, _, _) in games() {
            let entries = entries(game).expect("mapping json parses");
            let mut seen = std::collections::BTreeSet::new();
            for entry in &entries {
                assert!(
                    seen.insert(&entry.path),
                    "{game:?}: duplicate path {}",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn every_asset_exists_in_the_catalog() {
        for (game, catalog, _) in games() {
            for entry in entries(game).expect("mapping json parses") {
                assert!(
                    catalog.by_id(&entry.asset_id).is_some(),
                    "{game:?}: {} maps to unknown catalog id {}",
                    entry.path,
                    entry.asset_id,
                );
            }
        }
    }

    #[test]
    fn anchor_trio_is_pinned() {
        // 整页估值都建立在这三条上；它们错，一切全错还不报错。
        // 同一串路径，两种拼法：POE2 下划线，POE1 连字符（照各自 catalog）。
        for (game, exalted, divine, chaos) in [
            (Game::Poe2, "exalted_orb", "divine_orb", "chaos_orb"),
            (Game::Poe1, "exalted-orb", "divine-orb", "chaos-orb"),
        ] {
            let index = index(game).expect("mapping json parses");
            assert_eq!(
                index.get(EXALTED_PATH).map(String::as_str),
                Some(exalted),
                "{game:?}"
            );
            assert_eq!(
                index.get(DIVINE_PATH).map(String::as_str),
                Some(divine),
                "{game:?}"
            );
            assert_eq!(
                index.get(CHAOS_PATH).map(String::as_str),
                Some(chaos),
                "{game:?}"
            );
        }
    }
}
