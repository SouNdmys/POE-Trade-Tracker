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
    input::Input,
    table::{Column, Table, TableDelegate, TableState},
};
use ptt_runtime::domain::{
    Actionability, CaptureTimeEvidence, FreshnessStatus, RadarItem, RadarItemKind,
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
    LedgerButton, StatusKind, button, chip, chips_capped, detail_panel, empty_state,
    freshness_kind, kv_row, mono, panel, panel_header,
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

/// The profit column: what closing this route nets, signed, or the fact that
/// the book prices no way home.
///
/// The round trip and not `value_basis_points`, because that one means two
/// different things on the two row kinds -- a cycle's own edge, but a
/// conversion's margin over its pair's direct trade -- and they are nowhere
/// near the same size. The comparison against direct is still worth having
/// and is on the detail panel.
#[must_use]
pub fn edge_text(item: &RadarItem, language: UiLanguage) -> (String, u32) {
    item.round_trip_basis_points.map_or_else(
        || (report_text::report(language).unpriced.to_owned(), TEXT_META),
        |points| {
            (
                report_text::percent_from_basis_points(points),
                if points >= 0 { ACCENT_TEXT } else { DANGER },
            )
        },
    )
}

/// Every column's width, added up, may not exceed this.
///
/// Upstream columns are pixel-only — no flex, no auto, no measuring the
/// content — so these eight numbers *are* the layout, and nothing downstream
/// will reflow them to fit a narrower window. One fixed ceiling is what keeps
/// the two demands on this table from cancelling each other out: the route
/// column wants to grow until a whole route reads, and a window a little over
/// 1100px wide wants the whole table to fit inside it. Without a shared
/// budget, satisfying either one silently undoes the other.
pub const RADAR_TABLE_WIDTH_BUDGET: f32 = 1090.0;

// The seven columns that never move. They are narrower than they were: the
// verdict, freshness and risk cells hold badges that overflow any width this
// table can afford, and their full text is one click away in the detail
// panel, so the pixels are better spent on the column that says *which route
// this row is*.
const COL_KIND_WIDTH: f32 = 80.0;
const COL_EDGE_WIDTH: f32 = 80.0;
const COL_DEPTH_WIDTH: f32 = 90.0;
const COL_RATE_WIDTH: f32 = 120.0;
const COL_VERDICT_WIDTH: f32 = 120.0;
const COL_LIGHT_WIDTH: f32 = 90.0;
const COL_RISKS_WIDTH: f32 = 120.0;

/// What the seven fixed columns cost together.
const RADAR_FIXED_COLUMNS_WIDTH: f32 = COL_KIND_WIDTH
    + COL_EDGE_WIDTH
    + COL_DEPTH_WIDTH
    + COL_RATE_WIDTH
    + COL_VERDICT_WIDTH
    + COL_LIGHT_WIDTH
    + COL_RISKS_WIDTH;

/// Where the route column lives in `columns`.
const ROUTE_COLUMN: usize = 1;

/// The route column's floor: the width it had before it could grow at all.
const COL_ROUTE_MIN_WIDTH: f32 = 300.0;

/// The route column's ceiling: whatever the budget has left over.
const COL_ROUTE_MAX_WIDTH: f32 = RADAR_TABLE_WIDTH_BUDGET - RADAR_FIXED_COLUMNS_WIDTH;

/// Widening a fixed column past the budget has to fail here, not on screen.
///
/// The ceiling above is subtraction, so seven fixed columns that outgrow the
/// budget do not stop at zero — they push the ceiling under the floor, and
/// `route_column_width` clamps into `[floor, ceiling]`. `f32::clamp` panics
/// when its bounds are the wrong way round, so a one-number edit that looks
/// like styling would take the whole radar page down at the first row it
/// draws. Asserting it in a `const` moves the failure to the build, where
/// the message can say which number to change.
const _: () = assert!(
    COL_ROUTE_MIN_WIDTH <= COL_ROUTE_MAX_WIDTH,
    "the fixed radar columns no longer leave the route column its floor: \
     narrow one of them, or raise RADAR_TABLE_WIDTH_BUDGET (and check the \
     table still fits a ~1100px window)"
);

/// How much longer a route has to read before the column actually widens.
///
/// The row set is replaced on every accepted book, so a column that re-fits
/// itself exactly would shift the whole table sideways every few seconds.
/// A reader tracking a row down the list would rather have it truncated in a
/// stable place than perfectly sized in a moving one.
const COL_ROUTE_GROWTH_STEP: f32 = 24.0;

/// A cell's left plus right padding at [`Size::XSmall`], which is the size
/// this table is drawn at.
const CELL_PADDING: f32 = 8.0;

/// How wide a string draws in the table's monospace font.
///
/// gpui can measure text exactly, but only inside a layout pass, and a column
/// width has to exist before the first layout pass runs — so this is an
/// estimate by arithmetic instead. The font is monospace, which makes it a
/// sum of per-character advances: a CJK glyph takes a full em, everything
/// else about six tenths of one. Being a few pixels out is harmless in both
/// directions, because the answer is clamped into a range either way.
fn monospace_width(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|character| {
            if character.is_ascii() {
                font_size * 0.6
            } else {
                font_size
            }
        })
        .sum()
}

/// The width the route column would like, given the routes it has to show.
///
/// Clamped rather than granted: below the floor the column stops being worth
/// reading, and above the ceiling the table stops fitting the window.
fn route_column_width(
    rows: &[OpportunityRow],
    language: UiLanguage,
    catalog: &ptt_runtime::domain::Catalog,
) -> f32 {
    let widest = rows
        .iter()
        .map(|row| {
            monospace_width(
                &route_text(catalog, language, &row.item.path_asset_ids),
                FS_11_5,
            )
        })
        .fold(0.0_f32, f32::max);
    (widest + CELL_PADDING).clamp(COL_ROUTE_MIN_WIDTH, COL_ROUTE_MAX_WIDTH)
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
    /// Set once the reader has dragged a column edge, which retires the
    /// automatic fit: a width someone chose on purpose outranks one measured
    /// off whatever routes this scan happened to find.
    widths_are_the_readers: bool,
}

impl RadarTable {
    #[must_use]
    pub fn new(
        rows: Vec<OpportunityRow>,
        language: UiLanguage,
        catalog: &'static ptt_runtime::domain::Catalog,
    ) -> Self {
        let chrome = i18n::text(language);
        let route = route_column_width(&rows, language, catalog);
        Self {
            columns: vec![
                Column::new("kind", chrome.radar_column_kind).width(px(COL_KIND_WIDTH)),
                Column::new("route", chrome.radar_column_route).width(px(route)),
                Column::new("edge", chrome.radar_column_edge)
                    .width(px(COL_EDGE_WIDTH))
                    .text_right()
                    .sortable(),
                // Depth before rate: depth is what the list is ordered by, and
                // an order the reader cannot see is one they will read as
                // wrong. The rate sits beside it because a depth is only worth
                // reading next to the price it holds at.
                Column::new("depth", chrome.radar_column_depth)
                    .width(px(COL_DEPTH_WIDTH))
                    .text_right(),
                Column::new("rate", chrome.radar_column_rate)
                    .width(px(COL_RATE_WIDTH))
                    .text_right(),
                Column::new("verdict", chrome.radar_column_verdict).width(px(COL_VERDICT_WIDTH)),
                Column::new("light", chrome.radar_column_light).width(px(COL_LIGHT_WIDTH)),
                Column::new("risks", chrome.radar_column_risks).width(px(COL_RISKS_WIDTH)),
            ],
            rows,
            language,
            catalog,
            widths_are_the_readers: false,
        }
    }

    /// Replaces the rows in place, reporting whether the columns moved with
    /// them.
    ///
    /// The table owns the scroll position and the selection, so rebuilding it
    /// on every accepted book would throw the reader back to the top of the
    /// list every few seconds.
    ///
    /// The answer matters because refreshing a table clears every column's
    /// measured bounds for a frame, so a caller that refreshes on every scan
    /// pays a blank frame for a layout that did not change.
    pub fn set_rows(
        &mut self,
        rows: Vec<OpportunityRow>,
        language: UiLanguage,
        catalog: &'static ptt_runtime::domain::Catalog,
    ) -> bool {
        // The catalogue changes with the game, and both it and the language
        // are baked into the built columns, so either one moving is a rebuild.
        if language != self.language || !std::ptr::eq(catalog, self.catalog) {
            *self = Self::new(rows, language, catalog);
            return true;
        }
        let widened = self.fit_route_column(&rows);
        self.rows = rows;
        widened
    }

    /// Widens the route column when a scan brings in a longer route.
    ///
    /// Grow-only, and only in steps worth seeing. A scan replaces the rows
    /// every few seconds, and a column that tracked the longest route exactly
    /// would nudge the seven columns after it sideways each time — a table
    /// that will not hold still is harder to read than one that truncates.
    fn fit_route_column(&mut self, rows: &[OpportunityRow]) -> bool {
        if self.widths_are_the_readers {
            return false;
        }
        let wanted = route_column_width(rows, self.language, self.catalog);
        let column = &mut self.columns[ROUTE_COLUMN];
        if wanted <= f32::from(column.width) + COL_ROUTE_GROWTH_STEP {
            return false;
        }
        column.width = px(wanted);
        true
    }

    /// Takes the widths back from a column the reader dragged.
    ///
    /// Without this the drag is undone by the next scan: a refresh rebuilds
    /// the table's own column state straight out of these `Column` values, so
    /// a width that only ever lived in the table is lost the moment the rows
    /// change.
    pub fn set_column_widths(&mut self, widths: &[gpui::Pixels]) {
        for (column, width) in self.columns.iter_mut().zip(widths) {
            column.width = *width;
        }
        self.widths_are_the_readers = true;
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
            // The composed front rate where the walked payout used to be:
            // the scan runs at a canonical size nobody holds, so its output
            // said nothing about the reader — the rate is what they act on,
            // and the detail panel prices their own ask at this same rate.
            4 => cell(
                mono(
                    ptt_runtime::reports::walk_route(&row.leg_books, 1)
                        .rate
                        .map_or_else(|| "-".to_owned(), |rate| rate.text()),
                )
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
            // Capped and silent about it: what did not fit is in the detail
            // panel, which is where a reader who cares is going anyway.
            _ => cell(div())
                .child(chips_capped(
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

/// What makes a row *this route* and not whatever ends up at its index next
/// time.
///
/// The kind plus the asset sequence: those two come from the market, so two
/// scans that both find the chaos → exalt → divine conversion agree on them
/// no matter how the ranking came out. `RadarItem::item_id` looks like the
/// obvious key and is not one — it is built as `conversion-{n}-…` where `n`
/// is the row's push position within its own scan, so a route only keeps its
/// id for as long as every route ahead of it also survives.
fn route_identity(row: &OpportunityRow) -> (ptt_runtime::domain::RadarItemKind, &[MarketAssetId]) {
    (row.item.kind, &row.item.path_asset_ids)
}

/// Where the selected route ends up in a freshly scanned row set.
///
/// The table remembers the selection as an index while a scan replaces every
/// row underneath it, so an index nobody re-points at the route it was chosen
/// for quietly starts describing a different route. `None` when this scan did
/// not find that route at all: showing nothing is honest, showing whichever
/// route inherited the index is not.
fn remap_selection(
    selected: Option<usize>,
    old_rows: &[OpportunityRow],
    new_rows: &[OpportunityRow],
) -> Option<usize> {
    let identity = route_identity(old_rows.get(selected?)?);
    new_rows
        .iter()
        .position(|row| route_identity(row) == identity)
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
            // Worked out before the rows move, because the outgoing row set is
            // the only place that still says which route the index meant.
            let selected = state.selected_row();
            let remapped = remap_selection(selected, &state.delegate().rows, &rows);
            // Refreshing is how a changed column reaches the table, and also
            // how every column loses its measured bounds for a frame — cells
            // laid out against a zero width draw as a blank table until some
            // later repaint happens to put the bounds back. A scan that only
            // changed the rows does not need to pay that, so it just asks for
            // a repaint.
            if state.delegate_mut().set_rows(rows, language, catalog) {
                state.refresh(cx);
            } else {
                cx.notify();
            }
            match remapped {
                // Only when it actually moved: upstream `set_selected_row`
                // scrolls the row into view, and a scan that left the route
                // exactly where it was has no business yanking the list.
                Some(index) if Some(index) != selected => state.set_selected_row(index, cx),
                None if selected.is_some() => state.clear_selection(cx),
                _ => {}
            }
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
                RadarUnavailable::NoStartUnits { .. } => report.no_pair_yet,
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
                    report_text::report(language).scanning_from,
                    &[
                        // The catalogue's names, like every other currency on
                        // this page. The raw ids belong to the text report,
                        // which is read beside the database; printing them
                        // here put English into the middle of a Chinese
                        // sentence and named the scan's anchors differently
                        // from every row under them.
                        &scan
                            .starts
                            .iter()
                            .map(|asset| self.display_name(asset.as_str()))
                            .collect::<Vec<_>>()
                            .join(", "),
                        // Distinct targets, not the (start, target) pairs the budget
                        // is divided over: two settlement starts search every target
                        // twice, so the pair count read as twice the scope.
                        &scan.diagnostics.distinct_target_count.to_string(),
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
                    &report_text::percent_from_basis_points(
                        i64::try_from(self.settings_tuning().radar.minimum_profit_basis_points)
                            .unwrap_or(i64::MAX),
                    ),
                ],
            ))
            .text_size(fs(FS_11_5))
            .text_color(c(TEXT_META)),
        );
        // Two different facts, and they used to share one amber sentence: a
        // scan that ran out of budget may have missed something, while a list
        // cut to its page limit missed nothing at all. Together they printed
        // "扫描不完整 — 跳过 0 个目标" on the routine case, which reads as
        // broken and teaches the reader to ignore the warning that matters.
        //
        // Said above the results either way: a truncated search that looks
        // complete is how "there is nothing better" gets believed.
        if scan.diagnostics.budget_exhausted {
            header = header.child(
                mono(report_text::fill(
                    report_text::report(language).partial_scan,
                    &[
                        &scan.diagnostics.skipped_target_count.to_string(),
                        &scan.diagnostics.expansions_used.to_string(),
                    ],
                ))
                .text_size(fs(FS_11_5))
                .text_color(c(WARN_TEXT)),
            );
        }
        // Not amber: nothing was missed, the page is just a page.
        if scan.diagnostics.results_truncated {
            header = header.child(
                mono(report_text::fill(
                    report_text::report(language).results_cut,
                    &[
                        &scan.diagnostics.item_count_before_limit.to_string(),
                        &scan.items.len().to_string(),
                    ],
                ))
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_META)),
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

        // `min_w(0)` on both wrappers, for the same reason `min_h(0)` runs all
        // the way down the shell: a flex item's automatic minimum size is its
        // content, and this page's content is a fixed-width table beside a
        // fixed-width detail panel. Without it a window narrower than the two
        // of them together does not clip — it grows the page past the window
        // edge and takes the header and the probe strip with it.
        let mut body = div()
            .flex_grow()
            .min_w(px(0.))
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(table);
        if let Some(row) = selected {
            body = body.child(self.radar_detail(&row, cx));
        }
        div()
            .flex_grow()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .child(body)
            .child(self.radar_probes(scan.probe_candidates.clone(), cx))
    }

    /// One structural note as compact text: name, class, trend, greedy fit.
    /// Shared with the Convert page's greedy context line.
    pub(crate) fn structural_text(&self, note: &ptt_runtime::reports::StructuralNote) -> String {
        let language = self.language();
        let report = report_text::report(language);
        let mut parts = vec![format!(
            "{} {}",
            self.display_name(note.asset_id.as_str()),
            report_text::liquidity_class(language, note.class),
        )];
        if let Some(verdict) = note.verdict {
            parts.push(report_text::trend_verdict(language, verdict).to_owned());
        }
        if note.greedy_candidate {
            parts.push(report.analytics_marker_greedy.to_owned());
        }
        parts.join("·")
    }

    /// The parts of the selected route.
    /// The amount typed into the detail panel's box, when it holds a number.
    fn walk_amount(&self, cx: &gpui::App) -> Option<u64> {
        self.walk_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|count| *count > 0)
    }

    fn radar_detail(&self, row: &OpportunityRow, cx: &mut Context<Self>) -> gpui::Div {
        let language = self.language();
        let text = self.text();
        let item = &row.item;

        let evidence: Option<&CaptureTimeEvidence> = item
            .conversion_path
            .as_ref()
            .and_then(|path| path.capture_time_evidence.as_ref())
            .or_else(|| {
                item.triangle
                    .as_ref()
                    .and_then(|triangle| triangle.capture_time_evidence.as_ref())
            });

        let mut inner = div().p_3().flex().flex_col().child(kv_row(
            text.detail_route,
            &route_text(self.catalog(), language, &item.path_asset_ids),
        ));

        // The headline column states what closing the route nets. This is the
        // other number the route has: how much better it is than simply
        // trading the pair direct. A saving on a purchase rather than a
        // profit, so it sits here rather than on the row -- but on the owner's
        // book the two were 17.53% and 2.09% for the same route, and reading
        // the first as the second is the mistake this panel exists to stop.
        if let Some(points) = item.value_basis_points {
            inner = inner.child(kv_row(
                text.detail_versus_direct,
                &report_text::percent_from_basis_points(points),
            ));
        }

        // Per leg the front rate, because a route is only as good as the leg
        // that fails. The consumed→produced amounts that used to sit here
        // described the scan's canonical-size walk — numbers about nobody.
        // The reader's own numbers come from the walk box below.
        for (index, leg) in row.leg_books.iter().enumerate() {
            inner = inner.child(kv_row(
                &crate::i18n::leg_label(language, index + 1),
                &format!(
                    "{}   {}",
                    self.pair_label(leg.from_asset_id.as_str(), leg.to_asset_id.as_str()),
                    leg.rate.as_ref().map_or_else(
                        || "-".to_owned(),
                        |rate| ptt_runtime::reports::RouteRate {
                            numerator: u128::from(rate.numerator),
                            denominator: u128::from(rate.denominator),
                        }
                        .text()
                    ),
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
        // Season-scale context per leg asset: advisory, never a blocker and
        // never a sort key (the user's ordering ruling stands).
        if !row.structural.is_empty() {
            let notes = row
                .structural
                .iter()
                .map(|note| self.structural_text(note))
                .collect::<Vec<_>>()
                .join("  ");
            inner = inner.child(kv_row(text.detail_structural, &notes));
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

        // The bridge (user ruling): the radar found this rate without ever
        // asking what the reader holds — this box is where they bring the
        // size. Priced at draw time from the row's saved leg books, so typing
        // re-answers instantly without rebuilding the page.
        inner = inner.child(
            div()
                .pt_2()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(text.detail_walk),
                )
                .child(
                    div()
                        .w(px(110.))
                        .child(Input::new(&self.walk_input).with_size(Size::Small)),
                ),
        );
        if let Some(amount) = self.walk_amount(cx) {
            let walk = ptt_runtime::reports::walk_route(&row.leg_books, amount);
            if let Some(rate) = walk.rate {
                inner = inner.child(kv_row(text.detail_walk_rate, &rate.text()));
            }
            if let Some(out) = walk.projected_output {
                let mut projected = format!("{amount} → {out}");
                // A loop hands back the currency it started from, so the
                // difference is the whole answer and it rides on the row.
                if item.kind == RadarItemKind::Loop {
                    if out >= amount {
                        projected.push_str(&format!("  (+{})", out - amount));
                    } else {
                        projected.push_str(&format!("  (-{})", amount - out));
                    }
                }
                inner = inner.child(kv_row(text.detail_walk_out, &projected));
            }
            if let Some(fillable) = walk.fillable_input {
                let start = item
                    .path_asset_ids
                    .first()
                    .map_or_else(String::new, |asset| self.display_name(asset.as_str()));
                let depth = report_text::fill(
                    report_text::report(language).route_front_depth,
                    &[&fillable.to_string(), &start],
                );
                let value = if fillable < amount {
                    report_text::join_text(
                        language,
                        &[
                            depth.as_str(),
                            report_text::report(language).route_front_short,
                        ],
                    )
                } else {
                    depth
                };
                inner = inner.child(kv_row(text.detail_walk_depth, &value));
            }
            if let Some(leg) = walk.pinch() {
                let facts = report_text::leg_take_facts(
                    language,
                    &self.display_name(leg.from_asset_id.as_str()),
                    &self.display_name(leg.to_asset_id.as_str()),
                    leg,
                );
                let notes = report_text::leg_take_notes(language, leg);
                let value = if notes.is_empty() {
                    facts
                } else {
                    format!("{facts}   {}", report_text::join_text(language, &notes))
                };
                inner = inner.child(kv_row(text.detail_walk_pinch, &value));
            }
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

#[cfg(test)]
mod selection_tests {
    use super::*;
    use ptt_runtime::domain::{AssetAmount, AssetUnit, RadarItemKind};

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn amount(id: &str) -> AssetAmount {
        AssetAmount {
            asset_id: asset(id),
            quanta: 10,
            unit: AssetUnit::whole(),
        }
    }

    /// A row that is nothing but its route, because that is all the remap is
    /// allowed to look at.
    fn row(path: &[&str]) -> OpportunityRow {
        let path: Vec<MarketAssetId> = path.iter().map(|id| asset(id)).collect();
        OpportunityRow {
            item: RadarItem {
                item_id: "ignored".to_owned(),
                kind: RadarItemKind::BestConversion,
                category: Actionability::InstantExecutable,
                amount_in: amount(path.first().expect("a route starts somewhere").as_str()),
                amount_out: amount(path.last().expect("a route ends somewhere").as_str()),
                path_asset_ids: path,
                round_trip_basis_points: Some(100),
                value_basis_points: Some(100),
                liquidity_capacity: Some(10),
                reasons: Vec::new(),
                risk_flags: Vec::new(),
                blocking_risks: Vec::new(),
                conversion_path: None,
                triangle: None,
            },
            light: None,
            structural: Vec::new(),
            leg_books: Vec::new(),
        }
    }

    #[test]
    fn a_reordered_scan_keeps_the_selection_on_the_same_route() {
        let before = vec![row(&["one", "z"]), row(&["two", "z"]), row(&["three", "z"])];
        // The same three routes, re-ranked: the selected one moved from the
        // middle to the front.
        let after = vec![row(&["two", "z"]), row(&["one", "z"]), row(&["three", "z"])];
        assert_eq!(
            remap_selection(Some(1), &before, &after),
            Some(0),
            "the selection should follow the route it was put on, not stay on \
             the index that route used to have"
        );
    }

    #[test]
    fn a_scan_without_the_selected_route_clears_the_selection() {
        let before = vec![row(&["one", "z"]), row(&["two", "z"]), row(&["three", "z"])];
        // This scan did not find route two at all.
        let after = vec![row(&["one", "z"]), row(&["three", "z"])];
        assert_eq!(
            remap_selection(Some(1), &before, &after),
            None,
            "a route this scan did not find has no row, and pointing at a \
             neighbour instead is how the panel starts lying"
        );
    }
}

#[cfg(test)]
mod column_width_tests {
    use super::*;
    use ptt_runtime::domain::{AssetAmount, AssetUnit, RadarItemKind};

    fn catalog() -> &'static ptt_runtime::domain::Catalog {
        ptt_runtime::domain::poe2_catalog()
    }

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn amount(id: &str) -> AssetAmount {
        AssetAmount {
            asset_id: asset(id),
            quanta: 10,
            unit: AssetUnit::whole(),
        }
    }

    /// A row whose only interesting property is how long its route reads.
    ///
    /// The ids are runs of letters the catalogue has never heard of, so
    /// `asset_name` falls back to the id itself and the route text is exactly
    /// as long as the test asked for.
    fn row(name_length: usize) -> OpportunityRow {
        let from = "a".repeat(name_length);
        let to = "b".repeat(name_length);
        OpportunityRow {
            item: RadarItem {
                item_id: "width".to_owned(),
                kind: RadarItemKind::BestConversion,
                category: Actionability::InstantExecutable,
                path_asset_ids: vec![asset(&from), asset(&to)],
                amount_in: amount(&from),
                amount_out: amount(&to),
                round_trip_basis_points: Some(100),
                value_basis_points: Some(100),
                liquidity_capacity: Some(10),
                reasons: Vec::new(),
                risk_flags: Vec::new(),
                blocking_risks: Vec::new(),
                conversion_path: None,
                triangle: None,
            },
            light: None,
            structural: Vec::new(),
            leg_books: Vec::new(),
        }
    }

    fn table(rows: Vec<OpportunityRow>) -> RadarTable {
        RadarTable::new(rows, UiLanguage::English, catalog())
    }

    fn total_width(table: &RadarTable) -> f32 {
        table
            .columns
            .iter()
            .map(|column| f32::from(column.width))
            .sum()
    }

    fn route_width(table: &RadarTable) -> f32 {
        f32::from(table.columns[ROUTE_COLUMN].width)
    }

    #[test]
    fn every_column_together_fits_the_table_width_budget() {
        let mut table = table(Vec::new());
        assert!(
            total_width(&table) <= RADAR_TABLE_WIDTH_BUDGET,
            "empty table is {} wide",
            total_width(&table)
        );

        table.set_rows(vec![row(90)], UiLanguage::English, catalog());
        assert!(
            total_width(&table) <= RADAR_TABLE_WIDTH_BUDGET,
            "table with the longest possible route is {} wide",
            total_width(&table)
        );
    }

    #[test]
    fn a_long_route_widens_its_column_up_to_the_ceiling() {
        let mut table = table(Vec::new());
        assert!(
            (route_width(&table) - COL_ROUTE_MIN_WIDTH).abs() < 0.5,
            "an empty scan should leave the route column at its floor, got {}",
            route_width(&table)
        );

        table.set_rows(vec![row(90)], UiLanguage::English, catalog());
        assert!(
            (route_width(&table) - COL_ROUTE_MAX_WIDTH).abs() < 0.5,
            "a route far too long for the budget should pin the column to the \
             ceiling, got {}",
            route_width(&table)
        );
    }

    #[test]
    fn a_dragged_route_width_survives_the_next_scan() {
        let mut table = table(vec![row(22)]);
        let mut widths: Vec<gpui::Pixels> =
            table.columns.iter().map(|column| column.width).collect();
        widths[ROUTE_COLUMN] = px(200.0);
        table.set_column_widths(&widths);

        // A scan whose routes are far too long for the column the reader
        // chose: the fit must not talk them out of it.
        table.set_rows(vec![row(90)], UiLanguage::English, catalog());
        assert!(
            (route_width(&table) - 200.0).abs() < 0.5,
            "a scan overrode the width the reader dragged, leaving {}",
            route_width(&table)
        );
    }

    #[test]
    fn the_route_column_only_widens_in_steps_worth_seeing() {
        let mut table = table(vec![row(22)]);
        let start = route_width(&table);
        assert!(
            start > COL_ROUTE_MIN_WIDTH,
            "fixture must start above the floor so a shrink is visible, got {start}"
        );

        // About 14px longer: under the step, so nothing moves.
        table.set_rows(vec![row(23)], UiLanguage::English, catalog());
        assert!(
            (route_width(&table) - start).abs() < 0.5,
            "a change smaller than the step moved the column from {start} to {}",
            route_width(&table)
        );

        // About 41px longer: over the step, so it moves.
        table.set_rows(vec![row(25)], UiLanguage::English, catalog());
        assert!(
            route_width(&table) > start + COL_ROUTE_GROWTH_STEP,
            "a change larger than the step left the column at {}",
            route_width(&table)
        );
    }
}
