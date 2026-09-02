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
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
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
use crate::shell::{AppShell, RadarSegment};
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, detail_panel, empty_state, freshness_kind, kv_row, mono,
    panel, warning_band,
};

/// Execution category → status colour.
///
/// "Executable now" is neutral, not colored: a verdict that is fine has
/// nothing to warn about, and the semantic colors are reserved for the rows
/// that need attention (琥珀) or distrust (砖红).
#[must_use]
pub fn actionability_kind(category: Actionability) -> StatusKind {
    match category {
        Actionability::InstantExecutable => StatusKind::Idle,
        Actionability::MakerTheoretical | Actionability::ProbeRequired => StatusKind::Warning,
        Actionability::SuspiciousOutlier => StatusKind::Error,
    }
}

/// The verdict, at column width: four characters where `report_text` writes
/// a sentence. The sentence still reaches the reader via the detail panel.
#[must_use]
pub fn verdict_short(chrome: &'static crate::i18n::Text, category: Actionability) -> &'static str {
    match category {
        Actionability::InstantExecutable => chrome.radar_verdict_instant,
        Actionability::MakerTheoretical => chrome.radar_verdict_maker,
        Actionability::ProbeRequired => chrome.radar_verdict_probe,
        Actionability::SuspiciousOutlier => chrome.radar_verdict_outlier,
    }
}

/// The freshness tier, at column width: 新鲜 / 偏旧 / 过期 / 归档.
#[must_use]
pub fn freshness_short(
    chrome: &'static crate::i18n::Text,
    status: FreshnessStatus,
) -> &'static str {
    match status {
        FreshnessStatus::Fresh => chrome.freshness_fresh,
        FreshnessStatus::Usable => chrome.freshness_usable,
        FreshnessStatus::Stale => chrome.freshness_stale,
        FreshnessStatus::Archived => chrome.freshness_archived,
    }
}

/// The route as one row of names with ghost-gray arrows, truncating.
///
/// 12px 界面字体而不是等宽:路径是名字不是数字,等宽会把 348px 的预算吃掉
/// 一截。箭头降为幽灵灰,名字才是要读的东西。截断优于换行——行高固定是
/// 硬约束。
fn route_cell(
    catalog: &ptt_runtime::domain::Catalog,
    language: UiLanguage,
    path: &[MarketAssetId],
) -> gpui::Div {
    let mut row = div()
        .h_flex()
        .items_center()
        .gap(px(4.))
        .overflow_hidden()
        .text_size(fs(FS_12))
        .text_color(c(TEXT_PRIMARY));
    for (index, asset) in path.iter().enumerate() {
        if index > 0 {
            row = row.child(
                div()
                    .flex_none()
                    .text_color(c(TEXT_GHOST))
                    .child(SharedString::from("→")),
            );
        }
        row = row.child(div().whitespace_nowrap().child(SharedString::from(
            crate::names::asset_name(catalog, language, asset.as_str()),
        )));
    }
    row
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
pub fn edge_text(item: &RadarItem, language: UiLanguage) -> (String, Token) {
    item.round_trip_basis_points.map_or_else(
        || (report_text::report(language).unpriced.to_owned(), TEXT_META),
        |points| {
            (
                report_text::percent_from_basis_points(points),
                // 正收益金字(色字=主题);负数砖红文字是那条规则唯一的
                // 批准例外。
                if points >= 0 {
                    ACCENT_TEXT
                } else {
                    DANGER_TEXT
                },
            )
        },
    )
}

/// Every column's width, added up, must land exactly here.
///
/// UI-DESIGN.md §3 的列宽预算:1280 窗口下表宽 842,内容 822。八个数字就是
/// 布局——上游列是纯像素的,不会自己回流适配窗口。路径列 348 是量出来的:
/// 最长的五段闭环实测 328.5px,336 会截掉末段,而闭环行截掉末段就看不出它
/// 闭没闭。
pub const RADAR_TABLE_WIDTH_BUDGET: f32 = 822.0;

// §3 定稿列宽:数据 54 | 种类 42 | 路径 348 | 收益 62 | 流动性 58 |
// 汇率 88 | 可执行性 80 | 风险 90(设计里是 1fr,这里给它剩余预算)。
const COL_LIGHT_WIDTH: f32 = 54.0;
const COL_KIND_WIDTH: f32 = 42.0;
const COL_ROUTE_WIDTH: f32 = 348.0;
const COL_EDGE_WIDTH: f32 = 62.0;
const COL_DEPTH_WIDTH: f32 = 58.0;
const COL_RATE_WIDTH: f32 = 88.0;
const COL_VERDICT_WIDTH: f32 = 80.0;
const COL_RISKS_WIDTH: f32 = 90.0;

/// A column edit that breaks the budget has to fail at the build, not on
/// screen at 1280 where the last column quietly walks off the panel.
const _: () = assert!(
    COL_LIGHT_WIDTH
        + COL_KIND_WIDTH
        + COL_ROUTE_WIDTH
        + COL_EDGE_WIDTH
        + COL_DEPTH_WIDTH
        + COL_RATE_WIDTH
        + COL_VERDICT_WIDTH
        + COL_RISKS_WIDTH
        == RADAR_TABLE_WIDTH_BUDGET,
    "the radar columns no longer add up to the §3 budget: change a column \
     and its neighbour together, the 1280 window will not grow"
);

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
        Self {
            columns: vec![
                // 新鲜度排第一列(§3):这是用户第一眼要看的东西。
                Column::new("light", chrome.radar_column_light).width(px(COL_LIGHT_WIDTH)),
                Column::new("kind", chrome.radar_column_kind).width(px(COL_KIND_WIDTH)),
                Column::new("route", chrome.radar_column_route).width(px(COL_ROUTE_WIDTH)),
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
    /// pays a blank frame for a layout that did not change. The columns are
    /// fixed now (§3 列宽预算), so only a language or catalogue change moves
    /// them.
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
        self.rows = rows;
        false
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
        let chrome = i18n::text(language);
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let item = &row.item;
        let cell = |body: gpui::Div| body.h_flex().items_center().size_full();

        match col_ix {
            // 数据(第一列,§3):6px 色点 + 汉字。没有判定时给幽灵横杠——
            // "这里没有信息"不值得读清。
            0 => match row.light {
                Some(status) => cell(div())
                    .child(crate::ui::freshness_cell(
                        freshness_kind(status),
                        freshness_short(chrome, status),
                    ))
                    .into_any_element(),
                None => cell(div().text_size(fs(FS_11)).text_color(c(TEXT_GHOST)))
                    .child("—")
                    .into_any_element(),
            },
            // 种类:2 字,省下的 40px 全给了路径列。
            1 => cell(div().text_size(fs(FS_11)).text_color(c(TEXT_SECONDARY)))
                .child(match item.kind {
                    RadarItemKind::BestConversion => chrome.radar_kind_conversion,
                    RadarItemKind::Loop => chrome.radar_kind_loop,
                })
                .into_any_element(),
            2 => cell(route_cell(self.catalog, language, &item.path_asset_ids)).into_any_element(),
            3 => {
                let (label, colour) = edge_text(item, language);
                cell(
                    mono(label)
                        .text_size(fs(FS_12))
                        .text_color(c(colour))
                        .justify_end(),
                )
                .into_any_element()
            }
            // Depth, in the settlement anchor -- one currency for every row,
            // because a column that is sorted on has to be comparable down
            // its own length.
            4 => cell(
                mono(
                    item.liquidity_capacity
                        .map_or_else(|| "-".to_owned(), |capacity| capacity.to_string()),
                )
                .text_size(fs(FS_12))
                .text_color(c(TEXT_DATA))
                .justify_end(),
            )
            .into_any_element(),
            // The composed front rate where the walked payout used to be:
            // the scan runs at a canonical size nobody holds, so its output
            // said nothing about the reader — the rate is what they act on,
            // and the detail panel prices their own ask at this same rate.
            5 => cell(
                mono(
                    ptt_runtime::reports::walk_route(&row.leg_books, 1)
                        .rate
                        .map_or_else(|| "-".to_owned(), |rate| rate.text()),
                )
                .text_size(fs(FS_12))
                .text_color(c(TEXT_DATA))
                .justify_end(),
            )
            .into_any_element(),
            // 可执行性:4 字徽章,完整说法在明细栏。
            6 => cell(div())
                .child(crate::ui::chip_table(
                    actionability_kind(item.category),
                    verdict_short(chrome, item.category),
                ))
                .into_any_element(),
            // Capped and silent about it: what did not fit is in the detail
            // panel, which is where a reader who cares is going anyway.
            _ => cell(div())
                .child(crate::ui::chips_table(
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

/// 页标题行(26px):标题 + 页签条 + 发丝线拉满 + 右侧计数(有表才有)。
fn radar_title_row(
    title: &'static str,
    segment_row: gpui::Div,
    count: Option<String>,
) -> gpui::Div {
    let mut row = div()
        .h(px(H_INPUT))
        .flex_none()
        .h_flex()
        .items_center()
        .gap(px(SP_10))
        .child(div().text_size(fs(FS_13)).child(crate::ui::spaced(title)))
        .child(segment_row)
        .child(div().flex_1().h(px(1.)).bg(c(HAIRLINE_SOFT)));
    if let Some(count) = count {
        row = row.child(mono(count).text_size(fs(FS_11)).text_color(c(TEXT_META)));
    }
    row
}

/// 雷达页的外框:标题行在最上面,其余由调用方往下摞。
fn radar_page(title_row: gpui::Div) -> gpui::Div {
    div()
        .flex_grow()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(SP_8))
        .p(px(SP_10))
        .child(title_row)
}

/// 没有表可画时的一页:标题行(页签条还在,好切回去)+ 一句原因。
fn radar_unavailable(title_row: gpui::Div, message: &str) -> gpui::Div {
    radar_page(title_row).child(
        panel()
            .flex_1()
            .flex()
            .flex_col()
            .child(empty_state(message)),
    )
}

/// 事实带里的一格:小灰标签 + 等宽数值。交易所雷达段用;抓取段的同款
/// 闭包留在原处没动。
fn band_stat(label: &'static str, value: String, color: Token) -> gpui::Div {
    div()
        .h_flex()
        .items_baseline()
        .gap(px(6.))
        .px(px(SP_12))
        .child(
            div()
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META))
                .child(label),
        )
        .child(mono(value).text_size(fs(FS_12)).text_color(c(color)))
}

fn band_divider() -> gpui::Div {
    div().w(px(1.)).h(px(20.)).flex_none().bg(c(HAIRLINE_SOFT))
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
        // 两个页签共用一张表:行按当前页签取,切页签就是换行。
        let scan = match self.radar_segment {
            RadarSegment::Capture => Some(&model.scan),
            RadarSegment::Exchange => model.exchange.as_ref().map(|exchange| &exchange.scan),
        };
        let rows = match scan {
            Some(RadarScan::Ran(scan)) => scan.items.clone(),
            _ => Vec::new(),
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
        let segment_row = self.radar_segment_row(cx);
        if self.radar_segment == RadarSegment::Exchange {
            return self.render_exchange_radar(model, segment_row, cx);
        }

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
            let title_row = radar_title_row(text.page_opportunities, segment_row, None);
            return radar_unavailable(title_row, message);
        }
        let RadarScan::Ran(scan) = &model.scan else {
            unreachable!("the unavailable case returned above");
        };

        let selected = self
            .radar_table
            .read(cx)
            .selected_row()
            .and_then(|index| self.radar_table.read(cx).delegate().row(index).cloned());

        // 页标题行(26px):标题 + 发丝线拉满 + 右侧计数。
        let shown = scan.items.len();
        let found: usize = if scan.diagnostics.results_truncated {
            scan.diagnostics.item_count_before_limit as usize
        } else {
            shown
        };
        let title_row = div()
            .h(px(H_INPUT))
            .flex_none()
            .h_flex()
            .items_center()
            .gap(px(SP_10))
            .child(
                div()
                    .text_size(fs(FS_13))
                    .child(crate::ui::spaced(text.page_opportunities)),
            )
            .child(segment_row)
            .child(div().flex_1().h(px(1.)).bg(c(HAIRLINE_SOFT)))
            .child(
                mono(report_text::fill(
                    report_text::report(language).results_cut,
                    &[&found.to_string(), &shown.to_string()],
                ))
                .text_size(fs(FS_11))
                .text_color(c(TEXT_META)),
            );

        // 34px 事实带(§3):起点 / 目标 / 可定价 / 缺价 / 闭环·有得赚 / 门槛。
        // 之前是三行流水账句子;事实带一眼扫过去,句子只在出问题时出现。
        let divider = || div().w(px(1.)).h(px(20.)).flex_none().bg(c(HAIRLINE_SOFT));
        let stat = |label: &'static str, value: String, color: Token| {
            div()
                .h_flex()
                .items_baseline()
                .gap(px(6.))
                .px(px(SP_12))
                .child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META))
                        .child(label),
                )
                .child(mono(value).text_size(fs(FS_12)).text_color(c(color)))
        };
        let missing = scan.diagnostics.missing_conversion_count;
        let band = div()
            .h(px(34.))
            .flex_none()
            .h_flex()
            .items_center()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                div()
                    .h_flex()
                    .items_baseline()
                    .gap(px(6.))
                    .px(px(SP_12))
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(text.radar_band_start),
                    )
                    .child(
                        div().text_size(fs(FS_12)).child(SharedString::from(
                            scan.starts
                                .iter()
                                .map(|asset| self.display_name(asset.as_str()))
                                .collect::<Vec<_>>()
                                .join(" · "),
                        )),
                    ),
            )
            .child(divider())
            .child(stat(
                text.radar_band_targets,
                scan.diagnostics.distinct_target_count.to_string(),
                TEXT_DATA,
            ))
            .child(divider())
            .child(stat(
                text.radar_band_priced,
                scan.diagnostics.complete_conversion_count.to_string(),
                TEXT_DATA,
            ))
            .child(stat(
                text.radar_band_missing,
                missing.to_string(),
                // 缺价是唯一值得变色的数:0 缺价没事,缺了才需要注意。
                if missing > 0 { WARN_TEXT } else { TEXT_DATA },
            ))
            .child(divider())
            .child(stat(
                text.radar_band_loops,
                scan.diagnostics.triangle_evaluation_count.to_string(),
                TEXT_DATA,
            ))
            .child(stat(
                text.radar_band_profitable,
                scan.diagnostics.profitable_loop_count.to_string(),
                TEXT_DATA,
            ))
            .child(div().flex_1())
            .child(stat(
                text.radar_band_threshold,
                report_text::percent_from_basis_points(
                    i64::try_from(self.settings_tuning().radar.minimum_profit_basis_points)
                        .unwrap_or(i64::MAX),
                ),
                TEXT_DATA,
            ));

        // 出问题才出现的句子:预算耗尽(琥珀,可能漏了),以及结构性备注。
        // 注意条,不是一段段琥珀色的字——理由同关注列表页。
        let note_tag = self.text().note_band_tag;
        let mut warnings = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .children(model.notes.iter().map(|note| warning_band(note_tag, note)));
        if scan.diagnostics.budget_exhausted {
            warnings = warnings.child(warning_band(
                note_tag,
                &report_text::fill(
                    report_text::report(language).partial_scan,
                    &[
                        &scan.diagnostics.skipped_target_count.to_string(),
                        &scan.diagnostics.expansions_used.to_string(),
                    ],
                ),
            ));
        }

        let table = if scan.items.is_empty() {
            panel()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(empty_state(
                    report_text::report(language).nothing_beats_holding,
                ))
        } else {
            panel().flex_1().flex().flex_col().overflow_hidden().child(
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
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .flex()
            .gap(px(SP_8))
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
            .gap(px(SP_8))
            .p(px(SP_10))
            .child(title_row)
            .child(band)
            .child(warnings)
            .child(body)
            .child(self.radar_probes(scan.probe_candidates.clone(), cx))
    }

    /// 页签切换条：交易所雷达 / 抓取雷达。当前段金字 + 2px 金下划线，
    /// 同设置页分段栏的语汇；两段共用一张表，切段只是换行（`sync_radar_table`）。
    fn radar_segment_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        let text = self.text();
        let current = self.radar_segment;
        let mut row = div().h_flex().items_center().gap(px(SP_8));
        for segment in RadarSegment::ALL {
            let active = segment == current;
            let chip = div()
                .id(segment.element_id())
                .h(px(H_INPUT))
                .px(px(SP_8))
                .flex()
                .items_center()
                .text_size(fs(FS_12))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.radar_segment = segment;
                    this.sync_radar_table(cx);
                    cx.notify();
                }));
            let chip = if active {
                chip.border_b_2()
                    .border_color(c(ACCENT))
                    .font_semibold()
                    .text_color(c(ACCENT_TEXT))
            } else {
                chip.text_color(c(TEXT_SECONDARY))
                    .hover(|style| style.bg(c(HOVER)))
            };
            row = row.child(chip.child(SharedString::from(segment.label(text).to_string())));
        }
        row
    }

    /// 「交易所雷达」段：官方小时成交均价跑出来的环。同一张表、同一个明细栏，
    /// 只是事实带换成数据小时/资产/市场，并常驻一条"是线索不是承诺"的注意条。
    fn render_exchange_radar(
        &self,
        model: &OpportunitiesModel,
        segment_row: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let report = report_text::report(language);
        let max_age = ptt_runtime::reports::EXCHANGE_RADAR_MAX_AGE_HOURS.to_string();

        // 计数先算:标题行只造一次,不可用的几条出口各自把它带走。
        let ran = model
            .exchange
            .as_ref()
            .and_then(|exchange| match &exchange.scan {
                RadarScan::Ran(scan) => Some(scan),
                RadarScan::Unavailable(_) => None,
            });
        let count = ran.map(|scan| {
            report_text::fill(
                report.results_cut,
                &[
                    &scan.diagnostics.item_count_before_limit.to_string(),
                    &scan.items.len().to_string(),
                ],
            )
        });
        let title_row = radar_title_row(text.page_opportunities, segment_row, count);

        // 没配联赛 / 算不出来:一句话说清为什么没有表,页签条留着好切回去。
        let Some(exchange) = &model.exchange else {
            return radar_unavailable(title_row, text.exchange_no_league);
        };
        let scan = match &exchange.scan {
            RadarScan::Unavailable(RadarUnavailable::NoCoreCurrency) => {
                return radar_unavailable(title_row, report.no_core_currency);
            }
            RadarScan::Unavailable(RadarUnavailable::NotEnoughMarket) => {
                return radar_unavailable(
                    title_row,
                    &report_text::fill(report.exchange_radar_no_data, &[&max_age]),
                );
            }
            RadarScan::Unavailable(RadarUnavailable::NoStartUnits { anchor }) => {
                let anchor = anchor
                    .as_ref()
                    .map_or_else(|| "?".to_owned(), |asset| self.display_name(asset.as_str()));
                return radar_unavailable(
                    title_row,
                    &report_text::fill(report.exchange_radar_no_start, &[&anchor, &max_age]),
                );
            }
            RadarScan::Ran(scan) => scan,
        };

        let selected = self
            .radar_table
            .read(cx)
            .selected_row()
            .and_then(|index| self.radar_table.read(cx).delegate().row(index).cloned());

        // 34px 事实带:起点 / 数据小时(落后) / 资产 / 市场 / 评估环 / 过门槛 / 门槛。
        let data_hour = exchange
            .data_hour_ts
            .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
            .map_or_else(
                || "-".to_owned(),
                |at| {
                    at.with_timezone(&chrono::Local)
                        .format("%m-%d %H:00")
                        .to_string()
                },
            );
        let behind = exchange.hours_behind;
        let band = div()
            .h(px(34.))
            .flex_none()
            .h_flex()
            .items_center()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                div()
                    .h_flex()
                    .items_baseline()
                    .gap(px(6.))
                    .px(px(SP_12))
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(text.radar_band_start),
                    )
                    .child(
                        div().text_size(fs(FS_12)).child(SharedString::from(
                            scan.starts
                                .iter()
                                .map(|asset| self.display_name(asset.as_str()))
                                .collect::<Vec<_>>()
                                .join(" · "),
                        )),
                    ),
            )
            .child(band_divider())
            .child(band_stat(
                text.exchange_radar_band_hour,
                data_hour,
                TEXT_DATA,
            ))
            .child(band_stat(
                "",
                report_text::fill(text.exchange_radar_band_behind, &[&behind.to_string()]),
                // 落后两小时是 API 的常态;再多就是同步停了,值得变色。
                if behind > 2 { WARN_TEXT } else { TEXT_META },
            ))
            .child(band_divider())
            .child(band_stat(
                text.exchange_radar_band_assets,
                exchange.assets_used.to_string(),
                TEXT_DATA,
            ))
            .child(band_stat(
                text.exchange_radar_band_markets,
                exchange.pairs_used.to_string(),
                TEXT_DATA,
            ))
            .child(band_divider())
            .child(band_stat(
                text.radar_band_loops,
                scan.diagnostics.triangle_evaluation_count.to_string(),
                TEXT_DATA,
            ))
            .child(band_stat(
                text.radar_band_profitable,
                scan.diagnostics.profitable_loop_count.to_string(),
                TEXT_DATA,
            ))
            .child(div().flex_1())
            .child(band_stat(
                text.radar_band_threshold,
                report_text::percent_from_basis_points(i64::from(
                    exchange.minimum_profit_basis_points,
                )),
                TEXT_DATA,
            ));

        // 常驻注意条:这页永远带着"是线索不是承诺"。预算耗尽再加一条。
        let note_tag = text.note_band_tag;
        let mut warnings = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .child(warning_band(note_tag, report.exchange_radar_caveat));
        if scan.diagnostics.budget_exhausted {
            warnings = warnings.child(warning_band(
                note_tag,
                report.exchange_radar_budget_exhausted,
            ));
        }

        let table = if scan.items.is_empty() {
            panel()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(empty_state(report.exchange_radar_no_loop))
        } else {
            panel().flex_1().flex().flex_col().overflow_hidden().child(
                div().flex_1().overflow_hidden().child(
                    Table::new(&self.radar_table)
                        .stripe(true)
                        .bordered(false)
                        .with_size(Size::XSmall),
                ),
            )
        };
        let mut body = div()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .flex()
            .gap(px(SP_8))
            .overflow_hidden()
            .child(table);
        if let Some(row) = selected {
            body = body.child(self.radar_detail(&row, cx));
        }
        radar_page(title_row)
            .child(band)
            .child(warnings)
            .child(body)
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

        // 分隔线:明细栏用 soft 发丝线分组(路径 / 主数字 / 分步 / 依据)。
        let sep = || div().h(px(1.)).flex_none().bg(c(HAIRLINE_SOFT)).my(px(6.));

        let mut inner = div()
            .px(px(SP_10))
            .py(px(SP_8))
            .flex()
            .flex_col()
            .child(kv_row(
                text.detail_route,
                &route_text(self.catalog(), language, &item.path_asset_ids),
            ))
            .child(sep());

        // 整条收益是这一栏唯一的主数字(§3):15px 等宽 600。其余全部降级。
        match item.round_trip_basis_points {
            Some(points) => {
                inner = inner.child(crate::ui::kv_headline(
                    text.detail_round_trip,
                    &report_text::percent_from_basis_points(points),
                    if points >= 0 {
                        ACCENT_TEXT
                    } else {
                        DANGER_TEXT
                    },
                ));
            }
            None => {
                inner = inner.child(kv_row(
                    text.detail_round_trip,
                    report_text::report(language).unpriced,
                ));
            }
        }

        // The other number the route has: how much better it is than simply
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
        inner = inner.child(sep());

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
            inner = inner.child(sep());
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
        // 风险在明细栏是徽章不是句子:表格里被折掉的,在这里逐条铺开。
        if !item.blocking_risks.is_empty() {
            let labels: Vec<String> = item
                .blocking_risks
                .iter()
                .map(|risk| report_text::execution_risk(language, *risk).to_owned())
                .collect();
            inner = inner.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .py(px(3.))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .text_size(fs(FS_11))
                            .text_color(c(TEXT_META))
                            .child(text.detail_risks),
                    )
                    .child(
                        crate::ui::chips_table(StatusKind::Warning, &labels, labels.len())
                            .flex_wrap(),
                    ),
            );
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

        // 交易所雷达段:「去抓这组」把整条环的每一腿都排进待抓队列——
        // 大雷达给的是一组通货,不是一对;抓回来小雷达才有东西可裁。
        // 环的路径自带闭合(末尾回到起点),相邻两两就是全部的腿。
        if self.radar_segment == RadarSegment::Exchange {
            let legs: Vec<(String, String)> = item
                .path_asset_ids
                .windows(2)
                .map(|pair| (pair[0].as_str().to_owned(), pair[1].as_str().to_owned()))
                .collect();
            let pinned = legs
                .iter()
                .filter(|(from, to)| self.probe_queue.is_pinned(from, to))
                .count();
            let reason = report_text::fill(
                text.exchange_radar_pin_reason,
                &[&item
                    .round_trip_basis_points
                    .map_or_else(|| "?".to_owned(), report_text::percent_from_basis_points)],
            );
            let control = if !legs.is_empty() && pinned == legs.len() {
                let unpin_legs = legs.clone();
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        mono(report_text::fill(
                            text.exchange_radar_queued,
                            &[&pinned.to_string()],
                        ))
                        .text_size(fs(FS_11))
                        .text_color(c(ACCENT_TEXT)),
                    )
                    .child(
                        button(
                            "radar-unpin-group",
                            LedgerButton::Quiet,
                            text.unpin_label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            for (from, to) in &unpin_legs {
                                this.unpin_probe(from, to);
                            }
                            cx.notify();
                        })),
                    )
            } else {
                div().child(
                    button(
                        "radar-pin-group",
                        LedgerButton::Secondary,
                        text.exchange_radar_capture_group,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // 倒着钉:`pin` 每次插到队首,正着钉第一腿会被挤到最后。
                        for (from, to) in legs.iter().rev() {
                            this.pin_probe(from, to, &reason, true);
                        }
                        cx.notify();
                    })),
                )
            };
            inner = inner.child(div().pt_2().child(control));
        } else if item.category != Actionability::InstantExecutable
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
                        this.pin_probe(&from, &to, &reason, false);
                        cx.notify();
                    }),
                )
            }));
        }

        detail_panel(text.detail_header).child(inner)
    }

    /// The pairs whose absence or staleness limited what the scan could claim.
    ///
    /// A fixed 46px strip (§3), not a panel: it is a reminder, not a page.
    /// Only three pairs show; the rest fold into a ghost `+N`. Clicking a
    /// pair toggles it in the probe queue — the strip is the main window's
    /// half of the loop the HUD's read-only reminder line closes.
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
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        let language = self.language();
        let text = self.text();
        const SHOWN: usize = 3;
        let extra = candidates.len().saturating_sub(SHOWN);

        let mut bar = div()
            .h(px(46.))
            .flex_none()
            .h_flex()
            .items_center()
            .gap(px(SP_8))
            .px(px(SP_10))
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                div()
                    .w(px(76.))
                    .flex_none()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .line_height(px(FS_10_5 * 1.4))
                    .child(text.radar_probe_footer),
            )
            .child(div().w(px(1.)).h(px(24.)).flex_none().bg(c(HAIRLINE_SOFT)));

        for (index, candidate) in candidates.into_iter().take(SHOWN).enumerate() {
            let from = candidate.from_asset_id.as_str().to_owned();
            let to = candidate.to_asset_id.as_str().to_owned();
            let reason = report_text::probe_reason(language, candidate.reason).to_owned();
            let pinned = self.probe_queue.is_pinned(&from, &to);
            let pill = div()
                .id(("radar-probe", index))
                .h(px(24.))
                .flex_none()
                .h_flex()
                .items_center()
                .gap(px(6.))
                .px(px(SP_8))
                .rounded(px(RADIUS_BUTTON))
                .border_1()
                .border_color(c(if pinned { ACCENT_LINE } else { HAIRLINE }))
                .cursor_pointer()
                .hover(|style| style.bg(c(HOVER)))
                .child(
                    div()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_PRIMARY))
                        .whitespace_nowrap()
                        .child(SharedString::from(self.pair_label(&from, &to))),
                )
                .child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(if pinned { ACCENT_TEXT } else { TEXT_META }))
                        .whitespace_nowrap()
                        .child(SharedString::from(if pinned {
                            text.pinned_label.to_owned()
                        } else {
                            reason.clone()
                        })),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.probe_queue.is_pinned(&from, &to) {
                        this.unpin_probe(&from, &to);
                    } else {
                        this.pin_probe(&from, &to, &reason, false);
                    }
                    cx.notify();
                }));
            bar = bar.child(pill);
        }
        bar = bar.child(div().flex_1());
        if extra > 0 {
            bar = bar.child(
                mono(format!("+{extra}"))
                    .text_size(fs(FS_11))
                    .text_color(c(TEXT_GHOST)),
            );
        }
        bar
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

    fn table() -> RadarTable {
        RadarTable::new(
            Vec::new(),
            UiLanguage::English,
            ptt_runtime::domain::poe2_catalog(),
        )
    }

    /// The eight built columns must spend exactly the §3 budget: less leaves
    /// a gap after the risk column, more pushes it off the 1280 window. The
    /// const assert already checks the constants agree with each other; this
    /// checks the built table actually uses those constants.
    #[test]
    fn the_built_columns_spend_exactly_the_budget() {
        let total: f32 = table()
            .columns
            .iter()
            .map(|column| f32::from(column.width))
            .sum();
        assert!(
            (total - RADAR_TABLE_WIDTH_BUDGET).abs() < 0.5,
            "the built columns add up to {total}, the §3 budget is {RADAR_TABLE_WIDTH_BUDGET}"
        );
    }

    /// 新鲜度是用户第一眼要看的东西(§3),它必须是第一列。
    #[test]
    fn freshness_leads_the_table() {
        assert_eq!(
            table().columns.first().map(|column| column.key.clone()),
            Some("light".into()),
            "the data column moved out of first place"
        );
    }
}
