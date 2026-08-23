//! Component gallery and table-density probe for the data pages.
//!
//! Two things the app cannot answer on its own. The first is what a page
//! looks like in a state the live market does not happen to be in right now —
//! an empty book, a route carrying every risk flag at once, a Chinese reader.
//! The second is whether `gpui-component`'s table survives a real result set:
//! neither this app nor POE Alarm has ever rendered a virtualised list, and
//! every data page in P8 is built on one.
//!
//! The density panel answers the second question mechanically rather than by
//! eye. The table is virtualised or it is not, and the observable difference
//! is whether it asks the delegate for five hundred rows or for the twenty on
//! screen — so the delegate counts the cells it is asked to draw.
//!
//! Usage: `cargo run --bin ptt-ui-preview`

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{
    Root, Sizable, Size, StyledExt,
    table::{Column, Table, TableDelegate, TableState},
};
use ptt_app::shell::pages::opportunities::{RadarTable, actionability_kind};
use ptt_runtime::domain::{
    Actionability, AssetAmount, AssetUnit, ExecutionRisk, FreshnessStatus, RadarItem,
    RadarItemKind, RadarReason,
};
use ptt_runtime::report_text;
use ptt_runtime::reports::OpportunityRow;
use ptt_settings::UiLanguage;
use ptt_trade_domain::MarketAssetId;

use ptt_app::theme::{self, *};
use ptt_app::ui::*;

const PREVIEW_SIZE: (f32, f32) = (1280.0, 780.0);
/// Enough rows that an un-virtualised table would be obvious.
const DENSITY_ROWS: usize = 500;
const DENSITY_COLUMNS: usize = 7;
/// Set to make the gallery print its density reading and quit.
const PROBE_ENV: &str = "PTT_PREVIEW_PROBE";

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

/// Synthetic radar rows spanning every state a real one can be in.
///
/// Cycled rather than randomised: a gallery that looks different on every run
/// cannot be compared against the last one.
fn density_rows(count: usize) -> Vec<OpportunityRow> {
    const CURRENCIES: [&str; 6] = [
        "divine-orb",
        "chaos-orb",
        "exalted-orb",
        "vaal-orb",
        "mirror-shard",
        "annulment-orb",
    ];
    const CATEGORIES: [Actionability; 4] = [
        Actionability::InstantExecutable,
        Actionability::MakerTheoretical,
        Actionability::ProbeRequired,
        Actionability::SuspiciousOutlier,
    ];
    const LIGHTS: [Option<FreshnessStatus>; 4] = [
        Some(FreshnessStatus::Fresh),
        Some(FreshnessStatus::Usable),
        Some(FreshnessStatus::Stale),
        None,
    ];
    const RISKS: [ExecutionRisk; 4] = [
        ExecutionRisk::ThinLiquidity,
        ExecutionRisk::SingleListingBook,
        ExecutionRisk::CaptureSkewExceeded,
        ExecutionRisk::PriceOutlier,
    ];

    (0..count)
        .map(|index| {
            let from = CURRENCIES[index % CURRENCIES.len()];
            let via = CURRENCIES[(index + 2) % CURRENCIES.len()];
            let to = CURRENCIES[(index + 4) % CURRENCIES.len()];
            let triangle = index % 3 == 0;
            let mut path = vec![asset(from), asset(via), asset(to)];
            if triangle {
                path.push(asset(from));
            }
            // Every row carries one more risk than the last, up to all of
            // them: the widest row is the one that breaks a layout.
            let risks = RISKS[..(index % (RISKS.len() + 1))].to_vec();
            OpportunityRow {
                item: RadarItem {
                    item_id: format!("preview-{index}"),
                    kind: if triangle {
                        RadarItemKind::Loop
                    } else {
                        RadarItemKind::BestConversion
                    },
                    category: CATEGORIES[index % CATEGORIES.len()],
                    path_asset_ids: path,
                    amount_in: amount(from, 10),
                    amount_out: amount(to, 1_000 + index as u64),
                    // A quarter of the rows are unpriced, which is its own
                    // column state rather than a zero.
                    value_basis_points: (index % 4 != 3).then(|| (index as i64 % 900) * 13 - 400),
                    // Spread across magnitudes so the liquidity-first ordering
                    // is visible in the gallery rather than only in a test.
                    liquidity_capacity: (index % 5 != 4).then(|| (index as u64 % 17) * 250 + 5),
                    reasons: if triangle {
                        vec![RadarReason::LoopReturn]
                    } else {
                        vec![RadarReason::BetterThanDirect]
                    },
                    risk_flags: Vec::new(),
                    blocking_risks: risks,
                    conversion_path: None,
                    triangle: None,
                },
                light: LIGHTS[index % LIGHTS.len()],
                // A third of the rows carry structural context, cycling the
                // classes so every chip state renders in the gallery.
                structural: if index % 3 == 0 {
                    use ptt_runtime::domain::LiquidityClass;
                    vec![ptt_runtime::reports::StructuralNote {
                        asset_id: asset(via),
                        class: match index % 4 {
                            0 => LiquidityClass::Scarce,
                            1 => LiquidityClass::Oversupplied,
                            2 => LiquidityClass::Quiet,
                            _ => LiquidityClass::Balanced,
                        },
                        verdict: None,
                        greedy_candidate: index % 4 == 0,
                    }]
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// The shipping delegate, with a tally.
///
/// A wrapper rather than a second delegate: rehearsing a copy of the table
/// would measure the copy. Every method forwards, and the only thing added is
/// the count of cells the table actually asks for.
struct CountingTable {
    inner: RadarTable,
    drawn: Rc<Cell<usize>>,
}

impl CountingTable {
    fn new(inner: RadarTable, drawn: Rc<Cell<usize>>) -> Self {
        Self { inner, drawn }
    }
}

impl TableDelegate for CountingTable {
    fn columns_count(&self, cx: &App) -> usize {
        self.inner.columns_count(cx)
    }

    fn rows_count(&self, cx: &App) -> usize {
        self.inner.rows_count(cx)
    }

    fn column(&self, col_ix: usize, cx: &App) -> &Column {
        self.inner.column(col_ix, cx)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.drawn.set(self.drawn.get() + 1);
        // `Context` derefs to `App`, which is all a cell needs.
        self.inner.render_cell(row_ix, col_ix, window, cx)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Density,
    Kit,
}

struct Gallery {
    tab: Tab,
    language: UiLanguage,
    table: Entity<TableState<CountingTable>>,
    drawn: Rc<Cell<usize>>,
    /// Cells drawn at the moment the counter was last read, so the reading
    /// survives the frames that rendering the reading itself causes.
    sampled: usize,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let drawn = Rc::new(Cell::new(0));
        let rows = density_rows(DENSITY_ROWS);
        let delegate = CountingTable::new(
            RadarTable::new(
                rows,
                UiLanguage::English,
                ptt_runtime::domain::poe2_catalog(),
            ),
            drawn.clone(),
        );
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
        });
        // Under PTT_PREVIEW_PROBE the gallery reports its own density and
        // quits, so the virtualisation claim is something a build can check
        // rather than something a person has to click a button to see.
        if std::env::var_os(PROBE_ENV).is_some() {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1_500))
                    .await;
                let drawn = this
                    .read_with(cx, |this: &Gallery, _| this.drawn.get())
                    .unwrap_or(0);
                let eager = DENSITY_ROWS * DENSITY_COLUMNS;
                println!(
                    "density-probe: rows={DENSITY_ROWS} columns={DENSITY_COLUMNS} \
                     cells_if_eager={eager} cells_drawn={drawn}"
                );
                // An eager table draws every cell on its first frame, so one
                // frame alone would already exceed this. Several frames pass
                // in the window above, which is why the bar is the eager cost
                // of a single frame rather than a tighter number.
                if drawn >= eager {
                    eprintln!("density-probe: the table is drawing every row - not virtualised");
                    std::process::exit(1);
                }
                let _ = cx.update(|cx: &mut App| cx.quit());
            })
            .detach();
        }
        Self {
            tab: Tab::Density,
            language: UiLanguage::English,
            table,
            drawn,
            sampled: 0,
        }
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let drawn = self.drawn.clone();
        let rows = density_rows(DENSITY_ROWS);
        let delegate = CountingTable::new(
            RadarTable::new(rows, self.language, ptt_runtime::domain::poe2_catalog()),
            drawn,
        );
        self.table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
        });
    }

    fn density_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let selected = self.table.read(cx).selected_row();
        panel()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header("radar table · density probe"))
            .child(
                div()
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .child(
                        mono(format!("{DENSITY_ROWS} rows"))
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META)),
                    )
                    .child(
                        mono(format!("cells drawn: {}", self.sampled))
                            .text_size(fs(FS_11_5))
                            .text_color(c(if self.sampled > DENSITY_ROWS {
                                DANGER
                            } else {
                                ACCENT_TEXT
                            })),
                    )
                    .child(
                        mono(match selected {
                            Some(row) => format!("selected row {row}"),
                            None => "no row selected".to_owned(),
                        })
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_SECONDARY)),
                    )
                    .child(div().flex_grow())
                    .child(
                        button("density-sample", LedgerButton::Primary, "sample", cx).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.sampled = this.drawn.get();
                                this.drawn.set(0);
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                div().flex_1().overflow_hidden().child(
                    Table::new(&self.table)
                        .stripe(true)
                        .bordered(false)
                        .with_size(Size::XSmall),
                ),
            )
    }

    fn kit_panel(&self) -> gpui::Div {
        let risks = [
            "thin liquidity".to_owned(),
            "only one listing on this side".to_owned(),
            "capture skew exceeded".to_owned(),
            "price outlier".to_owned(),
        ];
        panel()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header("data-page kit"))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(inline_section("freshness"))
                    .child(
                        div().h_flex().gap_3().children(
                            [
                                FreshnessStatus::Fresh,
                                FreshnessStatus::Usable,
                                FreshnessStatus::Stale,
                                FreshnessStatus::Archived,
                            ]
                            .map(|status| {
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(status_dot(freshness_kind(status)))
                                    .child(
                                        div().text_size(fs(FS_11_5)).child(
                                            report_text::freshness_light(self.language, status),
                                        ),
                                    )
                            }),
                        ),
                    )
                    .child(inline_section("verdict chips"))
                    .child(
                        div().h_flex().gap_2().children(
                            [
                                Actionability::InstantExecutable,
                                Actionability::MakerTheoretical,
                                Actionability::ProbeRequired,
                                Actionability::SuspiciousOutlier,
                            ]
                            .map(|category| {
                                chip(
                                    actionability_kind(category),
                                    report_text::actionability(self.language, category),
                                )
                            }),
                        ),
                    )
                    .child(inline_section("risk chips, folded at two"))
                    .child(chips(StatusKind::Warning, &risks, 2))
                    .child(inline_section("detail panel"))
                    .child(
                        detail_panel("route detail").child(
                            div()
                                .p_3()
                                .flex()
                                .flex_col()
                                .child(kv_row("route", "divine-orb → chaos-orb → exalted-orb"))
                                .child(kv_row("in", "10 divine-orb"))
                                .child(kv_row("out", "2000 exalted-orb"))
                                .child(kv_row("captured", "earliest 4m ago, skew 91s"))
                                // Deliberately wider than the ~212px the value
                                // column gets inside a 340px panel, and CJK so
                                // there are no spaces to break on: this is the
                                // "structural liquidity" row from the live app,
                                // the one that used to run off the window edge.
                                // If it stops wrapping here, the min_w(0) on
                                // the value container has been lost again.
                                .child(kv_row(
                                    "risks",
                                    "神聖石 供需均衡，混沌石 供需均衡，骷髏馬克的邀請 供需均衡",
                                )),
                        ),
                    )
                    .child(inline_section("empty state"))
                    .child(empty_state("nothing to show yet")),
            )
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: [(Tab, &str, &'static str); 2] = [
            (Tab::Density, "tab-density", "density"),
            (Tab::Kit, "tab-kit", "kit"),
        ];
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(CANVAS))
            .text_color(c(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .child(
                div()
                    .h(px(H_TITLEBAR))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE_STRONG))
                    .child(div().text_size(fs(FS_12_5)).child(spaced("UI PREVIEW")))
                    .children(tabs.map(|(tab, id, label)| {
                        button(
                            id,
                            if self.tab == tab {
                                LedgerButton::Primary
                            } else {
                                LedgerButton::Quiet
                            },
                            label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = tab;
                            cx.notify();
                        }))
                    }))
                    .child(div().flex_grow())
                    .children([UiLanguage::English, UiLanguage::Chinese].map(|language| {
                        button(
                            match language {
                                UiLanguage::English => "lang-en",
                                UiLanguage::Chinese => "lang-zh",
                            },
                            if self.language == language {
                                LedgerButton::Primary
                            } else {
                                LedgerButton::Quiet
                            },
                            match language {
                                UiLanguage::English => "EN",
                                UiLanguage::Chinese => "中文",
                            },
                            cx,
                        )
                        .on_click(cx.listener(
                            move |this, _, window: &mut Window, cx| {
                                this.language = language;
                                this.rebuild(window, cx);
                                cx.notify();
                            },
                        ))
                    })),
            )
            .child(
                div()
                    .flex_grow()
                    .flex()
                    .p_3()
                    .overflow_hidden()
                    .child(match self.tab {
                        Tab::Density => self.density_panel(cx),
                        Tab::Kit => self.kit_panel(),
                    }),
            )
    }
}

fn main() {
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::apply_ledger_theme(cx);

        let (width, height) = PREVIEW_SIZE;
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("POE Trade Tracker — UI preview".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Gallery::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("failed to open preview window");
        cx.activate(true);
    });
}
