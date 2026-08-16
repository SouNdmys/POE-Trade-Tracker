//! Contract tests for three-tier route accounting.
//!
//! Fixtures are built by hand rather than driven through the depth engine on
//! purpose: these assertions are about what the accounting does with a route,
//! and a fixture that goes through selection would let a selection change
//! silently move the numbers under the test.

use chrono::{TimeZone, Utc};
use ptt_market_book::QuoteRiskFlag;
use ptt_strategy::{
    Actionability, ExecutionRisk, MarkRateSource, MarkRateTable, ModelCaveat, RiskThresholds,
    RouteAccountingRequest, derive_route_accounting,
};
use ptt_trade_domain::{MarketAssetId, Ratio};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, ComparisonDirection, ConversionPath, ExecutionRiskFlag, FillKind,
    PairBottleneck, PairFill, QuoteLevelFill, ResidualAmount,
};

fn asset(id: &str) -> MarketAssetId {
    MarketAssetId::try_new(id).expect("asset id")
}

fn amount(id: &str, quanta: u64) -> AssetAmount {
    AssetAmount {
        asset_id: asset(id),
        quanta,
        unit: AssetUnit::whole(),
    }
}

/// One leg that consumed `consumed` of `from` and produced `produced` of `to`.
/// `capacity` is the leg's ceiling; `None` means it never ran out.
fn leg(
    from: &str,
    to: &str,
    requested: u64,
    consumed: u64,
    produced: u64,
    capacity: Option<u64>,
) -> PairFill {
    let unfilled = requested.saturating_sub(consumed);
    let bottleneck = capacity.map(|capacity| PairBottleneck {
        asset_id: asset(from),
        max_consumable_input: amount(from, capacity),
        max_gross_output: amount(to, produced),
        reason: ExecutionRiskFlag::LiquidityCapped,
    });
    let mut risk_flags = Vec::new();
    if bottleneck.is_some() {
        risk_flags.push(ExecutionRiskFlag::LiquidityCapped);
    }
    PairFill {
        from_asset_id: asset(from),
        to_asset_id: asset(to),
        kind: FillKind::TakerDepth,
        requested_input: amount(from, requested),
        consumed_input: amount(from, consumed),
        unfilled_input: amount(from, unfilled),
        gross_amount_out: amount(to, produced),
        net_amount_out: None,
        fills: vec![QuoteLevelFill {
            quote_edge_id: format!("{from}->{to}"),
            snapshot_id: "snapshot-1".to_owned(),
            captured_at: Utc
                .with_ymd_and_hms(2026, 8, 16, 10, 0, 0)
                .single()
                .expect("time"),
            rate: Ratio::from_parts(produced.max(1), consumed.max(1)).expect("rate"),
            amount_in: amount(from, consumed),
            gross_amount_out: amount(to, produced),
            net_amount_out: None,
            capacity_from: amount(from, capacity.unwrap_or(consumed)),
            // Well above the thin-liquidity threshold: these tests are about
            // arithmetic, not about risk flags.
            stock: 100_000,
            quote_risk_flags: Vec::<QuoteRiskFlag>::new(),
        }],
        capture_time_evidence: None,
        is_fully_filled: unfilled == 0,
        execution_eligible: unfilled == 0,
        bottleneck,
        risk_flags,
    }
}

fn path(steps: Vec<PairFill>, requested: AssetAmount) -> ConversionPath {
    let mut assets = vec![steps[0].from_asset_id.clone()];
    for step in &steps {
        assets.push(step.to_asset_id.clone());
    }
    let mut residuals = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if step.unfilled_input.quanta > 0 {
            residuals.push(ResidualAmount {
                // The engine counts steps taken, so the leg at index `i`
                // reports `i + 1`.
                after_step: u8::try_from(index + 1).expect("step index"),
                amount: step.unfilled_input.clone(),
            });
        }
    }
    let amount_out = steps.last().expect("steps").gross_amount_out.clone();
    let is_fully_filled = residuals.is_empty();
    ConversionPath {
        path_asset_ids: assets,
        requested_input: requested,
        amount_out,
        gross_only: true,
        bottleneck: steps.iter().find_map(|step| step.bottleneck.clone()),
        steps,
        capture_time_evidence: None,
        residuals,
        is_fully_filled,
        execution_eligible: is_fully_filled,
        risk_flags: Vec::new(),
    }
}

/// divine -> chaos -> exalted, 10 divine in, nothing capped.
fn clean_route() -> ConversionPath {
    path(
        vec![
            leg("divine-orb", "chaos-orb", 10, 10, 2_000, None),
            leg("chaos-orb", "exalted-orb", 2_000, 2_000, 400, None),
        ],
        amount("divine-orb", 10),
    )
}

/// The same route with the second leg out of depth at 1500 chaos.
fn capped_route() -> ConversionPath {
    path(
        vec![
            leg("divine-orb", "chaos-orb", 10, 10, 2_000, None),
            leg("chaos-orb", "exalted-orb", 2_000, 1_500, 300, Some(1_500)),
        ],
        amount("divine-orb", 10),
    )
}

/// divine -> exalted at 39, the baseline to beat.
fn direct_route(input_quanta: u64) -> ConversionPath {
    path(
        vec![leg(
            "divine-orb",
            "exalted-orb",
            input_quanta,
            input_quanta,
            input_quanta * 39,
            None,
        )],
        amount("divine-orb", input_quanta),
    )
}

fn request<'a>(
    route: &'a ConversionPath,
    direct: Option<&'a ConversionPath>,
    rates: &'a MarkRateTable,
) -> RouteAccountingRequest<'a> {
    RouteAccountingRequest {
        path: route,
        direct_path: direct,
        mark_rates: rates,
        thresholds: RiskThresholds::default(),
        needs_probe: false,
    }
}

#[test]
fn an_uncapped_route_recommends_the_full_size_and_admits_it_has_no_ceiling() {
    let route = clean_route();
    let direct = direct_route(10);
    let rates = MarkRateTable::new();
    let accounting = derive_route_accounting(request(&route, Some(&direct), &rates)).expect("ok");

    assert_eq!(accounting.recommended_input.quanta, 10);
    assert!(
        accounting.capacity_is_lower_bound,
        "no leg ran out, so the ceiling is unknown rather than equal to the request"
    );
    // 10 divine -> 2000 chaos -> 400 exalted, against 390 direct.
    assert_eq!(accounting.closed.output.quanta, 400);
    assert_eq!(
        accounting.closed.baseline_output.expect("baseline").quanta,
        390
    );
    assert_eq!(
        accounting.closed.direction,
        Some(ComparisonDirection::Improved)
    );
    assert_eq!(accounting.closed.delta.expect("delta").quanta, 10);
    // 10/390 = 2.56%
    assert_eq!(accounting.closed.basis_points, Some(256));
    // Nothing was capped, so all three tiers agree.
    assert_eq!(accounting.theoretical.output.quanta, 400);
    assert_eq!(accounting.mark_to_market.output.quanta, 400);
    assert!(accounting.residuals.is_empty());
    assert!(
        !accounting
            .assessment
            .caveats
            .contains(&ModelCaveat::ExtrapolatedBeyondObservedDepth)
    );
}

#[test]
fn a_capped_route_sizes_down_to_what_flows_and_separates_the_three_tiers() {
    let route = capped_route();
    let direct_full = direct_route(10);
    let mut rates = MarkRateTable::new();
    // Chaos marks slightly worse than the route rate, so mark-to-market and
    // theoretical cannot coincide by accident.
    rates.insert(
        &asset("chaos-orb"),
        &asset("exalted-orb"),
        Ratio::from_parts(19, 100).expect("rate"),
    );
    let accounting =
        derive_route_accounting(request(&route, Some(&direct_full), &rates)).expect("ok");

    // Leg two tops out at 1500 chaos; leg one turns 1 divine into 200 chaos,
    // so 7 divine is the largest whole input that clears.
    assert_eq!(accounting.closed_input_capacity.quanta, 7);
    assert!(!accounting.capacity_is_lower_bound);
    assert_eq!(accounting.recommended_input.quanta, 7);

    // Closed: 7 divine -> 1400 chaos -> 280 exalted vs 273 direct.
    assert_eq!(accounting.closed.output.quanta, 280);
    assert_eq!(
        accounting.closed.baseline_output.expect("baseline").quanta,
        273
    );
    assert_eq!(accounting.closed.delta.expect("delta").quanta, 7);

    // Theoretical extrapolates leg two past the depth that was seen.
    assert_eq!(accounting.theoretical.output.quanta, 400);
    assert!(
        accounting
            .assessment
            .caveats
            .contains(&ModelCaveat::ExtrapolatedBeyondObservedDepth),
        "the full-size figure runs past observed depth and must say so"
    );

    // Mark to market: 300 exalted realised plus 500 chaos marked at 0.19.
    let residual = accounting.residuals.first().expect("one residual");
    assert_eq!(residual.amount.quanta, 500);
    assert_eq!(residual.mark_rate_source, Some(MarkRateSource::DirectQuote));
    assert_eq!(residual.mark_value.as_ref().expect("mark").quanta, 95);
    assert_eq!(accounting.mark_to_market.output.quanta, 395);
    assert!(accounting.residual_mark_complete);
    assert!(
        accounting.mark_to_market.output.quanta < accounting.theoretical.output.quanta,
        "holding stranded chaos is worth less than the route pretends"
    );
    assert!(
        accounting
            .assessment
            .contains(ExecutionRisk::ResidualInventory)
    );
}

#[test]
fn residual_cost_basis_and_break_even_price_are_exact() {
    let route = capped_route();
    let direct_full = direct_route(10);
    let mut rates = MarkRateTable::new();
    rates.insert(
        &asset("chaos-orb"),
        &asset("exalted-orb"),
        Ratio::from_parts(19, 100).expect("rate"),
    );
    let accounting =
        derive_route_accounting(request(&route, Some(&direct_full), &rates)).expect("ok");
    let residual = accounting.residuals.first().expect("one residual");

    // 500 stranded chaos came from 2 divine, which direct would have turned
    // into 78 exalted.
    assert_eq!(residual.cost_basis.as_ref().expect("basis").quanta, 78);
    assert_eq!(
        residual.clear_direction,
        Some(ComparisonDirection::Improved)
    );
    assert_eq!(residual.clear_delta.as_ref().expect("delta").quanta, 17);
    // 78 exalted over 500 chaos reduces to 39:250 and must not be rounded.
    let break_even = residual.break_even_unit_price.as_ref().expect("break even");
    assert_eq!(break_even.numerator, 39);
    assert_eq!(break_even.denominator, 250);
}

#[test]
fn without_a_direct_route_no_improvement_is_claimed() {
    // The audit's B-08 case: a usable multi-hop route with no direct quote is
    // still worth showing, but calling it an improvement over nothing is not.
    let route = clean_route();
    let rates = MarkRateTable::new();
    let accounting = derive_route_accounting(request(&route, None, &rates)).expect("ok");

    assert_eq!(accounting.closed.output.quanta, 400);
    assert!(accounting.closed.baseline_output.is_none());
    assert!(accounting.closed.direction.is_none());
    assert!(accounting.closed.basis_points.is_none());
    assert!(accounting.theoretical.basis_points.is_none());
}

#[test]
fn a_residual_with_no_mark_rate_is_reported_incomplete_rather_than_valued_at_zero() {
    let route = capped_route();
    let direct_full = direct_route(10);
    // No chaos->exalted quote, and the remaining route can still price it,
    // so drop that possibility by marking against an unrelated account asset.
    let rates = MarkRateTable::new();
    let accounting =
        derive_route_accounting(request(&route, Some(&direct_full), &rates)).expect("ok");
    let residual = accounting.residuals.first().expect("one residual");

    // Falls back to the rest of the route, which does end in exalted.
    assert_eq!(
        residual.mark_rate_source,
        Some(MarkRateSource::RemainingRoute)
    );
    assert!(accounting.residual_mark_complete);
    // 500 chaos at the route's own 300/1500 rate is 100 exalted.
    assert_eq!(residual.mark_value.as_ref().expect("mark").quanta, 100);
}

#[test]
fn a_closed_loop_accounts_in_the_asset_it_started_from() {
    let loop_route = path(
        vec![
            leg("divine-orb", "chaos-orb", 10, 10, 2_000, None),
            leg("chaos-orb", "exalted-orb", 2_000, 2_000, 400, None),
            leg("exalted-orb", "divine-orb", 400, 400, 11, None),
        ],
        amount("divine-orb", 10),
    );
    let rates = MarkRateTable::new();
    let accounting = derive_route_accounting(request(&loop_route, None, &rates)).expect("ok");

    assert!(accounting.is_closed_loop);
    assert_eq!(accounting.account_asset_id.as_str(), "divine-orb");
    assert_eq!(accounting.closed.output.quanta, 11);
}

#[test]
fn a_maker_leg_keeps_the_route_off_the_instant_rung() {
    let mut route = clean_route();
    route.steps[1].kind = FillKind::MakerTheory;
    let rates = MarkRateTable::new();
    let accounting = derive_route_accounting(request(&route, None, &rates)).expect("ok");

    assert_eq!(
        accounting.assessment.actionability,
        Actionability::MakerTheoretical
    );
    assert!(
        accounting
            .assessment
            .contains(ExecutionRisk::MakerReference)
    );
}
