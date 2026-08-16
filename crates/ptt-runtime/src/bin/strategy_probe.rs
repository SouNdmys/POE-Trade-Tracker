//! Runs the frozen screenshot corpus all the way through the strategy layer
//! and checks the invariants that must hold on real data.
//!
//! `book-probe --manifest` proves recognition still reads the corpus the same
//! way. This goes further: every accepted book becomes a domain capture, lands
//! in an in-memory store, and is then put through route accounting, maker
//! advice and price history — the parts a synthetic fixture cannot exercise,
//! because a fixture only contains what someone already thought to put in it.
//!
//! Usage: `strategy-probe [--manifest PATH]`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use ptt_market_book::{
        DataVisibility, QuoteSelectionPolicy, QuoteSelectionStrategy, build_coherent_current_book,
        select_quote_edges,
    };
    use ptt_recognition::profiles::poe2_zhtw::Route;
    use ptt_runtime::live::{capture_from_book, domain_asset_id, poe2_live_context};
    use ptt_storage::MarketStore;
    use ptt_strategy::{
        Actionability, ExecutionRisk, MakerMode, MakerRequest, RiskThresholds,
        calculate_maker_strategy,
    };
    use ptt_trade_engine::AssetAmount;

    #[derive(serde::Deserialize)]
    struct Manifest {
        screenshot_dir: String,
        cases: Vec<ManifestCase>,
    }

    #[derive(serde::Deserialize)]
    struct ManifestCase {
        file: String,
        expect: String,
    }

    let mut manifest_path = "tests/manifests/poe2-zhtw-corpus.json".to_owned();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--manifest" if index + 1 < arguments.len() => {
                manifest_path = arguments[index + 1].clone();
                index += 2;
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }

    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    let route = Route::new().map_err(|reason| format!("route init failed: {reason:?}"))?;
    let mut store = MarketStore::open_in_memory()?;
    let context = poe2_live_context("corpus-league")?;
    let context_key = context.stable_key();

    // Corpus screenshots were all taken within a few minutes of each other,
    // so stamp them at the run time: the point of this probe is the strategy
    // arithmetic, not the freshness gate, which has its own tests.
    let captured_at = Utc::now();
    let mut sequence = 0_u64;
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    let mut failures = Vec::new();

    for case in &manifest.cases {
        if case.expect != "accept" {
            continue;
        }
        let path = std::path::Path::new(&manifest.screenshot_dir).join(&case.file);
        let Ok(book) = route.recognize_screenshot(&path) else {
            failures.push(format!("{}: recognition regressed", case.file));
            continue;
        };
        sequence += 1;
        let hash = format!("{sequence:064x}");
        let capture =
            match capture_from_book(&book, &context, captured_at, [hash.clone(), hash], sequence) {
                Ok(capture) => capture,
                Err(error) => {
                    failures.push(format!("{}: capture mapping failed: {error:?}", case.file));
                    continue;
                }
            };
        pairs.insert((
            capture.have_asset_id.as_str().to_owned(),
            capture.need_asset_id.as_str().to_owned(),
        ));
        if let Err(error) = store.persist_capture(&capture) {
            failures.push(format!("{}: persist failed: {error}", case.file));
        }
    }

    let observations = store.load_observations(&context_key, None)?;
    println!(
        "corpus: {sequence} books, {} edges, {} pairs",
        observations.len(),
        pairs.len()
    );

    let book = build_coherent_current_book(&context_key, &observations, DataVisibility::default())?;
    let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)?;
    let selection = select_quote_edges(&book, &policy, Utc::now())?;
    let candidates: Vec<_> = selection
        .selections
        .iter()
        .flat_map(|entry| entry.candidate_edges.iter().cloned())
        .collect();

    for (have, need) in &pairs {
        let have_id = domain_asset_id(have)?;
        let need_id = domain_asset_id(need)?;

        // --- route accounting over the real book
        match ptt_runtime::reports::convert_report(&observations, &context_key, &have_id, &need_id)
        {
            Ok(lines) => {
                if lines.is_empty() {
                    failures.push(format!("{have} -> {need}: convert report was silent"));
                }
            }
            Err(reason) => failures.push(format!("{have} -> {need}: convert failed: {reason}")),
        }

        // --- history over the real book
        if let Err(reason) =
            ptt_runtime::reports::history_report(&observations, &context_key, &have_id, &need_id)
        {
            failures.push(format!("{have} -> {need}: history failed: {reason}"));
        }

        // --- maker advice, and the invariants it must never break
        let competing: Vec<_> = candidates
            .iter()
            .filter(|edge| {
                edge.observation.edge.from_asset_id == have_id
                    && edge.observation.edge.to_asset_id == need_id
            })
            .cloned()
            .collect();
        if competing.is_empty() {
            continue;
        }
        let amount_in = AssetAmount {
            asset_id: have_id.clone(),
            quanta: 100,
            unit: ptt_trade_engine::AssetUnit::whole(),
        };
        let strategy = calculate_maker_strategy(MakerRequest {
            from_asset_id: &have_id,
            to_asset_id: &need_id,
            amount_in: &amount_in,
            competing: &competing,
            instant: None,
            match_front: false,
            thresholds: RiskThresholds::default(),
        })?;

        // The queue is the product's single definition of "front"; if it ever
        // comes back unsorted, every sizing number downstream is wrong.
        for pair in strategy.queue.windows(2) {
            if pair[0].rate.compare_value(&pair[1].rate) == std::cmp::Ordering::Greater {
                failures.push(format!("{have} -> {need}: maker queue is not front-first"));
                break;
            }
        }
        // Depth ahead of you can only grow as you move back through the queue.
        let mut previous = 0_u64;
        for level in &strategy.queue {
            let Some(ahead) = &level.depth_ahead_from else {
                continue;
            };
            if ahead.quanta < previous {
                failures.push(format!("{have} -> {need}: depth ahead went backwards"));
                break;
            }
            previous = ahead.quanta;
        }
        for recommendation in &strategy.recommendations {
            // Listing is never something you can execute on your own.
            if recommendation.assessment.actionability == Actionability::InstantExecutable {
                failures.push(format!(
                    "{have} -> {need}: {:?} listing claimed instant execution",
                    recommendation.mode
                ));
            }
            if !recommendation
                .assessment
                .contains(ExecutionRisk::MakerReference)
            {
                failures.push(format!(
                    "{have} -> {need}: {:?} listing dropped its maker risk",
                    recommendation.mode
                ));
            }
            if recommendation.mode == MakerMode::Opportunity
                && let Some(front) = strategy.queue.first()
                && recommendation.rate.compare_value(&front.rate) != std::cmp::Ordering::Less
            {
                failures.push(format!(
                    "{have} -> {need}: opportunity listing did not undercut the front"
                ));
            }
        }
        println!(
            "  {have} -> {need}: {} listings, front depth {}, {} recommendations",
            strategy.queue.len(),
            strategy
                .front_depth_from
                .as_ref()
                .map_or_else(|| "—".to_owned(), |depth| depth.quanta.to_string()),
            strategy.recommendations.len(),
        );
    }

    // --- the loop that sends the user back into the game
    let queue = ptt_runtime::reports::probe_queue(&observations, &context_key, "corpus-league")
        .map_err(|reason| format!("probe queue failed: {reason}"))?;
    println!("probe queue:");
    for line in &queue {
        println!("  {line}");
    }

    if failures.is_empty() {
        println!("\n{} pairs, 0 invariant failures", pairs.len());
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("FAIL {failure}");
        }
        Err(format!("{} invariant failures", failures.len()).into())
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("strategy-probe requires Windows");
}
