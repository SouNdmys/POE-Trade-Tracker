//! The radar: what the book already knows, ranked.
//!
//! This is the page the loop points at. Every other page answers a question
//! the user had to think of first; this one ranks what is already true, so
//! the answer arrives before the question.
//!
//! Rows are a table rather than lines because a route has seven independent
//! facts and reading them out of a sentence means reading the whole sentence.
//! The table is virtualised, which fixes row height, so a row shows its parts
//! in a panel beside the table instead of expanding in place.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Styled, Window, div, px,
};
use gpui_component::{
    Sizable, Size, StyledExt as _,
    table::{Column, Table, TableDelegate, TableState},
};
use ptt_runtime::domain::{
    Actionability, CaptureTimeEvidence, FreshnessStatus, PairFill, RadarItem,
};
use ptt_runtime::report_text;
use ptt_runtime::reports::{OpportunitiesModel, OpportunityRow, RadarScan, RadarUnavailable};
use ptt_settings::UiLanguage;
use ptt_trade_domain::MarketAssetId;

use crate::i18n;
use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, chip, chips, detail_panel, empty_state, freshness_kind,
    kv_row, mono, panel, panel_header,
};

/// Execution category → status colour.
///
/// "Executable now" is the only green: the other three are all "not yet", and
/// colouring them apart would suggest a ranking the ladder does not have.
#[must_use]
pub fn actionability_kind(category: Actionability) -> StatusKind {
    match category {
        Actionability::InstantExecutable => StatusKind::Monitoring,
        Actionability::MakerTheoretical | Actionability::ProbeRequired => StatusKind::Warning,
        Actionability::SuspiciousOutlier => StatusKind::Error,
    }
}

/// The route, as the game names it.
#[must_use]
pub fn route_text(
    catalog: &ptt_runtime::domain::Catalog,
    language: UiLanguage,
    path: &[MarketAssetId],
) -> String {
    crate::names::route_name(catalog, language, path)
}

/// The profit column: a signed percentage, or the fact that there is none.
#[must_use]
pub fn edge_text(item: &RadarItem, language: UiLanguage) -> (String, u32) {
    item.value_basis_points.map_or_else(
        || (report_text::report(language).unpriced.to_owned(), TEXT_META),
        |points| {
            (
                format!("{}.{:02}%", points / 100, (points % 100).abs()),
                if points >= 0 { ACCENT_TEXT } else { DANGER },
            )
        },
    )
}

/// The radar's rows.
///
/// Public so the gallery can drive the same delegate against synthetic data:
/// a rehearsal against a different table would rehearse the wrong thing.
pub struct RadarTable {
    columns: Vec<Column>,
    rows: Vec<OpportunityRow>,
    language: UiLanguage,
    /// Held rather than looked up: the delegate renders currency names and
    /// has no route back to the shell's settings.
    catalog: &'static ptt_runtime::domain::Catalog,
}

impl RadarTable {
    #[must_use]
    pub fn new(
        rows: Vec<OpportunityRow>,
        language: UiLanguage,
        catalog: &'static ptt_runtime::domain::Catalog,
    ) -> Self {
        let chrome = i18n::text(language);
        Self {
            columns: vec![
                Column::new("kind", chrome.radar_column_kind).width(px(110.)),
                Column::new("route", chrome.radar_column_route).width(px(300.)),
                Column::new("edge", chrome.radar_column_edge)
                    .width(px(90.))
                    .text_right()
                    .sortable(),
                // Depth before amount: it is what the list is ordered by, and
                // an order the reader cannot see is one they will read as
                // wrong. The amount stays beside it because it is what the
                // trade actually produces at that depth.
                Column::new("depth", chrome.radar_column_depth)
                    .width(px(110.))
                    .text_right(),
                Column::new("out", chrome.radar_column_out)
                    .width(px(140.))
                    .text_right(),
                Column::new("verdict", chrome.radar_column_verdict).width(px(150.)),
                Column::new("light", chrome.radar_column_light).width(px(110.)),
                Column::new("risks", chrome.radar_column_risks).width(px(240.)),
            ],
            rows,
            language,
            catalog,
        }
    }

    /// Replaces the rows in place.
    ///
    /// The table owns the scroll position and the selection, so rebuilding it
    /// on every accepted book would throw the reader back to the top of the
    /// list every few seconds.
    pub fn set_rows(
        &mut self,
        rows: Vec<OpportunityRow>,
        language: UiLanguage,
        catalog: &'static ptt_runtime::domain::Catalog,
    ) {
        // The catalogue changes with the game, and both it and the language
        // are baked into the built columns, so either one moving is a rebuild.
        if language != self.language || !std::ptr::eq(catalog, self.catalog) {
            *self = Self::new(rows, language, catalog);
            return;
        }
        self.rows = rows;
    }

    #[must_use]
    pub fn row(&self, index: usize) -> Option<&OpportunityRow> {
        self.rows.get(index)
    }
}

impl TableDelegate for RadarTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> &Column {
        &self.columns[col_ix]
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.render_cell(row_ix, col_ix, window, cx)
    }
}

impl RadarTable {
    /// One cell.
    ///
    /// Outside the trait so a wrapping delegate can call it: `render_td`
    /// receives a context typed to whichever delegate the table holds, and a
    /// wrapper has no way to produce one for the delegate it wraps.
    pub fn render_cell(
        &self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> gpui::AnyElement {
        let language = self.language;
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let item = &row.item;
        let cell = |body: gpui::Div| body.h_flex().items_center().size_full();

        match col_ix {
            0 => cell(div().text_size(fs(FS_11_5)))
                .child(report_text::radar_item_kind(language, item.kind))
                .into_any_element(),
            1 => cell(
                mono(route_text(self.catalog, language, &item.path_asset_ids))
                    .text_size(fs(FS_11_5))
                    .whitespace_nowrap()
                    .overflow_hidden(),
            )
            .into_any_element(),
            2 => {
                let (label, colour) = edge_text(item, language);
                cell(
                    mono(label)
                        .text_size(fs(FS_11_5))
                        .text_color(c(colour))
                        .justify_end(),
                )
                .into_any_element()
            }
            // Depth, in the settlement anchor -- one currency for every row,
            // because a column that is sorted on has to be comparable down
            // its own length.
            3 => cell(
                mono(
                    item.liquidity_capacity
                        .map_or_else(|| "-".to_owned(), |capacity| capacity.to_string()),
                )
                .text_size(fs(FS_11_5))
                .justify_end(),
            )
            .into_any_element(),
            4 => cell(
                mono(format!(
                    "{} {}",
                    item.amount_out.quanta,
                    item.path_asset_ids.last().map_or_else(
                        || "?".to_owned(),
                        |asset| crate::names::asset_name(self.catalog, language, asset.as_str())
                    )
                ))
                .text_size(fs(FS_11_5))
                .justify_end(),
            )
            .into_any_element(),
            5 => cell(div())
                .child(chip(
                    actionability_kind(item.category),
                    report_text::actionability(language, item.category),
                ))
                .into_any_element(),
            6 => match row.light {
                Some(status) => cell(div().gap_2())
                    .child(crate::ui::status_dot(freshness_kind(status)))
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_SECONDARY))
                            .child(report_text::freshness_light(language, status)),
                    )
                    .into_any_element(),
                None => cell(div().text_size(fs(FS_10_5)).text_color(c(TEXT_DISABLED)))
                    .child("—")
                    .into_any_element(),
            },
            _ => cell(div())
                .child(chips(
                    StatusKind::Warning,
                    &item
                        .blocking_risks
                        .iter()
                        .map(|risk| report_text::execution_risk(language, *risk).to_owned())
                        .collect::<Vec<_>>(),
                    2,
                ))
                .into_any_element(),
        }
    }
}

impl AppShell {
    /// Creates the radar's table once, so it keeps its scroll and selection
    /// across every refresh.
    pub(crate) fn new_radar_table(
        window: &mut Window,
        cx: &mut Context<Self>,
        language: UiLanguage,
        catalog: &'static ptt_runtime::domain::Catalog,
    ) -> Entity<TableState<RadarTable>> {
        cx.new(|cx| {
            TableState::new(RadarTable::new(Vec::new(), language, catalog), window, cx)
                .row_selectable(true)
                .col_selectable(false)
        })
    }

    /// Pushes the newest scan into the live table.
    pub(crate) fn sync_radar_table(&mut self, cx: &mut Context<Self>) {
        let PageData::Opportunities(model) = &self.report else {
            return;
        };
        let rows = match &model.scan {
            RadarScan::Ran(scan) => scan.items.clone(),
            RadarScan::Unavailable(_) => Vec::new(),
        };
        let language = self.language();
        let catalog = self.catalog();
        let table = self.radar_table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().set_rows(rows, language, catalog);
            state.refresh(cx);
        });
    }

    /// The radar page.
    pub(crate) fn render_opportunities(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let report = self.text();

        let PageData::Opportunities(model) = &self.report else {
            return div().flex_grow().flex().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: &OpportunitiesModel = model;

        // A scan that could not run says why, once, instead of drawing an
        // empty table that looks like an answer.
        if let RadarScan::Unavailable(reason) = &model.scan {
            let message = match reason {
                RadarUnavailable::NoCoreCurrency => report_text::report(language).no_core_currency,
                RadarUnavailable::NotEnoughMarket => {
                    report_text::report(language).not_enough_market
                }
                RadarUnavailable::CannotStake { .. } => report.no_pair_yet,
            };
            return div().flex_grow().flex().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(message)),
            );
        }
        let RadarScan::Ran(scan) = &model.scan else {
            unreachable!("the unavailable case returned above");
        };

        let selected = self
            .radar_table
            .read(cx)
            .selected_row()
            .and_then(|index| self.radar_table.read(cx).delegate().row(index).cloned());

        let mut header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .children(model.notes.iter().map(|note| {
                mono(note.clone())
                    .text_size(fs(FS_11_5))
                    .text_color(c(WARN_TEXT))
            }))
            .child(
                mono(report_text::fill(
                    report_text::report(language).staking,
                    &[
                        &scan.stake.to_string(),
                        &scan
                            .starts
                            .iter()
                            .map(MarketAssetId::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        &scan.diagnostics.target_count.to_string(),
                    ],
                ))
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_META)),
            );
        // What the scan actually did, so an empty list is distinguishable
        // from a scan that never ran: "no opportunities across 40 priced
        // conversions" is an answer, "no opportunities" alone is a shrug.
        header = header.child(
            mono(report_text::fill(
                report_text::report(language).scan_accounting,
                &[
                    &scan.diagnostics.scanned_conversion_count.to_string(),
                    &scan.diagnostics.complete_conversion_count.to_string(),
                    &scan.diagnostics.unfillable_conversion_count.to_string(),
                    &scan.diagnostics.missing_conversion_count.to_string(),
                    &scan.diagnostics.triangle_evaluation_count.to_string(),
                    &scan.diagnostics.profitable_loop_count.to_string(),
                    &self
                        .settings_tuning()
                        .radar
                        .minimum_profit_basis_points
                        .to_string(),
                ],
            ))
            .text_size(fs(FS_11_5))
            .text_color(c(TEXT_META)),
        );
        // Said above the results, not below: a truncated search that looks
        // complete is how "there is nothing better" gets believed.
        if scan.diagnostics.budget_exhausted || scan.diagnostics.results_truncated {
            header = header.child(
                mono(report_text::fill(
                    report_text::report(language).partial_scan,
                    &[
                        &scan.diagnostics.skipped_target_count.to_string(),
                        &scan.diagnostics.expansions_used.to_string(),
                        if scan.diagnostics.results_truncated {
                            report_text::report(language).results_cut
                        } else {
                            ""
                        },
                    ],
                ))
                .text_size(fs(FS_11_5))
                .text_color(c(WARN_TEXT)),
            );
        }

        let table = if scan.items.is_empty() {
            panel()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(panel_header(text.page_opportunities))
                .child(header)
                .child(empty_state(
                    report_text::report(language).nothing_beats_holding,
                ))
        } else {
            panel()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(panel_header(text.page_opportunities))
                .child(header)
                .child(
                    div().flex_1().overflow_hidden().child(
                        Table::new(&self.radar_table)
                            .stripe(true)
                            .bordered(false)
                            .with_size(Size::XSmall),
                    ),
                )
        };

        let mut body = div().flex_grow().flex().gap_3().p_3().child(table);
        if let Some(row) = selected {
            body = body.child(self.radar_detail(&row, cx));
        }
        div()
            .flex_grow()
            .flex()
            .flex_col()
            .child(body)
            .child(self.radar_probes(scan.probe_candidates.clone(), cx))
    }

    /// The parts of the selected route.
    fn radar_detail(&self, row: &OpportunityRow, cx: &mut Context<Self>) -> gpui::Div {
        let language = self.language();
        let text = self.text();
        let item = &row.item;

        let steps: &[PairFill] = item
            .conversion_path
            .as_ref()
            .map(|path| path.steps.as_slice())
            .or_else(|| {
                item.triangle
                    .as_ref()
                    .map(|triangle| triangle.steps.as_slice())
            })
            .unwrap_or_default();
        let evidence: Option<&CaptureTimeEvidence> = item
            .conversion_path
            .as_ref()
            .and_then(|path| path.capture_time_evidence.as_ref())
            .or_else(|| {
                item.triangle
                    .as_ref()
                    .and_then(|triangle| triangle.capture_time_evidence.as_ref())
            });

        let mut inner = div()
            .p_3()
            .flex()
            .flex_col()
            .child(kv_row(
                text.detail_route,
                &route_text(self.catalog(), language, &item.path_asset_ids),
            ))
            .child(kv_row(
                text.detail_stake,
                &format!("{} {}", item.amount_in.quanta, item.amount_in.asset_id),
            ))
            .child(kv_row(
                text.detail_return,
                &format!(
                    "{} {}",
                    item.amount_out.quanta,
                    self.display_name(item.amount_out.asset_id.as_str())
                ),
            ));

        // Per leg, because a route is only as good as the leg that fails.
        for (index, step) in steps.iter().enumerate() {
            inner = inner.child(kv_row(
                &crate::i18n::leg_label(language, index + 1),
                &format!(
                    "{}   {} → {}",
                    self.pair_label(step.from_asset_id.as_str(), step.to_asset_id.as_str()),
                    step.consumed_input.quanta,
                    step.gross_amount_out.quanta,
                ),
            ));
        }

        if let Some(evidence) = evidence {
            inner = inner.child(kv_row(
                text.detail_capture,
                &format!(
                    "{} · {}s",
                    report_text::freshness_light(
                        language,
                        row.light.unwrap_or(FreshnessStatus::Archived)
                    ),
                    evidence.capture_skew_seconds,
                ),
            ));
        }
        if !item.blocking_risks.is_empty() {
            inner = inner.child(kv_row(
                text.detail_risks,
                &item
                    .blocking_risks
                    .iter()
                    .map(|risk| report_text::execution_risk(language, *risk))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        if !item.reasons.is_empty() {
            inner = inner.child(kv_row(
                text.detail_reasons,
                &item
                    .reasons
                    .iter()
                    .map(|reason| report_text::radar_reason(language, *reason))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        // A route that needs confirming is the one worth queueing, so the
        // control sits on the verdict that says so.
        if item.category != Actionability::InstantExecutable
            && let (Some(from), Some(to)) =
                (item.path_asset_ids.first(), item.path_asset_ids.get(1))
        {
            let (from, to) = (from.as_str().to_owned(), to.as_str().to_owned());
            let reason = report_text::actionability(language, item.category).to_owned();
            let pinned = self.probe_queue.is_pinned(&from, &to);
            inner = inner.child(div().pt_2().child(if pinned {
                button("radar-unpin", LedgerButton::Quiet, text.unpin_label, cx).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.unpin_probe(&from, &to);
                        cx.notify();
                    }),
                )
            } else {
                button("radar-pin", LedgerButton::Secondary, text.pin_label, cx).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.pin_probe(&from, &to, &reason);
                        cx.notify();
                    }),
                )
            }));
        }

        detail_panel(text.detail_header).child(inner)
    }

    /// The pairs whose absence or staleness limited what the scan could claim.
    ///
    /// Shown whether or not anything survived, because an empty page is
    /// exactly when "go and flip this" matters most.
    fn radar_probes(
        &self,
        candidates: Vec<ptt_runtime::domain::ProbeCandidate>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if candidates.is_empty() {
            return div();
        }
        let language = self.language();
        let text = self.text();
        let mut row = div().h_flex().items_center().gap_2().flex_wrap();
        for (index, candidate) in candidates.into_iter().take(4).enumerate() {
            let from = candidate.from_asset_id.as_str().to_owned();
            let to = candidate.to_asset_id.as_str().to_owned();
            let reason = report_text::probe_reason(language, candidate.reason).to_owned();
            let pinned = self.probe_queue.is_pinned(&from, &to);
            row = row.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(mono(self.pair_label(&from, &to)).text_size(fs(FS_11_5)))
                    .child(
                        mono(reason.clone())
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META)),
                    )
                    .child(if pinned {
                        chip(StatusKind::Monitoring, text.pinned_label)
                    } else {
                        div().child(
                            button(
                                ("radar-probe-pin", index),
                                LedgerButton::Quiet,
                                text.pin_label,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.pin_probe(&from, &to, &reason);
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            );
        }
        panel()
            .flex_none()
            .mx_3()
            .mb_3()
            .child(panel_header(
                report_text::report(language).radar_probe_header,
            ))
            .child(div().p_3().child(row))
    }
}
