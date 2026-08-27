//! 赛季录入的维护探针:校验目录数据文件、重算 SHA-256、回写 lib.rs 的钉子。
//!
//! 以前录一批新通货要手改 6 处、手算哈希;这个工具把机械的部分收成一条命令,
//! 校验的部分先于回写——数据不干净就不动任何东西。
//!
//! Usage:
//!   cargo run -p ptt-catalog --bin catalog_repin            # 只检查
//!   cargo run -p ptt-catalog --bin catalog_repin -- --write # 检查并回写钉子
//!
//! 检查项:JSON 可解析(拼错字段名直接报错)、id 不重复且符合各自拼写规范
//! (POE1 连字符、POE2 下划线)、双语齐全、transcription-order 与目录同步。
//! 回写项:`POE*_CATALOG_SHA256` 与 `POE*_CATALOG_ENTRIES`。
//! 不回写:`live.rs` 里带条数的 catalog id——动它会轮换 context key,是单独
//! 的决定;条数变了这里只提醒。

use sha2::{Digest, Sha256};

struct GameSpec {
    name: &'static str,
    data_path: &'static str,
    order_path: &'static str,
    sha_const: &'static str,
    entries_const: &'static str,
    separator: char,
}

const SPECS: [GameSpec; 2] = [
    GameSpec {
        name: "poe1",
        data_path: "data/poe1/market_assets.en.json",
        order_path: ptt_catalog::POE1_TRANSCRIPTION_ORDER_PATH,
        sha_const: "POE1_CATALOG_SHA256",
        entries_const: "POE1_CATALOG_ENTRIES",
        separator: '-',
    },
    GameSpec {
        name: "poe2",
        data_path: "data/poe2/currency_master.zh_tw.json",
        order_path: ptt_catalog::POE2_TRANSCRIPTION_ORDER_PATH,
        sha_const: "POE2_CATALOG_SHA256",
        entries_const: "POE2_CATALOG_ENTRIES",
        separator: '_',
    },
];

fn main() -> Result<(), String> {
    let write = std::env::args().any(|argument| argument == "--write");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib_path = root.join("src/lib.rs");
    let mut lib = std::fs::read_to_string(&lib_path)
        .map_err(|error| format!("{}: {error}", lib_path.display()))?;
    let mut changed = false;

    for spec in SPECS {
        let data_path = root.join(spec.data_path);
        let bytes = std::fs::read(&data_path)
            .map_err(|error| format!("{}: {error}", data_path.display()))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|error| format!("{}: not UTF-8: {error}", spec.data_path))?;
        let assets: Vec<ptt_catalog::CatalogAsset> =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", spec.data_path))?;
        validate(&spec, &assets)?;
        check_order_file(root, &spec, &assets)?;

        let sha = hex(&Sha256::digest(&bytes));
        let entries = assets.len();
        println!("{}: {} entries, sha256 {}", spec.name, entries, sha);
        changed |= replace_quoted(&mut lib, spec.sha_const, &sha)?;
        let entries_changed = replace_number(&mut lib, spec.entries_const, entries)?;
        changed |= entries_changed;
        if entries_changed {
            println!(
                "  NOTE: {} 变了。`ptt-runtime/src/live.rs` 的 catalog id \
                 (\"{}-catalog-N\") 带条数;要不要跟着改是单独的决定——改了会\
                 轮换 context key、切断历史,见 docs/P10-FOLLOWUPS.md。",
                spec.entries_const, spec.name,
            );
        }
    }

    if changed {
        if write {
            std::fs::write(&lib_path, lib)
                .map_err(|error| format!("{}: {error}", lib_path.display()))?;
            println!("pins rewritten - run `cargo test -p ptt-catalog` to confirm");
        } else {
            return Err("pins are stale - rerun with --write to update src/lib.rs".to_owned());
        }
    } else {
        println!("pins already match");
    }
    Ok(())
}

/// The checks a hand-edited data file most needs: everything here has either
/// happened or nearly happened during a transcription.
fn validate(spec: &GameSpec, assets: &[ptt_catalog::CatalogAsset]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for asset in assets {
        if !seen.insert(asset.id.as_str()) {
            return Err(format!("{}: duplicate id {}", spec.name, asset.id));
        }
        let spelling_ok = !asset.id.is_empty()
            && asset
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == spec.separator);
        if !spelling_ok {
            return Err(format!(
                "{}: id {:?} breaks the {} spelling convention (lowercase, digits, {:?})",
                spec.name, asset.id, spec.name, spec.separator,
            ));
        }
        if asset.name_en.trim().is_empty() || asset.name_zh_tw.trim().is_empty() {
            return Err(format!(
                "{}: {} is missing a language ({:?} / {:?})",
                spec.name, asset.id, asset.name_en, asset.name_zh_tw,
            ));
        }
    }
    Ok(())
}

/// The order file binds the next transcription; it must describe this one.
fn check_order_file(
    root: &std::path::Path,
    spec: &GameSpec,
    assets: &[ptt_catalog::CatalogAsset],
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct OrderEntry {
        id: String,
    }
    let path = root.join(spec.order_path);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "{}: {error} - 每条新通货都要同步加进 transcription-order",
            path.display()
        )
    })?;
    let entries: Vec<OrderEntry> =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", spec.order_path))?;
    if entries.len() != assets.len() {
        return Err(format!(
            "{}: order file describes {} assets, the catalogue has {} - 新条目没同步进去",
            spec.order_path,
            entries.len(),
            assets.len(),
        ));
    }
    let ids: std::collections::BTreeSet<&str> =
        assets.iter().map(|asset| asset.id.as_str()).collect();
    for entry in &entries {
        if !ids.contains(entry.id.as_str()) {
            return Err(format!(
                "{}: names {}, which is not in the catalogue",
                spec.order_path, entry.id
            ));
        }
    }
    Ok(())
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Rewrites the string literal after `pub const {name}`; true when it changed.
fn replace_quoted(lib: &mut String, name: &str, value: &str) -> Result<bool, String> {
    let marker = format!("pub const {name}");
    let at = lib
        .find(&marker)
        .ok_or_else(|| format!("{name} not found in lib.rs"))?;
    let open = lib[at..]
        .find('"')
        .ok_or_else(|| format!("{name}: no string literal"))?
        + at
        + 1;
    let close = lib[open..]
        .find('"')
        .ok_or_else(|| format!("{name}: unterminated literal"))?
        + open;
    if &lib[open..close] == value {
        return Ok(false);
    }
    lib.replace_range(open..close, value);
    Ok(true)
}

/// Rewrites the number after `pub const {name} ... = `; true when it changed.
fn replace_number(lib: &mut String, name: &str, value: usize) -> Result<bool, String> {
    let marker = format!("pub const {name}");
    let at = lib
        .find(&marker)
        .ok_or_else(|| format!("{name} not found in lib.rs"))?;
    let equals = lib[at..]
        .find('=')
        .ok_or_else(|| format!("{name}: no assignment"))?
        + at
        + 1;
    let semicolon = lib[equals..]
        .find(';')
        .ok_or_else(|| format!("{name}: no terminator"))?
        + equals;
    let old: usize = lib[equals..semicolon]
        .trim()
        .replace('_', "")
        .parse()
        .map_err(|error| format!("{name}: {error}"))?;
    if old == value {
        return Ok(false);
    }
    lib.replace_range(equals..semicolon, &format!(" {value}"));
    Ok(true)
}
