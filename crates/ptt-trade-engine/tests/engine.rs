use chrono::{TimeZone, Utc};
use ptt_market_book::{
    CaptureSkewPolicy, CostVerification, DataVisibility, EvaluatedQuoteEdge, FreshnessAssessment,
    FreshnessStatus, PolicyCalibrationStatus, QuoteSelectionPolicy, QuoteSelectionResult,
    QuoteSelectionStrategy, SelectedQuoteEdge,
};
use ptt_trade_domain::{
    Comparator, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge, QuoteEdgeRole,
    QuoteSide, Ratio, SnapshotRecordStatus,
};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ComparisonDirection, ConversionComparisonStatus,
    ConversionRequest, EngineError, ExecutionRiskFlag, FeePolicy, MarketDepthIndex, ProfitKind,
    SearchCancellation, TriangleEvaluationStatus, TriangleRequest, find_best_conversion,
    find_triangle_opportunities,
};

#[derive(Clone, Copy)]
struct LevelSpec<'a> {
    id: &'a str,
    rate: &'a str,
    stock: u64,
}

struct PairSpec<'a> {
    from: &'a str,
    to: &'a str,
    levels: Vec<LevelSpec<'a>>,
}

fn asset(value: &str) -> MarketAssetId {
    MarketAssetId::try_new(value).expect("asset")
}

fn whole_catalog(asset_ids: &[&str]) -> AssetUnitCatalog {
    AssetUnitCatalog::try_new(
        asset_ids
            .iter()
            .map(|value| (asset(value), AssetUnit::whole())),
    )
    .expect("catalog")
}

fn amount(asset_id: &str, whole_units: u64, catalog: &AssetUnitCatalog) -> AssetAmount {
    AssetAmount::from_whole_units(asset(asset_id), whole_units, catalog).expect("amount")
}

fn candidate(
    strategy: QuoteSelectionStrategy,
    from: &str,
    to: &str,
    level: LevelSpec<'_>,
) -> EvaluatedQuoteEdge {
    let captured_at = Utc
        .with_ymd_and_hms(2026, 7, 30, 10, 0, 0)
        .single()
        .expect("time");
    let maker = strategy != QuoteSelectionStrategy::Instant;
    let execution_type = if maker {
        ExecutionType::MakerReference
    } else {
        ExecutionType::Taker
    };
    let role = if maker {
        QuoteEdgeRole::CompetingMakerReference
    } else {
        QuoteEdgeRole::AvailableTaker
    };
    let source_side = if maker {
        QuoteSide::Competing
    } else {
        QuoteSide::Available
    };
    EvaluatedQuoteEdge {
        observation: MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: level.id.to_owned(),
                snapshot_id: format!("snapshot-{from}-{to}"),
                quote_id: format!("quote-{}", level.id),
                context_key: "context".to_owned(),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                rate: Ratio::parse(level.rate).expect("rate"),
                source_side,
                execution_type,
                role,
                stock: level.stock,
                original_need_asset_id: asset(to),
                original_have_asset_id: asset(from),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: true,
                machine_confidence_ppm: None,
                captured_at,
                confirmed_at: captured_at,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        },
        freshness: FreshnessAssessment {
            status: FreshnessStatus::Fresh,
            age_seconds: 60,
            future_timestamp: false,
        },
        effective_confidence_ppm: 1_000_000,
        risk_flags: Vec::new(),
        selection_rejections: Vec::new(),
        execution_blockers: Vec::new(),
        accepted_for_selection: true,
        eligible_for_depth_analysis: true,
    }
}

fn selection(strategy: QuoteSelectionStrategy, pairs: Vec<PairSpec<'_>>) -> QuoteSelectionResult {
    let mut policy = QuoteSelectionPolicy::personal_default(strategy).expect("policy");
    policy.identity.policy_id = "test_verified_policy".to_owned();
    policy.identity.source = "test-only calibrated fixture".to_owned();
    policy.identity.calibration_status = PolicyCalibrationStatus::Verified;
    policy.cost_verification = CostVerification {
        fee_verified: true,
        minimum_lots_verified: true,
    };
    policy.capture_skew = CaptureSkewPolicy {
        max_capture_skew_seconds: Some(600),
        calibration_status: PolicyCalibrationStatus::Verified,
    };
    policy.product_execution_allowed = true;
    policy.validate().expect("test policy");
    let selections = pairs
        .into_iter()
        .map(|pair| {
            let candidates = pair
                .levels
                .into_iter()
                .map(|level| candidate(strategy, pair.from, pair.to, level))
                .collect::<Vec<_>>();
            SelectedQuoteEdge {
                pair_key: format!("{}->{}", pair.from, pair.to),
                from_asset_id: asset(pair.from),
                to_asset_id: asset(pair.to),
                strategy,
                selected_edge: candidates.first().cloned(),
                candidate_edges: candidates,
                rejections: Vec::new(),
                execution_eligible: true,
                needs_probe: false,
            }
        })
        .collect();
    QuoteSelectionResult {
        context_key: "context".to_owned(),
        policy,
        selections,
    }
}

fn index(
    strategy: QuoteSelectionStrategy,
    catalog: AssetUnitCatalog,
    pairs: Vec<PairSpec<'_>>,
) -> MarketDepthIndex {
    MarketDepthIndex::try_from_selection(&selection(strategy, pairs), catalog).expect("index")
}

#[test]
fn taker_depth_consumes_multiple_levels_and_floors_each_listing() {
    let catalog = whole_catalog(&["a", "b"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![
                LevelSpec {
                    id: "edge-one",
                    rate: "2:3",
                    stock: 3,
                },
                LevelSpec {
                    id: "edge-two",
                    rate: "2:3",
                    stock: 3,
                },
            ],
        }],
    );
    let fill = index
        .fill_pair(&amount("a", 8, &catalog), &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert_eq!(fill.fills.len(), 2);
    assert_eq!(fill.consumed_input.quanta, 8);
    assert_eq!(fill.gross_amount_out.quanta, 4);
    assert_eq!(fill.net_amount_out.expect("known fee").quanta, 4);
    assert!(fill.is_fully_filled);
    assert!(fill.execution_eligible);
}

#[test]
fn target_side_stock_caps_input_with_exact_floor_inequality() {
    let catalog = whole_catalog(&["a", "b"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-cap",
                rate: "3:2",
                stock: 4,
            }],
        }],
    );
    let fill = index
        .fill_pair(&amount("a", 4, &catalog), &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert_eq!(fill.consumed_input.quanta, 2);
    assert_eq!(fill.gross_amount_out.quanta, 3);
    assert_eq!(fill.unfilled_input.quanta, 2);
    assert!(!fill.is_fully_filled);
    assert!(
        fill.risk_flags
            .contains(&ExecutionRiskFlag::LiquidityCapped)
    );
}

#[test]
fn minimum_unit_and_known_fee_round_at_the_target_boundary() {
    let catalog = AssetUnitCatalog::try_new([
        (asset("a"), AssetUnit::whole()),
        (asset("b"), AssetUnit::try_new(1, 2).expect("half unit")),
    ])
    .expect("catalog");
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-unit",
                rate: "1:1",
                stock: 10,
            }],
        }],
    );
    let fill = index
        .fill_pair(
            &amount("a", 1, &catalog),
            &asset("b"),
            FeePolicy::OutputFraction {
                numerator: 1,
                denominator: 4,
            },
        )
        .expect("fill")
        .expect("pair");
    assert_eq!(fill.gross_amount_out.quanta, 2);
    assert_eq!(fill.gross_amount_out.exact_display_fraction(), (1, 1));
    assert_eq!(fill.net_amount_out.expect("net").quanta, 1);
}

#[test]
fn unknown_fee_only_produces_gross_theory() {
    let catalog = whole_catalog(&["a", "b"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-fee",
                rate: "2:1",
                stock: 100,
            }],
        }],
    );
    let fill = index
        .fill_pair(&amount("a", 10, &catalog), &asset("b"), FeePolicy::Unknown)
        .expect("fill")
        .expect("pair");
    assert!(fill.net_amount_out.is_none());
    assert!(!fill.execution_eligible);
    assert!(fill.risk_flags.contains(&ExecutionRiskFlag::UnknownFee));
}

#[test]
fn unverified_policy_overrides_known_zero_fee_to_unknown_theory() {
    let catalog = whole_catalog(&["a", "b"]);
    let mut safe_selection = selection(
        QuoteSelectionStrategy::Instant,
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-unverified-cost",
                rate: "2:1",
                stock: 100,
            }],
        }],
    );
    safe_selection.policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
        .expect("safe policy");
    let index = MarketDepthIndex::try_from_selection(&safe_selection, catalog.clone())
        .expect("analysis index");
    let fill = index
        .fill_pair(&amount("a", 10, &catalog), &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert!(fill.net_amount_out.is_none());
    assert!(!fill.execution_eligible);
    assert!(fill.risk_flags.contains(&ExecutionRiskFlag::UnknownFee));
    assert!(
        fill.risk_flags
            .contains(&ExecutionRiskFlag::UnverifiedProductPolicy)
    );
    assert!(
        fill.risk_flags
            .contains(&ExecutionRiskFlag::UnverifiedMinimumLots)
    );

    let conversion = find_best_conversion(
        &index,
        &ConversionRequest {
            from_asset_id: asset("a"),
            to_asset_id: asset("b"),
            amount_in: amount("a", 10, &catalog),
            max_hops: 1,
            max_paths: 5,
            max_expansions: 5,
            alternative_limit: 1,
            allowed_intermediate_asset_ids: None,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let path = conversion.best_path.expect("path");
    assert!(path.gross_only);
    assert!(!path.execution_eligible);
    assert!(path.steps.iter().all(|step| step.net_amount_out.is_none()));
    assert_eq!(
        conversion.comparison.status,
        ConversionComparisonStatus::ComparableGross
    );
    assert_eq!(
        conversion.analysis_policy.identity.policy_id,
        "personal_default_v1"
    );
}

#[test]
fn none_fee_keeps_gross_theory_and_armed_skew_gate_passes_zero_skew() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let mut selected = selection(
        QuoteSelectionStrategy::Instant,
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b-unverified",
                    rate: "1:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-unverified",
                    rate: "1:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: vec![LevelSpec {
                    id: "c-a-unverified",
                    rate: "11:10",
                    stock: 200,
                }],
            },
        ],
    );
    selected.policy = QuoteSelectionPolicy::personal_default(QuoteSelectionStrategy::Instant)
        .expect("safe policy");
    let index = MarketDepthIndex::try_from_selection(&selected, catalog.clone()).expect("index");
    let result = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: asset("a"),
            amount_in: amount("a", 100, &catalog),
            minimum_profit_basis_points: 1,
            max_results: 10,
            max_evaluations: 10,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("triangle");
    let opportunity = result.opportunities.first().expect("gross theory");
    assert_eq!(opportunity.profit_kind, Some(ProfitKind::GrossTheory));
    assert!(!opportunity.execution_eligible);
    assert!(
        opportunity
            .steps
            .iter()
            .all(|step| step.net_amount_out.is_none())
    );
    let evidence = opportunity
        .capture_time_evidence
        .expect("capture time evidence");
    // F3: the skew gate is armed by default and measured directly; legs
    // captured together pass it cleanly instead of being "unverified".
    assert_eq!(evidence.capture_skew_seconds, 0);
    assert_eq!(evidence.max_capture_skew_seconds, Some(600));
    assert_eq!(evidence.exceeds_max_capture_skew, Some(false));
    assert!(
        !opportunity
            .risk_flags
            .contains(&ExecutionRiskFlag::CaptureSkewUnverified)
    );
    assert!(
        !opportunity
            .risk_flags
            .contains(&ExecutionRiskFlag::CaptureSkewExceeded)
    );
}

#[test]
fn multi_pair_capture_skew_evidence_exceeding_policy_blocks_execution() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let mut selected = selection(
        QuoteSelectionStrategy::Instant,
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b-skew",
                    rate: "2:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-skew",
                    rate: "2:1",
                    stock: 100,
                }],
            },
        ],
    );
    let late = Utc
        .with_ymd_and_hms(2026, 7, 30, 10, 20, 0)
        .single()
        .expect("late time");
    let late_edge = selected
        .selections
        .iter_mut()
        .find(|item| item.pair_key == "b->c")
        .expect("late pair");
    for candidate in &mut late_edge.candidate_edges {
        candidate.observation.edge.captured_at = late;
        candidate.observation.edge.confirmed_at = late;
    }
    late_edge.selected_edge = late_edge.candidate_edges.first().cloned();
    let index = MarketDepthIndex::try_from_selection(&selected, catalog.clone()).expect("index");
    let result = find_best_conversion(
        &index,
        &ConversionRequest {
            from_asset_id: asset("a"),
            to_asset_id: asset("c"),
            amount_in: amount("a", 1, &catalog),
            max_hops: 2,
            max_paths: 10,
            max_expansions: 10,
            alternative_limit: 1,
            allowed_intermediate_asset_ids: None,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let path = result.best_path.expect("path");
    let evidence = path.capture_time_evidence.expect("capture evidence");
    assert_eq!(evidence.capture_skew_seconds, 20 * 60);
    assert_eq!(evidence.max_capture_skew_seconds, Some(600));
    assert_eq!(evidence.exceeds_max_capture_skew, Some(true));
    assert!(!path.execution_eligible);
    assert!(
        path.risk_flags
            .contains(&ExecutionRiskFlag::CaptureSkewExceeded)
    );
}

#[test]
fn maker_reference_is_never_reported_as_guaranteed_depth() {
    let catalog = whole_catalog(&["a", "b"]);
    let index = index(
        QuoteSelectionStrategy::BalancedMaker,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-maker",
                rate: "2:1",
                stock: 5,
            }],
        }],
    );
    let fill = index
        .fill_pair(&amount("a", 10, &catalog), &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert_eq!(fill.gross_amount_out.quanta, 20);
    assert!(fill.is_fully_filled);
    assert!(!fill.execution_eligible);
    assert!(
        fill.risk_flags
            .contains(&ExecutionRiskFlag::FillNotGuaranteed)
    );
    assert!(
        fill.risk_flags
            .contains(&ExecutionRiskFlag::MakerDepthExceeded)
    );
}

#[test]
fn best_conversion_uses_one_engine_and_floors_after_every_leg() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b",
                    rate: "5:3",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c",
                    rate: "5:3",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "a",
                to: "c",
                levels: vec![LevelSpec {
                    id: "a-c",
                    rate: "2:1",
                    stock: 100,
                }],
            },
        ],
    );
    let result = find_best_conversion(
        &index,
        &ConversionRequest {
            from_asset_id: asset("a"),
            to_asset_id: asset("c"),
            amount_in: amount("a", 2, &catalog),
            max_hops: 2,
            max_paths: 20,
            max_expansions: 100,
            alternative_limit: 5,
            allowed_intermediate_asset_ids: None,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let best = result.best_path.expect("best");
    assert_eq!(
        best.path_asset_ids,
        vec![asset("a"), asset("b"), asset("c")]
    );
    assert_eq!(best.steps[0].gross_amount_out.quanta, 3);
    assert_eq!(best.amount_out.quanta, 5);
    assert_eq!(result.direct_path.expect("direct").amount_out.quanta, 4);
    assert_eq!(
        result.comparison.status,
        ConversionComparisonStatus::ComparableNet
    );
    assert_eq!(
        result.comparison.direction,
        Some(ComparisonDirection::Improved)
    );
}

#[test]
fn partial_direct_is_never_compared_as_if_it_used_the_full_input() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "c",
                levels: vec![LevelSpec {
                    id: "partial-direct",
                    rate: "3:1",
                    stock: 3,
                }],
            },
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "full-a-b",
                    rate: "1:1",
                    stock: 10,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "full-b-c",
                    rate: "2:1",
                    stock: 10,
                }],
            },
        ],
    );
    let result = find_best_conversion(
        &index,
        &ConversionRequest {
            from_asset_id: asset("a"),
            to_asset_id: asset("c"),
            amount_in: amount("a", 2, &catalog),
            max_hops: 2,
            max_paths: 20,
            max_expansions: 100,
            alternative_limit: 5,
            allowed_intermediate_asset_ids: None,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("conversion");
    assert!(result.best_path.as_ref().expect("best").is_fully_filled);
    assert!(!result.direct_path.as_ref().expect("direct").is_fully_filled);
    assert_eq!(
        result.comparison.status,
        ConversionComparisonStatus::IncomparableCoverage
    );
    assert!(result.comparison.delta.is_none());
}

#[test]
fn complete_route_wins_over_a_higher_output_partial_quote() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "c",
                levels: vec![LevelSpec {
                    id: "high-partial-direct",
                    rate: "10:1",
                    stock: 10,
                }],
            },
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "full-a-b-safe",
                    rate: "1:1",
                    stock: 10,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "full-b-c-safe",
                    rate: "1:1",
                    stock: 10,
                }],
            },
        ],
    );
    let result = find_best_conversion(
        &index,
        &ConversionRequest {
            from_asset_id: asset("a"),
            to_asset_id: asset("c"),
            amount_in: amount("a", 2, &catalog),
            max_hops: 2,
            max_paths: 20,
            max_expansions: 100,
            alternative_limit: 5,
            allowed_intermediate_asset_ids: None,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let best = result.best_path.expect("best");
    let direct = result.direct_path.expect("direct");
    assert!(best.is_fully_filled);
    assert_eq!(
        best.path_asset_ids,
        vec![asset("a"), asset("b"), asset("c")]
    );
    assert_eq!(best.amount_out.quanta, 2);
    assert!(!direct.is_fully_filled);
    assert_eq!(direct.amount_out.quanta, 10);
    assert_eq!(
        result.comparison.status,
        ConversionComparisonStatus::IncomparableCoverage
    );
}

#[test]
fn cancelled_search_stops_inside_the_path_loop() {
    let catalog = whole_catalog(&["a", "b"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-cancel",
                rate: "1:1",
                stock: 10,
            }],
        }],
    );
    let cancellation = SearchCancellation::default();
    cancellation.cancel();
    assert_eq!(
        find_best_conversion(
            &index,
            &ConversionRequest {
                from_asset_id: asset("a"),
                to_asset_id: asset("b"),
                amount_in: amount("a", 1, &catalog),
                max_hops: 1,
                max_paths: 5,
                max_expansions: 5,
                alternative_limit: 1,
                allowed_intermediate_asset_ids: None,
                fee_policy: FeePolicy::None,
            },
            &cancellation,
        ),
        Err(EngineError::Cancelled)
    );
}

#[test]
fn triangle_reuses_multi_level_depth_for_all_three_legs() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let two_levels = |prefix: &'static str, rate: &'static str, stock| {
        vec![
            LevelSpec {
                id: prefix,
                rate,
                stock,
            },
            LevelSpec {
                id: match prefix {
                    "a-b-1" => "a-b-2",
                    "b-c-1" => "b-c-2",
                    _ => "c-a-2",
                },
                rate,
                stock,
            },
        ]
    };
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: two_levels("a-b-1", "1:1", 5),
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: two_levels("b-c-1", "1:1", 5),
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: two_levels("c-a-1", "6:5", 6),
            },
        ],
    );
    let result = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: asset("a"),
            amount_in: amount("a", 10, &catalog),
            minimum_profit_basis_points: 100,
            max_results: 10,
            max_evaluations: 100,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("triangles");
    assert_eq!(result.opportunities.len(), 1);
    let opportunity = &result.opportunities[0];
    assert_eq!(opportunity.amount_out.quanta, 12);
    assert_eq!(opportunity.profit_basis_points, Some(2_000));
    assert_eq!(opportunity.profit_kind, Some(ProfitKind::Net));
    assert!(opportunity.execution_eligible);
    assert!(opportunity.steps.iter().all(|step| step.fills.len() == 2));
}

#[test]
fn partial_triangle_reports_residuals_and_never_claims_profit() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "small-a-b",
                    rate: "2:1",
                    stock: 2,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-partial",
                    rate: "2:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: vec![LevelSpec {
                    id: "c-a-partial",
                    rate: "2:1",
                    stock: 100,
                }],
            },
        ],
    );
    let result = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: asset("a"),
            amount_in: amount("a", 10, &catalog),
            minimum_profit_basis_points: 1,
            max_results: 10,
            max_evaluations: 10,
            fee_policy: FeePolicy::None,
        },
        &SearchCancellation::default(),
    )
    .expect("triangles");
    assert!(result.opportunities.is_empty());
    assert_eq!(result.rejections.len(), 1);
    let rejected = &result.rejections[0];
    assert_eq!(rejected.status, TriangleEvaluationStatus::PartialResidual);
    assert!(rejected.profit_amount.is_none());
    assert!(!rejected.residuals.is_empty());
}

#[test]
fn unknown_fee_triangle_is_labeled_gross_theory_not_net_execution() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b-fee",
                    rate: "1:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-fee",
                    rate: "1:1",
                    stock: 100,
                }],
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: vec![LevelSpec {
                    id: "c-a-fee",
                    rate: "11:10",
                    stock: 200,
                }],
            },
        ],
    );
    let result = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: asset("a"),
            amount_in: amount("a", 100, &catalog),
            minimum_profit_basis_points: 1,
            max_results: 10,
            max_evaluations: 10,
            fee_policy: FeePolicy::Unknown,
        },
        &SearchCancellation::default(),
    )
    .expect("triangles");
    let opportunity = result.opportunities.first().expect("gross opportunity");
    assert_eq!(opportunity.profit_kind, Some(ProfitKind::GrossTheory));
    assert!(!opportunity.execution_eligible);
    assert!(
        opportunity
            .risk_flags
            .contains(&ExecutionRiskFlag::UnknownFee)
    );
}

#[test]
fn known_per_leg_fee_can_erase_a_gross_triangle_opportunity() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b-known-fee",
                    rate: "1:1",
                    stock: 200,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-known-fee",
                    rate: "1:1",
                    stock: 200,
                }],
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: vec![LevelSpec {
                    id: "c-a-known-fee",
                    rate: "11:10",
                    stock: 200,
                }],
            },
        ],
    );
    let result = find_triangle_opportunities(
        &index,
        &TriangleRequest {
            start_asset_id: asset("a"),
            amount_in: amount("a", 100, &catalog),
            minimum_profit_basis_points: 1,
            max_results: 10,
            max_evaluations: 10,
            fee_policy: FeePolicy::OutputFraction {
                numerator: 1,
                denominator: 10,
            },
        },
        &SearchCancellation::default(),
    )
    .expect("triangles");
    assert!(result.opportunities.is_empty());
    let rejection = result.rejections.first().expect("net rejection");
    assert_eq!(rejection.status, TriangleEvaluationStatus::BelowThreshold);
    assert_eq!(rejection.profit_kind, Some(ProfitKind::Net));
    assert_eq!(rejection.profit_direction, Some(ComparisonDirection::Worse));
    assert_eq!(rejection.amount_out.quanta, 80);
}

#[test]
fn cancelled_triangle_search_stops_before_evaluation() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "a-b-cancel",
                    rate: "1:1",
                    stock: 10,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "b-c-cancel",
                    rate: "1:1",
                    stock: 10,
                }],
            },
            PairSpec {
                from: "c",
                to: "a",
                levels: vec![LevelSpec {
                    id: "c-a-cancel",
                    rate: "1:1",
                    stock: 10,
                }],
            },
        ],
    );
    let cancellation = SearchCancellation::default();
    cancellation.cancel();
    assert_eq!(
        find_triangle_opportunities(
            &index,
            &TriangleRequest {
                start_asset_id: asset("a"),
                amount_in: amount("a", 1, &catalog),
                minimum_profit_basis_points: 1,
                max_results: 10,
                max_evaluations: 10,
                fee_policy: FeePolicy::None,
            },
            &cancellation,
        ),
        Err(EngineError::Cancelled)
    );
}

#[test]
fn visibility_type_remains_independent_from_engine_inputs() {
    assert_eq!(
        DataVisibility::default(),
        DataVisibility {
            include_isolated: true,
            include_deleted: false,
        }
    );
}

/// First coverage of `allowed_intermediate_asset_ids = Some(...)` — the
/// focus-set restriction the radar leans on. The book offers a -> b -> c
/// (better) and a -> c (direct, worse). An empty allow-list prunes every
/// bridge and leaves only the direct route; allowing `b` restores the
/// better path; and the destination never needs to be on the list.
#[test]
fn an_intermediate_allow_list_prunes_bridges_but_never_the_destination() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let build = || {
        index(
            QuoteSelectionStrategy::Instant,
            catalog.clone(),
            vec![
                PairSpec {
                    from: "a",
                    to: "b",
                    levels: vec![LevelSpec {
                        id: "a-b",
                        rate: "5:3",
                        stock: 100,
                    }],
                },
                PairSpec {
                    from: "b",
                    to: "c",
                    levels: vec![LevelSpec {
                        id: "b-c",
                        rate: "5:3",
                        stock: 100,
                    }],
                },
                PairSpec {
                    from: "a",
                    to: "c",
                    levels: vec![LevelSpec {
                        id: "a-c",
                        rate: "2:1",
                        stock: 100,
                    }],
                },
            ],
        )
    };
    let request = |allowed: Option<Vec<MarketAssetId>>| ConversionRequest {
        from_asset_id: asset("a"),
        to_asset_id: asset("c"),
        amount_in: amount("a", 2, &catalog),
        max_hops: 2,
        max_paths: 20,
        max_expansions: 100,
        alternative_limit: 5,
        allowed_intermediate_asset_ids: allowed,
        fee_policy: FeePolicy::None,
    };

    // No bridges allowed: only the direct edge survives, even though the
    // bridged route pays more. The destination is reachable without being
    // on the (empty) list.
    let fenced = find_best_conversion(
        &build(),
        &request(Some(Vec::new())),
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let best = fenced.best_path.expect("best");
    assert_eq!(best.path_asset_ids, vec![asset("a"), asset("c")]);
    assert_eq!(best.amount_out.quanta, 4, "the direct route's own payout");

    // Allowing the bridge restores the better route.
    let open = find_best_conversion(
        &build(),
        &request(Some(vec![asset("b")])),
        &SearchCancellation::default(),
    )
    .expect("conversion");
    let best = open.best_path.expect("best");
    assert_eq!(
        best.path_asset_ids,
        vec![asset("a"), asset("b"), asset("c")]
    );
    assert_eq!(best.amount_out.quanta, 5);
}

/// "Too small to trade" and "never quoted" are different answers.
///
/// Whole units mean an input below one unit of the target buys nothing: at
/// eleven `a` per `b`, ten `a` floors to zero and the fill is empty. A caller
/// that only sees the empty fill cannot tell that from a pair nobody has
/// quoted — and the radar, seeing exactly that, spent weeks telling a user to
/// go capture pairs whose quotes were on file and fresh. These two answer it.
#[test]
fn the_minimum_lot_is_knowable_and_separate_from_the_pair_being_unpriced() {
    let catalog = whole_catalog(&["a", "b", "c"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog.clone(),
        vec![PairSpec {
            from: "a",
            to: "b",
            levels: vec![LevelSpec {
                id: "edge-one",
                rate: "1:11",
                stock: 500,
            }],
        }],
    );

    // Ten buys nothing; eleven buys one. The boundary is the answer.
    let below = index
        .fill_pair(&amount("a", 10, &catalog), &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert_eq!(below.gross_amount_out.quanta, 0);
    let minimum = index
        .minimum_input_for_one_unit(&asset("a"), &asset("b"), FeePolicy::None)
        .expect("minimum")
        .expect("the pair is priced, so a size exists");
    assert_eq!(minimum.quanta, 11);
    let at_minimum = index
        .fill_pair(&minimum, &asset("b"), FeePolicy::None)
        .expect("fill")
        .expect("pair");
    assert_eq!(at_minimum.gross_amount_out.quanta, 1);

    // A pair with no quotes has no minimum, which is what separates the two
    // cases: there is no size that would help, so the data really is missing.
    assert!(
        index
            .minimum_input_for_one_unit(&asset("a"), &asset("c"), FeePolicy::None)
            .expect("minimum")
            .is_none()
    );
}

/// Reachability is a question about the graph, not about the input.
///
/// The path search answers "no path" both when the book cannot get there and
/// when the caller asked to trade below a whole unit. This distinguishes them
/// without amounts, which is what lets a caller decide between "go capture
/// that pair" and "trade bigger".
#[test]
fn a_priced_chain_is_reachable_whatever_the_stake_and_a_fenced_one_is_not() {
    let catalog = whole_catalog(&["a", "b", "c", "d"]);
    let index = index(
        QuoteSelectionStrategy::Instant,
        catalog,
        vec![
            PairSpec {
                from: "a",
                to: "b",
                levels: vec![LevelSpec {
                    id: "edge-ab",
                    rate: "1:11",
                    stock: 500,
                }],
            },
            PairSpec {
                from: "b",
                to: "c",
                levels: vec![LevelSpec {
                    id: "edge-bc",
                    rate: "1:11",
                    stock: 500,
                }],
            },
        ],
    );

    assert!(index.is_priced_route(&asset("a"), &asset("b"), None, 3));
    assert!(
        index.is_priced_route(&asset("a"), &asset("c"), None, 3),
        "two priced legs chain, however small the stake would have to be"
    );
    assert!(
        !index.is_priced_route(&asset("a"), &asset("c"), Some(&[]), 3),
        "with the only intermediate fenced off there is no chain"
    );
    assert!(
        !index.is_priced_route(&asset("a"), &asset("c"), None, 1),
        "one hop cannot cross two legs"
    );
    assert!(
        !index.is_priced_route(&asset("a"), &asset("d"), None, 3),
        "nothing quotes d at all"
    );
}
