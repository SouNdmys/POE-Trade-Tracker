//! The market pulse: what every currency is worth, who wants it, and
//! whether the settlement anchor itself is drifting.
//!
//! The page answers the three questions the user specified as one feature:
//! value via settlement cross-rates (with the anchor-drift decomposition —
//! a market-wide "rise" is the anchor falling), supply and demand pressure
//! from the two listing sides (TASK-50 payout attribution), and the
//! scarce-vs-oversupplied discrimination that tells a mirror from junk.
//!
//! Quantities are drawn as proportional bars scaled within each asset's own
//! day range: display geometry only, nothing flows back into a model.

use gpui::{Context, InteractiveElement as _, ParentElement, Styled, div, px};
use gpui_component::StyledExt as _;
use ptt_runtime::report_text;
use ptt_runtime::reports::AnalyticsModel;
use ptt_settings::UiLanguage;

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{StatusKind, chip, empty_state, kv_row, mono, panel, panel_header, scrollable};

/// Liquidity class → chip colour: scarce is the interesting green-ish state,
/// oversupplied and quiet are warnings, balanced is neutral.
fn class_kind(class: ptt_runtime::domain::LiquidityClass) -> StatusKind {
    match class {
        ptt_runtime::domain::LiquidityClass::Scarce => StatusKind::Monitoring,
        ptt_runtime::domain::LiquidityClass::Oversupplied
        | ptt_runtime::domain::LiquidityClass::Quiet => StatusKind::Warning,
        ptt_runtime::domain::LiquidityClass::Balanced => StatusKind::Idle,
    }
}

fn signed_bps(value: i64) -> String {
    if value >= 0 {
        format!("+{value}bp")
    } else {
        format!("{value}bp")
    }
}

impl AppShell {
    /// The Analytics page.
    pub(crate) fn render_analytics(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let PageData::Analytics(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: AnalyticsModel = (**model).clone();
        let language = self.language();

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(self.analytics_main(&model, language, cx))
            .child(self.analytics_detail(&model, language))
    }

    /// Season banner, anchor health, and the asset table.
    fn analytics_main(
        &self,
        model: &AnalyticsModel,
        language: UiLanguage,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();

        let mut head = div().p_3().flex().flex_col().gap_1();
        for note in &model.notes {
            head = head.child(
                mono(note.clone())
                    .text_size(fs(FS_10_5))
                    .text_color(c(WARN_TEXT)),
            );
        }
        let season_line = model.season.as_ref().map_or_else(
            || text.analytics_no_season.to_owned(),
            |season| {
                format!(
                    "{} {} · {}",
                    text.analytics_season, season.label, season.started_day
                )
            },
        );
        let as_of = model.pulse.as_of_day.clone().unwrap_or_default();
        head = head.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(mono(season_line).text_size(fs(FS_11_5)))
                .child(
                    mono(format!(
                        "{as_of} · {} {}",
                        model.data_days, text.analytics_days
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
                ),
        );
        if let Some(health) = &model.pulse.anchor_health {
            let median = health
                .market_median_move_bps
                .map_or_else(|| "-".to_owned(), signed_bps);
            head = head
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            mono(self.display_name(health.anchor_asset_id.as_str()))
                                .text_size(fs(FS_11_5)),
                        )
                        .child(chip(
                            match health.drift {
                                ptt_runtime::domain::AnchorDrift::Steady => StatusKind::Idle,
                                _ => StatusKind::Warning,
                            },
                            report_text::anchor_drift(language, health.drift),
                        ))
                        .child(
                            mono(format!(
                                "{}: {}↑ {}↓ {}= · {median}",
                                text.analytics_breadth, health.risers, health.fallers, health.flat,
                            ))
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META)),
                        ),
                )
                .children(health.crosses.iter().map(|cross| {
                    let drift = cross.drift_bps.map_or_else(|| "-".to_owned(), signed_bps);
                    mono(format!(
                        "{} = {} ({drift})",
                        self.display_name(cross.asset_id.as_str()),
                        cross.latest_rate.text,
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                }));
        }

        // Column headers, then one row per currency, demand-pressure order.
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .text_size(fs(FS_10_5))
            .text_color(c(TEXT_META))
            .child(div().w(px(190.)).child(gpui::SharedString::from("")))
            .child(cell_text(text.analytics_col_value, 90.))
            .child(cell_text(text.analytics_col_demand, 110.))
            .child(cell_text(text.analytics_col_supply, 110.))
            .child(cell_text(text.analytics_col_class, 90.))
            .child(cell_text(text.analytics_col_trend, 120.))
            .child(cell_text(text.analytics_col_rows, 60.));

        let selected = self.analytics_selected.clone();
        let mut rows = div().flex().flex_col();
        for asset in &model.pulse.assets {
            let id = asset.asset_id.as_str().to_owned();
            let is_selected = selected.as_deref() == Some(id.as_str());
            let value = asset
                .value_in_anchor
                .as_ref()
                .map_or_else(|| "-".to_owned(), |rate| rate.text.clone());
            let demand = asset.demand_anchor.map_or_else(
                || format!("{}?", asset.demand_units),
                |units| units.to_string(),
            );
            let supply = asset.supply_anchor.map_or_else(
                || format!("{}?", asset.supply_units),
                |units| units.to_string(),
            );
            let trend = asset
                .trend_bps_relative
                .map_or_else(|| "-".to_owned(), signed_bps);
            let verdict = asset
                .verdict
                .map_or("", |verdict| report_text::trend_verdict(language, verdict));
            let click_id = id.clone();
            let mut row = div()
                .id(gpui::SharedString::from(format!("pulse-{id}")))
                .h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .text_size(fs(FS_11_5))
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.analytics_selected = Some(click_id.clone());
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(190.))
                        .flex_none()
                        .overflow_hidden()
                        .child(mono(self.display_name(&id)).text_size(fs(FS_11_5))),
                )
                .child(cell_mono(value, 90.))
                .child(cell_mono(demand, 110.))
                .child(cell_mono(supply, 110.))
                .child(div().w(px(90.)).flex_none().child(chip(
                    class_kind(asset.class),
                    report_text::liquidity_class(language, asset.class),
                )))
                .child(cell_mono(format!("{trend} {verdict}"), 120.))
                .child(cell_mono(asset.listing_rows.to_string(), 60.));
            if is_selected {
                row = row.bg(c(ACCENT_WASH));
            }
            rows = rows.child(row);
        }

        panel()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.page_analytics))
            .child(head)
            .child(header)
            .child(scrollable(rows, "analytics-rows"))
    }

    /// The selected currency's day-by-day story.
    fn analytics_detail(&self, model: &AnalyticsModel, language: UiLanguage) -> gpui::Div {
        let text = self.text();
        let report = report_text::report(language);
        let Some(asset) = self.analytics_selected.as_deref().and_then(|id| {
            model
                .pulse
                .assets
                .iter()
                .find(|asset| asset.asset_id.as_str() == id)
        }) else {
            return div().w(px(0.)).flex_none();
        };

        let mut body = div().p_3().flex().flex_col().gap_1();
        let value = asset
            .value_in_anchor
            .as_ref()
            .map_or_else(|| "-".to_owned(), |rate| rate.text.clone());
        let composed = if asset.value_is_composed {
            format!(" ({})", text.analytics_composed)
        } else {
            String::new()
        };
        body = body
            .child(kv_row(
                text.analytics_col_value,
                &format!("{value}{composed}"),
            ))
            .child(kv_row(
                text.analytics_col_class,
                report_text::liquidity_class(language, asset.class),
            ));
        if let Some(raw) = asset.trend_bps_raw {
            let relative = asset
                .trend_bps_relative
                .map_or_else(|| "-".to_owned(), signed_bps);
            body = body.child(kv_row(
                text.analytics_col_trend,
                &format!("{} / {relative}", signed_bps(raw)),
            ));
        }
        if let Some(norm) = asset.circulation_norm_units {
            body = body.child(kv_row(text.analytics_detail_norm, &norm.to_string()));
        }
        body = body.child(kv_row(
            text.analytics_detail_days,
            &asset.days_observed.to_string(),
        ));
        if asset.greedy_candidate {
            body = body.child(chip(StatusKind::Monitoring, report.analytics_marker_greedy));
        }

        // Recent days: value text plus a supply bar scaled to the asset's own
        // maximum. Widths are display geometry only.
        let recent: Vec<_> = asset.supply_by_day.iter().rev().take(14).rev().collect();
        let max_supply = recent.iter().map(|(_, units)| *units).max().unwrap_or(0);
        if !recent.is_empty() {
            body = body.child(
                div()
                    .pt_2()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(gpui::SharedString::from(
                        text.analytics_detail_series.to_string(),
                    )),
            );
            for (day, units) in recent {
                let value_of_day = asset
                    .value_by_day
                    .iter()
                    .find(|(value_day, _)| value_day == day)
                    .map_or_else(|| "-".to_owned(), |(_, rate)| rate.text.clone());
                // Proportional width, 0..=120px, floor — geometry, not data.
                let width = if max_supply == 0 {
                    0.0
                } else {
                    ((units.saturating_mul(120) / max_supply.max(1)) as u32).min(120) as f32
                };
                body = body.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            mono(day[5..].to_owned())
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_META)),
                        )
                        .child(div().h(px(8.)).w(px(width)).bg(c(ACCENT_LINE)).flex_none())
                        .child(mono(format!("{units} · {value_of_day}")).text_size(fs(FS_10_5))),
                );
            }
        }

        panel()
            .w(px(360.))
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(&self.display_name(asset.asset_id.as_str())))
            .child(scrollable(body, "analytics-detail"))
    }
}

fn cell_text(label: &str, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .child(gpui::SharedString::from(label.to_string()))
}

fn cell_mono(value: String, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .overflow_hidden()
        .child(mono(value).text_size(fs(FS_11_5)))
}
