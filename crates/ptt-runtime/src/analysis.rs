//! Shared pair analysis: observations → coherent book → selection → engine,
//! rendered as display lines for probes and the UI alike.

use std::collections::BTreeMap;

use chrono::Utc;
use ptt_market_book::{
    DataVisibility, QuoteSelectionPolicy, QuoteSelectionStrategy, build_coherent_current_book,
    select_quote_edges,
};
use ptt_settings::UiLanguage;
use ptt_trade_domain::{MarketAssetId, MarketEdgeObservation};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ConversionRequest, FeePolicy, MarketDepthIndex,
    SearchCancellation, TriangleRequest, canonical_cycle_key, find_best_conversion,
    find_triangle_opportunities,
};

/// Direct/best conversion for 100 units of `have` into `need`, plus the top
/// triangles starting from `have`. Purely informational lines; every number
/// keeps its risk flags attached.
pub fn pair_analysis_lines(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    need: &MarketAssetId,
    have: &MarketAssetId,
    language: UiLanguage,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use crate::report_text::{execution_risk_flag, join};

    let mut lines = Vec::new();
    let book = build_coherent_current_book(context_key, observations, DataVisibility::default())?;
    let policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)?;
    let selection = select_quote_edges(&book, &policy, Utc::now())?;

    let mut units = BTreeMap::new();
    for observation in observations {
        units.insert(observation.edge.from_asset_id.clone(), AssetUnit::whole());
        units.insert(observation.edge.to_asset_id.clone(), AssetUnit::whole());
    }
    let unit_catalog = AssetUnitCatalog::try_new(units)?;
    let index = MarketDepthIndex::try_from_selection(&selection, unit_catalog.clone())?;

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
        // Flag names come from the report catalogue rather than from `Debug`,
        // which put Rust identifiers on screen and let a new variant reach a
        // reader unnamed. Naming them there means a new one does not compile
        // until both languages have a word for it.
        (Some(direct), Some(best)) => lines.push(format!(
            "100 {have} -> {need}: direct {} | best {} ({} hops, risks {})",
            direct.amount_out.quanta,
            best.amount_out.quanta,
            best.steps.len(),
            join(language, &best.risk_flags, execution_risk_flag),
        )),
        (Some(direct), None) => lines.push(format!(
            "100 {have} -> {need}: direct {}",
            direct.amount_out.quanta
        )),
        _ => lines.push("no conversion path yet (book still thin)".to_owned()),
    }

    let triangles = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: have.clone(),
            amount_in: AssetAmount::from_whole_units(have.clone(), 100, &unit_catalog)?,
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
        lines.push(format!(
            "triangle {} profit={}bp risks={}",
            canonical_cycle_key(&cycle),
            opportunity
                .profit_basis_points
                .map_or_else(|| "?".to_owned(), |points| points.to_string()),
            join(language, &opportunity.risk_flags, execution_risk_flag),
        ));
    }
    if triangles.opportunities.is_empty() {
        lines.push(format!("no triangles above 10bp from {have} yet"));
    }
    Ok(lines)
}
