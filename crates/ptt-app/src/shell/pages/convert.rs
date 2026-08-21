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

/// One row of a currency picker: what the reader sees, and what is picked.
///
/// The two are different strings on purpose. Every layer under the interface
/// speaks catalogue ids, and the interface should never show one — `chaos-orb`
/// is a database key, and a person reading it has to translate before they can
/// act. Carrying both means the translation happens once, here, instead of at
/// every call site remembering to do it.
#[derive(Clone)]
pub struct AssetChoice {
    id: gpui::SharedString,
    label: gpui::SharedString,
    /// Folded forms of both names, the id and any aliases.
    keys: Vec<String>,
}

impl AssetChoice {
    pub fn new(id: String, label: String, keys: Vec<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            keys,
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl gpui_component::select::SelectItem for AssetChoice {
    type Value = gpui::SharedString;

    fn title(&self) -> gpui::SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    /// Matches any of the currency's names, its id or an alias.
    ///
    /// The list shows one name, but a person searching has whichever one they
    /// happen to know: someone running a Chinese interface still thinks
    /// "Divine Orb" from the wiki or a trade whisper, and typing `div` should
    /// find 神聖石 rather than nothing. Punctuation and case are folded away
    /// on both sides, so `divine orb` finds `divine-orb`.
    fn matches(&self, query: &str) -> bool {
        let query = crate::names::fold_query(query);
        query.is_empty() || self.keys.iter().any(|key| key.contains(&query))
    }
}

/// The currency picker's list.
///
/// Its own delegate rather than `SearchableVec`, which searches the title it
/// displays and nothing else: `SelectItem::matches` exists, but
/// `SearchableVec::perform_search` never calls it, so overriding it there is
/// dead code and a Chinese list stays searchable only in Chinese.
pub struct AssetList {
    items: Vec<AssetChoice>,
    matched: Vec<AssetChoice>,
}

impl AssetList {
    #[must_use]
    pub fn new(items: Vec<AssetChoice>) -> Self {
        Self {
            matched: items.clone(),
            items,
        }
    }

    /// The subset a query names.
    ///
    /// A free function so the search can be exercised without a window; this
    /// is the same call `perform_search` makes.
    #[must_use]
    pub fn filter(items: &[AssetChoice], query: &str) -> Vec<AssetChoice> {
        items
            .iter()
            .filter(|item| gpui_component::select::SelectItem::matches(*item, query))
            .cloned()
            .collect()
    }
}

impl gpui_component::select::SelectDelegate for AssetList {
    type Item = AssetChoice;

    fn items_count(&self, _: usize) -> usize {
        self.matched.len()
    }

    fn item(&self, ix: gpui_component::IndexPath) -> Option<&Self::Item> {
        self.matched.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<gpui_component::IndexPath>
    where
        Self::Item: gpui_component::select::SelectItem<Value = V>,
        V: PartialEq,
    {
        // Over the matched set, because that is what `item` indexes.
        self.matched
            .iter()
            .position(|item| gpui_component::select::SelectItem::value(item) == value)
            .map(|row| gpui_component::IndexPath::default().row(row))
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut gpui::Window,
        _: &mut Context<SelectState<Self>>,
    ) -> gpui::Task<()> {
        self.matched = Self::filter(&self.items, query);
        gpui::Task::ready(())
    }
}

/// The currency picker's items.
pub type AssetSelect = Entity<SelectState<AssetList>>;

impl AppShell {
    /// One currency picker.
    pub(crate) fn new_asset_select(
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AssetSelect {
        cx.new(|cx| SelectState::new(AssetList::new(Vec::new()), None, window, cx).searchable(true))
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

    /// Fills the pickers from the catalogue.
    ///
    /// Every currency the game has, not the ones already captured. Offering
    /// only what the book has seen made the page unusable until a panel had
    /// been flipped, and it answered the wrong question besides: "which pairs
    /// do I have data for" is the page's job to report, not the picker's job
    /// to enforce.
    ///
    /// Rebuilt only when the list or the selection changes: replacing the
    /// delegate closes an open menu, and neither changes often.
    pub(crate) fn sync_convert_selects(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Runs every frame, so the list is rebuilt only when what it would
        // contain changes: the catalogue follows the game, the labels follow
        // the interface language, and neither moves while a session runs.
        let key = (std::ptr::from_ref(self.catalog()).addr(), self.language());
        if self.convert_choices_key != Some(key) {
            self.convert_choices_key = Some(key);
            self.convert_choices = self.catalog_choices();
            for select in [
                self.convert_have.clone(),
                self.convert_need.clone(),
                self.settlement_select.clone(),
            ] {
                let items = AssetList::new(self.convert_choices.clone());
                select.update(cx, |state, cx| state.set_items(items, window, cx));
            }
        }
        // The pair the report describes, reflected in the pickers. Assigning
        // what a picker already holds is skipped, so a choice the user just
        // made is not disturbed; a book landing for another pair moves it.
        let Some((have, need)) = self.report_pair.clone() else {
            return;
        };
        for (select, chosen) in [
            (self.convert_have.clone(), have),
            (self.convert_need.clone(), need),
        ] {
            let chosen = gpui::SharedString::from(chosen);
            if select.read(cx).selected_value() == Some(&chosen) {
                continue;
            }
            // The list is reset first because a search leaves it filtered, and
            // the component looks an assigned value up in the filtered set: a
            // picker last searched for "div" cannot be told to hold chaos, and
            // silently empties instead. That is what the swap button looked
            // like before this line existed. It runs only when the pair really
            // changes -- a click, not a frame.
            let items = AssetList::new(self.convert_choices.clone());
            select.update(cx, |state, cx| {
                state.set_items(items, window, cx);
                state.set_selected_value(&chosen, window, cx);
            });
        }
    }

    /// Takes the pair from whatever the two pickers now say.
    ///
    /// Read off the widgets rather than accumulated as the events arrive. The
    /// first version filled the side that had not changed in from
    /// `report_pair`, which holds nothing until a book lands, so on a fresh
    /// session picking one currency was refused for want of the other and
    /// picking the second was refused because the first had never been
    /// recorded. The pickers are what the user chose; they are the place to
    /// ask.
    ///
    /// An explicit choice sticks: once the user has said which pair they are
    /// looking at, an accepted book for some other pair must not drag the page
    /// away mid-thought.
    pub(crate) fn choose_pair(&mut self, cx: &gpui::App) {
        let (Some(have), Some(need)) = (
            self.convert_have.read(cx).selected_value().cloned(),
            self.convert_need.read(cx).selected_value().cloned(),
        ) else {
            return;
        };
        let pair = (have.to_string(), need.to_string());
        self.pair_chosen_by_user = true;
        if self.report_pair.as_ref() == Some(&pair) {
            return;
        }
        self.report_pair = Some(pair);
        self.report_stale = true;
    }

    /// Turns the pair around.
    ///
    /// "What does the other direction look like" is the question that follows
    /// almost every answer this page gives — the two rates are not
    /// reciprocals, they are separate sides of a real book — and re-typing
    /// both currencies to ask it is the kind of friction that stops people
    /// asking.
    ///
    /// Writes the turned-around pair and lets [`Self::sync_convert_selects`]
    /// carry it into the pickers.
    ///
    /// Moving the two pickers here instead does not work, and the way it fails
    /// is quiet: `set_selected_value` does not land in `selected_value` by the
    /// time the call returns, so reading them back to recompute the pair —
    /// which is what every other change on this page does — reads the values
    /// from before the swap, writes them back, and the next frame restores the
    /// pickers to match. The button fires, the pair is read correctly, and
    /// nothing appears to happen.
    pub(crate) fn swap_pair(&mut self, cx: &gpui::App) {
        let (Some(have), Some(need)) = (
            self.convert_have.read(cx).selected_value().cloned(),
            self.convert_need.read(cx).selected_value().cloned(),
        ) else {
            return;
        };
        self.report_pair = Some((need.to_string(), have.to_string()));
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

        // The bar is chrome, not part of the answer. It used to be built
        // inside the branch that had a model, so a page that could not price
        // the pair — nothing captured yet, or both pickers on the same
        // currency — dropped the pickers along with the answer, leaving the
        // reader staring at a message with no way to change the question that
        // produced it.
        let bar = self.convert_bar(cx);

        let PageData::Convert(model) = &self.report else {
            return div()
                .flex_grow()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(bar)
                .child(
                    div().flex_1().flex().flex_col().gap_3().p_3().child(
                        panel()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(empty_state(&self.report_body().join("  "))),
                    ),
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
            .child(bar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .gap_3()
                    .overflow_hidden()
                    .child(
                        panel()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(panel_header(text.page_convert))
                            .child(crate::ui::scrollable(routes, "convert-routes")),
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
                // A glyph rather than a word: it is the same arrow in both
                // interface languages, and the bar has no room for a sentence.
                button("convert-swap", LedgerButton::Quiet, "⇄", cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.swap_pair(cx);
                        cx.notify();
                    },
                )),
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
            .map(|asset| self.display_name(asset.as_str()))
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
                        mono(format!(
                            "{size} {}",
                            self.display_name(accounting.requested_input.asset_id.as_str())
                        ))
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
                        &self.display_name(accounting.requested_input.asset_id.as_str()),
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
        // The sentence names the currencies the way the reader knows them;
        // `from`/`to` stay ids because they are the probe queue's key.
        let reason = report_text::fill(
            report_text::report(language).no_route_for_pair,
            &[&self.display_name(&from), &self.display_name(&to)],
        );
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(c(HAIRLINE_SOFT))
            .child(
                mono(reason.clone())
                    .text_size(fs(FS_11_5))
                    .text_color(c(TEXT_META)),
            )
            .child(mono(format!("×{size}")).text_size(fs(FS_10_5)))
            .child(div().flex_grow())
            .child(if pinned {
                chip(StatusKind::Monitoring, text.pinned_label)
            } else {
                div().child(
                    button(
                        ("convert-pin", usize::try_from(size).unwrap_or(usize::MAX)),
                        LedgerButton::Secondary,
                        text.pin_label,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pin_probe(&from, &to, &reason);
                        cx.notify();
                    })),
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
                            &self.display_name(strategy.to_asset_id.as_str()),
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
                    depth.quanta,
                    self.display_name(strategy.from_asset_id.as_str()),
                    cap.quanta,
                    self.display_name(strategy.from_asset_id.as_str())
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

#[cfg(test)]
mod tests {
    use super::{AssetChoice, AssetList};

    /// The list as a Chinese interface builds it: labels in Chinese, keys
    /// covering every name the currency has.
    fn choices() -> Vec<AssetChoice> {
        ptt_runtime::domain::poe2_catalog()
            .assets()
            .iter()
            .map(|asset| {
                AssetChoice::new(
                    asset.id.replace('_', "-"),
                    asset.name_zh_tw.clone(),
                    crate::names::search_keys(asset),
                )
            })
            .collect()
    }

    fn finds(query: &str, label: &str) -> bool {
        AssetList::filter(&choices(), query)
            .iter()
            .any(|choice| choice.label() == label)
    }

    /// Exercises `AssetList::filter`, which is what `perform_search` calls.
    ///
    /// The version before this one overrode `SelectItem::matches` instead --
    /// a method `SearchableVec::perform_search` never calls -- so the search
    /// silently kept working only in the language the list was displaying.
    /// A test against `matches` would have passed while the product did not.
    #[test]
    fn a_chinese_list_is_searchable_in_english() {
        for query in ["div", "Divine Orb", "divine orb", "DIVINE", "divine-orb"] {
            assert!(finds(query, "神聖石"), "{query:?} should find divine orb");
        }
        // And still in the language it is showing.
        assert!(finds("神聖石", "神聖石"));
    }

    #[test]
    fn an_empty_query_keeps_the_whole_list() {
        let items = choices();
        assert_eq!(AssetList::filter(&items, "").len(), items.len());
    }

    #[test]
    fn a_query_naming_nothing_matches_nothing() {
        assert!(AssetList::filter(&choices(), "zzzznotacurrency").is_empty());
    }
}
