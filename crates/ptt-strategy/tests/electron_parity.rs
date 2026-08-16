//! Parity against the Electron build, where the Electron build was right.
//!
//! `tests/fixtures/electron-reference-vectors.json` is emitted from the
//! shipped `executionSafety.ts` and `marketPolicy.ts` — the two algorithm
//! modules with no internal imports, so they transpile and run standalone.
//! Those files ran for a season; where the audit found no defect in them,
//! this port must agree.
//!
//! Where it deliberately does not agree, the difference is asserted here
//! rather than left for someone to discover: a silent divergence and an
//! intended one look identical in a passing test suite.

use std::collections::BTreeMap;

use ptt_strategy::{
    Actionability, AnchorAction, MarketPolicy, MarketRole, recommend_liquidity_anchors,
};
use ptt_trade_domain::MarketAssetId;

mod support;
use support::{EdgeSpec, asset};

fn vectors() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/electron-reference-vectors.json"
    );
    let text = std::fs::read_to_string(path).expect("reference vectors");
    serde_json::from_str(&text).expect("reference vectors parse")
}

/// The Electron catalogue wrote ids with underscores; this one uses hyphens.
/// Parity is about behaviour, not spelling.
fn rust_id(electron_id: &str) -> String {
    electron_id.replace('_', "-")
}

fn actionability(name: &str) -> Actionability {
    match name {
        "instant_executable" => Actionability::InstantExecutable,
        "maker_theoretical" => Actionability::MakerTheoretical,
        "probe_required" => Actionability::ProbeRequired,
        "suspicious_outlier" => Actionability::SuspiciousOutlier,
        other => panic!("unknown actionability {other}"),
    }
}

#[test]
fn the_actionability_ladder_orders_exactly_as_the_shipped_build_did() {
    let vectors = vectors();
    let pairs = vectors["safer_pairs"].as_array().expect("safer_pairs");
    let mut checked = 0;
    for pair in pairs {
        let (Some(left), Some(right)) = (pair["left"].as_str(), pair["right"].as_str()) else {
            // The no-argument call, covered separately below.
            continue;
        };
        let expected = actionability(pair["result"].as_str().expect("result"));
        assert_eq!(
            actionability(left).safer(actionability(right)),
            expected,
            "safer({left}, {right})"
        );
        checked += 1;
    }
    assert_eq!(checked, 16, "every ordered pair of the four rungs");
}

#[test]
fn an_empty_ladder_falls_to_the_same_rung_the_shipped_build_used() {
    let vectors = vectors();
    let empty = vectors["safer_pairs"]
        .as_array()
        .expect("safer_pairs")
        .iter()
        .find(|pair| pair["left"].is_null())
        .expect("no-argument case");
    assert_eq!(
        Actionability::safest(std::iter::empty()),
        actionability(empty["result"].as_str().expect("result")),
    );
}

#[test]
fn role_capabilities_match_the_shipped_table() {
    let vectors = vectors();
    for entry in vectors["role_capabilities"]
        .as_array()
        .expect("role_capabilities")
    {
        let role = match entry["role"].as_str().expect("role") {
            "anchor" => MarketRole::Anchor,
            "target" => MarketRole::Target,
            "bridge" => MarketRole::Bridge,
            "watch_only" => MarketRole::WatchOnly,
            other => panic!("unknown role {other}"),
        };
        let ours = serde_json::to_value(role.capabilities()).expect("serialize");
        assert_eq!(
            ours, entry["capabilities"],
            "capabilities for {:?}",
            entry["role"]
        );
    }
}

#[test]
fn the_core_set_dedupes_the_way_the_shipped_build_did() {
    let vectors = vectors();
    for case in vectors["normalize_cases"]
        .as_array()
        .expect("normalize_cases")
    {
        let label = case["label"].as_str().expect("label");
        let expected: Vec<String> = case["result"]
            .as_array()
            .expect("result")
            .iter()
            .map(|value| rust_id(value.as_str().expect("id")))
            .collect();

        // Blank and null entries cannot be constructed at all here, so only
        // the cases that survive typing are comparable.
        let input: Option<Vec<MarketAssetId>> = case["input"].as_array().map(|values| {
            values
                .iter()
                .filter_map(|value| MarketAssetId::try_new(rust_id(value.as_str()?)).ok())
                .collect()
        });
        let Some(input) = input else { continue };

        let mut policy = MarketPolicy::default_for("standard");
        if policy.set_core_liquidity(input.clone()) {
            let ours: Vec<String> = policy
                .core_liquidity
                .iter()
                .map(|asset| asset.as_str().to_owned())
                .collect();
            assert_eq!(ours, expected, "{label}");
        } else {
            // Intended divergence: the shipped build answered an empty list
            // with the shipped default, so a user who cleared the set got it
            // silently restored. Here the change is refused and the caller
            // is told, which is the only way the user learns nothing
            // happened.
            assert!(input.is_empty(), "{label}: only an empty set is refused");
            assert_eq!(
                policy.core_liquidity.len(),
                3,
                "{label}: the previous set stands"
            );
        }
    }
}

#[test]
fn anchor_recommendations_agree_on_evidence_if_not_on_the_score() {
    let vectors = vectors();
    let mut edges = Vec::new();
    for peer in ["divine-orb", "chaos-orb", "exalted-orb"] {
        for (from, to) in [("annulment-orb", peer), (peer, "annulment-orb")] {
            edges.push(
                EdgeSpec::new("edge", 100, 0)
                    .between(from, to)
                    .stock(500)
                    .build(),
            );
        }
    }
    edges.push(
        EdgeSpec::new("regal", 100, 1)
            .between("regal-orb", "divine-orb")
            .stock(10)
            .build(),
    );

    let policy = MarketPolicy::default_for("rise-of-the-abyssal");
    let ours: BTreeMap<String, _> = recommend_liquidity_anchors(&edges, &policy)
        .into_iter()
        .map(|item| (item.asset_id.as_str().to_owned(), item))
        .collect();

    let expected = vectors["anchor_recommendations"]
        .as_array()
        .expect("anchor_recommendations");
    assert_eq!(
        ours.len(),
        expected.len(),
        "same currencies survive the thresholds"
    );

    for entry in expected {
        let id = rust_id(entry["currency_id"].as_str().expect("id"));
        let item = ours.get(&id).unwrap_or_else(|| panic!("{id} recommended"));
        let action = match entry["action"].as_str().expect("action") {
            "recommend_core" => AnchorAction::PromoteToCore,
            "watch" => AnchorAction::Watch,
            other => panic!("unexpected action {other}"),
        };
        assert_eq!(item.action, action, "{id} action");
        assert_eq!(
            u64::from(item.recent_quote_count),
            entry["recent_quote_count"].as_u64().expect("recent"),
            "{id} recent quotes"
        );
        assert_eq!(
            u64::from(item.fresh_quote_count),
            entry["fresh_quote_count"].as_u64().expect("fresh"),
            "{id} fresh quotes"
        );
        assert_eq!(
            u64::from(item.pair_coverage_count),
            entry["pair_coverage_count"].as_u64().expect("pairs"),
            "{id} pair coverage"
        );
        assert_eq!(
            u64::from(item.bidirectional_pair_count),
            entry["bidirectional_pair_count"].as_u64().expect("bidi"),
            "{id} two-way coverage"
        );
        assert_eq!(
            item.available_stock_depth,
            entry["available_stock_depth"].as_u64().expect("depth"),
            "{id} available depth"
        );
        // The score itself is not compared: the shipped build ran the depth
        // term through Math.log10 and then sorted on the result, so its
        // ranking depended on floating point. Ours is an integer in tenths.
        assert!(item.score_tenths > 0, "{id} scored");
    }

    assert!(
        !ours.contains_key(asset("regal-orb").as_str()),
        "a currency seen once is not a liquidity anchor"
    );
}
