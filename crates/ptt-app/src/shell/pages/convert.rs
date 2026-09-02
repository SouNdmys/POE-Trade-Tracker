//! "I hold X and want Y": what the route returns, and how to place it.
//!
//! The page answers two different questions that used to run together in one
//! column of prose. The upper half prices taking the fill now, at whatever
//! sizes apply. The lower half prices listing instead — undercutting the
//! competing front, matching it, or asking above it — against that same fill,
//! which is the only baseline that makes those three comparable.

use gpui::{
    AppContext as _, Context, Entity, ParentElement, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Sizable, Size, StyledExt as _,
    input::{Input, InputState},
    select::{Select, SelectState},
};
use ptt_runtime::domain::{MakerMode, MakerRecommendation};
use ptt_runtime::report_text;
use ptt_runtime::reports::{ConvertModel, MakerModel, RouteQuote, RouteRate, SizeRoute};

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, chip, empty_state, freshness_kind, kv_row, mono, panel,
    panel_header, warning_band,
};

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
        // The pair the report describes, reflected in the pickers — but only
        // when it changes. The change is detected against our own record of
        // the last pair pushed, never against the picker's `selected_value`:
        // that field does not update synchronously with `set_selected_value`
        // (the swap button was broken by assuming it does), so a guard read
        // from it judged every frame "not yet applied" and re-pushed — and
        // since the push resets the list to clear any search filter, the
        // search box was wiped faster than a person can type. Searching was
        // simply dead while a pair was set, which is always.
        if self.report_pair == self.convert_synced_pair {
            return;
        }
        self.convert_synced_pair = self.report_pair.clone();
        let Some((have, need)) = self.report_pair.clone() else {
            return;
        };
        for (select, chosen) in [
            (self.convert_have.clone(), have),
            (self.convert_need.clone(), need),
        ] {
            let chosen = gpui::SharedString::from(chosen);
            // Rebuilt whole rather than patched. A search leaves two pieces
            // of state behind — the filtered list and the query text — and
            // the component clears neither on its own: patching the list put
            // the last search's text in front of the next one, so a reader
            // who searched "div", picked, and reopened found themselves
            // typing onto "div". A fresh state holds the full list, an empty
            // query and the right selection, and this runs only when the
            // pair changes, so nothing mid-search is ever torn down.
            let index = self
                .convert_choices
                .iter()
                .position(|choice| gpui_component::select::SelectItem::value(choice) == &chosen)
                .map(gpui_component::IndexPath::new);
            let items = AssetList::new(self.convert_choices.clone());
            select.update(cx, |state, cx| {
                *state = SelectState::new(items, index, window, cx).searchable(true);
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

    /// The convert page (§7 定稿 = 11a):一张按持仓算一遍的路线表,深度条
    /// 是主角,右侧路线明细 + 挂单策略。
    pub(crate) fn render_convert(&mut self, cx: &mut Context<Self>) -> gpui::Div {
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

        // 只按一个规模算一遍(§7:不再把同一批路线按 1/10/100 抄三遍)。
        // 填了持仓就是持仓;没填时取配置候选里最大的那档当默认。
        let shown = model
            .sizes
            .iter()
            .filter(|size| !size.quotes.is_empty())
            .max_by_key(|size| {
                if Some(size.size) == self.holdings_value(cx) {
                    u64::MAX
                } else {
                    size.size
                }
            })
            .cloned();

        let mut column = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(SP_8));
        for note in &model.notes {
            // 注意条,不是一段琥珀色的字——理由同关注列表页。
            column = column.child(warning_band(self.text().note_band_tag, note));
        }

        let body: gpui::Div = if let Some(route) = shown {
            let quotes = self.sorted_quotes(&route);
            column = column
                .child(self.convert_band(&route, &quotes, &model))
                .child(self.routes_table(&route, &quotes, cx));
            let selected = self.selected_quote(&quotes).cloned();
            let mut right = div()
                .w(px(W_DETAIL))
                .flex_none()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP_8));
            if let Some(quote) = selected {
                right = right.child(self.route_detail(&quote, route.size, language));
            }
            if let Some(maker) = model.maker.as_ref() {
                right = right.child(self.maker_panel(maker, model.need_structural.as_ref(), cx));
            }
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP_8))
                .overflow_hidden()
                .child(column)
                .child(right)
        } else {
            let size = model.sizes.first().map_or(1, |size| size.size);
            column = column.child(self.no_route_card(size, &model, cx));
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .overflow_hidden()
                .child(column)
        };

        div()
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(bar)
            .child(div().flex_1().min_h(px(0.)).flex().p(px(SP_10)).child(body))
    }

    /// The quotes in the order the toggle asks for.
    ///
    /// 按汇率 = 模型的名次;按吃得下的量 = fillable 降序。两种排序并存,
    /// 因为最优汇率常常做不完(§7)。
    fn sorted_quotes(&self, route: &SizeRoute) -> Vec<RouteQuote> {
        let mut quotes = route.quotes.clone();
        if self.convert_sort_by_depth {
            quotes.sort_by(|left, right| {
                right
                    .fillable_input
                    .unwrap_or(0)
                    .cmp(&left.fillable_input.unwrap_or(0))
            });
        }
        quotes
    }

    /// The selected route, resolved by identity rather than by row index so
    /// a sort toggle keeps the selection on the same route.
    fn selected_quote<'a>(&self, quotes: &'a [RouteQuote]) -> Option<&'a RouteQuote> {
        let wanted = self.convert_selected_route.as_ref()?;
        quotes.iter().find(|quote| {
            quote.route_asset_ids.len() == wanted.len()
                && quote
                    .route_asset_ids
                    .iter()
                    .zip(wanted.iter())
                    .all(|(asset, id)| asset.as_str() == id)
        })
    }

    /// 52px 结论带:理论上最多换到多少 · 比直兑多多少 · 但最好那条吃得下几个。
    fn convert_band(
        &self,
        route: &SizeRoute,
        quotes: &[RouteQuote],
        model: &ConvertModel,
    ) -> gpui::Div {
        let text = self.text();
        let size = route.size;
        let best = quotes
            .iter()
            .filter_map(|quote| quote.projected_output)
            .max();
        let direct = quotes
            .iter()
            .find(|quote| quote.is_direct)
            .and_then(|quote| quote.projected_output);
        let best_quote = quotes
            .iter()
            .max_by_key(|quote| quote.projected_output.unwrap_or(0));
        let thin = best_quote
            .and_then(|quote| quote.fillable_input)
            .filter(|fillable| *fillable < size);

        let divider = || {
            div()
                .w(px(1.))
                .flex_none()
                .my(px(SP_10))
                .bg(c(HAIRLINE_SOFT))
        };
        let mut band = div()
            .h(px(52.))
            .flex_none()
            .flex()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.))
                    .px(px(SP_16))
                    .child(
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap(px(6.))
                            .child(
                                mono(best.map_or_else(|| "—".to_owned(), |out| out.to_string()))
                                    .text_size(fs(FS_20))
                                    .text_color(c(ACCENT_TEXT)),
                            )
                            .child(div().text_size(fs(FS_12)).child(gpui::SharedString::from(
                                self.display_name(model.need.as_str()),
                            ))),
                    )
                    .child(div().text_size(fs(FS_10_5)).text_color(c(TEXT_META)).child(
                        gpui::SharedString::from(report_text::fill(
                            text.convert_band_best,
                            &[&size.to_string(), &self.display_name(model.have.as_str())],
                        )),
                    )),
            );
        if let (Some(best), Some(direct)) = (best, direct) {
            band = band.child(divider()).child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.))
                    .px(px(SP_16))
                    .child(
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap(px(6.))
                            .child(
                                mono(format!("+{}", best.saturating_sub(direct)))
                                    .text_size(fs(FS_15))
                                    .text_color(c(ACCENT_TEXT)),
                            )
                            .child(
                                div()
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(TEXT_META))
                                    .child(text.convert_band_vs),
                            ),
                    )
                    .child(
                        mono(report_text::fill(
                            text.convert_band_direct,
                            &[&direct.to_string()],
                        ))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED)),
                    ),
            );
        }
        if let Some(fillable) = thin {
            band = band.child(divider()).child(
                div()
                    .flex_1()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_10))
                    .px(px(SP_16))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(c(WARN)))
                    .child(div().text_size(fs(FS_12)).text_color(c(WARN_TEXT)).child(
                        gpui::SharedString::from(report_text::fill(
                            text.convert_band_thin,
                            &[&fillable.to_string()],
                        )),
                    )),
            );
        }
        band
    }

    /// The route table: 路线 | 步数 | 整条汇率 | 比直兑 | {size} 换到 | 深度条。
    fn routes_table(
        &self,
        route: &SizeRoute,
        quotes: &[RouteQuote],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        let text = self.text();
        let language = self.language();
        let report = report_text::report(language);
        let size = route.size;
        let max_steps = quotes
            .iter()
            .map(|quote| quote.route_asset_ids.len().saturating_sub(1))
            .max()
            .unwrap_or(1);

        let mut table = panel()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(H_INPUT))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE))
                    .child(crate::ui::micro_title(text.convert_routes_header))
                    .child(
                        mono(report_text::fill(
                            text.convert_routes_meta,
                            &[&quotes.len().to_string(), &max_steps.to_string()],
                        ))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .h_flex()
                            .border_1()
                            .border_color(c(HAIRLINE))
                            .rounded(px(RADIUS_BUTTON))
                            .child(
                                div()
                                    .id("convert-sort-rate")
                                    .h(px(20.))
                                    .px(px(SP_8))
                                    .flex()
                                    .items_center()
                                    .text_size(fs(FS_10_5))
                                    .cursor_pointer()
                                    .map(|cell| {
                                        if self.convert_sort_by_depth {
                                            cell.text_color(c(TEXT_SECONDARY))
                                        } else {
                                            cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
                                        }
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.convert_sort_by_depth = false;
                                        cx.notify();
                                    }))
                                    .child(text.convert_sort_rate),
                            )
                            .child(
                                div()
                                    .id("convert-sort-depth")
                                    .h(px(20.))
                                    .px(px(SP_8))
                                    .flex()
                                    .items_center()
                                    .border_l_1()
                                    .border_color(c(HAIRLINE))
                                    .text_size(fs(FS_10_5))
                                    .cursor_pointer()
                                    .map(|cell| {
                                        if self.convert_sort_by_depth {
                                            cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
                                        } else {
                                            cell.text_color(c(TEXT_SECONDARY))
                                        }
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.convert_sort_by_depth = true;
                                        cx.notify();
                                    }))
                                    .child(text.convert_sort_depth),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(H_ROW))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .px_3()
                    .border_b_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(div().flex_1().child(text.convert_col_route))
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .text_center()
                            .child(text.convert_col_steps),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .flex_none()
                            .text_right()
                            .pr_2()
                            .child(text.convert_col_rate),
                    )
                    .child(
                        div()
                            .w(px(76.))
                            .flex_none()
                            .text_right()
                            .pr_2()
                            .child(text.convert_col_vs),
                    )
                    .child(div().w(px(80.)).flex_none().text_right().pr_2().child(
                        gpui::SharedString::from(report_text::fill(
                            text.convert_col_out,
                            &[&size.to_string()],
                        )),
                    ))
                    .child(div().w(px(154.)).flex_none().child(text.convert_col_depth)),
            );

        let mut zebra = false;
        for (row, quote) in quotes.iter().enumerate() {
            table = table.child(self.route_row(quote, row, size, zebra, cx));
            zebra = !zebra;
        }
        if route.direct_is_the_only_one {
            table = table.child(
                div()
                    .px_3()
                    .py_1()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(gpui::SharedString::from(
                        report.no_route_beats_direct.to_owned(),
                    )),
            );
        }
        // 表尾口径:「能吃下」取全路径最窄的那一段(§7)。
        table.child(div().flex_1()).child(
            div()
                .h(px(H_ROW))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
                .border_t_1()
                .border_color(c(HAIRLINE_SOFT))
                .text_size(fs(FS_10))
                .text_color(c(TEXT_GHOST))
                .child(text.convert_depth_definition),
        )
    }

    /// One route row; the depth bar is the protagonist (§7).
    fn route_row(
        &self,
        quote: &RouteQuote,
        row: usize,
        size: u64,
        zebra: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        let steps = quote.route_asset_ids.len().saturating_sub(1);
        let selected = self.selected_quote(std::slice::from_ref(quote)).is_some();

        // 深度条:满 = 吃得下你全部,琥珀 = 部分,砖红 = 远远不够。
        // 14 条路线的汇率只差 5%,深度却差 100 倍——这根条才是决定。
        let depth_cell = match quote.fillable_input {
            Some(fillable) => {
                #[allow(clippy::cast_precision_loss)]
                let ratio = (fillable as f32 / size.max(1) as f32).min(1.0);
                let (bar_color, text_color) = if fillable >= size {
                    (FRESH, TEXT_DATA)
                } else if ratio >= 0.33 {
                    (WARN, WARN_TEXT)
                } else {
                    (DANGER, DANGER_TEXT)
                };
                div()
                    .w(px(154.))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_8))
                    .child(
                        div()
                            .w(px(88.))
                            .h(px(6.))
                            .flex_none()
                            .bg(c(HAIRLINE))
                            .child(
                                div()
                                    .w(px(88.0 * ratio.max(0.02)))
                                    .h(px(6.))
                                    .bg(c(bar_color)),
                            ),
                    )
                    .child(
                        mono(fillable.to_string())
                            .text_size(fs(FS_11_5))
                            .text_color(c(text_color)),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(gpui::SharedString::from(format!("/ {size}"))),
                    )
            }
            None => div()
                .w(px(154.))
                .flex_none()
                .text_size(fs(FS_11))
                .text_color(c(TEXT_GHOST))
                .child(gpui::SharedString::from("—")),
        };

        // 比直兑:正金负红(唯一批准的色字例外);直兑行自己是基准,给横杠。
        let vs = if quote.is_direct {
            mono("—".to_owned())
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_GHOST))
        } else {
            let (label, color) = match quote.versus_direct_bps {
                Some(points) => (
                    report_text::signed_percent_from_basis_points(points),
                    if points >= 0 {
                        ACCENT_TEXT
                    } else {
                        DANGER_TEXT
                    },
                ),
                None => ("—".to_owned(), TEXT_GHOST),
            };
            mono(label).text_size(fs(FS_11_5)).text_color(c(color))
        };

        let route_ids: Vec<String> = quote
            .route_asset_ids
            .iter()
            .map(|asset| asset.as_str().to_owned())
            .collect();

        let mut line = div()
            .id(("convert-route", row))
            .h(px(H_TABLE_ROW))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .border_b_1()
            .border_color(c(HAIRLINE_SOFT))
            .text_size(fs(FS_12))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.convert_selected_route = Some(route_ids.clone());
                cx.notify();
            }));
        if selected {
            line = line
                .pl(px(10.))
                .border_l_2()
                .border_color(c(ACCENT))
                .bg(c(SELECTED));
        } else if zebra {
            line = line.bg(c(ZEBRA));
            line = line.hover(|style| style.bg(c(HOVER)));
        } else {
            line = line.hover(|style| style.bg(c(HOVER)));
        }

        // 路线铺全路径,幽灵箭头,截断优于换行。
        let mut path = div()
            .flex_1()
            .min_w(px(0.))
            .h_flex()
            .items_center()
            .gap(px(4.))
            .overflow_hidden()
            .text_color(c(TEXT_PRIMARY));
        for (index, asset) in quote.route_asset_ids.iter().enumerate() {
            if index > 0 {
                path = path.child(
                    div()
                        .flex_none()
                        .text_color(c(TEXT_GHOST))
                        .child(gpui::SharedString::from("→")),
                );
            }
            path = path.child(
                div()
                    .whitespace_nowrap()
                    .child(gpui::SharedString::from(self.display_name(asset.as_str()))),
            );
        }

        line.child(path)
            .child(
                mono(steps.to_string())
                    .w(px(44.))
                    .flex_none()
                    .text_center()
                    .text_size(fs(FS_11))
                    .text_color(c(TEXT_SECONDARY)),
            )
            .child(
                mono(quote.rate.map_or_else(|| "—".to_owned(), RouteRate::text))
                    .w(px(96.))
                    .flex_none()
                    .text_right()
                    .pr_2()
                    .text_color(c(TEXT_DATA)),
            )
            .child(div().w(px(76.)).flex_none().text_right().pr_2().child(vs))
            .child(
                mono(
                    quote
                        .projected_output
                        .map_or_else(|| "—".to_owned(), |out| out.to_string()),
                )
                .w(px(80.))
                .flex_none()
                .text_right()
                .pr_2()
                .text_color(c(TEXT_PRIMARY)),
            )
            .child(depth_cell)
    }

    /// 路线明细(右上):每一段的汇率、这一段吃得下多少,卡点标出来。
    fn route_detail(
        &self,
        quote: &RouteQuote,
        size: u64,
        language: ptt_settings::UiLanguage,
    ) -> gpui::Div {
        let text = self.text();
        let pinch = quote.pinch().map(std::ptr::from_ref);
        let mut body = div().px(px(SP_10)).py(px(SP_8)).flex().flex_col();
        body = body.child(crate::ui::kv_headline(
            text.detail_walk_rate,
            &quote.rate.map_or_else(|| "—".to_owned(), RouteRate::text),
            ACCENT_TEXT,
        ));
        body = body.child(kv_row(
            &report_text::fill(text.convert_col_out, &[&size.to_string()]),
            &quote
                .projected_output
                .map_or_else(|| "—".to_owned(), |out| out.to_string()),
        ));
        for (index, leg) in quote.legs.iter().enumerate() {
            let is_pinch = pinch == Some(std::ptr::from_ref(leg));
            let facts = report_text::leg_take_facts(
                language,
                &self.display_name(leg.from_asset_id.as_str()),
                &self.display_name(leg.to_asset_id.as_str()),
                leg,
            );
            let mut row = div()
                .flex()
                .items_start()
                .gap_2()
                .py(px(3.))
                .text_size(fs(FS_11))
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .text_color(c(if is_pinch { WARN_TEXT } else { TEXT_META }))
                        .child(gpui::SharedString::from(crate::i18n::leg_label(
                            language,
                            index + 1,
                        ))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .font_family(FONT_MONO)
                        .text_size(fs(FS_11))
                        .text_color(c(TEXT_SECONDARY))
                        .child(gpui::SharedString::from(facts)),
                );
            if is_pinch {
                row = row.child(crate::ui::chip_table(
                    StatusKind::Warning,
                    text.detail_walk_pinch,
                ));
            }
            body = body.child(row);
        }
        crate::ui::detail_panel(text.detail_header).child(body)
    }

    /// The have/need pickers and the swap glyph.
    ///
    /// 兑换页和历史页共用:两页读写的是同一份"当前查看的通货对",在哪边
    /// 选都改同一对——这是拍板过的语义,不是偷懒。
    pub(crate) fn pair_pickers(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        div()
            .h_flex()
            .items_center()
            .gap_2()
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
            .child(self.pair_pickers(cx))
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
                        this.pin_probe(&from, &to, &reason, false);
                        cx.notify();
                    })),
                )
            })
    }

    /// Listing instead of taking: the three ways to place an order, priced
    /// against the instant fill.
    fn maker_panel(
        &self,
        maker: &MakerModel,
        need_structural: Option<&ptt_runtime::reports::StructuralNote>,
        _cx: &mut Context<Self>,
    ) -> gpui::Div {
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
                .flex_none()
                .flex()
                .flex_col()
                .child(panel_header(text.maker_header))
                .child(body.child(empty_state(report.maker_no_book)));
        }

        // 每档配一句代价(§7):原来只有百分比,看不出为什么不总选最贪的。
        let modes: Vec<(&str, &'static str, Option<&MakerRecommendation>)> = vec![
            (
                report.maker_undercut,
                text.maker_cost_undercut,
                strategy
                    .recommendations
                    .iter()
                    .find(|item| item.mode == MakerMode::Opportunity),
            ),
            (
                report.maker_match,
                text.maker_cost_match,
                maker.match_front.as_ref(),
            ),
            (
                report.maker_greedy,
                text.maker_cost_greedy,
                strategy
                    .recommendations
                    .iter()
                    .find(|item| item.mode == MakerMode::Greedy),
            ),
        ];
        for (label, cost, recommendation) in modes {
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
                            &report_text::percent_from_basis_points(points),
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
                            // 「要等人吃单」那类徽章去掉了:它不触发任何操作,
                            // 只是标签。位置换成这一档的代价。
                            .child(
                                div()
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(TEXT_META))
                                    .child(gpui::SharedString::from(cost.to_string())),
                            ),
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

        // The greedy decision is about the asset you would end up holding:
        // is its market scarce and drifting up (the greedy precondition), or
        // oversupplied junk? Advisory context from the season pulse.
        if let Some(note) = need_structural {
            body = body.child(kv_row(text.detail_structural, &self.structural_text(note)));
        }

        if let Some(spread) = strategy.spread_basis_points {
            body = body.child(kv_row(
                text.maker_spread_label,
                &report_text::fill(
                    report.maker_spread,
                    &[&report_text::percent_from_basis_points(spread)],
                ),
            ));
        }
        if let (Some(depth), Some(cap)) = (
            &strategy.visible_depth_from,
            &strategy.suggested_max_single_order,
        ) {
            // The row's label already says "visible depth", so the value
            // carries the depth and then the ceiling *in words*. It used to
            // join them with a bare "≤", which reads as the claim that
            // 104,178 is at most 19,823 -- two facts wearing the shape of one
            // false comparison.
            body = body.child(kv_row(
                text.maker_depth_label,
                &format!(
                    "{} {}   {}",
                    depth.quanta,
                    self.display_name(strategy.from_asset_id.as_str()),
                    report_text::fill(
                        report.maker_max_single,
                        &[
                            &cap.quanta.to_string(),
                            &self.display_name(strategy.from_asset_id.as_str()),
                        ],
                    ),
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
