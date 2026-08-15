//! Live watch with the full pipeline: recognition → double-read confirmation
//! → domain capture → SQLite → coherent book → selection → depth engine,
//! printing the current pair's opportunities after every accepted book.
//!
//! Usage: `watch-probe [--seconds N] [--db PATH]`
//! (default 120s; default db `%LOCALAPPDATA%\PoeTradeTracker\market.sqlite`)

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use chrono::Utc;
    use ptt_market_book::{
        DataVisibility, QuoteSelectionPolicy, QuoteSelectionStrategy, build_coherent_current_book,
        select_quote_edges,
    };
    use ptt_monitoring::{SessionConfig, SessionEvent, run_session};
    use ptt_recognition::profiles::poe2_zhtw::Route;
    use ptt_runtime::live::{capture_from_book, domain_asset_id, poe2_live_context};
    use ptt_storage::MarketStore;
    use ptt_trade_engine::{
        AssetAmount, AssetUnit, AssetUnitCatalog, ConversionRequest, FeePolicy, MarketDepthIndex,
        SearchCancellation, TriangleRequest, canonical_cycle_key, find_best_conversion,
        find_triangle_opportunities,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;

    let mut seconds: u64 = 120;
    let mut db_path: Option<String> = None;
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--seconds" if index + 1 < arguments.len() => {
                seconds = arguments[index + 1].parse()?;
                index += 2;
            }
            "--db" if index + 1 < arguments.len() => {
                db_path = Some(arguments[index + 1].clone());
                index += 2;
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }
    let db_path = db_path.unwrap_or_else(|| {
        format!(
            "{}\\PoeTradeTracker\\market.sqlite",
            std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA")
        )
    });
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    static CANCEL: AtomicBool = AtomicBool::new(false);
    println!("watching for {seconds}s, storing to {db_path}");
    let route = Route::new().map_err(|reason| format!("route init failed: {reason:?}"))?;
    let mut store = MarketStore::open(&db_path)?;
    let context = poe2_live_context("live-league")?;
    let context_key = context.stable_key();
    let mut sequence: u64 = 0;

    let stats = run_session(
        &route,
        &SessionConfig::default(),
        std::time::Duration::from_secs(seconds),
        &CANCEL,
        |event| {
            let SessionEvent::Accepted { book, elapsed } = event else {
                if let SessionEvent::FrameSkipped { .. } | SessionEvent::CaptureError(_) = event {
                    // Quiet: the session stats summarize skips at the end.
                }
                return;
            };
            sequence += 1;
            let need_id = book.observation.identity.need_asset_id.clone();
            let have_id = book.observation.identity.have_asset_id.clone();
            println!(
                "\nACCEPT #{sequence} [{:.0}ms] {} -> {} rows={}",
                elapsed.as_secs_f64() * 1e3,
                need_id,
                have_id,
                book.observation.rows.len(),
            );

            // Persist through the domain; a mapping failure is loud, never fatal.
            let capture = match capture_from_book(&book, &context, Utc::now(), sequence) {
                Ok(capture) => capture,
                Err(error) => {
                    println!("  capture mapping failed: {error:?}");
                    return;
                }
            };
            if let Err(error) = store.persist_capture(&capture) {
                println!("  persist failed: {error}");
                return;
            }

            // Recompute the whole book and show what matters for this pair.
            let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
                let observations = store.load_observations(&context_key)?;
                let book_view = build_coherent_current_book(
                    &context_key,
                    &observations,
                    DataVisibility::default(),
                )?;
                let policy =
                    QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)?;
                let selection = select_quote_edges(&book_view, &policy, Utc::now())?;
                let mut units = BTreeMap::new();
                for observation in &observations {
                    units.insert(observation.edge.from_asset_id.clone(), AssetUnit::whole());
                    units.insert(observation.edge.to_asset_id.clone(), AssetUnit::whole());
                }
                let unit_catalog = AssetUnitCatalog::try_new(units)?;
                let index = MarketDepthIndex::try_from_selection(&selection, unit_catalog.clone())?;

                let need = domain_asset_id(&need_id)?;
                let have = domain_asset_id(&have_id)?;
                let conversion = find_best_conversion(
                    &index,
                    &ConversionRequest {
                        from_asset_id: have.clone(),
                        to_asset_id: need.clone(),
                        amount_in: AssetAmount::from_whole_units(have.clone(), 100, &unit_catalog)?,
                        max_hops: 3,
                        max_paths: 64,
                        max_expansions: 10_000,
                        alternative_limit: 2,
                        allowed_intermediate_asset_ids: None,
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                )?;
                match (&conversion.direct_path, &conversion.best_path) {
                    (Some(direct), Some(best)) => println!(
                        "  100 {} -> {}: direct {} | best {} ({} hops, risks {:?})",
                        have_id,
                        need_id,
                        direct.amount_out.quanta,
                        best.amount_out.quanta,
                        best.steps.len(),
                        best.risk_flags,
                    ),
                    (Some(direct), None) => println!(
                        "  100 {have_id} -> {need_id}: direct {}",
                        direct.amount_out.quanta
                    ),
                    _ => println!("  no conversion path yet (book still thin)"),
                }

                let triangles = find_triangle_opportunities(
                    &index,
                    &TriangleRequest {
                        start_asset_id: have.clone(),
                        amount_in: AssetAmount::from_whole_units(have, 100, &unit_catalog)?,
                        minimum_profit_basis_points: 10,
                        max_results: 3,
                        max_evaluations: 50_000,
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                )?;
                for opportunity in triangles.opportunities.iter().take(3) {
                    let cycle: Vec<_> = opportunity
                        .steps
                        .iter()
                        .map(|step| step.from_asset_id.clone())
                        .collect();
                    println!(
                        "  triangle {} profit={:?}bp risks={:?}",
                        canonical_cycle_key(&cycle),
                        opportunity.profit_basis_points,
                        opportunity.risk_flags,
                    );
                }
                if triangles.opportunities.is_empty() {
                    println!("  no triangles above 10bp from {have_id} yet");
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                println!("  analysis failed: {error}");
            }
        },
    );

    println!(
        "\nticks={} idle={} recognitions={} accepted={} duplicates={} \
         confirm-mismatch={} capture-errors={}",
        stats.ticks,
        stats.idle_ticks,
        stats.recognitions,
        stats.accepted,
        stats.duplicates,
        stats.confirmation_mismatches,
        stats.capture_errors,
    );
    if !stats.frame_skips.is_empty() {
        println!("frame skips by reason:");
        for (reason, count) in &stats.frame_skips {
            println!("  {reason}: {count}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("watch-probe requires Windows");
}
