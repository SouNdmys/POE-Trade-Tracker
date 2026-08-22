//! Differential validation: replay the old POE2-Trade-Tracker-Electron
//! database (a full season of confirmed captures) through the new pipeline —
//! domain edge construction → coherent book → selection → depth engine —
//! and report everything deterministically. Any surprise here is either a
//! porting bug or must be attributable to a named F-series fix.
//!
//! Usage: `engine-replay-probe [OLD_SQLITE_PATH]`
//! (default: %APPDATA%\poe2-trade-tracker\poe2_trade_tracker.sqlite)

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use ptt_market_book::{
    DataVisibility, QuoteSelectionPolicy, QuoteSelectionStrategy, build_coherent_current_book,
    select_quote_edges,
};
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, Game, MarketAssetId, MarketContext, MarketEdgeObservation,
    ObservationIdentity, QuoteSide, ReviewAuditEntry, ReviewAuditKind, SnapshotRecordStatus,
};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ConversionRequest, FeePolicy, MarketDepthIndex,
    SearchCancellation, TriangleRequest, canonical_cycle_key, find_best_conversion,
    find_triangle_opportunities,
};
use rusqlite::{Connection, OpenFlags};

fn sha_filler() -> String {
    "0".repeat(64)
}

/// Old ids use underscores; the domain id grammar wants hyphens.
fn map_asset_id(old: &str) -> Result<MarketAssetId, String> {
    MarketAssetId::try_new(old.replace('_', "-"))
        .map_err(|error| format!("asset id {old:?}: {error:?}"))
}

/// Old ratios are REAL columns; the game displays at most two decimals, so
/// two-decimal formatting recovers the on-screen value exactly.
fn ratio_component(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as u64)
    } else {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

struct OldQuote {
    side: QuoteSide,
    row_index: u8,
    comparator: Comparator,
    ratio_text: String,
    stock: i64,
    confidence_ppm: Option<u32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}\\poe2-trade-tracker\\poe2_trade_tracker.sqlite",
            std::env::var("APPDATA").expect("APPDATA")
        )
    });
    println!("replaying {path}");
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let identity = ObservationIdentity::try_new(
        "electron-paddle-replay",
        "0.5.0",
        sha_filler(),
        sha_filler(),
        sha_filler(),
        "old-poe2-catalog",
        sha_filler(),
        "old-poe2-recognition",
        sha_filler(),
        "replay",
    )?;
    let context = MarketContext::try_new_for(
        Game::Poe2,
        ClientLanguage::TraditionalChinese,
        "replay-league",
        "electron-era",
        "electron-roi-profile",
        1,
        "electron-parser",
        identity.clone(),
    )?;
    let provenance = CaptureProvenance {
        draft_id: "replay-draft".to_owned(),
        capture_job_id: "replay-job".to_owned(),
        review_revision: 1,
        confirmation_mode: CaptureConfirmationMode::UserReviewed,
        source: "offline_real_evidence_replay".to_owned(),
        evidence_id: "replay-evidence".to_owned(),
        evidence_removed: true,
        frame_hashes: vec![sha_filler(), sha_filler()],
        profile_sha256: sha_filler(),
        provider_id: identity.ocr_provider_id.clone(),
        provider_version: identity.ocr_provider_version.clone(),
        model_sha256: identity.ocr_model_sha256.clone(),
        provider_manifest_sha256: identity.ocr_provider_manifest_sha256.clone(),
        parser_assets_sha256: identity.parser_assets_sha256.clone(),
    };

    // Load snapshots and their quotes.
    let mut snapshots: BTreeMap<String, (String, String, DateTime<Utc>)> = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT id, base_currency_id, quote_currency_id, captured_at
             FROM market_snapshots WHERE completeness_status = 'Complete' ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let captured: String = row.get(3)?;
            snapshots.insert(
                row.get(0)?,
                (
                    row.get(1)?,
                    row.get(2)?,
                    DateTime::parse_from_rfc3339(&captured)?.with_timezone(&Utc),
                ),
            );
        }
    }
    let mut quotes: BTreeMap<String, Vec<OldQuote>> = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT snapshot_id, side, row_index, comparator, ratio_num, ratio_den, stock,
                    confidence
             FROM market_quotes WHERE confirmed = 1 ORDER BY snapshot_id, side, row_index",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let snapshot_id: String = row.get(0)?;
            let side: String = row.get(1)?;
            let comparator: String = row.get(3)?;
            let ratio_num: f64 = row.get(4)?;
            let ratio_den: f64 = row.get(5)?;
            let confidence: Option<f64> = row.get(7)?;
            quotes.entry(snapshot_id).or_default().push(OldQuote {
                side: match side.as_str() {
                    "Available" => QuoteSide::Available,
                    "Competing" => QuoteSide::Competing,
                    other => {
                        println!("  skip quote: unknown side {other:?}");
                        continue;
                    }
                },
                row_index: u8::try_from(row.get::<_, i64>(2)?).unwrap_or(u8::MAX),
                comparator: match comparator.as_str() {
                    "Exact" => Comparator::Exact,
                    "LessThan" => Comparator::LessThan,
                    "GreaterThan" => Comparator::GreaterThan,
                    other => {
                        println!("  skip quote: unknown comparator {other:?}");
                        continue;
                    }
                },
                ratio_text: format!(
                    "{}:{}",
                    ratio_component(ratio_num),
                    ratio_component(ratio_den)
                ),
                stock: row.get(6)?,
                confidence_ppm: confidence
                    .map(|value| (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u32),
            });
        }
    }

    // Rebuild captures through the domain (its validation is the point).
    let mut observations: Vec<MarketEdgeObservation> = Vec::new();
    let mut rebuilt = 0usize;
    let mut skipped_rows = 0usize;
    let mut skipped_snapshots = Vec::new();
    let mut latest_capture = DateTime::<Utc>::UNIX_EPOCH;
    for (snapshot_id, (need_old, have_old, captured_at)) in &snapshots {
        let Some(old_rows) = quotes.get(snapshot_id) else {
            skipped_snapshots.push((snapshot_id.clone(), "no quotes".to_owned()));
            continue;
        };
        let (Ok(need), Ok(have)) = (map_asset_id(need_old), map_asset_id(have_old)) else {
            skipped_snapshots.push((snapshot_id.clone(), "bad asset id".to_owned()));
            continue;
        };
        let mut rows = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for quote in old_rows {
            if !seen.insert((quote.side as u8, quote.row_index)) {
                skipped_rows += 1;
                continue;
            }
            match ConfirmedOrderRow::try_new(
                quote.side,
                quote.row_index,
                quote.comparator,
                &quote.ratio_text,
                &quote.stock.to_string(),
                false,
                quote.confidence_ppm,
            ) {
                Ok(row) => rows.push(row),
                Err(_) => skipped_rows += 1,
            }
        }
        if rows.is_empty() {
            skipped_snapshots.push((snapshot_id.clone(), "no valid rows".to_owned()));
            continue;
        }
        match ConfirmedCapture::try_new(
            *captured_at,
            context.clone(),
            need,
            have,
            rows,
            provenance.clone(),
            "{\"replay\":true}".to_owned(),
            "{\"replay\":true}".to_owned(),
            vec![ReviewAuditEntry {
                field_path: "replay".to_owned(),
                before: serde_json::json!(null),
                after: serde_json::json!("old-db"),
                kind: ReviewAuditKind::AcceptedMachineValue,
            }],
        ) {
            Ok(capture) => {
                rebuilt += 1;
                latest_capture = latest_capture.max(capture.captured_at);
                observations.extend(
                    capture
                        .quote_edges
                        .iter()
                        .map(|edge| MarketEdgeObservation {
                            edge: edge.clone(),
                            snapshot_complete: true,
                            record_status: SnapshotRecordStatus::Active,
                            record_revision: 1,
                            record_reason: None,
                        }),
                );
            }
            Err(error) => skipped_snapshots.push((snapshot_id.clone(), format!("{error:?}"))),
        }
    }
    println!(
        "rebuilt {rebuilt}/{} snapshots ({} edges, {skipped_rows} rows skipped)",
        snapshots.len(),
        observations.len()
    );
    for (snapshot_id, reason) in &skipped_snapshots {
        println!("  skipped snapshot {snapshot_id}: {reason}");
    }

    // Book + selection at a frozen "now" just after the newest capture, so
    // freshness reflects the intra-dataset ages, not months of wall time.
    let now = latest_capture + Duration::seconds(60);
    let book = build_coherent_current_book(
        &context.stable_key(),
        &observations,
        DataVisibility::default(),
    )?;
    println!(
        "coherent book: {} pair views, {} exclusions",
        book.views.len(),
        book.exclusions.len()
    );
    // Replay-tuned freshness: the dataset is historical, so the windows are
    // widened to keep intra-season ages meaningful without archiving all of
    // it. Everything else stays at the shipping defaults.
    let mut policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)?;
    policy.freshness =
        ptt_market_book::FreshnessPolicy::try_new(30 * 24 * 3600, 60 * 24 * 3600, 90 * 24 * 3600)?;
    let selection = select_quote_edges(&book, &policy, now)?;
    let mut reject_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut selected_pairs = 0usize;
    for selected in &selection.selections {
        if selected.selected_edge.is_some() {
            selected_pairs += 1;
        }
        for rejection in &selected.rejections {
            for reason in &rejection.reasons {
                *reject_histogram.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
    }
    println!(
        "selection: {} directional pairs with a selected edge of {},          rejection reasons: {reject_histogram:?}",
        selected_pairs,
        selection.selections.len()
    );
    // Identity print for differential runs: which edge won each direction.
    for selected in &selection.selections {
        if let Some(edge) = &selected.selected_edge {
            println!(
                "selected {} = {} @ {}",
                selected.pair_key, edge.observation.edge.edge_id, edge.observation.edge.rate.text
            );
        }
    }

    // Whole units for every asset seen (POE2 trades whole units — F2).
    let mut unit_entries: BTreeMap<MarketAssetId, AssetUnit> = BTreeMap::new();
    for observation in &observations {
        unit_entries.insert(observation.edge.from_asset_id.clone(), AssetUnit::whole());
        unit_entries.insert(observation.edge.to_asset_id.clone(), AssetUnit::whole());
    }
    let units = AssetUnitCatalog::try_new(unit_entries.clone())?;
    let index = MarketDepthIndex::try_from_selection(&selection, units.clone())?;

    // Best conversion over the densest pair.
    let divine = MarketAssetId::try_new("divine-orb")?;
    let chaos = MarketAssetId::try_new("chaos-orb")?;
    if unit_entries.contains_key(&divine) && unit_entries.contains_key(&chaos) {
        let conversion = find_best_conversion(
            &index,
            &ConversionRequest {
                from_asset_id: divine.clone(),
                to_asset_id: chaos.clone(),
                amount_in: AssetAmount::from_whole_units(divine.clone(), 10, &units)?,
                max_hops: 3,
                max_paths: 64,
                max_expansions: 10_000,
                alternative_limit: 3,
                allowed_intermediate_asset_ids: None,
                fee_policy: FeePolicy::None,
            },
            &SearchCancellation::default(),
        )?;
        match &conversion.best_path {
            Some(path) => println!(
                "best divine->chaos (10): out={} hops={} eligible={} risks={:?}",
                path.amount_out.quanta,
                path.steps.len(),
                path.execution_eligible,
                path.risk_flags,
            ),
            None => println!("best divine->chaos: no path"),
        }
        if let Some(direct) = &conversion.direct_path {
            println!("direct divine->chaos: out={}", direct.amount_out.quanta);
        }
    }

    // Triangles from the three anchor assets.
    for start_name in ["divine-orb", "chaos-orb", "exalted-orb"] {
        let start = MarketAssetId::try_new(start_name)?;
        if !unit_entries.contains_key(&start) {
            continue;
        }
        let result = find_triangle_opportunities(
            &index,
            &TriangleRequest {
                start_asset_id: start.clone(),
                amount_in: Some(AssetAmount::from_whole_units(start.clone(), 10, &units)?),
                minimum_profit_basis_points: 1,
                max_cycle_length: 4,
                max_results: 10,
                max_evaluations: 100_000,
                fee_policy: FeePolicy::None,
            },
            &SearchCancellation::default(),
        )?;
        println!(
            "triangles from {start_name}: {} opportunities, {} rejections",
            result.opportunities.len(),
            result.rejections.len()
        );
        for opportunity in result.opportunities.iter().take(5) {
            let cycle: Vec<MarketAssetId> = opportunity
                .steps
                .iter()
                .map(|step| step.from_asset_id.clone())
                .collect();
            println!(
                "  {} profit={:?}bp kind={:?} eligible={} risks={:?}",
                canonical_cycle_key(&cycle),
                opportunity.profit_basis_points,
                opportunity.profit_kind,
                opportunity.execution_eligible,
                opportunity.risk_flags,
            );
        }
    }
    Ok(())
}
