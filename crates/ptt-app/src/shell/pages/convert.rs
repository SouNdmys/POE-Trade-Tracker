//! "I hold X and want Y": what the route returns, and how to place it.
//!
//! The page answers two different questions that used to run together in one
//! column of prose. The upper half prices taking the fill now, at whatever
//! sizes apply. The lower half prices listing instead — undercutting the
//! competing front, matching it, or asking above it — against that same fill,
//! which is the only baseline that makes those three comparable.

use gpui::{AppContext as _, Context, Entity, ParentElement, Styled, div, px};
use gpui_component::{
    Sizable, Size, StyledExt as _,
    input::{Input, InputState},
    select::{Select, SelectState},
};
use ptt_runtime::domain::{MakerMode, MakerRecommendation, ProfitTier, RouteAccounting};
use ptt_runtime::report_text;
use ptt_runtime::reports::{ConvertModel, MakerModel};

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, chip, chips, empty_state, freshness_kind, kv_row, mono,
    panel, panel_header,
};

use super::opportunities::actionability_kind;

/// The currency picker's items.
///
/// Plain strings because that is what the catalogue's ids are; the select
/// searches them as typed.
pub type AssetSelect = Entity<SelectState<gpui_component::select::SearchableVec<String>>>;

impl AppShell {
    /// One currency picker.
    pub(crate) fn new_asset_select(
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AssetSelect {
        cx.new(|cx| {
            SelectState::new(
                gpui_component::select::SearchableVec::new(Vec::<String>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        })
    }

    /// The holdings box.
    ///
    /// Empty means "price the configured ladder"; a number means "price
    /// exactly this much", because "I have 100 divine" is a question about a
    /// hundred, not about one, ten and a hundred.
    pub(crate) fn new_holdings_input(
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("—")
                // Rejected as typed rather than at submit: there is no submit,
                // and a value that silently does nothing is worse than one the
                // box refuses to hold.
                .validate(|value, _| value.is_empty() || value.parse::<u64>().is_ok())
        })
    }

    /// Keeps the pickers offering what the book actually knows.
    ///
    /// Rebuilt only when the set of assets changes: replacing the delegate
    /// closes an open menu, and the set changes rarely while a session runs.
    pub(crate) fn sync_convert_selects(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let PageData::Convert(model) = &self.report else {
            return;
        };
        let assets: Vec<String> = model
            .assets
            .iter()
            .map(|asset| asset.as_str().to_owned())
            .collect();
        if assets == self.convert_assets {
            return;
        }
        self.convert_assets = assets.clone();
        let (have, need) = (
            model.have.as_str().to_owned(),
            model.need.as_str().to_owned(),
        );
        for (select, chosen) in [
            (self.convert_have.clone(), have),
            (self.convert_need.clone(), need),
        ] {
            let items = assets.clone();
            select.update(cx, |state, cx| {
                let index = items
                    .iter()
                    .position(|asset| *asset == chosen)
                    .map(gpui_component::IndexPath::new);
                *state = SelectState::new(
                    gpui_component::select::SearchableVec::new(items),
                    index,
                    window,
                    cx,
                )
                .searchable(true);
            });
        }
    }

    /// Applies a picked currency.
    ///
    /// An explicit choice sticks: once the user has said which pair they are
    /// looking at, an accepted book for some other pair must not drag the
    /// page away mid-thought.
    pub(crate) fn choose_pair(&mut self, have: Option<String>, need: Option<String>) {
        let (current_have, current_need) = self
            .report_pair
            .clone()
            .unwrap_or_else(|| (String::new(), String::new()));
        let have = have.unwrap_or(current_have);
        let need = need.unwrap_or(current_need);
        if have.is_empty() || need.is_empty() {
            return;
        }
        self.report_pair = Some((have, need));
        self.pair_chosen_by_user = true;
        self.report_stale = true;
    }

    /// The holdings the page should price, if the box holds a number.
    pub(crate) fn holdings_value(&self, cx: &gpui::App) -> Option<u64> {
        self.holdings_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|count| *count > 0)
    }

    /// The convert page.
    pub(crate) fn render_convert(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();

        let PageData::Convert(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: ConvertModel = (**model).clone();

        let mut routes = div().flex().flex_col().gap_3().p_3();
        for note in &model.notes {
            routes = routes.child(
                mono(note.clone())
                    .text_size(fs(FS_11_5))
                    .text_color(c(WARN_TEXT)),
            );
        }
        if model.sizes.is_empty() {
            routes = routes.child(empty_state(
                report_text::report(language).nothing_to_convert,
            ));
        }
        for size in &model.sizes {
            routes = routes.child(match &size.accounting {
                Some(accounting) => self.route_card(size.size, accounting, cx),
                None => self.no_route_card(size.size, &model, cx),
            });
        }

        div()
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.convert_bar(cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .gap_3()
                    .overflow_hidden()
                    .child(
                        panel()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(panel_header(text.page_convert))
                            .child(routes),
                    )
                    .children(
                        model
                            .maker
                            .as_ref()
                            .map(|maker| self.maker_panel(maker, cx)),
                    ),
            )
    }

    /// The two pickers and the holdings box.
    fn convert_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        div()
            .flex_none()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .pt_3()
            .child(
                div()
                    .text_size(fs(FS_11_5))
                    .text_color(c(TEXT_META))
                    .child(text.convert_have_label),
            )
            .child(
                div().w(px(180.)).child(
                    Select::new(&self.convert_have)
                        .placeholder(text.convert_pick)
                        .with_size(Size::Small),
                ),
            )
            .child(
                div()
                    .text_size(fs(FS_11_5))
                    .text_color(c(TEXT_META))
                    .child(text.convert_need_label),
            )
            .child(
                div().w(px(180.)).child(
                    Select::new(&self.convert_need)
                        .placeholder(text.convert_pick)
                        .with_size(Size::Small),
                ),
            )
            .child(
                div()
                    .text_size(fs(FS_11_5))
                    .text_color(c(TEXT_META))
                    .child(text.convert_holdings_label),
            )
            .child(
                div()
                    .w(px(110.))
                    .child(Input::new(&self.holdings_input).with_size(Size::Small)),
            )
            .child(div().flex_grow())
            .child(
                button("convert-refresh", LedgerButton::Secondary, text.refresh, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.refresh_report(cx);
                        cx.notify();
                    }),
                ),
            )
    }

    /// One size, priced.
    fn route_card(
        &self,
        size: u64,
        accounting: &RouteAccounting,
        _cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let report = report_text::report(language);

        let route = accounting
            .route_asset_ids
            .iter()
            .map(ptt_trade_domain::MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(" → ");

        let mut card = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(c(HAIRLINE_SOFT))
            .bg(c(WELL))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        mono(format!("{size} {}", accounting.requested_input.asset_id))
                            .text_size(fs(FS_12_5)),
                    )
                    .child(mono(route).text_size(fs(FS_11_5)).text_color(c(TEXT_META)))
                    .child(div().flex_grow())
                    .child(chip(
                        actionability_kind(accounting.assessment.actionability),
                        report_text::actionability(language, accounting.assessment.actionability),
                    )),
            );

        // The three tiers stay three rows: they answer different questions,
        // and collapsing them to the friendliest number is how a theoretical
        // profit gets traded.
        for (label, tier) in [
            (report.tier_closed, &accounting.closed),
            (report.tier_theoretical, &accounting.theoretical),
            (report.tier_mark_to_market, &accounting.mark_to_market),
        ] {
            card = card.child(self.tier_row(label, tier, language));
        }

        if accounting.recommended_input.quanta < accounting.requested_input.quanta {
            card = card.child(kv_row(
                text.convert_size_down,
                &report_text::fill(
                    report.size_down_to,
                    &[
                        &accounting.recommended_input.quanta.to_string(),
                        accounting.requested_input.asset_id.as_str(),
                    ],
                ),
            ));
        }
        for residual in &accounting.residuals {
            let break_even = residual.break_even_unit_price.as_ref().map_or_else(
                || report.no_cost_basis.to_owned(),
                |price| report_text::fill(report.break_even_at, &[&price.text]),
            );
            card = card.child(kv_row(
                text.convert_stranded,
                &format!(
                    "{} {}   {break_even}",
                    residual.amount.quanta, residual.asset_id
                ),
            ));
        }
        let blocking = accounting.assessment.blocking();
        if !blocking.is_empty() {
            card = card.child(
                div().pt_1().child(chips(
                    StatusKind::Warning,
                    &blocking
                        .iter()
                        .map(|risk| report_text::execution_risk(language, *risk).to_owned())
                        .collect::<Vec<_>>(),
                    4,
                )),
            );
        }
        card
    }

    /// One profit tier: what went in, what came out, and how that compares.
    fn tier_row(
        &self,
        label: &str,
        tier: &ProfitTier,
        language: ptt_settings::UiLanguage,
    ) -> gpui::Div {
        let report = report_text::report(language);
        let (verdict, colour) = match (tier.direction, &tier.delta, tier.basis_points) {
            (
                Some(ptt_runtime::domain::ComparisonDirection::Improved),
                Some(delta),
                Some(points),
            ) => (
                report_text::fill(
                    report.better_than_direct,
                    &[&delta.quanta.to_string(), &points.to_string()],
                ),
                ACCENT_TEXT,
            ),
            (Some(ptt_runtime::domain::ComparisonDirection::Worse), Some(delta), Some(points)) => (
                report_text::fill(
                    report.worse_than_direct,
                    &[&delta.quanta.to_string(), &points.to_string()],
                ),
                DANGER,
            ),
            (Some(ptt_runtime::domain::ComparisonDirection::Equal), _, _) => {
                (report.level_with_direct.to_owned(), TEXT_SECONDARY)
            }
            _ => (report.no_direct_route.to_owned(), TEXT_META),
        };
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .text_size(fs(FS_11_5))
            .child(
                div()
                    .w(px(96.))
                    .flex_none()
                    .text_color(c(TEXT_META))
                    .child(label.to_owned()),
            )
            .child(
                mono(format!("{} → {}", tier.input.quanta, tier.output.quanta))
                    .text_color(c(TEXT_PRIMARY)),
            )
            .child(mono(verdict).text_color(c(colour)))
    }

    /// A size the search could not route, and the probe that would fix it.
    fn no_route_card(&self, size: u64, model: &ConvertModel, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let (from, to) = (
            model.have.as_str().to_owned(),
            model.need.as_str().to_owned(),
        );
        let pinned = self.probe_queue.is_pinned(&from, &to);
        let reason = report_text::report(language).no_route_for_pair;
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(c(HAIRLINE_SOFT))
            .child(
                mono(report_text::fill(reason, &[&from, &to]))
                    .text_size(fs(FS_11_5))
                    .text_color(c(TEXT_META)),
            )
            .child(mono(format!("×{size}")).text_size(fs(FS_10_5)))
            .child(div().flex_grow())
            .child(if pinned {
                chip(StatusKind::Monitoring, text.pinned_label)
            } else {
                let reason = report_text::fill(reason, &[&from, &to]);
                div().child(
                    button("convert-pin", LedgerButton::Secondary, text.pin_label, cx).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.pin_probe(&from, &to, &reason);
                            cx.notify();
                        }),
                    ),
                )
            })
    }

    /// Listing instead of taking: the three ways to place an order, priced
    /// against the instant fill.
    fn maker_panel(&self, maker: &MakerModel, _cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let report = report_text::report(language);
        let strategy = &maker.strategy;

        let mut body = div().p_3().flex().flex_col().gap_2().child(kv_row(
            text.maker_instant_label,
            &strategy.instant_rate.as_ref().map_or_else(
                || report.maker_no_instant.to_owned(),
                |rate| rate.text.clone(),
            ),
        ));

        if strategy.queue.is_empty() {
            return panel()
                .w(px(420.))
                .flex_none()
                .flex()
                .flex_col()
                .child(panel_header(text.maker_header))
                .child(body.child(empty_state(report.maker_no_book)));
        }

        let modes: Vec<(&str, Option<&MakerRecommendation>)> = vec![
            (
                report.maker_undercut,
                strategy
                    .recommendations
                    .iter()
                    .find(|item| item.mode == MakerMode::Opportunity),
            ),
            (report.maker_match, maker.match_front.as_ref()),
            (
                report.maker_greedy,
                strategy
                    .recommendations
                    .iter()
                    .find(|item| item.mode == MakerMode::Greedy),
            ),
        ];
        for (label, recommendation) in modes {
            let Some(recommendation) = recommendation else {
                continue;
            };
            let gain = if recommendation.beats_instant {
                match (
                    &recommendation.improvement_over_instant,
                    recommendation.improvement_basis_points,
                ) {
                    (Some(delta), Some(points)) => report_text::fill(
                        report.maker_improvement,
                        &[
                            &delta.quanta.to_string(),
                            strategy.to_asset_id.as_str(),
                            &points.to_string(),
                        ],
                    ),
                    _ => String::new(),
                }
            } else {
                report.maker_not_worth.to_owned()
            };
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .border_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                mono(report_text::fill(label, &[&recommendation.rate.text]))
                                    .text_size(fs(FS_11_5)),
                            )
                            .child(div().flex_grow())
                            .child(chip(
                                if recommendation.beats_instant {
                                    StatusKind::Monitoring
                                } else {
                                    StatusKind::Idle
                                },
                                report_text::actionability(
                                    language,
                                    recommendation.assessment.actionability,
                                ),
                            )),
                    )
                    .child(mono(gain).text_size(fs(FS_10_5)).text_color(c(
                        if recommendation.beats_instant {
                            ACCENT_TEXT
                        } else {
                            TEXT_META
                        },
                    ))),
            );
        }

        if let Some(spread) = strategy.spread_basis_points {
            body = body.child(kv_row(
                text.maker_spread_label,
                &report_text::fill(report.maker_spread, &[&spread.to_string()]),
            ));
        }
        if let (Some(depth), Some(cap)) = (
            &strategy.visible_depth_from,
            &strategy.suggested_max_single_order,
        ) {
            body = body.child(kv_row(
                text.maker_depth_label,
                &format!(
                    "{} {}   ≤ {} {}",
                    depth.quanta, strategy.from_asset_id, cap.quanta, strategy.from_asset_id
                ),
            ));
        }

        // The queue itself, front first, with the rows the admission gate
        // threw out shown below it rather than hidden: a listing excluded for
        // being a price outlier is exactly the one a person wants to see.
        let mut queue = div().flex().flex_col().gap_1();
        for level in strategy.queue.iter().take(8) {
            queue = queue.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .text_size(fs(FS_10_5))
                    .child(mono(level.rate.text.clone()).w(px(90.)))
                    .child(
                        mono(level.stock.to_string())
                            .w(px(70.))
                            .text_color(c(TEXT_SECONDARY)),
                    )
                    .child(crate::ui::status_dot(freshness_kind(level.freshness))),
            );
        }
        for excluded in &strategy.excluded {
            queue = queue.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED))
                    .child(mono(excluded.rate.text.clone()).w(px(90.)))
                    .child(mono(excluded.stock.to_string()).w(px(70.)))
                    .child(chip(
                        StatusKind::Error,
                        report_text::maker_exclusion(language, excluded.reason),
                    )),
            );
        }

        panel()
            .w(px(420.))
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.maker_header))
            .child(body)
            .child(div().px_3().pb_3().child(queue))
    }
}
