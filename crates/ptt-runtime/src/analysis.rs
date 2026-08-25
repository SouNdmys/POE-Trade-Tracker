//! Shared pair analysis: observations → coherent book → selection → engine,
//! held as typed facts.
//!
//! 句子拼好就拆不回来了:监视器页要把"这个盘口能怎么赚"画成表格
//! (种类 / 路径 / 直兑 / 最优 / 收益),所以这里存的是 typed 事实。
//! 探针仍然要打句子,[`PairAnalysis::lines`] 负责把同一份事实拼回
//! 原来的文本——两边共用一个来源,不给漂移留缝。

use std::collections::BTreeMap;

use chrono::Utc;
use ptt_market_book::{
    DataVisibility, QuoteSelectionPolicy, QuoteSelectionStrategy, build_coherent_current_book,
    select_quote_edges,
};
use ptt_settings::UiLanguage;
use ptt_trade_domain::{MarketAssetId, MarketEdgeObservation};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ConversionRequest, ExecutionRiskFlag, FeePolicy,
    MarketDepthIndex, SearchCancellation, TriangleRequest, canonical_cycle_key,
    find_best_conversion, find_triangle_opportunities,
};

/// What one accepted book says about its own pair: whether a conversion of
/// 100 units beats the direct trade, and whether any cycle from `have` comes
/// back with more than it left with.
#[derive(Clone, Debug)]
pub struct PairAnalysis {
    /// The pair, as the pipeline spells it (domain ids).
    pub have_asset_id: String,
    pub need_asset_id: String,
    /// The priced conversion, when the book prices one at all.
    pub conversion: Option<ConversionSummary>,
    /// Up to three cycles from `have`, best first.
    pub cycles: Vec<CycleSummary>,
    /// The analysis step failed outright. Kept so the page can say so — an
    /// empty table drawn over an error reads as "nothing to gain", which is
    /// a different claim entirely.
    pub error: Option<String>,
}

impl PairAnalysis {
    /// The failure value: identity kept, facts absent, reason attached.
    #[must_use]
    pub fn failed(have_asset_id: &str, need_asset_id: &str, error: String) -> Self {
        Self {
            have_asset_id: have_asset_id.to_owned(),
            need_asset_id: need_asset_id.to_owned(),
            conversion: None,
            cycles: Vec::new(),
            error: Some(error),
        }
    }

    /// The display lines the probes print, byte-for-byte what the old
    /// `Vec<String>` analysis carried: probes exist to mirror production, and
    /// text that drifts from the data it summarizes is how the today-fold
    /// bug happened.
    #[must_use]
    pub fn lines(&self, language: UiLanguage) -> Vec<String> {
        use crate::report_text::{execution_risk_flag, join};

        if let Some(error) = &self.error {
            return vec![format!("analysis error: {error}")];
        }
        let have = &self.have_asset_id;
        let need = &self.need_asset_id;
        let mut lines = Vec::new();
        match &self.conversion {
            Some(conversion) => match (conversion.direct_out, conversion.best_out) {
                (Some(direct), Some(best)) => lines.push(format!(
                    "100 {have} -> {need}: direct {direct} | best {best} ({} hops, risks {})",
                    conversion.hops(),
                    join(language, &conversion.risk_flags, execution_risk_flag),
                )),
                (Some(direct), None) => {
                    lines.push(format!("100 {have} -> {need}: direct {direct}"));
                }
                _ => lines.push("no conversion path yet (book still thin)".to_owned()),
            },
            None => lines.push("no conversion path yet (book still thin)".to_owned()),
        }
        for cycle in &self.cycles {
            lines.push(format!(
                "triangle {} profit={}bp risks={}",
                canonical_cycle_key(&cycle.cycle_asset_ids),
                cycle
                    .profit_basis_points
                    .map_or_else(|| "?".to_owned(), |points| points.to_string()),
                join(language, &cycle.risk_flags, execution_risk_flag),
            ));
        }
        if self.cycles.is_empty() {
            lines.push(format!("no triangles above 10bp from {have} yet"));
        }
        lines
    }
}

/// The 100-unit conversion, direct and best, as facts.
#[derive(Clone, Debug)]
pub struct ConversionSummary {
    /// The best path's asset sequence, `have` first, `need` last. Two
    /// entries means nothing beat the direct trade.
    pub path_asset_ids: Vec<MarketAssetId>,
    /// What 100 units bought on the direct pair, if it is quoted.
    pub direct_out: Option<u64>,
    /// What 100 units bought along the best path, if one priced.
    pub best_out: Option<u64>,
    /// The best path's risk flags.
    pub risk_flags: Vec<ExecutionRiskFlag>,
}

impl ConversionSummary {
    /// Steps along the best path.
    #[must_use]
    pub fn hops(&self) -> usize {
        self.path_asset_ids.len().saturating_sub(1)
    }

    /// Best over direct, in basis points — the number the 收益 column shows.
    #[must_use]
    pub fn gain_basis_points(&self) -> Option<i64> {
        let direct = self.direct_out?;
        let best = self.best_out?;
        if direct == 0 {
            return None;
        }
        let gain = (i128::from(best) - i128::from(direct)) * 10_000 / i128::from(direct);
        i64::try_from(gain).ok()
    }
}

/// One cycle from `have`, and what walking it returns.
#[derive(Clone, Debug)]
pub struct CycleSummary {
    /// The cycle's asset sequence, starting currency first (not repeated at
    /// the end).
    pub cycle_asset_ids: Vec<MarketAssetId>,
    pub profit_basis_points: Option<i64>,
    pub risk_flags: Vec<ExecutionRiskFlag>,
}

/// Direct/best conversion for 100 units of `have` into `need`, plus the top
/// triangles starting from `have`. Purely informational; every number keeps
/// its risk flags attached.
pub fn pair_analysis(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    need: &MarketAssetId,
    have: &MarketAssetId,
) -> Result<PairAnalysis, Box<dyn std::error::Error>> {
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
    // The path that gets stored is the best one when it exists, the direct
    // one otherwise: the 路径 column shows where the money actually goes.
    let summary = match (&conversion.direct_path, &conversion.best_path) {
        (None, None) => None,
        (direct, best) => {
            let shown = best.as_ref().or(direct.as_ref());
            shown.map(|path| {
                let mut ids: Vec<MarketAssetId> = path
                    .steps
                    .iter()
                    .map(|step| step.from_asset_id.clone())
                    .collect();
                if let Some(last) = path.steps.last() {
                    ids.push(last.to_asset_id.clone());
                }
                ConversionSummary {
                    path_asset_ids: ids,
                    direct_out: direct.as_ref().map(|path| path.amount_out.quanta),
                    best_out: best.as_ref().map(|path| path.amount_out.quanta),
                    risk_flags: path.risk_flags.clone(),
                }
            })
        }
    };

    let triangles = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: have.clone(),
            amount_in: Some(AssetAmount::from_whole_units(
                have.clone(),
                100,
                &unit_catalog,
            )?),
            minimum_profit_basis_points: 10,
            max_cycle_length: 4,
            max_results: 3,
            max_evaluations: 50_000,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )?;
    let cycles = triangles
        .opportunities
        .iter()
        .take(3)
        .map(|opportunity| CycleSummary {
            cycle_asset_ids: opportunity
                .steps
                .iter()
                .map(|step| step.from_asset_id.clone())
                .collect(),
            profit_basis_points: opportunity.profit_basis_points,
            risk_flags: opportunity.risk_flags.clone(),
        })
        .collect();

    Ok(PairAnalysis {
        have_asset_id: have.as_str().to_owned(),
        need_asset_id: need.as_str().to_owned(),
        conversion: summary,
        cycles,
        error: None,
    })
}

#[cfg(test)]
mod analysis_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// The probe's text is a projection of the typed facts, and the projection
    /// has to keep the old wording: probes are read next to old logs.
    #[test]
    fn lines_render_the_full_conversion_sentence() {
        let analysis = PairAnalysis {
            have_asset_id: "divine-orb".to_owned(),
            need_asset_id: "chaos-orb".to_owned(),
            conversion: Some(ConversionSummary {
                path_asset_ids: vec![
                    asset("divine-orb"),
                    asset("exalted-orb"),
                    asset("chaos-orb"),
                ],
                direct_out: Some(10),
                best_out: Some(15),
                risk_flags: Vec::new(),
            }),
            cycles: Vec::new(),
            error: None,
        };
        let lines = analysis.lines(UiLanguage::English);
        assert_eq!(
            lines[0],
            "100 divine-orb -> chaos-orb: direct 10 | best 15 (2 hops, risks )"
        );
        assert_eq!(lines[1], "no triangles above 10bp from divine-orb yet");
    }

    /// +50% for 10 → 15, negative for a losing pair, none without a direct
    /// baseline — the 收益 column's arithmetic.
    #[test]
    fn gain_is_best_over_direct_in_basis_points() {
        let summary = |direct: Option<u64>, best: Option<u64>| ConversionSummary {
            path_asset_ids: vec![asset("a"), asset("b")],
            direct_out: direct,
            best_out: best,
            risk_flags: Vec::new(),
        };
        assert_eq!(summary(Some(10), Some(15)).gain_basis_points(), Some(5000));
        assert_eq!(summary(Some(20), Some(15)).gain_basis_points(), Some(-2500));
        assert_eq!(summary(None, Some(15)).gain_basis_points(), None);
        assert_eq!(summary(Some(0), Some(15)).gain_basis_points(), None);
    }

    /// A failed analysis still says which pair it failed for, and its lines
    /// carry the reason instead of pretending the book had nothing.
    #[test]
    fn a_failed_analysis_keeps_its_reason() {
        let analysis = PairAnalysis::failed("divine-orb", "chaos-orb", "load: boom".to_owned());
        assert_eq!(
            analysis.lines(UiLanguage::English),
            vec!["analysis error: load: boom".to_owned()]
        );
    }
}
