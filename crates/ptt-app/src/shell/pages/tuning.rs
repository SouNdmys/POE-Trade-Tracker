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
use crate::ui::{LedgerButton, StatusKind, button, chip, mono, panel};

/// Every number the market tuning holds, as boxes that survive a refresh.
pub struct TuningInputs {
    pub fresh: Entity<InputState>,
    pub usable: Entity<InputState>,
    pub stale: Entity<InputState>,
    pub skew: Entity<InputState>,
    pub sizes: Entity<InputState>,
    pub max_hops: Entity<InputState>,
    pub max_results: Entity<InputState>,
    pub min_basis_points: Entity<InputState>,
    pub expansions: Entity<InputState>,
    pub thin_stock: Entity<InputState>,
    pub outlier_factor: Entity<InputState>,
    pub window_hours: Entity<InputState>,
    pub trend_recent: Entity<InputState>,
    pub trend_window: Entity<InputState>,
    pub breadth: Entity<InputState>,
    pub verdict_bps: Entity<InputState>,
    pub scarce_ratio: Entity<InputState>,
    pub quiet_floor: Entity<InputState>,
    pub thin_norm: Entity<InputState>,
    pub retention_days: Entity<InputState>,
    pub spike: Entity<InputState>,
    pub severe_spike: Entity<InputState>,
    pub wide_spread: Entity<InputState>,
    pub severe_spread: Entity<InputState>,
}

impl TuningInputs {
    /// Every box, in the order the page draws them.
    fn all(&self) -> [&Entity<InputState>; 24] {
        [
            &self.fresh,
            &self.usable,
            &self.stale,
            &self.skew,
            &self.sizes,
            &self.max_hops,
            &self.max_results,
            &self.min_basis_points,
            &self.expansions,
            &self.thin_stock,
            &self.outlier_factor,
            &self.window_hours,
            &self.trend_recent,
            &self.trend_window,
            &self.breadth,
            &self.verdict_bps,
            &self.scarce_ratio,
            &self.quiet_floor,
            &self.thin_norm,
            &self.retention_days,
            &self.spike,
            &self.severe_spike,
            &self.wide_spread,
            &self.severe_spread,
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
            max_results: make(tuning.radar.max_results.to_string()),
            min_basis_points: make(tuning.radar.minimum_profit_basis_points.to_string()),
            expansions: make(tuning.radar.max_total_expansions.to_string()),
            thin_stock: make(tuning.risk.thin_liquidity_stock.to_string()),
            outlier_factor: make(tuning.risk.top_book_outlier_factor.to_string()),
            window_hours: make(tuning.report_window_hours.to_string()),
            trend_recent: make(tuning.analytics.trend_recent_days.to_string()),
            trend_window: make(tuning.analytics.trend_window_days.to_string()),
            breadth: make(tuning.analytics.breadth_threshold_percent.to_string()),
            verdict_bps: make(tuning.analytics.verdict_threshold_bps.to_string()),
            scarce_ratio: make(tuning.analytics.scarce_ratio_percent.to_string()),
            quiet_floor: make(tuning.analytics.quiet_floor_anchor_units.to_string()),
            thin_norm: make(tuning.analytics.thin_norm_percent.to_string()),
            retention_days: make(tuning.analytics.raw_retention_days.to_string()),
            spike: make(tuning.risk.spike_basis_points.to_string()),
            severe_spike: make(tuning.risk.severe_spike_basis_points.to_string()),
            wide_spread: make(tuning.risk.wide_spread_basis_points.to_string()),
            severe_spread: make(tuning.risk.severe_spread_basis_points.to_string()),
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
            // 删掉的正是锚:清空显式选择,回落到列表第一个,别让设置里
            // 留着一个指向已删除通货的锚。
            if tuning.anchor_asset.as_deref() == Some(asset) {
                tuning.anchor_asset = None;
            }
        }
        self.save_tuning();
    }

    /// Makes one settlement currency the anchor everything is valued against.
    #[cfg(windows)]
    pub(crate) fn set_anchor(&mut self, asset: &str) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if !tuning.settlement_assets.iter().any(|held| held == asset)
                || tuning.anchor_asset.as_deref() == Some(asset)
            {
                return;
            }
            tuning.anchor_asset = Some(asset.to_owned());
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
            Some(max_results),
            Some(min_basis_points),
            Some(expansions),
            Some(thin_stock),
            Some(outlier_factor),
            Some(window_hours),
        ) = (
            sizes(&inputs.sizes, cx),
            number(&inputs.max_hops, cx),
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
        if max_hops == 0 || max_results == 0 || window_hours == 0 {
            return false;
        }
        // An outlier band of one admits nothing, and a band of zero is not a
        // band; either way the top-book gate would stop doing its job.
        if outlier_factor < 2 {
            return false;
        }
        let (
            Some(trend_recent),
            Some(trend_window),
            Some(breadth),
            Some(verdict_bps),
            Some(scarce_ratio),
            Some(quiet_floor),
            Some(thin_norm),
            Some(retention_days),
        ) = (
            number(&inputs.trend_recent, cx),
            number(&inputs.trend_window, cx),
            number(&inputs.breadth, cx),
            number(&inputs.verdict_bps, cx),
            number(&inputs.scarce_ratio, cx),
            number(&inputs.quiet_floor, cx),
            number(&inputs.thin_norm, cx),
            number(&inputs.retention_days, cx),
        )
        else {
            return false;
        };
        // Mirrors `AnalyticsThresholds::try_new`: values the runtime would
        // reject are refused here instead of written and silently defaulted.
        if trend_recent == 0
            || trend_window == 0
            || !(1..=100).contains(&breadth)
            || verdict_bps == 0
            || scarce_ratio < 100
            || thin_norm > 100
        {
            return false;
        }
        let (Some(spike), Some(severe_spike), Some(wide_spread), Some(severe_spread)) = (
            number(&inputs.spike, cx),
            number(&inputs.severe_spike, cx),
            number(&inputs.wide_spread, cx),
            number(&inputs.severe_spread, cx),
        ) else {
            return false;
        };
        // Severity must sit at or above its trigger, or "severe" fires first.
        if spike == 0 || severe_spike < spike || wide_spread == 0 || severe_spread < wide_spread {
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
            tuning.radar.max_results = max_results;
            tuning.radar.minimum_profit_basis_points = min_basis_points;
            tuning.radar.max_total_expansions = expansions;
            tuning.risk.thin_liquidity_stock = thin_stock;
            tuning.risk.top_book_outlier_factor = outlier_factor;
            tuning.risk.spike_basis_points = spike;
            tuning.risk.severe_spike_basis_points = severe_spike;
            tuning.risk.wide_spread_basis_points = wide_spread;
            tuning.risk.severe_spread_basis_points = severe_spread;
            tuning.report_window_hours = window_hours;
            tuning.analytics.trend_recent_days = trend_recent;
            tuning.analytics.trend_window_days = trend_window;
            tuning.analytics.breadth_threshold_percent = breadth;
            tuning.analytics.verdict_threshold_bps = verdict_bps;
            tuning.analytics.scarce_ratio_percent = scarce_ratio;
            tuning.analytics.quiet_floor_anchor_units = quiet_floor;
            tuning.analytics.thin_norm_percent = thin_norm;
            tuning.analytics.raw_retention_days = retention_days;
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

    /// 顶部通栏(§10):结算通货与「允许路过关注目标」。它们不属于任何
    /// 一组,且影响所有页面,所以放在四个分段之上,每个分段都看得见。
    #[cfg(windows)]
    pub(crate) fn settings_banner(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let tuning = self
            .settings
            .market_tuning(self.settings.active_profile.game);

        // Editable here and nowhere else. Changing the numéraire changes
        // what every number on every page means, so it belongs on the settings
        // screen rather than as a click beside a row on a page of results.
        let removable = tuning.settlement_assets.len() > 1;
        // 生效的锚和 policy 装配层同一条规则:显式选择,不在列表里就当没选,
        // 回落列表第一个。
        let anchor: String = match tuning
            .anchor_asset
            .as_ref()
            .filter(|anchor| tuning.settlement_assets.contains(anchor))
        {
            Some(anchor) => anchor.clone(),
            None => tuning
                .settlement_assets
                .first()
                .cloned()
                .unwrap_or_default(),
        };
        let mut settlement = div().h_flex().items_center().gap_2().flex_wrap();
        for (index, asset) in tuning.settlement_assets.iter().enumerate() {
            let mut tag = div()
                .id(("settlement-chip", index))
                .h_flex()
                .items_center()
                .gap_1()
                .child(chip(StatusKind::Monitoring, &self.display_name(asset)));
            if *asset == anchor {
                // 锚徽:金字说明"所有价格以它计"。不可点,它已经是基准。
                tag = tag.child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(ACCENT_TEXT))
                        .child(text.anchor_badge),
                );
            } else {
                let target = asset.clone();
                tag = tag.child(
                    div()
                        .id(("anchor-set", index))
                        .cursor_pointer()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .hover(|style| style.text_color(c(ACCENT_TEXT)))
                        .child(text.anchor_set)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_anchor(&target);
                            cx.notify();
                        })),
                );
            }
            if removable {
                let held = asset.clone();
                // × 是自己的点击目标:整个 chip 当删除热区的话,「设为锚」
                // 一点就会连带把通货删了。
                tag = tag.child(
                    div()
                        .id(("settlement-remove", index))
                        .cursor_pointer()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .hover(|style| style.text_color(c(DANGER_TEXT)))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_settlement(&held);
                            cx.notify();
                        })),
                );
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

        let allow = tuning.route_through_targets;
        let mut toggle = div()
            .h_flex()
            .items_center()
            .flex_none()
            .border_1()
            .border_color(c(HAIRLINE));
        for (index, (label, value)) in [(text.toggle_on, true), (text.toggle_off, false)]
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
            toggle = toggle.child(cell.child(label));
        }

        panel().flex_none().child(
            div()
                .h_flex()
                .items_center()
                .gap(px(SP_16))
                .px_3()
                .py_2()
                .flex_wrap()
                .child(
                    div()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(text.settlement_label),
                )
                .child(settlement)
                .child(div().w(px(1.)).h(px(20.)).bg(c(HAIRLINE_SOFT)))
                .child(
                    div()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(text.route_through_targets),
                )
                .child(toggle),
        )
    }

    /// The stored tuning, rendered to the same strings the boxes were seeded
    /// with — the "changes not applied" count is a string comparison against
    /// this, so this construction must stay a twin of
    /// [`AppShell::new_tuning_inputs`].
    #[cfg(windows)]
    fn tuning_stored_values(tuning: &ptt_settings::MarketTuning) -> [String; 24] {
        [
            tuning.freshness.fresh_seconds.to_string(),
            tuning.freshness.usable_seconds.to_string(),
            tuning.freshness.stale_seconds.to_string(),
            tuning.freshness.capture_skew_seconds.to_string(),
            tuning
                .convert
                .sizes
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            tuning.convert.max_hops.to_string(),
            tuning.radar.max_results.to_string(),
            tuning.radar.minimum_profit_basis_points.to_string(),
            tuning.radar.max_total_expansions.to_string(),
            tuning.risk.thin_liquidity_stock.to_string(),
            tuning.risk.top_book_outlier_factor.to_string(),
            tuning.report_window_hours.to_string(),
            tuning.analytics.trend_recent_days.to_string(),
            tuning.analytics.trend_window_days.to_string(),
            tuning.analytics.breadth_threshold_percent.to_string(),
            tuning.analytics.verdict_threshold_bps.to_string(),
            tuning.analytics.scarce_ratio_percent.to_string(),
            tuning.analytics.quiet_floor_anchor_units.to_string(),
            tuning.analytics.thin_norm_percent.to_string(),
            tuning.analytics.raw_retention_days.to_string(),
            tuning.risk.spike_basis_points.to_string(),
            tuning.risk.severe_spike_basis_points.to_string(),
            tuning.risk.wide_spread_basis_points.to_string(),
            tuning.risk.severe_spread_basis_points.to_string(),
        ]
    }

    /// The market tuning section: 24 numbers in six groups, wrapped three to
    /// a row so one screen holds them, each with its human conversion (§10).
    #[cfg(windows)]
    pub(crate) fn tuning_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let valid = self.tuning_is_valid(cx);
        let tuning = self
            .settings
            .market_tuning(self.settings.active_profile.game);
        let inputs = &self.tuning_inputs;

        // 改动未应用的计数:框里的字符串对存储值的字符串,一对一比。
        let stored = Self::tuning_stored_values(&tuning);
        let pending = inputs
            .all()
            .iter()
            .zip(stored.iter())
            .filter(|(input, stored)| input.read(cx).value().trim() != stored.as_str())
            .count();

        let cell =
            |label: &'static str, input: &Entity<InputState>, unit: Unit, note: &'static str| {
                let converted = unit.conversion(text, number(input, cx));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(fs(FS_11))
                            .text_color(c(TEXT_META))
                            .child(label),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .child(Input::new(input).with_size(Size::Small)),
                            )
                            .children(converted.map(|value| {
                                // 换算是金字:它是主题在替你读数,不是又一个语义色。
                                mono(value)
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(ACCENT_TEXT))
                            })),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_GHOST))
                            .child(note),
                    )
            };
        // 一组一张卡(13a):panel 底 + 24px 组头,三列摞放,一屏装下不用滚。
        // 原来是通栏分节线,组与组的边界在 24 个输入框里读不出来。
        let card = |title: &'static str, cells: Vec<gpui::Div>| {
            let mut body = div().flex().flex_col().gap_2().px_3().py_2();
            for one in cells {
                body = body.child(one);
            }
            div()
                .flex_none()
                .flex()
                .flex_col()
                .bg(c(PANEL))
                .border_1()
                .border_color(c(HAIRLINE))
                .child(
                    div()
                        .h(px(H_ROW))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .px_3()
                        .bg(c(RAIL))
                        .border_b_1()
                        .border_color(c(HAIRLINE))
                        .child(crate::ui::micro_title_sm(title)),
                )
                .child(body)
        };
        let column = |cards: Vec<gpui::Div>| {
            let mut col = div().flex_1().min_w(px(0.)).flex().flex_col().gap_2();
            for one in cards {
                col = col.child(one);
            }
            col
        };

        let freshness_card = card(
            text.group_freshness,
            vec![
                cell(
                    text.tuning_fresh,
                    &inputs.fresh,
                    Unit::Seconds,
                    text.tuning_fresh_note,
                ),
                cell(
                    text.tuning_usable,
                    &inputs.usable,
                    Unit::Seconds,
                    text.tuning_usable_note,
                ),
                cell(
                    text.tuning_stale,
                    &inputs.stale,
                    Unit::Seconds,
                    text.tuning_stale_note,
                ),
                cell(
                    text.tuning_skew,
                    &inputs.skew,
                    Unit::Seconds,
                    text.tuning_skew_note,
                ),
            ],
        );
        let scan_card = card(
            text.group_scan,
            vec![
                cell(
                    text.tuning_sizes,
                    &inputs.sizes,
                    Unit::None,
                    text.tuning_sizes_note,
                ),
                cell(
                    text.tuning_max_hops,
                    &inputs.max_hops,
                    Unit::None,
                    text.tuning_max_hops_note,
                ),
                cell(
                    text.tuning_results,
                    &inputs.max_results,
                    Unit::None,
                    text.tuning_results_note,
                ),
                cell(
                    text.tuning_min_bps,
                    &inputs.min_basis_points,
                    Unit::BasisPoints,
                    text.tuning_min_bps_note,
                ),
                cell(
                    text.tuning_expansions,
                    &inputs.expansions,
                    Unit::None,
                    text.tuning_expansions_note,
                ),
                cell(
                    text.tuning_window,
                    &inputs.window_hours,
                    Unit::None,
                    text.tuning_window_note,
                ),
            ],
        );
        let liquidity_card = card(
            text.group_liquidity,
            vec![
                cell(
                    text.tuning_thin,
                    &inputs.thin_stock,
                    Unit::None,
                    text.tuning_thin_note,
                ),
                cell(
                    text.tuning_outlier,
                    &inputs.outlier_factor,
                    Unit::None,
                    text.tuning_outlier_note,
                ),
                cell(
                    text.tuning_quiet,
                    &inputs.quiet_floor,
                    Unit::None,
                    text.tuning_quiet_note,
                ),
                cell(
                    text.tuning_thin_norm,
                    &inputs.thin_norm,
                    Unit::None,
                    text.tuning_thin_norm_note,
                ),
            ],
        );
        let trend_card = card(
            text.group_trend,
            vec![
                cell(
                    text.tuning_trend_recent,
                    &inputs.trend_recent,
                    Unit::None,
                    text.tuning_trend_recent_note,
                ),
                cell(
                    text.tuning_trend_window,
                    &inputs.trend_window,
                    Unit::None,
                    text.tuning_trend_window_note,
                ),
                cell(
                    text.tuning_breadth,
                    &inputs.breadth,
                    Unit::None,
                    text.tuning_breadth_note,
                ),
                cell(
                    text.tuning_verdict,
                    &inputs.verdict_bps,
                    Unit::BasisPoints,
                    text.tuning_verdict_note,
                ),
                cell(
                    text.tuning_scarce,
                    &inputs.scarce_ratio,
                    Unit::PercentTimes,
                    text.tuning_scarce_note,
                ),
            ],
        );
        let anomaly_card = card(
            text.group_anomaly,
            vec![
                cell(
                    text.tuning_spike,
                    &inputs.spike,
                    Unit::BasisPoints,
                    text.tuning_spike_note,
                ),
                cell(
                    text.tuning_severe_spike,
                    &inputs.severe_spike,
                    Unit::BasisPoints,
                    text.tuning_severe_spike_note,
                ),
                cell(
                    text.tuning_wide_spread,
                    &inputs.wide_spread,
                    Unit::BasisPoints,
                    text.tuning_wide_spread_note,
                ),
                cell(
                    text.tuning_severe_spread,
                    &inputs.severe_spread,
                    Unit::BasisPoints,
                    text.tuning_severe_spread_note,
                ),
            ],
        );
        let storage_card = card(
            text.group_storage,
            vec![cell(
                text.tuning_retention,
                &inputs.retention_days,
                Unit::None,
                text.tuning_retention_note,
            )],
        );

        // 不再套一层大面板:六张卡各自带框,再包一框就是框中框。
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .child(column(vec![freshness_card, trend_card]))
                    .child(column(vec![scan_card, anomaly_card]))
                    .child(column(vec![liquidity_card, storage_card])),
            )
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
                    // 底部说清有几项改了没应用:原来改完不点应用没有任何提示。
                    .children((pending > 0).then(|| {
                        mono(ptt_runtime::report_text::fill(
                            text.tuning_pending,
                            &[&pending.to_string()],
                        ))
                        .text_size(fs(FS_10_5))
                        .text_color(c(WARN_TEXT))
                    }))
                    .children((!valid).then(|| {
                        mono(text.tuning_invalid)
                            .text_size(fs(FS_10_5))
                            .text_color(c(DANGER_TEXT))
                    }))
                    .child(div().flex_1())
                    // 「默认值」那一列(24 个灰数字)删了,换成一个还原(§10)。
                    .child(
                        button("tuning-reset", LedgerButton::Quiet, text.tuning_reset, cx)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let defaults = ptt_settings::MarketTuning::default();
                                this.tuning_inputs = Self::new_tuning_inputs(window, cx, &defaults);
                                cx.notify();
                            })),
                    )
                    // 「100bp = 1%」只在右下角说一次。
                    .child(
                        mono(text.tuning_bp_hint)
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_GHOST)),
                    ),
            )
    }
}

/// 一个数怎么换算成人话(§10:秒数直接换算、bp 旁边给百分比)。
#[cfg(windows)]
#[derive(Clone, Copy)]
enum Unit {
    Seconds,
    BasisPoints,
    /// 稀缺比那类"300% = 3 倍"。
    PercentTimes,
    None,
}

#[cfg(windows)]
impl Unit {
    fn conversion(self, text: &'static crate::i18n::Text, value: Option<u64>) -> Option<String> {
        let value = value?;
        #[allow(clippy::cast_precision_loss)]
        let value_f = value as f64;
        let round = |value: f64| {
            if (value - value.round()).abs() < 0.05 {
                format!("{}", value.round() as i64)
            } else {
                format!("{value:.1}")
            }
        };
        match self {
            Self::Seconds if value >= 3600 => Some(ptt_runtime::report_text::fill(
                text.unit_hours,
                &[&round(value_f / 3600.0)],
            )),
            Self::Seconds if value >= 60 => Some(ptt_runtime::report_text::fill(
                text.unit_minutes,
                &[&round(value_f / 60.0)],
            )),
            Self::Seconds | Self::None => None,
            Self::BasisPoints => Some(ptt_runtime::report_text::fill(
                text.unit_percent,
                &[&format!("{:.2}", value_f / 100.0)],
            )),
            Self::PercentTimes => Some(ptt_runtime::report_text::fill(
                text.unit_times,
                &[&round(value_f / 100.0)],
            )),
        }
    }
}
