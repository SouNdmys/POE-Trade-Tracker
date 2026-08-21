//! The market tuning editors on the settings page.
//!
//! The settings file holds plain numbers and the consumers turn them into
//! domain types, refusing nonsense and falling back to the shipped defaults
//! with a visible note. That stays true — a hand-edited file still has to be
//! survivable — so this is a second gate rather than the only one: a value
//! the runtime would reject is refused here, in place, instead of being
//! written and silently ignored.

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, ParentElement,
    StatefulInteractiveElement as _, Styled, div, px,
};
use gpui_component::{
    Sizable, Size, StyledExt as _,
    input::{Input, InputState},
    select::Select,
};

use crate::shell::AppShell;
use crate::theme::*;
use crate::ui::{LedgerButton, StatusKind, button, chip, mono, panel, panel_header};

/// Every number the market tuning holds, as boxes that survive a refresh.
pub struct TuningInputs {
    pub fresh: Entity<InputState>,
    pub usable: Entity<InputState>,
    pub stale: Entity<InputState>,
    pub skew: Entity<InputState>,
    pub sizes: Entity<InputState>,
    pub max_hops: Entity<InputState>,
    pub stake: Entity<InputState>,
    pub max_results: Entity<InputState>,
    pub min_basis_points: Entity<InputState>,
    pub expansions: Entity<InputState>,
    pub thin_stock: Entity<InputState>,
    pub outlier_factor: Entity<InputState>,
    pub window_hours: Entity<InputState>,
}

impl TuningInputs {
    /// Every box, in the order the page draws them.
    fn all(&self) -> [&Entity<InputState>; 13] {
        [
            &self.fresh,
            &self.usable,
            &self.stale,
            &self.skew,
            &self.sizes,
            &self.max_hops,
            &self.stake,
            &self.max_results,
            &self.min_basis_points,
            &self.expansions,
            &self.thin_stock,
            &self.outlier_factor,
            &self.window_hours,
        ]
    }
}

/// A whole number from a box, or nothing if it does not hold one.
fn number(input: &Entity<InputState>, cx: &gpui::App) -> Option<u64> {
    input.read(cx).value().trim().parse::<u64>().ok()
}

/// The convert ladder, which is a list rather than a number.
fn sizes(input: &Entity<InputState>, cx: &gpui::App) -> Option<Vec<u64>> {
    let text = input.read(cx).value();
    let parsed: Option<Vec<u64>> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().ok().filter(|size| *size > 0))
        .collect();
    parsed.filter(|list| !list.is_empty())
}

impl AppShell {
    #[cfg(windows)]
    pub(crate) fn new_tuning_inputs(
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
        tuning: &ptt_settings::MarketTuning,
    ) -> TuningInputs {
        let mut make =
            |value: String| cx.new(|cx| InputState::new(window, cx).default_value(value));
        TuningInputs {
            fresh: make(tuning.freshness.fresh_seconds.to_string()),
            usable: make(tuning.freshness.usable_seconds.to_string()),
            stale: make(tuning.freshness.stale_seconds.to_string()),
            skew: make(tuning.freshness.capture_skew_seconds.to_string()),
            sizes: make(
                tuning
                    .convert
                    .sizes
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            max_hops: make(tuning.convert.max_hops.to_string()),
            stake: make(tuning.radar.stake.to_string()),
            max_results: make(tuning.radar.max_results.to_string()),
            min_basis_points: make(tuning.radar.minimum_profit_basis_points.to_string()),
            expansions: make(tuning.radar.max_total_expansions.to_string()),
            thin_stock: make(tuning.risk.thin_liquidity_stock.to_string()),
            outlier_factor: make(tuning.risk.top_book_outlier_factor.to_string()),
            window_hours: make(tuning.report_window_hours.to_string()),
        }
    }

    /// Flips whether routes may pass through a focus target.
    #[cfg(windows)]
    pub(crate) fn set_route_through_targets(&mut self, allow: bool) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if tuning.route_through_targets == allow {
                return;
            }
            tuning.route_through_targets = allow;
        }
        self.save_tuning();
    }

    /// Adds whatever the settlement picker names.
    #[cfg(windows)]
    pub(crate) fn add_settlement(&mut self, cx: &gpui::App) {
        let Some(asset) = self
            .settlement_select
            .read(cx)
            .selected_value()
            .map(std::string::ToString::to_string)
        else {
            return;
        };
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if tuning.settlement_assets.contains(&asset) {
                return;
            }
            tuning.settlement_assets.push(asset);
            tuning.settlement_assets.sort();
        }
        self.save_tuning();
    }

    /// Drops one settlement currency. Never the last.
    #[cfg(windows)]
    pub(crate) fn remove_settlement(&mut self, asset: &str) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if tuning.settlement_assets.len() <= 1 {
                return;
            }
            tuning.settlement_assets.retain(|held| held != asset);
        }
        self.save_tuning();
    }

    /// Writes the settings and marks the answer on screen out of date.
    #[cfg(windows)]
    fn save_tuning(&mut self) {
        match self.settings_store.save(&self.settings) {
            // Every page prices against the settlement set, so all of them
            // are now describing a market that no longer exists.
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// Reads every box and writes the settings, if all of them hold a value
    /// the runtime would accept.
    ///
    /// All or nothing: the freshness bands are only meaningful in relation to
    /// each other, so writing a valid `fresh` while `usable` is mid-edit
    /// would persist a moment that was never a coherent configuration.
    #[cfg(windows)]
    pub(crate) fn apply_tuning(&mut self, cx: &gpui::App) -> bool {
        let inputs = &self.tuning_inputs;
        let (Some(fresh), Some(usable), Some(stale), Some(skew)) = (
            number(&inputs.fresh, cx),
            number(&inputs.usable, cx),
            number(&inputs.stale, cx),
            number(&inputs.skew, cx),
        ) else {
            return false;
        };
        // The same ordering `FreshnessPolicy::try_new` insists on: equal
        // bounds would collapse the amber band, and the amber band is the
        // whole point of the traffic light.
        if !(0 < fresh && fresh < usable && usable < stale) {
            return false;
        }
        let (
            Some(sizes),
            Some(max_hops),
            Some(stake),
            Some(max_results),
            Some(min_basis_points),
            Some(expansions),
            Some(thin_stock),
            Some(outlier_factor),
            Some(window_hours),
        ) = (
            sizes(&inputs.sizes, cx),
            number(&inputs.max_hops, cx),
            number(&inputs.stake, cx),
            number(&inputs.max_results, cx),
            number(&inputs.min_basis_points, cx),
            number(&inputs.expansions, cx),
            number(&inputs.thin_stock, cx),
            number(&inputs.outlier_factor, cx),
            number(&inputs.window_hours, cx),
        )
        else {
            return false;
        };
        if max_hops == 0 || stake == 0 || max_results == 0 || window_hours == 0 {
            return false;
        }
        // An outlier band of one admits nothing, and a band of zero is not a
        // band; either way the top-book gate would stop doing its job.
        if outlier_factor < 2 {
            return false;
        }

        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            tuning.freshness.fresh_seconds = fresh;
            tuning.freshness.usable_seconds = usable;
            tuning.freshness.stale_seconds = stale;
            tuning.freshness.capture_skew_seconds = skew;
            tuning.convert.sizes = sizes;
            tuning.convert.max_hops = max_hops;
            tuning.radar.stake = stake;
            tuning.radar.max_results = max_results;
            tuning.radar.minimum_profit_basis_points = min_basis_points;
            tuning.radar.max_total_expansions = expansions;
            tuning.risk.thin_liquidity_stock = thin_stock;
            tuning.risk.top_book_outlier_factor = outlier_factor;
            tuning.report_window_hours = window_hours;
        }
        match self.settings_store.save(&self.settings) {
            Ok(()) => {
                self.report_stale = true;
                true
            }
            Err(error) => {
                self.push_log(format!("settings save failed: {error}"));
                false
            }
        }
    }

    /// Whether every box currently holds something writable.
    #[cfg(windows)]
    pub(crate) fn tuning_is_valid(&self, cx: &gpui::App) -> bool {
        let inputs = &self.tuning_inputs;
        if inputs.all().iter().any(|input| {
            let value = input.read(cx).value();
            value.trim().is_empty()
        }) {
            return false;
        }
        let (Some(fresh), Some(usable), Some(stale)) = (
            number(&inputs.fresh, cx),
            number(&inputs.usable, cx),
            number(&inputs.stale, cx),
        ) else {
            return false;
        };
        0 < fresh && fresh < usable && usable < stale && sizes(&inputs.sizes, cx).is_some()
    }

    /// The market tuning section.
    #[cfg(windows)]
    pub(crate) fn tuning_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let valid = self.tuning_is_valid(cx);
        let tuning = self
            .settings
            .market_tuning(self.settings.active_profile.game);
        let inputs = &self.tuning_inputs;

        let row = |label: &'static str, input: &Entity<InputState>, hint: String| {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(label),
                )
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .child(Input::new(input).with_size(Size::Small)),
                )
                .child(
                    mono(hint)
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED)),
                )
        };

        // Editable here and nowhere else. Changing the numéraire changes
        // what every number on every page means, so it belongs on the settings
        // screen rather than as a click beside a row on a page of results.
        let removable = tuning.settlement_assets.len() > 1;
        let mut settlement = div().h_flex().items_center().gap_2().flex_wrap();
        for (index, asset) in tuning.settlement_assets.iter().enumerate() {
            let held = asset.clone();
            let mut tag = div()
                .id(("settlement-chip", index))
                .h_flex()
                .items_center()
                .gap_1()
                .child(chip(StatusKind::Monitoring, &self.display_name(asset)));
            if removable {
                tag = tag
                    .cursor_pointer()
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_DISABLED))
                            .child("×"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_settlement(&held);
                        cx.notify();
                    }));
            }
            settlement = settlement.child(tag);
        }
        settlement = settlement
            .child(
                div().w(px(170.)).child(
                    Select::new(&self.settlement_select)
                        .placeholder(text.convert_pick)
                        .with_size(Size::Small),
                ),
            )
            .child(
                button(
                    "settlement-add",
                    LedgerButton::Secondary,
                    text.settlement_add,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.add_settlement(cx);
                    cx.notify();
                })),
            );
        // Stated rather than left to a disabled control: a market with nothing
        // to price against is not a configuration, and silence would read as a
        // broken button.
        if !removable {
            settlement = settlement.child(
                mono(text.settlement_last)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED)),
            );
        }

        panel().child(panel_header(text.tuning_header)).child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(150.))
                                .flex_none()
                                .text_size(fs(FS_11_5))
                                .text_color(c(TEXT_META))
                                .child(text.settlement_label),
                        )
                        .child(settlement),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(150.))
                                .flex_none()
                                .text_size(fs(FS_11_5))
                                .text_color(c(TEXT_META))
                                .child(text.route_through_targets),
                        )
                        .child({
                            let allow = tuning.route_through_targets;
                            let mut cells = div()
                                .h_flex()
                                .items_center()
                                .flex_none()
                                .border_1()
                                .border_color(c(HAIRLINE));
                            for (index, (label, value)) in
                                [(text.toggle_on, true), (text.toggle_off, false)]
                                    .into_iter()
                                    .enumerate()
                            {
                                let mut cell = div()
                                    .id(("route-through", index))
                                    .h(px(H_ROW))
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .text_size(fs(FS_11_5))
                                    .whitespace_nowrap()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_route_through_targets(value);
                                        cx.notify();
                                    }));
                                if index > 0 {
                                    cell = cell.border_l_1().border_color(c(HAIRLINE));
                                }
                                cell = if value == allow {
                                    cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
                                } else {
                                    cell.bg(c(PANEL))
                                        .text_color(c(TEXT_SECONDARY))
                                        .hover(|style| style.bg(c(HOVER)))
                                };
                                cells = cells.child(cell.child(label));
                            }
                            cells
                        }),
                )
                .child(row(text.tuning_fresh, &inputs.fresh, "7200".to_owned()))
                .child(row(text.tuning_usable, &inputs.usable, "21600".to_owned()))
                .child(row(text.tuning_stale, &inputs.stale, "86400".to_owned()))
                .child(row(text.tuning_skew, &inputs.skew, "3600".to_owned()))
                .child(row(
                    text.tuning_sizes,
                    &inputs.sizes,
                    "1, 10, 100".to_owned(),
                ))
                .child(row(text.tuning_max_hops, &inputs.max_hops, "3".to_owned()))
                .child(row(text.tuning_stake, &inputs.stake, "10".to_owned()))
                .child(row(
                    text.tuning_results,
                    &inputs.max_results,
                    "12".to_owned(),
                ))
                .child(row(
                    text.tuning_min_bps,
                    &inputs.min_basis_points,
                    "100".to_owned(),
                ))
                .child(row(
                    text.tuning_expansions,
                    &inputs.expansions,
                    "60000".to_owned(),
                ))
                .child(row(text.tuning_thin, &inputs.thin_stock, "100".to_owned()))
                .child(row(
                    text.tuning_outlier,
                    &inputs.outlier_factor,
                    "3".to_owned(),
                ))
                .child(row(
                    text.tuning_window,
                    &inputs.window_hours,
                    "24".to_owned(),
                ))
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .pt_2()
                        .child(
                            button(
                                "tuning-apply",
                                if valid {
                                    LedgerButton::Primary
                                } else {
                                    LedgerButton::Quiet
                                },
                                text.tuning_apply,
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.apply_tuning(cx) {
                                    this.push_log(this.text().tuning_saved.to_owned());
                                }
                                cx.notify();
                            })),
                        )
                        .children((!valid).then(|| {
                            mono(text.tuning_invalid)
                                .text_size(fs(FS_10_5))
                                .text_color(c(DANGER))
                        })),
                ),
        )
    }
}
